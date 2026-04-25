use anyhow::{Context, Result, anyhow};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use super::parse::split_logical_lines;
use super::path::bytes_to_path;
use super::{DiffContent, DiffLine, FileDiff, FileStatus, Hunk, LineKind, UNTRACKED_READ_CAP};

/// Return `true` when the path is untracked **and** not ignored by
/// `.gitignore`. Uses `git status --porcelain=v1 -z
/// --untracked-files=all -- <rel>` because it is the only check that
/// honors `.gitignore` the same way `compute_diff` / `list_untracked`
/// already do. `git ls-files --error-unmatch` alone classifies ignored
/// files as untracked, which would leak their contents into stream
/// snapshots.
pub(in crate::git) fn is_untracked_and_visible(root: &Path, rel: &Path) -> Result<bool> {
    // No pre-existence check: `git status --porcelain -- <missing>`
    // returns an empty record list, which falls through to `Ok(false)`
    // below. The pre-check would also be racy (TOCTOU) with the git
    // subprocess anyway.
    let output = Command::new("git")
        .args([
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--",
            &rel.to_string_lossy(),
        ])
        .current_dir(root)
        .output()
        .context("git status --porcelain for untracked classification")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "`git status --porcelain` failed: {}",
            stderr.trim()
        ));
    }
    for record in output.stdout.split(|&b| b == 0) {
        if record.len() < 3 {
            continue;
        }
        if &record[..3] == b"?? " {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Synthesize a `git diff`-shaped text for an untracked file so it is
/// visible to downstream parsers that expect `+`-prefixed lines (see
/// `parse_stream_diff_to_hunk` and `compute_operation_diff`). The
/// output matches the skeleton produced by git for a newly-added file
/// but uses a fixed zero hash since we do not need a real object id.
pub(in crate::git) fn synthesize_untracked_diff_text(root: &Path, rel: &Path) -> Result<String> {
    let synth = synthesize_untracked(root, rel)?;
    let mut out = String::new();
    let display = rel.to_string_lossy();
    out.push_str(&format!("diff --git a/{display} b/{display}\n"));
    out.push_str("new file mode 100644\n");
    out.push_str("index 0000000..0000000\n");
    out.push_str("--- /dev/null\n");
    out.push_str(&format!("+++ b/{display}\n"));
    match synth.content {
        DiffContent::Text(hunks) => {
            for hunk in &hunks {
                out.push_str(&format!(
                    "@@ -0,0 +{},{} @@\n",
                    hunk.new_start, hunk.new_count
                ));
                for line in &hunk.lines {
                    match line.kind {
                        LineKind::Added => out.push('+'),
                        LineKind::Context => out.push(' '),
                        LineKind::Deleted => out.push('-'),
                    }
                    out.push_str(&line.content);
                    out.push('\n');
                }
            }
        }
        DiffContent::Binary => {
            out.push_str("Binary files /dev/null and b/");
            out.push_str(&display);
            out.push_str(" differ\n");
        }
    }
    Ok(out)
}

/// List untracked files reported by `git status --porcelain=v1 -z`.
///
/// Why `-z`: porcelain v1 (the default) quotes and C-escapes filenames
/// with spaces or special characters, so a plain line parser would see
/// `?? "design draft.md"` and try to open a path that includes the
/// surrounding quotes. `-z` switches to NUL-delimited records with
/// **literal** pathnames, so `design draft.md` round-trips byte-for-byte
/// and non-UTF8 filenames survive as-is on Unix.
///
/// `--untracked-files=all` is required so git expands sub-directories
/// containing only untracked files into individual entries. Without
/// it, "normal" mode collapses `scratch/a.rs` and `scratch/b.rs` into
/// a single `?? scratch/` line and kizu would try to open the
/// directory itself as a file.
pub(in crate::git) fn list_untracked(root: &Path) -> Result<Vec<PathBuf>> {
    let output = Command::new("git")
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .current_dir(root)
        .output()
        .context("failed to spawn `git status --porcelain -z`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("`git status` failed: {}", stderr.trim()));
    }

    // `-z` output is NUL-delimited. Each record is `XY <path>\0` where
    // `XY` is the two-character status code followed by a single space.
    // Non-untracked records (`M `, ` M`, `A `, …) are ignored here —
    // they already show up in `git diff` output.
    let mut paths = Vec::new();
    for record in output.stdout.split(|&b| b == 0) {
        // Skip empty records (trailing NUL, leading NUL, …).
        if record.len() < 3 {
            continue;
        }
        if &record[..3] == b"?? " {
            let path_bytes = &record[3..];
            paths.push(bytes_to_path(path_bytes));
        }
    }
    Ok(paths)
}

/// Build a synthetic [`FileDiff`] for an untracked file, treating every line
/// as an added line. Reads at most [`UNTRACKED_READ_CAP`] bytes; binary files
/// (NUL byte detected in the read window) are returned as
/// [`DiffContent::Binary`].
pub(in crate::git) fn synthesize_untracked(root: &Path, rel_path: &Path) -> Result<FileDiff> {
    synthesize_untracked_with_cap(root, rel_path, UNTRACKED_READ_CAP)
}

/// Same as [`synthesize_untracked`] but with an explicit byte cap. Factored
/// out so tests can exercise the truncation / binary-detection paths with a
/// small cap instead of materialising a `UNTRACKED_READ_CAP`-sized fixture
/// on disk each run.
pub(super) fn synthesize_untracked_with_cap(
    root: &Path,
    rel_path: &Path,
    cap: usize,
) -> Result<FileDiff> {
    let abs = root.join(rel_path);
    let total_size = std::fs::metadata(&abs)
        .with_context(|| format!("statting untracked file {}", abs.display()))?
        .len() as usize;
    let mut file = std::fs::File::open(&abs)
        .with_context(|| format!("opening untracked file {}", abs.display()))?;
    // Reserve space that matches the smaller of the file and the cap, plus
    // one so `read_to_end` can still pull `cap + 1` bytes and let us detect
    // "file is strictly larger than cap" without allocating the full cap
    // upfront for every tiny untracked entry.
    let capacity = total_size.saturating_add(1).min(cap.saturating_add(1));
    let mut buf: Vec<u8> = Vec::with_capacity(capacity);
    file.by_ref()
        .take((cap as u64).saturating_add(1))
        .read_to_end(&mut buf)
        .with_context(|| format!("reading untracked file {}", abs.display()))?;
    let truncated = buf.len() > cap;
    if truncated {
        buf.truncate(cap);
    }

    if buf.contains(&0u8) {
        return Ok(FileDiff {
            path: rel_path.to_path_buf(),
            status: FileStatus::Untracked,
            added: 0,
            deleted: 0,
            content: DiffContent::Binary,
            mtime: SystemTime::UNIX_EPOCH,
            header_prefix: None,
        });
    }

    // We may have stopped mid-codepoint at the cap boundary; fall back to a
    // lossy decode so we never refuse a valid file because of an awkward cut.
    let text = String::from_utf8_lossy(&buf);
    let lines: Vec<DiffLine> = split_logical_lines(&text)
        .into_iter()
        .map(|(line, has_trailing_newline)| DiffLine {
            kind: LineKind::Added,
            content: line,
            has_trailing_newline,
        })
        .collect();
    let mut lines = lines;
    if truncated {
        let remaining = total_size.saturating_sub(cap);
        lines.push(DiffLine {
            kind: LineKind::Context,
            content: format!("[+{remaining} more bytes from new file]"),
            has_trailing_newline: false,
        });
    }
    let added = lines
        .iter()
        .filter(|line| line.kind == LineKind::Added)
        .count();
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
            context: None,
        }]),
        mtime: SystemTime::UNIX_EPOCH,
        header_prefix: None,
    })
}
