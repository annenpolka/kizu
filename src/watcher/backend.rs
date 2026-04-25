use std::time::Duration;

use notify_debouncer_full::{Debouncer, RecommendedCache};
#[cfg(not(target_os = "macos"))]
use notify_debouncer_full::{new_debouncer, notify::RecommendedWatcher};
#[cfg(target_os = "macos")]
use notify_debouncer_full::{
    new_debouncer_opt,
    notify::{Config as NotifyConfig, PollWatcher},
};

use super::WatchSource;

#[cfg(target_os = "macos")]
type KizuWatcher = PollWatcher;
#[cfg(not(target_os = "macos"))]
type KizuWatcher = RecommendedWatcher;
pub(in crate::watcher) type KizuDebouncer = Debouncer<KizuWatcher, RecommendedCache>;

pub(in crate::watcher) fn new_kizu_debouncer<F>(
    timeout: Duration,
    compare_contents: bool,
    event_handler: F,
) -> notify::Result<KizuDebouncer>
where
    F: notify_debouncer_full::DebounceEventHandler,
{
    #[cfg(target_os = "macos")]
    {
        // The native FSEvents-backed `RecommendedWatcher` is unreliable in
        // this project's real macOS environments and in cargo test: create
        // events can vanish entirely. PollWatcher is slower but observable.
        //
        // Keep the poll cadence below the public debounce window so the
        // worst-case latency stays close to the advertised 300ms / 100ms.
        let poll_interval = timeout.checked_div(4).unwrap_or(timeout);
        new_debouncer_opt::<F, KizuWatcher, RecommendedCache>(
            timeout,
            None,
            event_handler,
            RecommendedCache::new(),
            NotifyConfig::default()
                .with_poll_interval(poll_interval)
                .with_compare_contents(compare_contents),
        )
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = compare_contents;
        new_debouncer(timeout, None, event_handler)
    }
}

/// Format one or more notify errors into the human-readable footer
/// string the app surfaces in `last_error`. Prefixed with the
/// watcher layer so users can tell `worktree` failures apart from
/// `git_dir` failures when triaging.
pub(in crate::watcher) fn format_notify_errors(
    source: WatchSource,
    errors: &[notify::Error],
) -> String {
    let joined = errors
        .iter()
        .map(|e| e.to_string())
        .collect::<Vec<_>>()
        .join("; ");
    if joined.is_empty() {
        format!("watcher [{}]: unknown backend failure", source.label())
    } else {
        format!("watcher [{}]: {joined}", source.label())
    }
}
