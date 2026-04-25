use anyhow::{Context, Result};
use std::path::Path;
use std::time::Duration;

use notify::RecursiveMode;
use notify_debouncer_full::DebounceEventResult;
use tokio::sync::mpsc::UnboundedSender;

use super::backend::{KizuDebouncer, format_notify_errors, new_kizu_debouncer};
use super::matcher::SharedMatcher;
use super::{WatchEvent, WatchRoot, WatchSource};

/// `<git_dir>` debounce window (SPEC.md). HEAD/refs move much less often than
/// random worktree edits, so we keep the window short.
const HEAD_DEBOUNCE: Duration = Duration::from_millis(100);

pub(in crate::watcher) fn git_state_watch_roots(
    git_dir: &Path,
    common_git_dir: &Path,
) -> Vec<WatchRoot> {
    vec![
        WatchRoot {
            path: git_dir.join("HEAD"),
            recursive_mode: RecursiveMode::NonRecursive,
            compare_contents: true,
            source: WatchSource::GitPerWorktreeHead,
        },
        // Branch refs live under the shared `refs/` tree. Watching only
        // this subtree keeps the poll fallback off `.git/objects/**`
        // while still catching new branch files and nested branch names.
        WatchRoot {
            path: common_git_dir.join("refs"),
            recursive_mode: RecursiveMode::Recursive,
            compare_contents: true,
            source: WatchSource::GitRefs,
        },
        // Watch the common git-dir root non-recursively so `packed-refs`
        // is covered whether it exists at startup or is created later.
        // Keeping this non-recursive avoids polling `objects/**` while
        // still tracking root-level files like `packed-refs`.
        WatchRoot {
            path: common_git_dir.to_path_buf(),
            recursive_mode: RecursiveMode::NonRecursive,
            compare_contents: true,
            source: WatchSource::GitCommonRoot,
        },
    ]
}

pub(in crate::watcher) fn spawn_git_state_debouncer(
    watch_root: &WatchRoot,
    matcher: SharedMatcher,
    tx: UnboundedSender<WatchEvent>,
) -> Result<KizuDebouncer> {
    let source = watch_root.source;
    let compare_contents = watch_root.compare_contents;
    let mut debouncer = new_kizu_debouncer(
        HEAD_DEBOUNCE,
        compare_contents,
        move |result: DebounceEventResult| {
            let events = match result {
                Ok(events) => events,
                Err(errors) => {
                    let message = format_notify_errors(source, &errors);
                    let _ = tx.send(WatchEvent::Error { source, message });
                    return;
                }
            };
            // Read-lock the shared matcher once per burst. `R` may have
            // hot-swapped the inner value since the previous firing (the
            // user checked out a different branch and re-baselined), so
            // we always read through the Arc rather than capturing a
            // snapshot in the closure.
            //
            // Only treat baseline-affecting paths (the per-worktree HEAD,
            // the common-dir branch ref the session is currently tracking,
            // packed-refs) as real head movement. Plain bookkeeping churn
            // — `index`, `index.lock`, `logs/`, pack files, reflog writes
            // — and unrelated refs (remotes, tags, other branches) must
            // not raise the stale-baseline indicator, otherwise a
            // `git fetch` or a sibling linked worktree's commit would
            // wrongly flag our HEAD as drifted.
            let Ok(guard) = matcher.read() else {
                // Poisoned RwLock: refuse to swallow the burst silently —
                // bubble a health-level error so the app layer forces a
                // recompute and marks the watcher unhealthy.
                let _ = tx.send(WatchEvent::Error {
                    source,
                    message: format!(
                        "watcher [{}]: baseline matcher lock poisoned",
                        source.label()
                    ),
                });
                return;
            };
            let baseline_touched = events
                .iter()
                .any(|ev| ev.event.paths.iter().any(|p| guard.matches(p)));
            drop(guard);
            if baseline_touched {
                let _ = tx.send(WatchEvent::GitHead(source));
            }
        },
    )
    .context("failed to create git_dir debouncer")?;

    debouncer
        .watch(&watch_root.path, watch_root.recursive_mode)
        .with_context(|| format!("failed to watch git_dir at {}", watch_root.path.display()))?;
    Ok(debouncer)
}
