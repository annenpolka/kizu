use std::path::Path;
use std::time::Instant;

use crate::git::DiffContent;

use super::{
    App, CachedVisualIndex, CursorPlacement, HunkAnchor, RowKind, SCROLL_ANIM_DURATION, ScrollAnim,
    VisualIndex,
};

/// Threshold for treating a change run as "long" in adaptive motion.
/// Long runs get walked chunk-by-chunk so their body isn't teleported
/// past; short runs are atomic landings (one press per run). A run is
/// long when its visual height exceeds this fraction of the viewport —
/// so a run filling 80% of the screen or more is considered long.
const LONG_RUN_RATIO: f32 = 0.8;

/// v0.4 adaptive-navigation helper: given the nearest "run start"
/// and "hunk header" row strictly after the cursor, return the row
/// that `j` should land on — whichever is closer. Both inputs are
/// already row indices that satisfy `> cursor`.
fn nearest_landing_forward(next_run: Option<usize>, next_hh: Option<usize>) -> Option<usize> {
    match (next_run, next_hh) {
        (Some(r), Some(h)) => Some(r.min(h)),
        (Some(r), None) => Some(r),
        (None, Some(h)) => Some(h),
        (None, None) => None,
    }
}

/// Backward mirror of [`nearest_landing_forward`]. Row indices here
/// satisfy `< cursor`, so the closer one is the *larger* index.
fn nearest_landing_backward(prev_run: Option<usize>, prev_hh: Option<usize>) -> Option<usize> {
    match (prev_run, prev_hh) {
        (Some(r), Some(h)) => Some(r.max(h)),
        (Some(r), None) => Some(r),
        (None, Some(h)) => Some(h),
        (None, None) => None,
    }
}

fn next_sorted_after(values: &[usize], cursor: usize) -> Option<usize> {
    values
        .get(values.partition_point(|&value| value <= cursor))
        .copied()
}

fn prev_sorted_before(values: &[usize], cursor: usize) -> Option<usize> {
    values
        .partition_point(|&value| value < cursor)
        .checked_sub(1)
        .and_then(|idx| values.get(idx).copied())
}

fn change_run_at(runs: &[(usize, usize)], cursor: usize) -> Option<(usize, usize)> {
    let idx = runs.partition_point(|&(_, end)| end <= cursor);
    runs.get(idx)
        .copied()
        .filter(|(start, end)| *start <= cursor && cursor < *end)
}

fn next_change_run_start_after(runs: &[(usize, usize)], cursor: usize) -> Option<usize> {
    runs.get(runs.partition_point(|&(start, _)| start <= cursor))
        .map(|(start, _)| *start)
}

fn prev_change_run_start_before(runs: &[(usize, usize)], cursor: usize) -> Option<usize> {
    runs.partition_point(|&(start, _)| start < cursor)
        .checked_sub(1)
        .and_then(|idx| runs.get(idx).map(|(start, _)| *start))
}

fn is_long_run(run_visual: usize, viewport: usize) -> bool {
    run_visual as f32 > viewport as f32 * LONG_RUN_RATIO
}

impl App {
    pub fn scroll_by(&mut self, delta: isize) {
        let last = self.last_row_index();
        let body_width = self.last_body_width.get();
        if body_width.is_none() {
            // Nowrap fast path: one logical row == one visual row,
            // sub-row is always 0. Delegate to `scroll_to` which
            // resets `cursor_sub_row` unconditionally.
            let next = (self.scroll as isize + delta).clamp(0, last as isize) as usize;
            let next = self.normalize_row_target(next, delta.signum());
            self.scroll_to(next);
            return;
        }
        // Wrap mode: `delta` is interpreted as **visual rows** and
        // the cursor's position is the sum of its logical-row visual
        // y and its intra-row `cursor_sub_row`. ADR-0009 fix: the
        // previous implementation discarded the intra-row offset
        // returned by `VisualIndex::logical_at`, so Ctrl-d inside a
        // single long wrapped line stayed pinned to the same logical
        // row — `scroll_to(row)` treated the move as a no-op and
        // the user could never walk through a minified JSON edit.
        //
        // The fix routes wrap-mode navigation through `scroll_to_visual`
        // which preserves the sub-row offset so the visual cursor
        // genuinely advances.
        let vi = VisualIndex::build(&self.layout, &self.files, body_width);
        let cur_y = vi.visual_y(self.scroll) + self.cursor_sub_row;
        let new_y = (cur_y as isize + delta).max(0) as usize;
        let clamped = new_y.min(vi.total_visual().saturating_sub(1));
        let (target_row, target_sub) = vi.logical_at(clamped);
        let target_row = self.normalize_row_target(target_row.min(last), delta.signum());
        self.scroll_to_visual(target_row, target_sub, &vi);
    }

    /// Wrap-aware cursor move that preserves an intra-row visual
    /// offset. Nowrap callers must keep going through [`Self::scroll_to`]
    /// because they have no `VisualIndex` to clamp against and would
    /// just set `cursor_sub_row` to 0 anyway.
    ///
    /// Behaves like [`Self::scroll_to`] for the row side: starts a
    /// fresh animation when either the logical row or the sub-row
    /// actually changes, and updates the anchor. `sub_row` is
    /// clamped to the target row's visual height so callers can
    /// pass a speculative value without risking an out-of-range
    /// cursor.
    pub(crate) fn scroll_to_visual(&mut self, row: usize, sub_row: usize, vi: &VisualIndex) {
        // Any explicit cursor move drops the watcher-set pin so
        // subsequent renders fall back to normal placement.
        self.pinned_cursor_y = None;
        let last = self.last_row_index();
        let target_row = row.min(last);
        let row_height = vi.visual_height(target_row).max(1);
        let clamped_sub = sub_row.min(row_height - 1);
        if (target_row, clamped_sub) != (self.scroll, self.cursor_sub_row) {
            self.anim = Some(ScrollAnim {
                from: self.visual_top.get(),
                start: Instant::now(),
                dur: SCROLL_ANIM_DURATION,
            });
        }
        self.scroll = target_row;
        self.cursor_sub_row = clamped_sub;
        self.update_anchor_from_scroll();
    }

    /// Animated scroll: move the cursor row to `row` and kick off a
    /// viewport-top tween from the currently drawn visual position.
    /// No animation is started when `row` is already the cursor row
    /// (a no-op), which keeps idle frames free of needless ticks.
    ///
    /// Also resets `cursor_sub_row` to 0 — every caller of
    /// `scroll_to` is a "jump to a specific row" operation (next
    /// hunk, previous hunk, g, G, follow, picker jump, anchor
    /// restore) and those should all land on the first visual line of
    /// the destination logical row. Wrap-mode **intra-row** walks go
    /// through [`Self::scroll_to_visual`] instead.
    pub fn scroll_to(&mut self, row: usize) {
        // Any explicit cursor move drops the watcher-set pin so
        // subsequent renders fall back to normal placement.
        self.pinned_cursor_y = None;
        let last = self.last_row_index();
        let target = self.normalize_row_target(row.min(last), 1);
        if target != self.scroll || self.cursor_sub_row != 0 {
            self.anim = Some(ScrollAnim {
                from: self.visual_top.get(),
                start: Instant::now(),
                dur: SCROLL_ANIM_DURATION,
            });
        }
        self.scroll = target;
        self.cursor_sub_row = 0;
        self.update_anchor_from_scroll();
    }

    fn normalize_row_target(&self, row: usize, preferred_direction: isize) -> usize {
        if !matches!(self.layout.rows.get(row), Some(RowKind::Spacer)) {
            return row;
        }

        if preferred_direction >= 0
            && let Some(next) = self
                .layout
                .rows
                .iter()
                .enumerate()
                .skip(row + 1)
                .find_map(|(idx, r)| (!matches!(r, RowKind::Spacer)).then_some(idx))
        {
            return next;
        }
        if let Some(prev) = self.layout.rows[..row]
            .iter()
            .rposition(|r| !matches!(r, RowKind::Spacer))
        {
            return prev;
        }
        row
    }

    /// Mark the active animation as finished if enough time has passed.
    /// Returns `true` while an animation is still running, `false` once
    /// the run loop can stop scheduling frame ticks. Pure (`&mut self`
    /// only for the clear side-effect) so tests can inject `now`.
    pub fn tick_anim(&mut self, now: Instant) -> bool {
        let Some(anim) = self.anim else {
            return false;
        };
        let done = now.saturating_duration_since(anim.start) >= anim.dur;
        if done {
            self.anim = None;
            false
        } else {
            true
        }
    }

    /// Animated variant of [`Self::viewport_top`]: feeds the logical
    /// target through the active [`ScrollAnim`], sampling at `now`.
    /// Stores the result in [`Self::visual_top`] so the next animation
    /// kick-off starts from the exact row the last frame drew.
    ///
    /// This is the **nowrap** helper: it operates purely in logical
    /// row units and is retained for the existing centering/hunk-anchor
    /// tests plus nowrap renders. Wrap renders go through
    /// [`Self::viewport_placement`] instead, which speaks visual y.
    pub fn visual_viewport_top(&self, viewport_height: usize, now: Instant) -> usize {
        let target = self.viewport_top(viewport_height) as f32;
        let visual = match self.anim.as_ref() {
            Some(anim) => anim.sample(target, now).0,
            None => target,
        };
        self.visual_top.set(visual);
        visual.round().max(0.0) as usize
    }

    fn with_visual_index<R>(
        &self,
        body_width: Option<usize>,
        f: impl FnOnce(&VisualIndex) -> R,
    ) -> R {
        let needs_rebuild = !matches!(
            self.visual_index_cache.borrow().as_ref(),
            Some(cached) if cached.body_width == body_width
        );
        if needs_rebuild {
            let index = VisualIndex::build(&self.layout, &self.files, body_width);
            *self.visual_index_cache.borrow_mut() = Some(CachedVisualIndex { body_width, index });
        }
        let cache = self.visual_index_cache.borrow();
        let index = &cache.as_ref().expect("visual index cache populated").index;
        f(index)
    }

    fn run_visual_height(
        &self,
        run_start: usize,
        run_end: usize,
        body_width: Option<usize>,
    ) -> usize {
        match body_width {
            None => run_end.saturating_sub(run_start),
            Some(_) => self.with_visual_index(body_width, |vi| {
                vi.visual_y(run_end).saturating_sub(vi.visual_y(run_start))
            }),
        }
    }

    /// Compute the viewport's top position for the current render,
    /// returning `(top_row, skip_visual)` where `top_row` is the first
    /// logical layout row to draw and `skip_visual` is the number of
    /// visual lines of `top_row` the renderer should discard off the
    /// top so that the cursor lands at its desired placement target.
    ///
    /// In **nowrap** mode every logical row is one visual row tall,
    /// so the result is always `(visual_viewport_top(h), 0)` — the
    /// legacy scroll model is preserved byte-for-byte. In **wrap**
    /// mode `viewport_placement` converts the hunk-anchored placement
    /// logic from logical-row space into visual-row space via
    /// [`VisualIndex`]; the cursor's first visual row always lands at
    /// the centre-of-viewport (or viewport ceiling under `Top`
    /// placement), regardless of how much the preceding diff content
    /// wraps. Animation is preserved across the transition: the tween
    /// runs in visual y, which in nowrap collapses to logical rows
    /// and matches the pre-rework behaviour numerically.
    pub fn viewport_placement(
        &self,
        viewport_height: usize,
        body_width: Option<usize>,
        now: Instant,
    ) -> (usize, usize) {
        let Some(_width) = body_width else {
            // Nowrap fast path — identical to the old visual_viewport_top.
            return (self.visual_viewport_top(viewport_height, now), 0);
        };
        self.with_visual_index(body_width, |vi| {
            let target_y = self.placement_target_visual_y(viewport_height, vi);
            let sampled_y = match self.anim.as_ref() {
                Some(anim) => anim.sample(target_y as f32, now).0,
                None => target_y as f32,
            };
            self.visual_top.set(sampled_y);
            let y = sampled_y.round().max(0.0) as usize;
            vi.logical_at(y)
        })
    }

    /// Visual-y coordinate of the viewport's top edge under wrap mode,
    /// chosen so that the cursor (or its enclosing hunk) lands at the
    /// current [`CursorPlacement`]'s preferred target. Mirrors the
    /// nowrap [`Self::viewport_top`] hunk-anchoring logic, but in
    /// visual-row units.
    fn placement_target_visual_y(&self, viewport_height: usize, vi: &VisualIndex) -> usize {
        let total_visual = vi.total_visual();
        if total_visual <= viewport_height {
            return 0;
        }
        let max_top_y = total_visual - viewport_height;

        // Viewport pin: matches nowrap `viewport_top`. When a watcher-
        // driven recompute relocates the cursor, hold the cursor's
        // visual y at the pre-recompute screen row so the viewport
        // doesn't slide.
        if let Some(pinned_y) = self.pinned_cursor_y {
            let cursor_y = vi.visual_y(self.scroll) + self.cursor_sub_row;
            return cursor_y.saturating_sub(pinned_y).min(max_top_y);
        }

        // Hunk-fits-in-viewport case: anchor the entire hunk at the
        // placement target so the user always sees the full selected
        // change as a single block, matching nowrap behaviour.
        if let Some((hunk_top, hunk_end)) = self.current_hunk_range() {
            let hunk_visual = vi.visual_y(hunk_end).saturating_sub(vi.visual_y(hunk_top));
            if hunk_visual <= viewport_height {
                let hunk_top_y = vi.visual_y(hunk_top);
                let desired = match self.cursor_placement {
                    CursorPlacement::Centered => {
                        let pad = (viewport_height - hunk_visual) / 2;
                        hunk_top_y.saturating_sub(pad)
                    }
                    CursorPlacement::Top => hunk_top_y,
                };
                return desired.min(max_top_y);
            }
        }

        // Long-hunk / non-hunk fallback: place the cursor at the
        // placement target, measured in visual y. ADR-0009 fix:
        // include `cursor_sub_row` so intra-row walks through a
        // wrapped diff line actually move the viewport instead of
        // parking it at the logical row's first visual line.
        let cursor_y = vi.visual_y(self.scroll) + self.cursor_sub_row;
        let desired = match self.cursor_placement {
            CursorPlacement::Centered => {
                // Keep the cursor's current visual row at mid-viewport.
                // `cursor_sub_row` is already the intra-row offset, so
                // the 1-row cursor height is the right subtraction
                // here — wrap-continuation lines below the cursor
                // are drawn by the renderer.
                cursor_y.saturating_sub(viewport_height.saturating_sub(1) / 2)
            }
            CursorPlacement::Top => cursor_y,
        };
        desired.min(max_top_y)
    }

    pub(crate) fn last_row_index(&self) -> usize {
        self.layout
            .rows
            .iter()
            .rposition(|row| !matches!(row, RowKind::Spacer))
            .unwrap_or(0)
    }

    pub fn next_hunk(&mut self) {
        // Only advance when there is actually a hunk after the cursor.
        // The previous fallback to `hunk_starts.last()` caused `j` to
        // jump **backward** whenever the cursor sat past the final hunk
        // header (e.g. on the last diff line of a long hunk), which is
        // the opposite of what "next" should mean.
        if let Some(row) = next_sorted_after(&self.layout.hunk_starts, self.scroll) {
            self.scroll_to(row);
        }
    }

    pub fn prev_hunk(&mut self) {
        if let Some(row) = prev_sorted_before(&self.layout.hunk_starts, self.scroll) {
            self.scroll_to(row);
        } else if let Some(&row) = self.layout.hunk_starts.first() {
            self.scroll_to(row);
        }
    }

    /// `j` — adaptive forward motion. Only lands on **reviewable
    /// rows**: change-run starts, long-run bodies (chunk-walked), or
    /// the next hunk's header (via [`Self::next_hunk`]). Never parks
    /// the cursor mid-context.
    ///
    /// Decision ladder:
    ///
    /// 1. **Inside a long run, not at its last row** — chunk-scroll
    ///    forward by [`Self::chunk_size`], clamped to the run's last
    ///    row. A 30-line Added block gets walked top-to-bottom in a
    ///    few presses instead of teleporting past its body.
    /// 2. **Next run exists within this hunk** — land on its start.
    ///    (The jump is visually tweened by [`ScrollAnim`] so the
    ///    motion feels gradual even though the cursor doesn't stop
    ///    on any intermediate context rows.)
    /// 3. **No more runs in this hunk** — hand off to
    ///    [`Self::next_hunk`].
    ///
    /// A change run is a maximal stretch of non-`Context` rows — `-`
    /// and `+` lines share a run because a typical one-line edit
    /// (`-old\n+new`) is reviewed as a single unit.
    pub fn next_change(&mut self) {
        let cursor = self.scroll;
        let viewport = self.last_body_height.get().max(1);
        let body_width = self.last_body_width.get();

        // Inside a long run, not at its last row → chunk forward
        // within the run so its body isn't teleported past.
        if let Some((run_start, run_end)) = change_run_at(&self.layout.change_runs, cursor) {
            let run_visual = self.run_visual_height(run_start, run_end, body_width);
            if is_long_run(run_visual, viewport) && cursor + 1 < run_end {
                let last_row = run_end.saturating_sub(1);
                let target = (cursor + self.chunk_size()).min(last_row);
                self.scroll_to(target);
                return;
            }
        }

        // v0.4: landing candidates are `{HunkHeader rows} ∪
        // {change-run starts}`. Step to whichever comes first after
        // the cursor. This is what makes `j` stop on both hunk
        // headers *and* run starts, matching how a reader walks
        // through a diff (header = "here comes a hunk", run =
        // "here's a change inside it").
        let next_run = next_change_run_start_after(&self.layout.change_runs, cursor);
        let next_hh = next_sorted_after(&self.layout.hunk_starts, cursor);
        if let Some(target) = nearest_landing_forward(next_run, next_hh) {
            self.scroll_to(target);
        }
    }

    /// `k` — adaptive backward motion. Mirror of [`Self::next_change`].
    /// Only lands on change-run starts, long-run bodies, or the prev
    /// hunk's last run start (via [`Self::prev_hunk_last_run_start`]).
    pub fn prev_change(&mut self) {
        let cursor = self.scroll;
        let viewport = self.last_body_height.get().max(1);
        let body_width = self.last_body_width.get();

        // Inside a long run, not at its start → chunk back within.
        if let Some((run_start, run_end)) = change_run_at(&self.layout.change_runs, cursor) {
            let run_visual = self.run_visual_height(run_start, run_end, body_width);
            if is_long_run(run_visual, viewport) && cursor > run_start {
                let target = cursor.saturating_sub(self.chunk_size()).max(run_start);
                self.scroll_to(target);
                return;
            }
        }

        // v0.4: landing candidates are `{HunkHeader rows} ∪
        // {change-run starts}`. Mirror of [`Self::next_change`].
        let prev_run = prev_change_run_start_before(&self.layout.change_runs, cursor);
        let prev_hh = prev_sorted_before(&self.layout.hunk_starts, cursor);
        if let Some(target) = nearest_landing_backward(prev_run, prev_hh) {
            self.scroll_to(target);
        }
    }

    pub fn jump_to_file_first_hunk(&mut self, file_idx: usize) {
        if let Some(Some(row)) = self.layout.file_first_hunk.get(file_idx).copied() {
            self.scroll_to(row);
        }
    }

    pub fn follow_restore(&mut self) {
        self.follow_mode = true;
        if let Some(idx) = self.follow_target_row() {
            self.scroll_to(idx);
        }
    }

    /// Row that "follow mode" parks the scroll cursor on: the
    /// **last hunk header** of the newest file (files are sorted
    /// mtime-ascending, so the last file is the most recently
    /// touched one). Landing on the last @@ header lets the user
    /// see the most recent change's context and diff body below.
    fn follow_target_row(&self) -> Option<usize> {
        if self.files.is_empty() {
            return None;
        }
        let newest = self.files.len() - 1;
        if let Some((row, _)) = self
            .layout
            .hunk_ranges
            .get(newest)
            .and_then(|ranges| ranges.last())
            .copied()
        {
            return Some(row);
        }
        if let Some(row) = self.layout.file_first_hunk.get(newest).copied().flatten() {
            return Some(row);
        }
        self.layout.rows.len().checked_sub(1)
    }

    /// File index that the row at `self.scroll` belongs to.
    pub fn current_file_idx(&self) -> Option<usize> {
        self.layout.file_of_row.get(self.scroll).copied()
    }

    pub fn current_file_path(&self) -> Option<&Path> {
        self.current_file_idx()
            .and_then(|i| self.files.get(i))
            .map(|f| f.path.as_path())
    }

    /// `(file_idx, hunk_idx)` of the hunk the cursor is currently inside,
    /// or `None` when scroll is parked on a non-hunk row (file header,
    /// binary notice, spacer). The renderer uses this to pick the bright
    /// style for selected hunk rows and DIM for everyone else.
    pub fn current_hunk(&self) -> Option<(usize, usize)> {
        match self.layout.rows.get(self.scroll)? {
            RowKind::HunkHeader { file_idx, hunk_idx } => Some((*file_idx, *hunk_idx)),
            RowKind::DiffLine {
                file_idx, hunk_idx, ..
            } => Some((*file_idx, *hunk_idx)),
            _ => None,
        }
    }

    /// Slide `scroll` to the row of `self.anchor` in the new layout.
    /// In follow mode (or when the anchor is gone) re-anchor to the
    /// follow target instead.
    pub(crate) fn refresh_anchor(&mut self) {
        if self.layout.rows.is_empty() {
            self.scroll = 0;
            self.anchor = None;
            return;
        }

        if !self.follow_mode {
            if let Some(anchor) = self.anchor.clone() {
                if let Some(row) = self.find_anchor_row(&anchor) {
                    self.scroll = row;
                    return;
                }
                if let Some(row) = self.find_anchor_file_row(&anchor) {
                    self.scroll = row;
                    self.update_anchor_from_scroll();
                    return;
                }
            }
            // Manual mode should preserve the user's approximate viewport
            // position even when the anchored file disappeared entirely.
            // Falling back to follow mode here would silently violate the
            // "manual" contract and snap to the newest file.
            self.scroll = self.scroll.min(self.last_row_index());
            self.update_anchor_from_scroll();
            return;
        }

        // Follow-mode (or anchor missing): jump to the follow target.
        if let Some(target) = self.follow_target_row() {
            self.scroll = target;
        } else {
            self.scroll = 0;
        }
        self.update_anchor_from_scroll();
    }

    fn find_anchor_row(&self, anchor: &HunkAnchor) -> Option<usize> {
        // Find the file index for the anchor path.
        let file_idx = self.files.iter().position(|f| f.path == anchor.path)?;

        // Walk the file's hunks to find one whose old_start matches.
        match &self.files[file_idx].content {
            DiffContent::Text(hunks) => {
                let target_hunk = hunks
                    .iter()
                    .position(|h| h.old_start == anchor.hunk_old_start)?;
                // Now walk the layout to find the matching HunkHeader row.
                self.layout.rows.iter().position(|row| {
                    matches!(
                        row,
                        RowKind::HunkHeader { file_idx: f, hunk_idx } if *f == file_idx && *hunk_idx == target_hunk
                    )
                })
            }
            DiffContent::Binary => self.layout.file_first_hunk.get(file_idx).copied().flatten(),
        }
    }

    fn find_anchor_file_row(&self, anchor: &HunkAnchor) -> Option<usize> {
        let file_idx = self.files.iter().position(|f| f.path == anchor.path)?;
        match &self.files[file_idx].content {
            DiffContent::Text(hunks) if !hunks.is_empty() => {
                let nearest_hunk = hunks
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, hunk)| hunk.old_start.abs_diff(anchor.hunk_old_start))
                    .map(|(idx, _)| idx)?;
                self.layout.rows.iter().position(|row| {
                    matches!(
                        row,
                        RowKind::HunkHeader { file_idx: f, hunk_idx } if *f == file_idx && *hunk_idx == nearest_hunk
                    )
                })
            }
            _ => self
                .layout
                .file_first_hunk
                .get(file_idx)
                .copied()
                .flatten()
                .or_else(|| {
                    self.layout.rows.iter().position(|row| {
                        matches!(
                            row,
                            RowKind::FileHeader { file_idx: f } if *f == file_idx
                        )
                    })
                }),
        }
    }

    pub(crate) fn update_anchor_from_scroll(&mut self) {
        let Some(row) = self.layout.rows.get(self.scroll) else {
            self.anchor = None;
            return;
        };
        let (file_idx, hunk_idx) = match row {
            RowKind::HunkHeader { file_idx, hunk_idx } => (*file_idx, Some(*hunk_idx)),
            RowKind::DiffLine {
                file_idx, hunk_idx, ..
            } => (*file_idx, Some(*hunk_idx)),
            RowKind::BinaryNotice { file_idx } | RowKind::FileHeader { file_idx } => {
                (*file_idx, None)
            }
            RowKind::Spacer => {
                if let Some(file_idx) = self.layout.file_of_row.get(self.scroll).copied() {
                    (file_idx, None)
                } else {
                    self.anchor = None;
                    return;
                }
            }
        };

        let path = self.files.get(file_idx).map(|f| f.path.clone());
        match (path, hunk_idx) {
            (Some(path), Some(hunk_idx)) => {
                if let Some(file) = self.files.get(file_idx)
                    && let DiffContent::Text(hunks) = &file.content
                    && let Some(hunk) = hunks.get(hunk_idx)
                {
                    self.anchor = Some(HunkAnchor {
                        path,
                        hunk_old_start: hunk.old_start,
                    });
                }
            }
            (Some(path), None) => {
                self.anchor = Some(HunkAnchor {
                    path,
                    hunk_old_start: 0,
                });
            }
            (None, _) => self.anchor = None,
        }
    }
}
