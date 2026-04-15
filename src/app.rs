use anyhow::{Context, Result};
use std::path::PathBuf;
use std::time::SystemTime;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::git::{self, FileDiff};
use crate::watcher::{self, WatchEvent};

/// Top-level application state. All fields are owned; the event loop in
/// [`run`] mutates this through `&mut self` and never shares it across
/// threads (we use the `current_thread` tokio flavor — see ADR-0003).
pub struct App {
    pub root: PathBuf,
    pub git_dir: PathBuf,
    pub baseline_sha: String,
    /// Files in the diff, sorted by `mtime` descending. Index 0 is the
    /// most-recently-modified file.
    pub files: Vec<FileDiff>,
    /// Currently focused row in `files`. Always points at `selected_path`
    /// when one is set; only used for rendering.
    pub selected: usize,
    /// Path-tracked selection. Survives `recompute_diff` calls so the user's
    /// "I'm looking at this file" context is preserved across watcher events.
    pub selected_path: Option<PathBuf>,
    /// Logical row offset for the right-hand diff pane (M4 will read it).
    pub diff_scroll: usize,
    pub follow_mode: bool,
    /// Set when the most recent `compute_diff` failed. Cleared on success.
    /// The footer (M4) renders this in red without dropping `files`.
    pub last_error: Option<String>,
    /// Set whenever HEAD/refs move; the user must press `R` to re-baseline.
    pub head_dirty: bool,
    pub should_quit: bool,
}

impl App {
    /// Construct an `App` for `root`. Resolves the git directory, captures
    /// the HEAD SHA (falling back to the empty tree in a fresh repo), and
    /// loads the initial diff. Errors at this stage are fatal — the caller
    /// should print them to stderr and exit (M5 wires this up).
    pub fn bootstrap(root: PathBuf) -> Result<Self> {
        let root = git::find_root(&root).context("resolving worktree root")?;
        let git_dir = git::git_dir(&root).context("resolving git directory")?;
        let baseline_sha = git::head_sha(&root).context("capturing baseline HEAD")?;

        let mut app = Self {
            root,
            git_dir,
            baseline_sha,
            files: Vec::new(),
            selected: 0,
            selected_path: None,
            diff_scroll: 0,
            follow_mode: true,
            last_error: None,
            head_dirty: false,
            should_quit: false,
        };
        app.recompute_diff();
        Ok(app)
    }

    /// Re-run `git diff`, populate per-file mtimes, sort by mtime descending,
    /// and re-apply the path-tracked selection. On failure, record the error
    /// in `last_error` and keep the previous `files` snapshot intact
    /// (Decision Log: error footer + previous state retained).
    pub fn recompute_diff(&mut self) {
        match git::compute_diff(&self.root, &self.baseline_sha) {
            Ok(mut files) => {
                self.populate_mtimes(&mut files);
                self.last_error = None;
                self.sort_and_select(files);
            }
            Err(e) => {
                self.last_error = Some(format!("{e:#}"));
                // self.files intentionally untouched.
            }
        }
    }

    /// Re-capture HEAD as the new baseline (R key handler). Always followed
    /// by a recompute so the screen reflects the new baseline immediately.
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

    /// Mark that HEAD/refs moved without re-baselining. Pure flag flip; the
    /// app loop drains `WatchEvent::GitHead` and calls this so the footer
    /// (M4) can hint that the user might want to press `R`.
    pub fn mark_head_dirty(&mut self) {
        self.head_dirty = true;
    }

    /// Apply a key event. Pure-ish (no IO except for `reset_baseline`).
    /// Manual navigation downgrades `follow_mode` to manual; `f` re-enables
    /// follow and snaps the selection back to the mtime-newest file.
    pub fn handle_key(&mut self, key: KeyEvent) {
        // Quit shortcuts come first so they always win.
        if matches!(key.code, KeyCode::Char('q'))
            || (matches!(key.code, KeyCode::Char('c'))
                && key.modifiers.contains(KeyModifiers::CONTROL))
        {
            self.should_quit = true;
            return;
        }

        // Half-page diff scroll (Ctrl-d / Ctrl-u).
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('d') => {
                    self.diff_scroll = self.diff_scroll.saturating_add(HALF_PAGE);
                    self.follow_mode = false;
                    return;
                }
                KeyCode::Char('u') => {
                    self.diff_scroll = self.diff_scroll.saturating_sub(HALF_PAGE);
                    self.follow_mode = false;
                    return;
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.move_selection(1);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.move_selection(-1);
            }
            KeyCode::Char('g') => {
                self.diff_scroll = 0;
                self.follow_mode = false;
            }
            KeyCode::Char('G') => {
                // Jump to "end" — without a viewport size we just push the
                // scroll cursor very far; M4 will clamp during render.
                self.diff_scroll = usize::MAX / 2;
                self.follow_mode = false;
            }
            KeyCode::Char('f') => {
                self.follow_mode = true;
                if !self.files.is_empty() {
                    self.selected = 0;
                    self.selected_path = Some(self.files[0].path.clone());
                    self.diff_scroll = 0;
                }
            }
            KeyCode::Char('R') => {
                self.reset_baseline();
            }
            _ => {}
        }
    }

    // ---- internals ----------------------------------------------------

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

    /// Pure: take a fresh diff snapshot (with mtimes already filled in),
    /// sort it by mtime descending, and rewire the path-tracked selection.
    fn sort_and_select(&mut self, mut files: Vec<FileDiff>) {
        files.sort_by(|a, b| b.mtime.cmp(&a.mtime));
        self.files = files;
        self.refresh_selection();
    }

    fn refresh_selection(&mut self) {
        if self.files.is_empty() {
            self.selected = 0;
            self.selected_path = None;
            return;
        }

        // 1. Try to keep the same path under the cursor.
        if let Some(path) = self.selected_path.clone()
            && let Some(idx) = self.files.iter().position(|f| f.path == path)
        {
            self.selected = idx;
            return;
        }

        // 2. Path is gone (deleted file disappeared, etc).
        if self.follow_mode {
            // Follow mode snaps to the mtime-newest file (= index 0).
            self.selected = 0;
        } else {
            // Manual: clamp the previous index into the new bounds.
            self.selected = self.selected.min(self.files.len() - 1);
        }
        self.selected_path = Some(self.files[self.selected].path.clone());
    }

    fn move_selection(&mut self, delta: isize) {
        if self.files.is_empty() {
            return;
        }
        let len = self.files.len() as isize;
        let next = (self.selected as isize + delta).clamp(0, len - 1) as usize;
        self.selected = next;
        self.selected_path = Some(self.files[next].path.clone());
        self.diff_scroll = 0;
        self.follow_mode = false;
    }
}

/// Half-page scroll constant for `Ctrl-d` / `Ctrl-u`. M4 will swap this for
/// the real viewport height once the layout is known; until then a fixed
/// value keeps `handle_key` testable as a pure function.
const HALF_PAGE: usize = 12;

/// Async event loop. Wires the watcher channel and crossterm's async event
/// stream into a single `tokio::select!`, applies drain-based coalescing
/// (ADR-0005), and yields control to [`crate::ui::render`] every iteration.
///
/// Raw mode, alternate-screen entry, panic hook, and main wiring all live
/// in M5. For now this function compiles and runs the core loop, but it is
/// **not yet called from `main`**.
#[allow(dead_code)]
pub async fn run() -> Result<()> {
    use crossterm::event::{Event, EventStream};
    use futures_util::StreamExt;

    let cwd = std::env::current_dir().context("reading current directory")?;
    let mut app = App::bootstrap(cwd)?;
    let mut watch = watcher::start(&app.root, &app.git_dir)?;
    let mut events = EventStream::new();

    while !app.should_quit {
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
        // M4 will replace this with `terminal.draw(|f| ui::render(f, &app))?;`
        crate::ui::render(&app);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{DiffContent, FileStatus};
    use std::time::Duration;

    fn make_file(name: &str, secs: u64) -> FileDiff {
        FileDiff {
            path: PathBuf::from(name),
            status: FileStatus::Modified,
            added: 1,
            deleted: 0,
            content: DiffContent::Text(Vec::new()),
            mtime: SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
        }
    }

    /// Build a synthetic App without touching any real filesystem.
    fn fake_app() -> App {
        App {
            root: PathBuf::from("/tmp/fake"),
            git_dir: PathBuf::from("/tmp/fake/.git"),
            baseline_sha: "0000000000000000000000000000000000000000".into(),
            files: Vec::new(),
            selected: 0,
            selected_path: None,
            diff_scroll: 0,
            follow_mode: true,
            last_error: None,
            head_dirty: false,
            should_quit: false,
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn sort_and_select_orders_files_by_mtime_descending() {
        let mut app = fake_app();
        let files = vec![
            make_file("old.rs", 10),
            make_file("new.rs", 100),
            make_file("mid.rs", 50),
        ];
        app.sort_and_select(files);
        assert_eq!(app.files[0].path, PathBuf::from("new.rs"));
        assert_eq!(app.files[1].path, PathBuf::from("mid.rs"));
        assert_eq!(app.files[2].path, PathBuf::from("old.rs"));
    }

    #[test]
    fn refresh_selection_in_follow_mode_jumps_to_mtime_newest() {
        let mut app = fake_app();
        let files = vec![make_file("a.rs", 10), make_file("b.rs", 100)];
        app.sort_and_select(files);
        // After sort, b.rs (newer) is index 0, and follow mode selects it.
        assert_eq!(app.selected, 0);
        assert_eq!(app.selected_path, Some(PathBuf::from("b.rs")));
    }

    #[test]
    fn refresh_selection_preserves_path_across_recompute() {
        let mut app = fake_app();
        // Initial set: a.rs is older, b.rs newer. Follow mode picks b.rs.
        app.sort_and_select(vec![make_file("a.rs", 10), make_file("b.rs", 100)]);
        assert_eq!(app.selected_path, Some(PathBuf::from("b.rs")));

        // Manually move to a.rs (downgrades follow mode to manual).
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.selected_path, Some(PathBuf::from("a.rs")));
        assert!(!app.follow_mode);

        // A new file appears; old paths still exist with refreshed mtimes.
        // a.rs must remain selected because we are tracking by path.
        app.sort_and_select(vec![
            make_file("a.rs", 10),
            make_file("b.rs", 100),
            make_file("c.rs", 200),
        ]);
        assert_eq!(app.selected_path, Some(PathBuf::from("a.rs")));
    }

    #[test]
    fn refresh_selection_falls_back_to_clamp_when_path_disappears_in_manual_mode() {
        let mut app = fake_app();
        app.sort_and_select(vec![make_file("a.rs", 10), make_file("b.rs", 100)]);
        // Move down to a.rs and downgrade.
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.selected_path, Some(PathBuf::from("a.rs")));
        assert!(!app.follow_mode);

        // Drop a.rs entirely.
        app.sort_and_select(vec![make_file("b.rs", 100)]);
        // Path is gone, clamp to len - 1 = 0, which is now b.rs.
        assert_eq!(app.selected, 0);
        assert_eq!(app.selected_path, Some(PathBuf::from("b.rs")));
    }

    #[test]
    fn handle_key_j_advances_selection_and_downgrades_follow() {
        let mut app = fake_app();
        app.sort_and_select(vec![make_file("a.rs", 100), make_file("b.rs", 50)]);
        // Mtime sort puts a.rs at 0, b.rs at 1. Follow → selected = 0.
        assert_eq!(app.selected, 0);
        assert!(app.follow_mode);

        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.selected, 1);
        assert!(!app.follow_mode);
    }

    #[test]
    fn handle_key_k_retreats_selection() {
        let mut app = fake_app();
        app.sort_and_select(vec![make_file("a.rs", 100), make_file("b.rs", 50)]);
        app.handle_key(key(KeyCode::Char('j')));
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn handle_key_f_restores_follow_mode_and_snaps_to_top() {
        let mut app = fake_app();
        app.sort_and_select(vec![make_file("a.rs", 100), make_file("b.rs", 50)]);
        app.handle_key(key(KeyCode::Char('j')));
        assert!(!app.follow_mode);
        app.handle_key(key(KeyCode::Char('f')));
        assert!(app.follow_mode);
        assert_eq!(app.selected, 0);
        assert_eq!(app.selected_path, Some(PathBuf::from("a.rs")));
    }

    #[test]
    fn handle_key_ctrl_d_advances_diff_scroll_and_downgrades_follow() {
        let mut app = fake_app();
        app.sort_and_select(vec![make_file("a.rs", 100)]);
        assert_eq!(app.diff_scroll, 0);
        app.handle_key(ctrl('d'));
        assert!(app.diff_scroll > 0);
        assert!(!app.follow_mode);
    }

    #[test]
    fn handle_key_ctrl_u_retreats_diff_scroll_saturating_at_zero() {
        let mut app = fake_app();
        app.sort_and_select(vec![make_file("a.rs", 100)]);
        app.handle_key(ctrl('d'));
        let after_down = app.diff_scroll;
        app.handle_key(ctrl('u'));
        assert!(app.diff_scroll < after_down);

        app.handle_key(ctrl('u'));
        app.handle_key(ctrl('u'));
        app.handle_key(ctrl('u'));
        assert_eq!(app.diff_scroll, 0, "Ctrl-u should saturate at 0");
    }

    #[test]
    fn handle_key_g_resets_diff_scroll_and_capital_g_jumps_far() {
        let mut app = fake_app();
        app.sort_and_select(vec![make_file("a.rs", 100)]);
        app.handle_key(ctrl('d'));
        app.handle_key(ctrl('d'));
        assert!(app.diff_scroll > 0);

        app.handle_key(key(KeyCode::Char('g')));
        assert_eq!(app.diff_scroll, 0);
        assert!(!app.follow_mode);

        app.handle_key(key(KeyCode::Char('G')));
        assert!(
            app.diff_scroll > 1_000_000,
            "G should jump far for M4 to clamp"
        );
    }

    #[test]
    fn handle_key_q_sets_should_quit() {
        let mut app = fake_app();
        app.handle_key(key(KeyCode::Char('q')));
        assert!(app.should_quit);
    }

    #[test]
    fn handle_key_ctrl_c_sets_should_quit() {
        let mut app = fake_app();
        app.handle_key(ctrl('c'));
        assert!(app.should_quit);
    }

    #[test]
    fn move_selection_is_a_noop_on_empty_files() {
        let mut app = fake_app();
        app.handle_key(key(KeyCode::Char('j')));
        // Should not panic and should not produce nonsense state.
        assert_eq!(app.selected, 0);
        assert_eq!(app.selected_path, None);
    }
}
