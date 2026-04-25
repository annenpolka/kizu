mod event_log;
mod input;
mod output;
mod scan;
mod session_files;
mod types;

#[cfg(test)]
pub use event_log::prune_event_log_in;
pub use event_log::{prune_event_log, sanitize_event, write_event};
pub use input::parse_hook_input;
pub use output::{format_additional_context, format_stop_stderr};
pub use scan::{scan_scars, scan_scars_from_index};
pub use session_files::enumerate_session_files;
pub use types::{AgentKind, NormalizedHookInput, SanitizedEvent, ScarHit};

#[cfg(test)]
mod tests;
