use anyhow::{Context, Result};
use serde::Deserialize;
use std::io::Read;
use std::path::PathBuf;

use super::{AgentKind, NormalizedHookInput};

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
