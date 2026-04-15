use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
};

use crate::app::App;
use crate::git::{DiffContent, FileStatus, LineKind};

/// Hard cap on the number of diff lines we will hand to ratatui for a single
/// file. Anything above this is replaced with a `[+N more lines truncated]`
/// marker (Decision Log: 1 ファイル 2000 行で truncate).
const DIFF_LINE_LIMIT: usize = 2000;

/// Render the entire kizu frame: file list (left) + diff view (right) +
/// footer (bottom). Pure function — does not mutate `app`.
pub fn render(frame: &mut Frame<'_>, app: &App) {
    let area = frame.area();
    let chunks = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(area);
    let main = chunks[0];
    let footer = chunks[1];

    if app.files.is_empty() {
        render_empty(frame, main, app);
    } else {
        let panes = Layout::horizontal([Constraint::Percentage(30), Constraint::Percentage(70)])
            .split(main);
        render_file_list(frame, panes[0], app);
        render_diff(frame, panes[1], app);
    }

    render_footer(frame, footer, app);
}

fn render_file_list(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let items: Vec<ListItem<'_>> = app
        .files
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let status = match f.status {
                FileStatus::Modified => "M ",
                FileStatus::Added => "A ",
                FileStatus::Deleted => "D ",
                FileStatus::Untracked => "??",
            };
            let counts = match &f.content {
                DiffContent::Binary => "bin".to_string(),
                DiffContent::Text(_) => format!("+{}/-{}", f.added, f.deleted),
            };
            let body = format!("{} {} {}", status, f.path.display(), counts);
            let style = if i == app.selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            ListItem::new(body).style(style)
        })
        .collect();

    let list = List::new(items).block(Block::default().borders(Borders::RIGHT));
    frame.render_widget(list, area);
}

fn render_diff(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let Some(file) = app.files.get(app.selected) else {
        return;
    };

    match &file.content {
        DiffContent::Binary => {
            let placeholder = centered_line(area, "[binary file - diff suppressed]");
            let p = Paragraph::new("[binary file - diff suppressed]").alignment(Alignment::Center);
            frame.render_widget(p, placeholder);
        }
        DiffContent::Text(hunks) => {
            let mut lines: Vec<Line<'static>> = Vec::new();
            for hunk in hunks {
                let header = format!(
                    "@@ -{},{} +{},{} @@",
                    hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count
                );
                lines.push(Line::from(Span::styled(
                    header,
                    Style::default().fg(Color::Cyan),
                )));
                for diff_line in &hunk.lines {
                    let (prefix, color) = match diff_line.kind {
                        LineKind::Added => ('+', Some(Color::Green)),
                        LineKind::Deleted => ('-', Some(Color::Red)),
                        LineKind::Context => (' ', None),
                    };
                    let body = format!("{prefix}{}", diff_line.content);
                    let span = match color {
                        Some(c) => Span::styled(body, Style::default().fg(c)),
                        None => Span::raw(body),
                    };
                    lines.push(Line::from(span));
                }
            }

            // Truncate after collecting so the marker reflects the full size.
            let total = lines.len();
            if total > DIFF_LINE_LIMIT {
                lines.truncate(DIFF_LINE_LIMIT);
                lines.push(Line::from(Span::styled(
                    format!("[+{} more lines truncated]", total - DIFF_LINE_LIMIT),
                    Style::default().fg(Color::DarkGray),
                )));
            }

            // Apply the diff_scroll offset by skipping leading lines. We
            // keep lines as a Vec because Paragraph::new wants something
            // convertible into Text<'_>.
            let visible: Vec<Line<'static>> = if app.diff_scroll < lines.len() {
                lines.split_off(app.diff_scroll)
            } else {
                Vec::new()
            };

            let p = Paragraph::new(visible);
            frame.render_widget(p, area);
        }
    }
}

fn render_empty(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let short = app
        .baseline_sha
        .get(..7)
        .unwrap_or(&app.baseline_sha)
        .to_string();
    let body = format!("No changes since baseline (HEAD: {short})");
    let mid = centered_line(area, &body);
    let p = Paragraph::new(body).alignment(Alignment::Center);
    frame.render_widget(p, mid);
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let mode = if app.follow_mode {
        "[follow]"
    } else {
        "[manual]"
    };
    let current = match &app.selected_path {
        Some(p) => p.display().to_string(),
        None => "--".to_string(),
    };
    let session_added: usize = app.files.iter().map(|f| f.added).sum();
    let session_deleted: usize = app.files.iter().map(|f| f.deleted).sum();
    let head_marker = if app.head_dirty { " head*" } else { "" };
    let body = format!(
        "{mode} {current} | session: +{session_added}/-{session_deleted} {} files{head_marker}",
        app.files.len()
    );

    let mut spans = vec![Span::raw(body)];
    if let Some(err) = &app.last_error {
        spans.push(Span::styled(
            format!("  × {err}"),
            Style::default().fg(Color::Red),
        ));
    }
    let line = Line::from(spans);
    frame.render_widget(Paragraph::new(line), area);
}

/// Compute a 1-row sub-rect at the vertical centre of `area` that the centre
/// helpers below render into.
fn centered_line(area: Rect, _text: &str) -> Rect {
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
    use crate::git::{DiffContent, DiffLine, FileDiff, FileStatus, Hunk, LineKind};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};

    fn fake_app() -> App {
        App {
            root: PathBuf::from("/tmp/fake"),
            git_dir: PathBuf::from("/tmp/fake/.git"),
            baseline_sha: "abcdef1234567890abcdef1234567890abcdef12".into(),
            files: Vec::new(),
            selected: 0,
            selected_path: None,
            diff_scroll: 0,
            follow_mode: true,
            last_error: None,
            head_dirty: false,
            should_quit: false,
        }
    }

    fn modified_file(name: &str, added: usize, deleted: usize, secs: u64) -> FileDiff {
        let mut lines: Vec<DiffLine> = Vec::new();
        for i in 0..added {
            lines.push(DiffLine {
                kind: LineKind::Added,
                content: format!("added line {i}"),
            });
        }
        for i in 0..deleted {
            lines.push(DiffLine {
                kind: LineKind::Deleted,
                content: format!("deleted line {i}"),
            });
        }
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
            }]),
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
        let view = render_to_string(&app, 60, 6);
        assert!(
            view.contains("No changes since baseline (HEAD: abcdef1)"),
            "expected empty state with short SHA, got:\n{view}"
        );
        // Footer should still report 0 files in follow mode.
        assert!(view.contains("[follow] -- | session: +0/-0 0 files"));
    }

    #[test]
    fn render_lists_files_with_status_and_counts() {
        let mut app = fake_app();
        app.files = vec![
            modified_file("src/foo.rs", 3, 1, 200),
            modified_file("README.md", 5, 2, 100),
        ];
        // Pretend the App was sorted + selection refreshed for index 0.
        app.selected = 0;
        app.selected_path = Some(PathBuf::from("src/foo.rs"));

        let view = render_to_string(&app, 80, 10);
        assert!(
            view.contains("src/foo.rs"),
            "view did not list src/foo.rs:\n{view}"
        );
        assert!(
            view.contains("README.md"),
            "view did not list README.md:\n{view}"
        );
        assert!(view.contains("+3/-1"), "expected +3/-1 in foo entry");
        assert!(view.contains("+5/-2"), "expected +5/-2 in readme entry");
    }

    #[test]
    fn render_marks_binary_files_with_bin_suffix_and_placeholder() {
        let mut app = fake_app();
        app.files = vec![binary_file("assets/icon.png")];
        app.selected = 0;
        app.selected_path = Some(PathBuf::from("assets/icon.png"));

        let view = render_to_string(&app, 80, 8);
        // List entry uses 'bin' instead of +N/-M
        assert!(view.contains("bin"), "expected 'bin' marker:\n{view}");
        // Right pane shows the placeholder
        assert!(
            view.contains("[binary file - diff suppressed]"),
            "expected binary placeholder:\n{view}"
        );
    }

    #[test]
    fn render_diff_lines_carry_added_and_deleted_colors() {
        let mut app = fake_app();
        app.files = vec![modified_file("src/foo.rs", 2, 1, 100)];
        app.selected = 0;
        app.selected_path = Some(PathBuf::from("src/foo.rs"));

        // Use the buffer directly so we can inspect Styles.
        let backend = TestBackend::new(80, 12);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| render(f, &app)).expect("draw");
        let buffer = terminal.backend().buffer().clone();

        // Walk every cell, collect (symbol, fg) tuples, and verify that at
        // least one '+' is green and one '-' is red.
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
        assert!(found_added_green, "expected an Added '+' rendered in green");
        assert!(found_deleted_red, "expected a Deleted '-' rendered in red");
    }

    #[test]
    fn render_footer_shows_last_error_in_red_when_set() {
        let mut app = fake_app();
        app.files = vec![modified_file("src/foo.rs", 1, 0, 100)];
        app.selected = 0;
        app.selected_path = Some(PathBuf::from("src/foo.rs"));
        app.last_error = Some("git diff exploded".into());

        let backend = TestBackend::new(80, 6);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| render(f, &app)).expect("draw");
        let buffer = terminal.backend().buffer().clone();

        // The last row is the footer; scan it for "× git diff exploded"
        // rendered in red.
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
        let mut app = fake_app();
        app.files = vec![modified_file("src/foo.rs", 1, 0, 100)];
        app.selected = 0;
        app.selected_path = Some(PathBuf::from("src/foo.rs"));
        app.follow_mode = false;

        let view = render_to_string(&app, 80, 6);
        assert!(view.contains("[manual]"), "expected [manual]:\n{view}");
    }
}
