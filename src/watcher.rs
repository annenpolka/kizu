use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;

use notify::RecursiveMode;
use notify_debouncer_full::{
    DebounceEventResult, Debouncer, RecommendedCache, new_debouncer, notify::RecommendedWatcher,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// Worktree debounce window (SPEC.md).
const WORKTREE_DEBOUNCE: Duration = Duration::from_millis(300);
/// `<git_dir>` debounce window (SPEC.md). HEAD/refs move much less often than
/// random worktree edits, so we keep the window short.
const HEAD_DEBOUNCE: Duration = Duration::from_millis(100);

/// A coarse classification of file system activity that the app loop cares
/// about. The actual diff recompute is driven from these signals; we don't
/// pass payloads on this channel because the app always re-runs `git diff`
/// after coalescing (ADR-0005).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchEvent {
    /// Something inside the worktree (excluding `<git_dir>`) changed.
    Worktree,
    /// Something inside `<git_dir>` changed (HEAD, refs, packed refs, …).
    GitHead,
}

/// Owns the running notify debouncers and exposes a tokio receiver that the
/// app loop drains. Dropping the handle stops both watchers.
pub struct WatchHandle {
    pub events: UnboundedReceiver<WatchEvent>,
    // The debouncers must outlive `events`; dropping them stops the watchers.
    _worktree: Debouncer<RecommendedWatcher, RecommendedCache>,
    _git_dir: Debouncer<RecommendedWatcher, RecommendedCache>,
}

/// Start watching `root` (the worktree) and `git_dir` (resolved via
/// `git rev-parse --absolute-git-dir`, see ADR-0005).
///
/// The worktree watcher swallows any event whose paths all sit inside
/// `git_dir`, so git's own bookkeeping can't trigger a recompute storm.
/// The git_dir watcher fires on HEAD/refs movement.
pub fn start(root: &Path, git_dir: &Path) -> Result<WatchHandle> {
    let (tx, rx) = unbounded_channel::<WatchEvent>();

    let worktree_root = root.to_path_buf();
    let git_dir_owned = git_dir.to_path_buf();
    let worktree = spawn_worktree_debouncer(&worktree_root, &git_dir_owned, tx.clone())?;
    let git_dir_watcher = spawn_git_dir_debouncer(&git_dir_owned, tx)?;

    Ok(WatchHandle {
        events: rx,
        _worktree: worktree,
        _git_dir: git_dir_watcher,
    })
}

fn spawn_worktree_debouncer(
    root: &Path,
    git_dir: &Path,
    tx: UnboundedSender<WatchEvent>,
) -> Result<Debouncer<RecommendedWatcher, RecommendedCache>> {
    let git_dir = git_dir.to_path_buf();
    let mut debouncer = new_debouncer(
        WORKTREE_DEBOUNCE,
        None,
        move |result: DebounceEventResult| {
            let Ok(events) = result else {
                // Errors are surfaced separately by notify; the app layer
                // can't act on them yet, so just drop them in v0.1.
                return;
            };
            // If every path on every event lives inside `git_dir`, swallow
            // the burst — that's git churning its own bookkeeping. As soon
            // as one path is outside `git_dir`, we wake the app loop.
            let touches_worktree = events
                .iter()
                .any(|ev| ev.event.paths.iter().any(|p| !is_inside(p, &git_dir)));
            if touches_worktree {
                let _ = tx.send(WatchEvent::Worktree);
            }
        },
    )
    .context("failed to create worktree debouncer")?;

    debouncer
        .watch(root, RecursiveMode::Recursive)
        .with_context(|| format!("failed to watch worktree at {}", root.display()))?;
    Ok(debouncer)
}

fn spawn_git_dir_debouncer(
    git_dir: &Path,
    tx: UnboundedSender<WatchEvent>,
) -> Result<Debouncer<RecommendedWatcher, RecommendedCache>> {
    let git_dir_owned = git_dir.to_path_buf();
    let mut debouncer = new_debouncer(HEAD_DEBOUNCE, None, move |result: DebounceEventResult| {
        let Ok(events) = result else {
            return;
        };
        // Only treat baseline-affecting paths (HEAD, packed-refs, refs/**,
        // linked-worktree HEADs) as real head movement. Bookkeeping churn
        // like `index`, `index.lock`, `logs/`, pack files, or reflog writes
        // must not raise the stale-baseline indicator — otherwise a plain
        // `git add` (or even the tool's own shell-outs refreshing index
        // metadata) would wrongly flag HEAD as drifted.
        let baseline_touched = events.iter().any(|ev| {
            ev.event
                .paths
                .iter()
                .any(|p| is_baseline_path(p, &git_dir_owned))
        });
        if baseline_touched {
            let _ = tx.send(WatchEvent::GitHead);
        }
    })
    .context("failed to create git_dir debouncer")?;

    debouncer
        .watch(git_dir, RecursiveMode::Recursive)
        .with_context(|| format!("failed to watch git_dir at {}", git_dir.display()))?;
    Ok(debouncer)
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

fn canonicalize_or_self(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

/// Classify `path` as a baseline-affecting path inside `git_dir`.
///
/// Returns `true` only when `path` is one of:
/// - `<git_dir>/HEAD`
/// - `<git_dir>/packed-refs`
/// - `<git_dir>/refs/**`
/// - `<git_dir>/worktrees/*/HEAD` (linked-worktree HEAD)
///
/// Plain `.git/index`, `.git/index.lock`, `.git/logs/**`, `.git/objects/**`,
/// `.git/COMMIT_EDITMSG`, `.git/ORIG_HEAD`, `.git/FETCH_HEAD`, and any ad-hoc
/// marker files are rejected so they never raise the stale-baseline signal.
fn is_baseline_path(path: &Path, git_dir: &Path) -> bool {
    let p = canonicalize_or_self(path);
    let g = canonicalize_or_self(git_dir);
    let Ok(rel) = p.strip_prefix(&g) else {
        return false;
    };
    let parts: Vec<&std::ffi::OsStr> = rel.iter().collect();
    match parts.as_slice() {
        // `.git/HEAD`
        [name] if *name == "HEAD" => true,
        // `.git/packed-refs`
        [name] if *name == "packed-refs" => true,
        // `.git/refs/**` — any file under the refs subtree.
        [head, ..] if *head == "refs" => true,
        // `.git/worktrees/<name>/HEAD` — linked-worktree HEAD. Other files
        // under `worktrees/<name>/` (commondir, gitdir, locked, …) are
        // still rejected because they don't move the baseline SHA.
        [head, _, tail] if *head == "worktrees" && *tail == "HEAD" => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;
    use tokio::time::{Duration as TokioDuration, timeout};

    /// Build a fresh git repo in a tempdir so HEAD/refs are real and
    /// `git rev-parse --absolute-git-dir` resolves to a watchable directory.
    fn init_repo() -> TempDir {
        let dir = tempfile::tempdir().expect("create tempdir");
        run_git(dir.path(), &["init", "--quiet", "--initial-branch=main"]);
        run_git(dir.path(), &["config", "user.email", "test@example.com"]);
        run_git(dir.path(), &["config", "user.name", "kizu test"]);
        dir
    }

    fn run_git(cwd: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .status()
            .unwrap_or_else(|e| panic!("git {args:?} failed to spawn: {e}"));
        assert!(status.success(), "git {args:?} exited with {status:?}");
    }

    /// Wait long enough for a debouncer cycle to elapse, since the worktree
    /// debounce is 300 ms — anything shorter is racy.
    const DRAIN_WAIT: TokioDuration = TokioDuration::from_millis(2_000);

    #[tokio::test(flavor = "current_thread")]
    async fn worktree_event_is_received_for_a_new_file() {
        let repo = init_repo();
        let root = crate::git::find_root(repo.path()).expect("find_root");
        let git_dir = crate::git::git_dir(&root).expect("git_dir");

        let mut handle = start(&root, &git_dir).expect("start watcher");

        // Give the debouncer a moment to install its OS hook before we touch
        // the worktree, otherwise the create event can land before notify is
        // listening.
        tokio::time::sleep(TokioDuration::from_millis(150)).await;
        fs::write(root.join("hello.txt"), "hello\n").expect("write file");

        let event = timeout(DRAIN_WAIT, handle.events.recv())
            .await
            .expect("worktree event arrived")
            .expect("channel still open");
        assert_eq!(event, WatchEvent::Worktree);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn writes_inside_git_dir_do_not_emit_worktree_event() {
        let repo = init_repo();
        let root = crate::git::find_root(repo.path()).expect("find_root");
        let git_dir = crate::git::git_dir(&root).expect("git_dir");

        let mut handle = start(&root, &git_dir).expect("start watcher");

        tokio::time::sleep(TokioDuration::from_millis(150)).await;
        // Drop a file inside .git/ to mimic git's own bookkeeping. notify
        // should fire — but the worktree filter must swallow it, and since
        // the path is not HEAD/refs/packed-refs the git_dir watcher must
        // also stay silent.
        fs::write(git_dir.join("kizu_test_marker"), b"x").expect("write inside git_dir");

        // Drain whatever shows up within the debounce window. We must not
        // see either event type: non-baseline writes inside `.git/` are
        // git's own bookkeeping and should be completely swallowed.
        let mut saw_worktree = false;
        let mut saw_head = false;
        let drain_until = tokio::time::Instant::now() + DRAIN_WAIT;
        while tokio::time::Instant::now() < drain_until {
            match timeout(TokioDuration::from_millis(200), handle.events.recv()).await {
                Ok(Some(WatchEvent::Worktree)) => {
                    saw_worktree = true;
                    break;
                }
                Ok(Some(WatchEvent::GitHead)) => {
                    saw_head = true;
                    break;
                }
                Ok(None) => break,
                Err(_) => continue, // recv timed out, keep draining
            }
        }
        assert!(
            !saw_worktree,
            "git_dir-only writes must not surface as Worktree events"
        );
        assert!(
            !saw_head,
            "non-HEAD/refs writes inside git_dir must not surface as GitHead"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn writing_refs_heads_inside_git_dir_emits_head_event() {
        let repo = init_repo();
        let root = crate::git::find_root(repo.path()).expect("find_root");
        let git_dir = crate::git::git_dir(&root).expect("git_dir");

        let mut handle = start(&root, &git_dir).expect("start watcher");

        tokio::time::sleep(TokioDuration::from_millis(150)).await;
        // Simulate a real baseline move: create a ref under refs/heads/.
        // The content doesn't have to be valid — we're only exercising the
        // watcher's path classifier, not git itself.
        let refs_heads = git_dir.join("refs").join("heads");
        fs::create_dir_all(&refs_heads).expect("create refs/heads");
        fs::write(
            refs_heads.join("kizu-test-branch"),
            b"0000000000000000000000000000000000000000\n",
        )
        .expect("write ref");

        // We expect a GitHead within the drain window. Worktree events are
        // fine (the refs path also sits inside git_dir so worktree filter
        // should still swallow them, but we don't fail the test on stray
        // Worktree signals).
        let mut saw_head = false;
        let drain_until = tokio::time::Instant::now() + DRAIN_WAIT;
        while tokio::time::Instant::now() < drain_until {
            match timeout(TokioDuration::from_millis(200), handle.events.recv()).await {
                Ok(Some(WatchEvent::GitHead)) => {
                    saw_head = true;
                    break;
                }
                Ok(Some(WatchEvent::Worktree)) => continue,
                Ok(None) => break,
                Err(_) => continue,
            }
        }
        assert!(saw_head, "writes under refs/heads must emit GitHead");
    }

    #[test]
    fn is_baseline_path_matches_head_and_refs() {
        let git_dir = Path::new("/tmp/repo/.git");
        assert!(is_baseline_path(&git_dir.join("HEAD"), git_dir));
        assert!(is_baseline_path(&git_dir.join("packed-refs"), git_dir));
        assert!(is_baseline_path(
            &git_dir.join("refs").join("heads").join("main"),
            git_dir
        ));
        assert!(is_baseline_path(
            &git_dir.join("refs").join("tags").join("v1.0"),
            git_dir
        ));
        // Linked worktree HEAD
        assert!(is_baseline_path(
            &git_dir.join("worktrees").join("wt1").join("HEAD"),
            git_dir
        ));
    }

    #[test]
    fn is_baseline_path_rejects_bookkeeping_paths() {
        let git_dir = Path::new("/tmp/repo/.git");
        assert!(!is_baseline_path(&git_dir.join("index"), git_dir));
        assert!(!is_baseline_path(&git_dir.join("index.lock"), git_dir));
        assert!(!is_baseline_path(
            &git_dir.join("logs").join("HEAD"),
            git_dir
        ));
        assert!(!is_baseline_path(
            &git_dir.join("objects").join("pack").join("pack-abc.idx"),
            git_dir
        ));
        assert!(!is_baseline_path(&git_dir.join("COMMIT_EDITMSG"), git_dir));
        assert!(!is_baseline_path(&git_dir.join("ORIG_HEAD"), git_dir));
        assert!(!is_baseline_path(&git_dir.join("FETCH_HEAD"), git_dir));
    }
}
