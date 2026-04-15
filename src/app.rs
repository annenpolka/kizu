use anyhow::{Context, Result};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::git::{self, DiffContent, FileDiff, LineKind};
use crate::watcher::{self, WatchEvent};

/// Half-page scroll constant for `Ctrl-d` / `Ctrl-u`. M5+ may swap this for
/// the real viewport height once we plumb it through; until then a fixed
/// value keeps `handle_key` testable as a pure function.
const HALF_PAGE: usize = 12;

/// Top-level application state. Single-threaded, mutated by the event loop
/// via `&mut self` (we run on tokio's `current_thread` flavor — see ADR-0003).
pub struct App {
    pub root: PathBuf,
    pub git_dir: PathBuf,
    pub baseline_sha: String,
    /// Files in the diff, sorted by `mtime` descending. Index 0 is the
    /// most-recently-modified file.
    pub files: Vec<FileDiff>,
    /// Derived flat row plan for the scroll view. Rebuilt whenever `files`
    /// changes via `build_layout`.
    pub layout: ScrollLayout,
    /// The cursor's row index inside `layout.rows`. The renderer derives
    /// the actual viewport top from this + [`Self::cursor_placement`].
    pub scroll: usize,
    /// Where the cursor sits visually inside the viewport. Toggled by `z`.
    pub cursor_placement: CursorPlacement,
    /// Path-tracked anchor: which `(path, hunk_old_start)` the user is
    /// looking at. Lets `recompute_diff` slide `scroll` to the same hunk
    /// even when the row count has shifted.
    pub anchor: Option<HunkAnchor>,
    /// Modal file picker. `Some` when the user has pressed Space.
    pub picker: Option<PickerState>,
    pub follow_mode: bool,
    /// Set when the most recent `compute_diff` failed. Cleared on success.
    pub last_error: Option<String>,
    /// Set whenever HEAD/refs move; the user must press `R` to re-baseline.
    pub head_dirty: bool,
    pub should_quit: bool,
    /// Last viewport height (in rows) the renderer used. Updated through
    /// interior mutability so the next `J`/`K` press can size its scroll
    /// chunk relative to the current screen — bigger window, bigger jumps.
    /// Defaults to 24 before the first render.
    pub last_body_height: Cell<usize>,
}

/// Two ways the renderer can park the cursor inside the viewport.
/// Defaults to [`CursorPlacement::Centered`]; `z` toggles to
/// [`CursorPlacement::Bottom`] (a `tail -f`-flavoured layout where new
/// hunks scroll up from the floor).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorPlacement {
    Centered,
    Bottom,
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
            CursorPlacement::Bottom => cursor.saturating_sub(height.saturating_sub(1)),
        };
        raw.min(max_top)
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
    /// walked in [`App::chunk_size`]-sized scroll chunks (= viewport
    /// height / 3), and once the cursor passes the end of a run the next
    /// press flows into the next run even when that run lives in a
    /// different file.
    pub change_runs: Vec<(usize, usize)>,
}

/// Default body height assumed before the first render has had a chance
/// to update [`App::last_body_height`]. 24 is the classic VT100 height.
const DEFAULT_BODY_HEIGHT: usize = 24;

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

/// Modal file picker state. `cursor` indexes into `picker_results()`, not
/// into `App.files` directly.
#[derive(Debug, Clone, Default)]
pub struct PickerState {
    pub query: String,
    pub cursor: usize,
}

impl App {
    /// Construct an `App` for `root`. Resolves git layout, loads the initial
    /// diff, and parks the scroll cursor on the most-recently-modified hunk.
    pub fn bootstrap(root: PathBuf) -> Result<Self> {
        let root = git::find_root(&root).context("resolving worktree root")?;
        let git_dir = git::git_dir(&root).context("resolving git directory")?;
        let baseline_sha = git::head_sha(&root).context("capturing baseline HEAD")?;

        let mut app = Self {
            root,
            git_dir,
            baseline_sha,
            files: Vec::new(),
            layout: ScrollLayout::default(),
            scroll: 0,
            cursor_placement: CursorPlacement::Centered,
            anchor: None,
            picker: None,
            follow_mode: true,
            last_error: None,
            head_dirty: false,
            should_quit: false,
            last_body_height: Cell::new(DEFAULT_BODY_HEIGHT),
        };
        app.recompute_diff();
        Ok(app)
    }

    /// Half-page-ish chunk size used by `J`/`K` when scrolling within a
    /// long change run. Scales with the actual viewport so a 12-row pane
    /// gets 4-row chunks and a 36-row pane gets 12-row chunks. Always at
    /// least 1 so the cursor still moves on tiny terminals.
    pub fn chunk_size(&self) -> usize {
        (self.last_body_height.get() / 3).max(1)
    }

    /// Toggle the cursor placement between centered and bottom-pinned.
    /// `z` calls this — same vibe as `vim`'s `zz` (centre on cursor).
    pub fn toggle_cursor_placement(&mut self) {
        self.cursor_placement = match self.cursor_placement {
            CursorPlacement::Centered => CursorPlacement::Bottom,
            CursorPlacement::Bottom => CursorPlacement::Centered,
        };
    }

    /// Re-run `git diff`, populate per-file mtimes, sort files by mtime
    /// **ascending** (oldest first → newest last), rebuild the row layout,
    /// and restore the anchor. The ascending order is intentional so that
    /// the scroll view reads top-to-bottom in chronological order, like
    /// `tail -f`: the newest hunk lives at the bottom and follow mode is
    /// "pinned to the floor". On failure, record the error in `last_error`
    /// and keep the previous `files` snapshot intact.
    pub fn recompute_diff(&mut self) {
        match git::compute_diff(&self.root, &self.baseline_sha) {
            Ok(mut files) => {
                self.populate_mtimes(&mut files);
                files.sort_by(|a, b| a.mtime.cmp(&b.mtime));
                self.last_error = None;
                self.files = files;
                self.build_layout();
                self.refresh_anchor();
            }
            Err(e) => {
                self.last_error = Some(format!("{e:#}"));
                // self.files / self.layout intentionally untouched.
            }
        }
    }

    /// Re-capture HEAD as the new baseline (R key).
    pub fn reset_baseline(&mut self) {
        match git::head_sha(&self.root) {
            Ok(sha) => {
                self.baseline_sha = sha;
                self.head_dirty = false;
                self.recompute_diff();
            }
            Err(e) => {
                self.last_error = Some(format!("R: {e:#}"));
            }
        }
    }

    /// HEAD/refs moved without the user re-baselining yet.
    pub fn mark_head_dirty(&mut self) {
        self.head_dirty = true;
    }

    /// Top-level key dispatch. Picker mode shadows the normal bindings.
    pub fn handle_key(&mut self, key: KeyEvent) {
        if self.picker.is_some() {
            self.handle_picker_key(key);
        } else {
            self.handle_normal_key(key);
        }
    }

    // ---- normal-mode keys --------------------------------------------

    fn handle_normal_key(&mut self, key: KeyEvent) {
        // Quit shortcuts.
        if matches!(key.code, KeyCode::Char('q'))
            || (matches!(key.code, KeyCode::Char('c'))
                && key.modifiers.contains(KeyModifiers::CONTROL))
        {
            self.should_quit = true;
            return;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('d') => {
                    self.scroll_by(HALF_PAGE as isize);
                    self.follow_mode = false;
                    return;
                }
                KeyCode::Char('u') => {
                    self.scroll_by(-(HALF_PAGE as isize));
                    self.follow_mode = false;
                    return;
                }
                _ => {}
            }
        }

        match key.code {
            // Lowercase + arrows = the *common* case: hunk-by-hunk motion.
            // hunk is kizu's primary unit of attention, so it gets the easy
            // keys. SHIFT-J / SHIFT-K stay inside the current hunk and walk
            // change run by change run (each contiguous +/- block) so the
            // user can step over edit clusters without counting context lines.
            KeyCode::Char('j') | KeyCode::Down => {
                self.next_hunk();
                self.follow_mode = false;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.prev_hunk();
                self.follow_mode = false;
            }
            KeyCode::Char('J') => {
                self.next_change();
                self.follow_mode = false;
            }
            KeyCode::Char('K') => {
                self.prev_change();
                self.follow_mode = false;
            }
            KeyCode::Char('g') => {
                self.scroll_to(0);
                self.follow_mode = false;
            }
            KeyCode::Char('G') => {
                self.scroll_to(self.last_row_index());
                self.follow_mode = false;
            }
            KeyCode::Char('f') => {
                self.follow_restore();
            }
            KeyCode::Char(' ') => {
                self.open_picker();
            }
            KeyCode::Char('R') => {
                self.reset_baseline();
            }
            KeyCode::Char('z') => {
                self.toggle_cursor_placement();
            }
            _ => {}
        }
    }

    // ---- picker-mode keys --------------------------------------------

    fn handle_picker_key(&mut self, key: KeyEvent) {
        // Ctrl-* shortcuts: navigation + cancel. Picker uses fzf-style
        // bindings so any non-control char (including 'j' / 'k') is a
        // filter character.
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('n') | KeyCode::Char('j') => self.picker_cursor_down(),
                KeyCode::Char('p') | KeyCode::Char('k') => self.picker_cursor_up(),
                KeyCode::Char('c') => self.close_picker(),
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Esc => self.close_picker(),
            KeyCode::Enter => {
                let results = self.picker_results();
                let cursor = self.picker.as_ref().map(|p| p.cursor).unwrap_or(0);
                let target = results.get(cursor).copied();
                self.close_picker();
                if let Some(file_idx) = target {
                    self.jump_to_file_first_hunk(file_idx);
                }
            }
            KeyCode::Up => self.picker_cursor_up(),
            KeyCode::Down => self.picker_cursor_down(),
            KeyCode::Backspace => {
                if let Some(picker) = self.picker.as_mut() {
                    picker.query.pop();
                    picker.cursor = 0;
                }
            }
            KeyCode::Char(c) => {
                if let Some(picker) = self.picker.as_mut() {
                    picker.query.push(c);
                    picker.cursor = 0;
                }
            }
            _ => {}
        }
    }

    // ---- picker helpers ----------------------------------------------

    pub fn open_picker(&mut self) {
        self.picker = Some(PickerState::default());
    }

    pub fn close_picker(&mut self) {
        self.picker = None;
    }

    /// Indices into `self.files` for the picker's filtered view. The picker
    /// follows the file-picker convention of **newest first** even though
    /// `self.files` itself is stored in ascending mtime order: this way an
    /// empty-query → `Enter` lands on whatever the agent just touched.
    pub fn picker_results(&self) -> Vec<usize> {
        let needle = match &self.picker {
            Some(p) if !p.query.is_empty() => p.query.to_lowercase(),
            _ => return (0..self.files.len()).rev().collect(),
        };
        self.files
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, f)| f.path.to_string_lossy().to_lowercase().contains(&needle))
            .map(|(i, _)| i)
            .collect()
    }

    fn picker_cursor_down(&mut self) {
        let len = self.picker_results().len();
        if let Some(picker) = self.picker.as_mut()
            && len > 0
            && picker.cursor + 1 < len
        {
            picker.cursor += 1;
        }
    }

    fn picker_cursor_up(&mut self) {
        if let Some(picker) = self.picker.as_mut() {
            picker.cursor = picker.cursor.saturating_sub(1);
        }
    }

    // ---- navigation helpers ------------------------------------------

    pub fn scroll_by(&mut self, delta: isize) {
        let last = self.last_row_index();
        let next = (self.scroll as isize + delta).clamp(0, last as isize) as usize;
        self.scroll_to(next);
    }

    pub fn scroll_to(&mut self, row: usize) {
        let last = self.last_row_index();
        self.scroll = row.min(last);
        self.update_anchor_from_scroll();
    }

    fn last_row_index(&self) -> usize {
        self.layout.rows.len().saturating_sub(1)
    }

    pub fn next_hunk(&mut self) {
        if let Some(&row) = self
            .layout
            .hunk_starts
            .iter()
            .find(|&&start| start > self.scroll)
        {
            self.scroll_to(row);
        } else if let Some(&row) = self.layout.hunk_starts.last() {
            self.scroll_to(row);
        }
    }

    pub fn prev_hunk(&mut self) {
        if let Some(&row) = self
            .layout
            .hunk_starts
            .iter()
            .rev()
            .find(|&&start| start < self.scroll)
        {
            self.scroll_to(row);
        } else if let Some(&row) = self.layout.hunk_starts.first() {
            self.scroll_to(row);
        }
    }

    /// `J` — fine-grained forward motion through change runs.
    ///
    /// - Inside a long change run, scroll forward by [`Self::chunk_size`]
    ///   rows (clamped to the last row of the run).
    /// - Once the cursor reaches the end of the current run (or starts
    ///   outside any run), jump to the start of the next change run in
    ///   the layout — even when that run lives in a different hunk or
    ///   a different file. `J` flows continuously through the whole
    ///   scroll, ignoring hunk and file boundaries.
    pub fn next_change(&mut self) {
        let cursor = self.scroll;
        let chunk = self.chunk_size();
        if let Some(&(_, end)) = self.run_containing(cursor) {
            let last_row = end.saturating_sub(1);
            if cursor < last_row {
                let target = (cursor + chunk).min(last_row);
                self.scroll_to(target);
                return;
            }
        }
        if let Some(&(start, _)) = self.layout.change_runs.iter().find(|(s, _)| *s > cursor) {
            self.scroll_to(start);
        }
    }

    /// `K` — fine-grained backward motion. Mirror of [`Self::next_change`].
    pub fn prev_change(&mut self) {
        let cursor = self.scroll;
        let chunk = self.chunk_size();
        if let Some(&(start, _)) = self.run_containing(cursor)
            && cursor > start
        {
            let target = cursor.saturating_sub(chunk).max(start);
            self.scroll_to(target);
            return;
        }
        if let Some(&(start, _)) = self
            .layout
            .change_runs
            .iter()
            .rev()
            .find(|(s, _)| *s < cursor)
        {
            self.scroll_to(start);
        }
    }

    fn run_containing(&self, row: usize) -> Option<&(usize, usize)> {
        self.layout
            .change_runs
            .iter()
            .find(|(start, end)| row >= *start && row < *end)
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

    /// Row that "follow mode" parks the scroll cursor on: the *last* hunk of
    /// the *last* file. Files are sorted mtime-ascending, so the last file
    /// is the most recently touched one and its last hunk is the very
    /// bottom of the scroll view. Falls back to the absolute last row if
    /// there is no hunk anywhere.
    fn follow_target_row(&self) -> Option<usize> {
        if self.files.is_empty() {
            return None;
        }
        let file_idx = self.files.len() - 1;
        self.layout
            .hunk_starts
            .iter()
            .rev()
            .copied()
            .find(|&row| self.layout.file_of_row.get(row).copied() == Some(file_idx))
            .or_else(|| self.layout.file_first_hunk.last().copied().flatten())
            .or_else(|| self.layout.rows.len().checked_sub(1))
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

    // ---- layout build / anchor ----------------------------------------

    fn populate_mtimes(&self, files: &mut [FileDiff]) {
        for f in files {
            f.mtime = self
                .root
                .join(&f.path)
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
        }
    }

    pub(crate) fn build_layout(&mut self) {
        let mut layout = ScrollLayout {
            file_first_hunk: vec![None; self.files.len()],
            ..ScrollLayout::default()
        };

        for (file_idx, file) in self.files.iter().enumerate() {
            let header_row = layout.rows.len();
            layout.rows.push(RowKind::FileHeader { file_idx });

            match &file.content {
                DiffContent::Binary => {
                    let notice_row = layout.rows.len();
                    layout.rows.push(RowKind::BinaryNotice { file_idx });
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
                            layout.hunk_starts.push(row);
                            for line_idx in 0..hunk.lines.len() {
                                layout.rows.push(RowKind::DiffLine {
                                    file_idx,
                                    hunk_idx,
                                    line_idx,
                                });
                            }
                        }
                    }
                }
            }

            layout.rows.push(RowKind::Spacer);
        }

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
                } => match &self.files[*file_idx].content {
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

        self.layout = layout;
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

        if !self.follow_mode
            && let Some(anchor) = self.anchor.clone()
            && let Some(row) = self.find_anchor_row(&anchor)
        {
            self.scroll = row;
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

    fn update_anchor_from_scroll(&mut self) {
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

/// Async event loop. See ADR-0003 / ADR-0005.
pub async fn run() -> Result<()> {
    let cwd = std::env::current_dir().context("reading current directory")?;
    let mut app = App::bootstrap(cwd)?;
    let mut watch = watcher::start(&app.root, &app.git_dir)?;

    let mut terminal = ratatui::try_init().context("initializing terminal")?;
    let result = run_loop(&mut terminal, &mut app, &mut watch).await;
    let _ = ratatui::try_restore();
    result
}

async fn run_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    watch: &mut watcher::WatchHandle,
) -> Result<()> {
    use crossterm::event::{Event, EventStream};
    use futures_util::StreamExt;

    let mut events = EventStream::new();

    while !app.should_quit {
        // Draw at the top of the loop so the bootstrap state is visible
        // before we ever block on `select!`.
        terminal
            .draw(|frame| crate::ui::render(frame, app))
            .context("ratatui draw")?;

        tokio::select! {
            Some(Ok(event)) = events.next() => {
                if let Event::Key(key) = event {
                    app.handle_key(key);
                }
            }
            Some(first) = watch.events.recv() => {
                let mut worktree = matches!(first, WatchEvent::Worktree);
                let mut head = matches!(first, WatchEvent::GitHead);
                while let Ok(more) = watch.events.try_recv() {
                    match more {
                        WatchEvent::Worktree => worktree = true,
                        WatchEvent::GitHead => head = true,
                    }
                }
                if worktree {
                    app.recompute_diff();
                }
                if head {
                    app.mark_head_dirty();
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{DiffContent, DiffLine, FileStatus, Hunk, LineKind};
    use std::time::Duration;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn diff_line(kind: LineKind, content: &str) -> DiffLine {
        DiffLine {
            kind,
            content: content.to_string(),
        }
    }

    fn hunk(old_start: usize, lines: Vec<DiffLine>) -> Hunk {
        let added = lines.iter().filter(|l| l.kind == LineKind::Added).count();
        let deleted = lines.iter().filter(|l| l.kind == LineKind::Deleted).count();
        Hunk {
            old_start,
            old_count: deleted,
            new_start: old_start,
            new_count: added,
            lines,
            context: None,
        }
    }

    fn make_file(name: &str, hunks: Vec<Hunk>, secs: u64) -> FileDiff {
        let added: usize = hunks
            .iter()
            .flat_map(|h| h.lines.iter())
            .filter(|l| l.kind == LineKind::Added)
            .count();
        let deleted: usize = hunks
            .iter()
            .flat_map(|h| h.lines.iter())
            .filter(|l| l.kind == LineKind::Deleted)
            .count();
        FileDiff {
            path: PathBuf::from(name),
            status: FileStatus::Modified,
            added,
            deleted,
            content: DiffContent::Text(hunks),
            mtime: SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
        }
    }

    fn binary_file(name: &str, secs: u64) -> FileDiff {
        FileDiff {
            path: PathBuf::from(name),
            status: FileStatus::Modified,
            added: 0,
            deleted: 0,
            content: DiffContent::Binary,
            mtime: SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
        }
    }

    /// Build an `App` against `/tmp/fake` with no real filesystem use; the
    /// `populate_mtimes` step is bypassed by writing `mtime` directly on the
    /// fixtures. Files are sorted in ascending mtime order to match the
    /// real `recompute_diff` path.
    fn fake_app(files: Vec<FileDiff>) -> App {
        let mut app = App {
            root: PathBuf::from("/tmp/fake"),
            git_dir: PathBuf::from("/tmp/fake/.git"),
            baseline_sha: "abcdef1234567890abcdef1234567890abcdef12".into(),
            files: Vec::new(),
            layout: ScrollLayout::default(),
            scroll: 0,
            cursor_placement: CursorPlacement::Centered,
            anchor: None,
            picker: None,
            follow_mode: true,
            last_error: None,
            head_dirty: false,
            should_quit: false,
            last_body_height: Cell::new(DEFAULT_BODY_HEIGHT),
        };
        app.files = files;
        app.files.sort_by(|a, b| a.mtime.cmp(&b.mtime));
        app.build_layout();
        app.refresh_anchor();
        app
    }

    fn file_idx(app: &App, name: &str) -> usize {
        app.files
            .iter()
            .position(|f| f.path == Path::new(name))
            .unwrap_or_else(|| panic!("file {name} not in app.files"))
    }

    #[test]
    fn build_layout_produces_header_then_hunks_then_spacer_per_file() {
        let app = fake_app(vec![make_file(
            "a.rs",
            vec![hunk(1, vec![diff_line(LineKind::Added, "x")])],
            100,
        )]);

        // Expected sequence:
        //   FileHeader, HunkHeader, DiffLine, Spacer
        assert_eq!(app.layout.rows.len(), 4);
        assert!(matches!(
            app.layout.rows[0],
            RowKind::FileHeader { file_idx: 0 }
        ));
        assert!(matches!(
            app.layout.rows[1],
            RowKind::HunkHeader {
                file_idx: 0,
                hunk_idx: 0
            }
        ));
        assert!(matches!(
            app.layout.rows[2],
            RowKind::DiffLine {
                file_idx: 0,
                hunk_idx: 0,
                line_idx: 0
            }
        ));
        assert!(matches!(app.layout.rows[3], RowKind::Spacer));
        assert_eq!(app.layout.hunk_starts, vec![1]);
        assert_eq!(app.layout.file_first_hunk, vec![Some(1)]);
    }

    #[test]
    fn build_layout_marks_binary_file_with_notice_row() {
        let app = fake_app(vec![binary_file("icon.png", 100)]);
        // FileHeader, BinaryNotice, Spacer
        assert_eq!(app.layout.rows.len(), 3);
        assert!(matches!(
            app.layout.rows[1],
            RowKind::BinaryNotice { file_idx: 0 }
        ));
        assert!(app.layout.hunk_starts.is_empty());
        assert_eq!(app.layout.file_first_hunk, vec![Some(1)]);
    }

    #[test]
    fn next_hunk_jumps_across_file_boundaries() {
        let app_files = vec![
            // a.rs: newest, 2 hunks
            make_file(
                "a.rs",
                vec![
                    hunk(1, vec![diff_line(LineKind::Added, "x")]),
                    hunk(10, vec![diff_line(LineKind::Added, "y")]),
                ],
                200,
            ),
            // b.rs: older, 1 hunk
            make_file(
                "b.rs",
                vec![hunk(1, vec![diff_line(LineKind::Added, "z")])],
                100,
            ),
        ];
        let mut app = fake_app(app_files);
        // Three hunks total → three hunk_starts.
        assert_eq!(app.layout.hunk_starts.len(), 3);

        // Start at the very top.
        app.scroll_to(0);
        app.next_hunk();
        assert_eq!(app.scroll, app.layout.hunk_starts[0]);
        app.next_hunk();
        assert_eq!(app.scroll, app.layout.hunk_starts[1]);
        app.next_hunk();
        assert_eq!(app.scroll, app.layout.hunk_starts[2]);
        // Already past the last; stays put on the last.
        app.next_hunk();
        assert_eq!(app.scroll, app.layout.hunk_starts[2]);
    }

    #[test]
    fn prev_hunk_walks_backwards() {
        let app_files = vec![make_file(
            "a.rs",
            vec![
                hunk(1, vec![diff_line(LineKind::Added, "x")]),
                hunk(10, vec![diff_line(LineKind::Added, "y")]),
            ],
            100,
        )];
        let mut app = fake_app(app_files);
        let last_hunk = *app.layout.hunk_starts.last().unwrap();
        app.scroll_to(last_hunk);
        app.prev_hunk();
        assert_eq!(app.scroll, app.layout.hunk_starts[0]);
        // Already on the first; clamps.
        app.prev_hunk();
        assert_eq!(app.scroll, app.layout.hunk_starts[0]);
    }

    #[test]
    fn follow_target_row_is_last_hunk_of_last_file() {
        // newest.rs has the largest mtime → ends up at the *bottom* of
        // the ascending-sort layout. Its second hunk is the very last
        // hunk_starts entry, and the bootstrap follow refresh should
        // park scroll on it.
        let app = fake_app(vec![
            make_file(
                "older.rs",
                vec![hunk(1, vec![diff_line(LineKind::Added, "c")])],
                100,
            ),
            make_file(
                "newest.rs",
                vec![
                    hunk(1, vec![diff_line(LineKind::Added, "a")]),
                    hunk(20, vec![diff_line(LineKind::Added, "b")]),
                ],
                300,
            ),
        ]);
        let last_hunk_row = *app.layout.hunk_starts.last().unwrap();
        assert_eq!(app.scroll, last_hunk_row);
    }

    #[test]
    fn current_file_path_reports_the_file_under_the_cursor() {
        let mut app = fake_app(vec![
            make_file(
                "a.rs",
                vec![hunk(1, vec![diff_line(LineKind::Added, "x")])],
                200,
            ),
            make_file(
                "b.rs",
                vec![hunk(1, vec![diff_line(LineKind::Added, "y")])],
                100,
            ),
        ]);
        // a.rs has the larger mtime → it sorts to the bottom of the
        // layout, and bootstrap follow lands on it.
        assert_eq!(app.current_file_path(), Some(Path::new("a.rs")));

        // Jump to b.rs's first hunk by looking it up by path so the test
        // doesn't rely on a specific index.
        let b = file_idx(&app, "b.rs");
        app.jump_to_file_first_hunk(b);
        assert_eq!(app.current_file_path(), Some(Path::new("b.rs")));
    }

    #[test]
    fn handle_key_j_jumps_to_next_hunk_and_disables_follow() {
        // After M4v.swap, lowercase `j` is hunk-forward.
        let mut app = fake_app(vec![make_file(
            "a.rs",
            vec![
                hunk(1, vec![diff_line(LineKind::Added, "x")]),
                hunk(20, vec![diff_line(LineKind::Added, "y")]),
            ],
            100,
        )]);
        app.scroll_to(0);
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.scroll, app.layout.hunk_starts[0]);
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.scroll, app.layout.hunk_starts[1]);
        assert!(!app.follow_mode);
    }

    #[test]
    fn capital_j_in_short_run_jumps_to_next_run_anywhere() {
        // Two single-row runs separated by a context line. From the
        // first run's only row, SHIFT-J falls through to the next run
        // start because there are no more rows inside the current one.
        let mut app = fake_app(vec![make_file(
            "a.rs",
            vec![hunk(
                1,
                vec![
                    diff_line(LineKind::Context, " keep"),
                    diff_line(LineKind::Added, "alpha"),
                    diff_line(LineKind::Context, " keep"),
                    diff_line(LineKind::Added, "beta"),
                    diff_line(LineKind::Context, " keep"),
                ],
            )],
            100,
        )]);
        assert_eq!(app.layout.change_runs.len(), 2);
        let (first_start, _) = app.layout.change_runs[0];
        let (second_start, _) = app.layout.change_runs[1];

        app.scroll_to(first_start);
        app.handle_key(key(KeyCode::Char('J')));
        assert_eq!(app.scroll, second_start);
        assert!(!app.follow_mode);

        // No more runs after this one → stay put.
        app.handle_key(key(KeyCode::Char('J')));
        assert_eq!(app.scroll, second_start);
    }

    #[test]
    fn capital_j_in_long_run_scrolls_within_run_by_chunk() {
        // Force a small body height so the chunk size is exactly 5 rows
        // (15 / 3 = 5) — this way the test's expected scroll positions
        // are fixed regardless of the DEFAULT_BODY_HEIGHT used outside
        // of tests.
        let lines: Vec<DiffLine> = (0..20)
            .map(|i| diff_line(LineKind::Added, &format!("line {i}")))
            .collect();
        let mut app = fake_app(vec![make_file("a.rs", vec![hunk(1, lines)], 100)]);
        app.last_body_height.set(15);
        let chunk = app.chunk_size();
        assert_eq!(chunk, 5);
        let (start, end) = app.layout.change_runs[0];
        let last = end - 1;

        app.scroll_to(start);
        app.handle_key(key(KeyCode::Char('J')));
        assert_eq!(app.scroll, start + chunk);

        app.handle_key(key(KeyCode::Char('J')));
        assert_eq!(app.scroll, start + 2 * chunk);

        app.handle_key(key(KeyCode::Char('J')));
        assert_eq!(app.scroll, start + 3 * chunk);

        // Subsequent presses clamp at the last row of the run.
        app.handle_key(key(KeyCode::Char('J')));
        assert_eq!(app.scroll, last);
    }

    #[test]
    fn capital_j_crosses_hunk_and_file_boundaries() {
        // Run 1 in a.rs, run 2 in b.rs. From run 1's only row, SHIFT-J
        // should jump straight into run 2 even though that means
        // crossing both a hunk boundary and a file boundary.
        let mut app = fake_app(vec![
            make_file(
                "a.rs",
                vec![hunk(1, vec![diff_line(LineKind::Added, "alpha")])],
                100,
            ),
            make_file(
                "b.rs",
                vec![hunk(1, vec![diff_line(LineKind::Added, "beta")])],
                200,
            ),
        ]);
        assert_eq!(app.layout.change_runs.len(), 2);
        let (first_start, _) = app.layout.change_runs[0];
        let (second_start, _) = app.layout.change_runs[1];

        app.scroll_to(first_start);
        app.handle_key(key(KeyCode::Char('J')));
        assert_eq!(
            app.scroll, second_start,
            "SHIFT-J must cross hunk + file boundaries when no rows remain in the current run"
        );
    }

    #[test]
    fn capital_k_in_long_run_walks_back_by_chunk() {
        let lines: Vec<DiffLine> = (0..20)
            .map(|i| diff_line(LineKind::Added, &format!("line {i}")))
            .collect();
        let mut app = fake_app(vec![make_file("a.rs", vec![hunk(1, lines)], 100)]);
        app.last_body_height.set(15);
        let chunk = app.chunk_size();
        let (start, end) = app.layout.change_runs[0];
        let last = end - 1;

        app.scroll_to(last);
        app.handle_key(key(KeyCode::Char('K')));
        assert_eq!(app.scroll, last - chunk);

        // Continue back; should stay >= the run's first row, not before.
        app.handle_key(key(KeyCode::Char('K')));
        app.handle_key(key(KeyCode::Char('K')));
        app.handle_key(key(KeyCode::Char('K')));
        assert!(app.scroll >= start);
    }

    #[test]
    fn capital_k_at_run_start_jumps_to_previous_run() {
        let mut app = fake_app(vec![make_file(
            "a.rs",
            vec![hunk(
                1,
                vec![
                    diff_line(LineKind::Added, "alpha"),
                    diff_line(LineKind::Context, " keep"),
                    diff_line(LineKind::Added, "beta"),
                ],
            )],
            100,
        )]);
        let (first_start, _) = app.layout.change_runs[0];
        let (second_start, _) = app.layout.change_runs[1];

        app.scroll_to(second_start);
        app.handle_key(key(KeyCode::Char('K')));
        assert_eq!(app.scroll, first_start);
    }

    #[test]
    fn chunk_size_scales_with_last_body_height() {
        let app = fake_app(vec![]);
        // Default body height is 24 → chunk = 24/3 = 8.
        assert_eq!(app.chunk_size(), 8);

        // A taller pane should yield a bigger chunk.
        app.last_body_height.set(36);
        assert_eq!(app.chunk_size(), 12);

        // A tiny pane should never go below 1 row.
        app.last_body_height.set(2);
        assert_eq!(app.chunk_size(), 1);

        // Zero height (degenerate) still gives at least 1.
        app.last_body_height.set(0);
        assert_eq!(app.chunk_size(), 1);
    }

    #[test]
    fn cursor_placement_centered_keeps_cursor_in_the_middle() {
        // 100 row layout, viewport 20 rows, cursor at row 50.
        // Centered → viewport_top = 50 - 10 = 40, cursor visually at row 10.
        let placement = CursorPlacement::Centered;
        assert_eq!(placement.viewport_top(50, 100, 20), 40);
    }

    #[test]
    fn cursor_placement_centered_clamps_at_top_and_bottom() {
        let placement = CursorPlacement::Centered;
        // Cursor near the start: viewport_top can't go below 0.
        assert_eq!(placement.viewport_top(2, 100, 20), 0);
        // Cursor near the end: viewport_top clamped at total - height.
        assert_eq!(placement.viewport_top(99, 100, 20), 80);
    }

    #[test]
    fn cursor_placement_bottom_pins_cursor_to_the_floor() {
        // Cursor at row 50, viewport 20: cursor visually at row 19 (last
        // row of viewport), viewport_top = 50 - 19 = 31.
        let placement = CursorPlacement::Bottom;
        assert_eq!(placement.viewport_top(50, 100, 20), 31);
    }

    #[test]
    fn cursor_placement_returns_zero_when_layout_fits_in_viewport() {
        // 5 rows, viewport 20 → no scrolling possible regardless of mode.
        assert_eq!(CursorPlacement::Centered.viewport_top(3, 5, 20), 0);
        assert_eq!(CursorPlacement::Bottom.viewport_top(3, 5, 20), 0);
    }

    #[test]
    fn z_key_toggles_cursor_placement() {
        let mut app = fake_app(vec![]);
        assert_eq!(app.cursor_placement, CursorPlacement::Centered);
        app.handle_key(key(KeyCode::Char('z')));
        assert_eq!(app.cursor_placement, CursorPlacement::Bottom);
        app.handle_key(key(KeyCode::Char('z')));
        assert_eq!(app.cursor_placement, CursorPlacement::Centered);
    }

    #[test]
    fn change_runs_collapse_consecutive_same_kind_lines_into_one_entry() {
        // Three contiguous +/- lines should be a single change run, not three.
        let app = fake_app(vec![make_file(
            "a.rs",
            vec![hunk(
                1,
                vec![
                    diff_line(LineKind::Added, "a"),
                    diff_line(LineKind::Added, "b"),
                    diff_line(LineKind::Deleted, "c"),
                ],
            )],
            100,
        )]);
        assert_eq!(
            app.layout.change_runs.len(),
            1,
            "expected one change run for an all-contiguous +/- block"
        );
        let (start, end) = app.layout.change_runs[0];
        assert_eq!(end - start, 3);
    }

    #[test]
    fn handle_key_g_and_capital_g_move_to_top_and_bottom() {
        let mut app = fake_app(vec![make_file(
            "a.rs",
            vec![hunk(
                1,
                vec![
                    diff_line(LineKind::Added, "x"),
                    diff_line(LineKind::Added, "y"),
                    diff_line(LineKind::Added, "z"),
                ],
            )],
            100,
        )]);
        app.handle_key(key(KeyCode::Char('G')));
        assert_eq!(app.scroll, app.layout.rows.len() - 1);
        app.handle_key(key(KeyCode::Char('g')));
        assert_eq!(app.scroll, 0);
    }

    #[test]
    fn handle_key_f_restores_follow_mode_and_jumps_to_target() {
        let mut app = fake_app(vec![make_file(
            "a.rs",
            vec![
                hunk(1, vec![diff_line(LineKind::Added, "x")]),
                hunk(20, vec![diff_line(LineKind::Added, "y")]),
            ],
            100,
        )]);
        app.handle_key(key(KeyCode::Char('g'))); // jump to top, drops follow
        assert!(!app.follow_mode);
        app.handle_key(key(KeyCode::Char('f')));
        assert!(app.follow_mode);
        // Follow target = last hunk of newest file.
        assert_eq!(app.scroll, app.layout.hunk_starts[1]);
    }

    #[test]
    fn handle_key_q_and_ctrl_c_quit_in_normal_mode() {
        let mut app = fake_app(vec![]);
        app.handle_key(key(KeyCode::Char('q')));
        assert!(app.should_quit);

        let mut app = fake_app(vec![]);
        app.handle_key(ctrl('c'));
        assert!(app.should_quit);
    }

    #[test]
    fn space_opens_picker_and_esc_closes_it() {
        let mut app = fake_app(vec![make_file(
            "a.rs",
            vec![hunk(1, vec![diff_line(LineKind::Added, "x")])],
            100,
        )]);
        app.handle_key(key(KeyCode::Char(' ')));
        assert!(app.picker.is_some());

        app.handle_key(key(KeyCode::Esc));
        assert!(app.picker.is_none());
    }

    #[test]
    fn picker_filters_by_substring_case_insensitively() {
        let mut app = fake_app(vec![
            make_file(
                "src/Auth.rs",
                vec![hunk(1, vec![diff_line(LineKind::Added, "x")])],
                300,
            ),
            make_file(
                "src/handler.rs",
                vec![hunk(1, vec![diff_line(LineKind::Added, "y")])],
                200,
            ),
            make_file(
                "tests/auth_test.rs",
                vec![hunk(1, vec![diff_line(LineKind::Added, "z")])],
                100,
            ),
        ]);
        app.open_picker();
        for c in "auth".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        let results = app.picker_results();
        // src/Auth.rs and tests/auth_test.rs match; src/handler.rs does not.
        assert_eq!(results.len(), 2);
        let paths: Vec<_> = results.iter().map(|&i| app.files[i].path.clone()).collect();
        assert!(paths.contains(&PathBuf::from("src/Auth.rs")));
        assert!(paths.contains(&PathBuf::from("tests/auth_test.rs")));
    }

    #[test]
    fn picker_enter_jumps_to_selected_file_first_hunk_and_closes() {
        let mut app = fake_app(vec![
            make_file(
                "newest.rs",
                vec![hunk(1, vec![diff_line(LineKind::Added, "x")])],
                300,
            ),
            make_file(
                "older.rs",
                vec![hunk(50, vec![diff_line(LineKind::Added, "y")])],
                100,
            ),
        ]);
        app.open_picker();
        // picker_results runs newest-first regardless of how files are
        // stored internally, so cursor 0 = newest.rs and cursor 1 = older.rs.
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Enter));

        assert!(app.picker.is_none());
        let older = file_idx(&app, "older.rs");
        let expected = app.layout.file_first_hunk[older].unwrap();
        assert_eq!(app.scroll, expected);
        assert_eq!(app.current_file_path(), Some(Path::new("older.rs")));
    }

    #[test]
    fn refresh_anchor_keeps_us_on_the_same_hunk_after_recompute() {
        // First snapshot: 2 files, scroll parked on b.rs's hunk.
        let mut app = fake_app(vec![
            make_file(
                "a.rs",
                vec![hunk(1, vec![diff_line(LineKind::Added, "x")])],
                200,
            ),
            make_file(
                "b.rs",
                vec![hunk(42, vec![diff_line(LineKind::Added, "y")])],
                100,
            ),
        ]);
        // Move to b.rs's hunk by path lookup and disable follow so the
        // anchor stays put.
        let b = file_idx(&app, "b.rs");
        app.jump_to_file_first_hunk(b);
        app.follow_mode = false;
        app.update_anchor_from_scroll();
        let anchor_before = app.anchor.clone().expect("anchor set");
        assert_eq!(anchor_before.path, PathBuf::from("b.rs"));
        assert_eq!(anchor_before.hunk_old_start, 42);

        // Simulate a recompute by appending a new (older) file. The list
        // is re-sorted ascending; b.rs stays in the layout but its row
        // index moves. The anchor must still resolve to it.
        app.files.push(make_file(
            "c.rs",
            vec![hunk(1, vec![diff_line(LineKind::Added, "z")])],
            50, // older than b.rs
        ));
        app.files.sort_by(|x, y| x.mtime.cmp(&y.mtime));
        app.build_layout();
        app.refresh_anchor();

        assert_eq!(app.current_file_path(), Some(Path::new("b.rs")));
    }
}
