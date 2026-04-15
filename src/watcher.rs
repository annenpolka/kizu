use anyhow::Result;
use std::path::Path;
use std::sync::mpsc::Receiver;
use std::time::Duration;

#[derive(Debug, Clone)]
pub enum WatchEvent {
    /// Worktree files changed; recompute diff against the baseline.
    Worktree,
    /// `.git/HEAD` or refs changed; baseline may have shifted.
    GitHead,
}

pub struct WatchHandle {
    pub events: Receiver<WatchEvent>,
}

/// Start watching the worktree and the git internal directory.
///
/// Debounce thresholds (v0.1):
///   - worktree changes: 300ms
///   - .git/HEAD changes: 100ms
///
/// TODO v0.1:
///   - notify_debouncer_full::new_debouncer with WORKTREE_DEBOUNCE
///   - filter out paths matched by .gitignore (use git check-ignore initially,
///     replace with in-process matcher later — diffpane TODO note applies)
///   - emit WatchEvent::Worktree / WatchEvent::GitHead via mpsc::Sender
pub fn start(_root: &Path) -> Result<WatchHandle> {
    let (_tx, rx) = std::sync::mpsc::channel();
    let _worktree_debounce = Duration::from_millis(300);
    let _head_debounce = Duration::from_millis(100);
    Ok(WatchHandle { events: rx })
}
