use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::{NormalizedHookInput, SanitizedEvent};

/// Convert a [`NormalizedHookInput`] into a [`SanitizedEvent`],
/// stripping all code content and adding a timestamp.
pub fn sanitize_event(input: &NormalizedHookInput) -> SanitizedEvent {
    let timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    SanitizedEvent {
        session_id: input.session_id.clone(),
        hook_event_name: input.hook_event_name.clone(),
        tool_name: input.tool_name.clone(),
        file_paths: input.file_paths.clone(),
        cwd: input.cwd.clone().unwrap_or_default(),
        timestamp_ms,
    }
}

/// Write a [`SanitizedEvent`] to the events directory as an atomic
/// JSON file. Returns the path of the written file. The events
/// directory is created with `0700` permissions if it doesn't exist.
/// Individual event files are written with `0600` permissions.
///
/// Filenames include a uniqueness suffix (`pid` + nanosecond remainder)
/// so two hook processes firing in the same millisecond cannot
/// overwrite each other — the earlier format `<ms>-<tool>.json` was
/// collision-prone because `SanitizedEvent.timestamp_ms` is only
/// millisecond-precise. Temp files use the same suffix and are
/// created with `create_new` so a cross-process collision fails the
/// rename cleanly instead of silently replacing an in-flight event.
pub fn write_event(event: &SanitizedEvent) -> Result<PathBuf> {
    let dir = crate::paths::events_dir(&event.cwd)
        .ok_or_else(|| anyhow::anyhow!("cannot resolve kizu events directory"))?;
    crate::paths::ensure_private_dir(&dir)?;

    let tool = event.tool_name.as_deref().unwrap_or("unknown");
    let uniq = unique_filename_suffix();
    let filename = format!("{}-{}-{}.json", event.timestamp_ms, tool, uniq);
    let dest = dir.join(&filename);

    let json = serde_json::to_string(event).context("serializing event")?;

    // Atomic write: write to a **unique** temp path (same uniqueness
    // suffix as the final filename) then rename. `create_new`
    // equivalent via OpenOptions::create_new prevents two concurrent
    // writers from racing on the same temp file.
    let tmp_path = dir.join(format!(".{filename}.tmp"));
    {
        use std::io::Write;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts
            .open(&tmp_path)
            .with_context(|| format!("creating temp event file {}", tmp_path.display()))?;
        f.write_all(json.as_bytes())
            .with_context(|| format!("writing temp event file {}", tmp_path.display()))?;
    }

    #[cfg(unix)]
    {
        // Belt-and-braces: enforce 0600 even when OpenOptions::mode
        // is a no-op (non-Unix fallback builds).
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o600));
    }

    std::fs::rename(&tmp_path, &dest)
        .with_context(|| format!("renaming event file to {}", dest.display()))?;

    Ok(dest)
}

/// Build a per-invocation uniqueness suffix from the process id and
/// the sub-millisecond nanosecond remainder. Collisions require two
/// processes sharing the same pid **and** the same ns timestamp in
/// the same millisecond window — not possible on a single machine.
fn unique_filename_suffix() -> String {
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("{pid:x}{nanos:09}")
}

/// Prune the events directory: remove entries older than `ttl` and
/// enforce a maximum entry count. Returns the number of files removed.
/// Uses the default events directory from [`crate::paths::events_dir`].
pub fn prune_event_log(root: &Path, ttl: Duration, max_entries: usize) -> Result<usize> {
    let dir = match crate::paths::events_dir(root) {
        Some(d) if d.is_dir() => d,
        _ => return Ok(0),
    };
    prune_event_log_in(&dir, ttl, max_entries)
}

/// Prune events in the given directory. Testable variant of
/// [`prune_event_log`] that accepts an explicit path.
pub fn prune_event_log_in(dir: &Path, ttl: Duration, max_entries: usize) -> Result<usize> {
    if !dir.is_dir() {
        return Ok(0);
    }

    let mut entries: Vec<(PathBuf, u64)> = Vec::new();
    for entry in std::fs::read_dir(dir).context("reading events dir")? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        // Skip temp files.
        if name_str.starts_with('.') {
            continue;
        }
        // Parse timestamp from filename: <timestamp_ms>-<tool>.json
        if let Some(ts_str) = name_str.split('-').next()
            && let Ok(ts) = ts_str.parse::<u64>()
        {
            entries.push((entry.path(), ts));
        }
    }

    // Sort oldest first.
    entries.sort_by_key(|(_, ts)| *ts);

    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let ttl_ms = ttl.as_millis() as u64;

    let mut removed = 0;

    // Pass 1: remove entries older than TTL.
    entries.retain(|(path, ts)| {
        if now_ms.saturating_sub(*ts) > ttl_ms {
            let _ = std::fs::remove_file(path);
            removed += 1;
            false
        } else {
            true
        }
    });

    // Pass 2: enforce max entries (remove oldest first).
    if entries.len() > max_entries {
        let excess = entries.len() - max_entries;
        for (path, _) in entries.iter().take(excess) {
            let _ = std::fs::remove_file(path);
            removed += 1;
        }
    }

    Ok(removed)
}
