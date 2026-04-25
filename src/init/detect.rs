use std::path::Path;

use super::{AgentKind, DetectedAgent, SupportLevel, support_level};

/// Detect which AI coding agents are available on this system.
/// Checks binary existence via `which` and config directory presence.
pub fn detect_agents(project_root: &Path) -> Vec<DetectedAgent> {
    AgentKind::all()
        .iter()
        .map(|&kind| {
            let binary_found = which::which(kind.binary_name()).is_ok();
            let config_dir_found = kind
                .project_config_dir()
                .map(|d| project_root.join(d).is_dir())
                .unwrap_or(false)
                || kind.user_config_dir().map(|d| d.is_dir()).unwrap_or(false);
            let sl = support_level(kind);
            let recommended =
                binary_found && config_dir_found && !matches!(sl, SupportLevel::WriteSideOnly);
            DetectedAgent {
                kind,
                binary_found,
                config_dir_found,
                recommended,
            }
        })
        .collect()
}
