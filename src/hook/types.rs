use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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

/// One `@kizu[<kind>]: <message>` hit found inside a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScarHit {
    pub path: PathBuf,
    pub line_number: usize,
    pub kind: String,
    pub message: String,
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
