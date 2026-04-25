use anyhow::Result;
use std::path::{Path, PathBuf};

use super::pre_commit::shell_single_quote;
use super::settings_json::{
    HookCmd, contains_kizu_hook_command, kizu_command_token, merge_hooks_into_settings,
};
use super::{AgentKind, InstallReport, Scope, c_bold, c_yellow};

/// Resolve the kizu binary path for embedding in hook commands.
///
/// - `project-shared`: bare `kizu` — the file is committed and must
///   be portable across machines. Assumes kizu is on PATH.
/// - `project-local` / `user`: absolute path via `current_exe()` so
///   hooks work even when kizu is not globally installed. These files
///   are personal (gitignored or in `~/`) so machine-specific paths
///   are acceptable.
pub(in crate::init) fn kizu_bin_for_scope(scope: Scope) -> String {
    match scope {
        Scope::ProjectShared => "kizu".to_string(),
        _ => std::env::current_exe()
            .ok()
            .and_then(|p| p.to_str().map(String::from))
            .unwrap_or_else(|| "kizu".to_string()),
    }
}

/// Build a hook `command` string that is safe to hand to the agent's
/// shell-based hook runner. Project-local and user scopes embed an
/// absolute `current_exe()` path, so paths containing spaces (e.g.
/// `/Users/John Doe/.cargo/bin/kizu`) or shell metacharacters would
/// break `sh -c` parsing and either exec the wrong argv[0] or trigger
/// unintended expansion. Project-shared keeps the bare `kizu` token
/// because the committed config must work on any machine where kizu
/// is resolvable through PATH.
fn kizu_hook_command(scope: Scope, rest: &str) -> String {
    let bin = kizu_bin_for_scope(scope);
    kizu_hook_command_with_bin(scope, &bin, rest)
}

/// Testable variant of [`kizu_hook_command`] that accepts an
/// explicit `bin` path. The production call resolves the bin via
/// `current_exe()`; tests pass fabricated paths to cover quoting
/// edge cases (spaces, embedded quotes) without touching the
/// filesystem or the process's own installation.
pub(super) fn kizu_hook_command_with_bin(scope: Scope, bin: &str, rest: &str) -> String {
    match scope {
        Scope::ProjectShared => format!("{bin} {rest}"),
        Scope::ProjectLocal | Scope::User => {
            format!("{} {}", shell_single_quote(bin), rest)
        }
    }
}

/// Returns `true` when the requested scope is not natively supported
/// by this agent and a fallback choice is needed.
pub(super) fn needs_scope_fallback(kind: AgentKind, requested: Scope) -> bool {
    match (kind, requested) {
        (AgentKind::ClaudeCode, _) => false,
        (AgentKind::Cursor, Scope::ProjectLocal) => true,
        (AgentKind::Codex, Scope::ProjectLocal) => true,
        (AgentKind::QwenCode, Scope::ProjectLocal) => true,
        (AgentKind::Cline, Scope::ProjectLocal | Scope::User) => true,
        _ => false,
    }
}

/// Default fallback scope for non-interactive mode.
pub(super) fn fallback_scope(kind: AgentKind) -> Scope {
    match kind {
        AgentKind::Cline => Scope::ProjectShared,
        _ => Scope::User,
    }
}

/// Interactively ask the user what to do when the chosen scope is
/// unavailable for a specific agent. Returns `None` to skip.
pub(super) fn ask_scope_fallback(kind: AgentKind, requested: Scope) -> Result<Option<Scope>> {
    println!(
        "\n  {}  {} does not support {} scope",
        c_yellow("⚠"),
        c_bold(&kind.to_string()),
        requested,
    );

    let choices: Vec<(&str, Option<Scope>)> = match kind {
        AgentKind::Cline => vec![
            (
                "Install to project-shared (committed)",
                Some(Scope::ProjectShared),
            ),
            ("Skip this agent", None),
        ],
        _ => vec![
            (
                "Install to project-shared (committed)",
                Some(Scope::ProjectShared),
            ),
            ("Install to user (global, personal)", Some(Scope::User)),
            ("Skip this agent", None),
        ],
    };

    let labels: Vec<&str> = choices.iter().map(|(l, _)| *l).collect();
    let prompt_text = format!("How to install {} hooks?", kind);
    let selection = crate::prompt::run_select_one(&prompt_text, &labels, 0)?
        .ok_or_else(|| anyhow::anyhow!("scope fallback selection cancelled"))?;

    Ok(choices[selection].1)
}

pub(super) fn install_agent(
    kind: AgentKind,
    scope: Scope,
    project_root: &Path,
) -> Result<InstallReport> {
    match kind {
        AgentKind::ClaudeCode => install_settings_hook_agent(
            AgentKind::ClaudeCode,
            scope,
            project_root,
            "claude-code",
            Some("Edit|Write|MultiEdit"),
            vec![],
        ),
        AgentKind::Cursor => install_cursor(scope, project_root),
        AgentKind::Codex => install_settings_hook_agent(
            AgentKind::Codex,
            scope,
            project_root,
            "codex",
            None,
            vec![
                "Codex PreTool/PostTool currently only matches Bash tools; Stop hook only.".into(),
            ],
        ),
        AgentKind::QwenCode => install_settings_hook_agent(
            AgentKind::QwenCode,
            scope,
            project_root,
            "qwen",
            Some("Edit|Write|MultiEdit"),
            vec![],
        ),
        AgentKind::Cline => install_cline(project_root),
        AgentKind::Gemini => install_gemini(),
    }
}

/// Resolve the config file path for the given agent + scope.
fn config_path(kind: AgentKind, scope: Scope, project_root: &Path) -> Result<PathBuf> {
    match scope {
        Scope::ProjectLocal => {
            let dir = kind
                .project_config_dir()
                .ok_or_else(|| anyhow::anyhow!("{kind} has no project-level config"))?;
            Ok(project_root.join(dir).join("settings.local.json"))
        }
        Scope::ProjectShared => {
            let dir = kind
                .project_config_dir()
                .ok_or_else(|| anyhow::anyhow!("{kind} has no project-level config"))?;
            Ok(project_root.join(dir).join("settings.json"))
        }
        Scope::User => {
            let dir = kind
                .user_config_dir()
                .ok_or_else(|| anyhow::anyhow!("{kind} has no user-level config"))?;
            Ok(dir.join("settings.json"))
        }
    }
}

// ── JSON hook merging ───────────────────────────────────────────

// ── Per-agent installers ────────────────────────────────────────

fn settings_hook_path(kind: AgentKind, scope: Scope, project_root: &Path) -> Result<PathBuf> {
    if kind == AgentKind::Codex {
        return Ok(match scope {
            Scope::ProjectLocal | Scope::ProjectShared => {
                project_root.join(".codex").join("hooks.json")
            }
            Scope::User => dirs::home_dir()
                .ok_or_else(|| anyhow::anyhow!("cannot determine home dir"))?
                .join(".codex")
                .join("hooks.json"),
        });
    }
    config_path(kind, scope, project_root)
}

fn install_settings_hook_agent(
    kind: AgentKind,
    scope: Scope,
    project_root: &Path,
    agent_arg: &str,
    post_matcher: Option<&str>,
    warnings: Vec<String>,
) -> Result<InstallReport> {
    let path = settings_hook_path(kind, scope, project_root)?;
    let log_cmd = kizu_hook_command(scope, "hook-log-event");
    let post_cmd = kizu_hook_command(scope, &format!("hook-post-tool --agent {agent_arg}"));
    let stop_cmd = kizu_hook_command(scope, &format!("hook-stop --agent {agent_arg}"));
    let post_cmds = [
        HookCmd {
            command: &post_cmd,
            timeout: Some(10),
            is_async: false,
        },
        HookCmd {
            command: &log_cmd,
            timeout: None,
            is_async: true,
        },
    ];
    let stop_cmds = [HookCmd {
        command: &stop_cmd,
        timeout: Some(10),
        is_async: false,
    }];
    let mut hooks: Vec<(&str, &str, &[HookCmd<'_>])> = Vec::with_capacity(2);
    if let Some(matcher) = post_matcher {
        hooks.push(("PostToolUse", matcher, post_cmds.as_slice()));
    }
    hooks.push(("Stop", "", stop_cmds.as_slice()));

    let (added, skipped) = merge_hooks_into_settings(&path, &hooks)?;
    Ok(InstallReport {
        agent: kind,
        files_modified: vec![path],
        entries_added: added,
        entries_skipped: skipped,
        warnings,
    })
}

pub(super) fn install_cursor(scope: Scope, project_root: &Path) -> Result<InstallReport> {
    // Cursor uses .cursor/hooks.json at project or user level.
    let dir = match scope {
        Scope::ProjectLocal | Scope::ProjectShared => project_root.join(".cursor"),
        Scope::User => dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("cannot determine home dir"))?
            .join(".cursor"),
    };
    let path = dir.join("hooks.json");

    let mut doc: serde_json::Value = if path.exists() {
        let content = std::fs::read_to_string(&path)?;
        serde_json::from_str(&content)?
    } else {
        serde_json::json!({"version": 1, "hooks": {}})
    };

    let hooks_map = doc
        .get_mut("hooks")
        .and_then(|v| v.as_object_mut())
        .ok_or_else(|| anyhow::anyhow!("hooks is not an object in hooks.json"))?;

    let post_cmd = kizu_hook_command(scope, "hook-post-tool --agent cursor");
    let log_cmd = kizu_hook_command(scope, "hook-log-event");
    let stop_cmd = kizu_hook_command(scope, "hook-stop --agent cursor");
    // `hook-log-event` rides alongside the scar scan on every edit
    // so Cursor edits produce the event files that power stream
    // mode. Without it, `SupportLevel::Full` was a lie for Cursor:
    // scar scanning worked, but the Stream view stayed empty.
    let entries: &[(&str, &[&str])] = &[
        ("afterFileEdit", &[post_cmd.as_str(), log_cmd.as_str()]),
        ("stop", &[stop_cmd.as_str()]),
    ];

    let mut added = 0;
    let mut skipped = 0;
    for &(event, commands) in entries {
        let arr = hooks_map
            .entry(event)
            .or_insert_with(|| serde_json::json!([]))
            .as_array_mut()
            .ok_or_else(|| anyhow::anyhow!("hooks.{event} is not an array"))?;

        // Reconcile per-command (matching on the `hook-*`
        // subcommand token) so a rerun of `kizu init` upgrades an
        // older install that lacks `hook-log-event`. The previous
        // logic short-circuited on any kizu entry and skipped
        // every command, which is how stream mode stayed inert on
        // upgrade.
        for command in commands {
            let want_token = kizu_command_token(command);
            let already = arr.iter().any(|e| {
                e.get("command")
                    .and_then(|v| v.as_str())
                    .and_then(kizu_command_token)
                    == want_token
                    && want_token.is_some()
            });
            if already {
                skipped += 1;
            } else {
                arr.push(serde_json::json!({"command": command, "timeout": 10}));
                added += 1;
            }
        }
    }

    std::fs::create_dir_all(&dir)?;
    std::fs::write(&path, serde_json::to_string_pretty(&doc)?)?;
    Ok(InstallReport {
        agent: AgentKind::Cursor,
        files_modified: vec![path],
        entries_added: added,
        entries_skipped: skipped,
        warnings: vec![],
    })
}

fn install_cline(project_root: &Path) -> Result<InstallReport> {
    // Cline uses file-based hooks: .clinerules/hooks/<EventType>
    let hook_dir = project_root.join(".clinerules").join("hooks");
    std::fs::create_dir_all(&hook_dir)?;
    let hook_file = hook_dir.join("PostToolUse");

    let mut skipped = 0;
    let mut added = 0;
    if hook_file.exists() {
        let content = std::fs::read_to_string(&hook_file)?;
        if contains_kizu_hook_command(&content) {
            skipped = 1;
        } else {
            // Append to existing hook script.
            let mut new = content;
            if !new.ends_with('\n') {
                new.push('\n');
            }
            new.push_str(&format!(
                "{} hook-post-tool --agent cline\n",
                kizu_bin_for_scope(Scope::ProjectShared)
            ));
            std::fs::write(&hook_file, new)?;
            added = 1;
        }
    } else {
        std::fs::write(
            &hook_file,
            format!(
                "#!/bin/sh\n{} hook-post-tool --agent cline\n",
                kizu_bin_for_scope(Scope::ProjectShared)
            ),
        )?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook_file, std::fs::Permissions::from_mode(0o755))?;
        }
        added = 1;
    }

    Ok(InstallReport {
        agent: AgentKind::Cline,
        files_modified: vec![hook_file],
        entries_added: added,
        entries_skipped: skipped,
        warnings: vec![
            "Cline lacks a Stop hook; unresolved scars cannot block task completion.".into(),
        ],
    })
}

fn install_gemini() -> Result<InstallReport> {
    println!("  Gemini CLI has no hook mechanism.");
    println!("  Stream integration (kizu consume-gemini-stream) is planned for a future release.");
    Ok(InstallReport {
        agent: AgentKind::Gemini,
        files_modified: vec![],
        entries_added: 0,
        entries_skipped: 0,
        warnings: vec!["Gemini CLI: pipe integration only, no auto-install.".into()],
    })
}
