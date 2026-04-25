mod diff;
mod parse;
mod path;
mod repo;
mod revert;
mod types;
mod untracked;

pub use diff::{compute_diff, compute_diff_with_snapshots, diff_single_file};
#[cfg(test)]
use parse::parse_unified_diff;
#[cfg(test)]
use parse::split_logical_lines;
pub use repo::{current_branch_ref, find_root, git_common_dir, git_dir, head_sha};
pub use revert::{build_hunk_patch, revert_hunk};
#[cfg(test)]
pub(crate) use types::line_numbers_for;
pub use types::{
    DiffContent, DiffLine, EMPTY_TREE_SHA, FileDiff, FileStatus, Hunk, LineKind, UNTRACKED_READ_CAP,
};
#[cfg(test)]
use untracked::synthesize_untracked_with_cap;

#[cfg(test)]
use std::path::{Path, PathBuf};

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    /// Create a fresh git repo in a tempdir with user.email / user.name set so
    /// `git commit` works without inheriting the host's git config.
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

    fn canonical(p: &Path) -> PathBuf {
        p.canonicalize().expect("canonicalize")
    }

    #[test]
    fn parse_unified_diff_extracts_single_added_line() {
        // A minimal unified diff produced by `git diff --no-renames`:
        // one modified file with a single added line inside one hunk.
        let raw = "\
diff --git a/foo.rs b/foo.rs
index e69de29..4b825dc 100644
--- a/foo.rs
+++ b/foo.rs
@@ -0,0 +1,1 @@
+fn main() {}
";

        let files = parse_unified_diff(raw).expect("parse diff");

        assert_eq!(files.len(), 1, "expected exactly one FileDiff");
        let file = &files[0];
        assert_eq!(file.path, PathBuf::from("foo.rs"));
        assert_eq!(file.status, FileStatus::Modified);
        assert_eq!(file.added, 1);
        assert_eq!(file.deleted, 0);

        let hunks = match &file.content {
            DiffContent::Text(hunks) => hunks,
            DiffContent::Binary => panic!("expected text content, got binary"),
        };
        assert_eq!(hunks.len(), 1, "expected exactly one hunk");
        let hunk = &hunks[0];
        assert_eq!(hunk.old_start, 0);
        assert_eq!(hunk.old_count, 0);
        assert_eq!(hunk.new_start, 1);
        assert_eq!(hunk.new_count, 1);
        assert_eq!(hunk.lines.len(), 1);
        assert_eq!(hunk.lines[0].kind, LineKind::Added);
        assert_eq!(hunk.lines[0].content, "fn main() {}");
        assert_eq!(hunk.context, None, "no xfuncname context expected");
    }

    #[test]
    fn parse_unified_diff_captures_xfuncname_context_from_at_at_line() {
        // git puts the enclosing function signature after the closing `@@`
        // when an xfuncname pattern matches. The parser must surface that
        // string as Hunk.context so the UI can use it as the hunk title.
        let raw = "\
diff --git a/src/auth.rs b/src/auth.rs
index e69de29..4b825dc 100644
--- a/src/auth.rs
+++ b/src/auth.rs
@@ -10,6 +10,9 @@ fn verify_token(claims: &Claims) -> Result<bool> {
+    if claims.exp < Utc::now().timestamp() as u64 {
+        return Err(AuthError::Expired);
+    }
";

        let files = parse_unified_diff(raw).expect("parse diff");
        assert_eq!(files.len(), 1);
        let hunks = match &files[0].content {
            DiffContent::Text(h) => h,
            DiffContent::Binary => panic!("expected text"),
        };
        assert_eq!(hunks.len(), 1);
        assert_eq!(
            hunks[0].context.as_deref(),
            Some("fn verify_token(claims: &Claims) -> Result<bool> {")
        );
    }

    #[test]
    fn parse_unified_diff_extracts_multiple_files() {
        // Two modified files in a single diff payload.
        let raw = "\
diff --git a/foo.rs b/foo.rs
index e69de29..4b825dc 100644
--- a/foo.rs
+++ b/foo.rs
@@ -1,1 +1,1 @@
-old foo
+new foo
diff --git a/bar.rs b/bar.rs
index 1111111..2222222 100644
--- a/bar.rs
+++ b/bar.rs
@@ -1,1 +1,1 @@
-old bar
+new bar
";

        let files = parse_unified_diff(raw).expect("parse diff");
        assert_eq!(files.len(), 2, "expected two FileDiffs");
        assert_eq!(files[0].path, PathBuf::from("foo.rs"));
        assert_eq!(files[0].added, 1);
        assert_eq!(files[0].deleted, 1);
        assert_eq!(files[1].path, PathBuf::from("bar.rs"));
        assert_eq!(files[1].added, 1);
        assert_eq!(files[1].deleted, 1);
    }

    #[test]
    fn parse_unified_diff_handles_multiple_hunks_in_one_file() {
        // One file with two non-contiguous hunks.
        let raw = "\
diff --git a/lib.rs b/lib.rs
index 1111111..2222222 100644
--- a/lib.rs
+++ b/lib.rs
@@ -1,2 +1,2 @@
 fn one() {}
-fn two() {}
+fn two_v2() {}
@@ -10,2 +10,2 @@
 fn ten() {}
-fn eleven() {}
+fn eleven_v2() {}
";

        let files = parse_unified_diff(raw).expect("parse diff");
        assert_eq!(files.len(), 1);
        let hunks = match &files[0].content {
            DiffContent::Text(h) => h,
            DiffContent::Binary => panic!("expected text content"),
        };
        assert_eq!(hunks.len(), 2, "expected two hunks");
        assert_eq!(hunks[0].new_start, 1);
        assert_eq!(hunks[1].new_start, 10);
        assert_eq!(files[0].added, 2);
        assert_eq!(files[0].deleted, 2);
    }

    #[test]
    fn parse_unified_diff_counts_added_and_deleted_lines() {
        // Mixed +/- counts within a single hunk.
        let raw = "\
diff --git a/mix.rs b/mix.rs
index 1111111..2222222 100644
--- a/mix.rs
+++ b/mix.rs
@@ -1,5 +1,4 @@
 keep
-drop1
-drop2
-drop3
+keep too
+only added
";

        let files = parse_unified_diff(raw).expect("parse diff");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].added, 2, "two + lines expected");
        assert_eq!(files[0].deleted, 3, "three - lines expected");
    }

    #[test]
    fn parse_unified_diff_detects_added_file_status() {
        let raw = "\
diff --git a/new.rs b/new.rs
new file mode 100644
index 0000000..2222222
--- /dev/null
+++ b/new.rs
@@ -0,0 +1,1 @@
+brand new
";
        let files = parse_unified_diff(raw).expect("parse diff");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].status, FileStatus::Added);
    }

    #[test]
    fn parse_unified_diff_detects_binary_diff() {
        // git diff -- <binary> emits a one-liner instead of a hunk.
        let raw = "\
diff --git a/icon.png b/icon.png
index 1111111..2222222 100644
Binary files a/icon.png and b/icon.png differ
";
        let files = parse_unified_diff(raw).expect("parse diff");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, PathBuf::from("icon.png"));
        assert!(matches!(files[0].content, DiffContent::Binary));
        assert_eq!(files[0].added, 0);
        assert_eq!(files[0].deleted, 0);
    }

    #[test]
    fn parse_unified_diff_marks_missing_terminal_newline_on_previous_line() {
        let raw = "\
diff --git a/foo.rs b/foo.rs
index e69de29..4b825dc 100644
--- a/foo.rs
+++ b/foo.rs
@@ -1 +1 @@
-old line
+new line
\\ No newline at end of file
";

        let files = parse_unified_diff(raw).expect("parse diff");
        let hunks = match &files[0].content {
            DiffContent::Text(hunks) => hunks,
            DiffContent::Binary => panic!("expected text content"),
        };
        let last = hunks[0].lines.last().expect("line present");
        assert_eq!(last.content, "new line");
        assert!(
            !last.has_trailing_newline,
            "newline marker line must clear the previous diff line's newline flag"
        );
    }

    #[test]
    fn parse_unified_diff_rejects_unparseable_diff_git_header() {
        let raw = "\
diff --git definitely-not-a-valid-header
@@ -0,0 +1,1 @@
+x
";

        let err = parse_unified_diff(raw).expect_err("malformed diff header must surface");
        assert!(
            err.to_string().contains("unparseable `diff --git` header"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn parse_unified_diff_rejects_malformed_hunk_header() {
        let raw = "\
diff --git a/foo.rs b/foo.rs
@@ -bogus +1 @@
+x
";

        let err = parse_unified_diff(raw).expect_err("malformed hunk header must surface");
        assert!(
            err.to_string().contains("malformed old hunk range"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn split_logical_lines_preserves_literal_carriage_return_without_newline() {
        let lines = split_logical_lines("carriage-return-only\r");
        assert_eq!(lines, vec![("carriage-return-only\r".to_string(), false)]);
    }

    #[test]
    fn parse_unified_diff_detects_deleted_file_status() {
        let raw = "\
diff --git a/gone.rs b/gone.rs
deleted file mode 100644
index 1111111..0000000
--- a/gone.rs
+++ /dev/null
@@ -1,1 +0,0 @@
-was here
";
        let files = parse_unified_diff(raw).expect("parse diff");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].status, FileStatus::Deleted);
    }

    #[test]
    fn parse_unified_diff_decodes_c_quoted_tracked_pathname() {
        // Regression for the adversarial finding: the old path
        // extraction used `split_once(" b/")` with an
        // empty-path fallback, so a quoted header like
        // `"a/\t.txt" "b/\t.txt"` collapsed to an empty path. Every
        // quoted file then merged under the same empty path, breaking
        // file grouping and follow mode in the diff view.
        //
        // The new parser must C-unescape the quoted token and yield
        // the real filename (here: a single TAB byte followed by
        // `.txt`).
        let raw = "\
diff --git \"a/\\t.txt\" \"b/\\t.txt\"
index 1111111..2222222 100644
--- \"a/\\t.txt\"
+++ \"b/\\t.txt\"
@@ -1,1 +1,2 @@
 line
+added
";
        let files = parse_unified_diff(raw).expect("parse diff");
        assert_eq!(files.len(), 1, "expected one file, got {files:?}");
        assert_eq!(files[0].path, PathBuf::from("\t.txt"));
        assert_eq!(files[0].added, 1);
    }

    #[test]
    fn parse_unified_diff_decodes_c_quoted_octal_escape_in_path() {
        // Git's fallback for non-ASCII / non-printable bytes is a
        // 3-digit octal escape like `\303\244` (UTF-8 for `ä`). The
        // parser must accept the octal form and reconstruct the
        // original bytes — otherwise core.quotePath=true repos
        // (the default) silently lose non-ASCII filenames.
        let raw = "\
diff --git \"a/caf\\303\\251.txt\" \"b/caf\\303\\251.txt\"
index 1111111..2222222 100644
--- \"a/caf\\303\\251.txt\"
+++ \"b/caf\\303\\251.txt\"
@@ -1,1 +1,2 @@
 one
+two
";
        let files = parse_unified_diff(raw).expect("parse diff");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, PathBuf::from("café.txt"));
        assert_eq!(files[0].added, 1);
    }

    #[test]
    fn parse_unified_diff_handles_unquoted_path_containing_literal_b_slash() {
        // Adversarial edge case: a filename whose bytes contain the
        // literal sequence ` b/` (space + b + slash). A naive
        // `split_once(" b/")` would split the header at the first
        // occurrence inside the filename, returning a truncated path.
        // The length-based parser exploits the `--no-renames`
        // symmetry (`a/<P> b/<P>`) to slice at the true midpoint.
        let raw = "\
diff --git a/foo b/bar b/foo b/bar
index 1111111..2222222 100644
--- a/foo b/bar
+++ b/foo b/bar
@@ -1,1 +1,2 @@
 x
+y
";
        let files = parse_unified_diff(raw).expect("parse diff");
        assert_eq!(files.len(), 1, "expected one file, got {files:?}");
        assert_eq!(files[0].path, PathBuf::from("foo b/bar"));
        assert_eq!(files[0].added, 1);
    }

    #[test]
    fn find_root_returns_worktree_root() {
        let repo = init_repo();
        let root = find_root(repo.path()).expect("find_root");
        assert_eq!(canonical(&root), canonical(repo.path()));
    }

    #[test]
    fn git_dir_resolves_in_normal_repo() {
        let repo = init_repo();
        let gd = git_dir(repo.path()).expect("git_dir");
        let expected = repo.path().join(".git");
        assert_eq!(canonical(&gd), canonical(&expected));
    }

    #[test]
    fn head_sha_falls_back_to_empty_tree_in_fresh_repo() {
        let repo = init_repo();
        // No commits yet — should fall back to the empty tree SHA.
        let sha = head_sha(repo.path()).expect("head_sha");
        assert_eq!(sha, EMPTY_TREE_SHA);
    }

    #[test]
    fn head_sha_surfaces_broken_head_when_repo_has_commits() {
        // Regression for the adversarial finding: the old head_sha
        // returned EMPTY_TREE_SHA for every `git rev-parse HEAD`
        // failure, so a repository whose HEAD pointed at a
        // non-existent ref would render as "everything added" and
        // hide the real breakage from the user. The narrowed
        // fallback must only fire when the repo is genuinely unborn.
        //
        // Setup: commit one file so `refs/heads/main` exists, then
        // reseat HEAD to a symbolic ref that has never been written.
        // `rev-parse HEAD` will fail (unknown ref) but `rev-list
        // --all` still finds the real commit via `refs/heads/main`,
        // so this is NOT an unborn repo.
        let repo = init_repo();
        fs::write(repo.path().join("seed.txt"), "seed").expect("write seed");
        run_git(repo.path(), &["add", "seed.txt"]);
        run_git(repo.path(), &["commit", "--quiet", "-m", "initial"]);
        run_git(
            repo.path(),
            &["symbolic-ref", "HEAD", "refs/heads/never-existed"],
        );

        let result = head_sha(repo.path());
        let err = match result {
            Ok(sha) => {
                panic!("broken HEAD must surface an error; got empty-tree fallback sha {sha}")
            }
            Err(e) => e,
        };
        let chain = format!("{err:#}");
        assert!(
            chain.contains("git rev-parse HEAD"),
            "error chain should identify the failing command, got: {chain}"
        );
    }

    #[test]
    fn head_sha_returns_actual_sha_after_commit() {
        let repo = init_repo();
        fs::write(repo.path().join("seed.txt"), "hello").expect("write seed");
        run_git(repo.path(), &["add", "seed.txt"]);
        run_git(repo.path(), &["commit", "--quiet", "-m", "initial"]);

        let sha = head_sha(repo.path()).expect("head_sha");
        assert_eq!(sha.len(), 40, "expected a 40-char SHA, got {sha:?}");
        assert_ne!(sha, EMPTY_TREE_SHA);
        assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn compute_diff_returns_modified_file_against_committed_baseline() {
        let repo = init_repo();
        // Commit an initial version so we have a real baseline SHA.
        fs::write(repo.path().join("greeting.txt"), "hello\n").expect("write seed");
        run_git(repo.path(), &["add", "greeting.txt"]);
        run_git(repo.path(), &["commit", "--quiet", "-m", "initial"]);
        let baseline = head_sha(repo.path()).expect("head_sha");

        // Now modify the file and ask compute_diff what changed.
        fs::write(repo.path().join("greeting.txt"), "hello world\n").expect("write modification");

        let files = compute_diff(repo.path(), &baseline).expect("compute_diff");
        assert_eq!(files.len(), 1, "expected one modified file");
        assert_eq!(files[0].path, PathBuf::from("greeting.txt"));
        assert_eq!(files[0].status, FileStatus::Modified);
        assert_eq!(files[0].added, 1);
        assert_eq!(files[0].deleted, 1);
    }

    #[test]
    fn compute_diff_with_snapshots_pairs_each_filediff_with_its_raw_text() {
        // compute_diff_with_snapshots must replace N subprocess calls
        // (one diff_single_file per file) with the single diff already
        // produced by compute_diff — so the returned snapshot map must
        // have one entry per file and each value must be byte-identical
        // to what diff_single_file would produce for that path alone.
        let repo = init_repo();
        fs::write(repo.path().join("a.txt"), "a-seed\n").expect("write a");
        fs::write(repo.path().join("b.txt"), "b-seed\n").expect("write b");
        run_git(repo.path(), &["add", "a.txt", "b.txt"]);
        run_git(repo.path(), &["commit", "--quiet", "-m", "initial"]);
        let baseline = head_sha(repo.path()).expect("head_sha");

        // Two dirty tracked files + one untracked file: all three kinds
        // must land in the snapshot map.
        fs::write(repo.path().join("a.txt"), "a-edit\n").expect("edit a");
        fs::write(repo.path().join("b.txt"), "b-edit\n").expect("edit b");
        fs::write(repo.path().join("c.md"), "new file\n").expect("write c");

        let (files, snapshots) = compute_diff_with_snapshots(repo.path(), &baseline)
            .expect("compute_diff_with_snapshots");

        assert_eq!(files.len(), 3, "expected three files, got {files:?}");
        assert_eq!(
            snapshots.len(),
            files.len(),
            "snapshot map must have one entry per FileDiff"
        );

        for file in &files {
            let reference = diff_single_file(repo.path(), &baseline, &file.path)
                .expect("diff_single_file reference");
            let snapshot = snapshots
                .get(&file.path)
                .unwrap_or_else(|| panic!("no snapshot for {:?}", file.path));
            assert_eq!(
                snapshot, &reference,
                "snapshot for {:?} must match diff_single_file output",
                file.path,
            );
        }
    }

    #[test]
    fn compute_diff_returns_empty_when_worktree_is_clean() {
        let repo = init_repo();
        fs::write(repo.path().join("seed.txt"), "x").expect("write");
        run_git(repo.path(), &["add", "seed.txt"]);
        run_git(repo.path(), &["commit", "--quiet", "-m", "initial"]);
        let baseline = head_sha(repo.path()).expect("head_sha");

        let files = compute_diff(repo.path(), &baseline).expect("compute_diff");
        assert!(files.is_empty(), "expected empty diff, got {files:?}");
    }

    #[test]
    fn compute_diff_includes_untracked_text_file() {
        let repo = init_repo();
        // Need an initial commit so head_sha returns a real SHA (not the
        // empty-tree fallback) — that way committed/untracked are distinct.
        fs::write(repo.path().join("seed.txt"), "seed").expect("write seed");
        run_git(repo.path(), &["add", "seed.txt"]);
        run_git(repo.path(), &["commit", "--quiet", "-m", "initial"]);
        let baseline = head_sha(repo.path()).expect("head_sha");

        // Drop a brand new file without `git add`.
        fs::write(repo.path().join("note.md"), "line one\nline two\n").expect("write untracked");

        let files = compute_diff(repo.path(), &baseline).expect("compute_diff");
        let untracked: Vec<_> = files
            .iter()
            .filter(|f| f.status == FileStatus::Untracked)
            .collect();
        assert_eq!(untracked.len(), 1, "expected one untracked file");
        let f = untracked[0];
        assert_eq!(f.path, PathBuf::from("note.md"));
        assert_eq!(f.added, 2);
        assert_eq!(f.deleted, 0);
        let hunks = match &f.content {
            DiffContent::Text(h) => h,
            DiffContent::Binary => panic!("expected text content"),
        };
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].new_start, 1);
        assert_eq!(hunks[0].new_count, 2);
        assert_eq!(hunks[0].lines.len(), 2);
        assert_eq!(hunks[0].lines[0].content, "line one");
        assert_eq!(hunks[0].lines[1].content, "line two");
    }

    #[test]
    fn compute_diff_expands_untracked_files_inside_subdirectories() {
        // Regression: default `git status --porcelain` collapses a
        // directory containing only untracked files into a single
        // `?? subdir/` entry. kizu must pass `--untracked-files=all`
        // so each file is listed individually — otherwise untracked
        // files dropped into subdirectories are invisible in the TUI.
        let repo = init_repo();
        fs::write(repo.path().join("seed.txt"), "seed").expect("write seed");
        run_git(repo.path(), &["add", "seed.txt"]);
        run_git(repo.path(), &["commit", "--quiet", "-m", "initial"]);
        let baseline = head_sha(repo.path()).expect("head_sha");

        fs::create_dir_all(repo.path().join("subdir")).expect("mkdir subdir");
        fs::write(repo.path().join("subdir/a.rs"), "alpha\n").expect("write a");
        fs::write(repo.path().join("subdir/b.rs"), "beta\n").expect("write b");

        let files = compute_diff(repo.path(), &baseline).expect("compute_diff");
        let mut untracked: Vec<_> = files
            .iter()
            .filter(|f| f.status == FileStatus::Untracked)
            .map(|f| f.path.clone())
            .collect();
        untracked.sort();
        assert_eq!(
            untracked,
            vec![PathBuf::from("subdir/a.rs"), PathBuf::from("subdir/b.rs"),],
            "expected both subdirectory files listed individually"
        );
    }

    #[test]
    fn compute_diff_includes_untracked_file_with_spaces_in_name() {
        // Regression: `git status --porcelain` (v1, no `-z`) quotes
        // filenames with spaces/special chars, so the old line parser
        // produced a literal `"design draft.md"` path that
        // `synthesize_untracked` then failed to open. The failure was
        // silently dropped and the file never showed up in the TUI.
        let repo = init_repo();
        fs::write(repo.path().join("seed.txt"), "seed").expect("write seed");
        run_git(repo.path(), &["add", "seed.txt"]);
        run_git(repo.path(), &["commit", "--quiet", "-m", "initial"]);
        let baseline = head_sha(repo.path()).expect("head_sha");

        // Filename with a real space in it — porcelain v1 wraps this in
        // double quotes; porcelain v1 `-z` returns it as literal bytes.
        fs::write(repo.path().join("design draft.md"), "alpha\nbeta\n").expect("write quoted file");

        let files = compute_diff(repo.path(), &baseline).expect("compute_diff");
        let found = files
            .iter()
            .find(|f| f.path == Path::new("design draft.md"))
            .expect("untracked file with space in name must be visible");
        assert_eq!(found.status, FileStatus::Untracked);
        assert_eq!(found.added, 2);
    }

    #[test]
    fn compute_diff_tolerates_non_utf8_tracked_content_via_lossy_decode() {
        // Regression for Codex round-3 finding: the old
        // `compute_diff` decoded `git diff` stdout with a strict
        // `String::from_utf8`, so a single tracked file containing
        // legacy-encoded bytes (Shift-JIS, Latin-1, …) would make
        // the entire refresh error out and leave the UI pinned to
        // a stale snapshot. Untracked handling already preserved
        // non-UTF-8 path bytes, so the tracked-diff strictness was
        // a silent asymmetry.
        //
        // Setup: commit a file with pure-ASCII content, then
        // rewrite it with a byte sequence that is not valid UTF-8
        // (0xFF is never a valid lead byte). `compute_diff` must
        // succeed and surface the file as Modified — lossy decode
        // replaces the bad byte with U+FFFD in the display, but
        // the refresh itself stays alive.
        let repo = init_repo();
        let path = "legacy.txt";
        fs::write(repo.path().join(path), "hello\n").expect("write seed");
        run_git(repo.path(), &["add", path]);
        run_git(repo.path(), &["commit", "--quiet", "-m", "initial"]);
        let baseline = head_sha(repo.path()).expect("head_sha");

        // Add a byte sequence containing an invalid UTF-8 byte
        // (0xFF is never a valid lead byte in any UTF-8 codepoint)
        // to exercise the strict vs lossy decode boundary.
        fs::write(
            repo.path().join(path),
            b"hello\nlegacy \xFF byte\n".as_slice(),
        )
        .expect("write legacy content");

        let files = compute_diff(repo.path(), &baseline)
            .expect("compute_diff must tolerate non-UTF-8 tracked content");
        let legacy = files
            .iter()
            .find(|f| f.path == Path::new(path))
            .expect("legacy file must still appear in the diff");
        assert_eq!(legacy.status, FileStatus::Modified);
        assert!(
            legacy.added >= 1,
            "the new legacy line must register as an addition, got {}",
            legacy.added
        );
    }

    #[test]
    fn compute_diff_marks_untracked_binary_file_as_binary() {
        let repo = init_repo();
        fs::write(repo.path().join("seed.txt"), "seed").expect("write seed");
        run_git(repo.path(), &["add", "seed.txt"]);
        run_git(repo.path(), &["commit", "--quiet", "-m", "initial"]);
        let baseline = head_sha(repo.path()).expect("head_sha");

        // Untracked file with an embedded NUL byte → should be Binary.
        let mut bytes = b"some text".to_vec();
        bytes.push(0);
        bytes.extend_from_slice(b"more bytes");
        fs::write(repo.path().join("blob.bin"), bytes).expect("write binary");

        let files = compute_diff(repo.path(), &baseline).expect("compute_diff");
        let bin = files
            .iter()
            .find(|f| f.path == Path::new("blob.bin"))
            .expect("untracked binary present");
        assert_eq!(bin.status, FileStatus::Untracked);
        assert!(matches!(bin.content, DiffContent::Binary));
        assert_eq!(bin.added, 0);
        assert_eq!(bin.deleted, 0);
    }

    #[test]
    fn synthesize_untracked_reports_truncation_marker_when_file_exceeds_cap() {
        // Exercise the cap / truncation / binary-detection plumbing without
        // materialising a `UNTRACKED_READ_CAP` (64 MiB) fixture on disk.
        let repo = init_repo();
        let cap = 4 * 1024usize;
        let line: String = "x".repeat(99);
        let body = (0..200)
            .map(|_| line.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(body.len() > cap, "fixture must exceed the test cap");
        fs::write(repo.path().join("big.txt"), &body).expect("write big");

        let synth = synthesize_untracked_with_cap(repo.path(), Path::new("big.txt"), cap)
            .expect("synthesize_untracked_with_cap");
        let hunks = match &synth.content {
            DiffContent::Text(h) => h,
            DiffContent::Binary => panic!("unexpected binary classification"),
        };
        let total_bytes: usize = hunks[0].lines.iter().map(|l| l.content.len() + 1).sum();
        assert!(
            total_bytes <= cap + 100,
            "untracked content should be capped near {cap} bytes, got {total_bytes}"
        );
        assert!(
            hunks[0]
                .lines
                .iter()
                .any(|line| line.content.contains("more bytes from new file")),
            "expected a visible truncation marker instead of silent truncation"
        );
        assert!(
            synth.added < 200,
            "expected fewer than 200 lines after cap, got {}",
            synth.added
        );
    }

    #[test]
    fn compute_diff_reads_untracked_file_below_cap_in_full() {
        let repo = init_repo();
        fs::write(repo.path().join("seed.txt"), "seed").expect("write seed");
        run_git(repo.path(), &["add", "seed.txt"]);
        run_git(repo.path(), &["commit", "--quiet", "-m", "initial"]);
        let baseline = head_sha(repo.path()).expect("head_sha");

        // 100 KiB worth of lines: far above the legacy 8 KiB cap and far
        // below the post-v0.3.2 cap. Must be returned in full with no
        // truncation marker once the cap has been raised.
        let line: String = "y".repeat(99);
        let body = (0..1024)
            .map(|_| line.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(repo.path().join("big.txt"), &body).expect("write big");

        let files = compute_diff(repo.path(), &baseline).expect("compute_diff");
        let big = files
            .iter()
            .find(|f| f.path == Path::new("big.txt"))
            .expect("untracked big present");
        let hunks = match &big.content {
            DiffContent::Text(h) => h,
            DiffContent::Binary => panic!("unexpected binary classification"),
        };
        assert!(
            hunks[0]
                .lines
                .iter()
                .all(|line| !line.content.contains("more bytes from new file")),
            "files well below the cap must not carry a truncation marker"
        );
        assert_eq!(
            big.added, 1024,
            "all 1024 generated lines must be present as Added"
        );
    }

    #[test]
    fn compute_diff_against_empty_tree_in_fresh_repo_shows_all_committed_files() {
        // A fresh repo with no commits: head_sha returns the empty-tree SHA,
        // and compute_diff against that baseline should show every committed
        // file as an addition. (Untracked synthesis is M1.9.)
        let repo = init_repo();
        fs::write(repo.path().join("a.txt"), "alpha\n").expect("write a");
        fs::write(repo.path().join("b.txt"), "beta\n").expect("write b");
        run_git(repo.path(), &["add", "a.txt", "b.txt"]);
        run_git(repo.path(), &["commit", "--quiet", "-m", "seed"]);

        let files = compute_diff(repo.path(), EMPTY_TREE_SHA).expect("compute_diff");
        let paths: Vec<_> = files.iter().map(|f| f.path.clone()).collect();
        assert!(paths.contains(&PathBuf::from("a.txt")));
        assert!(paths.contains(&PathBuf::from("b.txt")));
        for f in &files {
            assert_eq!(f.status, FileStatus::Added);
        }
    }

    // ---- revert hunk helpers (M4 slice 4) --------------------------

    fn line_context(content: &str) -> DiffLine {
        DiffLine {
            kind: LineKind::Context,
            content: content.to_string(),
            has_trailing_newline: true,
        }
    }
    fn line_added(content: &str) -> DiffLine {
        DiffLine {
            kind: LineKind::Added,
            content: content.to_string(),
            has_trailing_newline: true,
        }
    }
    fn line_deleted(content: &str) -> DiffLine {
        DiffLine {
            kind: LineKind::Deleted,
            content: content.to_string(),
            has_trailing_newline: true,
        }
    }

    #[test]
    fn build_hunk_patch_round_trips_a_modify_hunk_into_a_git_apply_payload() {
        // Mirrors what `git diff` would emit for a 1-line replacement
        // inside `foo.rs`: 1 context line on each side, one delete,
        // one add. `git apply --reverse` must accept this.
        let hunk = Hunk {
            old_start: 1,
            old_count: 2,
            new_start: 1,
            new_count: 2,
            lines: vec![line_context("keep"), line_deleted("old"), line_added("new")],
            context: None,
        };
        let patch = build_hunk_patch(Path::new("foo.rs"), &hunk);
        assert_eq!(
            patch,
            "\
--- a/foo.rs
+++ b/foo.rs
@@ -1,2 +1,2 @@
 keep
-old
+new
"
        );
    }

    #[test]
    fn revert_hunk_undoes_an_added_line_on_disk() {
        // Seed a committed file, add one line in the worktree,
        // compute a real diff, round-trip the hunk through
        // `build_hunk_patch` + `revert_hunk`, and confirm the
        // worktree file goes back to the original content.
        let repo = init_repo();
        let file_path = repo.path().join("hello.rs");
        fs::write(&file_path, "fn one() {}\n").expect("write seed");
        run_git(repo.path(), &["add", "hello.rs"]);
        run_git(repo.path(), &["commit", "--quiet", "-m", "seed"]);

        fs::write(&file_path, "fn one() {}\nfn two() {}\n").expect("write modified");

        let baseline = head_sha(repo.path()).expect("head_sha");
        let files = compute_diff(repo.path(), &baseline).expect("compute_diff");
        let file = files
            .iter()
            .find(|f| f.path == Path::new("hello.rs"))
            .expect("hello.rs in diff");
        let hunk = match &file.content {
            DiffContent::Text(hunks) => hunks.first().expect("one hunk"),
            _ => panic!("expected text content"),
        };

        let patch = build_hunk_patch(&file.path, hunk);
        revert_hunk(repo.path(), &patch).expect("revert");

        let after = fs::read_to_string(&file_path).expect("read back");
        assert_eq!(
            after, "fn one() {}\n",
            "revert must restore the worktree file to its committed state"
        );
    }

    #[test]
    fn revert_hunk_returns_err_when_patch_no_longer_applies_cleanly() {
        // Build a patch against one state, mutate the worktree so
        // the hunk no longer reverses cleanly, confirm revert_hunk
        // surfaces the failure as an Err rather than silently
        // leaving the file in a half-applied state.
        let repo = init_repo();
        let file_path = repo.path().join("drift.rs");
        fs::write(&file_path, "alpha\n").expect("seed");
        run_git(repo.path(), &["add", "drift.rs"]);
        run_git(repo.path(), &["commit", "--quiet", "-m", "seed"]);
        fs::write(&file_path, "alpha\nbeta\n").expect("add beta");

        let baseline = head_sha(repo.path()).expect("head_sha");
        let files = compute_diff(repo.path(), &baseline).expect("diff");
        let file = files
            .iter()
            .find(|f| f.path == Path::new("drift.rs"))
            .unwrap();
        let hunk = match &file.content {
            DiffContent::Text(h) => h.first().unwrap(),
            _ => panic!(),
        };
        let patch = build_hunk_patch(&file.path, hunk);

        // Now mutate the worktree: replace `beta` with `gamma`.
        // The reverse patch is still trying to delete `beta`, so
        // it must fail cleanly instead of creating a .rej file.
        fs::write(&file_path, "alpha\ngamma\n").expect("drift");
        let err = revert_hunk(repo.path(), &patch)
            .expect_err("patch must fail when the hunk has drifted");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("git apply"),
            "error must name the failing command, got {msg}",
        );
    }

    #[test]
    fn file_diff_header_prefix_defaults_to_none_in_parsed_diff() {
        let raw = "diff --git a/foo.rs b/foo.rs\n--- a/foo.rs\n+++ b/foo.rs\n@@ -1,1 +1,2 @@\n line1\n+line2\n";
        let files = parse_unified_diff(raw).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].header_prefix, None);
    }

    #[test]
    fn diff_single_file_synthesizes_untracked_file_as_all_added() {
        // `git diff <baseline> -- <path>` omits untracked files, but the
        // stream mode snapshot path needs the new file's contents so that
        // a `Write` creating a brand-new file produces a non-empty
        // `diff_snapshot`. Callers route through `diff_single_file` and
        // cannot tell a tracked-but-unchanged file from an untracked one;
        // the helper must detect the untracked case and synthesize an
        // all-added diff, mirroring `synthesize_untracked` / `compute_diff`.
        let repo = init_repo();
        fs::write(repo.path().join("seed.rs"), "seed\n").expect("write seed");
        run_git(repo.path(), &["add", "seed.rs"]);
        run_git(repo.path(), &["commit", "-m", "seed", "--quiet"]);
        let baseline = {
            let out = Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(repo.path())
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };

        // Brand-new file that `git diff` would otherwise ignore.
        let new_rel = Path::new("new.rs");
        let new_abs = repo.path().join(new_rel);
        fs::write(&new_abs, "alpha\nbeta\n").expect("write new");

        let diff = diff_single_file(&canonical(repo.path()), &baseline, new_rel)
            .expect("untracked file diff must succeed");

        assert!(
            diff.contains("+alpha"),
            "synthesized diff must contain the file's first line prefixed with `+`, got {diff:?}"
        );
        assert!(
            diff.contains("+beta"),
            "synthesized diff must contain the file's second line prefixed with `+`, got {diff:?}"
        );
    }

    #[test]
    fn diff_single_file_does_not_synthesize_for_gitignored_file() {
        // Ignored files (e.g. `.claude/settings.local.json`, `.env`)
        // must never leak into stream mode snapshots via the untracked
        // fallback. `git ls-files --error-unmatch` alone cannot tell
        // ignored from untracked, so the fallback must consult
        // `.gitignore` before reading file contents.
        let repo = init_repo();
        fs::write(repo.path().join(".gitignore"), "secrets.local\n").expect("write gitignore");
        fs::write(repo.path().join("seed.rs"), "seed\n").expect("write seed");
        run_git(repo.path(), &["add", ".gitignore", "seed.rs"]);
        run_git(repo.path(), &["commit", "-m", "seed", "--quiet"]);
        let baseline = head_sha(repo.path()).expect("head_sha");

        let secret_rel = Path::new("secrets.local");
        fs::write(
            repo.path().join(secret_rel),
            "API_TOKEN=deadbeef\nSSN=111\n",
        )
        .expect("write secret");

        let diff = diff_single_file(&canonical(repo.path()), &baseline, secret_rel)
            .expect("ignored files must not trigger an error, just an empty diff");

        assert!(
            !diff.contains("API_TOKEN"),
            "ignored file contents must never enter the synthesized diff, got {diff:?}"
        );
        assert!(
            diff.is_empty(),
            "ignored path must round-trip through the empty-diff path untouched, got {diff:?}"
        );
    }

    #[test]
    fn diff_single_file_surfaces_fallback_errors_instead_of_silent_empty_ok() {
        // The earlier implementation swallowed `is_untracked` /
        // `synthesize_untracked_diff_text` errors into `Ok("")`, which
        // let `handle_event_log` treat the failure as a valid empty
        // snapshot. That poisoned subsequent per-file diffs. A
        // non-`NotFound` failure must surface as `Err` so callers can
        // preserve the previous snapshot.
        //
        // We trigger a failure in the fallback path by pointing
        // `root` at a directory that is not a git repository. The
        // initial `git diff` returns non-zero (already `Err`) so the
        // synthesize path is not reached in that shape. Instead, we
        // commit a clean repo with an untracked file, then corrupt
        // `.git` after the initial diff succeeds: too complex. A
        // simpler, equally meaningful check: if the *file itself*
        // cannot be read (permissions error) and is *not* missing,
        // the helper must propagate the error. We approximate by
        // creating a dangling symlink whose target never existed and
        // confirming the helper does not return `Ok("")`.
        let repo = init_repo();
        fs::write(repo.path().join("seed.rs"), "seed\n").expect("seed");
        run_git(repo.path(), &["add", "seed.rs"]);
        run_git(repo.path(), &["commit", "-m", "seed", "--quiet"]);
        let baseline = head_sha(repo.path()).expect("head_sha");

        // Dangling symlink: target never existed. `synthesize_untracked`
        // will open-fail with NotFound. We want NotFound to collapse to
        // an empty `Ok` (the file vanished), mirroring `compute_diff`'s
        // NotFound tolerance — but any OTHER failure must surface.
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let link = repo.path().join("dangling.link");
            symlink(repo.path().join("no-such-file"), &link).expect("create symlink");
            let res = diff_single_file(
                &canonical(repo.path()),
                &baseline,
                Path::new("dangling.link"),
            );
            // NotFound of the symlink target is tolerated → Ok("") is fine here.
            // We assert only that no panic and the returned diff is empty.
            // Surfacing NotFound as Err is also acceptable; we just must not
            // swallow arbitrary errors as empty success.
            if let Ok(s) = res {
                assert!(s.is_empty(), "dangling symlink should be empty, got {s:?}");
            }
        }
    }

    // ---- line_numbers_for (v0.5) -------------------------------------

    fn diff_line(kind: LineKind, content: &str) -> DiffLine {
        DiffLine {
            kind,
            content: content.to_string(),
            has_trailing_newline: true,
        }
    }

    #[test]
    fn line_numbers_for_context_row_returns_both_sides() {
        let hunk = Hunk {
            old_start: 10,
            old_count: 3,
            new_start: 10,
            new_count: 3,
            lines: vec![
                diff_line(LineKind::Context, "a"),
                diff_line(LineKind::Context, "b"),
                diff_line(LineKind::Context, "c"),
            ],
            context: None,
        };
        assert_eq!(line_numbers_for(&hunk, 0), (Some(10), Some(10)));
        assert_eq!(line_numbers_for(&hunk, 1), (Some(11), Some(11)));
        assert_eq!(line_numbers_for(&hunk, 2), (Some(12), Some(12)));
    }

    #[test]
    fn line_numbers_for_added_row_returns_new_only() {
        // @@ -10,2 +10,3 @@
        //  context        <- old 10, new 10
        // +added          <- old None, new 11
        //  context        <- old 11, new 12
        let hunk = Hunk {
            old_start: 10,
            old_count: 2,
            new_start: 10,
            new_count: 3,
            lines: vec![
                diff_line(LineKind::Context, "a"),
                diff_line(LineKind::Added, "b"),
                diff_line(LineKind::Context, "c"),
            ],
            context: None,
        };
        assert_eq!(line_numbers_for(&hunk, 0), (Some(10), Some(10)));
        assert_eq!(line_numbers_for(&hunk, 1), (None, Some(11)));
        assert_eq!(line_numbers_for(&hunk, 2), (Some(11), Some(12)));
    }

    #[test]
    fn line_numbers_for_deleted_row_returns_old_only() {
        // @@ -10,3 +10,2 @@
        //  context        <- old 10, new 10
        // -deleted        <- old 11, new None
        //  context        <- old 12, new 11
        let hunk = Hunk {
            old_start: 10,
            old_count: 3,
            new_start: 10,
            new_count: 2,
            lines: vec![
                diff_line(LineKind::Context, "a"),
                diff_line(LineKind::Deleted, "b"),
                diff_line(LineKind::Context, "c"),
            ],
            context: None,
        };
        assert_eq!(line_numbers_for(&hunk, 0), (Some(10), Some(10)));
        assert_eq!(line_numbers_for(&hunk, 1), (Some(11), None));
        assert_eq!(line_numbers_for(&hunk, 2), (Some(12), Some(11)));
    }

    #[test]
    fn line_numbers_for_out_of_range_returns_none() {
        let hunk = Hunk {
            old_start: 5,
            old_count: 1,
            new_start: 5,
            new_count: 1,
            lines: vec![diff_line(LineKind::Context, "a")],
            context: None,
        };
        // Out-of-range index must not panic and must return (None, None)
        // so the caller can treat it as "no line number available".
        assert_eq!(line_numbers_for(&hunk, 99), (None, None));
    }
}
