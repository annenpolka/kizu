use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use crate::app::StreamEvent;
use crate::git::{self, DiffContent, FileDiff, LineKind};

/// Convert stream events into virtual [`FileDiff`] entries so the
/// existing scroll infrastructure can render them identically to
/// git diff output. Each event becomes one `FileDiff` with:
/// - `header_prefix`: "HH:MM:SS Tool" for display in the file header
/// - `path`: the edited file path
/// - `content`: parsed diff lines from the captured snapshot
pub(crate) fn build_stream_files(events: &[StreamEvent]) -> Vec<FileDiff> {
    let mut out: Vec<FileDiff> = Vec::new();
    for (i, ev) in events.iter().enumerate() {
        let ts = ev.metadata.timestamp_ms;
        let time_str = crate::ui::format_local_time(ts);
        let tool = ev.metadata.tool_name.as_deref().unwrap_or("?");
        let prefix = format!("{time_str} {tool}");
        let mtime = SystemTime::UNIX_EPOCH + Duration::from_millis(ts);

        // Use `file_paths` order as the stable render order so a
        // multi-file tool call presents files in the order the agent
        // reported them, not in the BTreeMap's sort order.
        let paths: Vec<PathBuf> = if ev.metadata.file_paths.is_empty() {
            // Preserve the "empty placeholder" behavior for events
            // whose file_paths could not be resolved — they still
            // need to be visible in the stream as a metadata row.
            vec![PathBuf::new()]
        } else {
            ev.metadata.file_paths.clone()
        };

        // Space each (event, path) pair's old_start apart so hunk
        // anchors (keyed on path + old_start) stay unique across
        // events and paths.
        for (j, path) in paths.into_iter().enumerate() {
            let anchor_base = (i * 10_000) + (j * 100) + 1;
            let diff_text = ev.per_file_diffs.get(&path);
            let (hunks, added, deleted) = match diff_text {
                Some(t) if !t.is_empty() => parse_stream_diff_to_hunk(t, anchor_base),
                _ => (vec![], 0, 0),
            };

            out.push(FileDiff {
                path,
                status: git::FileStatus::Modified,
                added,
                deleted,
                content: DiffContent::Text(hunks),
                mtime,
                header_prefix: Some(prefix.clone()),
            });
        }
    }
    out
}

/// Parse raw diff text (from a stream event snapshot) into a single
/// `Hunk` with `DiffLine` entries. Hunk header lines (`@@`) are
/// skipped; `+`/`-`/` ` prefix determines `LineKind`.
fn parse_stream_diff_to_hunk(diff_text: &str, old_start: usize) -> (Vec<git::Hunk>, usize, usize) {
    let mut lines = Vec::new();
    let mut added = 0usize;
    let mut deleted = 0usize;
    for raw in diff_text.lines() {
        if raw.starts_with("@@")
            || raw.starts_with("diff ")
            || raw.starts_with("---")
            || raw.starts_with("+++")
            || raw.starts_with("index ")
        {
            continue;
        }
        let (kind, content) = if let Some(rest) = raw.strip_prefix('+') {
            added += 1;
            (LineKind::Added, rest.to_string())
        } else if let Some(rest) = raw.strip_prefix('-') {
            deleted += 1;
            (LineKind::Deleted, rest.to_string())
        } else if let Some(rest) = raw.strip_prefix(' ') {
            (LineKind::Context, rest.to_string())
        } else {
            (LineKind::Context, raw.to_string())
        };
        lines.push(git::DiffLine {
            kind,
            content,
            has_trailing_newline: true,
        });
    }
    if lines.is_empty() {
        return (vec![], 0, 0);
    }
    let hunk = git::Hunk {
        old_start,
        old_count: deleted,
        new_start: old_start,
        new_count: added,
        lines,
        context: None,
    };
    (vec![hunk], added, deleted)
}

/// Compute the "operation diff" — the lines in `current` that were
/// not already present in `previous`, counted as a **multiset** so
/// duplicate lines (e.g. two blank `+` lines, or two identical
/// closing-brace context rows) survive when `current` has more copies
/// than `previous`.
///
/// This is not a true diff-of-diff — hunk boundaries, line numbers,
/// and ordering drift are ignored. In practice the cumulative
/// snapshots differ by the lines one Write/Edit operation added or
/// re-shaped, so a multiset difference gives a readable approximation.
/// Limitations and design rationale are documented in ADR-0016.
pub(crate) fn compute_operation_diff(previous: &str, current: &str) -> String {
    use std::collections::HashMap;
    let mut prev_counts: HashMap<&str, usize> = HashMap::new();
    for line in previous.lines() {
        *prev_counts.entry(line).or_insert(0) += 1;
    }
    let mut result = String::new();
    for line in current.lines() {
        match prev_counts.get_mut(line) {
            Some(count) if *count > 0 => {
                *count -= 1;
            }
            _ => {
                result.push_str(line);
                result.push('\n');
            }
        }
    }
    result
}
