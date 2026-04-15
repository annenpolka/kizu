use anyhow::{Context, Result, anyhow};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

/// Maximum number of bytes read from an untracked file when synthesizing its
/// "all-added" diff. See M1.9 / Decision Log for rationale.
pub const UNTRACKED_READ_CAP: usize = 8 * 1024;

/// Empty tree SHA — used as the baseline when a repository has no commits yet.
/// See ADR notes in plans/v0.1-mvp.md (Decision Log: empty tree fallback).
pub const EMPTY_TREE_SHA: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub path: PathBuf,
    pub status: FileStatus,
    pub added: usize,
    pub deleted: usize,
    pub content: DiffContent,
    /// Last modification time of the worktree file. Filled by the app layer
    /// after `compute_diff` returns; the parser leaves this at
    /// [`SystemTime::UNIX_EPOCH`] so it is always defined.
    pub mtime: SystemTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffContent {
    Text(Vec<Hunk>),
    Binary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    Modified,
    Added,
    Deleted,
    Untracked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: usize,
    pub old_count: usize,
    pub new_start: usize,
    pub new_count: usize,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Deleted,
}

/// Run `git diff --no-renames <baseline> --` and parse the result, then
/// append synthesized [`FileDiff`] entries for untracked files.
///
/// The `--no-renames` flag (ADR-0001) keeps the parser simple and avoids
/// rename detection diverging from the user's mental model.
pub fn compute_diff(root: &Path, baseline_sha: &str) -> Result<Vec<FileDiff>> {
    let output = Command::new("git")
        .args(["diff", "--no-renames", baseline_sha, "--"])
        .current_dir(root)
        .output()
        .context("failed to spawn `git diff --no-renames`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("`git diff` failed: {}", stderr.trim()));
    }

    let raw = String::from_utf8(output.stdout).context("`git diff` produced non-UTF8 output")?;
    let mut files = parse_unified_diff(&raw);

    for rel in list_untracked(root)? {
        match synthesize_untracked(root, &rel) {
            Ok(synth) => files.push(synth),
            // A read failure (e.g. the file vanished between status and read)
            // shouldn't abort the entire diff — just skip the entry.
            Err(_) => continue,
        }
    }

    Ok(files)
}

/// List untracked files reported by `git status --porcelain`.
fn list_untracked(root: &Path) -> Result<Vec<PathBuf>> {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(root)
        .output()
        .context("failed to spawn `git status --porcelain`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("`git status` failed: {}", stderr.trim()));
    }

    let raw = String::from_utf8(output.stdout).context("`git status` produced non-UTF8 output")?;
    Ok(raw
        .lines()
        .filter_map(|line| line.strip_prefix("?? ").map(PathBuf::from))
        .collect())
}

/// Build a synthetic [`FileDiff`] for an untracked file, treating every line
/// as an added line. Reads at most [`UNTRACKED_READ_CAP`] bytes; binary files
/// (NUL byte detected in the read window) are returned as
/// [`DiffContent::Binary`].
fn synthesize_untracked(root: &Path, rel_path: &Path) -> Result<FileDiff> {
    let abs = root.join(rel_path);
    let mut file = std::fs::File::open(&abs)
        .with_context(|| format!("opening untracked file {}", abs.display()))?;
    let mut buf: Vec<u8> = Vec::with_capacity(UNTRACKED_READ_CAP);
    file.by_ref()
        .take(UNTRACKED_READ_CAP as u64)
        .read_to_end(&mut buf)
        .with_context(|| format!("reading untracked file {}", abs.display()))?;

    if buf.contains(&0u8) {
        return Ok(FileDiff {
            path: rel_path.to_path_buf(),
            status: FileStatus::Untracked,
            added: 0,
            deleted: 0,
            content: DiffContent::Binary,
            mtime: SystemTime::UNIX_EPOCH,
        });
    }

    // We may have stopped mid-codepoint at the 8KB boundary; fall back to a
    // lossy decode so we never refuse a valid file because of an awkward cut.
    let text = String::from_utf8_lossy(&buf);
    let lines: Vec<DiffLine> = text
        .lines()
        .map(|line| DiffLine {
            kind: LineKind::Added,
            content: line.to_string(),
        })
        .collect();
    let added = lines.len();
    let new_count = added;
    let new_start = if new_count == 0 { 0 } else { 1 };

    Ok(FileDiff {
        path: rel_path.to_path_buf(),
        status: FileStatus::Untracked,
        added,
        deleted: 0,
        content: DiffContent::Text(vec![Hunk {
            old_start: 0,
            old_count: 0,
            new_start,
            new_count,
            lines,
        }]),
        mtime: SystemTime::UNIX_EPOCH,
    })
}

/// Capture the current HEAD sha. Falls back to `EMPTY_TREE_SHA` in a fresh repo
/// (i.e. one with no commits yet). See ADR notes / Decision Log for rationale.
pub fn head_sha(root: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .output()
        .context("failed to spawn `git rev-parse HEAD`")?;

    if !output.status.success() {
        // Most common reason: the repository has no commits yet.
        // Fall back to the canonical empty-tree SHA so downstream code can
        // treat every worktree file as an addition without special-casing.
        return Ok(EMPTY_TREE_SHA.to_string());
    }

    let sha = String::from_utf8(output.stdout)
        .context("`git rev-parse HEAD` produced non-UTF8 output")?;
    Ok(sha.trim().to_string())
}

/// Resolve the absolute git directory (works for normal and linked worktrees).
/// See ADR-0005 for why we don't hardcode `<root>/.git`.
pub fn git_dir(root: &Path) -> Result<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--absolute-git-dir"])
        .current_dir(root)
        .output()
        .context("failed to spawn `git rev-parse --absolute-git-dir`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "`git rev-parse --absolute-git-dir` failed: {}",
            stderr.trim()
        ));
    }

    let raw = String::from_utf8(output.stdout)
        .context("`git rev-parse --absolute-git-dir` produced non-UTF8 output")?;
    Ok(PathBuf::from(raw.trim()))
}

/// Find the git worktree root from a starting path.
pub fn find_root(start: &Path) -> Result<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(start)
        .output()
        .context("failed to spawn `git rev-parse --show-toplevel`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "`git rev-parse --show-toplevel` failed: {}",
            stderr.trim()
        ));
    }

    let raw = String::from_utf8(output.stdout)
        .context("`git rev-parse --show-toplevel` produced non-UTF8 output")?;
    Ok(PathBuf::from(raw.trim()))
}

/// Parse a unified diff payload (the stdout of `git diff --no-renames ...`)
/// into a vector of [`FileDiff`].
pub(crate) fn parse_unified_diff(raw: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    let mut current_hunks: Vec<Hunk> = Vec::new();
    let mut current_hunk: Option<Hunk> = None;

    fn finish_hunk(current_hunk: &mut Option<Hunk>, hunks: &mut Vec<Hunk>) {
        if let Some(h) = current_hunk.take() {
            hunks.push(h);
        }
    }

    fn finish_file(
        files: &mut [FileDiff],
        current_hunks: &mut Vec<Hunk>,
        current_hunk: &mut Option<Hunk>,
    ) {
        finish_hunk(current_hunk, current_hunks);
        if let Some(file) = files.last_mut() {
            let hunks = std::mem::take(current_hunks);
            // Don't clobber a Binary marker that was set by the parser.
            if !matches!(file.content, DiffContent::Binary) {
                file.content = DiffContent::Text(hunks);
            }
        }
    }

    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            // New file — flush the previous one first.
            finish_file(&mut files, &mut current_hunks, &mut current_hunk);

            // `rest` looks like `a/foo.rs b/foo.rs`; pull the b-side path.
            let path = rest
                .split_once(" b/")
                .map(|(_, b)| PathBuf::from(b))
                .unwrap_or_default();
            files.push(FileDiff {
                path,
                status: FileStatus::Modified,
                added: 0,
                deleted: 0,
                content: DiffContent::Text(Vec::new()),
                mtime: SystemTime::UNIX_EPOCH,
            });
            continue;
        }

        if line.starts_with("Binary files ") && line.ends_with(" differ") {
            if let Some(file) = files.last_mut() {
                file.content = DiffContent::Binary;
            }
            continue;
        }

        if line.starts_with("new file mode ") {
            if let Some(file) = files.last_mut() {
                file.status = FileStatus::Added;
            }
            continue;
        }

        if line.starts_with("deleted file mode ") {
            if let Some(file) = files.last_mut() {
                file.status = FileStatus::Deleted;
            }
            continue;
        }

        if let Some(rest) = line.strip_prefix("@@ ") {
            // Hunk header: `@@ -old_start,old_count +new_start,new_count @@`
            finish_hunk(&mut current_hunk, &mut current_hunks);
            let header = rest.trim_end_matches(" @@").trim_end_matches("@@");
            let mut parts = header.split_whitespace();
            let old = parts.next().unwrap_or("-0,0");
            let new = parts.next().unwrap_or("+0,0");
            let (old_start, old_count) = parse_hunk_range(old.trim_start_matches('-'));
            let (new_start, new_count) = parse_hunk_range(new.trim_start_matches('+'));
            current_hunk = Some(Hunk {
                old_start,
                old_count,
                new_start,
                new_count,
                lines: Vec::new(),
            });
            continue;
        }

        if let Some(hunk) = current_hunk.as_mut() {
            if let Some(content) = line.strip_prefix('+') {
                hunk.lines.push(DiffLine {
                    kind: LineKind::Added,
                    content: content.to_string(),
                });
                if let Some(file) = files.last_mut() {
                    file.added += 1;
                }
                continue;
            }
            if let Some(content) = line.strip_prefix('-') {
                hunk.lines.push(DiffLine {
                    kind: LineKind::Deleted,
                    content: content.to_string(),
                });
                if let Some(file) = files.last_mut() {
                    file.deleted += 1;
                }
                continue;
            }
            if let Some(content) = line.strip_prefix(' ') {
                hunk.lines.push(DiffLine {
                    kind: LineKind::Context,
                    content: content.to_string(),
                });
                continue;
            }
        }
        // Other header lines (`index ...`, `--- a/...`, `+++ b/...`) are ignored
        // for now; M1.4/M1.5 will refine them.
    }

    // Flush trailing hunk + file.
    finish_file(&mut files, &mut current_hunks, &mut current_hunk);
    files
}

/// Parse `start,count` (or just `start`, defaulting count to 1) from a hunk header range.
fn parse_hunk_range(spec: &str) -> (usize, usize) {
    match spec.split_once(',') {
        Some((start, count)) => (start.parse().unwrap_or(0), count.parse().unwrap_or(0)),
        None => (spec.parse().unwrap_or(0), 1),
    }
}

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

        let files = parse_unified_diff(raw);

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

        let files = parse_unified_diff(raw);
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

        let files = parse_unified_diff(raw);
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

        let files = parse_unified_diff(raw);
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
        let files = parse_unified_diff(raw);
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
        let files = parse_unified_diff(raw);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, PathBuf::from("icon.png"));
        assert!(matches!(files[0].content, DiffContent::Binary));
        assert_eq!(files[0].added, 0);
        assert_eq!(files[0].deleted, 0);
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
        let files = parse_unified_diff(raw);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].status, FileStatus::Deleted);
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
    fn compute_diff_caps_untracked_file_at_read_limit() {
        let repo = init_repo();
        fs::write(repo.path().join("seed.txt"), "seed").expect("write seed");
        run_git(repo.path(), &["add", "seed.txt"]);
        run_git(repo.path(), &["commit", "--quiet", "-m", "initial"]);
        let baseline = head_sha(repo.path()).expect("head_sha");

        // 200 lines × 100 bytes = 20000 bytes (well over the 8KB cap).
        let line: String = "x".repeat(99);
        let body = (0..200)
            .map(|_| line.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(repo.path().join("big.txt"), &body).expect("write big");
        assert!(body.len() > UNTRACKED_READ_CAP);

        let files = compute_diff(repo.path(), &baseline).expect("compute_diff");
        let big = files
            .iter()
            .find(|f| f.path == Path::new("big.txt"))
            .expect("untracked big present");
        let hunks = match &big.content {
            DiffContent::Text(h) => h,
            DiffContent::Binary => panic!("unexpected binary classification"),
        };
        let total_bytes: usize = hunks[0].lines.iter().map(|l| l.content.len() + 1).sum();
        assert!(
            total_bytes <= UNTRACKED_READ_CAP + 100,
            "untracked content should be capped near {UNTRACKED_READ_CAP} bytes, got {total_bytes}"
        );
        assert!(
            big.added < 200,
            "expected fewer than 200 lines after cap, got {}",
            big.added
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
}
