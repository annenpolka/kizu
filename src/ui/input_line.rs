use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};
use unicode_width::UnicodeWidthStr;

use crate::app::App;

pub(super) struct InputLine {
    text: String,
    prefix: &'static str,
    cursor_pos: usize,
}

pub(super) fn active(app: &App) -> Option<InputLine> {
    if let Some(state) = app.scar_comment.as_ref() {
        Some(InputLine {
            text: state.body.clone(),
            prefix: "> ",
            cursor_pos: state.cursor_pos,
        })
    } else {
        app.search_input.as_ref().map(|input| InputLine {
            text: input.query.clone(),
            prefix: "/",
            cursor_pos: input.cursor_pos,
        })
    }
}

pub(super) fn height(area_width: u16, input: &InputLine) -> u16 {
    let total_width = input.prefix.width() + input.text.width() + 1;
    let width = (area_width as usize).max(1);
    total_width.div_ceil(width).max(1) as u16
}

/// Render a text-input line with wrapping support and a block cursor.
pub(super) fn render(frame: &mut Frame<'_>, area: Rect, input: &InputLine) {
    let before_cursor: String = input.text.chars().take(input.cursor_pos).collect();
    let cursor_char: String = input
        .text
        .chars()
        .nth(input.cursor_pos)
        .map(|c| c.to_string())
        .unwrap_or_default();
    let after_cursor: String = input
        .text
        .chars()
        .skip(input.cursor_pos + cursor_char.chars().count())
        .collect();

    let mut spans = vec![
        Span::styled(
            input.prefix.to_string(),
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(before_cursor.clone(), Style::default().fg(Color::White)),
    ];

    if cursor_char.is_empty() {
        spans.push(Span::styled(
            " ",
            Style::default().fg(Color::Black).bg(Color::White),
        ));
    } else {
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

    let cursor_display_offset = input.prefix.width() + before_cursor.width();
    let width = area.width.max(1) as usize;
    let cursor_y = area.y + (cursor_display_offset / width) as u16;
    let cursor_x = area.x + (cursor_display_offset % width) as u16;
    frame.set_cursor_position((
        cursor_x.min(area.right().saturating_sub(1)),
        cursor_y.min(area.bottom().saturating_sub(1)),
    ));
}
