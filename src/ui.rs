use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
};

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
        render_file_view(frame, main, fv, Some(hl), effective_top);
    } else if app.files.is_empty() {
        render_empty(frame, main, app);
    } else {
        render_scroll(frame, main, app);
    }

    // Render the dedicated input row when a text overlay is active.
    if let Some((text, prefix, cursor_pos)) = input_line {
        render_input_line(frame, input_area, prefix, &text, cursor_pos);
    }

    render_footer(frame, footer, app);

    if app.picker.is_some() {
        render_picker(frame, area, app);
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

    // In wrap mode we reserve 6 cells per row: 5 for the left bar,
    // 1 for the `¶` newline marker (ADR-0014 dropped the `+`/`-`
    // prefix column). Compute this *before* calling
    // `viewport_placement` because the placement math needs the wrap
    // body width to produce a correct `VisualIndex`.
    let wrap_body_width: Option<usize> = if app.wrap_lines {
        Some((area.width as usize).saturating_sub(6).max(1))
    } else {
        None
    };
    // Nowrap mode still needs a body width so the diff row
    // background color can extend to the viewport edge. 5 cells for
    // the left bar, the rest is body.
    let nowrap_body_width: usize = (area.width as usize).saturating_sub(5).max(1);

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
        };
        let row_lines = render_row(&app.layout.rows[row_idx], &ctx);
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
fn darken_cursor_body_bg(existing: Option<Color>) -> Color {
    // `bg_added = Rgb(10, 50, 10)` (default) maps to Rgb(7, 37, 7) —
    // still clearly green, just slightly muted against the surrounding
    // full-intensity rows. Earlier 0.5 made it too stark a contrast.
    const FACTOR: f32 = 0.75;
    const DEFAULT_DIM: Color = Color::Rgb(30, 30, 36);
    match existing {
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
    seen_hunks: &'a std::collections::BTreeSet<(std::path::PathBuf, usize)>,
    hl: Option<&'a crate::highlight::Highlighter>,
    bg_added: Color,
    bg_deleted: Color,
}

/// Build the styled visual `Line`s for a single logical layout row.
/// Most row types produce exactly one `Line`; a `DiffLine` in wrap
/// mode (`wrap_body_width.is_some()`) can produce multiple lines
/// when its content exceeds the body width.
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
fn render_row(row: &RowKind, ctx: &RowRenderCtx<'_>) -> Vec<Line<'static>> {
    let files = ctx.files;
    let selected_hunk = ctx.selected_hunk;
    let cursor_sub = ctx.cursor_sub;
    let wrap_body_width = ctx.wrap_body_width;
    let nowrap_body_width = ctx.nowrap_body_width;
    let seen_hunks = ctx.seen_hunks;
    let hl = ctx.hl;
    match row {
        RowKind::FileHeader { file_idx } => {
            vec![render_file_header(&files[*file_idx], cursor_sub.is_some())]
        }
        RowKind::HunkHeader { file_idx, hunk_idx } => {
            let DiffContent::Text(hunks) = &files[*file_idx].content else {
                return vec![Line::raw("")];
            };
            let is_selected = selected_hunk == Some((*file_idx, *hunk_idx));
            vec![render_hunk_header(
                &hunks[*hunk_idx],
                is_selected,
                cursor_sub.is_some(),
                crate::app::is_hunk_seen(
                    seen_hunks,
                    &files[*file_idx].path,
                    hunks[*hunk_idx].old_start,
                ),
            )]
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
            match wrap_body_width {
                Some(width) => render_diff_line_wrapped(
                    line,
                    is_selected,
                    cursor_sub,
                    width,
                    hl,
                    Some(&files[*file_idx].path),
                    ctx.bg_added,
                    ctx.bg_deleted,
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
                )],
            }
        }
        RowKind::BinaryNotice { .. } => vec![Line::from(Span::styled(
            if cursor_sub.is_some() {
                "  ▶    [binary file - diff suppressed]"
            } else {
                "       [binary file - diff suppressed]"
            },
            Style::default().fg(Color::DarkGray),
        ))],
        RowKind::Spacer => vec![Line::raw("")],
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

/// Wrap-mode variant of [`render_diff_line`]. Splits `line.content`
/// at `body_width` chars and paints every visual row with the delta-style
/// background color (ADR-0014). The last visual row gets a `¶` newline
/// marker so the reader can tell real newlines from wrap boundaries.
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
    let marker_style = match bg {
        // Keep the marker legible on top of the colored bg.
        Some(b) => Style::default()
            .bg(b)
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
        None => Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
    };

    // Highlight the full line once, then distribute tokens across
    // wrapped visual rows by tracking character positions.
    let tokens: Option<Vec<crate::highlight::HlToken>> =
        if let (Some(hl), Some(path)) = (hl, file_path) {
            let toks = hl.highlight_line(&line.content, path);
            if toks.len() > 1 || toks.first().is_some_and(|t| t.fg != Color::Reset) {
                Some(toks)
            } else {
                None
            }
        } else {
            None
        };

    // Pre-compute per-character fg colors from tokens (if available).
    // This avoids complex token-boundary tracking when distributing
    // across wrapped chunks.
    let char_colors: Vec<Color> = if let Some(ref toks) = tokens {
        let mut colors = Vec::with_capacity(line.content.len());
        for tok in toks {
            for _ in tok.text.chars() {
                colors.push(tok.fg);
            }
        }
        colors
    } else {
        Vec::new()
    };

    let chunks = wrap_at_chars(&line.content, body_width.max(1));
    let last_idx = chunks.len().saturating_sub(1);
    let mut char_offset = 0usize;

    chunks
        .into_iter()
        .enumerate()
        .map(|(i, chunk)| {
            let is_last = i == last_idx;
            let cursor_line = cursor_sub.map(|s| s.min(last_idx));
            let bar = if cursor_line == Some(i) {
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
            };
            let marker_reserve = if is_last && line.has_trailing_newline {
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

            if !char_colors.is_empty() {
                // Build per-token spans for this wrapped chunk.
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
                // No highlighting: single span for the whole chunk.
                let padded_body: String =
                    chunk.chars().chain(std::iter::repeat_n(' ', pad)).collect();
                spans.push(Span::styled(padded_body, base_style));
            }

            if is_last && line.has_trailing_newline {
                spans.push(Span::styled("¶", marker_style));
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

fn render_hunk_header(
    hunk: &Hunk,
    is_selected: bool,
    is_cursor: bool,
    is_seen: bool,
) -> Line<'static> {
    let seen_mark = if is_seen { "• " } else { "  " };
    let cursor_mark = if is_cursor { "  ▶  " } else { "     " };

    // Count added/deleted lines from the actual hunk content.
    let added: usize = hunk
        .lines
        .iter()
        .filter(|l| l.kind == LineKind::Added)
        .count();
    let deleted: usize = hunk
        .lines
        .iter()
        .filter(|l| l.kind == LineKind::Deleted)
        .count();
    let counts = format!("+{added}/-{deleted}");

    // Line range: show new_start (where the change lands in the
    // current file). For multi-line hunks, show the range end.
    let line_range = if hunk.new_count > 1 {
        format!(
            "L{}-{}",
            hunk.new_start,
            hunk.new_start + hunk.new_count - 1
        )
    } else {
        format!("L{}", hunk.new_start)
    };

    let body = match &hunk.context {
        Some(ctx) => format!("{cursor_mark}{seen_mark}@@ {ctx}  {line_range} {counts}"),
        None => format!("{cursor_mark}{seen_mark}@@ {line_range} {counts}"),
    };

    let mut style = Style::default().fg(Color::Cyan);
    if !is_selected {
        style = style.add_modifier(Modifier::DIM);
    }
    Line::from(Span::styled(body, style))
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
) -> Line<'static> {
    let bg = match line.kind {
        LineKind::Added => Some(bg_added),
        LineKind::Deleted => Some(bg_deleted),
        LineKind::Context => None,
    };
    let bar = if is_cursor {
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
    };
    let base_style = match (bg, is_selected) {
        (Some(b), true) => Style::default().bg(b),
        (Some(b), false) => Style::default().bg(b).add_modifier(Modifier::DIM),
        (None, true) => Style::default(),
        (None, false) => Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
    };

    // Try syntax highlighting. If available, produce per-token spans
    // with the token fg color + the diff bg style overlay.
    //
    // Widths are counted in display cells (via `unicode-width`), not
    // chars — each kanji is 2 cells, and char-based truncation would
    // push CJK rows past the viewport edge and smear the delta-style
    // background color past the right margin.
    if let (Some(hl), Some(path)) = (hl, file_path) {
        let tokens = hl.highlight_line(&line.content, path);
        if tokens.len() > 1 || tokens.first().is_some_and(|t| t.fg != Color::Reset) {
            let mut spans = vec![bar];
            let mut cells_emitted = 0usize;
            for token in &tokens {
                let remaining = body_width.saturating_sub(cells_emitted);
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
            // Pad to body_width.
            if cells_emitted < body_width {
                spans.push(Span::styled(
                    " ".repeat(body_width - cells_emitted),
                    base_style,
                ));
            }
            return Line::from(spans);
        }
    }

    // Fallback: single-span body (no highlighting or unknown extension).
    use unicode_width::UnicodeWidthStr;
    let content_cells = UnicodeWidthStr::width(line.content.as_str());
    let padded_body: String = if content_cells >= body_width {
        let (truncated, _) = take_cells(&line.content, body_width);
        truncated
    } else {
        let pad = body_width - content_cells;
        line.content
            .chars()
            .chain(std::iter::repeat_n(' ', pad))
            .collect()
    };
    Line::from(vec![bar, Span::styled(padded_body, base_style)])
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
    hl: Option<&crate::highlight::Highlighter>,
    effective_top: usize,
) {
    let height = area.height as usize;
    let width = area.width as usize;
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(height);

    for i in 0..height {
        let line_idx = effective_top + i;
        if line_idx >= fv.lines.len() {
            lines.push(Line::from(Span::styled(
                "~",
                Style::default().fg(Color::DarkGray),
            )));
            continue;
        }
        let is_cursor = line_idx == fv.cursor;
        let bar = if is_cursor {
            Span::styled(
                "  ▶  ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::raw("     ")
        };

        let content = &fv.lines[line_idx];
        let body_width = width.saturating_sub(5).max(1);
        let char_len = content.chars().count();
        let padded: String = if char_len >= body_width {
            content.chars().take(body_width).collect()
        } else {
            content
                .chars()
                .chain(std::iter::repeat_n(' ', body_width - char_len))
                .collect()
        };

        let base_style = if let Some(&bg) = fv.line_bg.get(&line_idx) {
            Style::default().bg(bg)
        } else {
            Style::default()
        };

        if let Some(hl) = hl {
            let tokens = hl.highlight_line(content, &fv.path);
            if tokens.len() > 1 || tokens.first().is_some_and(|t| t.fg != Color::Reset) {
                let mut spans = vec![bar];
                let mut chars_emitted = 0;
                for token in &tokens {
                    let remaining = body_width.saturating_sub(chars_emitted);
                    if remaining == 0 {
                        break;
                    }
                    let take = token.text.chars().count().min(remaining);
                    let text: String = token.text.chars().take(take).collect();
                    spans.push(Span::styled(text, base_style.fg(token.fg)));
                    chars_emitted += take;
                }
                if chars_emitted < body_width {
                    spans.push(Span::styled(
                        " ".repeat(body_width - chars_emitted),
                        base_style,
                    ));
                }
                lines.push(Line::from(spans));
                continue;
            }
        }

        lines.push(Line::from(vec![bar, Span::styled(padded, base_style)]));
    }

    frame.render_widget(Paragraph::new(lines), area);
}

/// Convert a Unix epoch millisecond timestamp to a local-time
/// `HH:MM:SS` string. Uses `libc::localtime_r` on Unix for
/// timezone-aware conversion; falls back to UTC on other platforms.
pub fn format_local_time(timestamp_ms: u64) -> String {
    let epoch_secs = (timestamp_ms / 1000) as i64;

    #[cfg(unix)]
    {
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let time_t = epoch_secs as libc::time_t;
        unsafe { libc::localtime_r(&time_t, &mut tm) };
        format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
    }

    #[cfg(not(unix))]
    {
        let secs = epoch_secs as u64;
        let hours = (secs / 3600) % 24;
        let mins = (secs / 60) % 60;
        let s = secs % 60;
        format!("{hours:02}:{mins:02}:{s:02}")
    }
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, app: &App) {
    // Pre-styled spans for the four "static" pieces of the status bar.
    let dim = Style::default().fg(Color::DarkGray);
    let bold = Modifier::BOLD;
    let sep = || Span::styled(" │ ", dim);

    let (mode_text, mode_color) = if app.picker.is_some() {
        ("[picker]", Color::Magenta)
    } else if app.scar_comment.is_some() {
        ("[scar]", Color::Magenta)
    } else if app.revert_confirm.is_some() {
        ("[revert?]", Color::Red)
    } else if app.search_input.is_some() {
        ("[search]", Color::Yellow)
    } else if app.file_view.is_some() {
        ("[file view]", Color::Cyan)
    } else if app.view_mode == crate::app::ViewMode::Stream {
        ("[stream]", Color::Blue)
    } else if app.follow_mode {
        ("[follow]", Color::Green)
    } else {
        ("[manual]", Color::Yellow)
    };
    let mode_span = Span::styled(
        mode_text,
        Style::default().fg(mode_color).add_modifier(bold),
    );

    let mut spans: Vec<Span<'static>> = vec![Span::raw(" "), mode_span, Span::raw(" ")];

    if app.picker.is_some() {
        // Picker hint stays muted; the modal popup is the loud surface.
        spans.push(sep());
        spans.push(Span::styled(
            "type to filter",
            Style::default().fg(Color::Yellow),
        ));
        spans.push(Span::styled(" / ", dim));
        spans.push(Span::styled(
            "↑↓ Ctrl-n/p",
            Style::default().fg(Color::Cyan),
        ));
        spans.push(Span::raw(" "));
        spans.push(Span::styled("move", dim));
        spans.push(Span::styled(" / ", dim));
        spans.push(Span::styled("Enter", Style::default().fg(Color::Green)));
        spans.push(Span::raw(" "));
        spans.push(Span::styled("jump", dim));
        spans.push(Span::styled(" / ", dim));
        spans.push(Span::styled("Esc", Style::default().fg(Color::Red)));
        spans.push(Span::raw(" "));
        spans.push(Span::styled("cancel", dim));
    } else if let Some(fv) = app.file_view.as_ref() {
        spans.push(sep());
        spans.push(Span::styled(
            fv.path.display().to_string(),
            Style::default().fg(Color::Cyan).add_modifier(bold),
        ));
        spans.push(Span::styled(
            format!(" [{}/{}]", fv.cursor + 1, fv.lines.len()),
            Style::default().fg(Color::DarkGray),
        ));
        spans.push(sep());
        spans.push(Span::styled("Enter", Style::default().fg(Color::Green)));
        spans.push(Span::styled("/", dim));
        spans.push(Span::styled("Esc", Style::default().fg(Color::Red)));
        spans.push(Span::raw(" "));
        spans.push(Span::styled("back", dim));
    } else if app.search_input.is_some() {
        // Body is rendered in the dedicated input row above.
        spans.push(sep());
        spans.push(Span::styled("Enter", Style::default().fg(Color::Green)));
        spans.push(Span::raw(" "));
        spans.push(Span::styled("find", dim));
        spans.push(Span::styled(" / ", dim));
        spans.push(Span::styled("Esc", Style::default().fg(Color::Red)));
        spans.push(Span::raw(" "));
        spans.push(Span::styled("cancel", dim));
    } else if let Some(state) = app.revert_confirm.as_ref() {
        spans.push(sep());
        spans.push(Span::styled(
            format!("revert hunk in {} ?", state.file_path.display()),
            Style::default().fg(Color::Red).add_modifier(bold),
        ));
        spans.push(Span::raw(" "));
        spans.push(Span::styled("(y/N)", Style::default().fg(Color::Yellow)));
    } else if app.scar_comment.is_some() {
        // Body is rendered in the dedicated input row above.
        spans.push(sep());
        spans.push(Span::styled("Enter", Style::default().fg(Color::Green)));
        spans.push(Span::raw(" "));
        spans.push(Span::styled("save", dim));
        spans.push(Span::styled(" / ", dim));
        spans.push(Span::styled("Esc", Style::default().fg(Color::Red)));
        spans.push(Span::raw(" "));
        spans.push(Span::styled("cancel", dim));
    } else {
        // Current file path uses the same status color the file header
        // uses up in the scroll, so the eye can match them.
        let current_path = app
            .current_file_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "--".to_string());
        let path_color = app
            .current_file_idx()
            .and_then(|i| app.files.get(i))
            .map(|f| match f.status {
                FileStatus::Modified => Color::Cyan,
                FileStatus::Added => Color::Green,
                FileStatus::Deleted => Color::Red,
                FileStatus::Untracked => Color::Yellow,
            })
            .unwrap_or(Color::Reset);

        spans.push(sep());
        spans.push(Span::styled(
            current_path,
            Style::default().fg(path_color).add_modifier(bold),
        ));

        let session_added: usize = app.files.iter().map(|f| f.added).sum();
        let session_deleted: usize = app.files.iter().map(|f| f.deleted).sum();

        spans.push(sep());
        spans.push(Span::styled("session", dim));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!("+{session_added}"),
            Style::default().fg(Color::Green).add_modifier(bold),
        ));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!("-{session_deleted}"),
            Style::default().fg(Color::Red).add_modifier(bold),
        ));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!("{} files", app.files.len()),
            Style::default().fg(Color::Cyan),
        ));

        if app.head_dirty {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                "HEAD*",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ));
        }

        // Cursor placement indicator. `z` toggles Centered ↔ Top.
        spans.push(sep());
        spans.push(Span::styled("z", Style::default().fg(Color::Cyan)));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            app.cursor_placement.label(),
            Style::default().fg(Color::Cyan).add_modifier(bold),
        ));

        // Line-wrap indicator. `w` toggles wrap on/off.
        spans.push(sep());
        spans.push(Span::styled("w", Style::default().fg(Color::Cyan)));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            if app.wrap_lines { "wrap" } else { "nowrap" },
            Style::default().fg(Color::Cyan).add_modifier(bold),
        ));

        spans.push(sep());
        spans.push(Span::styled("s", Style::default().fg(Color::Magenta)));
        spans.push(Span::raw(" "));
        spans.push(Span::styled("picker", dim));
    }

    // Watcher health takes footer precedence over transient diff
    // errors: a dead notify backend is a correctness-level problem
    // (auto-refresh has stopped) and must stay visible even if the
    // most recent one-off recompute happened to succeed. Drawn with
    // a distinct `WATCHER` tag so it cannot be confused with an
    // ordinary `git diff` failure. See ADR-0008.
    if let Some(msg) = app.watcher_health.summary() {
        spans.push(sep());
        spans.push(Span::styled(
            "⚠ WATCHER",
            Style::default().fg(Color::Red).add_modifier(bold),
        ));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(msg, Style::default().fg(Color::Red)));
    }

    if let Some(msg) = &app.input_health {
        spans.push(sep());
        spans.push(Span::styled(
            "⚠ INPUT",
            Style::default().fg(Color::Red).add_modifier(bold),
        ));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(msg.clone(), Style::default().fg(Color::Red)));
    }

    if let Some(err) = &app.last_error {
        spans.push(sep());
        spans.push(Span::styled(
            "×",
            Style::default().fg(Color::Red).add_modifier(bold),
        ));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(err.clone(), Style::default().fg(Color::Red)));
    }

    let line = Line::from(spans);
    frame.render_widget(Paragraph::new(line), area);
}

/// Pad or truncate `s` so its display width (cells) equals exactly
/// `target`. Truncation is rune-aware via `unicode-width` so CJK
/// filenames do not land mid-codepoint or overflow by one cell.
fn pad_or_truncate_display(s: &str, target: usize) -> String {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    let w = UnicodeWidthStr::width(s);
    if w <= target {
        let pad = target - w;
        let mut out = String::with_capacity(s.len() + pad);
        out.push_str(s);
        for _ in 0..pad {
            out.push(' ');
        }
        return out;
    }
    // Truncate, reserving 1 cell for an ellipsis when it fits.
    let keep = target.saturating_sub(1);
    let mut acc = 0usize;
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        if acc + cw > keep {
            break;
        }
        acc += cw;
        out.push(ch);
    }
    // Add the ellipsis marker + a trailing space when target - acc >= 1.
    if target > acc {
        out.push('…');
        acc += 1;
    }
    while acc < target {
        out.push(' ');
        acc += 1;
    }
    out
}

fn render_picker(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let popup_area = centered_rect(60, 60, area);
    let Some(picker) = &app.picker else { return };
    let results = app.picker_results();

    // Wipe whatever was beneath the popup so the underlying scroll view
    // doesn't bleed through translucent rows.
    frame.render_widget(Clear, popup_area);

    let block = Block::default().borders(Borders::ALL).title(format!(
        " Files {}/{} ",
        results.len(),
        app.files.len()
    ));
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    // Inside the block, top row is the query line; the rest is the file list.
    let chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(inner);
    let query_area = chunks[0];
    let list_area = chunks[1];

    let query_line = Line::from(vec![
        Span::styled("> ", Style::default().fg(Color::Yellow)),
        Span::raw(picker.query.clone()),
    ]);
    frame.render_widget(Paragraph::new(query_line), query_area);

    // Reserve a fixed right-hand slot for `HH:MM` + `+N -M` so the
    // minute column is never clipped by a long path. The selection
    // gutter (2 cells) sits left of the path, so the usable width
    // inside `list_area` is `width - 2`. The path takes the rest.
    let list_width = list_area.width as usize;
    const MTIME_WIDTH: usize = 5; // "HH:MM"
    const COUNTS_WIDTH: usize = 10; // "+NNN -MMM" fits comfortably
    const GAP: usize = 1;
    const GUTTER: usize = 2; // space for "▸ "
    let reserved = MTIME_WIDTH + GAP + COUNTS_WIDTH + GAP;
    let path_width = list_width.saturating_sub(reserved + GUTTER).max(10);

    let items: Vec<ListItem<'_>> = results
        .iter()
        .map(|&file_idx| {
            let file = &app.files[file_idx];
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
            // Pad path to `path_width` using unicode-width so CJK
            // filenames stay aligned. Truncate from the right if the
            // name overflows the column so the right-hand mtime/counts
            // columns are never pushed off-screen.
            let path_str = file.path.display().to_string();
            let padded_path = pad_or_truncate_display(&path_str, path_width);
            // Right-pad counts to a fixed width so the mtime column
            // lands at a stable offset even when +/- counts vary.
            let padded_counts = format!("{counts:>width$}", width = COUNTS_WIDTH);
            ListItem::new(Line::from(vec![
                Span::styled(padded_path, Style::default().fg(path_color)),
                Span::raw(" "),
                Span::styled(mtime, Style::default().fg(Color::DarkGray)),
                Span::raw(" "),
                Span::raw(padded_counts),
            ]))
        })
        .collect();

    let list = List::new(items)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▸ ");
    let mut state = ListState::default();
    if !results.is_empty() {
        state.select(Some(picker.cursor.min(results.len() - 1)));
    }
    frame.render_stateful_widget(list, list_area, &mut state);
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

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_width = area.width.saturating_mul(percent_x) / 100;
    let popup_height = area.height.saturating_mul(percent_y) / 100;
    Rect {
        x: area.x + area.width.saturating_sub(popup_width) / 2,
        y: area.y + area.height.saturating_sub(popup_height) / 2,
        width: popup_width,
        height: popup_height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{PickerState, ScrollLayout};
    use crate::git::{DiffContent, DiffLine, FileDiff, FileStatus, Hunk, LineKind};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};

    fn diff_line(kind: LineKind, content: &str) -> DiffLine {
        DiffLine {
            kind,
            content: content.to_string(),
            has_trailing_newline: true,
        }
    }

    fn hunk(old_start: usize, lines: Vec<DiffLine>) -> Hunk {
        let added = lines.iter().filter(|l| l.kind == LineKind::Added).count();
        let deleted = lines.iter().filter(|l| l.kind == LineKind::Deleted).count();
        Hunk {
            old_start,
            old_count: deleted,
            new_start: old_start,
            new_count: added,
            lines,
            context: None,
        }
    }

    fn make_file(name: &str, hunks: Vec<Hunk>, secs: u64) -> FileDiff {
        let added: usize = hunks
            .iter()
            .flat_map(|h| h.lines.iter())
            .filter(|l| l.kind == LineKind::Added)
            .count();
        let deleted: usize = hunks
            .iter()
            .flat_map(|h| h.lines.iter())
            .filter(|l| l.kind == LineKind::Deleted)
            .count();
        FileDiff {
            path: PathBuf::from(name),
            status: FileStatus::Modified,
            added,
            deleted,
            content: DiffContent::Text(hunks),
            mtime: SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
            header_prefix: None,
        }
    }

    fn binary_file(name: &str) -> FileDiff {
        FileDiff {
            path: PathBuf::from(name),
            status: FileStatus::Modified,
            added: 0,
            deleted: 0,
            content: DiffContent::Binary,
            mtime: SystemTime::UNIX_EPOCH,
            header_prefix: None,
        }
    }

    fn fake_app() -> App {
        App {
            root: PathBuf::from("/tmp/fake"),
            git_dir: PathBuf::from("/tmp/fake/.git"),
            common_git_dir: PathBuf::from("/tmp/fake/.git"),
            current_branch_ref: Some("refs/heads/main".into()),
            baseline_sha: "abcdef1234567890abcdef1234567890abcdef12".into(),
            files: Vec::new(),
            layout: ScrollLayout::default(),
            scroll: 0,
            cursor_sub_row: 0,
            cursor_placement: crate::app::CursorPlacement::Centered,
            anchor: None,
            picker: None,
            scar_comment: None,
            revert_confirm: None,
            file_view: None,
            search_input: None,
            search: None,
            seen_hunks: std::collections::BTreeSet::new(),
            follow_mode: true,
            last_error: None,
            input_health: None,
            head_dirty: false,
            should_quit: false,
            last_body_height: std::cell::Cell::new(24),
            last_body_width: std::cell::Cell::new(None),
            visual_top: std::cell::Cell::new(0.0),
            anim: None,
            wrap_lines: false,
            watcher_health: crate::app::WatcherHealth::default(),
            highlighter: std::cell::OnceCell::new(),
            config: crate::config::KizuConfig::default(),
            view_mode: crate::app::ViewMode::default(),
            saved_diff_scroll: 0,
            saved_stream_scroll: 0,
            stream_events: Vec::new(),
            processed_event_paths: std::collections::HashSet::new(),
            session_start_ms: 0,
            bound_session_id: None,
            diff_snapshots: std::collections::HashMap::new(),
            scar_undo_stack: Vec::new(),
            scar_focus: None,
            pinned_cursor_y: None,
        }
    }

    fn populated_app(files: Vec<FileDiff>) -> App {
        let mut app = fake_app();
        app.files = files;
        // Match recompute_diff: mtime ascending (oldest first, newest last).
        app.files.sort_by(|a, b| a.mtime.cmp(&b.mtime));
        // Replicate the bootstrap path without touching the real filesystem.
        app.build_layout();
        app.refresh_anchor();
        app
    }

    fn render_to_string(app: &App, w: u16, h: u16) -> String {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| render(f, app)).expect("draw");
        let buffer = terminal.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..buffer.area().height {
            for x in 0..buffer.area().width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn render_empty_state_when_no_files() {
        let app = fake_app();
        let view = render_to_string(&app, 70, 6);
        assert!(
            view.contains("No changes since baseline (baseline: abcdef1)"),
            "expected empty state with short SHA, got:\n{view}"
        );
        assert!(view.contains("[follow]"));
    }

    #[test]
    fn render_scroll_shows_file_header_hunk_header_and_diff_line() {
        let app = populated_app(vec![make_file(
            "src/foo.rs",
            vec![hunk(
                10,
                vec![
                    diff_line(LineKind::Context, "fn ok()"),
                    diff_line(LineKind::Added, "let x = 1;"),
                    diff_line(LineKind::Deleted, "let y = 2;"),
                ],
            )],
            100,
        )]);
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
        let app = populated_app(vec![make_file(
            "src/foo.rs",
            vec![hunk(
                1,
                vec![
                    diff_line(LineKind::Added, "x"),
                    diff_line(LineKind::Deleted, "y"),
                ],
            )],
            100,
        )]);
        let backend = TestBackend::new(80, 12);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| render(f, &app)).expect("draw");
        let buffer = terminal.backend().buffer().clone();

        let mut found_added_bg = false;
        let mut found_deleted_bg = false;
        for y in 0..buffer.area().height {
            for x in 0..buffer.area().width {
                let cell = &buffer[(x, y)];
                if cell.symbol() == "x" && cell.style().bg == Some(BG_ADDED) {
                    found_added_bg = true;
                }
                if cell.symbol() == "y" && cell.style().bg == Some(BG_DELETED) {
                    found_deleted_bg = true;
                }
            }
        }
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
        let app = populated_app(vec![make_file(
            "a.rs",
            vec![hunk(1, vec![diff_line(LineKind::Added, "tiny")])],
            100,
        )]);
        let width: u16 = 40;
        let backend = TestBackend::new(width, 12);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| render(f, &app)).expect("draw");
        let buffer = terminal.backend().buffer().clone();

        // Find the row that contains the "tiny" body text.
        let mut tiny_y: Option<u16> = None;
        for y in 0..buffer.area().height {
            let row: String = (0..width)
                .map(|x| buffer[(x, y)].symbol().chars().next().unwrap_or(' '))
                .collect();
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
        let app = populated_app(vec![make_file(
            "src/foo.rs",
            vec![hunk(
                1,
                vec![
                    diff_line(LineKind::Added, "ADDED_LINE"),
                    diff_line(LineKind::Deleted, "DELETED_LINE"),
                ],
            )],
            100,
        )]);
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
    fn wrap_mode_renders_newline_marker_and_wraps_long_line() {
        // 120-char diff line inside an 80-col terminal. In wrap mode
        // the line should wrap to at least two visual rows and the
        // last visible segment should end with a `¶` newline marker.
        let long_content: String = (0..120u8).map(|i| (b'a' + (i % 26)) as char).collect();
        let mut app = populated_app(vec![make_file(
            "a.rs",
            vec![hunk(1, vec![diff_line(LineKind::Added, &long_content)])],
            100,
        )]);
        app.wrap_lines = true;

        let view = render_to_string(&app, 80, 14);
        assert!(
            view.contains("¶"),
            "wrap mode should draw a ¶ newline marker:\n{view}"
        );
        // The second half of the content must be visible — i.e. the
        // line wrapped onto another visual row instead of being
        // truncated at the viewport edge.
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
        let mut app = populated_app(vec![make_file(
            "a.rs",
            vec![hunk(1, vec![diff_line(LineKind::Added, &forty_kanji)])],
            100,
        )]);
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
    fn wrap_mode_omits_newline_marker_when_diff_line_has_no_terminal_newline() {
        let long_content: String = (0..40u8).map(|i| (b'a' + (i % 26)) as char).collect();
        let mut file = make_file(
            "a.rs",
            vec![hunk(1, vec![diff_line(LineKind::Added, &long_content)])],
            100,
        );
        let DiffContent::Text(hunks) = &mut file.content else {
            panic!("expected text diff");
        };
        hunks[0].lines[0].has_trailing_newline = false;

        let mut app = populated_app(vec![file]);
        app.wrap_lines = true;
        let view = render_to_string(&app, 40, 10);
        assert!(
            !view.contains("¶"),
            "wrap mode must not invent a newline marker for EOF-no-newline lines:\n{view}"
        );
    }

    #[test]
    fn nowrap_mode_has_no_newline_marker() {
        let mut app = populated_app(vec![make_file(
            "a.rs",
            vec![hunk(1, vec![diff_line(LineKind::Added, "short")])],
            100,
        )]);
        app.wrap_lines = false;
        let view = render_to_string(&app, 80, 10);
        assert!(
            !view.contains("¶"),
            "nowrap mode should not draw newline markers:\n{view}"
        );
    }

    #[test]
    fn wrap_nowrap_indicator_appears_in_footer() {
        let mut app = populated_app(vec![make_file(
            "a.rs",
            vec![hunk(1, vec![diff_line(LineKind::Added, "x")])],
            100,
        )]);
        let nowrap_view = render_to_string(&app, 80, 8);
        assert!(nowrap_view.contains("nowrap"));

        app.wrap_lines = true;
        let wrap_view = render_to_string(&app, 80, 8);
        assert!(wrap_view.contains("wrap"));
        assert!(!wrap_view.contains("nowrap"));
    }

    #[test]
    fn render_scroll_marks_binary_file_with_notice() {
        let app = populated_app(vec![binary_file("assets/icon.png")]);
        let view = render_to_string(&app, 80, 8);
        assert!(view.contains("assets/icon.png"));
        assert!(view.contains("[binary file - diff suppressed]"));
        assert!(view.contains("bin"));
    }

    #[test]
    fn render_picker_overlays_a_box_with_query_and_filtered_list() {
        let mut app = populated_app(vec![
            make_file(
                "src/auth.rs",
                vec![hunk(1, vec![diff_line(LineKind::Added, "x")])],
                300,
            ),
            make_file(
                "src/handler.rs",
                vec![hunk(1, vec![diff_line(LineKind::Added, "y")])],
                200,
            ),
            make_file(
                "tests/auth_test.rs",
                vec![hunk(1, vec![diff_line(LineKind::Added, "z")])],
                100,
            ),
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
        let mut app = populated_app(vec![make_file(
            "src/foo.rs",
            vec![hunk(1, vec![diff_line(LineKind::Added, "x")])],
            100,
        )]);
        app.last_error = Some("git diff exploded".into());

        // Wide enough that the footer's body + error message both fit.
        let backend = TestBackend::new(140, 6);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| render(f, &app)).expect("draw");
        let buffer = terminal.backend().buffer().clone();

        let footer_y = buffer.area().height - 1;
        let mut footer_text = String::new();
        let mut had_red_x = false;
        for x in 0..buffer.area().width {
            let cell = &buffer[(x, footer_y)];
            footer_text.push_str(cell.symbol());
            if cell.symbol() == "×" && cell.style().fg == Some(Color::Red) {
                had_red_x = true;
            }
        }
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
        let mut app = populated_app(vec![make_file(
            "src/foo.rs",
            vec![hunk(1, vec![diff_line(LineKind::Added, "x")])],
            100,
        )]);
        app.watcher_health.record_failure(
            crate::watcher::WatchSource::GitRefs,
            "watcher [git.refs]: refs watcher dead".into(),
        );
        app.watcher_health.record_failure(
            crate::watcher::WatchSource::Worktree,
            "watcher [worktree]: worktree watcher dead".into(),
        );

        let view = render_to_string(&app, 160, 6);
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
        let mut app = populated_app(vec![make_file(
            "src/foo.rs",
            vec![hunk(1, vec![diff_line(LineKind::Added, "x")])],
            100,
        )]);
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
        let mut app = populated_app(vec![make_file(
            "src/foo.rs",
            vec![hunk(1, vec![diff_line(LineKind::Added, "x")])],
            100,
        )]);
        app.follow_mode = false;
        let view = render_to_string(&app, 80, 6);
        assert!(view.contains("[manual]"), "expected [manual]:\n{view}");
    }

    fn hunk_with_context(old_start: usize, ctx: &str, lines: Vec<DiffLine>) -> Hunk {
        let added = lines.iter().filter(|l| l.kind == LineKind::Added).count();
        let deleted = lines.iter().filter(|l| l.kind == LineKind::Deleted).count();
        Hunk {
            old_start,
            old_count: deleted,
            new_start: old_start,
            new_count: added,
            lines,
            context: Some(ctx.to_string()),
        }
    }

    fn modified_file_status(name: &str, status: FileStatus, secs: u64) -> FileDiff {
        FileDiff {
            path: PathBuf::from(name),
            status,
            added: 1,
            deleted: 0,
            content: DiffContent::Text(vec![hunk(1, vec![diff_line(LineKind::Added, "x")])]),
            mtime: SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
            header_prefix: None,
        }
    }

    #[test]
    fn file_header_path_color_encodes_status() {
        let mut app = populated_app(vec![
            modified_file_status("a.rs", FileStatus::Modified, 100),
            modified_file_status("b.rs", FileStatus::Added, 200),
            modified_file_status("c.rs", FileStatus::Deleted, 300),
            modified_file_status("d.rs", FileStatus::Untracked, 400),
        ]);
        // Bootstrap parks scroll on the follow target (bottom). Reset to 0
        // so every file header sits inside the viewport for inspection.
        app.scroll_to(0);
        let backend = TestBackend::new(80, 30);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| render(f, &app)).expect("draw");
        let buffer = terminal.backend().buffer().clone();

        // For each path, find its first cell (the 'a'/'b'/'c'/'d') and
        // assert the foreground color matches the status mapping.
        let want = [
            ("a.rs", Color::Cyan),
            ("b.rs", Color::Green),
            ("c.rs", Color::Red),
            ("d.rs", Color::Yellow),
        ];
        for (name, expected) in want {
            let mut found = false;
            'outer: for y in 0..buffer.area().height {
                for x in 0..buffer.area().width {
                    if buffer[(x, y)].symbol() == &name[..1] {
                        // Walk forward to make sure this is the start of `name`.
                        let chars: String = (0..name.len())
                            .map(|i| buffer[(x + i as u16, y)].symbol().to_string())
                            .collect();
                        if chars == name {
                            assert_eq!(
                                buffer[(x, y)].style().fg,
                                Some(expected),
                                "{name} should be in {expected:?}"
                            );
                            found = true;
                            break 'outer;
                        }
                    }
                }
            }
            assert!(found, "{name} not found in buffer");
        }
    }

    #[test]
    fn file_header_shows_prefix_when_set() {
        let mut file = make_file(
            "src/auth.rs",
            vec![hunk(1, vec![diff_line(LineKind::Added, "x")])],
            100,
        );
        file.header_prefix = Some("14:03:22 Write".to_string());
        let mut app = populated_app(vec![file]);
        app.scroll = 0;

        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 10))
            .expect("test terminal");
        terminal.draw(|frame| render(frame, &app)).expect("draw");
        let buffer = terminal.backend().buffer().clone();

        // The prefix "14:03:22 Write" should appear in the header line.
        let first_line: String = (0..buffer.area().width)
            .map(|x| buffer[(x, 0)].symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(
            first_line.contains("14:03:22 Write"),
            "header should contain prefix, got: {first_line:?}"
        );
    }

    #[test]
    fn hunk_header_uses_function_context_when_available() {
        let app = populated_app(vec![make_file(
            "src/auth.rs",
            vec![hunk_with_context(
                10,
                "fn verify_token(claims: &Claims) -> Result<bool> {",
                vec![diff_line(LineKind::Added, "let x = 1;")],
            )],
            100,
        )]);
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
        let app = populated_app(vec![make_file(
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
        )]);
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
    fn hunk_header_shows_range_in_fallback_format() {
        // When no function context is available, the header should
        // still show line range and counts.
        let app = populated_app(vec![make_file(
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
        )]);
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
        let mut app = populated_app(vec![make_file(
            "a.rs",
            vec![
                hunk(1, vec![diff_line(LineKind::Added, "~~~~")]),
                hunk(20, vec![diff_line(LineKind::Added, "!!!!")]),
            ],
            100,
        )]);
        // Snap to the first hunk so one hunk is focused and the other
        // is not.
        app.scroll_to(app.layout.hunk_starts[0]);
        let backend = TestBackend::new(100, 14);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| render(f, &app)).expect("draw");
        let buffer = terminal.backend().buffer().clone();

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
        let mut app = populated_app(vec![make_file(
            "src/foo.rs",
            vec![hunk(
                1,
                vec![
                    diff_line(LineKind::Added, "first"),
                    diff_line(LineKind::Added, "second"),
                ],
            )],
            100,
        )]);
        // Place the cursor on the hunk header so the `▎` (not `▶`) bar
        // covers both diff line rows.
        app.scroll_to(app.layout.hunk_starts[0]);
        let backend = TestBackend::new(80, 14);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| render(f, &app)).expect("draw");
        let buffer = terminal.backend().buffer().clone();

        let mut had_yellow_bar = false;
        for y in 0..buffer.area().height {
            for x in 0..buffer.area().width {
                let cell = &buffer[(x, y)];
                if cell.symbol() == "▎" && cell.style().fg == Some(Color::Yellow) {
                    had_yellow_bar = true;
                }
            }
        }
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
        let mut app = populated_app(vec![make_file(
            "src/foo.rs",
            vec![hunk(
                1,
                vec![
                    diff_line(LineKind::Added, "first"),
                    diff_line(LineKind::Added, "second"),
                ],
            )],
            100,
        )]);
        // Layout: FileHeader, HunkHeader, DiffLine(0), DiffLine(1), Spacer
        // hunk_starts[0] = 1 (HunkHeader). First DiffLine is at row 2.
        app.scroll_to(app.layout.hunk_starts[0] + 1);
        let backend = TestBackend::new(80, 14);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| render(f, &app)).expect("draw");
        let buffer = terminal.backend().buffer().clone();

        let mut had_arrow = false;
        let mut had_plain_bar = false;
        for y in 0..buffer.area().height {
            for x in 0..buffer.area().width {
                let cell = &buffer[(x, y)];
                if cell.symbol() == "▶" && cell.style().fg == Some(Color::Yellow) {
                    had_arrow = true;
                }
                if cell.symbol() == "▎" && cell.style().fg == Some(Color::Yellow) {
                    had_plain_bar = true;
                }
            }
        }
        assert!(had_arrow, "expected a yellow '▶' arrow at the cursor row");
        assert!(
            had_plain_bar,
            "expected a yellow '▎' ribbon on the other selected row"
        );
    }

    #[test]
    fn hunk_header_cursor_displays_arrow_marker() {
        let mut app = populated_app(vec![make_file(
            "src/foo.rs",
            vec![hunk(1, vec![diff_line(LineKind::Added, "first")])],
            100,
        )]);
        app.scroll_to(app.layout.hunk_starts[0]);

        let view = render_to_string(&app, 80, 10);
        assert!(
            view.contains("▶"),
            "cursor parked on a hunk header must still be visible:\n{view}"
        );
    }

    #[test]
    fn file_header_cursor_displays_arrow_marker() {
        let app = populated_app(vec![make_file(
            "src/foo.rs",
            vec![hunk(1, vec![diff_line(LineKind::Added, "first")])],
            100,
        )]);

        let view = render_to_string(&app, 80, 10);
        assert!(
            view.contains("▶"),
            "cursor parked on a file header must still be visible:\n{view}"
        );
    }

    #[test]
    fn binary_notice_cursor_displays_arrow_marker() {
        let mut app = populated_app(vec![binary_file("assets/icon.png")]);
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
        let mut app = populated_app(vec![make_file_with_context(
            "src/foo.rs",
            "fn long_function() {",
            lines,
            100,
        )]);
        let header = app.layout.hunk_starts[0];
        // Park the cursor 20 rows past the hunk header (well inside the
        // hunk). Settle the scroll animation so this test asserts on
        // the final viewport, not a mid-tween sample.
        app.scroll_to(header + 20);
        app.anim = None;

        let backend = TestBackend::new(80, 12);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| render(f, &app)).expect("draw");
        let buffer = terminal.backend().buffer().clone();

        // Find the row that holds the yellow `▶` marker.
        let mut cursor_y: Option<u16> = None;
        for y in 0..buffer.area().height {
            for x in 0..buffer.area().width {
                let cell = &buffer[(x, y)];
                if cell.symbol() == "▶" && cell.style().fg == Some(Color::Yellow) {
                    cursor_y = Some(y);
                }
            }
        }
        let y = cursor_y.expect("expected the cursor `▶` to be drawn");
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
        let mut app = populated_app(vec![make_file_with_context(
            "src/foo.rs",
            "fn long_function() {",
            lines,
            100,
        )]);
        let header = app.layout.hunk_starts[0];
        app.scroll_to(header + 20);
        app.anim = None;
        app.cursor_placement = crate::app::CursorPlacement::Top;

        let backend = TestBackend::new(80, 12);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| render(f, &app)).expect("draw");
        let buffer = terminal.backend().buffer().clone();

        let mut cursor_y: Option<u16> = None;
        for y in 0..buffer.area().height {
            for x in 0..buffer.area().width {
                let cell = &buffer[(x, y)];
                if cell.symbol() == "▶" && cell.style().fg == Some(Color::Yellow) {
                    cursor_y = Some(y);
                }
            }
        }
        let y = cursor_y.expect("expected the cursor `▶` to be drawn");
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
        let mut app = populated_app(vec![make_file_with_context(
            "src/foo.rs",
            "fn boundary() {",
            lines,
            100,
        )]);
        // Jump into the middle of the hunk so any sticky reservation
        // would definitely push the header off-screen.
        let header_row = app.layout.hunk_starts[0];
        app.scroll_to(header_row + 5);
        app.anim = None;

        let backend = TestBackend::new(80, 8);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| render(f, &app)).expect("draw");
        let buffer = terminal.backend().buffer().clone();

        // Pin: whichever branch the new decision flow picks, row 0
        // must either be the sticky header (contains `boundary`) OR
        // be the actual hunk header / a row on or after header_row
        // but never a row from later in the hunk with the header
        // silently dropped.
        let mut row0 = String::new();
        for x in 0..buffer.area().width {
            row0.push_str(buffer[(x, 0)].symbol());
        }
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
        let mut app = populated_app(vec![make_file_with_context(
            "src/foo.rs",
            "fn long_function() {",
            lines,
            100,
        )]);
        // Skip past the hunk header so the renderer has to pin it.
        let header_row = app.layout.hunk_starts[0];
        app.scroll_to(header_row + 10);
        app.anim = None;

        // Tight viewport so the original header row really is off-screen.
        let backend = TestBackend::new(80, 8);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| render(f, &app)).expect("draw");
        let buffer = terminal.backend().buffer().clone();

        // The very first row of the main area must contain the function
        // name from the sticky header.
        let mut row0 = String::new();
        for x in 0..buffer.area().width {
            row0.push_str(buffer[(x, 0)].symbol());
        }
        assert!(
            row0.contains("long_function"),
            "row 0 should be the pinned hunk header, got:\n{row0}"
        );
    }

    fn make_file_with_context(name: &str, ctx: &str, lines: Vec<DiffLine>, secs: u64) -> FileDiff {
        let added: usize = lines.iter().filter(|l| l.kind == LineKind::Added).count();
        let deleted: usize = lines.iter().filter(|l| l.kind == LineKind::Deleted).count();
        FileDiff {
            path: PathBuf::from(name),
            status: FileStatus::Modified,
            added,
            deleted,
            content: DiffContent::Text(vec![Hunk {
                old_start: 1,
                old_count: deleted,
                new_start: 1,
                new_count: added,
                lines,
                context: Some(ctx.to_string()),
            }]),
            mtime: SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
            header_prefix: None,
        }
    }
}
