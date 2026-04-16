use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize, Deserialize)]
pub struct SessionInfo {
    pub baseline_sha: String,
    pub pid: u32,
    pub root: PathBuf,
}

/// Write the session file for the given project root.
pub fn write_session(root: &Path, baseline_sha: &str) -> Result<()> {
    let path = crate::paths::session_file(root)
        .ok_or_else(|| anyhow::anyhow!("cannot resolve kizu state directory"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating session dir {}", parent.display()))?;
    }
    let info = SessionInfo {
        baseline_sha: baseline_sha.to_string(),
        pid: std::process::id(),
        root: root.to_path_buf(),
    };
    let json = serde_json::to_string(&info)?;
    std::fs::write(&path, json)
        .with_context(|| format!("writing session file {}", path.display()))?;
    Ok(())
}

/// Read the session file for the given project root.
/// Returns `None` if the file doesn't exist or is unreadable.
pub fn read_session(root: &Path) -> Option<SessionInfo> {
    let path = crate::paths::session_file(root)?;
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Delete the session file for the given project root.
/// Silently succeeds if the file doesn't exist.
pub fn remove_session(root: &Path) {
    if let Some(path) = crate::paths::session_file(root) {
        let _ = std::fs::remove_file(path);
    }
}

/// Check if the PID in the session file is still alive.
#[cfg(unix)]
pub fn is_session_alive(info: &SessionInfo) -> bool {
    unsafe { libc::kill(info.pid as i32, 0) == 0 }
}

#[cfg(not(unix))]
pub fn is_session_alive(_info: &SessionInfo) -> bool {
    // On non-Unix, assume alive (conservative).
    true
}
