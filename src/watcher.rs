use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use notify::RecursiveMode;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

mod backend;
mod event_log;
mod git_state;
mod matcher;
mod worktree;

use backend::KizuDebouncer;
use event_log::spawn_events_dir_debouncer;
use git_state::{git_state_watch_roots, spawn_git_state_debouncer};
use matcher::{BaselineMatcherInner, SharedMatcher};
use worktree::{recursive_worktree_children, spawn_worktree_debouncer};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum WatchSource {
    Worktree,
    GitPerWorktreeHead,
    GitRefs,
    GitCommonRoot,
}

impl WatchSource {
    pub fn label(self) -> &'static str {
        match self {
            WatchSource::Worktree => "worktree",
            WatchSource::GitPerWorktreeHead => "git.head",
            WatchSource::GitRefs => "git.refs",
            WatchSource::GitCommonRoot => "git.root",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WatchRoot {
    path: PathBuf,
    recursive_mode: RecursiveMode,
    compare_contents: bool,
    source: WatchSource,
}

/// A coarse classification of file system activity that the app loop cares
/// about. The actual diff recompute is driven from these signals; we don't
/// pass payloads on this channel because the app always re-runs `git diff`
/// after coalescing (ADR-0005).
///
/// `Error` is surfaced when the underlying notify backend reports a
/// failure — a dropped event queue on macOS FSEvents, a watched
/// directory that was moved or deleted, a kqueue overflow on BSD,
/// etc. The app turns it into a visible `last_error` and forces a
/// recompute so the UI can't silently drift stale if the filesystem
/// hook has quietly fallen over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchEvent {
    /// Something inside the worktree (excluding `<git_dir>`) changed.
    Worktree,
    /// Something baseline-affecting inside `<git_dir>` changed.
    GitHead(WatchSource),
    /// A new event-log file was created in `<state_dir>/events/`.
    /// Carries the absolute path to the new file so the app can
    /// read and parse it without re-scanning the directory.
    EventLog(PathBuf),
    /// The underlying notify backend reported an error. The app
    /// treats this as a forced recompute plus a visible error string.
    Error {
        source: WatchSource,
        message: String,
    },
}

/// Owns the running notify debouncers and exposes a tokio receiver that the
/// app loop drains. Dropping the handle stops every underlying watcher.
///
/// The `matcher` field is a shared, mutable view of the
/// [`BaselineMatcherInner`] that every debouncer callback consults on
/// each event. Holding it in an `Arc<RwLock<_>>` lets the app layer
/// reconfigure the tracked branch at runtime (e.g. after `R` detects
/// a `git checkout` to a different branch) without rebuilding the
/// debouncers or losing the event queue. See [`Self::update_current_branch_ref`]
/// and ADR-0008.
pub struct WatchHandle {
    pub events: UnboundedReceiver<WatchEvent>,
    /// Shared baseline path matcher. The debouncer closures hold a
    /// clone of this `Arc`; writes through the handle are visible to
    /// the next event without any restart.
    matcher: SharedMatcher,
    /// Per-worktree git dir, stashed so `update_current_branch_ref`
    /// can rebuild `BaselineMatcherInner` without the caller having
    /// to re-plumb it.
    git_dir: PathBuf,
    /// Common git dir (equal to `git_dir` for normal repos, different
    /// for linked worktrees). Same rationale as `git_dir`.
    common_git_dir: PathBuf,
    /// Worktree root, kept for `refresh_worktree_watches`.
    worktree_root: PathBuf,
    /// Set of top-level child directories that already have recursive
    /// watches. `refresh_worktree_watches` adds any new ones.
    watched_children: std::collections::HashSet<PathBuf>,
    // The debouncers must outlive `events`; dropping them stops the watchers.
    worktree_debouncer: KizuDebouncer,
    _git_state: Vec<KizuDebouncer>,
    /// Events dir debouncer for stream mode. `None` if the events
    /// directory could not be resolved or created.
    _events_debouncer: Option<KizuDebouncer>,
}

impl WatchHandle {
    /// Atomically reconfigure the set of baseline-affecting paths the
    /// debouncers match against. Called by the app layer when `R`
    /// discovers that the symbolic HEAD now points at a different
    /// branch than the one captured at startup — without this the
    /// matcher stays pinned to the old branch ref and subsequent
    /// commits on the new branch would silently stop raising
    /// `GitHead` (the core correctness break that ADR-0008 addresses).
    ///
    /// Passing the same branch that is already active is a cheap
    /// no-op: the rebuilt `BaselineMatcherInner` holds identical
    /// canonicalized paths.
    pub fn update_current_branch_ref(&self, current_branch_ref: Option<&str>) {
        let new_inner =
            BaselineMatcherInner::new(&self.git_dir, &self.common_git_dir, current_branch_ref);
        if let Ok(mut guard) = self.matcher.write() {
            *guard = new_inner;
        }
    }

    /// Re-scan the worktree root for new top-level directories and
    /// add recursive watches for any that appeared since the last scan.
    /// Called by the app after `WatchEvent::Worktree` to close the
    /// blind spot where a directory created after startup would not be
    /// watched recursively.
    pub fn refresh_worktree_watches(&mut self) {
        let children = match recursive_worktree_children(&self.worktree_root) {
            Ok(c) => c,
            Err(_) => return,
        };
        for child in children {
            if self.watched_children.contains(&child) {
                continue;
            }
            if self
                .worktree_debouncer
                .watch(&child, RecursiveMode::Recursive)
                .is_ok()
            {
                self.watched_children.insert(child);
            }
        }
    }
}

/// Start watching `root` (the worktree), the per-worktree `git_dir`
/// (resolved via `git rev-parse --absolute-git-dir`, see ADR-0005), and
/// the `common_git_dir` (`git rev-parse --git-common-dir`).
///
/// `current_branch_ref` is the full ref name HEAD currently points at
/// (for example `refs/heads/main`), or `None` when HEAD is detached.
/// It is the single most important input for false-positive control:
/// the watcher only raises `GitHead` when the active branch ref, the
/// per-worktree `HEAD`, or the common `packed-refs` file is touched.
/// Unrelated ref activity (`git fetch` writing `refs/remotes/*`, a
/// tag write, another linked worktree committing to a sibling branch)
/// is ignored. A stale `current_branch_ref` is harmless: the watcher
/// will simply miss a new branch the session was not started on,
/// which is the correct behavior for a frozen baseline.
///
/// For a normal repository `git_dir == common_git_dir` and only one
/// git-dir watcher is spawned. For a **linked worktree** the two
/// differ — `git_dir` is `.git/worktrees/<name>/` and `common_git_dir`
/// is the main `.git/`. Branch refs (`refs/heads/**`, `packed-refs`)
/// live in the common dir, so `git commit` inside the linked worktree
/// would otherwise be invisible to the watcher. Watching both lets
/// any HEAD/refs movement raise `WatchEvent::GitHead`.
///
/// The worktree watcher swallows any event whose paths all sit inside
/// `git_dir`, so git's own bookkeeping can't trigger a recompute storm.
pub fn start(
    root: &Path,
    git_dir: &Path,
    common_git_dir: &Path,
    current_branch_ref: Option<&str>,
) -> Result<WatchHandle> {
    let (tx, rx) = unbounded_channel::<WatchEvent>();

    let worktree_root = root.to_path_buf();
    let git_dir_owned = git_dir.to_path_buf();
    let common_git_dir_owned = common_git_dir.to_path_buf();
    let matcher: SharedMatcher = Arc::new(RwLock::new(BaselineMatcherInner::new(
        &git_dir_owned,
        &common_git_dir_owned,
        current_branch_ref,
    )));

    let (worktree_debouncer, initial_children) =
        spawn_worktree_debouncer(&worktree_root, &git_dir_owned, tx.clone())?;
    let mut git_state = Vec::new();
    for watch_root in git_state_watch_roots(&git_dir_owned, &common_git_dir_owned) {
        git_state.push(spawn_git_state_debouncer(
            &watch_root,
            Arc::clone(&matcher),
            tx.clone(),
        )?);
    }

    // Stream mode: watch the events directory for new event-log files.
    let events_debouncer = spawn_events_dir_debouncer(&worktree_root, tx.clone());

    Ok(WatchHandle {
        events: rx,
        matcher,
        git_dir: git_dir_owned,
        common_git_dir: common_git_dir_owned,
        worktree_root,
        watched_children: initial_children,
        worktree_debouncer,
        _git_state: git_state,
        _events_debouncer: events_debouncer,
    })
}

#[cfg(test)]
mod tests;
