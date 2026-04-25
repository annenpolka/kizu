use ratatui::{
    Frame,
    layout::{Constraint, Layout},
};

use super::{
    diff_view::render_scroll, empty_state, file_view::render_file_view, footer, input_line,
    overlays,
};
use crate::app::App;

/// Render the entire kizu frame: main view + optional input row + footer,
/// with modal overlays drawn last.
pub fn render(frame: &mut Frame<'_>, app: &App) {
    let area = frame.area();
    let input = input_line::active(app);
    let input_height = input
        .as_ref()
        .map(|line| input_line::height(area.width, line))
        .unwrap_or(0);

    let chunks = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(input_height),
        Constraint::Length(1),
    ])
    .split(area);
    let main = chunks[0];
    let input_area = chunks[1];
    let footer_area = chunks[2];

    if let Some(fv) = app.file_view.as_ref() {
        app.last_body_height.set(main.height as usize);
        let hl = app
            .highlighter
            .get_or_init(crate::highlight::Highlighter::new);
        let effective_top = if let Some(anim) = &fv.anim {
            let (value, _done) = anim.sample(fv.scroll_top as f32, std::time::Instant::now());
            value.round() as usize
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
        empty_state::render(frame, main, app);
    } else {
        render_scroll(frame, main, app);
    }

    if let Some(input) = input.as_ref() {
        input_line::render(frame, input_area, input);
    }

    footer::render_footer(frame, footer_area, app);

    if app.picker.is_some() {
        overlays::render_picker(frame, area, app);
    }

    if app.help_overlay {
        overlays::render_help_overlay(frame, area, app);
    }
}
