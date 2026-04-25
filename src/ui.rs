use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::{
    style::{Color, Modifier, Style},
    text::Span,
};

mod diff_line;
mod diff_view;
mod empty_state;
mod file_view;
mod footer;
mod frame;
mod geometry;
mod input_line;
mod line_numbers;
mod overlays;
mod text_cells;

#[cfg(test)]
use diff_line::{render_diff_line, render_diff_line_wrapped};
#[cfg(test)]
use file_view::{render_file_view_line, render_file_view_line_wrapped};
pub(crate) use footer::format_local_time;
pub use frame::render;
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

#[cfg(test)]
use crate::app::App;

/// Delta-style background color defaults. Production code reads these
/// from [`crate::config::ColorConfig`]; the constants remain for tests
/// that assert on default-config rendering (ADR-0014).
#[cfg(test)]
const BG_ADDED: Color = Color::Rgb(10, 50, 10);
#[cfg(test)]
const BG_DELETED: Color = Color::Rgb(60, 10, 10);

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

#[cfg(test)]
mod tests;
