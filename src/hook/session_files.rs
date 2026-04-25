use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// List files that might contain scars, scoped by the kizu session
/// baseline. Checks for a session file; if found, scans only files
/// changed since that baseline (committed + uncommitted + untracked).
/// Falls back to all tracked + untracked if no session is active.
pub fn enumerate_session_files(root: &Path) -> Result<Vec<PathBuf>> {
    use std::process::Command;

    let session = crate::session::read_session(root);
    let baseline = session
        .as_ref()
        .filter(|s| crate::session::is_session_alive(s))
        .map(|s| s.baseline_sha.as_str());

    let mut paths: Vec<PathBuf> = Vec::new();

    if let Some(base) = baseline {
        // Fail-closed: if any baseline-scoped diff fails (e.g. stale
        // session, rebase, corrupt SHA), fall back to a full tracked
        // scan instead of silently under-scanning.
        let mut any_failed = false;

        // Files changed since baseline (includes commits made during
        // the session): `git diff <baseline>..HEAD --name-only`.
        let diff_base = Command::new("git")
            .args(["diff", "--name-only", "-z", &format!("{base}..HEAD"), "--"])
            .current_dir(root)
            .output()
            .context("git diff baseline..HEAD")?;
        if diff_base.status.success() {
            extend_from_nul_list(&mut paths, root, &diff_base.stdout);
        } else {
            any_failed = true;
        }

        // Uncommitted changes (staged + unstaged).
        let diff_head = Command::new("git")
            .args(["diff", "--name-only", "-z", "HEAD", "--"])
            .current_dir(root)
            .output()
            .context("git diff HEAD")?;
        if diff_head.status.success() {
            extend_from_nul_list(&mut paths, root, &diff_head.stdout);
        } else {
            any_failed = true;
        }

        // Staged but not yet in HEAD.
        let diff_cached = Command::new("git")
            .args(["diff", "--cached", "--name-only", "-z", "--"])
            .current_dir(root)
            .output()
            .context("git diff --cached")?;
        if diff_cached.status.success() {
            extend_from_nul_list(&mut paths, root, &diff_cached.stdout);
        } else {
            any_failed = true;
        }

        // If any baseline diff failed, discard partial results and
        // fall through to full tracked scan so we don't miss scars.
        if any_failed {
            paths.clear();
            let ls_output = Command::new("git")
                .args(["ls-files", "-z"])
                .current_dir(root)
                .output()
                .context("git ls-files (fallback)")?;
            if ls_output.status.success() {
                extend_from_nul_list(&mut paths, root, &ls_output.stdout);
            }
        }
    } else {
        // No session → fallback to all tracked files.
        let ls_output = Command::new("git")
            .args(["ls-files", "-z"])
            .current_dir(root)
            .output()
            .context("git ls-files")?;
        if ls_output.status.success() {
            extend_from_nul_list(&mut paths, root, &ls_output.stdout);
        }
    }

    // Untracked files (always included).
    let status_output = Command::new("git")
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .current_dir(root)
        .output()
        .context("git status --porcelain")?;
    if status_output.status.success() {
        for record in status_output.stdout.split(|&b| b == 0) {
            if record.len() >= 3 && &record[..3] == b"?? " {
                let path_bytes = &record[3..];
                let rel = String::from_utf8_lossy(path_bytes);
                paths.push(root.join(rel.as_ref()));
            }
        }
    }

    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// Extend `paths` with each NUL-separated relative path in `stdout`,
/// joined onto `root`. Shared by the four `git diff --name-only -z` /
/// `git ls-files -z` branches in [`enumerate_session_files`].
fn extend_from_nul_list(paths: &mut Vec<PathBuf>, root: &Path, stdout: &[u8]) {
    for record in stdout.split(|&b| b == 0) {
        if !record.is_empty() {
            let rel = String::from_utf8_lossy(record);
            paths.push(root.join(rel.as_ref()));
        }
    }
}
