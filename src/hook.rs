use anyhow::{Context, Result};
use serde::Deserialize;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Supported AI coding agent kinds. Determines how stdin JSON is
/// parsed and how stdout feedback is formatted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentKind {
    ClaudeCode,
    Cursor,
    Codex,
    QwenCode,
    Cline,
}

impl AgentKind {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "claude-code" | "claude" => Some(Self::ClaudeCode),
            "cursor" => Some(Self::Cursor),
            "codex" => Some(Self::Codex),
            "qwen" | "qwen-code" => Some(Self::QwenCode),
            "cline" => Some(Self::Cline),
            _ => None,
        }
    }
}

/// Agent-agnostic representation of a hook invocation's input.
/// Produced by [`parse_hook_input`] which normalizes across agent
/// JSON dialects.
#[derive(Debug, Clone)]
pub struct NormalizedHookInput {
    #[allow(dead_code)]
    pub session_id: Option<String>,
    #[allow(dead_code)]
    pub hook_event_name: String,
    #[allow(dead_code)]
    pub tool_name: Option<String>,
    pub file_paths: Vec<PathBuf>,
    pub cwd: Option<PathBuf>,
    pub stop_hook_active: bool,
}

/// Raw stdin JSON shape shared by Claude Code / Cursor / Codex /
/// Qwen Code / Cline (minor field differences are absorbed by
/// `Option`). Cline's file-based hooks receive the same structure
/// on stdin.
#[derive(Debug, Deserialize)]
struct RawHookInput {
    session_id: Option<String>,
    hook_event_name: Option<String>,
    tool_name: Option<String>,
    tool_input: Option<serde_json::Value>,
    cwd: Option<String>,
    stop_hook_active: Option<bool>,
}

/// Parse the hook's stdin JSON into a [`NormalizedHookInput`].
/// The format is nearly identical across Claude Code / Cursor /
/// Codex / Qwen / Cline, so a single deserializer suffices.
pub fn parse_hook_input(_agent: AgentKind, reader: impl Read) -> Result<NormalizedHookInput> {
    let raw: RawHookInput = serde_json::from_reader(reader).context("parsing hook stdin JSON")?;

    let mut file_paths = Vec::new();
    if let Some(tool_input) = &raw.tool_input
        && let Some(fp) = tool_input.get("file_path").and_then(|v| v.as_str())
    {
        file_paths.push(PathBuf::from(fp));
    }

    Ok(NormalizedHookInput {
        session_id: raw.session_id,
        hook_event_name: raw.hook_event_name.unwrap_or_default(),
        tool_name: raw.tool_name,
        file_paths,
        cwd: raw.cwd.map(PathBuf::from),
        stop_hook_active: raw.stop_hook_active.unwrap_or(false),
    })
}

/// One `@kizu[<kind>]: <message>` hit found inside a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScarHit {
    pub path: PathBuf,
    pub line_number: usize,
    pub kind: String,
    pub message: String,
}

/// Grep every file in `paths` for `@kizu[...]` scars. Returns all
/// hits across all files, in order. Files that cannot be read (e.g.
/// deleted between the scan and the grep) are silently skipped.
// @kizu[ask]: explain this change
///
/// Only matches scars that appear as the primary content of a
/// comment line (preceded by `//`, `#`, `--`, `/*`, or `<!--`
/// after optional whitespace). This avoids false positives from
/// string literals in test code that happen to contain `@kizu[`.
pub fn scan_scars(paths: &[PathBuf]) -> Vec<ScarHit> {
    let re = regex::Regex::new(r"^\s*(?://|#|--|/\*|<!--)\s*@kizu\[(\w+)\]:\s*(.*)")
        .expect("scar regex");
    let mut hits = Vec::new();
    for path in paths {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        for (i, line) in content.lines().enumerate() {
            if let Some(caps) = re.captures(line) {
                hits.push(ScarHit {
                    path: path.clone(),
                    line_number: i + 1,
                    kind: caps[1].to_string(),
                    message: caps[2].trim().to_string(),
                });
            }
        }
    }
    hits
}

/// Format scar hits as a JSON `additionalContext` string suitable
/// for Claude Code / Cursor / Codex / Qwen / Cline stdout.
/// Returns `None` when there are no hits (caller should exit 0
/// silently).
pub fn format_additional_context(hits: &[ScarHit]) -> Option<String> {
    if hits.is_empty() {
        return None;
    }
    let mut lines = Vec::new();
    for hit in hits {
        lines.push(format!(
            "{}:{} @kizu[{}]: {}",
            hit.path.display(),
            hit.line_number,
            hit.kind,
            hit.message,
        ));
    }
    let context = lines.join("\n");
    let envelope = serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "additionalContext": context,
        }
    });
    Some(serde_json::to_string(&envelope).expect("json serialize"))
}

/// Format scar hits as stderr text for the Stop hook. Each hit
/// is one line; the caller writes this to stderr and exits 2.
pub fn format_stop_stderr(hits: &[ScarHit]) -> String {
    let mut out = String::from("Unresolved kizu scars:\n");
    for hit in hits {
        out.push_str(&format!(
            "  {}:{} @kizu[{}]: {}\n",
            hit.path.display(),
            hit.line_number,
            hit.kind,
            hit.message,
        ));
    }
    out
}

/// List all files that might contain scars: tracked modified +
/// untracked. Mirrors the file set that the kizu TUI's diff view
/// shows, ensuring the Stop hook scans the same scope.
pub fn enumerate_changed_files(root: &Path) -> Result<Vec<PathBuf>> {
    use std::process::Command;

    // tracked modified/added
    let diff_output = Command::new("git")
        .args(["diff", "--name-only", "HEAD", "--"])
        .current_dir(root)
        .output()
        .context("git diff --name-only")?;
    let mut paths: Vec<PathBuf> = Vec::new();
    if diff_output.status.success() {
        for line in String::from_utf8_lossy(&diff_output.stdout).lines() {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                paths.push(root.join(trimmed));
            }
        }
    }

    // untracked
    let status_output = Command::new("git")
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .current_dir(root)
        .output()
        .context("git status --porcelain")?;
    if status_output.status.success() {
        for record in status_output.stdout.split(|&b| b == 0) {
            if record.len() >= 3 && &record[..3] == b"?? " {
                let path_bytes = &record[3..];
                let rel = String::from_utf8_lossy(path_bytes);
                paths.push(root.join(rel.as_ref()));
            }
        }
    }

    paths.sort();
    paths.dedup();
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn parse_hook_input_extracts_claude_code_post_tool_use() {
        let json = r#"{
            "session_id": "abc123",
            "hook_event_name": "PostToolUse",
            "tool_name": "Write",
            "tool_input": { "file_path": "/tmp/foo.rs", "content": "fn main() {}" },
            "tool_response": "ok",
            "cwd": "/home/user/project",
            "stop_hook_active": false
        }"#;
        let input = parse_hook_input(AgentKind::ClaudeCode, json.as_bytes()).unwrap();
        assert_eq!(input.session_id.as_deref(), Some("abc123"));
        assert_eq!(input.hook_event_name, "PostToolUse");
        assert_eq!(input.tool_name.as_deref(), Some("Write"));
        assert_eq!(input.file_paths, vec![PathBuf::from("/tmp/foo.rs")]);
        assert_eq!(input.cwd.as_deref(), Some(Path::new("/home/user/project")));
        assert!(!input.stop_hook_active);
    }

    #[test]
    fn parse_hook_input_extracts_stop_event() {
        let json = r#"{
            "hook_event_name": "Stop",
            "stop_hook_active": true,
            "cwd": "/tmp"
        }"#;
        let input = parse_hook_input(AgentKind::ClaudeCode, json.as_bytes()).unwrap();
        assert_eq!(input.hook_event_name, "Stop");
        assert!(input.stop_hook_active);
        assert!(input.file_paths.is_empty());
    }

    #[test]
    fn scan_scars_finds_ask_reject_and_free() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("a.rs");
        fs::write(
            &file,
            "fn main() {}\n// @kizu[ask]: explain this change\n// @kizu[reject]: revert this change\n// @kizu[free]: custom note\n",
        ).unwrap();

        let hits = scan_scars(std::slice::from_ref(&file));

        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].kind, "ask");
        assert_eq!(hits[0].message, "explain this change");
        assert_eq!(hits[0].line_number, 2);
        assert_eq!(hits[1].kind, "reject");
        assert_eq!(hits[2].kind, "free");
        assert_eq!(hits[2].message, "custom note");
    }

    #[test]
    fn scan_scars_handles_python_comment_syntax() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("x.py");
        fs::write(&file, "def f():\n# @kizu[ask]: why?\n    pass\n").unwrap();

        let hits = scan_scars(&[file]);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, "ask");
        assert_eq!(hits[0].message, "why?");
    }

    #[test]
    fn scan_scars_skips_missing_files_without_panic() {
        let hits = scan_scars(&[PathBuf::from("/nonexistent/ghost.rs")]);
        assert!(hits.is_empty());
    }

    #[test]
    fn scan_scars_returns_empty_when_no_scars_present() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("clean.rs");
        fs::write(&file, "fn main() {}\n").unwrap();

        let hits = scan_scars(&[file]);
        assert!(hits.is_empty());
    }

    #[test]
    fn format_additional_context_produces_valid_json_envelope() {
        let hits = vec![ScarHit {
            path: PathBuf::from("src/foo.rs"),
            line_number: 10,
            kind: "ask".into(),
            message: "explain this".into(),
        }];
        let json_str = format_additional_context(&hits).expect("non-empty");
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        let ctx = parsed["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(ctx.contains("src/foo.rs:10"));
        assert!(ctx.contains("@kizu[ask]"));
    }

    #[test]
    fn format_additional_context_returns_none_when_no_hits() {
        assert!(format_additional_context(&[]).is_none());
    }

    #[test]
    fn format_stop_stderr_lists_all_hits() {
        let hits = vec![
            ScarHit {
                path: PathBuf::from("a.rs"),
                line_number: 1,
                kind: "ask".into(),
                message: "why".into(),
            },
            ScarHit {
                path: PathBuf::from("b.py"),
                line_number: 5,
                kind: "reject".into(),
                message: "revert".into(),
            },
        ];
        let stderr = format_stop_stderr(&hits);
        assert!(stderr.contains("a.rs:1"));
        assert!(stderr.contains("b.py:5"));
        assert!(stderr.contains("Unresolved kizu scars"));
    }

    #[test]
    fn agent_kind_from_str_matches_common_names() {
        assert_eq!(
            AgentKind::from_str("claude-code"),
            Some(AgentKind::ClaudeCode)
        );
        assert_eq!(AgentKind::from_str("claude"), Some(AgentKind::ClaudeCode));
        assert_eq!(AgentKind::from_str("cursor"), Some(AgentKind::Cursor));
        assert_eq!(AgentKind::from_str("codex"), Some(AgentKind::Codex));
        assert_eq!(AgentKind::from_str("qwen"), Some(AgentKind::QwenCode));
        assert_eq!(AgentKind::from_str("cline"), Some(AgentKind::Cline));
        assert_eq!(AgentKind::from_str("unknown"), None);
    }
}
