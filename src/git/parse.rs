use anyhow::{Result, anyhow};
use std::path::PathBuf;
use std::time::SystemTime;

use super::{DiffContent, DiffLine, FileDiff, FileStatus, Hunk, LineKind, path::bytes_to_path};

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

pub(in crate::git) fn split_logical_lines(text: &str) -> Vec<(String, bool)> {
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
