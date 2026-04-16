use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
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

// === Action resolution ===

/// All user-triggerable actions in kizu. `handle_key` dispatches
/// on this enum rather than raw keycodes, making the keybind layer
/// swappable via config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Used by resolve_action; consumed in future refactor
pub enum Action {
    Ask,
    Reject,
    Comment,
    Revert,
    Editor,
    Seen,
    Follow,
    Search,
    SearchNext,
    SearchPrev,
    Picker,
    FileView,
    ResetBaseline,
    CursorPlacement,
    WrapToggle,
    StreamMode,
    ScrollDown,
    ScrollUp,
    LineDown,
    LineUp,
    HunkNext,
    HunkPrev,
    Top,
    Bottom,
    HalfPageDown,
    HalfPageUp,
    Quit,
}

/// Resolve a [`KeyEvent`] into an [`Action`] using the current
/// keybinding configuration. Returns `None` for unmapped keys.
///
/// Non-remappable keys (Enter, Tab, arrows, Ctrl-combos, Esc) are
/// handled first, then the config's char mappings are checked.
#[allow(dead_code)] // Will be used when handle_normal_key fully migrates to Action dispatch
pub fn resolve_action(key: &KeyEvent, config: &KizuConfig) -> Option<Action> {
    // Ctrl-c / q → Quit
    if matches!(key.code, KeyCode::Char('c')) && key.modifiers.contains(KeyModifiers::CONTROL) {
        return Some(Action::Quit);
    }
    if matches!(key.code, KeyCode::Char('q')) && key.modifiers == KeyModifiers::NONE {
        return Some(Action::Quit);
    }

    // Non-remappable keys
    match key.code {
        KeyCode::Enter => return Some(Action::FileView),
        KeyCode::Tab => return Some(Action::StreamMode),
        KeyCode::Down => return Some(Action::ScrollDown),
        KeyCode::Up => return Some(Action::ScrollUp),
        _ => {}
    }

    // Ctrl-d / Ctrl-u → half-page
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('d') => return Some(Action::HalfPageDown),
            KeyCode::Char('u') => return Some(Action::HalfPageUp),
            _ => return None,
        }
    }

    // Char-based remappable keys
    if let KeyCode::Char(ch) = key.code {
        let k = &config.keys;
        if ch == k.ask {
            return Some(Action::Ask);
        }
        if ch == k.reject {
            return Some(Action::Reject);
        }
        if ch == k.comment {
            return Some(Action::Comment);
        }
        if ch == k.revert {
            return Some(Action::Revert);
        }
        if ch == k.editor {
            return Some(Action::Editor);
        }
        if ch == k.seen {
            return Some(Action::Seen);
        }
        if ch == k.follow {
            return Some(Action::Follow);
        }
        if ch == k.search {
            return Some(Action::Search);
        }
        if ch == k.search_next {
            return Some(Action::SearchNext);
        }
        if ch == k.search_prev {
            return Some(Action::SearchPrev);
        }
        if ch == k.picker {
            return Some(Action::Picker);
        }
        if ch == k.reset_baseline {
            return Some(Action::ResetBaseline);
        }
        if ch == k.cursor_placement {
            return Some(Action::CursorPlacement);
        }
        if ch == k.wrap_toggle {
            return Some(Action::WrapToggle);
        }

        // Hardcoded navigation (j/k/J/K/h/l/g/G are not remappable)
        match ch {
            'j' => return Some(Action::ScrollDown),
            'k' => return Some(Action::ScrollUp),
            'J' => return Some(Action::LineDown),
            'K' => return Some(Action::LineUp),
            'l' => return Some(Action::HunkNext),
            'h' => return Some(Action::HunkPrev),
            'g' => return Some(Action::Top),
            'G' => return Some(Action::Bottom),
            _ => {}
        }
    }

    None
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
    fn resolve_action_maps_default_ask_key() {
        let config = KizuConfig::default();
        let key = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        assert_eq!(resolve_action(&key, &config), Some(Action::Ask));
    }

    #[test]
    fn resolve_action_maps_remapped_ask_key() {
        let mut config = KizuConfig::default();
        config.keys.ask = 'A';
        // Old key should not match
        let old_key = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        assert_ne!(resolve_action(&old_key, &config), Some(Action::Ask));
        // New key should match
        let new_key = KeyEvent::new(KeyCode::Char('A'), KeyModifiers::NONE);
        assert_eq!(resolve_action(&new_key, &config), Some(Action::Ask));
    }

    #[test]
    fn resolve_action_ctrl_c_is_quit() {
        let config = KizuConfig::default();
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(resolve_action(&key, &config), Some(Action::Quit));
    }

    #[test]
    fn resolve_action_enter_is_file_view() {
        let config = KizuConfig::default();
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(resolve_action(&key, &config), Some(Action::FileView));
    }

    #[test]
    fn resolve_action_tab_is_stream_mode() {
        let config = KizuConfig::default();
        let key = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(resolve_action(&key, &config), Some(Action::StreamMode));
    }

    #[test]
    fn resolve_action_navigation_keys_are_hardcoded() {
        let config = KizuConfig::default();
        let j = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
        let k = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE);
        let big_j = KeyEvent::new(KeyCode::Char('J'), KeyModifiers::SHIFT);
        let big_k = KeyEvent::new(KeyCode::Char('K'), KeyModifiers::SHIFT);
        assert_eq!(resolve_action(&j, &config), Some(Action::ScrollDown));
        assert_eq!(resolve_action(&k, &config), Some(Action::ScrollUp));
        assert_eq!(resolve_action(&big_j, &config), Some(Action::LineDown));
        assert_eq!(resolve_action(&big_k, &config), Some(Action::LineUp));
    }

    #[test]
    fn resolve_action_unmapped_key_returns_none() {
        let config = KizuConfig::default();
        let key = KeyEvent::new(KeyCode::Char('Z'), KeyModifiers::NONE);
        assert_eq!(resolve_action(&key, &config), None);
    }

    #[test]
    fn resolve_action_half_page_keys() {
        let config = KizuConfig::default();
        let ctrl_d = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);
        let ctrl_u = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL);
        assert_eq!(resolve_action(&ctrl_d, &config), Some(Action::HalfPageDown));
        assert_eq!(resolve_action(&ctrl_u, &config), Some(Action::HalfPageUp));
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
