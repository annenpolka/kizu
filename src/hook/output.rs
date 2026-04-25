use super::{AgentKind, ScarHit};

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
