use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use super::{
    cursor_bar,
    diff_line::{render_diff_line, render_diff_line_wrapped},
    format_mtime,
    geometry::{LineNumberGutter, RenderGeometry},
    line_numbers::{
        add_line_number_gutters, diff_ln_span, insert_blank_gutter, insert_blank_gutter_at,
    },
};
use crate::app::{App, RowKind};
use crate::git::{DiffContent, FileDiff, FileStatus, Hunk, LineKind};

/// Hard cap on the number of scroll rows we will hand to ratatui in a single
/// frame. Anything past this becomes a `[+N more rows truncated]` marker.
const SCROLL_ROW_LIMIT: usize = 2000;

pub(super) fn render_scroll(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let total_rows = app.layout.rows.len();
    let selected = app.current_hunk();
    let cursor_row = app.scroll;
    let now = std::time::Instant::now();

    let geometry = RenderGeometry::for_diff(
        area.width as usize,
        app.show_line_numbers,
        app.view_mode == crate::app::ViewMode::Stream,
        app.wrap_lines,
        app.layout.max_line_number,
    );
    let wrap_body_width = geometry.wrap_body_width;
    let nowrap_body_width = geometry.nowrap_body_width;

    let raw_body_height = area.height as usize;
    let candidate_header = selected.and_then(|(file_idx, hunk_idx)| {
        find_hunk_header_row(&app.layout.rows, file_idx, hunk_idx)
            .map(|row| (row, file_idx, hunk_idx))
    });

    let (sticky, body_height, viewport_top, skip_visual) = match candidate_header {
        Some((header_row, file_idx, hunk_idx)) if raw_body_height > 1 => {
            let reduced = raw_body_height - 1;
            let (top_reduced, skip_reduced) = app.viewport_placement(reduced, wrap_body_width, now);
            if header_row < top_reduced {
                (
                    Some((file_idx, hunk_idx)),
                    reduced,
                    top_reduced,
                    skip_reduced,
                )
            } else {
                let (top_full, skip_full) =
                    app.viewport_placement(raw_body_height, wrap_body_width, now);
                (None, raw_body_height, top_full, skip_full)
            }
        }
        _ => {
            let (top_full, skip_full) =
                app.viewport_placement(raw_body_height, wrap_body_width, now);
            (None, raw_body_height, top_full, skip_full)
        }
    };

    let (header_area, content_area) = if sticky.is_some() {
        let header = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: 1,
        };
        let body = Rect {
            x: area.x,
            y: area.y + 1,
            width: area.width,
            height: area.height - 1,
        };
        (Some(header), body)
    } else {
        (None, area)
    };

    let viewport_height = body_height;
    app.last_body_height.set(viewport_height);
    app.last_body_width.set(wrap_body_width);

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(viewport_height);
    let mut row_idx = viewport_top;
    let mut skip_remaining = skip_visual;
    let mut cursor_viewport_line: Option<usize> = None;
    while row_idx < total_rows && lines.len() < viewport_height {
        let cursor_sub = if row_idx == cursor_row {
            Some(app.cursor_sub_row)
        } else {
            None
        };
        let hl = app
            .highlighter
            .get_or_init(crate::highlight::Highlighter::new);
        let ctx = RowRenderCtx {
            files: &app.files,
            selected_hunk: selected,
            cursor_sub,
            wrap_body_width,
            nowrap_body_width,
            seen_hunks: &app.seen_hunks,
            hl: Some(hl),
            bg_added: app.config.colors.bg_added_color(),
            bg_deleted: app.config.colors.bg_deleted_color(),
            search: app.search.as_ref(),
            effective_show_ln: geometry.effective_show_ln,
            diff_line_numbers: &app.layout.diff_line_numbers,
            hunk_fingerprints: &app.layout.hunk_fingerprints,
            ln_gutter: geometry.ln_gutter,
        };
        let row_lines = render_row(row_idx, &app.layout.rows[row_idx], &ctx);
        let mut take = row_lines.into_iter();
        let initial_skip = skip_remaining;
        for _ in 0..skip_remaining {
            if take.next().is_none() {
                break;
            }
        }
        skip_remaining = 0;
        let row_start_line = lines.len();
        for line in take {
            if lines.len() >= viewport_height {
                break;
            }
            lines.push(line);
        }
        if row_idx == cursor_row && cursor_viewport_line.is_none() {
            let sub_in_view = app.cursor_sub_row.saturating_sub(initial_skip);
            let candidate = row_start_line + sub_in_view;
            if candidate < lines.len() {
                cursor_viewport_line = Some(candidate);
            }
        }
        row_idx += 1;
    }

    if total_rows > SCROLL_ROW_LIMIT && row_idx < total_rows {
        let remaining = total_rows - row_idx;
        if remaining > 0 && lines.len() < viewport_height {
            lines.push(Line::from(Span::styled(
                format!("[+{remaining} more rows]"),
                Style::default().fg(Color::DarkGray),
            )));
        }
    }

    frame.render_widget(Paragraph::new(lines), content_area);
    apply_cursor_gutter_tint(frame, content_area, cursor_viewport_line);

    if let (Some(header_rect), Some((file_idx, hunk_idx))) = (header_area, sticky)
        && let DiffContent::Text(hunks) = &app.files[file_idx].content
    {
        let line = render_hunk_header(
            &hunks[hunk_idx],
            true,
            false,
            app.hunk_is_seen(file_idx, hunk_idx),
        );
        let line = if geometry.effective_show_ln {
            insert_blank_gutter(line, &geometry.ln_gutter)
        } else {
            line
        };
        frame.render_widget(Paragraph::new(line), header_rect);
    }
}

fn apply_cursor_gutter_tint(
    frame: &mut Frame<'_>,
    content_area: Rect,
    cursor_viewport_line: Option<usize>,
) {
    let Some(line_in_viewport) = cursor_viewport_line else {
        return;
    };
    if line_in_viewport >= content_area.height as usize {
        return;
    }
    let y = content_area.y + line_in_viewport as u16;
    let end_x = content_area.x.saturating_add(content_area.width);
    let buf = frame.buffer_mut();
    for x in content_area.x..end_x {
        if let Some(cell) = buf.cell_mut((x, y)) {
            let darkened = darken_cursor_body_bg(cell.style().bg);
            let new_style = cell.style().bg(darkened);
            cell.set_style(new_style);
        }
    }
}

fn darken_cursor_body_bg(existing: Option<Color>) -> Color {
    const FACTOR: f32 = 0.75;
    const DEFAULT_DIM: Color = Color::Rgb(30, 30, 36);
    match existing {
        Some(Color::Yellow) => Color::Yellow,
        Some(Color::Rgb(r, g, b)) => Color::Rgb(
            (r as f32 * FACTOR) as u8,
            (g as f32 * FACTOR) as u8,
            (b as f32 * FACTOR) as u8,
        ),
        _ => DEFAULT_DIM,
    }
}

fn find_hunk_header_row(rows: &[RowKind], file_idx: usize, hunk_idx: usize) -> Option<usize> {
    rows.iter().position(|r| {
        matches!(
            r,
            RowKind::HunkHeader {
                file_idx: f,
                hunk_idx: h,
            } if *f == file_idx && *h == hunk_idx
        )
    })
}

struct RowRenderCtx<'a> {
    files: &'a [FileDiff],
    selected_hunk: Option<(usize, usize)>,
    cursor_sub: Option<usize>,
    wrap_body_width: Option<usize>,
    nowrap_body_width: usize,
    seen_hunks: &'a std::collections::BTreeMap<(std::path::PathBuf, usize), u64>,
    hl: Option<&'a crate::highlight::Highlighter>,
    bg_added: Color,
    bg_deleted: Color,
    search: Option<&'a crate::app::SearchState>,
    effective_show_ln: bool,
    diff_line_numbers: &'a [Option<(Option<usize>, Option<usize>)>],
    hunk_fingerprints: &'a [Vec<Option<u64>>],
    ln_gutter: LineNumberGutter,
}

fn render_row(row_idx: usize, row: &RowKind, ctx: &RowRenderCtx<'_>) -> Vec<Line<'static>> {
    let files = ctx.files;
    let selected_hunk = ctx.selected_hunk;
    let cursor_sub = ctx.cursor_sub;
    let wrap_body_width = ctx.wrap_body_width;
    let nowrap_body_width = ctx.nowrap_body_width;
    let seen_hunks = ctx.seen_hunks;
    let hl = ctx.hl;
    match row {
        RowKind::FileHeader { file_idx } => {
            let line = render_file_header(&files[*file_idx], cursor_sub.is_some());
            if ctx.effective_show_ln {
                vec![insert_blank_gutter(line, &ctx.ln_gutter)]
            } else {
                vec![line]
            }
        }
        RowKind::HunkHeader { file_idx, hunk_idx } => {
            let DiffContent::Text(hunks) = &files[*file_idx].content else {
                return vec![Line::raw("")];
            };
            let is_selected = selected_hunk == Some((*file_idx, *hunk_idx));
            let marked_fp = crate::app::seen_hunk_fingerprint(
                seen_hunks,
                &files[*file_idx].path,
                hunks[*hunk_idx].old_start,
            );
            let is_seen = marked_fp.is_some_and(|marked| {
                let current = ctx
                    .hunk_fingerprints
                    .get(*file_idx)
                    .and_then(|fps| fps.get(*hunk_idx))
                    .copied()
                    .flatten()
                    .unwrap_or_else(|| crate::app::hunk_fingerprint(&hunks[*hunk_idx]));
                marked == current
            });
            let line = render_hunk_header(
                &hunks[*hunk_idx],
                is_selected,
                cursor_sub.is_some(),
                is_seen,
            );
            if ctx.effective_show_ln {
                vec![insert_blank_gutter(line, &ctx.ln_gutter)]
            } else {
                vec![line]
            }
        }
        RowKind::DiffLine {
            file_idx,
            hunk_idx,
            line_idx,
        } => {
            let DiffContent::Text(hunks) = &files[*file_idx].content else {
                return vec![Line::raw("")];
            };
            let is_selected = selected_hunk == Some((*file_idx, *hunk_idx));
            let line = &hunks[*hunk_idx].lines[*line_idx];
            let is_cursor = cursor_sub.is_some();
            let search_matches = row_search_matches(ctx.search, row_idx);
            let rendered = match wrap_body_width {
                Some(width) => render_diff_line_wrapped(
                    line,
                    is_selected,
                    cursor_sub,
                    width,
                    hl,
                    Some(&files[*file_idx].path),
                    ctx.bg_added,
                    ctx.bg_deleted,
                    &search_matches,
                ),
                None => vec![render_diff_line(
                    line,
                    is_selected,
                    is_cursor,
                    nowrap_body_width,
                    hl,
                    Some(&files[*file_idx].path),
                    ctx.bg_added,
                    ctx.bg_deleted,
                    &search_matches,
                )],
            };
            if !ctx.effective_show_ln {
                rendered
            } else {
                let pair = ctx
                    .diff_line_numbers
                    .get(row_idx)
                    .copied()
                    .flatten()
                    .unwrap_or((None, None));
                add_line_number_gutters(
                    rendered,
                    diff_ln_span(pair, &ctx.ln_gutter),
                    &ctx.ln_gutter,
                )
            }
        }
        RowKind::BinaryNotice { .. } => {
            let line = Line::from(Span::styled(
                if cursor_sub.is_some() {
                    "  ▶    [binary file - diff suppressed]"
                } else {
                    "       [binary file - diff suppressed]"
                },
                Style::default().fg(Color::DarkGray),
            ));
            if ctx.effective_show_ln {
                vec![insert_blank_gutter_at(line, &ctx.ln_gutter, 5)]
            } else {
                vec![line]
            }
        }
        RowKind::Spacer => vec![Line::raw("")],
    }
}

fn row_search_matches(
    search: Option<&crate::app::SearchState>,
    row_idx: usize,
) -> Vec<(usize, usize, bool)> {
    let Some(state) = search else {
        return Vec::new();
    };
    let start = state.matches.partition_point(|m| m.row < row_idx);
    let end = start + state.matches[start..].partition_point(|m| m.row == row_idx);
    state.matches[start..end]
        .iter()
        .enumerate()
        .map(|(offset, m)| {
            let match_idx = start + offset;
            (m.byte_start, m.byte_end, match_idx == state.current)
        })
        .collect()
}

fn render_file_header(file: &FileDiff, is_cursor: bool) -> Line<'static> {
    let path_color = match file.status {
        FileStatus::Modified => Color::Cyan,
        FileStatus::Added => Color::Green,
        FileStatus::Deleted => Color::Red,
        FileStatus::Untracked => Color::Yellow,
    };

    let counts = match &file.content {
        DiffContent::Binary => "bin".to_string(),
        DiffContent::Text(_) => format!("+{} -{}", file.added, file.deleted),
    };
    let mtime = format_mtime(file.mtime);

    let mut spans = vec![if is_cursor {
        Span::styled(
            "▶ ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::raw("  ")
    }];
    if let Some(prefix) = &file.header_prefix {
        spans.push(Span::styled(
            format!("{prefix}  "),
            Style::default().fg(Color::DarkGray),
        ));
    }
    spans.extend([
        Span::styled(
            file.path.display().to_string(),
            Style::default().fg(path_color).add_modifier(Modifier::BOLD),
        ),
        Span::raw("   "),
        Span::styled(mtime, Style::default().fg(Color::DarkGray)),
        Span::raw("   "),
        Span::raw(counts),
    ]);
    spans.push(Span::raw(""));
    Line::from(spans)
}

fn render_hunk_header(
    hunk: &Hunk,
    is_selected: bool,
    is_cursor: bool,
    is_seen: bool,
) -> Line<'static> {
    let seen_mark = if is_seen { "▸ " } else { "  " };
    let (added, deleted) = hunk
        .lines
        .iter()
        .fold((0usize, 0usize), |(a, d), l| match l.kind {
            LineKind::Added => (a + 1, d),
            LineKind::Deleted => (a, d + 1),
            _ => (a, d),
        });
    let counts = format!("+{added}/-{deleted}");

    let line_range = if hunk.new_count == 0 {
        if hunk.old_count > 1 {
            format!(
                "L{}-{}",
                hunk.old_start,
                hunk.old_start + hunk.old_count - 1
            )
        } else {
            format!("L{}", hunk.old_start)
        }
    } else if hunk.new_count > 1 {
        format!(
            "L{}-{}",
            hunk.new_start,
            hunk.new_start + hunk.new_count - 1
        )
    } else {
        format!("L{}", hunk.new_start)
    };

    let label = match &hunk.context {
        Some(ctx) => format!("{seen_mark}@@ {ctx}  {line_range} {counts}"),
        None => format!("{seen_mark}@@ {line_range} {counts}"),
    };

    let mut label_style = Style::default().fg(Color::Cyan);
    if !is_selected {
        label_style = label_style.add_modifier(Modifier::DIM);
    }
    let gutter = cursor_bar(is_cursor, is_selected);

    Line::from(vec![gutter, Span::styled(label, label_style)])
}
