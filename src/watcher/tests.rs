use super::backend::new_kizu_debouncer;
use super::git_state::git_state_watch_roots;
use super::matcher::{BaselineMatcherInner, canonicalize_or_self};
use super::*;
use notify::RecursiveMode;
use std::fs;
use std::process::Command;
use std::sync::mpsc;
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

/// Drain all pending events for `wait` duration, discarding them.
/// Used to clear startup noise before testing a specific write.
async fn drain_events(handle: &mut WatchHandle, wait: TokioDuration) {
    let deadline = tokio::time::Instant::now() + wait;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let poll = remaining.min(TokioDuration::from_millis(200));
        match timeout(poll, handle.events.recv()).await {
            Ok(Some(_)) => continue,
            Ok(None) => break,
            Err(_) => continue,
        }
    }
}

async fn saw_matching_event<F>(
    handle: &mut WatchHandle,
    wait: TokioDuration,
    mut matches: F,
) -> bool
where
    F: FnMut(&WatchEvent) -> bool,
{
    let deadline = tokio::time::Instant::now() + wait;
    while tokio::time::Instant::now() < deadline {
        let now = tokio::time::Instant::now();
        let remaining = deadline.saturating_duration_since(now);
        let next_poll = if remaining > TokioDuration::from_millis(200) {
            TokioDuration::from_millis(200)
        } else {
            remaining
        };
        match timeout(next_poll, handle.events.recv()).await {
            Ok(Some(event)) if matches(&event) => return true,
            Ok(Some(_)) => continue,
            Ok(None) => return false,
            Err(_) => continue,
        }
    }
    false
}

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
async fn worktree_watcher_skips_target_directory() {
    let repo = init_repo();
    fs::create_dir_all(repo.path().join("target")).expect("create target");
    fs::create_dir_all(repo.path().join("src")).expect("create src");
    fs::write(repo.path().join("target").join("foo.rs"), "fn build() {}\n")
        .expect("write target file");
    fs::write(repo.path().join("src").join("bar.rs"), "fn app() {}\n").expect("write src file");

    let root = crate::git::find_root(repo.path()).expect("find_root");
    let git_dir = crate::git::git_dir(&root).expect("git_dir");
    let common = crate::git::git_common_dir(&root).expect("common git_dir");
    let branch = crate::git::current_branch_ref(&root).expect("current branch");

    let mut handle = start(&root, &git_dir, &common, branch.as_deref()).expect("start watcher");

    // Drain any startup events (inotify may fire for files that
    // existed before the watcher was created).
    drain_events(&mut handle, TokioDuration::from_millis(800)).await;

    fs::write(root.join("target").join("foo.rs"), "fn build() { 1 }\n")
        .expect("rewrite target file");

    let saw_target_event =
        saw_matching_event(&mut handle, TokioDuration::from_millis(1_000), |event| {
            *event == WatchEvent::Worktree
        })
        .await;
    assert!(
        !saw_target_event,
        "nested writes under excluded target/ must not emit Worktree"
    );

    fs::write(root.join("src").join("bar.rs"), "fn app() { 1 }\n").expect("rewrite src file");

    let saw_src_event = saw_matching_event(&mut handle, DRAIN_WAIT, |event| {
        *event == WatchEvent::Worktree
    })
    .await;
    assert!(
        saw_src_event,
        "nested writes under non-excluded top-level directories must still emit Worktree"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn worktree_watcher_still_sees_root_level_file_writes() {
    let repo = init_repo();
    fs::write(repo.path().join("README.md"), "before\n").expect("write root file");

    let root = crate::git::find_root(repo.path()).expect("find_root");
    let git_dir = crate::git::git_dir(&root).expect("git_dir");
    let common = crate::git::git_common_dir(&root).expect("common git_dir");
    let branch = crate::git::current_branch_ref(&root).expect("current branch");

    let mut handle = start(&root, &git_dir, &common, branch.as_deref()).expect("start watcher");

    tokio::time::sleep(TokioDuration::from_millis(250)).await;
    fs::write(root.join("README.md"), "after!\n").expect("rewrite root file");

    let saw_root_event = saw_matching_event(&mut handle, DRAIN_WAIT, |event| {
        *event == WatchEvent::Worktree
    })
    .await;
    assert!(
        saw_root_event,
        "root-level file writes must still emit Worktree with a non-recursive root watch"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn writes_inside_git_dir_do_not_emit_worktree_event() {
    let repo = init_repo();
    let root = crate::git::find_root(repo.path()).expect("find_root");
    let git_dir = crate::git::git_dir(&root).expect("git_dir");
    let common = crate::git::git_common_dir(&root).expect("common git_dir");
    let branch = crate::git::current_branch_ref(&root).expect("current branch");

    let mut handle = start(&root, &git_dir, &common, branch.as_deref()).expect("start watcher");

    // Drain startup events before testing the specific write.
    drain_events(&mut handle, TokioDuration::from_millis(800)).await;

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
            Ok(Some(WatchEvent::GitHead(_))) => {
                saw_head = true;
                break;
            }
            Ok(Some(WatchEvent::Error { .. } | WatchEvent::EventLog(_))) => continue,
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
            Ok(Some(WatchEvent::GitHead(_))) => {
                saw_head = true;
                break;
            }
            Ok(Some(WatchEvent::Worktree)) => continue,
            Ok(Some(WatchEvent::Error { .. } | WatchEvent::EventLog(_))) => continue,
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
            Ok(Some(WatchEvent::GitHead(_))) => {
                saw_head = true;
                break;
            }
            Ok(Some(WatchEvent::Worktree)) => continue,
            Ok(Some(WatchEvent::Error { .. } | WatchEvent::EventLog(_))) => continue,
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
    let linked_git_dir = crate::git::git_dir(&linked_root).expect("linked per-worktree git_dir");
    let common_git_dir = crate::git::git_common_dir(&linked_root).expect("linked common git_dir");
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
            Ok(Some(WatchEvent::GitHead(_))) => {
                saw_head = true;
                break;
            }
            Ok(Some(WatchEvent::Worktree)) => continue,
            Ok(Some(WatchEvent::Error { .. } | WatchEvent::EventLog(_))) => continue,
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

#[test]
fn canonicalize_or_self_preserves_missing_tail_under_canonical_parent() {
    let temp = tempfile::tempdir().expect("tempdir");
    let parent = temp.path().join("refs").join("heads");
    fs::create_dir_all(&parent).expect("create existing parent");

    let missing = parent.join("future-branch");
    let canonical_parent = parent.canonicalize().expect("canonical parent");
    assert_eq!(
        canonicalize_or_self(&missing),
        canonical_parent.join("future-branch")
    );
}

#[test]
fn git_state_watch_roots_focus_on_head_refs_and_common_root() {
    let temp = tempfile::tempdir().expect("tempdir");
    let git_dir = temp.path().join(".git");
    fs::create_dir_all(git_dir.join("refs").join("heads")).expect("create refs/heads");

    let roots = git_state_watch_roots(&git_dir, &git_dir);
    assert_eq!(
        roots,
        vec![
            WatchRoot {
                path: git_dir.join("HEAD"),
                recursive_mode: RecursiveMode::NonRecursive,
                compare_contents: true,
                source: WatchSource::GitPerWorktreeHead,
            },
            WatchRoot {
                path: git_dir.join("refs"),
                recursive_mode: RecursiveMode::Recursive,
                compare_contents: true,
                source: WatchSource::GitRefs,
            },
            WatchRoot {
                path: git_dir.clone(),
                recursive_mode: RecursiveMode::NonRecursive,
                compare_contents: true,
                source: WatchSource::GitCommonRoot,
            },
        ]
    );
}

#[test]
fn selected_kizu_backend_smoke_receives_create_event() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (tx, rx) = mpsc::channel();
    let mut debouncer =
        new_kizu_debouncer(TokioDuration::from_millis(50), false, tx).expect("new debouncer");
    debouncer
        .watch(dir.path(), RecursiveMode::Recursive)
        .expect("watch tempdir");

    let file = dir.path().join("smoke.txt");
    fs::write(&file, "ok\n").expect("write smoke file");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        let batch = rx
            .recv_timeout(deadline - std::time::Instant::now())
            .expect("receive debounced event")
            .expect("notify backend error");
        if batch.iter().any(|event| {
            event.event.paths.iter().any(|path| {
                *path == file
                    || path
                        .canonicalize()
                        .ok()
                        .is_some_and(|canonical| canonical == file)
            })
        }) {
            return;
        }
    }

    panic!(
        "selected kizu watcher backend never observed {}",
        file.display()
    );
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
            Ok(Some(WatchEvent::GitHead(_))) => {
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
            Ok(Some(WatchEvent::GitHead(_))) => {
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

#[tokio::test(flavor = "current_thread")]
async fn packed_refs_rewrites_after_birth_still_emit_head_event() {
    let repo = init_repo();
    fs::write(repo.path().join("seed.txt"), "seed\n").expect("write seed");
    run_git(repo.path(), &["add", "seed.txt"]);
    run_git(repo.path(), &["commit", "--quiet", "-m", "init"]);

    let root = crate::git::find_root(repo.path()).expect("find_root");
    let git_dir = crate::git::git_dir(&root).expect("git_dir");
    let common = crate::git::git_common_dir(&root).expect("common git_dir");
    let branch = crate::git::current_branch_ref(&root).expect("current branch");
    let packed_refs = common.join("packed-refs");

    let mut handle = start(&root, &git_dir, &common, branch.as_deref()).expect("start watcher");

    tokio::time::sleep(TokioDuration::from_millis(150)).await;

    // Phase 1: create packed-refs after startup. This simulates a repo
    // that was born with loose refs only and later ran pack-refs.
    fs::write(
        &packed_refs,
        "0000000000000000000000000000000000000000 refs/heads/main\n",
    )
    .expect("create packed-refs");

    let mut saw_birth = false;
    let phase1_until = tokio::time::Instant::now() + DRAIN_WAIT;
    while tokio::time::Instant::now() < phase1_until {
        match timeout(TokioDuration::from_millis(200), handle.events.recv()).await {
            Ok(Some(WatchEvent::GitHead(_))) => {
                saw_birth = true;
                break;
            }
            Ok(Some(_)) => continue,
            Ok(None) => break,
            Err(_) => continue,
        }
    }
    assert!(saw_birth, "creating packed-refs must emit GitHead");

    // Phase 2: rewrite the same packed-refs file in place. The watcher
    // must still see this even though packed-refs did not exist at
    // startup and therefore did not have a dedicated file watcher.
    fs::write(
        &packed_refs,
        "1111111111111111111111111111111111111111 refs/heads/main\n",
    )
    .expect("rewrite packed-refs");

    let mut saw_rewrite = false;
    let phase2_until = tokio::time::Instant::now() + DRAIN_WAIT;
    while tokio::time::Instant::now() < phase2_until {
        match timeout(TokioDuration::from_millis(200), handle.events.recv()).await {
            Ok(Some(WatchEvent::GitHead(_))) => {
                saw_rewrite = true;
                break;
            }
            Ok(Some(_)) => continue,
            Ok(None) => break,
            Err(_) => continue,
        }
    }
    assert!(
        saw_rewrite,
        "rewriting packed-refs after it is created must still emit GitHead"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn same_size_existing_file_rewrite_emits_worktree_event() {
    let repo = init_repo();
    fs::write(repo.path().join("same.txt"), "alpha\n").expect("write seed");
    run_git(repo.path(), &["add", "same.txt"]);
    run_git(repo.path(), &["commit", "--quiet", "-m", "init"]);

    let root = crate::git::find_root(repo.path()).expect("find_root");
    let git_dir = crate::git::git_dir(&root).expect("git_dir");
    let common = crate::git::git_common_dir(&root).expect("common git_dir");
    let branch = crate::git::current_branch_ref(&root).expect("current branch");

    let mut handle = start(&root, &git_dir, &common, branch.as_deref()).expect("start watcher");

    tokio::time::sleep(TokioDuration::from_millis(250)).await;
    fs::write(root.join("same.txt"), "omega\n").expect("rewrite same-size file");

    let mut saw_worktree = false;
    let drain_until = tokio::time::Instant::now() + DRAIN_WAIT;
    while tokio::time::Instant::now() < drain_until {
        match timeout(TokioDuration::from_millis(200), handle.events.recv()).await {
            Ok(Some(WatchEvent::Worktree)) => {
                saw_worktree = true;
                break;
            }
            Ok(Some(_)) => continue,
            Ok(None) => break,
            Err(_) => continue,
        }
    }
    assert!(
        saw_worktree,
        "rewriting an existing file with the same size must still emit Worktree"
    );
}
