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
    if std::env::var("TERM_PROGRAM").ok().as_deref() == Some("ghostty") {
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
            let mut cmd = Command::new("tmux");
            cmd.args(["split-window", "-h", &bin]);
            run_split_command(cmd, "tmux split-window")?;
        }
        TerminalKind::Zellij => {
            let mut cmd = Command::new("zellij");
            cmd.args(["run", "--floating", "--", &*bin]);
            run_split_command(cmd, "zellij run")?;
        }
        TerminalKind::Kitty => {
            let mut cmd = Command::new("kitty");
            cmd.args(["@", "launch", "--type=window", &*bin]);
            run_split_command(cmd, "kitty @ launch")?;
        }
        TerminalKind::Ghostty => {
            #[cfg(target_os = "macos")]
            {
                let script = build_ghostty_split_script(&bin);
                let mut cmd = Command::new("osascript");
                cmd.args(["-e", &script]);
                run_split_command(cmd, "Ghostty AppleScript split")?;
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

/// Run a terminal-split command, check its exit status, and surface
/// a non-zero exit as an `Err` that carries the backend name and the
/// child's stderr. The pre-existing `.status()?` calls silently
/// accepted non-zero exits from tmux / zellij / kitty / osascript,
/// which meant a failed split still returned `Ok(())` and the parent
/// process exited 0 — leaving wrappers blind to the failure.
///
/// Uses `output()` rather than `status()` so stderr is captured; the
/// split command's stdout is already consumed by the terminal itself.
fn run_split_command(mut cmd: Command, context: &str) -> Result<()> {
    let output = cmd
        .output()
        .with_context(|| format!("spawning {context}"))?;
    if output.status.success() {
        return Ok(());
    }
    let code = output
        .status
        .code()
        .map(|c| c.to_string())
        .unwrap_or_else(|| "signal".to_string());
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    if stderr.is_empty() {
        Err(anyhow!("{context} exited with status {code}"))
    } else {
        Err(anyhow!("{context} exited with status {code}: {stderr}"))
    }
}

/// Build the AppleScript command body for Ghostty's horizontal
/// split. Ghostty treats the `command` argument as a shell command
/// string, so the kizu binary path must be **shell-quoted** (POSIX
/// single-quote) first, **then** AppleScript-escaped for embedding
/// in the outer double-quoted literal. Applying only AppleScript
/// escaping leaves a path like `/Users/John Doe/kizu` exposed to
/// shell word-splitting, which either runs the wrong binary or
/// fails outright; worse, shell metacharacters in the path become
/// a command-injection boundary.
#[cfg(target_os = "macos")]
fn build_ghostty_split_script(bin: &str) -> String {
    let bin_shell = crate::init::shell_single_quote(bin);
    let bin_escaped = escape_applescript_string(&bin_shell);
    format!(
        r#"tell application "Ghostty" to tell front window to split horizontally with command "{bin_escaped}""#,
    )
}

/// Escape a string for embedding inside an AppleScript double-quoted
/// literal. Only `\` and `"` need escaping — every other character
/// (including newlines, which break `osascript -e`, but those don't
/// appear in a valid filesystem path we'd pass to `split horizontally
/// with command "..."`) passes through unchanged.
///
/// Without this, a `kizu` binary installed at a path containing a `"`
/// or `\` would break out of the command string and let AppleScript
/// execute arbitrary appended text. In practice `current_exe()` on
/// macOS produces well-formed absolute paths, but defending the
/// boundary is free here.
#[cfg(target_os = "macos")]
fn escape_applescript_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(ch),
        }
    }
    out
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

    #[test]
    fn run_split_command_surfaces_nonzero_exit_as_err() {
        // The terminal split backends used to call `.status()?`
        // without asserting `ExitStatus::success()`, so a tmux /
        // zellij / kitty failure (missing binary, invalid args,
        // out-of-session invocation) returned `Ok(())` and `main`
        // happily exited 0. A non-zero child must now surface as
        // `Err` so the caller and any CI wrapper can react.
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "printf 'split failed\\n' >&2; exit 42"]);
        let err = run_split_command(cmd, "sh failing split").expect_err("must fail");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("sh failing split"),
            "error must carry the context tag, got {msg}"
        );
        assert!(
            msg.contains("split failed") || msg.contains("42"),
            "error must surface stderr or exit code, got {msg}"
        );
    }

    #[test]
    fn run_split_command_accepts_successful_exit() {
        let cmd = Command::new("/bin/sh");
        let mut cmd = cmd;
        cmd.args(["-c", "exit 0"]);
        run_split_command(cmd, "sh ok").expect("success must be Ok");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn ghostty_script_shell_quotes_path_with_spaces() {
        // Ghostty treats the `command` argument of `split horizontally
        // with command "..."` as a shell command string. If the kizu
        // binary lives at `/Users/John Doe/kizu`, embedding the raw
        // path breaks Ghostty's shell parse and either runs the wrong
        // command or fails. The script builder must shell-quote
        // first, then AppleScript-escape the quoted form.
        let script = build_ghostty_split_script("/Users/John Doe/kizu");
        assert!(
            script.contains(r#""'/Users/John Doe/kizu'""#),
            "Ghostty script must embed the bin path inside a shell-safe single-quoted token, got {script}"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn ghostty_script_preserves_single_quote_via_shell_escape() {
        // A path containing `'` needs the shell `'\''` escape. The
        // resulting backslash in turn needs AppleScript doubling
        // (`\\`), so the final literal shows `'\\''` inside the
        // double-quoted AppleScript argument.
        let script = build_ghostty_split_script("/home/ev'an/kizu");
        assert!(
            script.contains(r"'/home/ev'\\''an/kizu'"),
            "single quote in path must survive shell + AppleScript escape, got {script}"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn applescript_escape_handles_quote_and_backslash() {
        assert_eq!(escape_applescript_string("/usr/bin/kizu"), "/usr/bin/kizu");
        assert_eq!(escape_applescript_string(r#"a"b"#), r#"a\"b"#);
        assert_eq!(escape_applescript_string(r"a\b"), r"a\\b");
        assert_eq!(escape_applescript_string(r#"a\"b"#), r#"a\\\"b"#);
    }
}
