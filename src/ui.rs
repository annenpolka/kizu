use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

mod diff_line;
mod diff_view;
mod file_view;
mod footer;
mod geometry;
mod line_numbers;
mod overlays;
mod text_cells;

#[cfg(test)]
use diff_line::{render_diff_line, render_diff_line_wrapped};
use diff_view::render_scroll;
use file_view::render_file_view;
#[cfg(test)]
use file_view::{render_file_view_line, render_file_view_line_wrapped};
pub(crate) use footer::format_local_time;
#[cfg(test)]
use geometry::LineNumberGutter;
#[cfg(test)]
use geometry::line_number_digits;
#[cfg(test)]
use line_numbers::file_ln_span;
#[cfg(test)]
use line_numbers::{add_line_number_gutters, diff_ln_span};
#[cfg(test)]
use text_cells::wrap_at_chars;

use crate::app::App;

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

pub(super) fn cursor_bar(is_cursor: bool, is_selected: bool) -> Span<'static> {
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

/// `HH:MM` formatted local time. Returns `--:--` when the metadata
/// read failed and the parser left the field at `UNIX_EPOCH`.
pub(super) fn format_mtime(t: SystemTime) -> String {
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
        added_hunk, added_hunk_app, app_with_file, app_with_files, app_with_hunks,
        binary_file as timed_binary_file, diff_line, file_view_state, hunk, install_search,
        make_file, numbered_added_lines, prefixed_diff_lines, single_added_app, single_added_file,
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
            prefixed_diff_lines(LineKind::Context, "line ", 30),
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
            (
                first_text_run(&buffer, "@@").0 as usize,
                first_text_run(&buffer, "fn ok()").0 as usize,
            )
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
        let mut app = added_hunk_app("src/foo.rs", 100, &["x"], 100);
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
        let y = row_containing(&buffer, "tiny");

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
        let mut file = make_file(name, vec![added_hunk(1, &["x"])], secs);
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
            let (x, y, _) = first_text_run(&buffer, name);
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
            vec![added_hunk(1, &["~~~~"]), added_hunk(20, &["!!!!"])],
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
        let mut app = added_hunk_app("src/foo.rs", 1, &["first", "second"], 100);
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
        let mut app = added_hunk_app("src/foo.rs", 1, &["first", "second"], 100);
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
        let lines = numbered_added_lines(40);
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
        let lines = numbered_added_lines(40);
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
        let lines = numbered_added_lines(20);
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
        let lines = numbered_added_lines(40);
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

    fn first_text_run(buf: &ratatui::buffer::Buffer, needle: &str) -> (u16, u16, usize) {
        find_text_runs(buf, needle)
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("{needle} not found in buffer"))
    }

    fn row_containing(buf: &ratatui::buffer::Buffer, needle: &str) -> u16 {
        first_text_run(buf, needle).1
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
        let mut app = added_hunk_app("a.rs", 1, &["foo one", "foo two", "foo three"], 100);
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
                added_hunk(1, &["first foo"]),
                added_hunk(50, &["second foo"]),
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
