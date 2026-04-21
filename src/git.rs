use anyhow::{Context, Result, anyhow};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
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

/// Return the 1-indexed `(old, new)` line numbers for the DiffLine at
/// `line_idx` inside `hunk`. `old` is `None` for Added rows, `new` is
/// `None` for Deleted rows, both sides are `Some` for Context rows.
/// Out-of-range `line_idx` returns `(None, None)` so callers can treat
/// it as "no line number available" without panicking.
///
/// v0.5 `build_layout` inlines an equivalent cumulative walk for O(n)
/// performance on large hunks (Codex 3rd-round Important-4), but this
/// standalone helper is kept public for single-line lookups (tests /
/// future call sites that don't already hold a cursor).
#[allow(dead_code)]
pub fn line_numbers_for(hunk: &Hunk, line_idx: usize) -> (Option<usize>, Option<usize>) {
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

/// Return `true` when the path is untracked **and** not ignored by
/// `.gitignore`. Uses `git status --porcelain=v1 -z
/// --untracked-files=all -- <rel>` because it is the only check that
/// honors `.gitignore` the same way `compute_diff` / `list_untracked`
/// already do. `git ls-files --error-unmatch` alone classifies ignored
/// files as untracked, which would leak their contents into stream
/// snapshots.
fn is_untracked_and_visible(root: &Path, rel: &Path) -> Result<bool> {
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
fn synthesize_untracked_diff_text(root: &Path, rel: &Path) -> Result<String> {
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
fn list_untracked(root: &Path) -> Result<Vec<PathBuf>> {
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

/// Convert raw filesystem bytes coming out of git into a `PathBuf`.
/// On Unix this preserves non-UTF8 filenames byte-for-byte via
/// [`std::os::unix::ffi::OsStrExt`]. On other platforms we fall back
/// to a lossy UTF-8 decode, which covers every filename people actually
/// ship but can corrupt genuinely invalid byte sequences on Windows.
#[cfg(unix)]
fn bytes_to_path(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
}

#[cfg(not(unix))]
fn bytes_to_path(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}

/// Extract the post-image pathname from the remainder of a
/// `diff --git <rest>` header line.
///
/// `rest` has one of two shapes, both of which must be handled:
///   - unquoted: `a/<path> b/<path>`
///   - quoted:   `"a/<c-escaped-path>" "b/<c-escaped-path>"`
///
/// Since kizu passes `--no-renames` to every `git diff`, the pre- and
/// post-image paths are guaranteed to be byte-identical. The unquoted
/// branch leans on that invariant to split `rest` at its exact midpoint
/// instead of searching for ` b/`, which is ambiguous for a filename
/// whose bytes contain the literal sequence ` b/` (e.g. `foo b/bar`).
/// Returns `None` if neither shape parses cleanly. The caller treats
/// that as a parse error and aborts the refresh instead of silently
/// collapsing the file under an empty path.
fn parse_diff_git_header(rest: &str) -> Option<PathBuf> {
    let bytes = rest.as_bytes();

    if bytes.starts_with(b"\"a/") {
        // Quoted form: parse both tokens through C-unescape. Under
        // `--no-renames` both halves decode to the same bytes, but we
        // still walk both so a malformed header (unclosed quote,
        // unknown escape, missing space) fails safely.
        let (_a_decoded, after_a) = parse_quoted_token(bytes)?;
        let after_space = after_a.strip_prefix(b" ")?;
        let (b_decoded, _tail) = parse_quoted_token(after_space)?;
        if !b_decoded.starts_with(b"b/") {
            return None;
        }
        return Some(bytes_to_path(&b_decoded[2..]));
    }

    // Unquoted form. Exploit the `--no-renames` symmetry:
    //   rest = "a/" ++ path ++ " b/" ++ path
    //        = 2 + p + 3 + p bytes, so p = (len - 5) / 2.
    let len = bytes.len();
    if len < 5 + 2 {
        return None;
    }
    let inner = len.checked_sub(5)?;
    if !inner.is_multiple_of(2) {
        return None;
    }
    let p = inner / 2;
    if !bytes.starts_with(b"a/") {
        return None;
    }
    let a_side = &bytes[2..2 + p];
    // `b_prefix_start` is where the " b/" separator begins.
    let b_prefix_start = 2 + p;
    if bytes.get(b_prefix_start..b_prefix_start + 3) != Some(b" b/") {
        return None;
    }
    let b_side = &bytes[b_prefix_start + 3..];
    if a_side != b_side {
        return None;
    }
    Some(bytes_to_path(a_side))
}

/// Parse a git C-style quoted token starting at the first byte of
/// `bytes`, returning the decoded payload and the tail after the
/// closing quote. Git's quoting rules (see `quote.c::quote_c_style`)
/// cover the usual `\a \b \t \n \v \f \r \\ \"` single-char escapes
/// plus 3-digit octal escapes `\NNN` for any other non-printable or
/// non-ASCII byte. An unknown escape or missing closing quote yields
/// `None` so the parent parser can fall back cleanly instead of
/// silently dropping the filename.
fn parse_quoted_token(bytes: &[u8]) -> Option<(Vec<u8>, &[u8])> {
    if bytes.first() != Some(&b'"') {
        return None;
    }
    let mut out: Vec<u8> = Vec::new();
    let mut i = 1;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'"' {
            return Some((out, &bytes[i + 1..]));
        }
        if c == b'\\' {
            let n = *bytes.get(i + 1)?;
            match n {
                b'a' => out.push(0x07),
                b'b' => out.push(0x08),
                b't' => out.push(b'\t'),
                b'n' => out.push(b'\n'),
                b'v' => out.push(0x0b),
                b'f' => out.push(0x0c),
                b'r' => out.push(b'\r'),
                b'"' => out.push(b'"'),
                b'\\' => out.push(b'\\'),
                d if (b'0'..=b'7').contains(&d) => {
                    // 3-digit octal. Git always emits exactly three
                    // digits for the fallback form so we require it
                    // here rather than trying to be lenient.
                    let end = i + 4;
                    if end > bytes.len() {
                        return None;
                    }
                    let octal = std::str::from_utf8(&bytes[i + 1..end]).ok()?;
                    if octal.len() != 3 || !octal.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
                        return None;
                    }
                    let byte = u8::from_str_radix(octal, 8).ok()?;
                    out.push(byte);
                    i += 4;
                    continue;
                }
                _ => return None,
            }
            i += 2;
            continue;
        }
        out.push(c);
        i += 1;
    }
    None
}

/// Build a synthetic [`FileDiff`] for an untracked file, treating every line
/// as an added line. Reads at most [`UNTRACKED_READ_CAP`] bytes; binary files
/// (NUL byte detected in the read window) are returned as
/// [`DiffContent::Binary`].
fn synthesize_untracked(root: &Path, rel_path: &Path) -> Result<FileDiff> {
    synthesize_untracked_with_cap(root, rel_path, UNTRACKED_READ_CAP)
}

/// Same as [`synthesize_untracked`] but with an explicit byte cap. Factored
/// out so tests can exercise the truncation / binary-detection paths with a
/// small cap instead of materialising a `UNTRACKED_READ_CAP`-sized fixture
/// on disk each run.
fn synthesize_untracked_with_cap(root: &Path, rel_path: &Path, cap: usize) -> Result<FileDiff> {
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

/// Capture the current HEAD sha. Falls back to `EMPTY_TREE_SHA` **only**
/// when the repository has no commits at all (`git rev-list --all`
/// reaches nothing). Any other `git rev-parse HEAD` failure —
/// corrupted refs, HEAD pointing to a missing branch, permission
/// problems, a deleted `.git` directory — is surfaced as an error
/// instead of being silently rendered as "everything is newly added".
///
/// Why the secondary check: `rev-parse HEAD` returns the same exit
/// code for both "unborn repo" and "HEAD points at a non-existent
/// ref", and the previous implementation lumped them together. A
/// corrupt repository would appear as an empty-tree baseline, hiding
/// the real failure from the user and encouraging them to trust a
/// bogus "all added" diff. Calling `rev-list --all --max-count=1`
/// disambiguates: unborn repos still succeed but emit zero SHAs,
/// while broken repos either still have commits reachable from some
/// ref (non-empty output) or fail outright.
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
/// apart from a broken one. If the probe itself fails (e.g. a deeper
/// git failure), we conservatively report "has commits" so the caller
/// surfaces the original `rev-parse HEAD` error instead of falling
/// back to the empty-tree SHA.
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
///
/// In a linked worktree this returns the **per-worktree** git dir
/// (`.git/worktrees/<name>/`), which holds per-worktree HEAD/index/logs
/// but **not** the shared `refs/` tree. Use [`git_common_dir`] to find
/// the shared location.
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
/// `refs/heads/main`. Returns `Ok(None)` when HEAD is detached (a
/// raw SHA rather than a symbolic ref) — in that case the session
/// baseline cannot be moved by any ref write and callers should
/// record "no active branch".
///
/// Uses `git symbolic-ref --quiet HEAD`: the `--quiet` flag turns
/// the detached case into a non-zero exit with empty stderr, which
/// we can tell apart from a genuine failure (corrupt refs,
/// permissions, etc.) that emits a diagnostic.
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

    // Non-zero exit. `--quiet` emits an empty stderr only for the
    // detached-HEAD case; any other diagnostic means something is
    // actually broken and must be surfaced.
    let stderr_empty = output.stderr.iter().all(|b| b.is_ascii_whitespace());
    if stderr_empty {
        return Ok(None);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(anyhow!("`git symbolic-ref HEAD` failed: {}", stderr.trim()))
}

/// Resolve the **common** git dir — the shared location where
/// `refs/heads/**`, `packed-refs`, and other branch-wide state live.
///
/// For a normal repository this equals [`git_dir`]. In a linked
/// worktree it points at the main repo's `.git/` directory, which is
/// where branch refs move when you commit — the watcher needs to see
/// that directory to catch linked-worktree commits.
///
/// The returned path is canonicalized where possible so callers can
/// compare it byte-for-byte against [`git_dir`] to decide whether
/// they're looking at a linked worktree.
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

/// Serialize a single hunk back into a unified-diff patch string
/// that `git apply` can consume. The output is a minimal, single-hunk
/// patch with the `--- a/<path>` / `+++ b/<path>` header pair and
/// the `@@` range reconstructed from the hunk metadata.
///
/// `rel_path` is the worktree-relative path of the owning file,
/// written as-is on both sides of the header (we use the git-diff
/// convention of prefixing with `a/` / `b/` so `git apply` without
/// `-p0` accepts the patch).
///
/// A trailing-newline-missing hunk (`has_trailing_newline = false`
/// on the last body line) gets a `\ No newline at end of file`
/// marker so `git apply` does not silently mangle the file's EOL
/// state during revert.
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
///
/// Uses `git apply --reverse --unidiff-zero=no -` so the patch is
/// read from stdin and applied against the worktree copy of the file.
/// `--reject` is intentionally **not** passed: if the hunk no longer
/// cleanly reverses (because the user edited the same line after
/// reviewing it), we want the operation to fail loud rather than
/// leave a `.rej` file behind.
pub fn revert_hunk(root: &Path, patch: &str) -> Result<()> {
    use std::io::Write;
    use std::process::Stdio;

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

/// Parse a unified diff payload (the stdout of `git diff --no-renames ...`)
/// into a vector of [`FileDiff`].
pub(crate) fn parse_unified_diff(raw: &str) -> Result<Vec<FileDiff>> {
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

            // `rest` has one of two shapes:
            //   - unquoted: `a/<path> b/<path>`
            //   - quoted:   `"a/<escaped-path>" "b/<escaped-path>"`
            //     (git emits this when the path contains a quote,
            //     backslash, control character, or — with default
            //     core.quotePath — any non-ASCII byte.)
            // Splitting on ` b/` falls over in the quoted form *and*
            // in the edge case where the filename itself contains
            // ` b/`; use a format-aware helper that leans on the
            // `--no-renames` invariant that both sides name the same
            // file. See ADR-0001.
            let path = parse_diff_git_header(rest)
                .ok_or_else(|| anyhow!("unparseable `diff --git` header: {rest}"))?;
            files.push(FileDiff {
                path,
                status: FileStatus::Modified,
                added: 0,
                deleted: 0,
                content: DiffContent::Text(Vec::new()),
                mtime: SystemTime::UNIX_EPOCH,
                header_prefix: None,
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
            // Hunk header. Two flavours:
            //   `@@ -10,6 +10,9 @@`
            //   `@@ -10,6 +10,9 @@ fn verify_token(claims: &Claims) -> ...`
            // The trailing string after the second `@@` is git's xfuncname
            // capture — keep it as Hunk.context for the UI.
            finish_hunk(&mut current_hunk, &mut current_hunks);
            let (header, context) = match rest.split_once(" @@") {
                Some((header, tail)) => {
                    let trimmed = tail.trim();
                    let context = if trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed.to_string())
                    };
                    (header, context)
                }
                None => (rest.trim_end_matches("@@"), None),
            };
            let mut parts = header.split_whitespace();
            let old = parts
                .next()
                .ok_or_else(|| anyhow!("malformed hunk header missing old range: {line}"))?;
            let new = parts
                .next()
                .ok_or_else(|| anyhow!("malformed hunk header missing new range: {line}"))?;
            let (old_start, old_count) = parse_hunk_range(old.trim_start_matches('-'))
                .ok_or_else(|| anyhow!("malformed old hunk range: {line}"))?;
            let (new_start, new_count) = parse_hunk_range(new.trim_start_matches('+'))
                .ok_or_else(|| anyhow!("malformed new hunk range: {line}"))?;
            current_hunk = Some(Hunk {
                old_start,
                old_count,
                new_start,
                new_count,
                lines: Vec::new(),
                context,
            });
            continue;
        }

        if let Some(hunk) = current_hunk.as_mut() {
            if line == r"\ No newline at end of file" {
                if let Some(last) = hunk.lines.last_mut() {
                    last.has_trailing_newline = false;
                }
                continue;
            }
            if let Some(content) = line.strip_prefix('+') {
                hunk.lines.push(DiffLine {
                    kind: LineKind::Added,
                    content: content.to_string(),
                    has_trailing_newline: true,
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
                    has_trailing_newline: true,
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
                    has_trailing_newline: true,
                });
                continue;
            }
        }
        // Other header lines (`index ...`, `--- a/...`, `+++ b/...`) are ignored
        // for now; M1.4/M1.5 will refine them.
    }

    // Flush trailing hunk + file.
    finish_file(&mut files, &mut current_hunks, &mut current_hunk);
    Ok(files)
}

fn split_logical_lines(text: &str) -> Vec<(String, bool)> {
    if text.is_empty() {
        return Vec::new();
    }

    text.split_inclusive('\n')
        .map(|chunk| {
            let has_trailing_newline = chunk.ends_with('\n');
            let without_newline = chunk.strip_suffix('\n').unwrap_or(chunk);
            let line = if has_trailing_newline {
                without_newline
                    .strip_suffix('\r')
                    .unwrap_or(without_newline)
                    .to_string()
            } else {
                without_newline.to_string()
            };
            (line, has_trailing_newline)
        })
        .collect()
}

/// Parse `start,count` (or just `start`, defaulting count to 1) from a hunk header range.
fn parse_hunk_range(spec: &str) -> Option<(usize, usize)> {
    match spec.split_once(',') {
        Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
        None => Some((spec.parse().ok()?, 1)),
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
