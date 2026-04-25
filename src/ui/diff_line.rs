use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use super::{
    cursor_bar,
    text_cells::{eof_no_newline_span, wrap_at_chars},
};
use crate::git::LineKind;

/// Classification of a single character position against the active
/// search matches. Drives the style overlay inside the renderer.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SearchHl {
    None,
    Other,
    Current,
}

/// Classify every char in `content` against the match ranges. `matches`
/// are `(byte_start, byte_end, is_current)` in source order; byte
/// offsets are guaranteed to be UTF-8 char boundaries by `find_matches`.
fn classify_chars_by_match(content: &str, matches: &[(usize, usize, bool)]) -> Vec<SearchHl> {
    let n = content.chars().count();
    let mut out = vec![SearchHl::None; n];
    if matches.is_empty() {
        return out;
    }
    for (char_idx, (byte_pos, _)) in content.char_indices().enumerate() {
        for &(bs, be, is_current) in matches {
            if byte_pos >= bs && byte_pos < be {
                let new_kind = if is_current {
                    SearchHl::Current
                } else {
                    SearchHl::Other
                };
                // Current wins over Other on overlap (defensive).
                if out[char_idx] != SearchHl::Current || is_current {
                    out[char_idx] = new_kind;
                }
            }
        }
    }
    out
}

/// Compose a base diff-line style with the search-highlight overlay
/// for one char.
fn apply_search_overlay(base: Style, fg: Color, hl: SearchHl) -> Style {
    match hl {
        SearchHl::None => base.fg(fg),
        SearchHl::Other => base
            .fg(fg)
            .remove_modifier(Modifier::DIM)
            .add_modifier(Modifier::UNDERLINED | Modifier::BOLD),
        SearchHl::Current => Style::default()
            .bg(Color::Yellow)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD),
    }
}

/// Wrap-mode variant of [`render_diff_line`]. Splits `line.content`
/// at `body_width` cells and paints every visual row with the
/// delta-style background color.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_diff_line_wrapped(
    line: &crate::git::DiffLine,
    is_selected: bool,
    cursor_sub: Option<usize>,
    body_width: usize,
    hl: Option<&crate::highlight::Highlighter>,
    file_path: Option<&std::path::Path>,
    bg_added: Color,
    bg_deleted: Color,
    search_matches: &[(usize, usize, bool)],
) -> Vec<Line<'static>> {
    use unicode_width::UnicodeWidthStr;
    // ADR-0014: background-color diff rendering. Focused hunks keep
    // full brightness; unfocused hunks wear `Modifier::DIM` so the
    // eye still flows to the cursor band. Context rows use the
    // terminal default inside the focus and dark-gray + DIM outside.
    let bg = match line.kind {
        LineKind::Added => Some(bg_added),
        LineKind::Deleted => Some(bg_deleted),
        LineKind::Context => None,
    };
    let base_style = match (bg, is_selected) {
        (Some(b), true) => Style::default().bg(b),
        (Some(b), false) => Style::default().bg(b).add_modifier(Modifier::DIM),
        (None, true) => Style::default(),
        (None, false) => Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
    };
    // Per-char fg + search highlight kind, built once for the whole
    // line. Distributed across wrapped chunks below so a match that
    // spans a wrap boundary still paints cleanly on both sides.
    let char_fgs = per_char_fg(&line.content, hl, file_path);
    let char_hls = classify_chars_by_match(&line.content, search_matches);

    let chunks = wrap_at_chars(&line.content, body_width.max(1));
    let last_idx = chunks.len().saturating_sub(1);
    let mut char_offset = 0usize;
    let cursor_line = cursor_sub.map(|s| s.min(last_idx));

    chunks
        .into_iter()
        .enumerate()
        .map(|(i, chunk)| {
            let is_last = i == last_idx;
            let bar = cursor_bar(cursor_line == Some(i), is_selected);
            let marker_reserve = if is_last && !line.has_trailing_newline {
                1
            } else {
                0
            };
            let chunk_char_count = chunk.chars().count();
            let chunk_cell_count = UnicodeWidthStr::width(chunk);
            let pad = body_width.saturating_sub(chunk_cell_count + marker_reserve);

            let mut spans = vec![bar];

            let chunk_fgs = &char_fgs[char_offset..char_offset + chunk_char_count];
            let chunk_hls = &char_hls[char_offset..char_offset + chunk_char_count];
            let chunk_chars: Vec<char> = chunk.chars().collect();
            let mut run_start = 0usize;
            while run_start < chunk_chars.len() {
                let run_attr = (chunk_fgs[run_start], chunk_hls[run_start]);
                let run_end = (run_start + 1..chunk_chars.len())
                    .find(|&j| (chunk_fgs[j], chunk_hls[j]) != run_attr)
                    .unwrap_or(chunk_chars.len());
                let text: String = chunk_chars[run_start..run_end].iter().collect();
                let style = apply_search_overlay(base_style, run_attr.0, run_attr.1);
                spans.push(Span::styled(text, style));
                run_start = run_end;
            }
            if pad > 0 {
                spans.push(Span::styled(" ".repeat(pad), base_style));
            }

            if is_last && !line.has_trailing_newline {
                spans.push(eof_no_newline_span(bg));
            }
            char_offset += chunk_char_count;
            Line::from(spans)
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render_diff_line(
    line: &crate::git::DiffLine,
    is_selected: bool,
    is_cursor: bool,
    body_width: usize,
    hl: Option<&crate::highlight::Highlighter>,
    file_path: Option<&std::path::Path>,
    bg_added: Color,
    bg_deleted: Color,
    search_matches: &[(usize, usize, bool)],
) -> Line<'static> {
    let bg = match line.kind {
        LineKind::Added => Some(bg_added),
        LineKind::Deleted => Some(bg_deleted),
        LineKind::Context => None,
    };
    let bar = cursor_bar(is_cursor, is_selected);
    let base_style = match (bg, is_selected) {
        (Some(b), true) => Style::default().bg(b),
        (Some(b), false) => Style::default().bg(b).add_modifier(Modifier::DIM),
        (None, true) => Style::default(),
        (None, false) => Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
    };

    let char_fgs = per_char_fg(&line.content, hl, file_path);
    let char_hls = classify_chars_by_match(&line.content, search_matches);

    let eof_marker = !line.has_trailing_newline;
    let body_budget = if eof_marker {
        body_width.saturating_sub(1)
    } else {
        body_width
    };
    use unicode_width::UnicodeWidthChar;
    let mut spans = vec![bar];
    let mut cells_emitted = 0usize;
    let chars: Vec<char> = line.content.chars().collect();

    let mut run_start = 0usize;
    while run_start < chars.len() {
        let run_attr = (char_fgs[run_start], char_hls[run_start]);
        let mut run_end = run_start + 1;
        let mut run_cells = chars[run_start].width().unwrap_or(0);
        if cells_emitted + run_cells > body_budget {
            break;
        }
        while run_end < chars.len() {
            let candidate_attr = (char_fgs[run_end], char_hls[run_end]);
            if candidate_attr != run_attr {
                break;
            }
            let w = chars[run_end].width().unwrap_or(0);
            if cells_emitted + run_cells + w > body_budget {
                break;
            }
            run_cells += w;
            run_end += 1;
        }
        let text: String = chars[run_start..run_end].iter().collect();
        let style = apply_search_overlay(base_style, run_attr.0, run_attr.1);
        spans.push(Span::styled(text, style));
        cells_emitted += run_cells;
        run_start = run_end;
        if cells_emitted >= body_budget {
            break;
        }
    }

    if cells_emitted < body_budget {
        spans.push(Span::styled(
            " ".repeat(body_budget - cells_emitted),
            base_style,
        ));
    }
    if eof_marker {
        spans.push(eof_no_newline_span(bg));
    }
    Line::from(spans)
}

/// Build a per-char foreground color vector for `content`. Falls back
/// to `Color::Reset` for every char when no highlighter is available
/// or the file extension is unknown.
fn per_char_fg(
    content: &str,
    hl: Option<&crate::highlight::Highlighter>,
    file_path: Option<&std::path::Path>,
) -> Vec<Color> {
    let n = content.chars().count();
    if let (Some(hl), Some(path)) = (hl, file_path) {
        let tokens = hl.highlight_line(content, path);
        if tokens.len() > 1 || tokens.first().is_some_and(|t| t.fg != Color::Reset) {
            let mut out = Vec::with_capacity(n);
            for tok in &tokens {
                for _ in tok.text.chars() {
                    out.push(tok.fg);
                }
            }
            if out.len() == n {
                return out;
            }
            // Token char count drift should not happen with syntect.
            // Fall through to the flat-color fallback instead of
            // panicking in the renderer.
        }
    }
    vec![Color::Reset; n]
}
