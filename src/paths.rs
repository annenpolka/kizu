use std::path::{Path, PathBuf};

/// Resolve the kizu config file path.
///
/// - `$KIZU_CONFIG` override (for tests)
/// - `$XDG_CONFIG_HOME/kizu/config.toml`
/// - `~/.config/kizu/config.toml` (fallback)
#[allow(dead_code)] // Used in M2 (config file)
pub fn config_file() -> Option<PathBuf> {
    if let Ok(override_path) = std::env::var("KIZU_CONFIG") {
        return Some(PathBuf::from(override_path));
    }
    std::env::var("XDG_CONFIG_HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
        .map(|d| d.join("kizu").join("config.toml"))
}

/// Resolve the kizu state directory for session/event data.
///
/// - macOS: `~/Library/Application Support/kizu/`
/// - Linux: `$XDG_STATE_HOME/kizu/` (default `~/.local/state/kizu/`)
/// - Override: `$KIZU_STATE_DIR` (for tests)
pub fn state_dir() -> Option<PathBuf> {
    if let Ok(override_dir) = std::env::var("KIZU_STATE_DIR") {
        return Some(PathBuf::from(override_dir));
    }

    #[cfg(target_os = "macos")]
    {
        dirs::home_dir().map(|h| h.join("Library/Application Support/kizu"))
    }

    #[cfg(not(target_os = "macos"))]
    {
        std::env::var("XDG_STATE_HOME")
            .ok()
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|h| h.join(".local/state")))
            .map(|d| d.join("kizu"))
    }
}

/// Derive a short hash from the project root path, used as the
/// session file name so multiple kizu instances on different
/// projects don't collide.
pub fn project_hash(root: &Path) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    root.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Full path to the session file for a given project root.
/// Returns `None` if the state directory cannot be resolved.
pub fn session_file(root: &Path) -> Option<PathBuf> {
    state_dir().map(|d| {
        d.join("sessions")
            .join(format!("{}.json", project_hash(root)))
    })
}

/// Full path to the events directory for stream mode data.
/// Returns `None` if the state directory cannot be resolved.
pub fn events_dir() -> Option<PathBuf> {
    state_dir().map(|d| d.join("events"))
}

/// Create a directory with `0700` permissions (owner-only access).
/// Creates parent directories as needed. No-op if the directory
/// already exists with correct permissions.
#[cfg(unix)]
pub fn ensure_private_dir(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path)
        .with_context(|| format!("creating directory {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("setting permissions on {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
pub fn ensure_private_dir(path: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(path)
        .with_context(|| format!("creating directory {}", path.display()))?;
    Ok(())
}

use anyhow::Context;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn project_hash_is_deterministic() {
        let a = project_hash(Path::new("/home/user/project"));
        let b = project_hash(Path::new("/home/user/project"));
        assert_eq!(a, b);
    }

    #[test]
    fn project_hash_differs_for_different_roots() {
        let a = project_hash(Path::new("/home/user/project-a"));
        let b = project_hash(Path::new("/home/user/project-b"));
        assert_ne!(a, b);
    }

    #[test]
    fn session_file_with_override() {
        // SAFETY: test is single-threaded and restores the var immediately.
        unsafe { std::env::set_var("KIZU_STATE_DIR", "/tmp/kizu-test-state") };
        let path = session_file(Path::new("/project")).unwrap();
        unsafe { std::env::remove_var("KIZU_STATE_DIR") };
        assert!(path.starts_with("/tmp/kizu-test-state/sessions/"));
        assert!(path.to_str().unwrap().ends_with(".json"));
    }

    #[test]
    fn events_dir_with_override() {
        unsafe { std::env::set_var("KIZU_STATE_DIR", "/tmp/kizu-test-state") };
        let path = events_dir().unwrap();
        unsafe { std::env::remove_var("KIZU_STATE_DIR") };
        assert_eq!(path, PathBuf::from("/tmp/kizu-test-state/events"));
    }

    #[test]
    fn config_file_with_override() {
        unsafe { std::env::set_var("KIZU_CONFIG", "/tmp/kizu-test.toml") };
        let path = config_file().unwrap();
        unsafe { std::env::remove_var("KIZU_CONFIG") };
        assert_eq!(path, PathBuf::from("/tmp/kizu-test.toml"));
    }

    #[test]
    fn ensure_private_dir_creates_with_correct_permissions() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("private_test");
        ensure_private_dir(&dir).unwrap();
        assert!(dir.is_dir());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700);
        }
    }

    #[test]
    fn ensure_private_dir_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("idem_test");
        ensure_private_dir(&dir).unwrap();
        ensure_private_dir(&dir).unwrap(); // second call should not fail
        assert!(dir.is_dir());
    }
}
