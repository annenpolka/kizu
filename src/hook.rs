use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
    pub session_id: Option<String>,
    pub hook_event_name: String,
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
    if let Some(tool_input) = &raw.tool_input {
        // Agents use different field names for the edited file path:
        // - Claude Code / Qwen: tool_input.file_path
        // - Cline: tool_input.path
        // - Cursor: tool_input.filePath
        // Try all known variants so every agent's payload is accepted.
        let fp = tool_input
            .get("file_path")
            .or_else(|| tool_input.get("path"))
            .or_else(|| tool_input.get("filePath"))
            .and_then(|v| v.as_str());
        if let Some(fp) = fp {
            file_paths.push(PathBuf::from(fp));
        }

        // Cursor afterFileEdit sends an `edits` array with per-file
        // entries. Extract paths from each element so multi-file edits
        // don't silently drop scar notifications.
        if let Some(edits) = tool_input.get("edits").and_then(|v| v.as_array()) {
            for edit in edits {
                let ep = edit
                    .get("file_path")
                    .or_else(|| edit.get("path"))
                    .or_else(|| edit.get("filePath"))
                    .and_then(|v| v.as_str());
                if let Some(ep) = ep {
                    file_paths.push(PathBuf::from(ep));
                }
            }
        }

        file_paths.sort();
        file_paths.dedup();
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
        let fence_aware = is_fenced_code_aware(path);
        hits.extend(scan_content_for_scars(&re, &content, path, fence_aware));
    }
    hits
}

/// Format scar hits as a JSON string suitable for the given agent's
/// stdout protocol. Returns `None` when there are no hits (caller
/// should exit 0 silently).
///
/// Output envelopes per agent (see `docs/deep-research-ai-agent-hooks.md`):
/// - Claude Code / Qwen / Cline: `{"hookSpecificOutput":{"hookEventName":"PostToolUse","additionalContext":"..."}}`
/// - Cursor: `{"additional_context":"..."}`
/// - Codex: `{"additionalContext":"..."}`
pub fn format_additional_context(agent: AgentKind, hits: &[ScarHit]) -> Option<String> {
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
    let envelope = match agent {
        AgentKind::Cursor => serde_json::json!({
            "additional_context": context,
        }),
        AgentKind::Codex => serde_json::json!({
            "additionalContext": context,
        }),
        AgentKind::ClaudeCode | AgentKind::QwenCode | AgentKind::Cline => serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PostToolUse",
                "additionalContext": context,
            }
        }),
    };
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

/// Returns `true` for file extensions where fenced code blocks can
/// contain scar-like examples that should not be treated as live scars.
fn is_fenced_code_aware(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| matches!(ext, "md" | "txt" | "rst" | "adoc"))
}

/// Scan `content` for scar hits, skipping matches inside Markdown
/// fenced code blocks (`` ``` `` / `~~~`) when `fence_aware` is true.
fn scan_content_for_scars(
    re: &regex::Regex,
    content: &str,
    path: &Path,
    fence_aware: bool,
) -> Vec<ScarHit> {
    let mut hits = Vec::new();
    let mut in_fence = false;
    for (i, line) in content.lines().enumerate() {
        if fence_aware {
            let trimmed = line.trim_start();
            if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
                in_fence = !in_fence;
                continue;
            }
            if in_fence {
                continue;
            }
        }
        if let Some(caps) = re.captures(line) {
            hits.push(ScarHit {
                path: path.to_path_buf(),
                line_number: i + 1,
                kind: caps[1].to_string(),
                message: caps[2].trim().to_string(),
            });
        }
    }
    hits
}

/// Grep staged (index) contents of `paths` for `@kizu[...]` scars.
/// Unlike [`scan_scars`] which reads the worktree, this reads the
/// staged blob via `git show :<path>` so that worktree edits after
/// staging don't mask scars that are actually about to be committed.
pub fn scan_scars_from_index(root: &Path, paths: &[PathBuf]) -> Vec<ScarHit> {
    let re = regex::Regex::new(r"^\s*(?://|#|--|/\*|<!--)\s*@kizu\[(\w+)\]:\s*(.*)")
        .expect("scar regex");
    let mut hits = Vec::new();
    for path in paths {
        let rel = match path.strip_prefix(root) {
            Ok(r) => r,
            Err(_) => path.as_path(),
        };
        let output = match std::process::Command::new("git")
            .args(["show", &format!(":{}", rel.display())])
            .current_dir(root)
            .output()
        {
            Ok(o) if o.status.success() => o,
            _ => continue,
        };
        let content = String::from_utf8_lossy(&output.stdout);
        let fence_aware = is_fenced_code_aware(path);
        hits.extend(scan_content_for_scars(&re, &content, path, fence_aware));
    }
    hits
}

/// Sanitized event metadata for the stream mode event log.
/// Contains only non-sensitive metadata — code content, prompts,
/// and agent responses are stripped during sanitization.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SanitizedEvent {
    pub session_id: Option<String>,
    pub hook_event_name: String,
    pub tool_name: Option<String>,
    pub file_paths: Vec<PathBuf>,
    pub cwd: PathBuf,
    pub timestamp_ms: u64,
}

/// Convert a [`NormalizedHookInput`] into a [`SanitizedEvent`],
/// stripping all code content and adding a timestamp.
pub fn sanitize_event(input: &NormalizedHookInput) -> SanitizedEvent {
    let timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    SanitizedEvent {
        session_id: input.session_id.clone(),
        hook_event_name: input.hook_event_name.clone(),
        tool_name: input.tool_name.clone(),
        file_paths: input.file_paths.clone(),
        cwd: input.cwd.clone().unwrap_or_default(),
        timestamp_ms,
    }
}

/// Write a [`SanitizedEvent`] to the events directory as an atomic
/// JSON file. Returns the path of the written file. The events
/// directory is created with `0700` permissions if it doesn't exist.
/// Individual event files are written with `0600` permissions.
pub fn write_event(event: &SanitizedEvent) -> Result<PathBuf> {
    let dir = crate::paths::events_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot resolve kizu events directory"))?;
    crate::paths::ensure_private_dir(&dir)?;

    let tool = event.tool_name.as_deref().unwrap_or("unknown");
    let filename = format!("{}-{}.json", event.timestamp_ms, tool);
    let dest = dir.join(&filename);

    let json = serde_json::to_string(event).context("serializing event")?;

    // Atomic write: write to temp file then rename.
    let tmp_path = dir.join(format!(".{filename}.tmp"));
    std::fs::write(&tmp_path, &json)
        .with_context(|| format!("writing temp event file {}", tmp_path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o600));
    }

    std::fs::rename(&tmp_path, &dest)
        .with_context(|| format!("renaming event file to {}", dest.display()))?;

    Ok(dest)
}

/// Prune the events directory: remove entries older than `ttl` and
/// enforce a maximum entry count. Returns the number of files removed.
/// Uses the default events directory from [`crate::paths::events_dir`].
pub fn prune_event_log(ttl: Duration, max_entries: usize) -> Result<usize> {
    let dir = match crate::paths::events_dir() {
        Some(d) if d.is_dir() => d,
        _ => return Ok(0),
    };
    prune_event_log_in(&dir, ttl, max_entries)
}

/// Prune events in the given directory. Testable variant of
/// [`prune_event_log`] that accepts an explicit path.
pub fn prune_event_log_in(dir: &Path, ttl: Duration, max_entries: usize) -> Result<usize> {
    if !dir.is_dir() {
        return Ok(0);
    }

    let mut entries: Vec<(PathBuf, u64)> = Vec::new();
    for entry in std::fs::read_dir(dir).context("reading events dir")? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        // Skip temp files.
        if name_str.starts_with('.') {
            continue;
        }
        // Parse timestamp from filename: <timestamp_ms>-<tool>.json
        if let Some(ts_str) = name_str.split('-').next()
            && let Ok(ts) = ts_str.parse::<u64>()
        {
            entries.push((entry.path(), ts));
        }
    }

    // Sort oldest first.
    entries.sort_by_key(|(_, ts)| *ts);

    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let ttl_ms = ttl.as_millis() as u64;

    let mut removed = 0;

    // Pass 1: remove entries older than TTL.
    entries.retain(|(path, ts)| {
        if now_ms.saturating_sub(*ts) > ttl_ms {
            let _ = std::fs::remove_file(path);
            removed += 1;
            false
        } else {
            true
        }
    });

    // Pass 2: enforce max entries (remove oldest first).
    if entries.len() > max_entries {
        let excess = entries.len() - max_entries;
        for (path, _) in entries.iter().take(excess) {
            let _ = std::fs::remove_file(path);
            removed += 1;
        }
    }

    Ok(removed)
}

/// List files that might contain scars, scoped by the kizu session
/// baseline. Checks for a session file; if found, scans only files
/// changed since that baseline (committed + uncommitted + untracked).
/// Falls back to all tracked + untracked if no session is active.
pub fn enumerate_session_files(root: &Path) -> Result<Vec<PathBuf>> {
    use std::process::Command;

    let session = crate::session::read_session(root);
    let baseline = session
        .as_ref()
        .filter(|s| crate::session::is_session_alive(s))
        .map(|s| s.baseline_sha.as_str());

    let mut paths: Vec<PathBuf> = Vec::new();

    if let Some(base) = baseline {
        // Fail-closed: if any baseline-scoped diff fails (e.g. stale
        // session, rebase, corrupt SHA), fall back to a full tracked
        // scan instead of silently under-scanning.
        let mut any_failed = false;

        // Files changed since baseline (includes commits made during
        // the session): `git diff <baseline>..HEAD --name-only`.
        let diff_base = Command::new("git")
            .args(["diff", "--name-only", "-z", &format!("{base}..HEAD"), "--"])
            .current_dir(root)
            .output()
            .context("git diff baseline..HEAD")?;
        if diff_base.status.success() {
            for record in diff_base.stdout.split(|&b| b == 0) {
                if !record.is_empty() {
                    let rel = String::from_utf8_lossy(record);
                    paths.push(root.join(rel.as_ref()));
                }
            }
        } else {
            any_failed = true;
        }

        // Uncommitted changes (staged + unstaged).
        let diff_head = Command::new("git")
            .args(["diff", "--name-only", "-z", "HEAD", "--"])
            .current_dir(root)
            .output()
            .context("git diff HEAD")?;
        if diff_head.status.success() {
            for record in diff_head.stdout.split(|&b| b == 0) {
                if !record.is_empty() {
                    let rel = String::from_utf8_lossy(record);
                    paths.push(root.join(rel.as_ref()));
                }
            }
        } else {
            any_failed = true;
        }

        // Staged but not yet in HEAD.
        let diff_cached = Command::new("git")
            .args(["diff", "--cached", "--name-only", "-z", "--"])
            .current_dir(root)
            .output()
            .context("git diff --cached")?;
        if diff_cached.status.success() {
            for record in diff_cached.stdout.split(|&b| b == 0) {
                if !record.is_empty() {
                    let rel = String::from_utf8_lossy(record);
                    paths.push(root.join(rel.as_ref()));
                }
            }
        } else {
            any_failed = true;
        }

        // If any baseline diff failed, discard partial results and
        // fall through to full tracked scan so we don't miss scars.
        if any_failed {
            paths.clear();
            let ls_output = Command::new("git")
                .args(["ls-files", "-z"])
                .current_dir(root)
                .output()
                .context("git ls-files (fallback)")?;
            if ls_output.status.success() {
                for record in ls_output.stdout.split(|&b| b == 0) {
                    if !record.is_empty() {
                        let rel = String::from_utf8_lossy(record);
                        paths.push(root.join(rel.as_ref()));
                    }
                }
            }
        }
    } else {
        // No session → fallback to all tracked files.
        let ls_output = Command::new("git")
            .args(["ls-files", "-z"])
            .current_dir(root)
            .output()
            .context("git ls-files")?;
        if ls_output.status.success() {
            for record in ls_output.stdout.split(|&b| b == 0) {
                if !record.is_empty() {
                    let rel = String::from_utf8_lossy(record);
                    paths.push(root.join(rel.as_ref()));
                }
            }
        }
    }

    // Untracked files (always included).
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
    fn parse_hook_input_extracts_cline_path_field() {
        let json = r#"{
            "hook_event_name": "PostToolUse",
            "tool_name": "Write",
            "tool_input": { "path": "/tmp/cline.py", "content": "print(1)" },
            "cwd": "/home/user"
        }"#;
        let input = parse_hook_input(AgentKind::Cline, json.as_bytes()).unwrap();
        assert_eq!(input.file_paths, vec![PathBuf::from("/tmp/cline.py")]);
    }

    #[test]
    fn parse_hook_input_extracts_cursor_file_path_field() {
        let json = r#"{
            "hook_event_name": "PostToolUse",
            "tool_name": "Edit",
            "tool_input": { "filePath": "/tmp/cursor.ts", "content": "const x = 1" },
            "cwd": "/home/user"
        }"#;
        let input = parse_hook_input(AgentKind::Cursor, json.as_bytes()).unwrap();
        assert_eq!(input.file_paths, vec![PathBuf::from("/tmp/cursor.ts")]);
    }

    #[test]
    fn parse_hook_input_extracts_cursor_multi_file_edits_array() {
        let json = r#"{
            "hook_event_name": "PostToolUse",
            "tool_name": "MultiEdit",
            "tool_input": {
                "edits": [
                    { "filePath": "/tmp/a.ts", "content": "a" },
                    { "filePath": "/tmp/b.ts", "content": "b" }
                ]
            },
            "cwd": "/home/user"
        }"#;
        let input = parse_hook_input(AgentKind::Cursor, json.as_bytes()).unwrap();
        assert_eq!(
            input.file_paths,
            vec![PathBuf::from("/tmp/a.ts"), PathBuf::from("/tmp/b.ts")]
        );
    }

    #[test]
    fn parse_hook_input_deduplicates_paths_from_scalar_and_edits() {
        let json = r#"{
            "hook_event_name": "PostToolUse",
            "tool_name": "Edit",
            "tool_input": {
                "filePath": "/tmp/a.ts",
                "edits": [
                    { "filePath": "/tmp/a.ts", "content": "dup" }
                ]
            },
            "cwd": "/home/user"
        }"#;
        let input = parse_hook_input(AgentKind::Cursor, json.as_bytes()).unwrap();
        assert_eq!(input.file_paths, vec![PathBuf::from("/tmp/a.ts")]);
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
    fn scan_scars_skips_fenced_code_blocks_in_markdown() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("spec.md");
        fs::write(
            &file,
            "# Spec\n\nExample:\n\n```html\n<!-- @kizu[ask]: example in fence -->\n```\n\nEnd.\n",
        )
        .unwrap();

        let hits = scan_scars(&[file]);
        assert!(hits.is_empty(), "fenced code block scar should be ignored");
    }

    #[test]
    fn scan_scars_detects_real_scar_outside_fence_in_markdown() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("notes.md");
        fs::write(
            &file,
            "# Notes\n\n<!-- @kizu[ask]: real scar outside fence -->\n\n```\n<!-- @kizu[ask]: example -->\n```\n",
        )
        .unwrap();

        let hits = scan_scars(&[file]);
        assert_eq!(hits.len(), 1, "only the scar outside the fence");
        assert_eq!(hits[0].message, "real scar outside fence -->");
        assert_eq!(hits[0].line_number, 3);
    }

    #[test]
    fn scan_scars_does_not_skip_fences_in_non_markdown_files() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("a.rs");
        // Rust files should not get fence-aware treatment even if
        // they happen to contain triple backticks in a string.
        fs::write(
            &file,
            "let s = \"```\";\n// @kizu[ask]: real scar\nlet t = \"```\";\n",
        )
        .unwrap();

        let hits = scan_scars(&[file]);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, "ask");
    }

    #[test]
    fn format_additional_context_claude_code_envelope() {
        let hits = vec![ScarHit {
            path: PathBuf::from("src/foo.rs"),
            line_number: 10,
            kind: "ask".into(),
            message: "explain this".into(),
        }];
        let json_str = format_additional_context(AgentKind::ClaudeCode, &hits).expect("non-empty");
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        let ctx = parsed["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(ctx.contains("src/foo.rs:10"));
        assert!(ctx.contains("@kizu[ask]"));
    }

    #[test]
    fn format_additional_context_cursor_envelope() {
        let hits = vec![ScarHit {
            path: PathBuf::from("src/foo.rs"),
            line_number: 5,
            kind: "reject".into(),
            message: "revert".into(),
        }];
        let json_str = format_additional_context(AgentKind::Cursor, &hits).expect("non-empty");
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        // Cursor uses flat "additional_context" key, not hookSpecificOutput.
        let ctx = parsed["additional_context"].as_str().unwrap();
        assert!(ctx.contains("src/foo.rs:5"));
        assert!(ctx.contains("@kizu[reject]"));
        assert!(parsed.get("hookSpecificOutput").is_none());
    }

    #[test]
    fn format_additional_context_codex_envelope() {
        let hits = vec![ScarHit {
            path: PathBuf::from("lib.py"),
            line_number: 3,
            kind: "free".into(),
            message: "note".into(),
        }];
        let json_str = format_additional_context(AgentKind::Codex, &hits).expect("non-empty");
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        // Codex uses flat "additionalContext" key.
        let ctx = parsed["additionalContext"].as_str().unwrap();
        assert!(ctx.contains("lib.py:3"));
        assert!(ctx.contains("@kizu[free]"));
        assert!(parsed.get("hookSpecificOutput").is_none());
    }

    #[test]
    fn format_additional_context_qwen_uses_claude_code_envelope() {
        let hits = vec![ScarHit {
            path: PathBuf::from("a.rs"),
            line_number: 1,
            kind: "ask".into(),
            message: "why".into(),
        }];
        let json_str = format_additional_context(AgentKind::QwenCode, &hits).expect("non-empty");
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert!(
            parsed["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .is_some()
        );
    }

    #[test]
    fn format_additional_context_cline_uses_claude_code_envelope() {
        let hits = vec![ScarHit {
            path: PathBuf::from("a.rs"),
            line_number: 1,
            kind: "ask".into(),
            message: "why".into(),
        }];
        let json_str = format_additional_context(AgentKind::Cline, &hits).expect("non-empty");
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert!(
            parsed["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .is_some()
        );
    }

    #[test]
    fn format_additional_context_returns_none_when_no_hits() {
        assert!(format_additional_context(AgentKind::ClaudeCode, &[]).is_none());
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

    #[test]
    fn sanitize_event_strips_content_and_adds_timestamp() {
        let input = NormalizedHookInput {
            session_id: Some("sess-1".to_string()),
            hook_event_name: "PostToolUse".to_string(),
            tool_name: Some("Edit".to_string()),
            file_paths: vec![PathBuf::from("/tmp/foo.rs")],
            cwd: Some(PathBuf::from("/tmp/project")),
            stop_hook_active: false,
        };
        let event = sanitize_event(&input);
        assert_eq!(event.session_id.as_deref(), Some("sess-1"));
        assert_eq!(event.hook_event_name, "PostToolUse");
        assert_eq!(event.tool_name.as_deref(), Some("Edit"));
        assert_eq!(event.file_paths, vec![PathBuf::from("/tmp/foo.rs")]);
        assert_eq!(event.cwd, PathBuf::from("/tmp/project"));
        assert!(event.timestamp_ms > 0);
    }

    #[test]
    fn sanitize_event_serialized_json_has_no_content_fields() {
        let input = NormalizedHookInput {
            session_id: Some("sess-2".to_string()),
            hook_event_name: "PostToolUse".to_string(),
            tool_name: Some("Write".to_string()),
            file_paths: vec![PathBuf::from("/tmp/bar.rs")],
            cwd: Some(PathBuf::from("/tmp")),
            stop_hook_active: false,
        };
        let event = sanitize_event(&input);
        let json = serde_json::to_string(&event).unwrap();
        // Verify no content/response/prompt fields leak through.
        assert!(!json.contains("\"content\""));
        assert!(!json.contains("\"new_string\""));
        assert!(!json.contains("\"old_string\""));
        assert!(!json.contains("\"output\""));
        assert!(!json.contains("\"prompt\""));
        // Verify expected fields are present.
        assert!(json.contains("\"session_id\""));
        assert!(json.contains("\"tool_name\""));
        assert!(json.contains("\"file_paths\""));
        assert!(json.contains("\"timestamp_ms\""));
    }

    #[test]
    fn write_event_creates_file_with_correct_content() {
        let tmp = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("KIZU_STATE_DIR", tmp.path().to_str().unwrap()) };

        let event = SanitizedEvent {
            session_id: Some("test-sess".to_string()),
            hook_event_name: "PostToolUse".to_string(),
            tool_name: Some("Edit".to_string()),
            file_paths: vec![PathBuf::from("/tmp/foo.rs")],
            cwd: PathBuf::from("/tmp"),
            timestamp_ms: 1700000000000,
        };
        let path = write_event(&event).unwrap();

        unsafe { std::env::remove_var("KIZU_STATE_DIR") };

        assert!(path.exists());
        assert!(path.to_str().unwrap().contains("1700000000000-Edit.json"));

        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: SanitizedEvent = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn write_event_sets_0600_permissions() {
        let tmp = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("KIZU_STATE_DIR", tmp.path().to_str().unwrap()) };

        let event = SanitizedEvent {
            session_id: None,
            hook_event_name: "PostToolUse".to_string(),
            tool_name: Some("Write".to_string()),
            file_paths: vec![],
            cwd: PathBuf::from("/tmp"),
            timestamp_ms: 1700000000001,
        };
        let path = write_event(&event).unwrap();

        unsafe { std::env::remove_var("KIZU_STATE_DIR") };

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn prune_removes_old_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let events_dir = tmp.path().join("events");
        std::fs::create_dir_all(&events_dir).unwrap();

        // Write an "old" event (timestamp 1000, effectively ancient).
        let old_file = events_dir.join("1000-Edit.json");
        std::fs::write(&old_file, "{}").unwrap();

        // Write a "recent" event.
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let new_file = events_dir.join(format!("{now_ms}-Write.json"));
        std::fs::write(&new_file, "{}").unwrap();

        let removed = prune_event_log_in(&events_dir, Duration::from_secs(3600), 1000).unwrap();

        assert_eq!(removed, 1);
        assert!(!old_file.exists());
        assert!(new_file.exists());
    }

    #[test]
    fn prune_enforces_max_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let events_dir = tmp.path().join("events");
        std::fs::create_dir_all(&events_dir).unwrap();

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        // Create 5 recent events.
        for i in 0..5 {
            let file = events_dir.join(format!("{}-Edit.json", now_ms + i));
            std::fs::write(&file, "{}").unwrap();
        }

        // Prune with max_entries = 3.
        let removed = prune_event_log_in(&events_dir, Duration::from_secs(86400), 3).unwrap();

        assert_eq!(removed, 2);
        let remaining: Vec<_> = std::fs::read_dir(&events_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(remaining.len(), 3);
    }

    #[test]
    fn prune_returns_zero_when_events_dir_missing() {
        let removed = prune_event_log_in(
            Path::new("/nonexistent/path"),
            Duration::from_secs(3600),
            1000,
        )
        .unwrap();
        assert_eq!(removed, 0);
    }

    #[test]
    fn scan_scars_from_index_reads_staged_blob_not_worktree() {
        use std::process::Command;

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // Set up a git repo.
        Command::new("git")
            .args(["init"])
            .current_dir(root)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(root)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(root)
            .output()
            .unwrap();

        let file = root.join("a.rs");

        // Write file with a scar and stage it.
        fs::write(&file, "fn main() {}\n// @kizu[ask]: staged scar\n").unwrap();
        Command::new("git")
            .args(["add", "a.rs"])
            .current_dir(root)
            .output()
            .unwrap();

        // Now remove the scar from the worktree (but leave it staged).
        fs::write(&file, "fn main() {}\n").unwrap();

        // scan_scars (worktree) should find nothing.
        let worktree_hits = scan_scars(std::slice::from_ref(&file));
        assert!(worktree_hits.is_empty(), "worktree should be clean");

        // scan_scars_from_index should still find the staged scar.
        let index_hits = scan_scars_from_index(root, &[file]);
        assert_eq!(index_hits.len(), 1);
        assert_eq!(index_hits[0].kind, "ask");
        assert_eq!(index_hits[0].message, "staged scar");
    }
}
