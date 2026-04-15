use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
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
    /// Something inside `<git_dir>` changed (HEAD, refs, packed refs, …).
    GitHead,
    /// The underlying notify backend reported an error. The app
    /// treats this as a forced recompute plus a visible error string.
    Error(String),
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
    // The debouncers must outlive `events`; dropping them stops the watchers.
    _worktree: Debouncer<RecommendedWatcher, RecommendedCache>,
    _git_dir: Debouncer<RecommendedWatcher, RecommendedCache>,
    // Only set when the repository is a linked worktree — the common
    // git dir lives elsewhere and holds the shared `refs/heads/` tree
    // that actually moves when `git commit` runs. When the common dir
    // matches `git_dir` (normal repos) we skip the second watcher to
    // avoid double-firing GitHead.
    _common_git_dir: Option<Debouncer<RecommendedWatcher, RecommendedCache>>,
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

    let worktree = spawn_worktree_debouncer(&worktree_root, &git_dir_owned, tx.clone())?;
    let git_dir_watcher =
        spawn_git_dir_debouncer(&git_dir_owned, Arc::clone(&matcher), tx.clone())?;

    // Only spin up a second watcher when the common dir really differs
    // from the per-worktree dir; otherwise we'd double-fire GitHead on
    // every HEAD/ref write in a normal repo.
    let common_git_dir_watcher =
        if canonicalize_or_self(&common_git_dir_owned) == canonicalize_or_self(&git_dir_owned) {
            None
        } else {
            Some(spawn_git_dir_debouncer(
                &common_git_dir_owned,
                Arc::clone(&matcher),
                tx,
            )?)
        };

    Ok(WatchHandle {
        events: rx,
        matcher,
        git_dir: git_dir_owned,
        common_git_dir: common_git_dir_owned,
        _worktree: worktree,
        _git_dir: git_dir_watcher,
        _common_git_dir: common_git_dir_watcher,
    })
}

/// Shared, runtime-mutable handle to the baseline path set. Every
/// debouncer callback holds a clone of this `Arc` and read-locks on
/// each event; the app layer can hot-swap the inner value through
/// [`WatchHandle::update_current_branch_ref`].
pub(crate) type SharedMatcher = Arc<RwLock<BaselineMatcherInner>>;

/// Set of git-dir paths that, when touched, genuinely indicate the
/// session baseline SHA has drifted. Captured at watcher startup
/// **and refreshed at runtime** whenever `R` discovers a new
/// symbolic HEAD (ADR-0008). Paths are canonicalized so byte
/// comparisons work across symlinked tempdirs (e.g. macOS
/// `/var/folders` → `/private/var/folders`).
#[derive(Debug, Clone)]
pub(crate) struct BaselineMatcherInner {
    /// `<per-worktree git_dir>/HEAD` — moves on `git checkout`, or
    /// on reseating HEAD to a different branch via `symbolic-ref`.
    head_file: PathBuf,
    /// `<common git_dir>/refs/heads/<current branch>` — moves on
    /// `git commit`, `git reset`, or any direct ref write. `None`
    /// when HEAD is detached: in that case the session baseline is
    /// a raw SHA and only `head_file` can move it (via checkout).
    branch_ref: Option<PathBuf>,
    /// `<common git_dir>/packed-refs` — touched when loose refs get
    /// packed, which can atomically replace the loose branch ref
    /// file with an entry inside packed-refs. Tracking this catches
    /// the corner case where a `git pack-refs` happens between two
    /// HEAD movements.
    packed_refs: PathBuf,
}

impl BaselineMatcherInner {
    pub(crate) fn new(
        git_dir: &Path,
        common_git_dir: &Path,
        current_branch_ref: Option<&str>,
    ) -> Self {
        let head_file = canonicalize_or_self(&git_dir.join("HEAD"));
        let branch_ref = current_branch_ref.map(|r| {
            // `r` looks like `refs/heads/foo/bar` — split on `/` and
            // join to preserve nested branch names on platforms where
            // path joining with a multi-segment string works differently.
            let mut p = common_git_dir.to_path_buf();
            for segment in r.split('/') {
                p.push(segment);
            }
            canonicalize_or_self(&p)
        });
        let packed_refs = canonicalize_or_self(&common_git_dir.join("packed-refs"));
        Self {
            head_file,
            branch_ref,
            packed_refs,
        }
    }

    pub(crate) fn matches(&self, path: &Path) -> bool {
        let p = canonicalize_or_self(path);
        p == self.head_file
            || self.branch_ref.as_ref().is_some_and(|r| p == *r)
            || p == self.packed_refs
    }
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
            let events = match result {
                Ok(events) => events,
                Err(errors) => {
                    // Surface backend failures (FSEvents drop, moved
                    // watch target, kqueue overflow, …) so the app
                    // layer can flip the footer to red and force a
                    // recompute instead of silently drifting stale.
                    let msg = format_notify_errors("worktree", &errors);
                    let _ = tx.send(WatchEvent::Error(msg));
                    return;
                }
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
    matcher: SharedMatcher,
    tx: UnboundedSender<WatchEvent>,
) -> Result<Debouncer<RecommendedWatcher, RecommendedCache>> {
    let mut debouncer = new_debouncer(HEAD_DEBOUNCE, None, move |result: DebounceEventResult| {
        let events = match result {
            Ok(events) => events,
            Err(errors) => {
                let msg = format_notify_errors("git_dir", &errors);
                let _ = tx.send(WatchEvent::Error(msg));
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
            let _ = tx.send(WatchEvent::Error(
                "watcher [git_dir]: baseline matcher lock poisoned".to_string(),
            ));
            return;
        };
        let baseline_touched = events
            .iter()
            .any(|ev| ev.event.paths.iter().any(|p| guard.matches(p)));
        drop(guard);
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

/// Format one or more notify errors into the human-readable footer
/// string the app surfaces in `last_error`. Prefixed with the
/// watcher layer so users can tell `worktree` failures apart from
/// `git_dir` failures when triaging.
fn format_notify_errors(layer: &str, errors: &[notify::Error]) -> String {
    let joined = errors
        .iter()
        .map(|e| e.to_string())
        .collect::<Vec<_>>()
        .join("; ");
    if joined.is_empty() {
        format!("watcher [{layer}]: unknown backend failure")
    } else {
        format!("watcher [{layer}]: {joined}")
    }
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

pub(crate) fn canonicalize_or_self(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
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
        let common = crate::git::git_common_dir(&root).expect("common git_dir");
        let branch = crate::git::current_branch_ref(&root).expect("current branch");

        let mut handle = start(&root, &git_dir, &common, branch.as_deref()).expect("start watcher");

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
        let common = crate::git::git_common_dir(&root).expect("common git_dir");
        let branch = crate::git::current_branch_ref(&root).expect("current branch");

        let mut handle = start(&root, &git_dir, &common, branch.as_deref()).expect("start watcher");

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
                Ok(Some(WatchEvent::Error(_))) => continue,
                Ok(None) => break,
                Err(_) => continue,
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
    async fn writing_current_branch_ref_emits_head_event() {
        // The active-branch-only narrowing still has to fire for the
        // session's own branch. Create a ref under refs/heads/<branch>
        // and verify GitHead lands; the next test verifies that an
        // unrelated ref DOES NOT fire.
        let repo = init_repo();
        let root = crate::git::find_root(repo.path()).expect("find_root");
        let git_dir = crate::git::git_dir(&root).expect("git_dir");
        let common = crate::git::git_common_dir(&root).expect("common git_dir");

        // `git init --initial-branch=main` leaves HEAD pointing at
        // `refs/heads/main`, but the branch ref file does not exist
        // until the first commit. Pretend the session's branch is
        // `kizu-test-branch` so we can watch its birth and drive the
        // event from a direct file write (no `git commit` rigging).
        let mut handle = start(
            &root,
            &git_dir,
            &common,
            Some("refs/heads/kizu-test-branch"),
        )
        .expect("start watcher");

        tokio::time::sleep(TokioDuration::from_millis(150)).await;
        let refs_heads = git_dir.join("refs").join("heads");
        fs::create_dir_all(&refs_heads).expect("create refs/heads");
        fs::write(
            refs_heads.join("kizu-test-branch"),
            b"0000000000000000000000000000000000000000\n",
        )
        .expect("write ref");

        let mut saw_head = false;
        let drain_until = tokio::time::Instant::now() + DRAIN_WAIT;
        while tokio::time::Instant::now() < drain_until {
            match timeout(TokioDuration::from_millis(200), handle.events.recv()).await {
                Ok(Some(WatchEvent::GitHead)) => {
                    saw_head = true;
                    break;
                }
                Ok(Some(WatchEvent::Worktree)) => continue,
                Ok(Some(WatchEvent::Error(_))) => continue,
                Ok(None) => break,
                Err(_) => continue,
            }
        }
        assert!(
            saw_head,
            "writes under the session's own refs/heads/<branch> must emit GitHead"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn writing_unrelated_refs_does_not_emit_head_event() {
        // Adversarial finding: the previous matcher treated every
        // `refs/**` path as baseline-affecting, so a plain `git fetch`
        // updating `refs/remotes/*` would wrongly fire GitHead and
        // push users to re-baseline. With the narrowed matcher only
        // the session's own branch ref should count.
        let repo = init_repo();
        let root = crate::git::find_root(repo.path()).expect("find_root");
        let git_dir = crate::git::git_dir(&root).expect("git_dir");
        let common = crate::git::git_common_dir(&root).expect("common git_dir");

        let mut handle = start(&root, &git_dir, &common, Some("refs/heads/main"))
            .expect("start watcher with main as active branch");

        tokio::time::sleep(TokioDuration::from_millis(150)).await;
        // Write a sibling branch, a remote ref, and a tag — none of
        // which the session is tracking. The matcher must reject all
        // three.
        let refs_heads = git_dir.join("refs").join("heads");
        fs::create_dir_all(&refs_heads).expect("create refs/heads");
        fs::write(
            refs_heads.join("sibling-branch"),
            b"0000000000000000000000000000000000000000\n",
        )
        .expect("write sibling");
        let refs_remotes = git_dir.join("refs").join("remotes").join("origin");
        fs::create_dir_all(&refs_remotes).expect("create refs/remotes/origin");
        fs::write(
            refs_remotes.join("feature"),
            b"0000000000000000000000000000000000000000\n",
        )
        .expect("write remote ref");
        let refs_tags = git_dir.join("refs").join("tags");
        fs::create_dir_all(&refs_tags).expect("create refs/tags");
        fs::write(
            refs_tags.join("v1.0"),
            b"0000000000000000000000000000000000000000\n",
        )
        .expect("write tag");

        let mut saw_head = false;
        let drain_until = tokio::time::Instant::now() + DRAIN_WAIT;
        while tokio::time::Instant::now() < drain_until {
            match timeout(TokioDuration::from_millis(200), handle.events.recv()).await {
                Ok(Some(WatchEvent::GitHead)) => {
                    saw_head = true;
                    break;
                }
                Ok(Some(WatchEvent::Worktree)) => continue,
                Ok(Some(WatchEvent::Error(_))) => continue,
                Ok(None) => break,
                Err(_) => continue,
            }
        }
        assert!(
            !saw_head,
            "unrelated ref activity (sibling branch, remotes, tags) \
             must not raise GitHead under the narrowed matcher"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn linked_worktree_commit_raises_head_event_via_common_git_dir() {
        // Regression for Codex's linked-worktree finding: a commit inside
        // a linked worktree updates `refs/heads/<branch>` in the *common*
        // git dir (the main repo's `.git/`), not in the per-worktree dir
        // that `git rev-parse --absolute-git-dir` returns. If the watcher
        // only looks at the per-worktree dir, the commit never raises
        // GitHead and the UI stays pinned to a stale baseline.
        let main = init_repo();
        // Need an initial commit so we can spin off a branch.
        fs::write(main.path().join("seed.txt"), "seed\n").expect("write seed");
        run_git(main.path(), &["add", "seed.txt"]);
        run_git(main.path(), &["commit", "--quiet", "-m", "init"]);

        // Create a linked worktree at a sibling path. `git worktree add`
        // materializes `main/.git/worktrees/linked/` and a worktree tree
        // whose `.git` file points there.
        let linked_path = main
            .path()
            .parent()
            .expect("tempdir has parent")
            .join(format!("kizu-linked-wt-{}", std::process::id()));
        let _ = fs::remove_dir_all(&linked_path);
        run_git(
            main.path(),
            &[
                "worktree",
                "add",
                "-b",
                "feature-branch",
                linked_path.to_str().expect("linked path utf8"),
            ],
        );

        let linked_root = crate::git::find_root(&linked_path).expect("find_root linked");
        let linked_git_dir =
            crate::git::git_dir(&linked_root).expect("linked per-worktree git_dir");
        let common_git_dir =
            crate::git::git_common_dir(&linked_root).expect("linked common git_dir");
        assert_ne!(
            canonicalize_or_self(&linked_git_dir),
            canonicalize_or_self(&common_git_dir),
            "linked worktree must have distinct per-worktree and common git dirs \
             (got both = {})",
            linked_git_dir.display()
        );

        // Linked worktree starts on `feature-branch`, resolve that
        // at runtime instead of hard-coding.
        let linked_branch = crate::git::current_branch_ref(&linked_root).expect("linked branch");

        let mut handle = start(
            &linked_root,
            &linked_git_dir,
            &common_git_dir,
            linked_branch.as_deref(),
        )
        .expect("start watcher with common git dir");

        tokio::time::sleep(TokioDuration::from_millis(150)).await;

        // Commit inside the linked worktree. This writes the new commit
        // object + updates `refs/heads/feature-branch` in the common git
        // dir. The per-worktree git dir only gets index/logs churn,
        // which the BaselineMatcher correctly rejects.
        fs::write(linked_root.join("new.txt"), "hi\n").expect("write new");
        run_git(&linked_root, &["add", "new.txt"]);
        run_git(&linked_root, &["commit", "--quiet", "-m", "linked commit"]);

        let mut saw_head = false;
        let drain_until = tokio::time::Instant::now() + DRAIN_WAIT;
        while tokio::time::Instant::now() < drain_until {
            match timeout(TokioDuration::from_millis(200), handle.events.recv()).await {
                Ok(Some(WatchEvent::GitHead)) => {
                    saw_head = true;
                    break;
                }
                Ok(Some(WatchEvent::Worktree)) => continue,
                Ok(Some(WatchEvent::Error(_))) => continue,
                Ok(None) => break,
                Err(_) => continue,
            }
        }
        assert!(
            saw_head,
            "commit in a linked worktree must raise GitHead via the common git dir"
        );

        drop(handle);
        let _ = fs::remove_dir_all(&linked_path);
    }

    #[test]
    fn baseline_matcher_accepts_head_branch_ref_and_packed_refs_only() {
        // The matcher must recognize exactly three path classes: the
        // per-worktree HEAD, the common-dir branch ref the session
        // baseline was captured from, and common-dir packed-refs.
        // Anything else — unrelated refs, remotes, tags, bookkeeping
        // files — must be rejected.
        let git_dir = Path::new("/tmp/repo/.git");
        let matcher = BaselineMatcherInner::new(git_dir, git_dir, Some("refs/heads/main"));

        // Accepted: HEAD, the current branch ref, packed-refs.
        assert!(matcher.matches(&git_dir.join("HEAD")));
        assert!(matcher.matches(&git_dir.join("refs").join("heads").join("main")));
        assert!(matcher.matches(&git_dir.join("packed-refs")));

        // Rejected: unrelated refs.
        assert!(!matcher.matches(&git_dir.join("refs").join("heads").join("feature")));
        assert!(
            !matcher.matches(
                &git_dir
                    .join("refs")
                    .join("remotes")
                    .join("origin")
                    .join("main")
            )
        );
        assert!(!matcher.matches(&git_dir.join("refs").join("tags").join("v1.0")));

        // Rejected: pure bookkeeping.
        assert!(!matcher.matches(&git_dir.join("index")));
        assert!(!matcher.matches(&git_dir.join("index.lock")));
        assert!(!matcher.matches(&git_dir.join("logs").join("HEAD")));
        assert!(!matcher.matches(&git_dir.join("objects").join("pack").join("pack-abc.idx")));
        assert!(!matcher.matches(&git_dir.join("COMMIT_EDITMSG")));
        assert!(!matcher.matches(&git_dir.join("ORIG_HEAD")));
        assert!(!matcher.matches(&git_dir.join("FETCH_HEAD")));
    }

    #[test]
    fn baseline_matcher_detached_head_tracks_head_file_only() {
        // Detached HEAD: no current branch ref, so only the HEAD
        // file and packed-refs matter. Every refs/** path — including
        // what would otherwise have been "our" branch — must be
        // rejected, because in a detached session the baseline is a
        // raw SHA and no branch ref can move it.
        let git_dir = Path::new("/tmp/repo/.git");
        let matcher = BaselineMatcherInner::new(git_dir, git_dir, None);

        assert!(matcher.matches(&git_dir.join("HEAD")));
        assert!(matcher.matches(&git_dir.join("packed-refs")));
        assert!(!matcher.matches(&git_dir.join("refs").join("heads").join("main")));
        assert!(!matcher.matches(&git_dir.join("refs").join("heads").join("feature")));
    }

    #[test]
    fn baseline_matcher_linked_worktree_splits_head_and_branch_ref() {
        // Linked worktree: the per-worktree HEAD lives inside
        // `.git/worktrees/<name>/`, while the branch ref lives under
        // the main repo's `.git/refs/heads/`. The matcher must
        // recognize HEAD in the per-worktree dir and the branch ref
        // in the common dir simultaneously.
        let per = Path::new("/tmp/repo/.git/worktrees/wt1");
        let common = Path::new("/tmp/repo/.git");
        let matcher = BaselineMatcherInner::new(per, common, Some("refs/heads/feature"));

        assert!(matcher.matches(&per.join("HEAD")));
        assert!(matcher.matches(&common.join("refs").join("heads").join("feature")));
        assert!(matcher.matches(&common.join("packed-refs")));
        // HEAD in the common dir (the main worktree's HEAD) must NOT
        // match — a checkout in the main worktree is a different
        // session's concern.
        assert!(!matcher.matches(&common.join("HEAD")));
        // A sibling linked worktree's HEAD file is also unrelated.
        assert!(!matcher.matches(&common.join("worktrees").join("wt2").join("HEAD")));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn update_current_branch_ref_reroutes_head_detection_without_restart() {
        // Regression for Codex round-3 finding: previously the
        // watcher captured the startup branch into an immutable
        // struct, so `R`'ing after a `git checkout` to a different
        // branch silently stopped raising `GitHead` for commits
        // on the new branch. The new design wraps the matcher in
        // an `Arc<RwLock<_>>` so the app layer can hot-swap the
        // tracked branch through `WatchHandle::update_current_branch_ref`.
        //
        // Setup: start watching branch `main`, write to
        // `refs/heads/sibling` (ignored by the matcher), confirm
        // GitHead does NOT fire. Then update the matcher to track
        // `sibling`, write to it again, confirm GitHead fires.
        let repo = init_repo();
        let root = crate::git::find_root(repo.path()).expect("find_root");
        let git_dir = crate::git::git_dir(&root).expect("git_dir");
        let common = crate::git::git_common_dir(&root).expect("common git_dir");

        let mut handle =
            start(&root, &git_dir, &common, Some("refs/heads/main")).expect("start watcher");

        tokio::time::sleep(TokioDuration::from_millis(150)).await;

        // Phase 1: write a sibling branch the matcher is NOT
        // tracking — must be ignored.
        let refs_heads = git_dir.join("refs").join("heads");
        fs::create_dir_all(&refs_heads).expect("create refs/heads");
        fs::write(
            refs_heads.join("sibling"),
            b"1111111111111111111111111111111111111111\n",
        )
        .expect("write sibling phase 1");

        let mut saw_head_before_update = false;
        let phase1_until = tokio::time::Instant::now() + TokioDuration::from_millis(600);
        while tokio::time::Instant::now() < phase1_until {
            match timeout(TokioDuration::from_millis(200), handle.events.recv()).await {
                Ok(Some(WatchEvent::GitHead)) => {
                    saw_head_before_update = true;
                    break;
                }
                Ok(Some(_)) => continue,
                Ok(None) => break,
                Err(_) => continue,
            }
        }
        assert!(
            !saw_head_before_update,
            "writes to a branch the matcher is not tracking must not fire GitHead"
        );

        // Phase 2: hot-swap the matcher to point at `sibling`, write
        // to it again, confirm GitHead fires this time. The handle
        // is `&self` for the update call, so no mutable borrow
        // conflict with the subsequent `events.recv()`.
        handle.update_current_branch_ref(Some("refs/heads/sibling"));
        tokio::time::sleep(TokioDuration::from_millis(150)).await;
        fs::write(
            refs_heads.join("sibling"),
            b"2222222222222222222222222222222222222222\n",
        )
        .expect("write sibling phase 2");

        let mut saw_head_after_update = false;
        let phase2_until = tokio::time::Instant::now() + DRAIN_WAIT;
        while tokio::time::Instant::now() < phase2_until {
            match timeout(TokioDuration::from_millis(200), handle.events.recv()).await {
                Ok(Some(WatchEvent::GitHead)) => {
                    saw_head_after_update = true;
                    break;
                }
                Ok(Some(_)) => continue,
                Ok(None) => break,
                Err(_) => continue,
            }
        }
        assert!(
            saw_head_after_update,
            "after update_current_branch_ref the matcher must see the newly tracked branch"
        );
    }
}
