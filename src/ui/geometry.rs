use ratatui::text::Span;

/// Shared per-frame width decisions for diff and file-view renderers.
///
/// Keep body width, wrap width, and line-number gutter suppression in
/// one place so render placement and rendered text do not drift.
#[derive(Debug, Clone, Copy)]
pub(super) struct RenderGeometry {
    pub(super) effective_show_ln: bool,
    pub(super) ln_gutter: LineNumberGutter,
    pub(super) body_width: usize,
    pub(super) wrap_body_width: Option<usize>,
    pub(super) nowrap_body_width: usize,
}

impl RenderGeometry {
    pub(super) fn for_diff(
        viewport_width: usize,
        show_line_numbers: bool,
        stream_mode: bool,
        wrap_lines: bool,
        max_line_number: usize,
    ) -> Self {
        let wants_ln = show_line_numbers && !stream_mode;
        let (effective_show_ln, ln_gutter_width, ln_gutter) =
            resolve_ln_gutter(wants_ln, max_line_number, viewport_width);
        let body_width = body_width_after_gutter(viewport_width, ln_gutter_width);
        Self {
            effective_show_ln,
            ln_gutter,
            body_width,
            wrap_body_width: wrap_lines.then_some(body_width),
            nowrap_body_width: body_width,
        }
    }

    pub(super) fn for_file_view(
        viewport_width: usize,
        show_line_numbers: bool,
        max_line_number: usize,
    ) -> Self {
        let (effective_show_ln, ln_gutter_width, ln_gutter) =
            resolve_ln_gutter(show_line_numbers, max_line_number, viewport_width);
        let body_width = body_width_after_gutter(viewport_width, ln_gutter_width);
        Self {
            effective_show_ln,
            ln_gutter,
            body_width,
            wrap_body_width: Some(body_width),
            nowrap_body_width: body_width,
        }
    }
}

fn body_width_after_gutter(viewport_width: usize, ln_gutter_width: usize) -> usize {
    viewport_width.saturating_sub(5 + ln_gutter_width).max(1)
}

/// Width configuration for the line-number gutter (v0.5).
///
/// Single-column format for both diff view and file view: `" N "` —
/// 1-cell leading pad, right-aligned number column, 1-cell trailing
/// pad. Earlier revisions used a two-column `OLD|NEW` layout, but
/// user feedback (2026-04-21) was that the doubled numbers on every
/// Context row ("13 13", "14 14", ...) looked like a bug. The
/// single column shows only the worktree (new) line number
/// (Context/Added), and Deleted rows get a blank gutter because the
/// line no longer exists in the worktree. Mixing `old` baseline
/// numbers in the same column broke monotonicity when earlier hunks
/// shifted subsequent hunks by N lines.
#[derive(Debug, Clone, Copy)]
pub(crate) struct LineNumberGutter {
    pub total_width: usize,
    pub col_width: usize,
}

impl LineNumberGutter {
    /// Single-column gutter with the given column width.
    pub fn single(col_width: usize) -> Self {
        Self {
            total_width: 1 + col_width + 1,
            col_width,
        }
    }

    /// Return a blank span of the full gutter width. Used for wrap
    /// continuation rows and for non-DiffLine rows (HunkHeader,
    /// BinaryNotice, Spacer) that still need the gutter column
    /// reserved so downstream body rendering lines up.
    pub(super) fn blank_span(&self) -> Span<'static> {
        Span::raw(" ".repeat(self.total_width))
    }
}

/// Compute the line-number gutter width for a given max line number.
/// 10 is the lower bound so tiny files stay at a stable 2 digits.
pub(crate) fn line_number_digits(max: usize) -> usize {
    let mut n = max.max(10);
    let mut digits = 0;
    while n > 0 {
        n /= 10;
        digits += 1;
    }
    digits
}

/// Resolve the effective line-number gutter: apply the
/// extreme-narrow fallback so the caller doesn't have to. Returns
/// `(effective_show_ln, ln_gutter_width, ln_gutter)` where
/// `ln_gutter` uses a zero col-width when the gutter is suppressed
/// so blank-slot helpers keep working without separate nil handling.
pub(super) fn resolve_ln_gutter(
    show_ln: bool,
    max_line_number: usize,
    viewport_width: usize,
) -> (bool, usize, LineNumberGutter) {
    let digits = line_number_digits(max_line_number);
    let raw_width = if show_ln {
        LineNumberGutter::single(digits).total_width
    } else {
        0
    };
    // Fallback: if the gutter would leave < 4 cells of body width,
    // drop it entirely so the user still gets diff content.
    if show_ln && viewport_width >= 5 + raw_width + 4 {
        (true, raw_width, LineNumberGutter::single(digits))
    } else {
        (false, 0, LineNumberGutter::single(0))
    }
}
