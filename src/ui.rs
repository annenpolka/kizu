use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

mod footer;
mod overlays;

pub(crate) use footer::format_local_time;

use crate::app::{App, RowKind};
use crate::git::{DiffContent, FileDiff, FileStatus, Hunk, LineKind};

/// Hard cap on the number of scroll rows we will hand to ratatui in a single
/// frame. Anything past this becomes a `[+N more rows truncated]` marker.
/// Decision Log: 1 ファイル 2000 行で truncate — for the new scroll model the
/// limit is "2000 visible rows total" since hunks are now flattened across
/// every modified file.
const SCROLL_ROW_LIMIT: usize = 2000;

/// Delta-style background color defaults. Production code reads these
/// from [`crate::config::ColorConfig`]; the constants remain for tests
/// that assert on default-config rendering (ADR-0014).
#[cfg(test)]
const BG_ADDED: Color = Color::Rgb(10, 50, 10);
#[cfg(test)]
const BG_DELETED: Color = Color::Rgb(60, 10, 10);

/// Render the entire kizu frame: scroll view (main) + footer (bottom),
/// optionally with the modal file picker overlaid on top.
///
/// When a text-input overlay is active (scar comment `c` or search
/// `/`), a dedicated input row is inserted between the main area
/// and the footer. The input row wraps long text and the terminal
/// cursor is placed at the text end so the IME composition window
/// appears in the right spot.
pub fn render(frame: &mut Frame<'_>, app: &App) {
    let area = frame.area();

    // Determine input-line content + prefix + cursor pos for rendering.
    let input_line: Option<(String, &str, usize)> = if let Some(state) = app.scar_comment.as_ref() {
        Some((state.body.clone(), "> ", state.cursor_pos))
    } else if let Some(input) = app.search_input.as_ref() {
        Some((input.query.clone(), "/", input.cursor_pos))
    } else {
        None
    };

    let input_height: u16 = if let Some((ref text, prefix, _)) = input_line {
        use unicode_width::UnicodeWidthStr;
        let total_width = prefix.width() + text.width() + 1; // +1 for cursor block
        let w = (area.width as usize).max(1);
        total_width.div_ceil(w).max(1) as u16
    } else {
        0
    };

    let chunks = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(input_height),
        Constraint::Length(1),
    ])
    .split(area);
    let main = chunks[0];
    let input_area = chunks[1];
    let footer = chunks[2];

    if let Some(fv) = app.file_view.as_ref() {
        app.last_body_height.set(main.height as usize);
        let hl = app
            .highlighter
            .get_or_init(crate::highlight::Highlighter::new);
        // Sample the file-view scroll animation if active.
        let effective_top = if let Some(anim) = &fv.anim {
            let (v, _done) = anim.sample(fv.scroll_top as f32, std::time::Instant::now());
            v.round() as usize
        } else {
            fv.scroll_top
        };
        render_file_view(
            frame,
            main,
            fv,
            app.wrap_lines,
            app.show_line_numbers,
            Some(hl),
            effective_top,
        );
    } else if app.files.is_empty() {
        render_empty(frame, main, app);
    } else {
        render_scroll(frame, main, app);
    }

    // Render the dedicated input row when a text overlay is active.
    if let Some((text, prefix, cursor_pos)) = input_line {
        render_input_line(frame, input_area, prefix, &text, cursor_pos);
    }

    footer::render_footer(frame, footer, app);

    if app.picker.is_some() {
        overlays::render_picker(frame, area, app);
    }

    if app.help_overlay {
        overlays::render_help_overlay(frame, area, app);
    }
}

/// Render a text-input line with wrapping support and a blinking
/// cursor block at the text end. Used for both scar-comment (`> `)
/// and search (`/`) overlays.
fn render_input_line(
    frame: &mut Frame<'_>,
    area: Rect,
    prefix: &str,
    text: &str,
    cursor_pos: usize,
) {
    use ratatui::widgets::Wrap;
    use unicode_width::UnicodeWidthStr;

    // Split text at cursor position for visual cursor rendering.
    let before_cursor: String = text.chars().take(cursor_pos).collect();
    let cursor_char: String = text
        .chars()
        .nth(cursor_pos)
        .map(|c| c.to_string())
        .unwrap_or_default();
    let after_cursor: String = text
        .chars()
        .skip(cursor_pos + cursor_char.chars().count())
        .collect();

    let mut spans = vec![
        Span::styled(
            prefix.to_string(),
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(before_cursor.clone(), Style::default().fg(Color::White)),
    ];

    if cursor_char.is_empty() {
        // Cursor at end: show block cursor placeholder
        spans.push(Span::styled(
            " ",
            Style::default().fg(Color::Black).bg(Color::White),
        ));
    } else {
        // Cursor on a character: highlight it
        spans.push(Span::styled(
            cursor_char,
            Style::default().fg(Color::Black).bg(Color::White),
        ));
        spans.push(Span::styled(
            after_cursor,
            Style::default().fg(Color::White),
        ));
    }

    let paragraph = Paragraph::new(Line::from(spans)).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);

    // Place the terminal cursor at the edit position for IME.
    let cursor_display_offset = prefix.width() + before_cursor.width();
    let w = area.width.max(1) as usize;
    let cursor_y = area.y + (cursor_display_offset / w) as u16;
    let cursor_x = area.x + (cursor_display_offset % w) as u16;
    frame.set_cursor_position((
        cursor_x.min(area.right().saturating_sub(1)),
        cursor_y.min(area.bottom().saturating_sub(1)),
    ));
}

fn render_scroll(frame: &mut Frame<'_>, area: Rect, app: &App) {
    // M4v + cursor placement + wrap-aware scroll (ADR-0007):
    //   - The cursor row inside `app.layout.rows` is `app.scroll`.
    //   - The viewport's *top* is produced by
    //     `App::viewport_placement`, which returns `(top_row,
    //     skip_visual)`. In nowrap `skip_visual` is always 0 so the
    //     renderer walks whole logical rows. In wrap the placement
    //     operates in visual-row space via `VisualIndex`, and
    //     `skip_visual` lets the renderer start drawing mid-row so
    //     the cursor always lands at its placement target regardless
    //     of how much preceding diff content wraps.
    //   - Sticky header still works: the cursor's enclosing hunk is
    //     pinned to viewport row 0 when its header would otherwise
    //     be off the top.
    let total_rows = app.layout.rows.len();
    let selected = app.current_hunk();
    let cursor_row = app.scroll;
    let now = std::time::Instant::now();

    // v0.5: determine the effective line-number gutter width and
    // decide whether we can actually draw it given the viewport size.
    // Single-source-of-truth body_width calc (Codex review §Critical-1):
    // every downstream consumer — `VisualIndex`, `wrap_at_chars`, the
    // numbered renderers — receives the exact same width this block
    // computes.
    let aw = area.width as usize;
    // Single-column gutter for both diff and file view (2026-04-21
    // feedback: two columns read as "duplicated"). `resolve_ln_gutter`
    // also applies the extreme-narrow fallback so the body never
    // collapses below 4 cells.
    let wants_ln = app.show_line_numbers && app.view_mode != crate::app::ViewMode::Stream;
    let (effective_show_ln, ln_gutter_width, ln_gutter) =
        resolve_ln_gutter(wants_ln, app.layout.max_line_number, aw);

    // v0.5 M2: wrap mode reserves 5 cells for the left bar only. The
    // legacy `¶` marker column took another cell on every row, but
    // under the new semantics the end-of-line marker is drawn solely
    // on EOF-no-newline rows (rare), so keeping the cell reserved on
    // every row left a permanent one-cell blank band at the right
    // edge. On the rare EOF row the renderer overlaps its `∅` with
    // the trailing pad — if the chunk fills the full body width the
    // last glyph is overwritten by the marker, which is acceptable
    // for the `\ No newline at end of file` edge case.
    let wrap_body_width: Option<usize> = if app.wrap_lines {
        Some(aw.saturating_sub(5 + ln_gutter_width).max(1))
    } else {
        None
    };
    // Nowrap mode still needs a body width so the diff row
    // background color can extend to the viewport edge. 5 cells for
    // the left bar, the rest is body.
    let nowrap_body_width: usize = aw.saturating_sub(5 + ln_gutter_width).max(1);

    // Sticky header decision (ADR-0009 fix):
    //
    // The previous implementation computed a `provisional_top` with
    // the *full* area height, decided stickiness from it, and only
    // then shrank the body by one row if sticky was on. The
    // recomputed placement against the smaller body could produce a
    // different `top` than the provisional one — especially in
    // long-hunk / wrap cases where the target y is viewport-height
    // dependent — so the sticky decision was occasionally based on
    // a viewport the renderer was not actually going to draw. The
    // result was a disappearing hunk header at a boundary.
    //
    // The new algorithm pessimistically peeks at what the sticky
    // case would look like (body = raw - 1). If the enclosing
    // hunk's header row is above that reduced-body top, sticky is
    // warranted and we render with body = raw - 1. Otherwise sticky
    // is off and we render with the full body. Either branch ends
    // with exactly one "final" `viewport_placement` call whose
    // side-effect (setting `visual_top` for the animation state) is
    // authoritative. Peek calls that happen during the decision are
    // harmless because the final call overwrites `visual_top`.
    let raw_body_height = area.height as usize;
    let candidate_header = selected.and_then(|(file_idx, hunk_idx)| {
        find_hunk_header_row(&app.layout.rows, file_idx, hunk_idx)
            .map(|row| (row, file_idx, hunk_idx))
    });

    let (sticky, body_height, viewport_top, skip_visual) = match candidate_header {
        Some((header_row, file_idx, hunk_idx)) if raw_body_height > 1 => {
            // Peek: what would the viewport look like with one row
            // reserved for the sticky banner? If the header would
            // still sit above this peeked top, sticky wins and
            // we commit to the reduced body.
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
                // Not worth stealing a row: the header fits in the
                // full-body viewport. Commit to the full body via a
                // fresh placement call so `visual_top` reflects the
                // version we actually render.
                let (top_full, skip_full) =
                    app.viewport_placement(raw_body_height, wrap_body_width, now);
                (None, raw_body_height, top_full, skip_full)
            }
        }
        _ => {
            // Either no enclosing hunk to sticky-pin, or the
            // viewport is too small to spare a row for a banner —
            // render with the full body.
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
    // Tell the App layer how tall/wide the body actually was so the
    // next `J`/`K` / Ctrl-d press can size its scroll chunk relative
    // to the current screen dimensions.
    app.last_body_height.set(viewport_height);
    app.last_body_width.set(wrap_body_width);

    // Walk logical rows from `viewport_top` and accumulate visual
    // lines until we've filled the viewport or run out of rows.
    // Wrapped DiffLines contribute multiple visual rows per logical
    // row; the first row honours `skip_visual` so wrap-mode placement
    // can begin drawing in the middle of a wrapped line when needed.
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(viewport_height);
    let mut row_idx = viewport_top;
    let mut skip_remaining = skip_visual;
    // Viewport-relative line index where the cursor's own visual line
    // lands. Recorded during the render loop so the flash overlay
    // afterwards knows which row to paint without rebuilding the
    // visual index.
    let mut cursor_viewport_line: Option<usize> = None;
    while row_idx < total_rows && lines.len() < viewport_height {
        // The cursor row gets `Some(sub)` so wrap-mode rendering
        // can position the arrow on the correct visual sub-row;
        // every other row gets `None` which is "no arrow here".
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
            effective_show_ln,
            diff_line_numbers: &app.layout.diff_line_numbers,
            ln_gutter,
        };
        let row_lines = render_row(row_idx, &app.layout.rows[row_idx], &ctx);
        let mut take = row_lines.into_iter();
        // Discard any leading visual lines requested by the
        // placement layer (only the first logical row ever carries
        // a non-zero `skip_remaining` budget).
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
            // The cursor's visual sub-row within this logical row,
            // minus any leading lines the placement layer asked us to
            // discard for the *first* row in the viewport.
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

    // Paint a subtle background tint on the cursor row's left gutter
    // so the user can pick out the current row at a glance without
    // the diff body's `bg_added` / `bg_deleted` colors being obscured.
    apply_cursor_gutter_tint(frame, content_area, cursor_viewport_line);

    // Pin the header on top after the body, so the overlay always wins.
    if let (Some(header_rect), Some((file_idx, hunk_idx))) = (header_area, sticky)
        && let DiffContent::Text(hunks) = &app.files[file_idx].content
    {
        let line = render_hunk_header(
            &hunks[hunk_idx],
            true,
            false,
            app.hunk_is_seen(file_idx, hunk_idx),
        );
        // v0.5: sticky header also needs the blank gutter so its body
        // lines up with the scrolling DiffLine bodies underneath
        // (Codex 3rd-round Important-1).
        let line = if effective_show_ln {
            insert_blank_gutter(line, &ln_gutter)
        } else {
            line
        };
        frame.render_widget(Paragraph::new(line), header_rect);
    }
}

/// Walk `rows` to find the row index of the `HunkHeader` matching
/// `(file_idx, hunk_idx)`. Returns `None` if the layout is empty or the
/// cursor's hunk has no header row (binary, etc).
/// Darken every cell of the cursor row's bg, hue preserved. No
/// separate gutter tint — the gutter gets the same semi-transparent
/// darken treatment as the diff body, so the row is visually recessed
/// uniformly and the `+` / `-` add/delete signal carried by the
/// surrounding full-intensity rows stays legible.
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

/// Return a background color that sits behind the cursor row's body
/// cells. `Color::Rgb` values are scaled down (hue preserved, value
/// reduced) so `bg_added` green stays green-but-dimmer; cells without
/// a set bg (`Color::Reset` / unset) pick up a subtle dark gray.
/// Other color kinds (ANSI names) fall through to the same dim gray
/// since scaling them precisely would require resolving terminal
/// palette which we don't control.
///
/// `Color::Yellow` is preserved as-is: it is used exclusively as the
/// `/`-search current-match bg, and the whole point of the highlight
/// is to stand out. Without this carve-out, every `n`/`N` press
/// (which parks the cursor on the match row) would collapse the
/// Yellow reversal into `DEFAULT_DIM` and the user would perceive
/// the highlight as "dark".
fn darken_cursor_body_bg(existing: Option<Color>) -> Color {
    // `bg_added = Rgb(10, 50, 10)` (default) maps to Rgb(7, 37, 7) —
    // still clearly green, just slightly muted against the surrounding
    // full-intensity rows. Earlier 0.5 made it too stark a contrast.
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

/// Per-frame rendering context passed to [`render_row`]. Bundles
/// the shared state that every row type needs without inflating
/// the function signature.
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
    /// Confirmed `/` search state. When set, every DiffLine row paints
    /// match byte ranges with the overlay (see `apply_search_overlay`).
    /// `None` skips the overlay entirely.
    search: Option<&'a crate::app::SearchState>,
    /// v0.5: effective line-number state. `true` only when
    /// `app.show_line_numbers` is on AND the current view mode is not
    /// Stream (Stream suppresses the gutter because its synthetic
    /// `old_start`/`new_start` are not real file line numbers).
    effective_show_ln: bool,
    /// Per-row cached `(old, new)` line numbers. Parallel to
    /// `app.layout.rows`; `None` for non-DiffLine rows.
    diff_line_numbers: &'a [Option<(Option<usize>, Option<usize>)>],
    /// Gutter geometry. Fields are all zero when `effective_show_ln`
    /// is false (either because the flag is off or because the
    /// viewport is too narrow to reserve a gutter).
    ln_gutter: LineNumberGutter,
}

/// Classification of a single character position against the active
/// search matches. Drives the style overlay inside the renderer.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SearchHl {
    None,
    Other,
    Current,
}

fn cursor_bar(is_cursor: bool, is_selected: bool) -> Span<'static> {
    if is_cursor {
        Span::styled(
            "  ▶  ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
    } else if is_selected {
        Span::styled("  ▎  ", Style::default().fg(Color::Yellow))
    } else {
        Span::raw("     ")
    }
}

/// Build the styled visual `Line`s for a single logical layout row.
/// Most row types produce exactly one `Line`; a `DiffLine` in wrap
/// mode (`wrap_body_width.is_some()`) can produce multiple lines
/// when its content exceeds the body width.
///
/// `row_idx` is the logical row's position in `layout.rows`; passed
/// so DiffLine rendering can filter `ctx.search.matches` down to the
/// highlights that belong to this row.
///
/// `selected_hunk` identifies the (file_idx, hunk_idx) the cursor is
/// currently inside; rows belonging to that hunk render at full
/// saturation, all other hunk rows render with `Modifier::DIM`.
/// `cursor_sub` is `Some(n)` for the logical row the cursor is on:
/// `n` is the visual sub-row index (0 for the first visual row of a
/// wrapped block, larger values when the user has walked into the
/// middle of a long wrapped line via Ctrl-d / J). The arrow marker
/// lands on that visual sub-row instead of always on the first
/// (ADR-0009 fix). `None` for non-cursor rows.
///
/// Rendering context is bundled into [`RowRenderCtx`] to keep the
/// signature manageable as we add more per-frame state (seen marks,
/// highlighter, etc.).
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
            let line = render_hunk_header(
                &hunks[*hunk_idx],
                is_selected,
                cursor_sub.is_some(),
                crate::app::is_hunk_seen(
                    seen_hunks,
                    &files[*file_idx].path,
                    hunks[*hunk_idx].old_start,
                    crate::app::hunk_fingerprint(&hunks[*hunk_idx]),
                ),
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
            // Collect this row's search matches as (byte_start, byte_end,
            // is_current) tuples so the renderer doesn't need to walk
            // SearchState.current for every span.
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
                // The cache built by build_layout
                // (`diff_line_numbers[row_idx]`) is Some for every
                // DiffLine row; unwrap_or is defensive.
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
                // BinaryNotice builds its own 5-cell bar + 2-cell pad
                // inline, so the gutter slot has to be spliced in by
                // splitting the single span at x=5.
                vec![insert_blank_gutter_at(line, &ctx.ln_gutter, 5)]
            } else {
                vec![line]
            }
        }
        RowKind::Spacer => vec![Line::raw("")],
    }
}

/// Splice `gutter_span` directly after the first span of `line`
/// (which is the bar — 5-cell for DiffLine / HunkHeader rows,
/// 2-cell for FileHeader rows). Used to keep non-DiffLine row bodies
/// horizontally aligned with DiffLine bodies when the gutter is on.
/// `bar_fallback` is the span to emit when the line has no spans at
/// all; callers pass a matching-width blank bar so downstream
/// measurements stay consistent.
fn insert_gutter_span(
    line: Line<'static>,
    gutter_span: Span<'static>,
    bar_fallback: Span<'static>,
) -> Line<'static> {
    let mut spans = line.spans;
    let bar = if spans.is_empty() {
        bar_fallback
    } else {
        spans.remove(0)
    };
    let mut new_spans = Vec::with_capacity(spans.len() + 2);
    new_spans.push(bar);
    new_spans.push(gutter_span);
    new_spans.extend(spans);
    Line::from(new_spans)
}

/// Shorthand for splicing a blank gutter after the head span of a
/// line that already has its own bar (or no bar at all, for Spacer).
fn insert_blank_gutter(line: Line<'static>, gutter: &LineNumberGutter) -> Line<'static> {
    insert_gutter_span(line, gutter.blank_span(), Span::raw(""))
}

/// Splice a blank gutter span after the first `split_at` cells of
/// `line`'s single-span content. Used for BinaryNotice which packs
/// its bar + pad + body into one `Span` literal.
fn insert_blank_gutter_at(
    line: Line<'static>,
    gutter: &LineNumberGutter,
    split_at: usize,
) -> Line<'static> {
    let mut spans = line.spans;
    if spans.is_empty() {
        return Line::from(vec![gutter.blank_span()]);
    }
    let first = spans.remove(0);
    let text = first.content.as_ref();
    let (head, tail) = text.split_at(split_at.min(text.len()));
    let head_span = Span::styled(head.to_string(), first.style);
    let tail_span = Span::styled(tail.to_string(), first.style);
    let mut new_spans = Vec::with_capacity(spans.len() + 3);
    new_spans.push(head_span);
    new_spans.push(gutter.blank_span());
    new_spans.push(tail_span);
    new_spans.extend(spans);
    Line::from(new_spans)
}

/// Project the global `SearchState.matches` onto a single layout row.
/// Returns `(byte_start, byte_end, is_current)` tuples in source order.
/// Empty vector when there's no confirmed search or this row has no hits.
fn row_search_matches(
    search: Option<&crate::app::SearchState>,
    row_idx: usize,
) -> Vec<(usize, usize, bool)> {
    let Some(state) = search else {
        return Vec::new();
    };
    state
        .matches
        .iter()
        .enumerate()
        .filter(|(_, m)| m.row == row_idx)
        .map(|(i, m)| (m.byte_start, m.byte_end, i == state.current))
        .collect()
}

/// Classify every char in `content` against the match ranges. `matches`
/// are `(byte_start, byte_end, is_current)` in source order; byte
/// offsets are guaranteed to be UTF-8 char boundaries by `find_matches`.
///
/// Invariant: output length equals `content.chars().count()`. Chars
/// whose byte position sits inside a match range get `Current` or
/// `Other`; everything else stays `None`. A single char that straddles
/// the current-match range and an overlapping other-match (can only
/// happen if matches overlap, which `find_matches` doesn't produce
/// because it advances `start` past each hit) would bias to Current —
/// but the guard is defensive, not load-bearing.
fn classify_chars_by_match(content: &str, matches: &[(usize, usize, bool)]) -> Vec<SearchHl> {
    let n = content.chars().count();
    let mut out = vec![SearchHl::None; n];
    if matches.is_empty() {
        return out;
    }
    for (char_idx, (byte_pos, _)) in content.char_indices().enumerate() {
        for &(bs, be, is_current) in matches {
            if byte_pos >= bs && byte_pos < be {
                let new_kind = if is_current {
                    SearchHl::Current
                } else {
                    SearchHl::Other
                };
                // Current wins over Other on overlap (defensive).
                if out[char_idx] != SearchHl::Current || is_current {
                    out[char_idx] = new_kind;
                }
            }
        }
    }
    out
}

/// Compose a base diff-line style with the search-highlight overlay
/// for one char. `Current` fully overrides the base with the
/// yellow-reversal look; `Other` keeps the base bg/fg but adds a
/// bold underline so the word stands out without swallowing the
/// add/delete signal. `Other` explicitly strips `DIM` because matches
/// that land in an unfocused hunk still need to pop — otherwise the
/// whole point of a search highlight is lost on everything but the
/// currently selected hunk.
fn apply_search_overlay(base: Style, fg: Color, hl: SearchHl) -> Style {
    match hl {
        SearchHl::None => base.fg(fg),
        SearchHl::Other => base
            .fg(fg)
            .remove_modifier(Modifier::DIM)
            .add_modifier(Modifier::UNDERLINED | Modifier::BOLD),
        SearchHl::Current => Style::default()
            .bg(Color::Yellow)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD),
    }
}

/// Split `content` into chunks whose **display width** is at most
/// `width` cells. Always returns at least one chunk; an empty input
/// produces `[""]`. CJK / emoji / other wide characters consume 2
/// cells apiece, so a body width of 40 cells holds roughly 20 kanji —
/// feeding char counts straight into ratatui overflowed the viewport
/// and broke the wrap marker on CJK-heavy diffs.
///
/// Zero-width combining marks (`char.width()` returns `Some(0)`) and
/// control chars (`None`) are folded into the current chunk without
/// advancing the cell counter so they stick with their preceding
/// base glyph instead of getting orphaned onto a new visual row.
fn wrap_at_chars(content: &str, width: usize) -> Vec<&str> {
    use unicode_width::UnicodeWidthChar;
    if content.is_empty() || width == 0 {
        return vec![content];
    }
    let mut chunks = Vec::new();
    let mut chunk_start = 0usize;
    let mut chunk_cells = 0usize;
    for (idx, ch) in content.char_indices() {
        let ch_cells = ch.width().unwrap_or(0);
        // Flush the current chunk before placing a char that would
        // overshoot the cell budget. Requires `chunk_cells > 0` so
        // that a single char wider than the whole width still lands
        // in one chunk rather than looping forever on an empty one.
        if chunk_cells > 0 && chunk_cells + ch_cells > width {
            chunks.push(&content[chunk_start..idx]);
            chunk_start = idx;
            chunk_cells = 0;
        }
        chunk_cells += ch_cells;
    }
    if chunk_start < content.len() {
        chunks.push(&content[chunk_start..]);
    }
    if chunks.is_empty() {
        chunks.push(content);
    }
    chunks
}

/// v0.5 M2: EOF-no-newline marker (`∅`) span. Drawn at the end of
/// the *last* visual row of a DiffLine whose `has_trailing_newline`
/// is false — the git `\ No newline at end of file` case. Yellow +
/// Bold so the rare event is visually loud; legacy `¶` on every
/// normal row was too chatty.
fn eof_no_newline_span(bg: Option<Color>) -> Span<'static> {
    let mut style = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    if let Some(b) = bg {
        style = style.bg(b);
    }
    Span::styled("∅", style)
}

/// Wrap-mode variant of [`render_diff_line`]. Splits `line.content`
/// at `body_width` chars and paints every visual row with the delta-style
/// background color (ADR-0014). Only the last visual row of a DiffLine
/// whose `has_trailing_newline = false` gets the `∅` EOF-no-newline
/// marker (v0.5 M2, was previously `¶` on every newline-terminated row).
///
/// Each visual row is padded out to `body_width` with trailing spaces
/// so the background color extends uniformly to the right margin
/// instead of stopping at the last content character.
#[allow(clippy::too_many_arguments)]
fn render_diff_line_wrapped(
    line: &crate::git::DiffLine,
    is_selected: bool,
    cursor_sub: Option<usize>,
    body_width: usize,
    hl: Option<&crate::highlight::Highlighter>,
    file_path: Option<&std::path::Path>,
    bg_added: Color,
    bg_deleted: Color,
    search_matches: &[(usize, usize, bool)],
) -> Vec<Line<'static>> {
    use unicode_width::UnicodeWidthStr;
    // ADR-0014: background-color diff rendering. Focused hunks keep
    // full brightness; unfocused hunks wear `Modifier::DIM` so the
    // eye still flows to the cursor band. Context rows use the
    // terminal default inside the focus and dark-gray + DIM outside.
    let bg = match line.kind {
        LineKind::Added => Some(bg_added),
        LineKind::Deleted => Some(bg_deleted),
        LineKind::Context => None,
    };
    let base_style = match (bg, is_selected) {
        (Some(b), true) => Style::default().bg(b),
        (Some(b), false) => Style::default().bg(b).add_modifier(Modifier::DIM),
        (None, true) => Style::default(),
        (None, false) => Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
    };
    // Per-char fg + search highlight kind, built once for the whole
    // line. Distributed across wrapped chunks below so a match that
    // spans a wrap boundary still paints cleanly on both sides.
    let char_fgs = per_char_fg(&line.content, hl, file_path);
    let char_hls = classify_chars_by_match(&line.content, search_matches);

    let chunks = wrap_at_chars(&line.content, body_width.max(1));
    let last_idx = chunks.len().saturating_sub(1);
    let mut char_offset = 0usize;
    let cursor_line = cursor_sub.map(|s| s.min(last_idx));

    chunks
        .into_iter()
        .enumerate()
        .map(|(i, chunk)| {
            let is_last = i == last_idx;
            let bar = cursor_bar(cursor_line == Some(i), is_selected);
            // v0.5 M2: reserve 1 cell only when this is the last
            // visual row AND the DiffLine is EOF-no-newline (the `∅`
            // marker case). Normal newline-terminated rows get no
            // marker and no reservation.
            let marker_reserve = if is_last && !line.has_trailing_newline {
                1
            } else {
                0
            };
            let chunk_char_count = chunk.chars().count();
            // Display width drives the trailing-space pad that extends
            // the delta-style background to the viewport edge. Using
            // char count would leave CJK rows short by one cell per
            // wide char and the bg color would end mid-line.
            let chunk_cell_count = UnicodeWidthStr::width(chunk);
            let pad = body_width.saturating_sub(chunk_cell_count + marker_reserve);

            let mut spans = vec![bar];

            let chunk_fgs = &char_fgs[char_offset..char_offset + chunk_char_count];
            let chunk_hls = &char_hls[char_offset..char_offset + chunk_char_count];
            let chunk_chars: Vec<char> = chunk.chars().collect();
            let mut run_start = 0usize;
            while run_start < chunk_chars.len() {
                let run_attr = (chunk_fgs[run_start], chunk_hls[run_start]);
                let run_end = (run_start + 1..chunk_chars.len())
                    .find(|&j| (chunk_fgs[j], chunk_hls[j]) != run_attr)
                    .unwrap_or(chunk_chars.len());
                let text: String = chunk_chars[run_start..run_end].iter().collect();
                let style = apply_search_overlay(base_style, run_attr.0, run_attr.1);
                spans.push(Span::styled(text, style));
                run_start = run_end;
            }
            if pad > 0 {
                spans.push(Span::styled(" ".repeat(pad), base_style));
            }

            if is_last && !line.has_trailing_newline {
                spans.push(eof_no_newline_span(bg));
            }
            char_offset += chunk_char_count;
            Line::from(spans)
        })
        .collect()
}

/// Bottom-up file header: `  path                                14:03   +12 -3`.
/// Path color encodes the status (cyan / green / red / yellow), no `M`/`A`/`D`
/// label needed.
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
    // Stream mode prefix (e.g. "14:03:22 Write") before the file path.
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
    // Spacing for a future scar indicator placeholder; M4v leaves it
    // dormant so the column stays stable when scar lands.
    spans.push(Span::raw(""));
    Line::from(spans)
}

// ---- v0.5 line-number gutter ----------------------------------------

/// Width configuration for the line-number gutter (v0.5).
///
/// Single-column format for both diff view and file view: `" N "` —
/// 1-cell leading pad, right-aligned number column, 1-cell trailing
/// pad. Earlier revisions used a two-column `OLD|NEW` layout, but
/// user feedback (2026-04-21) was that the doubled numbers on every
/// Context row ("13 13", "14 14", …) looked like a bug ("二重表示").
/// The single column shows only the worktree (new) line number
/// (Context/Added), and Deleted rows get a blank gutter because the
/// line no longer exists in the worktree — mixing `old` baseline
/// numbers in the same column broke monotonicity when earlier hunks
/// shifted subsequent hunks by N lines.
#[derive(Debug, Clone, Copy)]
pub(crate) struct LineNumberGutter {
    pub total_width: usize,
    pub col_width: usize,
}

impl LineNumberGutter {
    /// Single-column gutter with the given column width.
    pub fn single(col_width: usize) -> Self {
        Self {
            total_width: 1 + col_width + 1,
            col_width,
        }
    }

    /// Return a blank span of the full gutter width. Used for wrap
    /// continuation rows and for non-DiffLine rows (HunkHeader,
    /// BinaryNotice, Spacer) that still need the gutter column
    /// reserved so downstream body rendering lines up.
    fn blank_span(&self) -> Span<'static> {
        Span::raw(" ".repeat(self.total_width))
    }
}

/// Diff-view line-number gutter span. Shows the worktree (new-side)
/// line number only:
///
/// - Context → new (the line exists in the current worktree)
/// - Added   → new (same)
/// - Deleted → blank (the line no longer exists in the worktree, so
///   there is no "current" line number to show)
///
/// Earlier revisions mixed `old` and `new` values in the same column
/// so Deleted rows would still render a number. User feedback
/// (2026-04-21) pointed out that when *earlier* hunks in the same
/// file shift N lines, the old-side baseline number on a Deleted row
/// and the new-side worktree number on an adjacent Added row diverge
/// by N, breaking the intuition that "the gutter tracks the file I'm
/// looking at". Showing only the worktree side keeps the column
/// monotonic.
fn diff_ln_span(pair: (Option<usize>, Option<usize>), gutter: &LineNumberGutter) -> Span<'static> {
    let w = gutter.col_width;
    let content = match pair.1 {
        Some(v) => format!(" {v:>w$} "),
        None => " ".repeat(gutter.total_width),
    };
    Span::styled(
        content,
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
    )
}

/// File-view single-column line-number gutter span.
fn file_ln_span(line_number: usize, gutter: &LineNumberGutter) -> Span<'static> {
    Span::styled(
        format!(" {line_number:>w$} ", w = gutter.col_width),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
    )
}

/// Insert a line-number gutter after the 5-cell cursor bar on every
/// rendered visual line. The first visual row gets the supplied
/// number span; wrap continuation rows get blanks so the number is
/// not repeated down the screen.
fn add_line_number_gutters(
    lines: Vec<Line<'static>>,
    first_gutter: Span<'static>,
    gutter: &LineNumberGutter,
) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .enumerate()
        .map(|(i, line)| {
            let ln = if i == 0 {
                first_gutter.clone()
            } else {
                gutter.blank_span()
            };
            insert_gutter_span(line, ln, Span::raw("     "))
        })
        .collect()
}

/// Compute the line-number gutter width for a given max line number.
/// 10 is the lower bound so tiny files stay at a stable 2 digits.
pub(crate) fn line_number_digits(max: usize) -> usize {
    let mut n = max.max(10);
    let mut digits = 0;
    while n > 0 {
        n /= 10;
        digits += 1;
    }
    digits
}

/// Resolve the effective line-number gutter: apply the
/// extreme-narrow fallback so the caller doesn't have to. Returns
/// `(effective_show_ln, ln_gutter_width, ln_gutter)` where
/// `ln_gutter` uses a zero col-width when the gutter is suppressed
/// so blank-slot helpers keep working without separate nil handling.
fn resolve_ln_gutter(
    show_ln: bool,
    max_line_number: usize,
    viewport_width: usize,
) -> (bool, usize, LineNumberGutter) {
    let digits = line_number_digits(max_line_number);
    let raw_width = if show_ln {
        LineNumberGutter::single(digits).total_width
    } else {
        0
    };
    // Fallback: if the gutter would leave < 4 cells of body width,
    // drop it entirely so the user still gets diff content.
    if show_ln && viewport_width >= 5 + raw_width + 4 {
        (true, raw_width, LineNumberGutter::single(digits))
    } else {
        (false, 0, LineNumberGutter::single(0))
    }
}

fn render_hunk_header(
    hunk: &Hunk,
    is_selected: bool,
    is_cursor: bool,
    is_seen: bool,
) -> Line<'static> {
    // v0.4: the seen mark becomes a fold glyph (▸) since a marked
    // hunk is collapsed. Distinct shape + smaller than the cursor
    // `▶` so the two can coexist on the same row without being
    // mistaken for each other.
    let seen_mark = if is_seen { "▸ " } else { "  " };

    // Count added/deleted lines from the actual hunk content in a
    // single pass — render_hunk_header is called once per visible
    // hunk header and a second time for the sticky copy, so even one
    // extra iteration compounds across large hunks.
    let (added, deleted) = hunk
        .lines
        .iter()
        .fold((0usize, 0usize), |(a, d), l| match l.kind {
            LineKind::Added => (a + 1, d),
            LineKind::Deleted => (a, d + 1),
            _ => (a, d),
        });
    let counts = format!("+{added}/-{deleted}");

    // Line range: show new_start (where the change lands in the
    // current file). For pure deletion (new_count == 0) the new side
    // has no range, so fall back to the baseline (old) range so the
    // header stays a useful positional signal — especially since v0.5
    // Deleted rows have a blank gutter (Codex 3rd-round Important-3).
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

    // v0.4: split the left gutter into its own Yellow + Bold span
    // when the cursor is on the header. Keeps the cursor arrow
    // visually consistent with DiffLine rows (same color, same
    // weight) so the eye can track the cursor across the
    // expanded/collapsed boundary without losing it in the Cyan
    // header body.
    // Selected (but not cursor) hunk header matches the DiffLine
    // ribbon color so the whole hunk reads as a focused block.
    let gutter = cursor_bar(is_cursor, is_selected);

    Line::from(vec![gutter, Span::styled(label, label_style)])
}

#[allow(clippy::too_many_arguments)]
fn render_diff_line(
    line: &crate::git::DiffLine,
    is_selected: bool,
    is_cursor: bool,
    body_width: usize,
    hl: Option<&crate::highlight::Highlighter>,
    file_path: Option<&std::path::Path>,
    bg_added: Color,
    bg_deleted: Color,
    search_matches: &[(usize, usize, bool)],
) -> Line<'static> {
    let bg = match line.kind {
        LineKind::Added => Some(bg_added),
        LineKind::Deleted => Some(bg_deleted),
        LineKind::Context => None,
    };
    let bar = cursor_bar(is_cursor, is_selected);
    let base_style = match (bg, is_selected) {
        (Some(b), true) => Style::default().bg(b),
        (Some(b), false) => Style::default().bg(b).add_modifier(Modifier::DIM),
        (None, true) => Style::default(),
        (None, false) => Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
    };

    // Derive a per-char foreground color from syntax tokens when
    // available. Fallback: `Color::Reset` so non-highlighted content
    // inherits the terminal default fg (same look as before the
    // search-overlay refactor). Length matches `content.chars().count()`.
    let char_fgs = per_char_fg(&line.content, hl, file_path);
    let char_hls = classify_chars_by_match(&line.content, search_matches);

    // Walk chars, respect `body_width` in display cells, and group
    // consecutive chars with the same `(fg, hl)` into one span so
    // ratatui doesn't pay span overhead per char.
    //
    // v0.5 M2: reserve the last cell for the EOF-no-newline marker
    // (`∅`) when the DiffLine lacks a trailing newline. The cell is
    // only reserved in the EOF case so normal rows still have the
    // full body width for content + pad.
    let eof_marker = !line.has_trailing_newline;
    let body_budget = if eof_marker {
        body_width.saturating_sub(1)
    } else {
        body_width
    };
    use unicode_width::UnicodeWidthChar;
    let mut spans = vec![bar];
    let mut cells_emitted = 0usize;
    let chars: Vec<char> = line.content.chars().collect();

    let mut run_start = 0usize;
    while run_start < chars.len() {
        let run_attr = (char_fgs[run_start], char_hls[run_start]);
        let mut run_end = run_start + 1;
        let mut run_cells = chars[run_start].width().unwrap_or(0);
        // Short-circuit: don't bother extending the run past the
        // body budget (body_width minus the EOF marker reserve).
        if cells_emitted + run_cells > body_budget {
            break;
        }
        while run_end < chars.len() {
            let candidate_attr = (char_fgs[run_end], char_hls[run_end]);
            if candidate_attr != run_attr {
                break;
            }
            let w = chars[run_end].width().unwrap_or(0);
            if cells_emitted + run_cells + w > body_budget {
                break;
            }
            run_cells += w;
            run_end += 1;
        }
        let text: String = chars[run_start..run_end].iter().collect();
        let style = apply_search_overlay(base_style, run_attr.0, run_attr.1);
        spans.push(Span::styled(text, style));
        cells_emitted += run_cells;
        run_start = run_end;
        if cells_emitted >= body_budget {
            break;
        }
    }

    // Pad to body_budget so the delta-style background extends to the
    // viewport edge (ADR-0014). The padding keeps the base diff bg
    // (no search overlay) so the gutter and trailing cells don't get
    // falsely underlined.
    if cells_emitted < body_budget {
        spans.push(Span::styled(
            " ".repeat(body_budget - cells_emitted),
            base_style,
        ));
    }
    if eof_marker {
        spans.push(eof_no_newline_span(bg));
    }
    Line::from(spans)
}

/// Build a per-char foreground color vector for `content`. Falls back
/// to `Color::Reset` for every char when no highlighter is available
/// or the file extension is unknown — which preserves the pre-overlay
/// terminal-default appearance for plain-text diffs.
fn per_char_fg(
    content: &str,
    hl: Option<&crate::highlight::Highlighter>,
    file_path: Option<&std::path::Path>,
) -> Vec<Color> {
    let n = content.chars().count();
    if let (Some(hl), Some(path)) = (hl, file_path) {
        let tokens = hl.highlight_line(content, path);
        if tokens.len() > 1 || tokens.first().is_some_and(|t| t.fg != Color::Reset) {
            let mut out = Vec::with_capacity(n);
            for tok in &tokens {
                for _ in tok.text.chars() {
                    out.push(tok.fg);
                }
            }
            if out.len() == n {
                return out;
            }
            // Token char count drift (shouldn't happen with syntect)
            // — fall through to the flat-color fallback rather than
            // panicking in the renderer.
        }
    }
    vec![Color::Reset; n]
}

/// Take as many leading chars from `s` as fit into `max_cells` display
/// cells, without splitting a wide char. Returns the prefix and the
/// number of cells actually consumed.
fn take_cells(s: &str, max_cells: usize) -> (String, usize) {
    use unicode_width::UnicodeWidthChar;
    let mut out = String::new();
    let mut cells = 0usize;
    for ch in s.chars() {
        let w = ch.width().unwrap_or(0);
        if cells + w > max_cells {
            break;
        }
        out.push(ch);
        cells += w;
    }
    (out, cells)
}

/// `HH:MM` formatted **local** time. Returns `--:--` when the metadata
/// read failed and the parser left the field at `UNIX_EPOCH`. Uses
/// `libc::localtime_r` on Unix so the picker mtime column matches the
/// user's wall clock; falls back to UTC on non-Unix platforms.
fn format_mtime(t: SystemTime) -> String {
    if t == UNIX_EPOCH {
        return "--:--".to_string();
    }
    let secs = t
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;

    #[cfg(unix)]
    {
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let time_t = secs as libc::time_t;
        unsafe { libc::localtime_r(&time_t, &mut tm) };
        format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
    }

    #[cfg(not(unix))]
    {
        let day_secs = (secs as u64) % 86_400;
        let hour = (day_secs / 3600) as u32;
        let minute = ((day_secs % 3600) / 60) as u32;
        format!("{hour:02}:{minute:02}")
    }
}

fn render_empty(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let short = app
        .baseline_sha
        .get(..7)
        .unwrap_or(&app.baseline_sha)
        .to_string();
    let body = format!("No changes since baseline (baseline: {short})");
    let mid = centered_line(area);
    let p = Paragraph::new(body).alignment(Alignment::Center);
    frame.render_widget(p, mid);
}

fn render_file_view(
    frame: &mut Frame<'_>,
    area: Rect,
    fv: &crate::app::FileViewState,
    wrap_lines: bool,
    show_line_numbers: bool,
    hl: Option<&crate::highlight::Highlighter>,
    effective_top: usize,
) {
    let height = area.height as usize;
    let width = area.width as usize;
    // v0.5: mirror render_scroll's single-source body_width calc so
    // VisualIndex::build_lines and the numbered renderer see the same value
    // (Codex review §Critical-1).
    let (effective_show_ln, ln_gutter_width, ln_gutter) =
        resolve_ln_gutter(show_line_numbers, fv.lines.len(), width);
    let body_width = width.saturating_sub(5 + ln_gutter_width).max(1);
    fv.last_body_width.set(body_width);
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(height);

    // v0.5 M2: draw `∅` on the last line only when the on-disk file
    // is missing a trailing newline. Mid-file rows never get the marker.
    let last_line_idx = fv.lines.len().saturating_sub(1);
    let mark_last_no_newline = !fv.last_line_has_trailing_newline;
    if wrap_lines {
        let vi = crate::app::VisualIndex::build_lines(&fv.lines, Some(body_width));
        let (mut line_idx, mut skip_remaining) = vi.logical_at(effective_top);
        while line_idx < fv.lines.len() && lines.len() < height {
            let base_style = if let Some(&bg) = fv.line_bg.get(&line_idx) {
                Style::default().bg(bg)
            } else {
                Style::default()
            };
            let cursor_sub = (line_idx == fv.cursor).then_some(fv.cursor_sub_row);
            let show_eof_marker = mark_last_no_newline && line_idx == last_line_idx;
            let rendered = render_file_view_line_wrapped(
                &fv.lines[line_idx],
                cursor_sub,
                body_width,
                base_style,
                hl,
                &fv.path,
                show_eof_marker,
            );
            let rendered = if effective_show_ln {
                add_line_number_gutters(
                    rendered,
                    file_ln_span(line_idx + 1, &ln_gutter),
                    &ln_gutter,
                )
            } else {
                rendered
            };
            let mut take = rendered.into_iter();
            for _ in 0..skip_remaining {
                if take.next().is_none() {
                    break;
                }
            }
            skip_remaining = 0;
            for line in take {
                if lines.len() >= height {
                    break;
                }
                lines.push(line);
            }
            line_idx += 1;
        }
    } else {
        for i in 0..height {
            let line_idx = effective_top + i;
            if line_idx >= fv.lines.len() {
                break;
            }
            let base_style = if let Some(&bg) = fv.line_bg.get(&line_idx) {
                Style::default().bg(bg)
            } else {
                Style::default()
            };
            let show_eof_marker = mark_last_no_newline && line_idx == last_line_idx;
            let rendered = render_file_view_line(
                &fv.lines[line_idx],
                line_idx == fv.cursor,
                body_width,
                base_style,
                hl,
                &fv.path,
                show_eof_marker,
            );
            let rendered = if effective_show_ln {
                let mut lines = add_line_number_gutters(
                    vec![rendered],
                    file_ln_span(line_idx + 1, &ln_gutter),
                    &ln_gutter,
                );
                lines.remove(0)
            } else {
                rendered
            };
            lines.push(rendered);
        }
    }

    while lines.len() < height {
        lines.push(Line::from(Span::styled(
            "~",
            Style::default().fg(Color::DarkGray),
        )));
    }

    frame.render_widget(Paragraph::new(lines), area);
}

fn render_file_view_line(
    content: &str,
    is_cursor: bool,
    body_width: usize,
    base_style: Style,
    hl: Option<&crate::highlight::Highlighter>,
    file_path: &std::path::Path,
    show_eof_marker: bool,
) -> Line<'static> {
    let bar = cursor_bar(is_cursor, false);
    // v0.5 M2: reserve 1 cell at the end when we need to paint the
    // EOF-no-newline marker. body_budget governs both content fit
    // and pad so the `∅` lands inside body_width (never overflows).
    let body_budget = if show_eof_marker {
        body_width.saturating_sub(1)
    } else {
        body_width
    };

    if let Some(hl) = hl {
        let tokens = hl.highlight_line(content, file_path);
        if tokens.len() > 1 || tokens.first().is_some_and(|t| t.fg != Color::Reset) {
            let mut spans = vec![bar];
            let mut cells_emitted = 0usize;
            for token in &tokens {
                let remaining = body_budget.saturating_sub(cells_emitted);
                if remaining == 0 {
                    break;
                }
                let (text, token_cells) = take_cells(&token.text, remaining);
                if text.is_empty() {
                    break;
                }
                spans.push(Span::styled(text, base_style.fg(token.fg)));
                cells_emitted += token_cells;
            }
            if cells_emitted < body_budget {
                spans.push(Span::styled(
                    " ".repeat(body_budget - cells_emitted),
                    base_style,
                ));
            }
            if show_eof_marker {
                spans.push(eof_no_newline_span(base_style.bg));
            }
            return Line::from(spans);
        }
    }

    use unicode_width::UnicodeWidthStr;
    let content_cells = UnicodeWidthStr::width(content);
    let padded_body: String = if content_cells >= body_budget {
        let (truncated, _) = take_cells(content, body_budget);
        truncated
    } else {
        let pad = body_budget - content_cells;
        content
            .chars()
            .chain(std::iter::repeat_n(' ', pad))
            .collect()
    };
    let mut spans = vec![bar, Span::styled(padded_body, base_style)];
    if show_eof_marker {
        spans.push(eof_no_newline_span(base_style.bg));
    }
    Line::from(spans)
}

#[allow(clippy::too_many_arguments)]
fn render_file_view_line_wrapped(
    content: &str,
    cursor_sub: Option<usize>,
    body_width: usize,
    base_style: Style,
    hl: Option<&crate::highlight::Highlighter>,
    file_path: &std::path::Path,
    show_eof_marker: bool,
) -> Vec<Line<'static>> {
    use unicode_width::UnicodeWidthStr;

    let tokens: Option<Vec<crate::highlight::HlToken>> = hl.and_then(|hl| {
        let toks = hl.highlight_line(content, file_path);
        (toks.len() > 1 || toks.first().is_some_and(|t| t.fg != Color::Reset)).then_some(toks)
    });

    let char_colors: Vec<Color> = if let Some(ref toks) = tokens {
        let mut colors = Vec::with_capacity(content.len());
        for tok in toks {
            for _ in tok.text.chars() {
                colors.push(tok.fg);
            }
        }
        colors
    } else {
        Vec::new()
    };

    let chunks = wrap_at_chars(content, body_width.max(1));
    let last_idx = chunks.len().saturating_sub(1);
    let cursor_line = cursor_sub.map(|s| s.min(last_idx));
    let mut char_offset = 0usize;

    chunks
        .into_iter()
        .enumerate()
        .map(|(i, chunk)| {
            let bar = cursor_bar(cursor_line == Some(i), cursor_line.is_some());

            let is_last = i == last_idx;
            let emit_marker = show_eof_marker && is_last;
            let body_budget = if emit_marker {
                body_width.saturating_sub(1)
            } else {
                body_width
            };
            let chunk_char_count = chunk.chars().count();
            let chunk_cell_count = UnicodeWidthStr::width(chunk).min(body_budget);
            let pad = body_budget.saturating_sub(chunk_cell_count);
            let mut spans = vec![bar];

            if !char_colors.is_empty() {
                let chunk_colors = &char_colors[char_offset..char_offset + chunk_char_count];
                let mut run_start = 0usize;
                let chunk_chars: Vec<char> = chunk.chars().collect();
                while run_start < chunk_chars.len() {
                    let run_color = chunk_colors[run_start];
                    let run_end = (run_start + 1..chunk_chars.len())
                        .find(|&j| chunk_colors[j] != run_color)
                        .unwrap_or(chunk_chars.len());
                    let text: String = chunk_chars[run_start..run_end].iter().collect();
                    spans.push(Span::styled(text, base_style.fg(run_color)));
                    run_start = run_end;
                }
                if pad > 0 {
                    spans.push(Span::styled(" ".repeat(pad), base_style));
                }
            } else {
                let padded_body: String =
                    chunk.chars().chain(std::iter::repeat_n(' ', pad)).collect();
                spans.push(Span::styled(padded_body, base_style));
            }

            if emit_marker {
                spans.push(eof_no_newline_span(base_style.bg));
            }

            char_offset += chunk_char_count;
            Line::from(spans)
        })
        .collect()
}

fn centered_line(area: Rect) -> Rect {
    let row = area.y + area.height / 2;
    Rect {
        x: area.x,
        y: row,
        width: area.width,
        height: 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::PickerState;
    use crate::git::{DiffContent, DiffLine, FileDiff, FileStatus, Hunk, LineKind};
    use crate::test_support::{
        app_with_file, app_with_files, app_with_hunks, binary_file as timed_binary_file, diff_line,
        file_view_state, hunk, install_search, make_file, single_added_app, single_added_file,
        single_added_hunk_file, single_hunk_app,
    };
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render_buffer(app: &App, w: u16, h: u16) -> ratatui::buffer::Buffer {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| render(f, app)).expect("draw");
        terminal.backend().buffer().clone()
    }

    fn render_to_string(app: &App, w: u16, h: u16) -> String {
        let buffer = render_buffer(app, w, h);
        let mut out = String::new();
        for y in 0..buffer.area().height {
            out.push_str(&buffer_row_text(&buffer, y));
            out.push('\n');
        }
        out
    }

    fn render_footer_text(app: &App, w: u16, h: u16) -> String {
        let buffer = render_buffer(app, w, h);
        buffer_row_text(&buffer, buffer.area().height - 1)
    }

    fn buffer_row_text(buffer: &ratatui::buffer::Buffer, y: u16) -> String {
        let mut out = String::new();
        for x in 0..buffer.area().width {
            out.push_str(buffer[(x, y)].symbol());
        }
        out
    }

    fn first_cell_matching<F>(buffer: &ratatui::buffer::Buffer, f: F) -> Option<(u16, u16)>
    where
        F: Fn(&ratatui::buffer::Cell) -> bool,
    {
        for y in 0..buffer.area().height {
            for x in 0..buffer.area().width {
                if f(&buffer[(x, y)]) {
                    return Some((x, y));
                }
            }
        }
        None
    }

    fn buffer_has_cell<F>(buffer: &ratatui::buffer::Buffer, f: F) -> bool
    where
        F: Fn(&ratatui::buffer::Cell) -> bool,
    {
        first_cell_matching(buffer, f).is_some()
    }

    #[test]
    fn render_empty_state_when_no_files() {
        let app = app_with_files(Vec::new());
        let view = render_to_string(&app, 70, 6);
        assert!(
            view.contains("No changes since baseline (baseline: abcdef1)"),
            "expected empty state with short SHA, got:\n{view}"
        );
        assert!(view.contains("[follow]"));
    }

    // ---- v0.5 line-number gutter -------------------------------------

    #[test]
    fn render_diff_line_numbered_inserts_line_number_span_after_bar() {
        let line = diff_line(LineKind::Context, "hello");
        let gutter = LineNumberGutter::single(2);
        let base = render_diff_line(
            &line,
            false,
            false,
            40,
            None,
            None,
            Color::Reset,
            Color::Reset,
            &[],
        );
        let mut rendered = add_line_number_gutters(
            vec![base],
            diff_ln_span((Some(10), Some(10)), &gutter),
            &gutter,
        );
        let rendered = rendered.remove(0);
        // span 0: 5-cell cursor bar
        // span 1: " 10 " single-column line-number gutter
        // span 2..: body
        assert!(
            rendered.spans.len() >= 3,
            "expected at least 3 spans, got {}",
            rendered.spans.len()
        );
        let ln = rendered.spans[1].content.as_ref();
        assert!(ln.contains("10"), "line-number gutter text: {ln:?}");
        assert!(
            ln.starts_with(' ') && ln.ends_with(' '),
            "gutter must be padded with 1 leading / 1 trailing space: {ln:?}"
        );
        assert_eq!(ln.len(), gutter.total_width);
    }

    #[test]
    fn render_diff_line_numbered_added_row_shows_new_side() {
        let line = diff_line(LineKind::Added, "x");
        let gutter = LineNumberGutter::single(2);
        let base = render_diff_line(
            &line,
            false,
            false,
            40,
            None,
            None,
            Color::Reset,
            Color::Reset,
            &[],
        );
        let mut rendered =
            add_line_number_gutters(vec![base], diff_ln_span((None, Some(11)), &gutter), &gutter);
        let rendered = rendered.remove(0);
        let ln = rendered.spans[1].content.as_ref();
        // Single column shows the new-side line number.
        assert!(ln.contains("11"), "Added row must show new number: {ln:?}");
        assert_eq!(ln.len(), gutter.total_width);
    }

    #[test]
    fn render_diff_line_numbered_deleted_row_leaves_gutter_blank() {
        // Deleted rows no longer exist in the worktree, so there is no
        // "current" line number to print. The gutter must be blank.
        // See `diff_ln_span` docstring for the intuition / reasoning.
        let line = diff_line(LineKind::Deleted, "y");
        let gutter = LineNumberGutter::single(2);
        let base = render_diff_line(
            &line,
            false,
            false,
            40,
            None,
            None,
            Color::Reset,
            Color::Reset,
            &[],
        );
        let mut rendered =
            add_line_number_gutters(vec![base], diff_ln_span((Some(12), None), &gutter), &gutter);
        let rendered = rendered.remove(0);
        let ln = rendered.spans[1].content.as_ref();
        assert!(
            ln.chars().all(|c| c == ' '),
            "Deleted row gutter must be blank: {ln:?}"
        );
        assert_eq!(ln.len(), gutter.total_width);
    }

    #[test]
    fn render_diff_line_wrapped_numbered_continuation_rows_blank_the_gutter() {
        // Long content that will wrap into at least 2 visual rows at
        // body_width=4. Continuation rows must not repeat the number.
        let line = diff_line(LineKind::Context, "aaaaaaaaaa");
        let gutter = LineNumberGutter::single(2);
        let base = render_diff_line_wrapped(
            &line,
            false,
            Some(0),
            4,
            None,
            None,
            Color::Reset,
            Color::Reset,
            &[],
        );
        let rendered =
            add_line_number_gutters(base, diff_ln_span((Some(10), Some(10)), &gutter), &gutter);
        assert!(rendered.len() >= 2, "content must wrap into 2+ rows");
        // First row has numbers.
        let first_ln = rendered[0].spans[1].content.as_ref();
        assert!(
            first_ln.contains("10"),
            "first row must show 10: {first_ln:?}"
        );
        // Continuation row has blank gutter.
        let cont_ln = rendered[1].spans[1].content.as_ref();
        assert!(
            cont_ln.chars().all(|c| c == ' '),
            "continuation row must be all spaces: {cont_ln:?}"
        );
        assert_eq!(cont_ln.len(), gutter.total_width);
    }

    #[test]
    fn render_file_view_line_numbered_shows_single_column() {
        let gutter = LineNumberGutter::single(3);
        let base = render_file_view_line(
            "hello world",
            false,
            40,
            Style::default(),
            None,
            std::path::Path::new("foo.rs"),
            false,
        );
        let mut rendered = add_line_number_gutters(vec![base], file_ln_span(42, &gutter), &gutter);
        let rendered = rendered.remove(0);
        assert!(rendered.spans.len() >= 3);
        let ln = rendered.spans[1].content.as_ref();
        assert!(ln.contains("42"), "file-view gutter: {ln:?}");
        assert_eq!(ln.len(), gutter.total_width);
    }

    #[test]
    fn render_file_view_line_wrapped_numbered_blanks_continuation() {
        let gutter = LineNumberGutter::single(3);
        let base = render_file_view_line_wrapped(
            "aaaaaaaaaa",
            Some(0),
            /*body_width*/ 4,
            Style::default(),
            None,
            std::path::Path::new("foo.rs"),
            false,
        );
        let rendered = add_line_number_gutters(base, file_ln_span(42, &gutter), &gutter);
        assert!(rendered.len() >= 2);
        let cont_ln = rendered[1].spans[1].content.as_ref();
        assert!(
            cont_ln.chars().all(|c| c == ' '),
            "continuation row must be blank: {cont_ln:?}"
        );
    }

    #[test]
    fn sticky_hunk_header_also_reserves_ln_gutter() {
        // Codex 3rd-round Important-1: the sticky header is drawn
        // directly by render_scroll with render_hunk_header(),
        // bypassing render_row's insert_blank_gutter branch. Without
        // an explicit fix the pinned header sits 4 cells left of the
        // scrolling DiffLine bodies whenever LN is on and the cursor
        // is deep enough inside a hunk to activate stickiness.
        let mut app = single_hunk_app(
            "src/foo.rs",
            10,
            // Enough DiffLines that the cursor will scroll past
            // the hunk header and stickiness kicks in.
            (0..30)
                .map(|i| diff_line(LineKind::Context, &format!("line {i}")))
                .collect(),
            100,
        );
        app.show_line_numbers = true;
        app.build_layout();
        // Move cursor deep into the hunk so the header becomes sticky.
        app.scroll = 20;
        let buffer = render_buffer(&app, 80, 10);

        // Sticky header lives at y=0. Its `@@` must share its x with
        // the DiffLine `line` bodies below.
        let top_row: String = (0..buffer.area().width)
            .map(|x| buffer[(x, 0)].symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(
            top_row.contains("@@"),
            "top row must hold the sticky header: {top_row:?}"
        );
        let header_x = top_row.find("@@").unwrap();
        let mut body_x: Option<usize> = None;
        for y in 1..buffer.area().height {
            let row: String = (0..buffer.area().width)
                .map(|x| buffer[(x, y)].symbol().chars().next().unwrap_or(' '))
                .collect();
            if let Some(col) = row.find("line ") {
                body_x = Some(col);
                break;
            }
        }
        let bx = body_x.expect("at least one DiffLine body must be visible");
        // `@@` sits 2 cells to the right of the DiffLine body because
        // render_hunk_header prefixes the label with a 2-cell seen_mark
        // pad ("  " for unseen hunks). Pin the relative offset so the
        // sticky-header alignment can't silently regress.
        assert_eq!(
            (header_x as isize) - (bx as isize),
            2,
            "sticky header '@@' must sit 2 cells right of DiffLine body (header_x={header_x}, body_x={bx})"
        );
    }

    #[test]
    fn ln_gutter_preserves_hunk_header_vs_diff_body_alignment() {
        // Bug (user-reported 2026-04-21): HunkHeader / BinaryNotice
        // must reserve the gutter column when LN is on, otherwise
        // their bodies slide left relative to DiffLine bodies. Pin
        // the invariant: the relative offset between `@@` and the
        // first body glyph must match between LN OFF and LN ON —
        // turning the gutter on shifts *both* by the same amount.
        let make_app = |show_ln: bool| {
            let mut app = single_hunk_app(
                "src/foo.rs",
                10,
                vec![
                    diff_line(LineKind::Context, "fn ok()"),
                    diff_line(LineKind::Added, "x"),
                ],
                100,
            );
            app.show_line_numbers = show_ln;
            app.build_layout();
            app
        };
        let probe = |app: &App| -> (usize, usize) {
            let buffer = render_buffer(app, 80, 12);
            let mut header_x: Option<usize> = None;
            let mut body_x: Option<usize> = None;
            for y in 0..buffer.area().height {
                let row = buffer_row_text(&buffer, y);
                if header_x.is_none()
                    && let Some(col) = row.find("@@")
                {
                    header_x = Some(col);
                }
                if body_x.is_none()
                    && let Some(col) = row.find("fn ok()")
                {
                    body_x = Some(col);
                }
            }
            (header_x.expect("@@"), body_x.expect("fn ok()"))
        };
        let (off_h, off_b) = probe(&make_app(false));
        let (on_h, on_b) = probe(&make_app(true));
        // Both rows must shift by the same gutter width — the relative
        // offset between `@@` and `fn` is invariant under the toggle.
        assert_eq!(
            (on_h as isize - off_h as isize),
            (on_b as isize - off_b as isize),
            "gutter toggle must shift hunk header and diff body by the same amount (off: @@={off_h}, fn={off_b}; on: @@={on_h}, fn={on_b})"
        );
        // Sanity: LN ON actually adds width (otherwise the invariant
        // holds trivially).
        assert!(on_b > off_b, "LN ON must widen the left gutter");
    }

    #[test]
    fn render_scroll_shows_line_numbers_when_enabled() {
        // v0.5 end-to-end: `show_line_numbers=true` must put a
        // right-aligned worktree line number in the gutter of every
        // Context / Added row.
        let mut app = single_hunk_app(
            "src/foo.rs",
            10,
            vec![
                diff_line(LineKind::Context, "fn ok()"),
                diff_line(LineKind::Added, "let x = 1;"),
            ],
            100,
        );
        app.show_line_numbers = true;
        app.build_layout();
        let view = render_to_string(&app, 80, 12);
        // Context row: new=10 → " 10 " in the gutter.
        // Added row: new=10 as well (new_count starts at new_start for
        // the first Added when old_count=0 → see git.rs:line_numbers_for).
        assert!(
            view.contains(" 10 "),
            "Context/Added row must show the worktree line number:\n{view}"
        );
    }

    #[test]
    fn render_scroll_omits_line_numbers_in_stream_mode_even_when_enabled() {
        // Codex review §Critical-2: Stream mode FileDiffs carry
        // synthetic old_start/new_start values that are not real file
        // line numbers. The renderer must suppress the gutter.
        let mut app = single_hunk_app(
            "src/foo.rs",
            100, // new_start=100 so any LN artifact would be visible
            vec![diff_line(LineKind::Added, "x")],
            100,
        );
        app.show_line_numbers = true;
        app.view_mode = crate::app::ViewMode::Stream;
        app.build_layout();
        let view = render_to_string(&app, 80, 12);
        // No "100" glyph should appear as a line-number gutter (it
        // might still appear in the hunk header `L100` — let's pin
        // something more specific: no `"100"` as a right-aligned
        // gutter value with a trailing separator).
        assert!(
            !view.contains(" 100 "),
            "Stream mode must not render line-number gutter:\n{view}"
        );
    }

    #[test]
    fn render_scroll_drops_gutter_when_viewport_is_extremely_narrow() {
        // Codex review §Critical-2: at widths where 5 (cursor bar)
        // + gutter + 4 (min body) cannot fit, the renderer must
        // silently fall back to the no-gutter layout so the user
        // still sees diff content.
        let mut app = app_with_file(single_added_hunk_file("src/foo.rs", 10, "xyz", 100));
        app.show_line_numbers = true;
        app.build_layout();
        // Width 9 is too small: 5 + 9 (gutter) + 4 > 9. Fallback
        // forces ln off, body_width = 9 - 5 = 4.
        let view = render_to_string(&app, 12, 4);
        // No "10" as a gutter number should appear in this narrow view.
        // The fixture's new_start is 10 so we'd see it only if the
        // fallback failed.
        assert!(
            !view.contains(" 10 "),
            "narrow viewport must drop the gutter:\n{view}"
        );
    }

    #[test]
    fn line_number_digits_clamps_to_lower_bound_of_two() {
        assert_eq!(line_number_digits(0), 2);
        assert_eq!(line_number_digits(1), 2);
        assert_eq!(line_number_digits(9), 2);
        assert_eq!(line_number_digits(10), 2);
        assert_eq!(line_number_digits(99), 2);
        assert_eq!(line_number_digits(100), 3);
        assert_eq!(line_number_digits(9999), 4);
    }

    #[test]
    fn render_scroll_shows_file_header_hunk_header_and_diff_line() {
        let app = single_hunk_app(
            "src/foo.rs",
            10,
            vec![
                diff_line(LineKind::Context, "fn ok()"),
                diff_line(LineKind::Added, "let x = 1;"),
                diff_line(LineKind::Deleted, "let y = 2;"),
            ],
            100,
        );
        let view = render_to_string(&app, 80, 12);
        assert!(view.contains("src/foo.rs"), "missing file header:\n{view}");
        // New hunk header format: @@ L<range> +N/-M
        assert!(
            view.contains("@@ L10"),
            "missing hunk header line range:\n{view}"
        );
        assert!(
            view.contains("+1/-1"),
            "missing hunk header counts:\n{view}"
        );
        // ADR-0014: no `+`/`-` prefix; the body text appears bare on
        // the row and the add/delete signal lives in the background.
        assert!(view.contains("let x = 1;"), "missing added line:\n{view}");
        assert!(view.contains("let y = 2;"), "missing deleted line:\n{view}");
    }

    #[test]
    fn render_scroll_lines_use_background_color_for_added_and_deleted() {
        // ADR-0014: delta-style background color for diff rows.
        //
        // The `+`/`-` prefix column is gone; added/deleted rows are
        // identified purely by their background color. We assert that
        // (a) the body text carries a green or red `bg` Style and
        // (b) no literal `+`/`-` prefix cell appears on the row body.
        let app = single_hunk_app(
            "src/foo.rs",
            1,
            vec![
                diff_line(LineKind::Added, "x"),
                diff_line(LineKind::Deleted, "y"),
            ],
            100,
        );
        let buffer = render_buffer(&app, 80, 12);

        let found_added_bg = buffer_has_cell(&buffer, |cell| {
            cell.symbol() == "x" && cell.style().bg == Some(BG_ADDED)
        });
        let found_deleted_bg = buffer_has_cell(&buffer, |cell| {
            cell.symbol() == "y" && cell.style().bg == Some(BG_DELETED)
        });
        assert!(
            found_added_bg,
            "expected an added 'x' cell with green background"
        );
        assert!(
            found_deleted_bg,
            "expected a deleted 'y' cell with red background"
        );
    }

    #[test]
    fn nowrap_added_row_background_extends_to_viewport_edge() {
        // ADR-0014: the delta-style coloured band must run from the
        // first body cell to the last cell of the viewport, even
        // when the diff content is shorter than the width. This
        // tests nowrap mode (the default). If the padding logic
        // breaks, the right edge of a short added row will fall
        // back to the terminal default background.
        let app = single_added_app("a.rs", "tiny");
        let width: u16 = 40;
        let buffer = render_buffer(&app, width, 12);

        // Find the row that contains the "tiny" body text.
        let mut tiny_y: Option<u16> = None;
        for y in 0..buffer.area().height {
            let row = buffer_row_text(&buffer, y);
            if row.contains("tiny") {
                tiny_y = Some(y);
                break;
            }
        }
        let y = tiny_y.expect("tiny row must render somewhere");

        // Every cell from the start of the body region (x = 5) to
        // the last column must carry BG_ADDED.
        for x in 5..width {
            let cell = &buffer[(x, y)];
            assert_eq!(
                cell.style().bg,
                Some(BG_ADDED),
                "cell (x={x}, y={y}) lost the added background; symbol = {:?}",
                cell.symbol()
            );
        }
    }

    #[test]
    fn render_scroll_lines_omit_plus_minus_prefix() {
        // ADR-0014: there must be no `+`/`-` prefix cells in the diff
        // body region (rows past the 5-char bar margin). The background
        // color encodes add/delete instead.
        let app = single_hunk_app(
            "src/foo.rs",
            1,
            vec![
                diff_line(LineKind::Added, "ADDED_LINE"),
                diff_line(LineKind::Deleted, "DELETED_LINE"),
            ],
            100,
        );
        let view = render_to_string(&app, 80, 12);
        assert!(
            !view.contains("+ADDED_LINE"),
            "must not carry a `+` prefix next to added body:\n{view}"
        );
        assert!(
            !view.contains("-DELETED_LINE"),
            "must not carry a `-` prefix next to deleted body:\n{view}"
        );
    }

    #[test]
    fn wrap_mode_does_not_show_any_marker_when_terminal_newline_present() {
        // v0.5 M2 (plan v0.5-newline-marker.md): the former `¶` marker
        // drew on every row with has_trailing_newline=true, which is
        // ~99% of rows. Now common rows carry no marker at all; only
        // EOF-no-newline rows get a Yellow `∅`. Pin the new default:
        // normal rows must be glyph-free.
        let long_content: String = (0..120u8).map(|i| (b'a' + (i % 26)) as char).collect();
        let mut app = single_added_app("a.rs", &long_content);
        app.wrap_lines = true;

        let view = render_to_string(&app, 80, 14);
        assert!(
            !view.contains("¶"),
            "v0.5 M2: `¶` must no longer appear on normal wrap rows:\n{view}"
        );
        assert!(
            !view.contains("∅"),
            "no EOF marker when has_trailing_newline=true:\n{view}"
        );
        // The second half of the content must still be visible.
        assert!(
            view.contains(&long_content[90..110]),
            "expected wrapped continuation to be visible:\n{view}"
        );
    }

    #[test]
    fn render_diff_line_nowrap_cjk_pads_to_cell_width_not_char_count() {
        // Fallback (no highlighter) path: 5 kanji = 5 chars = 10 cells.
        // At body_width=20 cells the padded body must be exactly 20
        // cells wide — not 20-5=15 pad chars tacked on (which would
        // produce a 25-cell body and bleed past the viewport).
        use unicode_width::UnicodeWidthStr;
        let line = diff_line(LineKind::Added, "あいうえお");
        let rendered = super::render_diff_line(
            &line,
            false,
            false,
            20,
            None,
            None,
            Color::Rgb(10, 50, 10),
            Color::Rgb(60, 10, 10),
            &[],
        );
        // Skip the 5-cell left bar; the remaining spans make up the body.
        let body_cells: usize = rendered
            .spans
            .iter()
            .skip(1)
            .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
            .sum();
        assert_eq!(
            body_cells, 20,
            "nowrap CJK body must pad to body_width in cells, got {body_cells} cells",
        );
    }

    #[test]
    fn wrap_at_chars_respects_cjk_display_width() {
        // Each kanji in `日本語テスト` has display width 2. At a body
        // width of 4 cells we must emit 2 kanji per chunk — NOT 4
        // kanji (which would overflow to 8 cells and let the chunk
        // spill beyond the viewport in wrap mode).
        let chunks = super::wrap_at_chars("日本語テスト", 4);
        assert_eq!(chunks, vec!["日本", "語テ", "スト"]);
    }

    #[test]
    fn wrap_at_chars_handles_mixed_ascii_and_cjk() {
        // `ab漢字` = 1+1+2+2 = 6 cells. At width 4, the first chunk
        // fits `ab漢` (1+1+2=4 cells exactly); `字` starts a new chunk.
        let chunks = super::wrap_at_chars("ab漢字cd", 4);
        assert_eq!(chunks, vec!["ab漢", "字cd"]);
    }

    #[test]
    fn wrap_mode_cjk_line_wraps_within_viewport() {
        // 40 kanji at width 40 (viewport cell width): the line must
        // wrap to 2 rows since each kanji is 2 cells wide. Previously
        // this rendered as a single over-wide row that ratatui
        // silently truncated or that bled past the viewport.
        let forty_kanji: String = "あいうえおかきくけこ".repeat(4);
        assert_eq!(forty_kanji.chars().count(), 40);
        let mut app = single_added_app("a.rs", &forty_kanji);
        app.wrap_lines = true;

        // Viewport: 45 cols (5 for left bar + 40 for body).
        let view = render_to_string(&app, 45, 10);
        // Both the first kanji and a kanji past the 20th position
        // must be visible: if wrap worked correctly, both halves of
        // the content appear on separate visual rows.
        assert!(view.contains("あ"), "first kanji must be visible:\n{view}");
        // The 35th kanji (well past the 20-kanji midpoint) must also
        // land in the viewport — only possible if the line actually
        // wrapped rather than being truncated at column 40.
        let late_kanji: char = forty_kanji.chars().nth(35).unwrap();
        assert!(
            view.contains(late_kanji),
            "late CJK char {late_kanji:?} must survive wrap:\n{view}"
        );
    }

    #[test]
    fn wrap_mode_shows_eof_marker_when_no_terminal_newline() {
        // v0.5 M2: has_trailing_newline=false is the git `\ No newline
        // at end of file` case. The new EOF marker is `∅` in Yellow.
        // Pin the *presence* of `∅` and the *absence* of the legacy `¶`.
        let long_content: String = (0..40u8).map(|i| (b'a' + (i % 26)) as char).collect();
        let mut file = single_added_file("a.rs", &long_content, 100);
        let DiffContent::Text(hunks) = &mut file.content else {
            panic!("expected text diff");
        };
        hunks[0].lines[0].has_trailing_newline = false;

        let mut app = app_with_file(file);
        app.wrap_lines = true;
        let view = render_to_string(&app, 40, 10);
        assert!(
            view.contains("∅"),
            "EOF-no-newline must render a `∅` marker in wrap mode:\n{view}"
        );
        assert!(!view.contains("¶"), "legacy `¶` must be gone:\n{view}");
    }

    #[test]
    fn nowrap_mode_shows_eof_marker_when_no_terminal_newline() {
        // v0.5 M2: EOF-no-newline information is independent of wrap
        // mode. The marker must appear in nowrap as well.
        let mut file = single_added_file("a.rs", "short", 100);
        let DiffContent::Text(hunks) = &mut file.content else {
            panic!("expected text diff");
        };
        hunks[0].lines[0].has_trailing_newline = false;

        let mut app = app_with_file(file);
        app.wrap_lines = false;
        let view = render_to_string(&app, 80, 10);
        assert!(
            view.contains("∅"),
            "EOF-no-newline must render a `∅` marker in nowrap mode:\n{view}"
        );
    }

    #[test]
    fn nowrap_mode_omits_marker_for_normal_line() {
        // Symmetric to the wrap "normal row" test: no marker on a
        // has_trailing_newline=true nowrap row.
        let mut app = single_added_app("a.rs", "short");
        app.wrap_lines = false;
        let view = render_to_string(&app, 80, 10);
        assert!(
            !view.contains("¶") && !view.contains("∅"),
            "normal nowrap row must carry no end-of-line marker:\n{view}"
        );
    }

    #[test]
    fn line_numbers_hint_appears_in_footer_and_marks_state() {
        // v0.5 plan §Important-4 (Codex): wrap/`z` hints are already
        // visible in the footer, so the LN toggle must be too.
        let mut app = single_added_app("a.rs", "x");
        // OFF by default → still shows a hint (just without bold).
        let off_view = render_to_string(&app, 80, 8);
        assert!(
            off_view.contains("nums off"),
            "footer must spell out the disabled LN state:\n{off_view}"
        );

        // ON → state must be visible in text, not only in bold styling.
        app.show_line_numbers = true;
        app.build_layout();
        let on_view = render_to_string(&app, 80, 8);
        assert!(
            on_view.contains("nums on"),
            "footer must spell out the enabled LN state:\n{on_view}"
        );
    }

    #[test]
    fn line_numbers_hint_marks_stream_mode_as_off() {
        // Stream mode always suppresses the gutter, so the footer
        // should make that explicit with an `(off)` marker no matter
        // what `show_line_numbers` is.
        let mut app = single_added_app("a.rs", "x");
        app.show_line_numbers = true;
        app.view_mode = crate::app::ViewMode::Stream;
        app.build_layout();
        let view = render_to_string(&app, 80, 8);
        assert!(
            view.contains("nums") && view.contains("off"),
            "stream footer must flag LN as disabled:\n{view}"
        );
    }

    #[test]
    fn wrap_nowrap_indicator_appears_in_footer() {
        let mut app = single_added_app("a.rs", "x");
        let nowrap_view = render_to_string(&app, 80, 8);
        assert!(nowrap_view.contains("nowrap"));

        app.wrap_lines = true;
        let wrap_view = render_to_string(&app, 80, 8);
        assert!(wrap_view.contains("wrap"));
        assert!(!wrap_view.contains("nowrap"));
    }

    #[test]
    fn responsive_footer_keeps_state_not_keymap_when_normal_mode_is_narrow() {
        let mut app = single_added_app(
            "src/extremely/long/path/that/pushes/status/content/out/of/sight/component.rs",
            "x",
        );
        app.follow_mode = false;
        app.wrap_lines = true;
        app.show_line_numbers = true;
        app.head_dirty = true;
        app.build_layout();

        let footer = render_footer_text(&app, 64, 8);
        assert!(footer.contains("[manual]"), "missing mode:\n{footer}");
        assert!(
            footer.contains("wrap"),
            "narrow footer must keep wrap state visible without relying on key labels:\n{footer}"
        );
        assert!(
            footer.contains("nums"),
            "narrow footer must keep line-number state visible without relying on key labels:\n{footer}"
        );
        assert!(
            !footer.contains("w wrap"),
            "footer should not carry keymap:\n{footer}"
        );
        assert!(
            !footer.contains("# nums"),
            "footer should not carry keymap:\n{footer}"
        );
        assert!(
            !footer.contains("picker"),
            "narrow footer should drop verbose low-priority labels first:\n{footer}"
        );
    }

    #[test]
    fn responsive_footer_keeps_back_hint_when_file_view_path_is_long() {
        let mut app = app_with_files(Vec::new());
        app.file_view = Some(file_view_state(
            "src/extremely/long/path/that/would/otherwise/hide/the/back/hint/demo.rs",
            vec!["first".into(), "second".into(), "third".into()],
            1,
            true,
        ));
        app.wrap_lines = true;
        app.show_line_numbers = true;

        let footer = render_footer_text(&app, 56, 8);
        assert!(
            footer.contains("[file") || footer.contains("[file view]"),
            "missing file-view mode:\n{footer}"
        );
        assert!(
            footer.contains("wrap"),
            "file-view footer must keep wrap state visible:\n{footer}"
        );
        assert!(
            footer.contains("nums"),
            "file-view footer must keep line-number state visible:\n{footer}"
        );
        assert!(
            footer.contains("Esc") || footer.contains("back"),
            "file-view footer must keep the back hint visible:\n{footer}"
        );
    }

    #[test]
    fn help_overlay_uses_configured_key_labels() {
        let mut app = single_added_app("src/foo.rs", "x");
        app.follow_mode = false;
        app.config.keys.cursor_placement = 'Z';
        app.config.keys.wrap_toggle = 'W';
        app.config.keys.line_numbers_toggle = 'L';
        app.config.keys.picker = 'p';
        app.help_overlay = true;

        let view = render_to_string(&app, 100, 24);
        assert!(
            view.contains("Z") && view.contains("center"),
            "help overlay must show remapped cursor-placement key:\n{view}"
        );
        assert!(
            view.contains("W") && view.contains("wrap"),
            "help overlay must show remapped wrap key:\n{view}"
        );
        assert!(
            view.contains("L") && view.contains("line numbers"),
            "help overlay must show remapped line-number key:\n{view}"
        );
        assert!(
            view.contains("p") && view.contains("picker"),
            "help overlay must show remapped picker key:\n{view}"
        );
    }

    #[test]
    fn file_view_marks_eof_no_newline_on_last_line_nowrap() {
        // v0.5 M2: when the on-disk file lacks a trailing LF, the
        // file view must draw `∅` at the end of the final line.
        // Non-last lines never get the marker, regardless of
        // `last_line_has_trailing_newline`.
        let mut app = app_with_files(Vec::new());
        app.file_view = Some(file_view_state(
            "foo.rs",
            vec!["first".into(), "tail-no-newline".into()],
            1,
            false,
        ));
        app.wrap_lines = false;
        let view = render_to_string(&app, 40, 8);
        assert!(
            view.contains("∅"),
            "file view must mark EOF-no-newline on the last line:\n{view}"
        );
    }

    #[test]
    fn file_view_omits_marker_when_last_line_has_newline() {
        let mut app = app_with_files(Vec::new());
        app.file_view = Some(file_view_state(
            "foo.rs",
            vec!["first".into(), "last".into()],
            1,
            true,
        ));
        app.wrap_lines = false;
        let view = render_to_string(&app, 40, 8);
        assert!(
            !view.contains("∅") && !view.contains("¶"),
            "normal file must carry no EOF marker:\n{view}"
        );
    }

    #[test]
    fn file_view_wrap_mode_renders_late_content_and_footer_indicator() {
        let long = format!("const DATA: &str = {:?};", "0123456789".repeat(12));
        let mut app = app_with_files(Vec::new());
        app.file_view = Some(file_view_state("src/demo.rs", vec![long.clone()], 0, true));
        app.wrap_lines = true;

        let view = render_to_string(&app, 40, 8);
        assert!(
            view.contains(&long[45..65]),
            "wrapped file view must surface a later slice of the long line:\n{view}"
        );
        assert!(
            view.contains("[file view]"),
            "file view footer missing:\n{view}"
        );
        assert!(view.contains("wrap"), "wrap indicator missing:\n{view}");
    }

    #[test]
    fn render_scroll_marks_binary_file_with_notice() {
        let app = app_with_file(timed_binary_file("assets/icon.png", 0));
        let view = render_to_string(&app, 80, 8);
        assert!(view.contains("assets/icon.png"));
        assert!(view.contains("[binary file - diff suppressed]"));
        assert!(view.contains("bin"));
    }

    #[test]
    fn render_picker_overlays_a_box_with_query_and_filtered_list() {
        let mut app = app_with_files(vec![
            single_added_file("src/auth.rs", "x", 300),
            single_added_file("src/handler.rs", "y", 200),
            single_added_file("tests/auth_test.rs", "z", 100),
        ]);
        app.picker = Some(PickerState {
            query: "auth".into(),
            cursor: 0,
        });

        let view = render_to_string(&app, 90, 14);
        // Query input rendered at the top of the popup
        assert!(view.contains("> auth"), "missing query line:\n{view}");
        // Two filtered files are visible
        assert!(view.contains("src/auth.rs"), "missing src/auth.rs:\n{view}");
        assert!(
            view.contains("tests/auth_test.rs"),
            "missing tests/auth_test.rs:\n{view}"
        );
        // The non-matching file should NOT be inside the picker popup.
        // It might still appear in the underlying scroll view; the popup
        // header should advertise the filtered count.
        assert!(view.contains("Files 2/3"), "missing files counter:\n{view}");
        // Footer switches to picker hint copy
        assert!(view.contains("[picker]"));
        assert!(view.contains("type to filter"));
        assert!(view.contains("Esc"));
    }

    #[test]
    fn render_footer_shows_last_error_in_red_when_set() {
        let mut app = single_added_app("src/foo.rs", "x");
        app.last_error = Some("git diff exploded".into());

        // Wide enough that the footer's body + error message both fit.
        let buffer = render_buffer(&app, 140, 6);

        let footer_y = buffer.area().height - 1;
        let footer_text = buffer_row_text(&buffer, footer_y);
        let had_red_x = (0..buffer.area().width).any(|x| {
            let cell = &buffer[(x, footer_y)];
            cell.symbol() == "×" && cell.style().fg == Some(Color::Red)
        });
        assert!(
            footer_text.contains("git diff exploded"),
            "footer:\n{footer_text}"
        );
        assert!(
            had_red_x,
            "expected red '×' marker before the error message"
        );
    }

    #[test]
    fn render_footer_shows_source_aware_watcher_failures() {
        let mut app = single_added_app("src/foo.rs", "x");
        app.watcher_health.record_failure(
            crate::watcher::WatchSource::GitRefs,
            "watcher [git.refs]: refs watcher dead".into(),
        );
        app.watcher_health.record_failure(
            crate::watcher::WatchSource::Worktree,
            "watcher [worktree]: worktree watcher dead".into(),
        );

        // v0.5: the footer grew a `# nums` segment, so the viewport
        // needs a little more room before the worktree warning is
        // truncated at the right edge.
        let view = render_to_string(&app, 200, 6);
        assert!(
            view.contains("⚠ WATCHER"),
            "missing watcher warning:\n{view}"
        );
        assert!(
            view.contains("watcher [git.refs]: refs watcher dead"),
            "missing git watcher message:\n{view}"
        );
        assert!(
            view.contains("watcher [worktree]: worktree"),
            "missing worktree watcher message:\n{view}"
        );
    }

    #[test]
    fn render_footer_shows_input_health_warning() {
        let mut app = single_added_app("src/foo.rs", "x");
        app.input_health = Some("input: stream hiccup".into());

        let view = render_to_string(&app, 140, 6);
        assert!(view.contains("⚠ INPUT"), "missing input warning:\n{view}");
        assert!(
            view.contains("input: stream hiccup"),
            "missing input health message:\n{view}"
        );
    }

    #[test]
    fn render_footer_switches_to_manual_when_follow_mode_off() {
        let mut app = single_added_app("src/foo.rs", "x");
        app.follow_mode = false;
        let view = render_to_string(&app, 80, 6);
        assert!(view.contains("[manual]"), "expected [manual]:\n{view}");
    }

    fn hunk_with_context(old_start: usize, ctx: &str, lines: Vec<DiffLine>) -> Hunk {
        let mut hunk = hunk(old_start, lines);
        hunk.context = Some(ctx.to_string());
        hunk
    }

    fn modified_file_status(name: &str, status: FileStatus, secs: u64) -> FileDiff {
        let mut file = make_file(
            name,
            vec![hunk(1, vec![diff_line(LineKind::Added, "x")])],
            secs,
        );
        file.status = status;
        file
    }

    #[test]
    fn file_header_path_color_encodes_status() {
        let mut app = app_with_files(vec![
            modified_file_status("a.rs", FileStatus::Modified, 100),
            modified_file_status("b.rs", FileStatus::Added, 200),
            modified_file_status("c.rs", FileStatus::Deleted, 300),
            modified_file_status("d.rs", FileStatus::Untracked, 400),
        ]);
        // Bootstrap parks scroll on the follow target (bottom). Reset to 0
        // so every file header sits inside the viewport for inspection.
        app.scroll_to(0);
        let buffer = render_buffer(&app, 80, 30);

        // For each path, find its first cell (the 'a'/'b'/'c'/'d') and
        // assert the foreground color matches the status mapping.
        let want = [
            ("a.rs", Color::Cyan),
            ("b.rs", Color::Green),
            ("c.rs", Color::Red),
            ("d.rs", Color::Yellow),
        ];
        for (name, expected) in want {
            let (x, y, _) = find_text_runs(&buffer, name)
                .into_iter()
                .next()
                .unwrap_or_else(|| panic!("{name} not found in buffer"));
            assert_eq!(
                buffer[(x, y)].style().fg,
                Some(expected),
                "{name} should be in {expected:?}"
            );
        }
    }

    #[test]
    fn file_header_shows_prefix_when_set() {
        let mut file = single_added_file("src/auth.rs", "x", 100);
        file.header_prefix = Some("14:03:22 Write".to_string());
        let mut app = app_with_file(file);
        app.scroll = 0;

        let buffer = render_buffer(&app, 60, 10);

        // The prefix "14:03:22 Write" should appear in the header line.
        let first_line = buffer_row_text(&buffer, 0);
        assert!(
            first_line.contains("14:03:22 Write"),
            "header should contain prefix, got: {first_line:?}"
        );
    }

    #[test]
    fn hunk_header_uses_function_context_when_available() {
        let app = app_with_hunks(
            "src/auth.rs",
            vec![hunk_with_context(
                10,
                "fn verify_token(claims: &Claims) -> Result<bool> {",
                vec![diff_line(LineKind::Added, "let x = 1;")],
            )],
            100,
        );
        let view = render_to_string(&app, 100, 14);
        assert!(
            view.contains("@@ fn verify_token(claims: &Claims) -> Result<bool> {"),
            "expected xfuncname header, got:\n{view}"
        );
        // The literal `@@ -10,X +10,Y @@` form should NOT appear once
        // context is available.
        assert!(
            !view.contains("@@ -10,0 +10,1 @@"),
            "old hunk-range header leaked through:\n{view}"
        );
    }

    #[test]
    fn hunk_header_shows_line_range_and_counts() {
        // Hunk header should display line number range and +/-
        // counts alongside the function context.
        let app = app_with_hunks(
            "a.rs",
            vec![Hunk {
                old_start: 10,
                old_count: 2,
                new_start: 10,
                new_count: 5,
                lines: vec![
                    diff_line(LineKind::Context, "ok"),
                    diff_line(LineKind::Added, "new1"),
                    diff_line(LineKind::Added, "new2"),
                    diff_line(LineKind::Added, "new3"),
                    diff_line(LineKind::Deleted, "old1"),
                ],
                context: Some("fn example()".to_string()),
            }],
            100,
        );
        let view = render_to_string(&app, 100, 14);
        // Should contain the line range.
        assert!(
            view.contains("L10"),
            "expected line range L10, got:\n{view}"
        );
        // Should contain the change counts.
        assert!(
            view.contains("+3/-1"),
            "expected +3/-1 counts, got:\n{view}"
        );
    }

    #[test]
    fn hunk_header_pure_deletion_uses_baseline_range_not_l0() {
        // Codex 3rd-round Important-3: a hunk that removes lines from
        // the top of the file ends up with new_start=0 / new_count=0.
        // The previous range formula `L{new_start}-{new_start + new_count - 1}`
        // would render "L0-?" (underflow or nonsense) and, now that
        // Deleted DiffLine rows have a blank gutter, the header was
        // the only positional signal left. Fall back to the baseline
        // range so the reader can still locate the removal.
        let app = app_with_hunks(
            "a.rs",
            vec![Hunk {
                old_start: 1,
                old_count: 3,
                new_start: 0,
                new_count: 0,
                lines: vec![
                    diff_line(LineKind::Deleted, "gone1"),
                    diff_line(LineKind::Deleted, "gone2"),
                    diff_line(LineKind::Deleted, "gone3"),
                ],
                context: None,
            }],
            100,
        );
        let view = render_to_string(&app, 100, 14);
        assert!(
            !view.contains("L0"),
            "pure deletion must not render L0 as the header range:\n{view}"
        );
        // Baseline range should appear instead (old_start .. old_start+old_count-1).
        assert!(
            view.contains("L1-3") || view.contains("L1"),
            "pure deletion header must fall back to baseline range:\n{view}"
        );
    }

    #[test]
    fn hunk_header_shows_range_in_fallback_format() {
        // When no function context is available, the header should
        // still show line range and counts.
        let app = app_with_hunks(
            "a.rs",
            vec![Hunk {
                old_start: 5,
                old_count: 1,
                new_start: 5,
                new_count: 3,
                lines: vec![
                    diff_line(LineKind::Added, "x"),
                    diff_line(LineKind::Added, "y"),
                    diff_line(LineKind::Deleted, "z"),
                ],
                context: None,
            }],
            100,
        );
        let view = render_to_string(&app, 100, 14);
        assert!(view.contains("L5"), "expected line range L5, got:\n{view}");
        assert!(
            view.contains("+2/-1"),
            "expected +2/-1 counts, got:\n{view}"
        );
    }

    #[test]
    fn selected_hunk_is_bright_and_unselected_hunk_is_dim() {
        // ADR-0014: the delta-style bg color is the same for both
        // focused and unfocused add rows — the contrast comes from
        // `Modifier::DIM` on the unfocused hunk (plus the left bar
        // `▎` / `▶` on the focused one). This pins both halves of
        // that contract so a future refactor can't silently flatten
        // the focus signal.
        //
        // We use content-only characters ('~', '!') that don't appear
        // in file paths, hunk headers (`@@`), or scroll chrome so the
        // cell lookup doesn't collide.
        let mut app = app_with_hunks(
            "a.rs",
            vec![
                hunk(1, vec![diff_line(LineKind::Added, "~~~~")]),
                hunk(20, vec![diff_line(LineKind::Added, "!!!!")]),
            ],
            100,
        );
        // Snap to the first hunk so one hunk is focused and the other
        // is not.
        app.scroll_to(app.layout.hunk_starts[0]);
        let buffer = render_buffer(&app, 100, 14);

        // Both body characters must sit on BG_ADDED (the colored
        // band never disappears), but the unfocused row must also
        // carry `Modifier::DIM` while the focused row must not.
        let mut focused_cells: Vec<(Option<Color>, Modifier)> = Vec::new();
        let mut unfocused_cells: Vec<(Option<Color>, Modifier)> = Vec::new();
        for y in 0..buffer.area().height {
            for x in 5..buffer.area().width {
                let cell = &buffer[(x, y)];
                let style = cell.style();
                if cell.symbol() == "~" {
                    focused_cells.push((style.bg, style.add_modifier));
                }
                if cell.symbol() == "!" {
                    unfocused_cells.push((style.bg, style.add_modifier));
                }
            }
        }
        assert!(
            !focused_cells.is_empty(),
            "focused hunk body '~' never rendered"
        );
        assert!(
            !unfocused_cells.is_empty(),
            "unfocused hunk body '!' never rendered"
        );
        // Focused: BG_ADDED, no DIM.
        assert!(
            focused_cells
                .iter()
                .all(|(bg, m)| *bg == Some(BG_ADDED) && !m.contains(Modifier::DIM)),
            "focused hunk must be BG_ADDED without DIM, got {focused_cells:?}"
        );
        // Unfocused: BG_ADDED, with DIM.
        assert!(
            unfocused_cells
                .iter()
                .all(|(bg, m)| *bg == Some(BG_ADDED) && m.contains(Modifier::DIM)),
            "unfocused hunk must be BG_ADDED with DIM, got {unfocused_cells:?}"
        );
    }

    #[test]
    fn selected_hunk_displays_yellow_left_bar() {
        // Multi-line hunk so that *some* row exists that's selected but
        // not the cursor — proving the `▎` ribbon still gets drawn for
        // the rest of the hunk while only one row gets the `▶` arrow.
        let mut app = single_hunk_app(
            "src/foo.rs",
            1,
            vec![
                diff_line(LineKind::Added, "first"),
                diff_line(LineKind::Added, "second"),
            ],
            100,
        );
        // Place the cursor on the hunk header so the `▎` (not `▶`) bar
        // covers both diff line rows.
        app.scroll_to(app.layout.hunk_starts[0]);
        let buffer = render_buffer(&app, 80, 14);

        let had_yellow_bar = buffer_has_cell(&buffer, |cell| {
            cell.symbol() == "▎" && cell.style().fg == Some(Color::Yellow)
        });
        assert!(
            had_yellow_bar,
            "expected a yellow '▎' on the selected hunk row"
        );
    }

    #[test]
    fn cursor_row_displays_arrow_marker_distinct_from_hunk_bar() {
        // Two-line hunk: park the cursor on the first diff line. That row
        // should render `▶` in the left margin while the *other* diff row
        // of the same hunk still uses the plain `▎` ribbon.
        let mut app = single_hunk_app(
            "src/foo.rs",
            1,
            vec![
                diff_line(LineKind::Added, "first"),
                diff_line(LineKind::Added, "second"),
            ],
            100,
        );
        // Layout: FileHeader, HunkHeader, DiffLine(0), DiffLine(1), Spacer
        // hunk_starts[0] = 1 (HunkHeader). First DiffLine is at row 2.
        app.scroll_to(app.layout.hunk_starts[0] + 1);
        let buffer = render_buffer(&app, 80, 14);

        let had_arrow = buffer_has_cell(&buffer, |cell| {
            cell.symbol() == "▶" && cell.style().fg == Some(Color::Yellow)
        });
        let had_plain_bar = buffer_has_cell(&buffer, |cell| {
            cell.symbol() == "▎" && cell.style().fg == Some(Color::Yellow)
        });
        assert!(had_arrow, "expected a yellow '▶' arrow at the cursor row");
        assert!(
            had_plain_bar,
            "expected a yellow '▎' ribbon on the other selected row"
        );
    }

    #[test]
    fn hunk_header_cursor_displays_arrow_marker() {
        let mut app = single_added_app("src/foo.rs", "first");
        app.scroll_to(app.layout.hunk_starts[0]);

        let view = render_to_string(&app, 80, 10);
        assert!(
            view.contains("▶"),
            "cursor parked on a hunk header must still be visible:\n{view}"
        );
    }

    #[test]
    fn hunk_header_cursor_arrow_is_yellow_and_bold() {
        // v0.4: when a hunk collapses under the seen mark the only
        // on-screen anchor the reader has is the hunk header row.
        // Paint its `▶` with the same Yellow + Bold style DiffLine
        // rows use so the cursor stays visible across the hand-off
        // between expanded and collapsed hunks.
        let mut app = single_added_app("src/foo.rs", "first");
        app.scroll_to(app.layout.hunk_starts[0]);

        let buffer = render_buffer(&app, 80, 10);

        let found = buffer_has_cell(&buffer, |cell| {
            let st = cell.style();
            cell.symbol() == "▶"
                && st.fg == Some(Color::Yellow)
                && st.add_modifier.contains(Modifier::BOLD)
        });
        assert!(
            found,
            "cursor `▶` on a hunk header must be Yellow + Bold, not Cyan"
        );
    }

    #[test]
    fn seen_hunk_header_shows_fold_glyph() {
        // v0.4: seen hunks render with a ▸ fold glyph in the hunk
        // header so the reader can tell "this hunk is collapsed,
        // not empty" at a glance. Unseen hunks have no such glyph.
        let mut app = single_added_app("src/foo.rs", "first");
        app.scroll_to(app.layout.hunk_starts[0] + 1); // onto the DiffLine
        app.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char(' '),
            crossterm::event::KeyModifiers::NONE,
        ));

        let view = render_to_string(&app, 80, 10);
        assert!(
            view.contains("▸"),
            "seen hunk header must display a ▸ fold glyph:\n{view}"
        );
    }

    #[test]
    fn file_header_cursor_displays_arrow_marker() {
        let app = single_added_app("src/foo.rs", "first");

        let view = render_to_string(&app, 80, 10);
        assert!(
            view.contains("▶"),
            "cursor parked on a file header must still be visible:\n{view}"
        );
    }

    #[test]
    fn binary_notice_cursor_displays_arrow_marker() {
        let mut app = app_with_file(timed_binary_file("assets/icon.png", 0));
        app.scroll_to(1);

        let view = render_to_string(&app, 80, 8);
        assert!(
            view.contains("▶"),
            "cursor parked on a binary notice row must still be visible:\n{view}"
        );
    }

    #[test]
    fn centered_cursor_renders_arrow_near_viewport_middle() {
        // 40-row hunk, 12-row viewport, cursor parked deep inside the
        // hunk. In centered mode the cursor row should land at roughly
        // viewport_height / 2.
        let lines: Vec<DiffLine> = (0..40)
            .map(|i| diff_line(LineKind::Added, &format!("line {i}")))
            .collect();
        let mut app = app_with_context_hunk("src/foo.rs", "fn long_function() {", lines, 100);
        let header = app.layout.hunk_starts[0];
        // Park the cursor 20 rows past the hunk header (well inside the
        // hunk). Settle the scroll animation so this test asserts on
        // the final viewport, not a mid-tween sample.
        app.scroll_to(header + 20);
        app.anim = None;

        let buffer = render_buffer(&app, 80, 12);

        // Find the row that holds the yellow `▶` marker.
        let (_, y) = first_cell_matching(&buffer, |cell| {
            cell.symbol() == "▶" && cell.style().fg == Some(Color::Yellow)
        })
        .expect("expected the cursor `▶` to be drawn");
        // Sticky takes row 0, so the body height is 11. We expect the
        // cursor near the middle of the body — between rows 4 and 8 of
        // the full buffer, well within tolerance.
        assert!(
            (4..=8).contains(&y),
            "expected cursor near viewport middle, was at row {y}"
        );
    }

    #[test]
    fn top_cursor_renders_arrow_near_viewport_top() {
        // Same fixture, toggled to Top placement. The arrow should
        // sit at the body's top row, just below the pinned sticky
        // hunk header.
        let lines: Vec<DiffLine> = (0..40)
            .map(|i| diff_line(LineKind::Added, &format!("line {i}")))
            .collect();
        let mut app = app_with_context_hunk("src/foo.rs", "fn long_function() {", lines, 100);
        let header = app.layout.hunk_starts[0];
        app.scroll_to(header + 20);
        app.anim = None;
        app.cursor_placement = crate::app::CursorPlacement::Top;

        let buffer = render_buffer(&app, 80, 12);

        let (_, y) = first_cell_matching(&buffer, |cell| {
            cell.symbol() == "▶" && cell.style().fg == Some(Color::Yellow)
        })
        .expect("expected the cursor `▶` to be drawn");
        // Top mode + sticky header (row 0): the cursor should sit at
        // the first body row, which is y=1 (right below the sticky
        // header).
        assert_eq!(y, 1, "expected cursor at viewport ceiling, was at row {y}");
    }

    #[test]
    fn sticky_header_decision_agrees_with_final_body_height() {
        // Regression for Codex round-4 finding #3: the previous
        // decision flow computed a provisional top with the full
        // area height, decided stickiness from it, and only then
        // shrank the body for the sticky banner. At the boundary
        // where `header_row == provisional_top`, a placement
        // recomputed against the (unused) reduced body would have
        // moved the top down by one row, but the sticky decision
        // had already been made with the stale top → the header
        // disappeared.
        //
        // The new flow pessimistically peeks at the reduced body
        // FIRST. If the header would fall off that peeked top,
        // sticky kicks in and the render uses the same reduced
        // body. Otherwise render uses the full body. The two are
        // now self-consistent.
        //
        // We exercise the boundary by positioning the cursor so
        // that the full-body placement puts the header JUST at the
        // top (not sticky-worthy) but any reduction would push it
        // off. A 20-line hunk with a tight viewport hits this.
        let lines: Vec<DiffLine> = (0..20)
            .map(|i| diff_line(LineKind::Added, &format!("line {i}")))
            .collect();
        let mut app = app_with_context_hunk("src/foo.rs", "fn boundary() {", lines, 100);
        // Jump into the middle of the hunk so any sticky reservation
        // would definitely push the header off-screen.
        let header_row = app.layout.hunk_starts[0];
        app.scroll_to(header_row + 5);
        app.anim = None;

        let buffer = render_buffer(&app, 80, 8);

        // Pin: whichever branch the new decision flow picks, row 0
        // must either be the sticky header (contains `boundary`) OR
        // be the actual hunk header / a row on or after header_row
        // but never a row from later in the hunk with the header
        // silently dropped.
        let row0 = buffer_row_text(&buffer, 0);
        assert!(
            row0.contains("boundary") || row0.contains("@@"),
            "row 0 must show the hunk header (sticky or inline), got:\n{row0}"
        );
    }

    #[test]
    fn sticky_hunk_header_appears_when_cursor_is_below_it() {
        // Build a single hunk tall enough that scrolling past the header
        // pushes it off the top of a small viewport. The renderer should
        // pin the header on viewport row 0.
        let lines: Vec<DiffLine> = (0..40)
            .map(|i| diff_line(LineKind::Added, &format!("line {i}")))
            .collect();
        let mut app = app_with_context_hunk("src/foo.rs", "fn long_function() {", lines, 100);
        // Skip past the hunk header so the renderer has to pin it.
        let header_row = app.layout.hunk_starts[0];
        app.scroll_to(header_row + 10);
        app.anim = None;

        // Tight viewport so the original header row really is off-screen.
        let buffer = render_buffer(&app, 80, 8);

        // The very first row of the main area must contain the function
        // name from the sticky header.
        let row0 = buffer_row_text(&buffer, 0);
        assert!(
            row0.contains("long_function"),
            "row 0 should be the pinned hunk header, got:\n{row0}"
        );
    }

    fn app_with_context_hunk(name: &str, ctx: &str, lines: Vec<DiffLine>, secs: u64) -> App {
        app_with_hunks(name, vec![hunk_with_context(1, ctx, lines)], secs)
    }

    // ---- v0.4 slash-search in-body highlight ------------------------

    /// Find the cells whose symbol matches `needle` in `buf` and return
    /// the starting cell of each run plus its length in cells. Used by
    /// the search-highlight tests to locate the rendered `foo` runs
    /// without depending on concrete x offsets (which shift when the
    /// left gutter evolves).
    ///
    /// Skips the last row — the footer echoes the confirmed `/query`
    /// so a naive scan would double-count matches from the status bar.
    fn find_text_runs(buf: &ratatui::buffer::Buffer, needle: &str) -> Vec<(u16, u16, usize)> {
        let mut out = Vec::new();
        let width = buf.area().width;
        let height = buf.area().height;
        let body_height = height.saturating_sub(1);
        for y in 0..body_height {
            let row: String = (0..width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect();
            let mut search_from = 0;
            while let Some(off) = row[search_from..].find(needle) {
                let start_byte = search_from + off;
                // Map byte offset to cell x index by walking symbols.
                let mut x = 0u16;
                let mut cum_bytes = 0usize;
                for xi in 0..width {
                    let sym = buf[(xi, y)].symbol();
                    if cum_bytes == start_byte {
                        x = xi;
                        break;
                    }
                    cum_bytes += sym.len();
                }
                out.push((x, y, needle.chars().count()));
                search_from = start_byte + needle.len();
            }
        }
        out
    }

    #[test]
    fn search_current_match_renders_with_yellow_background() {
        // `/foo<Enter>` on a single Added line containing one `foo` must
        // paint the 3 cells of `foo` with Yellow bg + Black fg + Bold.
        // Before this slice the matched cells inherit the diff bg_added
        // green, so the test fails loudly until the overlay lands.
        let mut app = single_added_app("a.rs", "let foo = 1;");
        let match_count = install_search(&mut app, "foo", 0);
        assert_eq!(match_count, 1, "test fixture precondition");

        let buffer = render_buffer(&app, 80, 6);

        let runs = find_text_runs(&buffer, "foo");
        assert_eq!(runs.len(), 1, "rendered buffer should contain one `foo`");
        let (x, y, len) = runs[0];
        for dx in 0..len as u16 {
            let cell = &buffer[(x + dx, y)];
            let style = cell.style();
            assert_eq!(
                style.bg,
                Some(Color::Yellow),
                "current-match cell at ({},{}) must carry Yellow bg, got {:?}",
                x + dx,
                y,
                style,
            );
            assert_eq!(
                style.fg,
                Some(Color::Black),
                "current-match cell at ({},{}) must carry Black fg, got {:?}",
                x + dx,
                y,
                style,
            );
            assert!(
                style.add_modifier.contains(Modifier::BOLD),
                "current-match cell at ({},{}) must be bold, got {:?}",
                x + dx,
                y,
                style,
            );
        }
    }

    #[test]
    fn search_other_matches_render_with_underline_and_preserve_diff_bg() {
        // Two `foo` matches on a single Added line. `current=0` (= first
        // match) wears the Yellow reversal; the second one still gets a
        // visual cue (UNDERLINED + BOLD) while keeping the diff bg_added
        // green so the add/delete signal survives the overlay.
        let mut app = single_added_app("a.rs", "foo bar foo");
        let match_count = install_search(&mut app, "foo", 0);
        assert_eq!(match_count, 2, "test fixture precondition");

        let buffer = render_buffer(&app, 80, 6);

        let runs = find_text_runs(&buffer, "foo");
        assert_eq!(runs.len(), 2, "expected two `foo` runs in the buffer");

        // First run = current. Yellow bg already asserted by the other
        // test; here just sanity-check it is NOT underlined (it is
        // reversed instead so underline would be redundant + distracting).
        let (x0, y0, len0) = runs[0];
        let first_cell = &buffer[(x0, y0)].style();
        assert_eq!(first_cell.bg, Some(Color::Yellow));

        // Second run = non-current. Must carry UNDERLINED + BOLD, and
        // must keep the diff bg_added green (NOT Yellow).
        let (x1, y1, len1) = runs[1];
        for dx in 0..len1 as u16 {
            let style = buffer[(x1 + dx, y1)].style();
            assert_eq!(
                style.bg,
                Some(BG_ADDED),
                "non-current match cell at ({},{}) must keep bg_added green, got {:?}",
                x1 + dx,
                y1,
                style,
            );
            assert!(
                style.add_modifier.contains(Modifier::UNDERLINED),
                "non-current match cell at ({},{}) must be underlined, got {:?}",
                x1 + dx,
                y1,
                style,
            );
            assert!(
                style.add_modifier.contains(Modifier::BOLD),
                "non-current match cell at ({},{}) must be bold, got {:?}",
                x1 + dx,
                y1,
                style,
            );
        }

        // Also assert that non-match cells in the same row keep bg_added
        // and do NOT carry UNDERLINED (no accidental whole-line underline).
        // The ` bar ` space between matches sits in (x0+len0 .. x1).
        for xi in (x0 + len0 as u16)..x1 {
            let style = buffer[(xi, y0)].style();
            assert_eq!(
                style.bg,
                Some(BG_ADDED),
                "inter-match cell at ({},{}) must keep bg_added green, got {:?}",
                xi,
                y0,
                style,
            );
            assert!(
                !style.add_modifier.contains(Modifier::UNDERLINED),
                "inter-match cell at ({},{}) must NOT be underlined, got {:?}",
                xi,
                y0,
                style,
            );
        }
    }

    #[test]
    fn search_current_match_on_cursor_row_retains_yellow_background() {
        // `commit_search_input` and `search_jump_next`/`_prev` both call
        // `scroll_to(row)`, so the cursor almost always sits on top of
        // the current match. `apply_cursor_gutter_tint` darkens every
        // cell bg on the cursor row; without a carve-out for search
        // colors, the Yellow reversal collapses into `DEFAULT_DIM`
        // (Rgb(30,30,36)) and the user perceives the highlight as
        // "dark". This test pins the carve-out.
        let mut app = single_added_app("a.rs", "let foo = 1;");
        install_search(&mut app, "foo", 0);
        let match_row = app.search.as_ref().unwrap().matches[0].row;
        // Force the cursor onto the match row (mirrors the
        // post-`n`/`N` state where `scroll_to(match.row)` runs).
        app.scroll = match_row;
        app.follow_mode = false;

        let buffer = render_buffer(&app, 80, 6);

        let runs = find_text_runs(&buffer, "foo");
        assert_eq!(runs.len(), 1, "rendered buffer should contain one `foo`");
        let (x, y, len) = runs[0];
        for dx in 0..len as u16 {
            let cell = &buffer[(x + dx, y)];
            let style = cell.style();
            assert_eq!(
                style.bg,
                Some(Color::Yellow),
                "current-match cell at ({},{}) must keep Yellow bg even on the cursor row, got {:?}",
                x + dx,
                y,
                style,
            );
            assert_eq!(
                style.fg,
                Some(Color::Black),
                "current-match cell at ({},{}) must keep Black fg on the cursor row, got {:?}",
                x + dx,
                y,
                style,
            );
        }
    }

    #[test]
    fn footer_shows_search_query_and_position() {
        // With an active `SearchState`, the footer must echo the query
        // and a `[current/total]` counter so the user can tell which
        // hit `n`/`N` is about to jump to without counting visually.
        let mut app = single_hunk_app(
            "a.rs",
            1,
            vec![
                diff_line(LineKind::Added, "foo one"),
                diff_line(LineKind::Added, "foo two"),
                diff_line(LineKind::Added, "foo three"),
            ],
            100,
        );
        let match_count = install_search(&mut app, "foo", 1); // 2/3
        assert_eq!(match_count, 3);
        app.follow_mode = false;

        let view = render_to_string(&app, 120, 8);
        assert!(
            view.contains("/foo"),
            "footer must echo the confirmed search query, got:\n{view}"
        );
        assert!(
            view.contains("[2/3]"),
            "footer must show [current/total] position, got:\n{view}"
        );
    }

    #[test]
    fn search_other_match_in_unfocused_hunk_is_not_dimmed() {
        // When matches land in a hunk that is NOT currently selected,
        // `base_style` carries `Modifier::DIM` (the rest of the hunk
        // body dims so the focused hunk stands out). The search
        // overlay must strip DIM so matches stay loud regardless of
        // which hunk the cursor is in — otherwise every non-focus
        // match looks "dark" even though underline + bold are set.
        let mut app = app_with_hunks(
            "a.rs",
            vec![
                hunk(1, vec![diff_line(LineKind::Added, "first foo")]),
                hunk(50, vec![diff_line(LineKind::Added, "second foo")]),
            ],
            100,
        );
        let match_count = install_search(&mut app, "foo", 0);
        assert_eq!(match_count, 2, "test fixture precondition");
        // `current = 0` -> first hunk's `foo` is current; the cursor
        // sits on the first match row so the second hunk is unfocused.
        let first_row = app.search.as_ref().unwrap().matches[0].row;
        app.follow_mode = false;
        // Place the cursor explicitly on the first hunk's DiffLine so
        // `current_hunk()` resolves to hunk 0 and hunk 1 is unfocused.
        app.scroll = first_row;

        let buffer = render_buffer(&app, 80, 20);

        let runs = find_text_runs(&buffer, "foo");
        assert_eq!(runs.len(), 2, "both `foo` runs must render in the viewport");
        // The second run is in the unfocused hunk — `apply_search_overlay`
        // must have stripped `DIM` from `base_style` so the highlight
        // does not look washed out.
        let (x1, y1, len1) = runs[1];
        for dx in 0..len1 as u16 {
            let style = buffer[(x1 + dx, y1)].style();
            assert!(
                !style.add_modifier.contains(Modifier::DIM),
                "non-current match in unfocused hunk at ({},{}) must NOT carry DIM, got {:?}",
                x1 + dx,
                y1,
                style,
            );
            assert!(
                style.add_modifier.contains(Modifier::UNDERLINED),
                "non-current match in unfocused hunk at ({},{}) must be underlined, got {:?}",
                x1 + dx,
                y1,
                style,
            );
        }
    }
}
