use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};

use crate::git;
use crate::hook::SanitizedEvent;
use crate::stream::{build_stream_files, compute_operation_diff};

use super::{App, ViewMode};

/// Maximum number of per-file raw `git diff` snapshots retained in
/// [`App::diff_snapshots`]. The entry cost is dominated by each diff
/// string (commonly a few KB for a small edit, tens of KB for a large
/// one). 500 paths is a comfortable upper bound for agent sessions
/// that churn through many files, while capping peak memory at a few
/// MB. When the cap is hit the least-recently-touched entry is
/// evicted — the same discipline `prune_event_log` enforces for the
/// on-disk event log.
pub const DEFAULT_DIFF_SNAPSHOTS_CAP: usize = 500;

/// LRU-ish cap for per-file raw `git diff` text. Each hook event
/// touches a path (read its previous snapshot, write the new one); a
/// touch moves the path to the "most recently used" end so the next
/// eviction drops something the agent has stopped touching instead of
/// a hot file.
///
/// Intentionally a thin wrapper over [`HashMap`] + [`VecDeque`]: the
/// cardinality is bounded (<= `cap`) and the workload is "insert or
/// refresh one entry per event", so the O(n) `VecDeque::retain` on
/// re-insert is cheaper than pulling in `indexmap` or writing a
/// doubly-linked LRU for a few hundred entries.
#[derive(Debug, Clone)]
pub struct DiffSnapshots {
    map: HashMap<PathBuf, String>,
    order: VecDeque<PathBuf>,
    cap: usize,
}

impl Default for DiffSnapshots {
    fn default() -> Self {
        Self::with_cap(DEFAULT_DIFF_SNAPSHOTS_CAP)
    }
}

impl DiffSnapshots {
    pub fn with_cap(cap: usize) -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            cap: cap.max(1),
        }
    }

    pub fn get(&self, path: &Path) -> Option<&String> {
        self.map.get(path)
    }

    pub fn insert(&mut self, path: PathBuf, diff: String) {
        // Re-insert refreshes recency: pull the prior position out of
        // `order` before appending to the back, otherwise `d` getting
        // re-written would still look older than a `c` inserted once.
        if self.map.contains_key(&path) {
            self.order.retain(|p| p != &path);
        }
        self.order.push_back(path.clone());
        self.map.insert(path, diff);
        while self.map.len() > self.cap {
            if let Some(evicted) = self.order.pop_front() {
                self.map.remove(&evicted);
            } else {
                break;
            }
        }
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
    }

    #[cfg(test)]
    pub fn contains_key(&self, path: &Path) -> bool {
        self.map.contains_key(path)
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    #[cfg(test)]
    pub fn keys(&self) -> impl Iterator<Item = &PathBuf> {
        self.map.keys()
    }

    /// Replace all entries with the ones from `map`, preserving the
    /// configured cap. Iteration order of [`HashMap`] is unspecified,
    /// so new entries land in whatever order the incoming map yields
    /// — acceptable here because the caller only needs "recent after
    /// bootstrap", not a precise total order.
    pub fn replace_from_map(&mut self, map: HashMap<PathBuf, String>) {
        self.clear();
        for (path, diff) in map {
            self.insert(path, diff);
        }
    }
}

/// One entry in the stream mode view. Combines the sanitized event
/// metadata (from `hook-log-event`) with optionally captured diff
/// snapshots for per-operation diff display.
#[derive(Debug, Clone)]
pub struct StreamEvent {
    pub metadata: SanitizedEvent,
    /// Per-file operation diffs captured by the TUI in real-time,
    /// keyed by the same path entries as `metadata.file_paths`.
    /// A `MultiEdit` or multi-file `Write` carries one entry per
    /// touched path so the stream view can render per-path hunks
    /// instead of collapsing everything onto the first path.
    /// Empty for events that occurred before the TUI was started.
    pub per_file_diffs: BTreeMap<PathBuf, String>,
}

impl App {
    /// Canonicalize an agent-supplied path and project it onto the
    /// repo-relative form used by `git::compute_diff` outputs. Returns
    /// `None` when the path does not resolve inside `self.root`.
    ///
    /// `self.root` is canonicalized by `git::find_root`
    /// (`git rev-parse --show-toplevel`), so on macOS it lives on the
    /// `/private/...` side of the `/tmp` → `/private/tmp` symlink.
    /// Agent-provided file paths often follow the symlinked side
    /// instead; `canonicalize` resolves both to the same absolute
    /// path so downstream keys (`diff_snapshots`, `per_file_diffs`,
    /// `StreamEvent.metadata.file_paths`) stay stable regardless of
    /// symlink spelling.
    ///
    /// Falls back to the raw path when the file no longer exists
    /// (deleted between the hook write and this handler); in that
    /// case we still try to strip `self.root` so a fresh path that
    /// already starts with the canonical root becomes repo-relative.
    fn normalize_event_path(&self, p: &Path) -> Option<PathBuf> {
        let canon = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        if let Ok(rel) = canon.strip_prefix(&self.root) {
            return Some(rel.to_path_buf());
        }
        if let Ok(rel) = p.strip_prefix(&self.root) {
            return Some(rel.to_path_buf());
        }
        // Already repo-relative? Accept only when every component is
        // forward-only (no `..` traversal) AND the resolved absolute
        // form still lives inside `self.root`. Checking parent
        // components is required because
        // `root.join("../outside.rs").starts_with(root)` is true
        // lexically — the filesystem escape happens only at resolve
        // time. This is the security boundary for stream mode's
        // repo filter.
        if p.is_relative()
            && p.components()
                .all(|c| !matches!(c, std::path::Component::ParentDir))
            && self.root.join(p).starts_with(&self.root)
        {
            return Some(p.to_path_buf());
        }
        None
    }

    /// Handle a new event-log file notification. Reads the event file,
    /// captures the per-operation diff snapshot, and appends to
    /// `stream_events`. Failures are silently ignored (non-critical).
    ///
    /// Idempotent per-path: if this exact event file was already
    /// ingested (e.g. replayed at startup and then re-delivered by
    /// the watcher), the call is a no-op. The path key is stable
    /// because `hook::write_event` embeds a uniqueness suffix.
    pub fn handle_event_log(&mut self, path: PathBuf) {
        if !self.processed_event_paths.insert(path.clone()) {
            return;
        }
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return,
        };
        let mut event: SanitizedEvent = match serde_json::from_str(&content) {
            Ok(e) => e,
            Err(_) => return,
        };

        // Session isolation — layer 1: drop events that predate this
        // session's start. The previous implementation used
        // `clean_stale_events` to bulk-delete the shared per-project
        // events directory, which destroyed a concurrently-running
        // kizu session's live history. Filtering on `timestamp_ms`
        // keeps our stream clean of leftover noise without touching
        // other sessions' files on disk.
        if event.timestamp_ms < self.session_start_ms {
            return;
        }

        // Session isolation — layer 2: if an **explicit** expected
        // session_id was set at bootstrap (from `KIZU_SESSION_ID`
        // or a future CLI flag), drop events that carry a
        // different session_id. Auto-binding to the first observed
        // session was intentionally removed: under concurrent
        // agent activity it locked the TUI onto whichever session
        // happened to fire first, which could easily be a foreign
        // agent and would silently hide the edits the user was
        // attached to review. Unbound sessions accept all events
        // (round-5 behavior) so the default path is non-trapping
        // when no explicit binding is provided.
        if let (Some(expected), Some(sid)) =
            (self.bound_session_id.as_ref(), event.session_id.as_ref())
            && expected != sid
        {
            return;
        }

        // Normalize every file path to repo-relative form **once**,
        // up front. Downstream snapshot lookups, per-file diff keys,
        // and the stored `metadata.file_paths` must all agree on the
        // same key shape — otherwise an event on `/abs/src/foo.rs`
        // misses the seeded `src/foo.rs` entry and the first event
        // for a pre-existing dirty file is rendered as the entire
        // cumulative diff instead of only the tool call's delta.
        // Paths that do not resolve inside the repo root are dropped
        // from the event (edits to `~/.config`, `/tmp`, etc.) so
        // they never appear as empty noise in the stream.
        let normalized: Vec<PathBuf> = event
            .file_paths
            .iter()
            .filter_map(|p| self.normalize_event_path(p))
            .collect();
        if normalized.is_empty() {
            return;
        }
        event.file_paths = normalized;

        // Capture per-operation diff for each affected file separately
        // so multi-file events keep one hunk set per path.
        let mut per_file_diffs: BTreeMap<PathBuf, String> = BTreeMap::new();

        for file_path in &event.file_paths {
            // Preserve prior snapshot on transient git failure. An
            // `Err` here (e.g. the baseline object was pruned by a
            // rebase, or the index is locked mid-operation) must not
            // clobber `diff_snapshots[file_path]` — otherwise the
            // *next* event for this file would diff against an empty
            // baseline and spuriously emit the entire cumulative diff
            // as the next operation's op_diff.
            let Ok(current_diff) = git::diff_single_file(&self.root, &self.baseline_sha, file_path)
            else {
                continue;
            };

            let op_diff = if let Some(prev) = self.diff_snapshots.get(file_path) {
                // Previous snapshot exists — compute delta.
                compute_operation_diff(prev, &current_diff)
            } else {
                // No previous snapshot (first event for this file).
                // Use the cumulative diff as the operation diff — this
                // is accurate when seed_diff_snapshots ran at startup
                // (the seed captured the pre-event state so current_diff
                // minus seed = this operation's change). For truly new
                // files, current_diff IS the operation's change.
                current_diff.clone()
            };
            if !op_diff.is_empty() {
                per_file_diffs.insert(file_path.clone(), op_diff);
            }

            self.diff_snapshots.insert(file_path.clone(), current_diff);
        }

        let stream_event = StreamEvent {
            metadata: event,
            per_file_diffs,
        };
        self.stream_events.push(stream_event);

        // If in stream mode, rebuild files/layout to include the new event.
        if self.view_mode == ViewMode::Stream {
            let stream_files = build_stream_files(&self.stream_events);
            self.apply_computed_files(stream_files);
        }
    }

    /// Scan an events directory in timestamp order and feed each
    /// `.json` file through [`Self::handle_event_log`]. Closes the
    /// gap between session startup and `watcher::start`: any
    /// `hook-log-event` written in that window lands on disk but is
    /// never delivered by the notify backend because the watcher is
    /// not yet armed. Calling this once after watcher startup drains
    /// those files before the main loop begins.
    ///
    /// Dedup is handled by [`Self::handle_event_log`]: if the watcher
    /// later re-delivers one of these paths (some notify backends
    /// fire pre-existing entries on arm), the call becomes a no-op.
    pub fn replay_events_dir(&mut self, dir: &Path) {
        if !dir.is_dir() {
            return;
        }
        let Ok(read_dir) = std::fs::read_dir(dir) else {
            return;
        };
        // Replay order is driven by the JSON payload's
        // `timestamp_ms` (agent-side wall clock), with the on-disk
        // file modification time as the tie-break for events that
        // share a millisecond. The earlier implementation sorted by
        // filename (`<ms>-<tool>-<uniq>.json`), which meant two
        // same-millisecond events were reordered by tool name
        // instead of by actual write sequence. Because
        // `handle_event_log` mutates `diff_snapshots` in the order
        // it receives events, out-of-order replay fabricated
        // per-operation diffs and poisoned the baseline the next
        // event diffed against.
        //
        // Tie-break chain: (timestamp_ms, mtime, path).
        let mut entries: Vec<(u64, std::time::SystemTime, PathBuf)> = Vec::new();
        for entry in read_dir.flatten() {
            let name = entry.file_name();
            let s = name.to_string_lossy();
            if s.starts_with('.') || !s.ends_with(".json") {
                continue;
            }
            let path = entry.path();
            let ts_ms = std::fs::read_to_string(&path)
                .ok()
                .and_then(|c| serde_json::from_str::<SanitizedEvent>(&c).ok())
                .map(|e| e.timestamp_ms)
                .unwrap_or(0);
            let mtime = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            entries.push((ts_ms, mtime, path));
        }
        entries.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| a.1.cmp(&b.1))
                .then_with(|| a.2.cmp(&b.2))
        });
        for (_, _, path) in entries {
            self.handle_event_log(path);
        }
    }
}
