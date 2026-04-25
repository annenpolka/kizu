use anyhow::{Context, Result, anyhow};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use super::{Hunk, LineKind};

/// Serialize a single hunk back into a unified-diff patch string
/// that `git apply` can consume.
pub fn build_hunk_patch(rel_path: &Path, hunk: &Hunk) -> String {
    let mut out = String::new();
    let display = rel_path.display();
    out.push_str(&format!("--- a/{display}\n"));
    out.push_str(&format!("+++ b/{display}\n"));
    out.push_str(&format!(
        "@@ -{},{} +{},{} @@\n",
        hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count,
    ));
    let last = hunk.lines.len().saturating_sub(1);
    for (i, line) in hunk.lines.iter().enumerate() {
        let prefix = match line.kind {
            LineKind::Context => ' ',
            LineKind::Added => '+',
            LineKind::Deleted => '-',
        };
        out.push(prefix);
        out.push_str(&line.content);
        out.push('\n');
        if i == last && !line.has_trailing_newline {
            out.push_str("\\ No newline at end of file\n");
        }
    }
    out
}

/// Apply a single-hunk patch (as produced by [`build_hunk_patch`])
/// in reverse, mutating the worktree so the target hunk is undone.
pub fn revert_hunk(root: &Path, patch: &str) -> Result<()> {
    let mut child = Command::new("git")
        .args(["apply", "--reverse", "--whitespace=nowarn", "-"])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to spawn `git apply --reverse`")?;

    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!("`git apply` stdin unavailable"))?;
        stdin
            .write_all(patch.as_bytes())
            .context("failed to write patch to `git apply` stdin")?;
    }

    let output = child
        .wait_with_output()
        .context("failed to wait for `git apply --reverse`")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("`git apply --reverse` failed: {}", stderr.trim()));
    }
    Ok(())
}
