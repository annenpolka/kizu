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

/// Render the entire kizu frame: scroll view (main) + footer (bottom),
/// optionally with the modal file picker overlaid on top.
pub fn render(frame: &mut Frame<'_>, app: &App) {
    let area = frame.area();
    let chunks = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(area);
    let main = chunks[0];
    let footer = chunks[1];

    if app.files.is_empty() {
        render_empty(frame, main, app);
    } else {
        render_scroll(frame, main, app);
    }

    render_footer(frame, footer, app);

    if app.picker.is_some() {
        render_picker(frame, area, app);
    }
}

fn render_scroll(frame: &mut Frame<'_>, area: Rect, app: &App) {
    // M4v + cursor placement:
    //   - The cursor row inside `app.layout.rows` is `app.scroll`.
    //   - The viewport's *top* row is derived from the cursor + the
    //     current `CursorPlacement` (centered by default, bottom when
    //     the user has pressed `z`).
    //   - When the cursor sits inside a hunk whose header has scrolled
    //     off the top, the header is pinned to viewport row 0 (sticky
    //     scroll), shrinking the body area by one row.
    let total_rows = app.layout.rows.len();
    let selected = app.current_hunk();
    let cursor_row = app.scroll;
    let now = std::time::Instant::now();

    // Provisional body height (before sticky decision). We re-compute
    // viewport_top with the *real* body height further down so the
    // sticky overlay never makes the cursor jitter.
    let raw_body_height = area.height as usize;
    let provisional_top = app.visual_viewport_top(raw_body_height, now);

    // Sticky header: cursor is inside a hunk whose header sits above
    // the visible viewport top.
    let sticky = selected.and_then(|(file_idx, hunk_idx)| {
        find_hunk_header_row(&app.layout.rows, file_idx, hunk_idx)
            .filter(|&row| row < provisional_top)
            .map(|_| (file_idx, hunk_idx))
    });

    let (header_area, content_area) = if sticky.is_some() && area.height > 1 {
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

    let viewport_height = content_area.height as usize;
    // Tell the App layer how tall the body actually was so the next
    // `J`/`K` press can size its scroll chunk relative to this height.
    app.last_body_height.set(viewport_height);
    let viewport_top = app.visual_viewport_top(viewport_height, now);

    let start = viewport_top;
    let cap_end = start.saturating_add(SCROLL_ROW_LIMIT.min(viewport_height));
    let end = cap_end.min(total_rows);

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(end.saturating_sub(start));
    for row_idx in start..end {
        let is_cursor = row_idx == cursor_row;
        lines.push(render_row(
            &app.layout.rows[row_idx],
            &app.files,
            selected,
            is_cursor,
        ));
    }

    if total_rows > SCROLL_ROW_LIMIT && (start + viewport_height) < total_rows {
        let remaining = total_rows - end;
        if remaining > 0 {
            lines.push(Line::from(Span::styled(
                format!("[+{remaining} more rows]"),
                Style::default().fg(Color::DarkGray),
            )));
        }
    }

    frame.render_widget(Paragraph::new(lines), content_area);

    // Pin the header on top after the body, so the overlay always wins.
    if let (Some(header_rect), Some((file_idx, hunk_idx))) = (header_area, sticky)
        && let DiffContent::Text(hunks) = &app.files[file_idx].content
    {
        let line = render_hunk_header(&hunks[hunk_idx], true);
        frame.render_widget(Paragraph::new(line), header_rect);
    }
}

/// Walk `rows` to find the row index of the `HunkHeader` matching
/// `(file_idx, hunk_idx)`. Returns `None` if the layout is empty or the
/// cursor's hunk has no header row (binary, etc).
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

/// Build the styled `Line` for a single layout row. `selected_hunk`
/// identifies the (file_idx, hunk_idx) the cursor is currently inside;
/// rows belonging to that hunk render at full saturation, all other
/// hunk rows render with `Modifier::DIM` so the cursor's hunk pops out
/// of the surrounding diff without hiding it. `is_cursor` is `true` for
/// the *single* row that `app.scroll` points at — that row gets a
/// stronger left-margin marker so the user can find their position
/// inside a long hunk.
fn render_row(
    row: &RowKind,
    files: &[FileDiff],
    selected_hunk: Option<(usize, usize)>,
    is_cursor: bool,
) -> Line<'static> {
    match row {
        RowKind::FileHeader { file_idx } => render_file_header(*file_idx, &files[*file_idx]),
        RowKind::HunkHeader { file_idx, hunk_idx } => {
            let DiffContent::Text(hunks) = &files[*file_idx].content else {
                return Line::raw("");
            };
            let is_selected = selected_hunk == Some((*file_idx, *hunk_idx));
            render_hunk_header(&hunks[*hunk_idx], is_selected)
        }
        RowKind::DiffLine {
            file_idx,
            hunk_idx,
            line_idx,
        } => {
            let DiffContent::Text(hunks) = &files[*file_idx].content else {
                return Line::raw("");
            };
            let is_selected = selected_hunk == Some((*file_idx, *hunk_idx));
            let line = &hunks[*hunk_idx].lines[*line_idx];
            render_diff_line(line, is_selected, is_cursor)
        }
        RowKind::BinaryNotice { .. } => Line::from(Span::styled(
            "       [binary file - diff suppressed]",
            Style::default().fg(Color::DarkGray),
        )),
        RowKind::Spacer => Line::raw(""),
    }
}

/// Bottom-up file header: `  path                                14:03   +12 -3`.
/// Path color encodes the status (cyan / green / red / yellow), no `M`/`A`/`D`
/// label needed.
fn render_file_header(_file_idx: usize, file: &FileDiff) -> Line<'static> {
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

    let mut spans = vec![
        Span::raw("  "),
        Span::styled(
            file.path.display().to_string(),
            Style::default().fg(path_color).add_modifier(Modifier::BOLD),
        ),
        Span::raw("   "),
        Span::styled(mtime, Style::default().fg(Color::DarkGray)),
        Span::raw("   "),
        Span::raw(counts),
    ];
    // Spacing for a future scar indicator placeholder; M4v leaves it
    // dormant so the column stays stable when scar lands.
    spans.push(Span::raw(""));
    Line::from(spans)
}

fn render_hunk_header(hunk: &Hunk, is_selected: bool) -> Line<'static> {
    let body = match &hunk.context {
        Some(ctx) => format!("       @@ {ctx}"),
        None => format!(
            "       @@ -{},{} +{},{} @@",
            hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count
        ),
    };
    let mut style = Style::default().fg(Color::Cyan);
    if !is_selected {
        style = style.add_modifier(Modifier::DIM);
    }
    Line::from(Span::styled(body, style))
}

fn render_diff_line(
    line: &crate::git::DiffLine,
    is_selected: bool,
    is_cursor: bool,
) -> Line<'static> {
    let (prefix_char, color) = match line.kind {
        LineKind::Added => ('+', Some(Color::Green)),
        LineKind::Deleted => ('-', Some(Color::Red)),
        LineKind::Context => (' ', None),
    };
    // Left margin (5 cells). When this is the cursor's exact row, drop
    // a `▶` arrow there so it stands out from the `▎` ribbon that the
    // selected hunk shares across all of its rows.
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
    let prefix_span = match (color, is_selected) {
        (Some(c), true) => Span::styled(prefix_char.to_string(), Style::default().fg(c)),
        (Some(c), false) => Span::styled(
            prefix_char.to_string(),
            Style::default().fg(c).add_modifier(Modifier::DIM),
        ),
        (None, true) => Span::raw(prefix_char.to_string()),
        (None, false) => Span::styled(
            prefix_char.to_string(),
            Style::default().add_modifier(Modifier::DIM),
        ),
    };
    let body_style = match (color, is_selected) {
        (Some(c), true) => Style::default().fg(c),
        (Some(c), false) => Style::default().fg(c).add_modifier(Modifier::DIM),
        (None, true) => Style::default(),
        (None, false) => Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
    };
    Line::from(vec![
        bar,
        prefix_span,
        Span::styled(line.content.clone(), body_style),
    ])
}

/// `HH:MM` formatted local time. Returns `--:--` when the metadata read
/// failed and the parser left the field at `UNIX_EPOCH`.
fn format_mtime(t: SystemTime) -> String {
    if t == UNIX_EPOCH {
        return "--:--".to_string();
    }
    // We avoid pulling in `chrono` for a single timestamp render: the
    // duration since midnight UTC is enough to derive HH:MM, and any
    // off-by-timezone is acceptable for an at-a-glance hint. Real local
    // time will arrive with the v0.2 dependency on `time` if it pays for
    // itself elsewhere.
    let secs = t
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let day_secs = secs % 86_400;
    let hour = (day_secs / 3600) as u32;
    let minute = ((day_secs % 3600) / 60) as u32;
    format!("{hour:02}:{minute:02}")
}

fn render_empty(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let short = app
        .baseline_sha
        .get(..7)
        .unwrap_or(&app.baseline_sha)
        .to_string();
    let body = format!("No changes since baseline (HEAD: {short})");
    let mid = centered_line(area);
    let p = Paragraph::new(body).alignment(Alignment::Center);
    frame.render_widget(p, mid);
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, app: &App) {
    // Pre-styled spans for the four "static" pieces of the status bar.
    let dim = Style::default().fg(Color::DarkGray);
    let bold = Modifier::BOLD;
    let sep = || Span::styled(" │ ", dim);

    let (mode_text, mode_color) = if app.picker.is_some() {
        ("[picker]", Color::Magenta)
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

        spans.push(sep());
        spans.push(Span::styled("⎵", Style::default().fg(Color::Magenta)));
        spans.push(Span::raw(" "));
        spans.push(Span::styled("picker", dim));
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
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{:<40}", file.path.display()),
                    Style::default().fg(path_color),
                ),
                Span::raw(" "),
                Span::styled(mtime, Style::default().fg(Color::DarkGray)),
                Span::raw(" "),
                Span::raw(counts),
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
        }
    }

    fn fake_app() -> App {
        App {
            root: PathBuf::from("/tmp/fake"),
            git_dir: PathBuf::from("/tmp/fake/.git"),
            baseline_sha: "abcdef1234567890abcdef1234567890abcdef12".into(),
            files: Vec::new(),
            layout: ScrollLayout::default(),
            scroll: 0,
            cursor_placement: crate::app::CursorPlacement::Centered,
            anchor: None,
            picker: None,
            follow_mode: true,
            last_error: None,
            head_dirty: false,
            should_quit: false,
            last_body_height: std::cell::Cell::new(24),
            visual_top: std::cell::Cell::new(0.0),
            anim: None,
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
            view.contains("No changes since baseline (HEAD: abcdef1)"),
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
        assert!(
            view.contains("@@ -10,1 +10,1 @@"),
            "missing hunk header:\n{view}"
        );
        assert!(view.contains("+let x = 1;"), "missing added line:\n{view}");
        assert!(
            view.contains("-let y = 2;"),
            "missing deleted line:\n{view}"
        );
    }

    #[test]
    fn render_scroll_lines_carry_added_and_deleted_colors() {
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

        let mut found_added_green = false;
        let mut found_deleted_red = false;
        for y in 0..buffer.area().height {
            for x in 0..buffer.area().width {
                let cell = &buffer[(x, y)];
                if cell.symbol() == "+" && cell.style().fg == Some(Color::Green) {
                    found_added_green = true;
                }
                if cell.symbol() == "-" && cell.style().fg == Some(Color::Red) {
                    found_deleted_red = true;
                }
            }
        }
        assert!(found_added_green, "expected an added '+' rendered in green");
        assert!(found_deleted_red, "expected a deleted '-' rendered in red");
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
    fn selected_hunk_diff_lines_render_at_full_color() {
        // 2 hunks in 1 file: cursor lives inside the first hunk after
        // bootstrap (because there is only one file → mtime newest = it,
        // and follow_target lands on the *last* hunk of the newest file
        // → the second hunk here). So we manually scroll to the first
        // hunk to test the selection contrast.
        let mut app = populated_app(vec![make_file(
            "src/foo.rs",
            vec![
                hunk(1, vec![diff_line(LineKind::Added, "first")]),
                hunk(20, vec![diff_line(LineKind::Added, "second")]),
            ],
            100,
        )]);
        // Snap to the first hunk header.
        app.scroll_to(app.layout.hunk_starts[0]);
        let backend = TestBackend::new(100, 14);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| render(f, &app)).expect("draw");
        let buffer = terminal.backend().buffer().clone();

        // The cursor's hunk should render its `+` at full Color::Green
        // *without* DIM. The other hunk's `+` should still be Green but
        // with DIM modifier.
        let mut found_bright = false;
        let mut found_dim = false;
        for y in 0..buffer.area().height {
            for x in 0..buffer.area().width {
                let cell = &buffer[(x, y)];
                if cell.symbol() == "+" && cell.style().fg == Some(Color::Green) {
                    if cell.style().add_modifier.contains(Modifier::DIM) {
                        found_dim = true;
                    } else {
                        found_bright = true;
                    }
                }
            }
        }
        assert!(
            found_bright,
            "expected at least one bright '+' for selected hunk"
        );
        assert!(
            found_dim,
            "expected at least one DIM '+' for unselected hunk"
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
    fn bottom_cursor_renders_arrow_near_viewport_bottom() {
        // Same fixture, toggled to Bottom placement. The arrow should
        // sit near the very last visible row.
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
        app.cursor_placement = crate::app::CursorPlacement::Bottom;

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
        // Bottom mode + sticky header (row 0) + footer (last row): the
        // cursor should be on the last body row, which is two rows up
        // from the absolute bottom of the buffer.
        assert_eq!(
            y,
            buffer.area().height - 2,
            "expected cursor at viewport floor, was at row {y}"
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
        }
    }
}
