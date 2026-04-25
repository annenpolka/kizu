use anyhow::{Context, Result, anyhow};
use std::path::{Path, PathBuf};
use std::process::Command;

use super::EMPTY_TREE_SHA;

/// Capture the current HEAD sha. Falls back to `EMPTY_TREE_SHA` **only**
/// when the repository has no commits at all (`git rev-list --all`
/// reaches nothing). Any other `git rev-parse HEAD` failure —
/// corrupted refs, HEAD pointing to a missing branch, permission
/// problems, a deleted `.git` directory — is surfaced as an error
/// instead of being silently rendered as "everything is newly added".
pub fn head_sha(root: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .output()
        .context("failed to spawn `git rev-parse HEAD`")?;

    if output.status.success() {
        let sha = String::from_utf8(output.stdout)
            .context("`git rev-parse HEAD` produced non-UTF8 output")?;
        return Ok(sha.trim().to_string());
    }

    if repo_has_any_commit(root)? {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("`git rev-parse HEAD` failed: {}", stderr.trim()));
    }

    Ok(EMPTY_TREE_SHA.to_string())
}

/// Return `true` if any ref in the repository resolves to at least
/// one commit. Used by [`head_sha`] to tell a genuinely unborn repo
/// apart from a broken one.
fn repo_has_any_commit(root: &Path) -> Result<bool> {
    let output = Command::new("git")
        .args(["rev-list", "--all", "--max-count=1"])
        .current_dir(root)
        .output()
        .context("failed to spawn `git rev-list --all`")?;
    if !output.status.success() {
        return Ok(true);
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(!stdout.trim().is_empty())
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

/// Resolve the full ref name HEAD currently points at, e.g.
/// `refs/heads/main`. Returns `Ok(None)` when HEAD is detached.
pub fn current_branch_ref(root: &Path) -> Result<Option<String>> {
    let output = Command::new("git")
        .args(["symbolic-ref", "--quiet", "HEAD"])
        .current_dir(root)
        .output()
        .context("failed to spawn `git symbolic-ref HEAD`")?;

    if output.status.success() {
        let raw = String::from_utf8(output.stdout)
            .context("`git symbolic-ref HEAD` produced non-UTF8 output")?;
        return Ok(Some(raw.trim().to_string()));
    }

    let stderr_empty = output.stderr.iter().all(|b| b.is_ascii_whitespace());
    if stderr_empty {
        return Ok(None);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(anyhow!("`git symbolic-ref HEAD` failed: {}", stderr.trim()))
}

/// Resolve the **common** git dir — the shared location where
/// `refs/heads/**`, `packed-refs`, and other branch-wide state live.
pub fn git_common_dir(root: &Path) -> Result<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .current_dir(root)
        .output()
        .context("failed to spawn `git rev-parse --git-common-dir`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "`git rev-parse --git-common-dir` failed: {}",
            stderr.trim()
        ));
    }

    let raw = String::from_utf8(output.stdout)
        .context("`git rev-parse --git-common-dir` produced non-UTF8 output")?;
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
