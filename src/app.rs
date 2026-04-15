use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::git::{self, DiffContent, FileDiff};
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
    /// Current top-of-viewport row index inside `layout.rows`.
    pub scroll: usize,
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
}

/// Pre-computed layout for the scroll view. Built once per `recompute_diff`,
/// then sliced into a viewport at render time.
#[derive(Debug, Default, Clone)]
pub struct ScrollLayout {
    /// Every visible row in order.
    pub rows: Vec<RowKind>,
    /// `rows` indices that point at a `HunkHeader` — used by `J/K` to jump
    /// hunk-by-hunk regardless of how many context lines sit in between.
    pub hunk_starts: Vec<usize>,
    /// For each file in `App.files`, the row index of its first hunk header
    /// (or the file header for binaries / empty hunks). `None` only when the
    /// layout build couldn't produce any anchorable row for that file.
    pub file_first_hunk: Vec<Option<usize>>,
    /// `file_of_row[i]` is the index into `App.files` for whichever file row
    /// `i` belongs to. The footer reads this to display the current file.
    pub file_of_row: Vec<usize>,
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
            anchor: None,
            picker: None,
            follow_mode: true,
            last_error: None,
            head_dirty: false,
            should_quit: false,
        };
        app.recompute_diff();
        Ok(app)
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
            KeyCode::Char('j') | KeyCode::Down => {
                self.scroll_by(1);
                self.follow_mode = false;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.scroll_by(-1);
                self.follow_mode = false;
            }
            KeyCode::Char('J') => {
                self.next_hunk();
                self.follow_mode = false;
            }
            KeyCode::Char('K') => {
                self.prev_hunk();
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
            anchor: None,
            picker: None,
            follow_mode: true,
            last_error: None,
            head_dirty: false,
            should_quit: false,
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
    fn handle_key_j_scrolls_one_row_and_disables_follow() {
        let mut app = fake_app(vec![make_file(
            "a.rs",
            vec![hunk(
                1,
                vec![
                    diff_line(LineKind::Added, "x"),
                    diff_line(LineKind::Added, "y"),
                ],
            )],
            100,
        )]);
        let start = app.scroll;
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.scroll, start + 1);
        assert!(!app.follow_mode);
    }

    #[test]
    fn handle_key_capital_j_jumps_to_next_hunk() {
        let mut app = fake_app(vec![make_file(
            "a.rs",
            vec![
                hunk(1, vec![diff_line(LineKind::Added, "x")]),
                hunk(20, vec![diff_line(LineKind::Added, "y")]),
            ],
            100,
        )]);
        app.scroll_to(0);
        app.handle_key(key(KeyCode::Char('J')));
        assert_eq!(app.scroll, app.layout.hunk_starts[0]);
        app.handle_key(key(KeyCode::Char('J')));
        assert_eq!(app.scroll, app.layout.hunk_starts[1]);
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
