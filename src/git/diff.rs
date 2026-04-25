use anyhow::{Context, Result, anyhow};
use std::path::{Path, PathBuf};
use std::process::Command;

use super::untracked::{
    is_untracked_and_visible, list_untracked, synthesize_untracked, synthesize_untracked_diff_text,
};
use super::{FileDiff, parse::parse_unified_diff};

/// Run `git diff --no-renames <baseline> --` and parse the result, then
/// append synthesized [`FileDiff`] entries for untracked files.
///
/// The `--no-renames` flag (ADR-0001) keeps the parser simple and avoids
/// rename detection diverging from the user's mental model.
/// Get the unified diff for a single file against the baseline.
/// Returns `Err` when `git diff` exits non-zero (missing baseline
/// object, invalid path, broken index) so callers can preserve prior
/// state rather than treating the empty-stdout case as "no diff".
pub fn diff_single_file(root: &Path, baseline_sha: &str, file_path: &Path) -> Result<String> {
    let rel = file_path.strip_prefix(root).unwrap_or(file_path);
    let output = Command::new("git")
        .args([
            "diff",
            "--no-renames",
            baseline_sha,
            "--",
            &rel.to_string_lossy(),
        ])
        .current_dir(root)
        .output()
        .context("git diff single file")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("git diff single file failed: {}", stderr.trim()));
    }
    let raw = String::from_utf8_lossy(&output.stdout).into_owned();
    if !raw.is_empty() {
        return Ok(raw);
    }
    // Empty stdout is ambiguous: the path may be tracked-but-unchanged
    // *or* untracked-and-visible (git diff ignores untracked entries
    // entirely) *or* gitignored. Stream mode needs the new file's
    // contents only in the untracked-and-visible case — gitignored
    // files must stay out of stream snapshots to preserve the same
    // confidentiality boundary `compute_diff` already honors.
    //
    // Any failure of the classification step or the synthesis step
    // (other than `NotFound`, which is just a TOCTOU race) surfaces
    // as `Err` so the caller preserves its prior snapshot instead of
    // storing an accidental empty.
    if is_untracked_and_visible(root, rel)? {
        match synthesize_untracked_diff_text(root, rel) {
            Ok(text) => return Ok(text),
            Err(e) => {
                let vanished = e.chain().any(|cause| {
                    cause
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound)
                });
                if vanished {
                    return Ok(String::new());
                }
                return Err(e)
                    .with_context(|| format!("synthesizing untracked snapshot {}", rel.display()));
            }
        }
    }
    Ok(raw)
}

pub fn compute_diff(root: &Path, baseline_sha: &str) -> Result<Vec<FileDiff>> {
    compute_diff_with_snapshots(root, baseline_sha).map(|(files, _)| files)
}

/// Same as [`compute_diff`] but also returns per-file raw `git diff`
/// text suitable for seeding `App::diff_snapshots`. Collapses the
/// previous bootstrap pattern of "compute_diff + N × diff_single_file"
/// into the single `git diff` invocation `compute_diff` already
/// issued — the raw output was being discarded by the parser, so the
/// seed loop was paying N subprocess startups for data we already had.
pub fn compute_diff_with_snapshots(
    root: &Path,
    baseline_sha: &str,
) -> Result<(Vec<FileDiff>, std::collections::HashMap<PathBuf, String>)> {
    let output = Command::new("git")
        .args(["diff", "--no-renames", baseline_sha, "--"])
        .current_dir(root)
        .output()
        .context("failed to spawn `git diff --no-renames`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("`git diff` failed: {}", stderr.trim()));
    }

    // Use a lossy decode here: git can legitimately emit raw
    // non-UTF-8 bytes inside tracked content (legacy-encoded text
    // fixtures, e.g. Shift-JIS or Latin-1) and a strict decode would
    // abort the **entire** refresh over one problematic file, freezing
    // the UI on a stale snapshot. The untracked path already goes
    // byte-oriented in `list_untracked` / `bytes_to_path`; the tracked
    // payload now matches. Invalid bytes become U+FFFD in the display,
    // which is a tolerable cost for preserving refresh liveness.
    let raw = String::from_utf8_lossy(&output.stdout);
    let mut files = parse_unified_diff(&raw).context("parsing git diff output")?;

    let mut snapshots: std::collections::HashMap<PathBuf, String> =
        split_raw_diff_by_file(&raw, &files);

    for rel in list_untracked(root)? {
        match synthesize_untracked(root, &rel) {
            Ok(synth) => {
                // Synthesize the `git diff`-shaped text for this
                // untracked file so its snapshot has the same shape
                // that `diff_single_file` would produce — otherwise
                // the first stream event for the file would compare
                // against an empty string and mis-attribute the whole
                // file as the operation's change.
                match synthesize_untracked_diff_text(root, &rel) {
                    Ok(text) => {
                        snapshots.insert(synth.path.clone(), text);
                    }
                    Err(e) => {
                        let vanished = e.chain().any(|cause| {
                            cause
                                .downcast_ref::<std::io::Error>()
                                .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound)
                        });
                        if !vanished {
                            return Err(e).with_context(|| {
                                format!("synthesizing untracked snapshot {}", rel.display())
                            });
                        }
                        // Vanished between status and read: skip the
                        // snapshot entry; the FileDiff from the first
                        // read still pushes so layout stays consistent.
                    }
                }
                files.push(synth);
            }
            // If the file genuinely vanished between `status` and our
            // read (an agent deleted it in the same burst), skip it.
            // Any other failure (pathname parse bug, decode bug, …)
            // must surface so tests catch it instead of the file
            // silently disappearing from the TUI.
            Err(e) => {
                let vanished = e.chain().any(|cause| {
                    cause
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound)
                });
                if vanished {
                    continue;
                }
                return Err(e)
                    .with_context(|| format!("synthesizing untracked file {}", rel.display()));
            }
        }
    }

    Ok((files, snapshots))
}

/// Split a concatenated `git diff` payload at each `diff --git` header
/// and assign the resulting fragment to the `FileDiff.path` captured
/// by the parser for that header. The fragment includes the header
/// line itself and ends with a trailing newline, matching the byte
/// shape `git diff --no-renames <baseline> -- <path>` produces when
/// invoked for that single file.
fn split_raw_diff_by_file(
    raw: &str,
    files: &[FileDiff],
) -> std::collections::HashMap<PathBuf, String> {
    let mut snapshots = std::collections::HashMap::new();
    if files.is_empty() || raw.is_empty() {
        return snapshots;
    }

    let mut file_idx = 0usize;
    let mut current = String::new();

    for line in raw.lines() {
        if line.starts_with("diff --git ")
            && !current.is_empty()
            && let Some(file) = files.get(file_idx)
        {
            snapshots.insert(file.path.clone(), std::mem::take(&mut current));
            file_idx += 1;
        }
        current.push_str(line);
        current.push('\n');
    }
    if !current.is_empty()
        && let Some(file) = files.get(file_idx)
    {
        snapshots.insert(file.path.clone(), current);
    }

    snapshots
}
