use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::git::{DiffContent, FileDiff, Hunk, LineKind};

/// Linear membership probe for [`crate::app::App::seen_hunks`] that takes the
/// path by reference, so the renderer can check every visible hunk
/// header without allocating a `PathBuf` per frame. The map size is
/// bounded by user toggles (typically tens of entries), so the scan
/// is cheaper than the clone it replaces.
///
/// v0.4: the mark is bound to the hunk's content fingerprint (not
/// just the `(path, old_start)` pre-image anchor). A key hit with a
/// mismatched fingerprint means the hunk has been edited since it
/// was marked seen — in that case the mark is considered stale and
/// the hunk behaves as if it were unmarked (auto-expand).
pub fn seen_hunk_fingerprint(
    seen: &BTreeMap<(PathBuf, usize), u64>,
    path: &Path,
    old_start: usize,
) -> Option<u64> {
    seen.iter()
        .find(|((p, o), _)| *o == old_start && p.as_path() == path)
        .map(|(_, fp)| *fp)
}

/// Hash the full `lines` vector of a hunk (kind + content + trailing
/// newline flag) into a single u64 fingerprint. Used by the seen
/// mark to detect content drift between the moment the user pressed
/// Space and the current watcher-driven recompute (v0.4).
///
/// `DefaultHasher` is not guaranteed deterministic across process
/// runs, but the fingerprint only needs to be stable within a single
/// kizu session — the seen set is not persisted to disk.
pub fn hunk_fingerprint(hunk: &Hunk) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut h = DefaultHasher::new();
    for line in &hunk.lines {
        // Stable discriminator for LineKind — the enum itself is
        // not `Hash`, so map to a small byte tag.
        let tag: u8 = match line.kind {
            LineKind::Context => 0,
            LineKind::Added => 1,
            LineKind::Deleted => 2,
        };
        tag.hash(&mut h);
        line.content.hash(&mut h);
        line.has_trailing_newline.hash(&mut h);
    }
    h.finish()
}

/// Two ways the renderer can park the cursor inside the viewport.
/// Defaults to [`CursorPlacement::Centered`]; `z` toggles to
/// [`CursorPlacement::Top`] (the cursor sits at the viewport ceiling
/// and the selected hunk body reads downward from there — the
/// natural direction for diff reading).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorPlacement {
    Centered,
    Top,
}

impl CursorPlacement {
    /// Compute the viewport's top-row index given the cursor's logical
    /// row, the total layout size, and the viewport height. The result
    /// is clamped to `[0, total - height]` so we never reveal phantom
    /// rows past either end of the layout.
    pub fn viewport_top(self, cursor: usize, total: usize, height: usize) -> usize {
        if total <= height {
            return 0;
        }
        let max_top = total - height;
        let raw = match self {
            CursorPlacement::Centered => cursor.saturating_sub(height / 2),
            // Cursor at viewport row 0. The selected hunk flows
            // downward from there into the body.
            CursorPlacement::Top => cursor,
        };
        raw.min(max_top)
    }

    /// Short human label used in the footer indicator.
    pub fn label(self) -> &'static str {
        match self {
            CursorPlacement::Centered => "center",
            CursorPlacement::Top => "top",
        }
    }
}

/// Pre-computed layout for the scroll view. Built once per `recompute_diff`,
/// then sliced into a viewport at render time.
#[derive(Debug, Default, Clone)]
pub struct ScrollLayout {
    /// Every visible row in order.
    pub rows: Vec<RowKind>,
    /// `rows` indices that point at a `HunkHeader` — used by `j/k` to jump
    /// hunk-by-hunk regardless of how many context lines sit in between.
    pub hunk_starts: Vec<usize>,
    /// Per-file, per-hunk `(start, end_exclusive)` spans in `rows`.
    /// Used by viewport anchoring so every render can find the
    /// selected hunk's extent without scanning the whole layout.
    pub hunk_ranges: Vec<Vec<(usize, usize)>>,
    /// Per-file, per-hunk content fingerprints. `None` means no seen
    /// mark existed for that `(path, old_start)` during `build_layout`,
    /// so callers can avoid hashing untouched hunks on hot render paths.
    pub hunk_fingerprints: Vec<Vec<Option<u64>>>,
    /// For each file in `App.files`, the row index of its first hunk header
    /// (or the file header for binaries / empty hunks). `None` only when the
    /// layout build couldn't produce any anchorable row for that file.
    pub file_first_hunk: Vec<Option<usize>>,
    /// `file_of_row[i]` is the index into `App.files` for whichever file row
    /// `i` belongs to. The footer reads this to display the current file.
    pub file_of_row: Vec<usize>,
    /// `(start, end_exclusive)` row spans of every contiguous `+`/`-` block
    /// across the entire layout. `J` / `K` walk these spans in *both*
    /// directions: short runs collapse to a one-press jump, long runs are
    /// walked in [`crate::app::App::chunk_size`]-sized scroll chunks
    /// (= viewport height / 3), and once the cursor passes the end of a run
    /// the next press flows into the next run even when that run lives in a
    /// different file.
    pub change_runs: Vec<(usize, usize)>,
    /// v0.5: parallel Vec to `rows`. For every `RowKind::DiffLine`, the
    /// corresponding slot holds `Some((old_line_number, new_line_number))`
    /// using the per-kind rule Context → both sides, Added → new only,
    /// Deleted → old only. All other row kinds carry `None`.
    /// `build_layout` fills this with a single cumulative walk per
    /// hunk in the same pass that pushes rows, so renderer cost stays
    /// O(viewport) regardless of hunk size. `git::line_numbers_for`
    /// in test builds pins the same semantics as a single-line spec.
    pub diff_line_numbers: Vec<Option<(Option<usize>, Option<usize>)>>,
    /// v0.5: largest line number (either `old` or `new`) among all
    /// **visible** DiffLine rows. Seen (collapsed) hunks are excluded
    /// because their rows are not in `rows`. Clamped to a lower bound
    /// of 10 so the gutter stays at a minimum of 2 digits and doesn't
    /// flicker between 1- and 2-digit widths for tiny files.
    pub max_line_number: usize,
}

/// Per-render map from logical row index → visual y offset, computed
/// against the current wrap body width. Every frame the renderer
/// rebuilds a fresh index (cheap: O(rows) with the 2000-row cap from
/// `SCROLL_ROW_LIMIT`) so scroll math can talk about visual y instead
/// of logical rows.
///
/// The key invariant: `prefix[i]` is the visual y-offset where logical
/// row `i` begins, and `prefix[i+1] - prefix[i]` is the visual height
/// of row `i`. In **nowrap** mode every row is exactly 1 visual row
/// tall, so `prefix` is `[0, 1, 2, …, n]` and `visual_y(row)` is the
/// identity — all existing logical-row tests stay numerically correct.
/// In **wrap** mode, diff lines whose content exceeds `body_width`
/// contribute multiple visual rows and the prefix becomes non-trivial.
///
/// Animation (`ScrollAnim`) and viewport placement operate over this
/// coordinate space, not logical rows. That's the crux of the wrap-mode
/// fix — logical-row scrolling against wrap rendering was pushing the
/// cursor off-screen because a few wrapped rows ahead of the cursor
/// could silently consume the entire viewport before the cursor's
/// logical row was ever emitted.
#[derive(Debug, Clone)]
pub struct VisualIndex {
    /// Cumulative visual y-offsets, length `rows.len() + 1`.
    /// `prefix[rows.len()]` is the total visual height of the layout.
    prefix: Vec<usize>,
    /// Wrap body width this index was built against. `None` means
    /// nowrap, in which case `prefix` is the identity mapping — kept
    /// on the value so downstream code (and tests) can tell at a
    /// glance whether visual and logical coordinates coincide.
    #[allow(dead_code)]
    pub body_width: Option<usize>,
}

impl VisualIndex {
    /// Build a fresh prefix sum against the current layout and the
    /// supplied wrap body width. Pass `None` for nowrap mode; the
    /// resulting index acts as the identity and keeps the legacy
    /// logical-row scroll model intact.
    pub fn build(layout: &ScrollLayout, files: &[FileDiff], body_width: Option<usize>) -> Self {
        Self::from_heights(
            body_width,
            layout
                .rows
                .iter()
                .map(|row| Self::row_visual_height(row, files, body_width)),
        )
    }

    /// Build a visual index for full-file view lines. The prefix
    /// coordinate machinery is identical to the diff layout; only
    /// the per-logical-row height calculation changes.
    pub fn build_lines(lines: &[String], body_width: Option<usize>) -> Self {
        Self::from_heights(
            body_width,
            lines
                .iter()
                .map(|line| Self::line_visual_height(line, body_width)),
        )
    }

    fn from_heights<I>(body_width: Option<usize>, heights: I) -> Self
    where
        I: IntoIterator<Item = usize>,
    {
        let mut prefix = Vec::new();
        prefix.push(0);
        let mut acc = 0usize;
        for h in heights {
            acc += h;
            prefix.push(acc);
        }
        Self { prefix, body_width }
    }

    /// Visual y offset where logical row `row_idx` begins.
    pub fn visual_y(&self, row_idx: usize) -> usize {
        self.prefix.get(row_idx).copied().unwrap_or(0)
    }

    /// Visual-row height of logical row `row_idx`. Falls back to 1
    /// for out-of-range indices so callers don't need to bounds-check.
    pub fn visual_height(&self, row_idx: usize) -> usize {
        match (self.prefix.get(row_idx), self.prefix.get(row_idx + 1)) {
            (Some(&a), Some(&b)) => b - a,
            _ => 1,
        }
    }

    /// Total visual height of the layout.
    pub fn total_visual(&self) -> usize {
        self.prefix.last().copied().unwrap_or(0)
    }

    /// Given a visual y offset, return `(logical_row, skip_within_row)`
    /// where `logical_row` is the logical row that contains y and
    /// `skip_within_row` is how many visual lines of that row sit at
    /// or above y. Used by the renderer to begin drawing mid-row
    /// when wrap pushes the viewport's top into the middle of a
    /// wrapped diff line.
    pub fn logical_at(&self, y: usize) -> (usize, usize) {
        if self.prefix.len() < 2 {
            return (0, 0);
        }
        // Clamp past-the-end to the last row's final visual line.
        let total = self.total_visual();
        if y >= total {
            let last = self.prefix.len() - 2;
            return (last, self.visual_height(last).saturating_sub(1));
        }
        // Binary search: smallest `i` such that prefix[i+1] > y.
        let mut lo = 0usize;
        let mut hi = self.prefix.len() - 1;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.prefix[mid + 1] > y {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        let within = y - self.prefix[lo];
        (lo, within)
    }

    fn row_visual_height(row: &RowKind, files: &[FileDiff], body_width: Option<usize>) -> usize {
        let Some(width) = body_width else {
            return 1;
        };
        let RowKind::DiffLine {
            file_idx,
            hunk_idx,
            line_idx,
        } = row
        else {
            return 1;
        };
        let Some(file) = files.get(*file_idx) else {
            return 1;
        };
        let DiffContent::Text(hunks) = &file.content else {
            return 1;
        };
        let Some(hunk) = hunks.get(*hunk_idx) else {
            return 1;
        };
        let Some(line) = hunk.lines.get(*line_idx) else {
            return 1;
        };
        // Visual row count = ceil(display-width(content) / body_width).
        // CJK / emoji consume 2 cells each, so counting chars would
        // under-estimate the height and ratatui's scroll math would
        // push the cursor off-screen in wrap mode.
        let cells = unicode_width::UnicodeWidthStr::width(line.content.as_str());
        if cells == 0 {
            1
        } else {
            cells.div_ceil(width.max(1))
        }
    }

    fn line_visual_height(line: &str, body_width: Option<usize>) -> usize {
        let Some(width) = body_width else {
            return 1;
        };
        use unicode_width::UnicodeWidthChar;

        if line.is_empty() {
            return 1;
        }

        let mut rows = 1usize;
        let mut chunk_cells = 0usize;
        for ch in line.chars() {
            let ch_cells = ch.width().unwrap_or(0);
            if chunk_cells > 0 && chunk_cells + ch_cells > width {
                rows += 1;
                chunk_cells = 0;
            }
            chunk_cells += ch_cells;
        }
        rows
    }
}

/// One displayable row in the scroll view. The renderer turns each variant
/// into a styled `Line`; the App layer cares about `(file_idx, hunk_idx)`
/// for navigation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowKind {
    /// `path  ── status ── +A/-D ── mtime`
    FileHeader { file_idx: usize },
    /// `@@ -... +... @@`
    HunkHeader { file_idx: usize, hunk_idx: usize },
    /// One ` `/`+`/`-` line within a hunk.
    DiffLine {
        file_idx: usize,
        hunk_idx: usize,
        line_idx: usize,
    },
    /// `[binary file - diff suppressed]`
    BinaryNotice { file_idx: usize },
    /// Visual breathing room between files.
    Spacer,
}

/// Identifies "the hunk the user is looking at" across `recompute_diff`.
/// `hunk_old_start` is enough of a fingerprint to find the same hunk even
/// when neighbouring hunks shift around.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HunkAnchor {
    pub path: PathBuf,
    pub hunk_old_start: usize,
}

pub(crate) fn build_scroll_layout(
    files: &[FileDiff],
    seen_hunks: &BTreeMap<(PathBuf, usize), u64>,
) -> ScrollLayout {
    let mut layout = ScrollLayout {
        file_first_hunk: vec![None; files.len()],
        ..ScrollLayout::default()
    };
    let mut max_line_number: usize = 0;

    for (file_idx, file) in files.iter().enumerate() {
        let mut file_hunk_ranges = Vec::new();
        let mut file_hunk_fingerprints = Vec::new();
        let header_row = layout.rows.len();
        layout.rows.push(RowKind::FileHeader { file_idx });
        layout.diff_line_numbers.push(None);

        match &file.content {
            DiffContent::Binary => {
                let notice_row = layout.rows.len();
                layout.rows.push(RowKind::BinaryNotice { file_idx });
                layout.diff_line_numbers.push(None);
                layout.file_first_hunk[file_idx] = Some(notice_row);
            }
            DiffContent::Text(hunks) => {
                if hunks.is_empty() {
                    // Treat the file header itself as the anchor row when
                    // there are no hunks at all (extremely rare in
                    // practice; happens for empty `git diff` payloads).
                    layout.file_first_hunk[file_idx] = Some(header_row);
                } else {
                    let first_hunk_row = layout.rows.len();
                    layout.file_first_hunk[file_idx] = Some(first_hunk_row);
                    for (hunk_idx, hunk) in hunks.iter().enumerate() {
                        let row = layout.rows.len();
                        layout.rows.push(RowKind::HunkHeader { file_idx, hunk_idx });
                        layout.diff_line_numbers.push(None);
                        layout.hunk_starts.push(row);
                        // v0.4: seen hunks collapse — omit their
                        // DiffLine rows from the layout so only
                        // the hunk header is visible. The mark
                        // auto-clears (below, in the fingerprint
                        // check) once the hunk content drifts.
                        let marked_fp =
                            seen_hunk_fingerprint(seen_hunks, &file.path, hunk.old_start);
                        let current_fp = marked_fp.map(|_| hunk_fingerprint(hunk));
                        file_hunk_fingerprints.push(current_fp);
                        let is_seen = matches!(
                            (marked_fp, current_fp),
                            (Some(marked), Some(current)) if marked == current
                        );
                        if !is_seen {
                            // v0.5: inline the old/new counter walk
                            // so the whole hunk is O(n). Calling
                            // `line_numbers_for` per line is
                            // O(line_idx) each, which compounds to
                            // O(n²) for large hunks (Codex 3rd-round
                            // Important-4).
                            let mut old = hunk.old_start;
                            let mut new = hunk.new_start;
                            for (line_idx, line) in hunk.lines.iter().enumerate() {
                                layout.rows.push(RowKind::DiffLine {
                                    file_idx,
                                    hunk_idx,
                                    line_idx,
                                });
                                let pair = match line.kind {
                                    LineKind::Context => {
                                        let p = (Some(old), Some(new));
                                        old += 1;
                                        new += 1;
                                        p
                                    }
                                    LineKind::Added => {
                                        let p = (None, Some(new));
                                        new += 1;
                                        p
                                    }
                                    LineKind::Deleted => {
                                        let p = (Some(old), None);
                                        old += 1;
                                        p
                                    }
                                };
                                if let Some(n) = pair.0
                                    && n > max_line_number
                                {
                                    max_line_number = n;
                                }
                                if let Some(n) = pair.1
                                    && n > max_line_number
                                {
                                    max_line_number = n;
                                }
                                layout.diff_line_numbers.push(Some(pair));
                            }
                        }
                        file_hunk_ranges.push((row, layout.rows.len()));
                    }
                }
            }
        }

        layout.hunk_ranges.push(file_hunk_ranges);
        layout.hunk_fingerprints.push(file_hunk_fingerprints);
        layout.rows.push(RowKind::Spacer);
        layout.diff_line_numbers.push(None);
    }

    // Lower bound of 10 keeps the gutter at a stable minimum of
    // 2 digits so tiny files don't flicker between 1- and 2-digit
    // widths as hunks get added.
    layout.max_line_number = max_line_number.max(10);

    // Fill the file_of_row map by walking rows once.
    layout.file_of_row = layout
        .rows
        .iter()
        .scan(0usize, |last_file, row| {
            let f = match row {
                RowKind::FileHeader { file_idx } => *file_idx,
                RowKind::HunkHeader { file_idx, .. } => *file_idx,
                RowKind::DiffLine { file_idx, .. } => *file_idx,
                RowKind::BinaryNotice { file_idx } => *file_idx,
                RowKind::Spacer => *last_file,
            };
            *last_file = f;
            Some(f)
        })
        .collect();

    // Detect change-run spans: a "change run" is a maximal contiguous
    // range of `+`/`-` DiffLine rows. We record `(start, end_exclusive)`
    // pairs; `J`/`K` use these to know when they are *inside* a run
    // (and should scroll within it) versus *between* runs (and should
    // jump to the next/prev run).
    let mut current_run_start: Option<usize> = None;
    for (row_idx, row) in layout.rows.iter().enumerate() {
        let is_change = match row {
            RowKind::DiffLine {
                file_idx,
                hunk_idx,
                line_idx,
            } => match &files[*file_idx].content {
                DiffContent::Text(hunks) => {
                    hunks[*hunk_idx].lines[*line_idx].kind != LineKind::Context
                }
                DiffContent::Binary => false,
            },
            _ => false,
        };
        match (is_change, current_run_start) {
            (true, None) => {
                current_run_start = Some(row_idx);
            }
            (false, Some(start)) => {
                layout.change_runs.push((start, row_idx));
                current_run_start = None;
            }
            _ => {}
        }
    }
    if let Some(start) = current_run_start {
        layout.change_runs.push((start, layout.rows.len()));
    }

    layout
}
