use std::path::{Path, PathBuf};

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
}
