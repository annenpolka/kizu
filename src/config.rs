use ratatui::style::Color;
use serde::Deserialize;
use std::path::Path;

/// Top-level configuration loaded from `~/.config/kizu/config.toml`.
/// All fields use `Option` wrappers so that a partial TOML file
/// merges cleanly with [`KizuConfig::default`].
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct KizuConfig {
    pub keys: KeyConfig,
    pub colors: ColorConfig,
    pub timing: TimingConfig,
    pub editor: EditorConfig,
    pub attach: AttachConfig,
}

/// Keybinding configuration. Each field holds the character that
/// triggers the corresponding action. Non-char keys (Enter, Tab,
/// arrows) are not remappable in v0.3.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct KeyConfig {
    pub ask: char,
    pub reject: char,
    pub comment: char,
    pub revert: char,
    pub editor: char,
    pub seen: char,
    pub follow: char,
    pub search: char,
    pub search_next: char,
    pub search_prev: char,
    pub picker: char,
    pub reset_baseline: char,
    pub cursor_placement: char,
    pub wrap_toggle: char,
}

/// Diff background color configuration. Each field is an `[R, G, B]`
/// triple (0–255).
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ColorConfig {
    pub bg_added: [u8; 3],
    pub bg_deleted: [u8; 3],
}

/// Debounce timing configuration (milliseconds).
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct TimingConfig {
    pub debounce_worktree_ms: u64,
    pub debounce_git_dir_ms: u64,
}

/// External editor configuration.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct EditorConfig {
    /// Override for `$EDITOR`. Empty string means "use $EDITOR".
    pub command: String,
}

/// Terminal auto-split configuration for `--attach`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AttachConfig {
    /// Force a specific terminal: "tmux", "zellij", "kitty", "ghostty".
    /// Empty string means auto-detect.
    pub terminal: String,
}

// === Defaults ===

impl Default for KeyConfig {
    fn default() -> Self {
        Self {
            ask: 'a',
            reject: 'r',
            comment: 'c',
            revert: 'x',
            editor: 'e',
            seen: ' ',
            follow: 'f',
            search: '/',
            search_next: 'n',
            search_prev: 'N',
            picker: 's',
            reset_baseline: 'R',
            cursor_placement: 'z',
            wrap_toggle: 'w',
        }
    }
}

impl Default for ColorConfig {
    fn default() -> Self {
        Self {
            bg_added: [10, 50, 10],
            bg_deleted: [60, 10, 10],
        }
    }
}

impl Default for TimingConfig {
    fn default() -> Self {
        Self {
            debounce_worktree_ms: 300,
            debounce_git_dir_ms: 100,
        }
    }
}

// === Color helpers ===

impl ColorConfig {
    pub fn bg_added_color(&self) -> Color {
        Color::Rgb(self.bg_added[0], self.bg_added[1], self.bg_added[2])
    }

    pub fn bg_deleted_color(&self) -> Color {
        Color::Rgb(self.bg_deleted[0], self.bg_deleted[1], self.bg_deleted[2])
    }
}

/// Load configuration from the config file path resolved by
/// [`crate::paths::config_file`]. Returns [`KizuConfig::default`]
/// if the file does not exist. Logs a warning to stderr and falls
/// back to defaults if the file exists but is unparseable.
pub fn load_config() -> KizuConfig {
    let path = match crate::paths::config_file() {
        Some(p) => p,
        None => return KizuConfig::default(),
    };
    load_config_from(&path)
}

/// Load configuration from a specific path. Useful for testing.
pub fn load_config_from(path: &Path) -> KizuConfig {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return KizuConfig::default(),
    };
    match toml::from_str(&content) {
        Ok(config) => config,
        Err(e) => {
            eprintln!(
                "kizu: warning: failed to parse config {}: {e}",
                path.display()
            );
            KizuConfig::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_correct_key_values() {
        let config = KizuConfig::default();
        assert_eq!(config.keys.ask, 'a');
        assert_eq!(config.keys.reject, 'r');
        assert_eq!(config.keys.comment, 'c');
        assert_eq!(config.keys.revert, 'x');
        assert_eq!(config.keys.editor, 'e');
        assert_eq!(config.keys.seen, ' ');
        assert_eq!(config.keys.follow, 'f');
        assert_eq!(config.keys.search, '/');
        assert_eq!(config.keys.picker, 's');
    }

    #[test]
    fn default_config_has_correct_colors() {
        let config = KizuConfig::default();
        assert_eq!(config.colors.bg_added, [10, 50, 10]);
        assert_eq!(config.colors.bg_deleted, [60, 10, 10]);
        assert_eq!(config.colors.bg_added_color(), Color::Rgb(10, 50, 10));
        assert_eq!(config.colors.bg_deleted_color(), Color::Rgb(60, 10, 10));
    }

    #[test]
    fn default_config_has_correct_timing() {
        let config = KizuConfig::default();
        assert_eq!(config.timing.debounce_worktree_ms, 300);
        assert_eq!(config.timing.debounce_git_dir_ms, 100);
    }

    #[test]
    fn toml_partial_override_only_changes_specified_fields() {
        let toml_str = r#"
[keys]
ask = "A"

[colors]
bg_added = [0, 80, 0]
"#;
        let config: KizuConfig = toml::from_str(toml_str).unwrap();
        // Overridden
        assert_eq!(config.keys.ask, 'A');
        assert_eq!(config.colors.bg_added, [0, 80, 0]);
        // Defaults preserved
        assert_eq!(config.keys.reject, 'r');
        assert_eq!(config.keys.comment, 'c');
        assert_eq!(config.colors.bg_deleted, [60, 10, 10]);
        assert_eq!(config.timing.debounce_worktree_ms, 300);
    }

    #[test]
    fn toml_empty_string_parses_to_defaults() {
        let config: KizuConfig = toml::from_str("").unwrap();
        assert_eq!(config.keys.ask, 'a');
        assert_eq!(config.colors.bg_added, [10, 50, 10]);
    }

    #[test]
    fn load_config_from_nonexistent_file_returns_defaults() {
        let config = load_config_from(Path::new("/nonexistent/kizu/config.toml"));
        assert_eq!(config.keys.ask, 'a');
    }

    #[test]
    fn load_config_from_invalid_toml_returns_defaults() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "this is not valid toml {{{").unwrap();
        let config = load_config_from(tmp.path());
        assert_eq!(config.keys.ask, 'a');
    }

    #[test]
    fn color_config_produces_correct_rgb() {
        let config = ColorConfig {
            bg_added: [20, 60, 20],
            ..Default::default()
        };
        assert_eq!(config.bg_added_color(), Color::Rgb(20, 60, 20));
    }
}
