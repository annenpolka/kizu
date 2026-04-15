use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
};

use crate::app::{App, RowKind};
use crate::git::{DiffContent, FileDiff, FileStatus, LineKind};

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
    let viewport_height = area.height as usize;
    let total_rows = app.layout.rows.len();

    // Slice the layout to the viewport, applying scroll offset and the
    // SCROLL_ROW_LIMIT safety cap.
    let start = app.scroll;
    let cap_end = start.saturating_add(SCROLL_ROW_LIMIT.min(viewport_height));
    let end = cap_end.min(total_rows);

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(end.saturating_sub(start));
    for row_idx in start..end {
        lines.push(render_row(&app.layout.rows[row_idx], &app.files));
    }

    if total_rows > SCROLL_ROW_LIMIT && (start + viewport_height) < total_rows {
        // We're not at the bottom yet but the view is capped at the row limit
        // — surface that fact in the last visible row.
        let remaining = total_rows - end;
        if remaining > 0 {
            lines.push(Line::from(Span::styled(
                format!("[+{remaining} more rows]"),
                Style::default().fg(Color::DarkGray),
            )));
        }
    }

    let p = Paragraph::new(lines);
    frame.render_widget(p, area);
}

fn render_row(row: &RowKind, files: &[FileDiff]) -> Line<'static> {
    match row {
        RowKind::FileHeader { file_idx } => {
            let file = &files[*file_idx];
            let status = status_label(file.status);
            let counts = match &file.content {
                DiffContent::Binary => "bin".to_string(),
                DiffContent::Text(_) => format!("+{}/-{}", file.added, file.deleted),
            };
            let body = format!("  {} ── {} ── {}", file.path.display(), status, counts,);
            Line::from(Span::styled(
                body,
                Style::default()
                    .fg(Color::Gray)
                    .add_modifier(Modifier::BOLD),
            ))
        }
        RowKind::HunkHeader { file_idx, hunk_idx } => {
            let DiffContent::Text(hunks) = &files[*file_idx].content else {
                return Line::raw("");
            };
            let h = &hunks[*hunk_idx];
            let body = format!(
                "  @@ -{},{} +{},{} @@",
                h.old_start, h.old_count, h.new_start, h.new_count
            );
            Line::from(Span::styled(body, Style::default().fg(Color::Cyan)))
        }
        RowKind::DiffLine {
            file_idx,
            hunk_idx,
            line_idx,
        } => {
            let DiffContent::Text(hunks) = &files[*file_idx].content else {
                return Line::raw("");
            };
            let line = &hunks[*hunk_idx].lines[*line_idx];
            let (prefix, color) = match line.kind {
                LineKind::Added => ('+', Some(Color::Green)),
                LineKind::Deleted => ('-', Some(Color::Red)),
                LineKind::Context => (' ', None),
            };
            let body = format!("{prefix}{}", line.content);
            match color {
                Some(c) => Line::from(Span::styled(body, Style::default().fg(c))),
                None => Line::from(Span::raw(body)),
            }
        }
        RowKind::BinaryNotice { .. } => Line::from(Span::styled(
            "  [binary file - diff suppressed]",
            Style::default().fg(Color::DarkGray),
        )),
        RowKind::Spacer => Line::raw(""),
    }
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
    let mode_label = if app.picker.is_some() {
        "[picker]"
    } else if app.follow_mode {
        "[follow]"
    } else {
        "[manual]"
    };

    let body = if app.picker.is_some() {
        "type to filter / ↑↓ Ctrl-n/p to move / Enter to jump / Esc to cancel".to_string()
    } else {
        let current: String = app
            .current_file_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "--".to_string());
        let session_added: usize = app.files.iter().map(|f| f.added).sum();
        let session_deleted: usize = app.files.iter().map(|f| f.deleted).sum();
        let head_marker = if app.head_dirty { " head*" } else { "" };
        format!(
            "{current} | session: +{session_added}/-{session_deleted} {} files{head_marker} | <Space> picker",
            app.files.len()
        )
    };

    let mut spans = vec![Span::raw(mode_label), Span::raw(" "), Span::raw(body)];
    if let Some(err) = &app.last_error {
        spans.push(Span::styled(
            format!("  × {err}"),
            Style::default().fg(Color::Red),
        ));
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
            let status = status_label(file.status);
            let counts = match &file.content {
                DiffContent::Binary => "bin".to_string(),
                DiffContent::Text(_) => format!("+{}/-{}", file.added, file.deleted),
            };
            let body = format!("{} {:<40} {}", status, file.path.display(), counts);
            ListItem::new(body)
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

fn status_label(status: FileStatus) -> &'static str {
    match status {
        FileStatus::Modified => "M ",
        FileStatus::Added => "A ",
        FileStatus::Deleted => "D ",
        FileStatus::Untracked => "??",
    }
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
            anchor: None,
            picker: None,
            follow_mode: true,
            last_error: None,
            head_dirty: false,
            should_quit: false,
        }
    }

    fn populated_app(files: Vec<FileDiff>) -> App {
        let mut app = fake_app();
        app.files = files;
        app.files.sort_by(|a, b| b.mtime.cmp(&a.mtime));
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
        assert!(view.contains("Esc to cancel"));
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
}
