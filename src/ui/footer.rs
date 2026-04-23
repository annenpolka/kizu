use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::app::{App, ViewMode};
use crate::git::FileStatus;

/// Convert a Unix epoch millisecond timestamp to a local-time
/// `HH:MM:SS` string. Uses `libc::localtime_r` on Unix for
/// timezone-aware conversion; falls back to UTC on other platforms.
pub(crate) fn format_local_time(timestamp_ms: u64) -> String {
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FooterDensity {
    Full,
    Compact,
    Minimal,
}

fn spans_display_width(spans: &[Span<'static>]) -> usize {
    use unicode_width::UnicodeWidthStr;
    spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum()
}

fn truncate_display(s: &str, max_width: usize) -> String {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

    if max_width == 0 {
        return String::new();
    }
    if UnicodeWidthStr::width(s) <= max_width {
        return s.to_string();
    }
    if max_width == 1 {
        return "…".to_string();
    }

    let mut out = String::new();
    let mut used = 0usize;
    let limit = max_width - 1;
    for ch in s.chars() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w > limit {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    out
}

/// Walk `densities` from preferred (e.g. Full) to fallback (e.g.
/// Minimal), building each variant only when its predecessor did not
/// fit. Returns the first variant that fits within `width`, or the
/// last-built one when none fit. `build` is the per-variant span
/// constructor. Wide terminals pay for one `build` call per frame
/// instead of all three.
fn choose_footer_variant<F>(
    densities: &[FooterDensity],
    width: u16,
    mut build: F,
) -> Vec<Span<'static>>
where
    F: FnMut(FooterDensity) -> Vec<Span<'static>>,
{
    let width = width as usize;
    let mut last: Option<Vec<Span<'static>>> = None;
    for &density in densities {
        let candidate = build(density);
        if spans_display_width(&candidate) <= width {
            return candidate;
        }
        last = Some(candidate);
    }
    last.unwrap_or_default()
}

fn sep_span(dim: Style) -> Span<'static> {
    Span::styled(" │ ", dim)
}

fn slash_span(dim: Style) -> Span<'static> {
    Span::styled(" / ", dim)
}

fn footer_mode(app: &App) -> (&'static str, Color) {
    if app.picker.is_some() {
        ("[picker]", Color::Magenta)
    } else if app.scar_comment.is_some() {
        ("[scar]", Color::Magenta)
    } else if app.revert_confirm.is_some() {
        ("[revert?]", Color::Red)
    } else if app.search_input.is_some() {
        ("[search]", Color::Yellow)
    } else if app.file_view.is_some() {
        ("[file view]", Color::Cyan)
    } else if app.view_mode == ViewMode::Stream {
        ("[stream]", Color::Blue)
    } else if app.follow_mode {
        ("[follow]", Color::Green)
    } else {
        ("[manual]", Color::Yellow)
    }
}

fn push_mode(spans: &mut Vec<Span<'static>>, app: &App, bold: Modifier) {
    let (mode_text, mode_color) = footer_mode(app);
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        mode_text,
        Style::default().fg(mode_color).add_modifier(bold),
    ));
    spans.push(Span::raw(" "));
}

fn line_numbers_label(app: &App) -> &'static str {
    // Stream mode forces the gutter off regardless of the flag, so
    // it collapses into the same "nums off" label as the normal
    // disabled state. `line_numbers_style` still distinguishes them
    // visually (dim vs. plain cyan) since Stream is non-toggleable.
    if app.show_line_numbers && app.view_mode != ViewMode::Stream {
        "nums on"
    } else {
        "nums off"
    }
}

fn line_numbers_style(app: &App, dim: Style, bold: Modifier) -> Style {
    if app.view_mode == ViewMode::Stream {
        dim
    } else if app.show_line_numbers {
        Style::default().fg(Color::Cyan).add_modifier(bold)
    } else {
        Style::default().fg(Color::Cyan)
    }
}

fn wrap_label(app: &App) -> &'static str {
    if app.wrap_lines { "wrap" } else { "nowrap" }
}

fn push_line_numbers_full(spans: &mut Vec<Span<'static>>, app: &App, dim: Style, bold: Modifier) {
    spans.push(sep_span(dim));
    spans.push(Span::styled(
        line_numbers_label(app),
        line_numbers_style(app, dim, bold),
    ));
}

fn push_compact_toggles(
    spans: &mut Vec<Span<'static>>,
    app: &App,
    dim: Style,
    bold: Modifier,
    include_picker: bool,
) {
    spans.push(Span::styled(
        app.cursor_placement.label(),
        Style::default().fg(Color::Cyan).add_modifier(bold),
    ));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        wrap_label(app),
        Style::default().fg(Color::Cyan).add_modifier(bold),
    ));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        line_numbers_label(app),
        line_numbers_style(app, dim, bold),
    ));
    if include_picker {
        spans.push(Span::raw(" "));
        spans.push(Span::styled("? help", Style::default().fg(Color::Magenta)));
    }
}

fn session_counts(app: &App) -> (usize, usize, usize) {
    let (added, deleted) = app
        .files
        .iter()
        .fold((0usize, 0usize), |(a, d), f| (a + f.added, d + f.deleted));
    (added, deleted, app.files.len())
}

fn push_session_full(spans: &mut Vec<Span<'static>>, app: &App, dim: Style, bold: Modifier) {
    let (session_added, session_deleted, files_len) = session_counts(app);
    spans.push(sep_span(dim));
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
        format!("{files_len} files"),
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
}

fn push_session_compact(spans: &mut Vec<Span<'static>>, app: &App) {
    let (session_added, session_deleted, files_len) = session_counts(app);
    spans.push(Span::raw(format!(
        "+{session_added}/-{session_deleted} {files_len}f"
    )));
    if app.head_dirty {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            "HEAD*",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }
}

fn current_path_and_color(app: &App) -> (String, Color) {
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
    (current_path, path_color)
}

fn push_diagnostics(
    spans: &mut Vec<Span<'static>>,
    app: &App,
    density: FooterDensity,
    dim: Style,
    bold: Modifier,
) {
    if let Some(msg) = app.watcher_health.summary() {
        spans.push(sep_span(dim));
        spans.push(Span::styled(
            "⚠ WATCHER",
            Style::default().fg(Color::Red).add_modifier(bold),
        ));
        if density != FooterDensity::Minimal {
            spans.push(Span::raw(" "));
            let msg = if density == FooterDensity::Full {
                msg
            } else {
                truncate_display(&msg, 28)
            };
            spans.push(Span::styled(msg, Style::default().fg(Color::Red)));
        }
    }

    if let Some(msg) = &app.input_health {
        spans.push(sep_span(dim));
        spans.push(Span::styled(
            "⚠ INPUT",
            Style::default().fg(Color::Red).add_modifier(bold),
        ));
        if density != FooterDensity::Minimal {
            spans.push(Span::raw(" "));
            let msg = if density == FooterDensity::Full {
                msg.clone()
            } else {
                truncate_display(msg, 28)
            };
            spans.push(Span::styled(msg, Style::default().fg(Color::Red)));
        }
    }

    if let Some(err) = &app.last_error {
        spans.push(sep_span(dim));
        spans.push(Span::styled(
            "×",
            Style::default().fg(Color::Red).add_modifier(bold),
        ));
        if density != FooterDensity::Minimal {
            spans.push(Span::raw(" "));
            let err = if density == FooterDensity::Full {
                err.clone()
            } else {
                truncate_display(err, 28)
            };
            spans.push(Span::styled(err, Style::default().fg(Color::Red)));
        }
    }
}

fn build_footer_spans(
    app: &App,
    density: FooterDensity,
    dim: Style,
    bold: Modifier,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    push_mode(&mut spans, app, bold);

    if app.picker.is_some() {
        spans.push(sep_span(dim));
        match density {
            FooterDensity::Full => {
                spans.push(Span::styled(
                    "type to filter",
                    Style::default().fg(Color::Yellow),
                ));
                spans.push(slash_span(dim));
                spans.push(Span::styled(
                    "↑↓ Ctrl-n/p",
                    Style::default().fg(Color::Cyan),
                ));
                spans.push(Span::raw(" "));
                spans.push(Span::styled("move", dim));
                spans.push(slash_span(dim));
                spans.push(Span::styled("Enter", Style::default().fg(Color::Green)));
                spans.push(Span::raw(" "));
                spans.push(Span::styled("jump", dim));
                spans.push(slash_span(dim));
                spans.push(Span::styled("Esc", Style::default().fg(Color::Red)));
                spans.push(Span::raw(" "));
                spans.push(Span::styled("cancel", dim));
            }
            FooterDensity::Compact => {
                spans.push(Span::styled("filter", Style::default().fg(Color::Yellow)));
                spans.push(Span::raw(" "));
                spans.push(Span::styled("Enter", Style::default().fg(Color::Green)));
                spans.push(Span::styled("/Esc", dim));
            }
            FooterDensity::Minimal => {
                spans.push(Span::styled("filter", Style::default().fg(Color::Yellow)));
                spans.push(Span::raw(" "));
                spans.push(Span::styled("Esc", Style::default().fg(Color::Red)));
            }
        }
    } else if let Some(fv) = app.file_view.as_ref() {
        match density {
            FooterDensity::Full => {
                spans.push(sep_span(dim));
                spans.push(Span::styled(
                    wrap_label(app),
                    Style::default().fg(Color::Cyan).add_modifier(bold),
                ));
                push_line_numbers_full(&mut spans, app, dim, bold);
                spans.push(sep_span(dim));
                spans.push(Span::styled(
                    fv.path.display().to_string(),
                    Style::default().fg(Color::Cyan).add_modifier(bold),
                ));
                spans.push(Span::styled(
                    format!(" [{}/{}]", fv.cursor + 1, fv.lines.len()),
                    Style::default().fg(Color::DarkGray),
                ));
                spans.push(sep_span(dim));
                spans.push(Span::styled("Enter", Style::default().fg(Color::Green)));
                spans.push(Span::styled("/", dim));
                spans.push(Span::styled("Esc", Style::default().fg(Color::Red)));
                spans.push(Span::raw(" "));
                spans.push(Span::styled("back", dim));
            }
            FooterDensity::Compact => {
                spans.push(sep_span(dim));
                push_compact_toggles(&mut spans, app, dim, bold, false);
                spans.push(sep_span(dim));
                spans.push(Span::styled(
                    truncate_display(&fv.path.display().to_string(), 18),
                    Style::default().fg(Color::Cyan).add_modifier(bold),
                ));
                spans.push(Span::raw(format!(" {}/{}", fv.cursor + 1, fv.lines.len())));
                spans.push(sep_span(dim));
                spans.push(Span::styled("Esc", Style::default().fg(Color::Red)));
                spans.push(Span::raw(" "));
                spans.push(Span::styled("back", dim));
            }
            FooterDensity::Minimal => {
                spans.push(sep_span(dim));
                spans.push(Span::raw(format!("{}/{}", fv.cursor + 1, fv.lines.len())));
                spans.push(sep_span(dim));
                spans.push(Span::styled(
                    wrap_label(app),
                    Style::default().fg(Color::Cyan).add_modifier(bold),
                ));
                spans.push(Span::raw(" "));
                spans.push(Span::styled(
                    line_numbers_label(app),
                    line_numbers_style(app, dim, bold),
                ));
                spans.push(Span::raw(" "));
                spans.push(Span::styled("Esc", Style::default().fg(Color::Red)));
            }
        }
    } else if app.search_input.is_some() {
        spans.push(sep_span(dim));
        spans.push(Span::styled("Enter", Style::default().fg(Color::Green)));
        spans.push(Span::raw(" "));
        spans.push(Span::styled("find", dim));
        if density != FooterDensity::Minimal {
            spans.push(slash_span(dim));
        } else {
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled("Esc", Style::default().fg(Color::Red)));
        if density != FooterDensity::Minimal {
            spans.push(Span::raw(" "));
            spans.push(Span::styled("cancel", dim));
        }
    } else if let Some(state) = app.revert_confirm.as_ref() {
        spans.push(sep_span(dim));
        match density {
            FooterDensity::Full => spans.push(Span::styled(
                format!("revert hunk in {} ?", state.file_path.display()),
                Style::default().fg(Color::Red).add_modifier(bold),
            )),
            FooterDensity::Compact => spans.push(Span::styled(
                format!(
                    "revert {} ?",
                    truncate_display(&state.file_path.display().to_string(), 24)
                ),
                Style::default().fg(Color::Red).add_modifier(bold),
            )),
            FooterDensity::Minimal => spans.push(Span::styled(
                "revert ?",
                Style::default().fg(Color::Red).add_modifier(bold),
            )),
        }
        spans.push(Span::raw(" "));
        spans.push(Span::styled("(y/N)", Style::default().fg(Color::Yellow)));
    } else if app.scar_comment.is_some() {
        spans.push(sep_span(dim));
        spans.push(Span::styled("Enter", Style::default().fg(Color::Green)));
        spans.push(Span::raw(" "));
        spans.push(Span::styled("save", dim));
        if density != FooterDensity::Minimal {
            spans.push(slash_span(dim));
        } else {
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled("Esc", Style::default().fg(Color::Red)));
        if density != FooterDensity::Minimal {
            spans.push(Span::raw(" "));
            spans.push(Span::styled("cancel", dim));
        }
    } else {
        let (current_path, path_color) = current_path_and_color(app);
        match density {
            FooterDensity::Full => {
                spans.push(sep_span(dim));
                spans.push(Span::styled(
                    current_path,
                    Style::default().fg(path_color).add_modifier(bold),
                ));
                push_session_full(&mut spans, app, dim, bold);

                if let Some(state) = app.search.as_ref() {
                    spans.push(sep_span(dim));
                    spans.push(Span::styled(
                        format!("/{}", state.query),
                        Style::default().fg(Color::Yellow).add_modifier(bold),
                    ));
                    spans.push(Span::raw(" "));
                    let position = if state.matches.is_empty() {
                        "[0/0]".to_string()
                    } else {
                        format!("[{}/{}]", state.current + 1, state.matches.len())
                    };
                    spans.push(Span::styled(position, Style::default().fg(Color::DarkGray)));
                }

                spans.push(sep_span(dim));
                spans.push(Span::styled(
                    app.cursor_placement.label(),
                    Style::default().fg(Color::Cyan).add_modifier(bold),
                ));

                spans.push(sep_span(dim));
                spans.push(Span::styled(
                    wrap_label(app),
                    Style::default().fg(Color::Cyan).add_modifier(bold),
                ));

                push_line_numbers_full(&mut spans, app, dim, bold);

                spans.push(sep_span(dim));
                spans.push(Span::styled("? help", Style::default().fg(Color::Magenta)));
            }
            FooterDensity::Compact => {
                spans.push(sep_span(dim));
                spans.push(Span::styled(
                    truncate_display(&current_path, 18),
                    Style::default().fg(path_color).add_modifier(bold),
                ));
                spans.push(sep_span(dim));
                push_session_compact(&mut spans, app);
                if let Some(state) = app.search.as_ref() {
                    spans.push(sep_span(dim));
                    spans.push(Span::styled(
                        truncate_display(&format!("/{}", state.query), 16),
                        Style::default().fg(Color::Yellow).add_modifier(bold),
                    ));
                    spans.push(Span::raw(" "));
                    let position = if state.matches.is_empty() {
                        "[0/0]".to_string()
                    } else {
                        format!("[{}/{}]", state.current + 1, state.matches.len())
                    };
                    spans.push(Span::styled(position, Style::default().fg(Color::DarkGray)));
                }
                spans.push(sep_span(dim));
                push_compact_toggles(&mut spans, app, dim, bold, true);
            }
            FooterDensity::Minimal => {
                spans.push(sep_span(dim));
                push_session_compact(&mut spans, app);
                if let Some(state) = app.search.as_ref() {
                    spans.push(Span::raw(" "));
                    let position = if state.matches.is_empty() {
                        "[0/0]".to_string()
                    } else {
                        format!("[{}/{}]", state.current + 1, state.matches.len())
                    };
                    spans.push(Span::styled(position, Style::default().fg(Color::DarkGray)));
                }
                spans.push(sep_span(dim));
                push_compact_toggles(&mut spans, app, dim, bold, true);
            }
        }
    }

    push_diagnostics(&mut spans, app, density, dim, bold);
    spans
}

pub(super) fn render_footer(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let dim = Style::default().fg(Color::DarkGray);
    let bold = Modifier::BOLD;
    let spans = choose_footer_variant(
        &[
            FooterDensity::Full,
            FooterDensity::Compact,
            FooterDensity::Minimal,
        ],
        area.width,
        |density| build_footer_spans(app, density, dim, bold),
    );
    let line = Line::from(spans);
    frame.render_widget(Paragraph::new(line), area);
}
