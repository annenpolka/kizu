use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use super::geometry::LineNumberGutter;

/// Diff-view line-number gutter span. Shows the worktree (new-side)
/// line number only:
///
/// - Context -> new (the line exists in the current worktree)
/// - Added   -> new (same)
/// - Deleted -> blank (the line no longer exists in the worktree, so
///   there is no "current" line number to show)
///
/// Earlier revisions mixed `old` and `new` values in the same column
/// so Deleted rows would still render a number. User feedback
/// (2026-04-21) pointed out that when earlier hunks in the same file
/// shift N lines, the old-side baseline number on a Deleted row and
/// the new-side worktree number on an adjacent Added row diverge by N,
/// breaking the intuition that "the gutter tracks the file I'm looking
/// at". Showing only the worktree side keeps the column monotonic.
pub(super) fn diff_ln_span(
    pair: (Option<usize>, Option<usize>),
    gutter: &LineNumberGutter,
) -> Span<'static> {
    let w = gutter.col_width;
    let content = match pair.1 {
        Some(v) => format!(" {v:>w$} "),
        None => " ".repeat(gutter.total_width),
    };
    Span::styled(
        content,
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
    )
}

/// File-view single-column line-number gutter span.
pub(super) fn file_ln_span(line_number: usize, gutter: &LineNumberGutter) -> Span<'static> {
    Span::styled(
        format!(" {line_number:>w$} ", w = gutter.col_width),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
    )
}

/// Insert a line-number gutter after the 5-cell cursor bar on every
/// rendered visual line. The first visual row gets the supplied
/// number span; wrap continuation rows get blanks so the number is
/// not repeated down the screen.
pub(super) fn add_line_number_gutters(
    lines: Vec<Line<'static>>,
    first_gutter: Span<'static>,
    gutter: &LineNumberGutter,
) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .enumerate()
        .map(|(i, line)| {
            let ln = if i == 0 {
                first_gutter.clone()
            } else {
                gutter.blank_span()
            };
            insert_gutter_span(line, ln, Span::raw("     "))
        })
        .collect()
}

/// Shorthand for splicing a blank gutter after the head span of a
/// line that already has its own bar (or no bar at all, for Spacer).
pub(super) fn insert_blank_gutter(line: Line<'static>, gutter: &LineNumberGutter) -> Line<'static> {
    insert_gutter_span(line, gutter.blank_span(), Span::raw(""))
}

/// Splice a blank gutter span after the first `split_at` cells of
/// `line`'s single-span content. Used for BinaryNotice which packs
/// its bar + pad + body into one `Span` literal.
pub(super) fn insert_blank_gutter_at(
    line: Line<'static>,
    gutter: &LineNumberGutter,
    split_at: usize,
) -> Line<'static> {
    let mut spans = line.spans;
    if spans.is_empty() {
        return Line::from(vec![gutter.blank_span()]);
    }
    let first = spans.remove(0);
    let text = first.content.as_ref();
    let (head, tail) = text.split_at(split_at.min(text.len()));
    let head_span = Span::styled(head.to_string(), first.style);
    let tail_span = Span::styled(tail.to_string(), first.style);
    let mut new_spans = Vec::with_capacity(spans.len() + 3);
    new_spans.push(head_span);
    new_spans.push(gutter.blank_span());
    new_spans.push(tail_span);
    new_spans.extend(spans);
    Line::from(new_spans)
}

/// Splice `gutter_span` directly after the first span of `line`
/// (which is the bar: 5-cell for DiffLine / HunkHeader rows,
/// 2-cell for FileHeader rows). Used to keep non-DiffLine row bodies
/// horizontally aligned with DiffLine bodies when the gutter is on.
/// `bar_fallback` is the span to emit when the line has no spans at
/// all; callers pass a matching-width blank bar so downstream
/// measurements stay consistent.
fn insert_gutter_span(
    line: Line<'static>,
    gutter_span: Span<'static>,
    bar_fallback: Span<'static>,
) -> Line<'static> {
    let mut spans = line.spans;
    let bar = if spans.is_empty() {
        bar_fallback
    } else {
        spans.remove(0)
    };
    let mut new_spans = Vec::with_capacity(spans.len() + 2);
    new_spans.push(bar);
    new_spans.push(gutter_span);
    new_spans.extend(spans);
    Line::from(new_spans)
}
