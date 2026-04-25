use ratatui::{
    style::{Color, Modifier, Style},
    text::Span,
};

/// Split `content` into chunks whose **display width** is at most
/// `width` cells. Always returns at least one chunk; an empty input
/// produces `[""]`. CJK / emoji / other wide characters consume 2
/// cells apiece, so a body width of 40 cells holds roughly 20 kanji.
///
/// Zero-width combining marks (`char.width()` returns `Some(0)`) and
/// control chars (`None`) are folded into the current chunk without
/// advancing the cell counter so they stick with their preceding base
/// glyph instead of getting orphaned onto a new visual row.
pub(super) fn wrap_at_chars(content: &str, width: usize) -> Vec<&str> {
    use unicode_width::UnicodeWidthChar;
    if content.is_empty() || width == 0 {
        return vec![content];
    }
    let mut chunks = Vec::new();
    let mut chunk_start = 0usize;
    let mut chunk_cells = 0usize;
    for (idx, ch) in content.char_indices() {
        let ch_cells = ch.width().unwrap_or(0);
        // Flush the current chunk before placing a char that would
        // overshoot the cell budget. Requires `chunk_cells > 0` so
        // that a single char wider than the whole width still lands
        // in one chunk rather than looping forever on an empty one.
        if chunk_cells > 0 && chunk_cells + ch_cells > width {
            chunks.push(&content[chunk_start..idx]);
            chunk_start = idx;
            chunk_cells = 0;
        }
        chunk_cells += ch_cells;
    }
    if chunk_start < content.len() {
        chunks.push(&content[chunk_start..]);
    }
    if chunks.is_empty() {
        chunks.push(content);
    }
    chunks
}

/// v0.5 M2: EOF-no-newline marker (`∅`) span. Drawn at the end of
/// the last visual row of a line whose `has_trailing_newline` is false.
pub(super) fn eof_no_newline_span(bg: Option<Color>) -> Span<'static> {
    let mut style = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    if let Some(b) = bg {
        style = style.bg(b);
    }
    Span::styled("∅", style)
}

/// Take as many leading chars from `s` as fit into `max_cells` display
/// cells, without splitting a wide char. Returns the prefix and the
/// number of cells actually consumed.
pub(super) fn take_cells(s: &str, max_cells: usize) -> (String, usize) {
    use unicode_width::UnicodeWidthChar;
    let mut out = String::new();
    let mut cells = 0usize;
    for ch in s.chars() {
        let w = ch.width().unwrap_or(0);
        if cells + w > max_cells {
            break;
        }
        out.push(ch);
        cells += w;
    }
    (out, cells)
}
