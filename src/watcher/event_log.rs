use std::path::Path;
use std::time::Duration;

use notify::RecursiveMode;
use notify_debouncer_full::DebounceEventResult;
use tokio::sync::mpsc::UnboundedSender;

use super::WatchEvent;
use super::backend::{KizuDebouncer, new_kizu_debouncer};

/// Debounce window for the events directory — short, since each
/// hook-log-event writes exactly one file.
const EVENTS_DEBOUNCE: Duration = Duration::from_millis(100);

/// Spawn a debouncer that watches `<state_dir>/events/` for new
/// event-log files and emits [`WatchEvent::EventLog`]. Returns
/// `None` if the events directory cannot be resolved or the watcher
/// fails to start (non-fatal: stream mode simply won't get live
/// updates).
pub(in crate::watcher) fn spawn_events_dir_debouncer(
    root: &Path,
    tx: UnboundedSender<WatchEvent>,
) -> Option<KizuDebouncer> {
    let events_dir = crate::paths::events_dir(root)?;
    // Ensure the directory exists so the watcher has something to watch.
    let _ = crate::paths::ensure_private_dir(&events_dir);
    if !events_dir.is_dir() {
        return None;
    }

    let events_dir_owned = events_dir.clone();
    let mut debouncer = new_kizu_debouncer(
        EVENTS_DEBOUNCE,
        false, // No compare_contents needed for event log files
        move |result: DebounceEventResult| {
            let events = match result {
                Ok(events) => events,
                Err(_) => return, // Swallow errors — stream mode is best-effort
            };
            for event in events {
                for path in &event.event.paths {
                    // Skip temp files written by write_event.
                    if path
                        .file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with('.'))
                    {
                        continue;
                    }
                    // Only emit for files inside the events dir.
                    if path.starts_with(&events_dir_owned) {
                        let _ = tx.send(WatchEvent::EventLog(path.clone()));
                    }
                }
            }
        },
    )
    .ok()?;

    debouncer
        .watch(&events_dir, RecursiveMode::NonRecursive)
        .ok()?;
    Some(debouncer)
}
