use anyhow::{Context, Result, anyhow};
use std::path::Path;
use std::process::Command;

/// Supported terminal multiplexers / emulators for `--attach`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalKind {
    Tmux,
    Zellij,
    Kitty,
    Ghostty,
}

impl TerminalKind {
    /// Try to parse a terminal name string into a [`TerminalKind`].
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "tmux" => Some(Self::Tmux),
            "zellij" => Some(Self::Zellij),
            "kitty" => Some(Self::Kitty),
            "ghostty" => Some(Self::Ghostty),
            _ => None,
        }
    }
}

/// Detect the current terminal multiplexer / emulator by checking
/// environment variables. Returns the first match in priority order:
/// tmux → zellij → kitty → ghostty (SPEC.md).
pub fn detect_terminal() -> Option<TerminalKind> {
    if std::env::var("TMUX").is_ok() {
        return Some(TerminalKind::Tmux);
    }
    if std::env::var("ZELLIJ").is_ok() {
        return Some(TerminalKind::Zellij);
    }
    if std::env::var("KITTY_LISTEN_ON").is_ok() {
        return Some(TerminalKind::Kitty);
    }
    if std::env::var("TERM_PROGRAM")
        .ok()
        .as_deref()
        == Some("ghostty")
    {
        return Some(TerminalKind::Ghostty);
    }
    None
}

/// Split the current terminal and launch kizu in the new pane.
/// The `kizu_bin` path should be the absolute path to the kizu binary
/// (typically resolved via `std::env::current_exe()`).
pub fn split_and_launch(terminal: TerminalKind, kizu_bin: &Path) -> Result<()> {
    let bin = kizu_bin.to_string_lossy();
    match terminal {
        TerminalKind::Tmux => {
            Command::new("tmux")
                .args(["split-window", "-h", &bin])
                .status()
                .context("tmux split-window")?;
        }
        TerminalKind::Zellij => {
            Command::new("zellij")
                .args(["run", "--floating", "--", &*bin])
                .status()
                .context("zellij run")?;
        }
        TerminalKind::Kitty => {
            Command::new("kitty")
                .args(["@", "launch", "--type=window", &*bin])
                .status()
                .context("kitty @ launch")?;
        }
        TerminalKind::Ghostty => {
            #[cfg(target_os = "macos")]
            {
                let script = format!(
                    r#"tell application "Ghostty" to tell front window to split horizontally with command "{bin}""#,
                );
                Command::new("osascript")
                    .args(["-e", &script])
                    .status()
                    .context("Ghostty AppleScript split")?;
            }
            #[cfg(not(target_os = "macos"))]
            {
                return Err(anyhow!(
                    "Ghostty --attach is only supported on macOS (requires AppleScript)"
                ));
            }
        }
    }
    Ok(())
}

/// Resolve which terminal to use: config override → auto-detect.
/// Returns an error if no terminal can be determined.
pub fn resolve_terminal(config_terminal: &str) -> Result<TerminalKind> {
    if !config_terminal.is_empty() {
        return TerminalKind::from_str(config_terminal).ok_or_else(|| {
            anyhow!(
                "unknown terminal '{}' in config; expected: tmux, zellij, kitty, ghostty",
                config_terminal
            )
        });
    }
    detect_terminal().ok_or_else(|| {
        anyhow!(
            "could not detect terminal multiplexer. \
             Set [attach].terminal in ~/.config/kizu/config.toml \
             or run inside tmux/zellij/kitty/Ghostty"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_kind_from_str_matches_known_names() {
        assert_eq!(TerminalKind::from_str("tmux"), Some(TerminalKind::Tmux));
        assert_eq!(TerminalKind::from_str("TMUX"), Some(TerminalKind::Tmux));
        assert_eq!(TerminalKind::from_str("zellij"), Some(TerminalKind::Zellij));
        assert_eq!(TerminalKind::from_str("kitty"), Some(TerminalKind::Kitty));
        assert_eq!(
            TerminalKind::from_str("ghostty"),
            Some(TerminalKind::Ghostty)
        );
        assert_eq!(TerminalKind::from_str("unknown"), None);
    }

    #[test]
    fn resolve_terminal_uses_config_override() {
        let term = resolve_terminal("tmux").unwrap();
        assert_eq!(term, TerminalKind::Tmux);
    }

    #[test]
    fn resolve_terminal_rejects_invalid_config() {
        let err = resolve_terminal("invalid").unwrap_err();
        assert!(err.to_string().contains("unknown terminal"));
    }
}
