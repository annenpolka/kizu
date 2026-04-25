use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;

use notify::RecursiveMode;
use notify_debouncer_full::DebounceEventResult;
use tokio::sync::mpsc::UnboundedSender;

use super::backend::{KizuDebouncer, format_notify_errors, new_kizu_debouncer};
use super::matcher::canonicalize_or_self;
use super::{WatchEvent, WatchSource};

/// Worktree debounce window (SPEC.md).
const WORKTREE_DEBOUNCE: Duration = Duration::from_millis(300);
/// Top-level worktree directories we intentionally do not recurse into.
/// These are common dependency caches / build outputs whose initial poll scan
/// is far more expensive than the value they provide to the v0.1 diff UI.
const WORKTREE_EXCLUDED_DIR_NAMES: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    ".direnv",
    ".venv",
    "dist",
    "build",
    ".next",
    ".turbo",
    ".cache",
    ".gradle",
    ".mvn",
    ".idea",
    ".vscode",
    "__pycache__",
];

pub(in crate::watcher) fn spawn_worktree_debouncer(
    root: &Path,
    git_dir: &Path,
    tx: UnboundedSender<WatchEvent>,
) -> Result<(KizuDebouncer, std::collections::HashSet<PathBuf>)> {
    let git_dir = git_dir.to_path_buf();
    // Pre-resolve excluded directory paths so the callback can filter
    // cheaply. On Linux inotify, a non-recursive root watch still
    // reports mtime changes on excluded child directories (e.g. when
    // a file inside target/ is written, target/'s mtime updates and
    // inotify fires on the root watch). Without this filter, those
    // events leak through as Worktree and cause spurious recomputes.
    let excluded_dirs: Vec<PathBuf> = WORKTREE_EXCLUDED_DIR_NAMES
        .iter()
        .map(|name| root.join(name))
        .collect();
    let callback_tx = tx.clone();
    let mut debouncer = new_kizu_debouncer(
        WORKTREE_DEBOUNCE,
        true,
        move |result: DebounceEventResult| {
            let events = match result {
                Ok(events) => events,
                Err(errors) => {
                    let message = format_notify_errors(WatchSource::Worktree, &errors);
                    let _ = callback_tx.send(WatchEvent::Error {
                        source: WatchSource::Worktree,
                        message,
                    });
                    return;
                }
            };
            // Swallow events whose paths all live inside git_dir or an
            // excluded directory (target/, node_modules/, etc.). Only
            // wake the app loop when a genuine worktree path is touched.
            let dominated = |p: &Path| {
                is_inside(p, &git_dir) || excluded_dirs.iter().any(|excl| is_inside(p, excl))
            };
            let touches_worktree = events
                .iter()
                .any(|ev| ev.event.paths.iter().any(|p| !dominated(p)));
            if touches_worktree {
                let _ = callback_tx.send(WatchEvent::Worktree);
            }
        },
    )
    .context("failed to create worktree debouncer")?;

    debouncer
        .watch(root, RecursiveMode::NonRecursive)
        .with_context(|| format!("failed to watch worktree at {}", root.display()))?;

    let recursive_children = match recursive_worktree_children(root) {
        Ok(children) => children,
        Err(err) => {
            let _ = tx.send(WatchEvent::Error {
                source: WatchSource::Worktree,
                message: format!("watcher [{}]: {err:#}", WatchSource::Worktree.label()),
            });
            return Ok((debouncer, std::collections::HashSet::new()));
        }
    };

    let mut watched = std::collections::HashSet::with_capacity(recursive_children.len());
    for child in recursive_children {
        debouncer
            .watch(&child, RecursiveMode::Recursive)
            .with_context(|| format!("failed to watch worktree at {}", child.display()))?;
        watched.insert(child);
    }
    Ok((debouncer, watched))
}

pub(in crate::watcher) fn recursive_worktree_children(root: &Path) -> Result<Vec<PathBuf>> {
    let mut children = Vec::new();
    let entries = std::fs::read_dir(root)
        .with_context(|| format!("failed to read worktree root {}", root.display()))?;
    for entry in entries {
        let entry = entry
            .with_context(|| format!("failed to enumerate worktree root {}", root.display()))?;
        if entry
            .file_name()
            .to_str()
            .is_some_and(is_excluded_worktree_dir_name)
        {
            continue;
        }

        let path = entry.path();
        if path.is_dir() {
            children.push(path);
        }
    }
    children.sort();
    Ok(children)
}

fn is_excluded_worktree_dir_name(name: &str) -> bool {
    WORKTREE_EXCLUDED_DIR_NAMES.contains(&name)
}

/// Return true when `path` is `git_dir` itself or any descendant of it.
/// Both sides are canonicalized when possible so that symlink-y temp
/// directories on macOS (`/var/folders` vs `/private/var/folders`) compare
/// correctly.
fn is_inside(path: &Path, git_dir: &Path) -> bool {
    let p = canonicalize_or_self(path);
    let g = canonicalize_or_self(git_dir);
    p.starts_with(&g)
}
