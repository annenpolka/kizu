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
    pub line_numbers: LineNumbersConfig,
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
    /// Undo the most recent scar insertion (Ask / Reject / Free). The
    /// key pops the top of the session's scar undo stack and reverses
    /// just that one write, matching text-editor undo ergonomics.
    pub undo: char,
    /// Toggle the line-number gutter (v0.5). Works in diff view and
    /// file view; Stream mode always suppresses line numbers regardless.
    pub line_numbers_toggle: char,
}

/// Line-number gutter configuration (v0.5).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct LineNumbersConfig {
    /// Initial state of the line-number gutter. `false` (default)
    /// preserves v0.4 layout; set `true` in config.toml to opt in.
    pub enabled: bool,
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
            undo: 'u',
            line_numbers_toggle: '#',
        }
    }
}

impl KeyConfig {
    /// Every `(action_name, char)` pair in this map, in a stable
    /// order. Used by [`Self::conflicts`] for duplicate detection.
    fn bindings(&self) -> [(&'static str, char); 16] {
        [
            ("ask", self.ask),
            ("reject", self.reject),
            ("comment", self.comment),
            ("revert", self.revert),
            ("editor", self.editor),
            ("seen", self.seen),
            ("follow", self.follow),
            ("search", self.search),
            ("search_next", self.search_next),
            ("search_prev", self.search_prev),
            ("picker", self.picker),
            ("reset_baseline", self.reset_baseline),
            ("cursor_placement", self.cursor_placement),
            ("wrap_toggle", self.wrap_toggle),
            ("undo", self.undo),
            ("line_numbers_toggle", self.line_numbers_toggle),
        ]
    }

    /// Group binding conflicts by the char that collides. Returns
    /// one `(char, Vec<action_name>)` entry per char that two or
    /// more actions share. A partial config that doesn't override
    /// anything stays conflict-free because the defaults are disjoint.
    pub fn conflicts(&self) -> Vec<(char, Vec<&'static str>)> {
        use std::collections::BTreeMap;
        let mut by_char: BTreeMap<char, Vec<&'static str>> = BTreeMap::new();
        for (name, ch) in self.bindings() {
            by_char.entry(ch).or_default().push(name);
        }
        by_char
            .into_iter()
            .filter(|(_, names)| names.len() > 1)
            .collect()
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
    let config: KizuConfig = match toml::from_str(&content) {
        Ok(config) => config,
        Err(e) => {
            eprintln!(
                "kizu: warning: failed to parse config {}: {e}",
                path.display()
            );
            return KizuConfig::default();
        }
    };
    for (ch, actions) in config.keys.conflicts() {
        let display = if ch == ' ' {
            "<space>".to_string()
        } else {
            ch.to_string()
        };
        eprintln!(
            "kizu: warning: config key {display:?} is bound to multiple actions: {}",
            actions.join(", ")
        );
    }
    config
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_config_conflicts_returns_empty_for_defaults() {
        let config = KizuConfig::default();
        assert!(
            config.keys.conflicts().is_empty(),
            "default key map must have no duplicates"
        );
    }

    #[test]
    fn key_config_conflicts_reports_duplicate_assignments() {
        // User accidentally rebinds `reject` to the default `ask` char.
        let mut config = KizuConfig::default();
        config.keys.reject = 'a'; // Collides with ask = 'a'.
        let conflicts = config.keys.conflicts();
        assert_eq!(
            conflicts.len(),
            1,
            "one group of conflicting actions expected, got: {conflicts:?}",
        );
        let (ch, names) = &conflicts[0];
        assert_eq!(*ch, 'a');
        assert!(names.contains(&"ask"));
        assert!(names.contains(&"reject"));
    }

    #[test]
    fn key_config_conflicts_ignores_space_search_next_prev_defaults() {
        // The default `seen = ' '` doesn't conflict with any other key
        // because no other default action uses space; make sure the
        // detector doesn't false-positive on the default map.
        let config = KizuConfig::default();
        assert!(config.keys.conflicts().is_empty());
    }

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

    // ---- line numbers (v0.5) -----------------------------------------

    #[test]
    fn default_config_has_line_numbers_toggle_and_disabled() {
        let config = KizuConfig::default();
        // Default toggle key is '#' (see plan Decision Log: candidates
        // 'l' / 'n' / 'L' all had conflicts or ergonomic issues).
        assert_eq!(config.keys.line_numbers_toggle, '#');
        // Default state is OFF to keep v0.4 layout unchanged for users
        // who don't opt in.
        assert!(!config.line_numbers.enabled);
    }

    #[test]
    fn toml_can_override_line_numbers_enabled() {
        let config: KizuConfig = toml::from_str("[line_numbers]\nenabled = true\n").unwrap();
        assert!(config.line_numbers.enabled);
        // Unrelated defaults must still be preserved.
        assert_eq!(config.keys.ask, 'a');
    }
}
