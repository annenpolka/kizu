use std::path::PathBuf;
use std::time::SystemTime;

/// Maximum number of bytes read from an untracked file when synthesizing its
/// "all-added" diff. 64 MiB is effectively unlimited for source / text files
/// while keeping an OOM guard against pathological large binaries that an
/// agent might accidentally drop into the worktree. See plans/v0.3.2.md.
pub const UNTRACKED_READ_CAP: usize = 64 * 1024 * 1024;

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
    /// Optional label prepended to the file header in the TUI.
    /// Stream mode uses this for "HH:MM:SS Write" etc.
    /// `None` for normal git diff entries.
    pub header_prefix: Option<String>,
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
    /// Trailing context that git puts after the closing `@@` of a hunk
    /// header — usually the enclosing function signature for languages
    /// where git's xfuncname pattern is configured (Rust, C, Go, Python,
    /// JS/TS, Ruby, …). The UI uses this as a human-readable hunk title
    /// instead of `@@ -10,6 +10,9 @@`.
    pub context: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub content: String,
    /// Whether this logical diff line ended with a real newline in the
    /// source material. Git signals a missing terminal newline via the
    /// standalone marker `\ No newline at end of file`; wrap-mode UI uses
    /// this to decide whether to draw the `¶` newline marker honestly.
    pub has_trailing_newline: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Deleted,
}

/// Test-only reference implementation of per-line number resolution.
/// Pins the semantics (Context → both sides, Added → new only,
/// Deleted → old only) as a single-line spec that the inline
/// cumulative walk in `App::build_layout` must match. Production
/// rendering uses that inline O(n) walk instead of calling this
/// O(line_idx) helper per line (see Codex 3rd-round Important-4).
#[cfg(test)]
pub(crate) fn line_numbers_for(hunk: &Hunk, line_idx: usize) -> (Option<usize>, Option<usize>) {
    let mut old = hunk.old_start;
    let mut new = hunk.new_start;
    for (i, line) in hunk.lines.iter().enumerate() {
        if i == line_idx {
            return match line.kind {
                LineKind::Context => (Some(old), Some(new)),
                LineKind::Added => (None, Some(new)),
                LineKind::Deleted => (Some(old), None),
            };
        }
        match line.kind {
            LineKind::Context => {
                old += 1;
                new += 1;
            }
            LineKind::Added => {
                new += 1;
            }
            LineKind::Deleted => {
                old += 1;
            }
        }
    }
    (None, None)
}
