use anyhow::Result;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct FileDiff {
    pub path: PathBuf,
    pub status: FileStatus,
    pub added: usize,
    pub deleted: usize,
    pub hunks: Vec<Hunk>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
}

#[derive(Debug, Clone)]
pub struct Hunk {
    pub old_start: usize,
    pub old_count: usize,
    pub new_start: usize,
    pub new_count: usize,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone)]
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

/// Run `git diff --no-renames <baseline> --` and parse the result.
///
/// TODO v0.1:
///   - shell out to `git diff --no-renames <baseline_sha> --`
///   - parse unified diff into FileDiff/Hunk/DiffLine
///   - merge `git status --porcelain` for untracked files
pub fn compute_diff(_root: &std::path::Path, _baseline_sha: &str) -> Result<Vec<FileDiff>> {
    Ok(Vec::new())
}

/// Capture the current HEAD sha as the session baseline.
pub fn head_sha(_root: &std::path::Path) -> Result<String> {
    // TODO v0.1: shell out to `git rev-parse HEAD`
    Ok(String::new())
}

/// Find the git worktree root from a starting path.
pub fn find_root(_start: &std::path::Path) -> Result<std::path::PathBuf> {
    // TODO v0.1: shell out to `git rev-parse --show-toplevel`
    Ok(std::env::current_dir()?)
}
