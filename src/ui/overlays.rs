use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::app::App;
use crate::git::{DiffContent, FileStatus};

fn key_label(ch: char) -> String {
    if ch == ' ' {
        "Space".to_string()
    } else {
        ch.to_string()
    }
}

fn help_section(title: &'static str) -> Line<'static> {
    Line::from(Span::styled(
        title,
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    ))
}

fn help_row(key: impl Into<String>, description: impl Into<String>) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{:<14}", key.into()),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(description.into()),
    ])
}

pub(super) fn render_help_overlay(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let popup_area = centered_rect(72, 72, area);
    frame.render_widget(Clear, popup_area);

    let block = Block::default().borders(Borders::ALL).title(" Help ");
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let k = &app.config.keys;
    let columns =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).split(inner);

    let left = vec![
        help_section("Navigation"),
        help_row("j / ↓", "next change"),
        help_row("k / ↑", "previous change"),
        help_row("J / K", "move one visual row"),
        help_row("h / l", "previous / next hunk"),
        help_row("g / G", "top / bottom"),
        help_row("Ctrl-d/u", "half-page down / up"),
        Line::raw(""),
        help_section("Review"),
        help_row(key_label(k.ask), "ask scar"),
        help_row(key_label(k.reject), "reject scar"),
        help_row(key_label(k.comment), "free comment scar"),
        help_row(key_label(k.revert), "revert hunk"),
        help_row(key_label(k.seen), "seen / fold hunk"),
        help_row(key_label(k.undo), "undo scar"),
        help_row(key_label(k.editor), "open editor"),
    ];

    let right = vec![
        help_section("Views"),
        help_row("Enter", "file view / back"),
        help_row("Tab", "stream / diff"),
        help_row(key_label(k.follow), "follow latest"),
        help_row(key_label(k.picker), "picker"),
        help_row(key_label(k.cursor_placement), "center / top cursor"),
        help_row(key_label(k.wrap_toggle), "wrap"),
        help_row(key_label(k.line_numbers_toggle), "line numbers"),
        Line::raw(""),
        help_section("Search"),
        help_row(key_label(k.search), "search"),
        help_row(key_label(k.search_next), "next match"),
        help_row(key_label(k.search_prev), "previous match"),
        Line::raw(""),
        help_section("Other"),
        help_row("? / Esc", "close help"),
        help_row("q", "quit"),
    ];

    frame.render_widget(Paragraph::new(left).wrap(Wrap { trim: false }), columns[0]);
    frame.render_widget(Paragraph::new(right).wrap(Wrap { trim: false }), columns[1]);
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

pub(super) fn render_picker(frame: &mut Frame<'_>, area: Rect, app: &App) {
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
            let mtime = super::format_mtime(file.mtime);
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
