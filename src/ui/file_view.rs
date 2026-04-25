use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use super::{
    cursor_bar,
    geometry::RenderGeometry,
    line_numbers::{add_line_number_gutters, file_ln_span},
    text_cells::{eof_no_newline_span, take_cells, wrap_at_chars},
};

pub(super) fn render_file_view(
    frame: &mut Frame<'_>,
    area: Rect,
    fv: &crate::app::FileViewState,
    wrap_lines: bool,
    show_line_numbers: bool,
    hl: Option<&crate::highlight::Highlighter>,
    effective_top: usize,
) {
    let height = area.height as usize;
    // v0.5: mirror render_scroll's single-source body_width calc so
    // VisualIndex::build_lines and the numbered renderer see the same value
    // (Codex review Critical-1).
    let geometry =
        RenderGeometry::for_file_view(area.width as usize, show_line_numbers, fv.lines.len());
    let body_width = geometry.body_width;
    fv.last_body_width.set(body_width);
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(height);

    // v0.5 M2: draw `∅` on the last line only when the on-disk file
    // is missing a trailing newline. Mid-file rows never get the marker.
    let last_line_idx = fv.lines.len().saturating_sub(1);
    let mark_last_no_newline = !fv.last_line_has_trailing_newline;
    if wrap_lines {
        let vi = crate::app::VisualIndex::build_lines(&fv.lines, Some(body_width));
        let (mut line_idx, mut skip_remaining) = vi.logical_at(effective_top);
        while line_idx < fv.lines.len() && lines.len() < height {
            let base_style = if let Some(&bg) = fv.line_bg.get(&line_idx) {
                Style::default().bg(bg)
            } else {
                Style::default()
            };
            let cursor_sub = (line_idx == fv.cursor).then_some(fv.cursor_sub_row);
            let show_eof_marker = mark_last_no_newline && line_idx == last_line_idx;
            let rendered = render_file_view_line_wrapped(
                &fv.lines[line_idx],
                cursor_sub,
                body_width,
                base_style,
                hl,
                &fv.path,
                show_eof_marker,
            );
            let rendered = if geometry.effective_show_ln {
                add_line_number_gutters(
                    rendered,
                    file_ln_span(line_idx + 1, &geometry.ln_gutter),
                    &geometry.ln_gutter,
                )
            } else {
                rendered
            };
            let mut take = rendered.into_iter();
            for _ in 0..skip_remaining {
                if take.next().is_none() {
                    break;
                }
            }
            skip_remaining = 0;
            for line in take {
                if lines.len() >= height {
                    break;
                }
                lines.push(line);
            }
            line_idx += 1;
        }
    } else {
        for i in 0..height {
            let line_idx = effective_top + i;
            if line_idx >= fv.lines.len() {
                break;
            }
            let base_style = if let Some(&bg) = fv.line_bg.get(&line_idx) {
                Style::default().bg(bg)
            } else {
                Style::default()
            };
            let show_eof_marker = mark_last_no_newline && line_idx == last_line_idx;
            let rendered = render_file_view_line(
                &fv.lines[line_idx],
                line_idx == fv.cursor,
                body_width,
                base_style,
                hl,
                &fv.path,
                show_eof_marker,
            );
            let rendered = if geometry.effective_show_ln {
                let mut lines = add_line_number_gutters(
                    vec![rendered],
                    file_ln_span(line_idx + 1, &geometry.ln_gutter),
                    &geometry.ln_gutter,
                );
                lines.remove(0)
            } else {
                rendered
            };
            lines.push(rendered);
        }
    }

    while lines.len() < height {
        lines.push(Line::from(Span::styled(
            "~",
            Style::default().fg(Color::DarkGray),
        )));
    }

    frame.render_widget(Paragraph::new(lines), area);
}

pub(super) fn render_file_view_line(
    content: &str,
    is_cursor: bool,
    body_width: usize,
    base_style: Style,
    hl: Option<&crate::highlight::Highlighter>,
    file_path: &std::path::Path,
    show_eof_marker: bool,
) -> Line<'static> {
    let bar = cursor_bar(is_cursor, false);
    // v0.5 M2: reserve 1 cell at the end when we need to paint the
    // EOF-no-newline marker. body_budget governs both content fit
    // and pad so the `∅` lands inside body_width (never overflows).
    let body_budget = if show_eof_marker {
        body_width.saturating_sub(1)
    } else {
        body_width
    };

    if let Some(hl) = hl {
        let tokens = hl.highlight_line(content, file_path);
        if tokens.len() > 1 || tokens.first().is_some_and(|t| t.fg != Color::Reset) {
            let mut spans = vec![bar];
            let mut cells_emitted = 0usize;
            for token in &tokens {
                let remaining = body_budget.saturating_sub(cells_emitted);
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
            if cells_emitted < body_budget {
                spans.push(Span::styled(
                    " ".repeat(body_budget - cells_emitted),
                    base_style,
                ));
            }
            if show_eof_marker {
                spans.push(eof_no_newline_span(base_style.bg));
            }
            return Line::from(spans);
        }
    }

    use unicode_width::UnicodeWidthStr;
    let content_cells = UnicodeWidthStr::width(content);
    let padded_body: String = if content_cells >= body_budget {
        let (truncated, _) = take_cells(content, body_budget);
        truncated
    } else {
        let pad = body_budget - content_cells;
        content
            .chars()
            .chain(std::iter::repeat_n(' ', pad))
            .collect()
    };
    let mut spans = vec![bar, Span::styled(padded_body, base_style)];
    if show_eof_marker {
        spans.push(eof_no_newline_span(base_style.bg));
    }
    Line::from(spans)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render_file_view_line_wrapped(
    content: &str,
    cursor_sub: Option<usize>,
    body_width: usize,
    base_style: Style,
    hl: Option<&crate::highlight::Highlighter>,
    file_path: &std::path::Path,
    show_eof_marker: bool,
) -> Vec<Line<'static>> {
    use unicode_width::UnicodeWidthStr;

    let tokens: Option<Vec<crate::highlight::HlToken>> = hl.and_then(|hl| {
        let toks = hl.highlight_line(content, file_path);
        (toks.len() > 1 || toks.first().is_some_and(|t| t.fg != Color::Reset)).then_some(toks)
    });

    let char_colors: Vec<Color> = if let Some(ref toks) = tokens {
        let mut colors = Vec::with_capacity(content.len());
        for tok in toks {
            for _ in tok.text.chars() {
                colors.push(tok.fg);
            }
        }
        colors
    } else {
        Vec::new()
    };

    let chunks = wrap_at_chars(content, body_width.max(1));
    let last_idx = chunks.len().saturating_sub(1);
    let cursor_line = cursor_sub.map(|s| s.min(last_idx));
    let mut char_offset = 0usize;

    chunks
        .into_iter()
        .enumerate()
        .map(|(i, chunk)| {
            let bar = cursor_bar(cursor_line == Some(i), cursor_line.is_some());

            let is_last = i == last_idx;
            let emit_marker = show_eof_marker && is_last;
            let body_budget = if emit_marker {
                body_width.saturating_sub(1)
            } else {
                body_width
            };
            let chunk_char_count = chunk.chars().count();
            let chunk_cell_count = UnicodeWidthStr::width(chunk).min(body_budget);
            let pad = body_budget.saturating_sub(chunk_cell_count);
            let mut spans = vec![bar];

            if !char_colors.is_empty() {
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
                let padded_body: String =
                    chunk.chars().chain(std::iter::repeat_n(' ', pad)).collect();
                spans.push(Span::styled(padded_body, base_style));
            }

            if emit_marker {
                spans.push(eof_no_newline_span(base_style.bg));
            }

            char_offset += chunk_char_count;
            Line::from(spans)
        })
        .collect()
}
