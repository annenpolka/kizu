use anyhow::Result;
use std::path::{Path, PathBuf};

mod detect;
mod pre_commit;
mod settings_json;
mod teardown;
mod types;

pub use detect::detect_agents;
use pre_commit::install_git_pre_commit_hook;
#[cfg(test)]
use pre_commit::pre_commit_shim_body;
pub(crate) use pre_commit::shell_single_quote;
#[cfg(test)]
use settings_json::remove_kizu_hooks_from_json;
use settings_json::{
    HookCmd, contains_kizu_hook_command, kizu_command_token, merge_hooks_into_settings,
};
pub use teardown::run_teardown;
#[cfg(test)]
use teardown::teardown_cursor_user_hooks;
pub use types::{AgentKind, DetectedAgent, InstallReport, Scope, SupportLevel, support_level};

// ── M6: agent detection ─────────────────────────────────────────

// ── M7: scope + install ─────────────────────────────────────────

/// Resolve the kizu binary path for embedding in hook commands.
///
/// - `project-shared`: bare `kizu` — the file is committed and must
///   be portable across machines. Assumes kizu is on PATH.
/// - `project-local` / `user`: absolute path via `current_exe()` so
///   hooks work even when kizu is not globally installed. These files
///   are personal (gitignored or in `~/`) so machine-specific paths
///   are acceptable.
fn kizu_bin_for_scope(scope: Scope) -> String {
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
fn kizu_hook_command_with_bin(scope: Scope, bin: &str, rest: &str) -> String {
    match scope {
        Scope::ProjectShared => format!("{bin} {rest}"),
        Scope::ProjectLocal | Scope::User => {
            format!("{} {}", shell_single_quote(bin), rest)
        }
    }
}

/// Run `kizu init` interactively or non-interactively.
pub fn run_init(
    project_root: &Path,
    agents_flag: Option<&[String]>,
    scope_flag: Option<&str>,
    non_interactive: bool,
) -> Result<()> {
    if !non_interactive {
        print_banner();
    }

    let detected = detect_agents(project_root);

    let selected_agents: Vec<AgentKind> = if let Some(names) = agents_flag {
        names
            .iter()
            .map(|n| {
                AgentKind::from_cli_name(n).ok_or_else(|| anyhow::anyhow!("unknown agent: {n}"))
            })
            .collect::<Result<Vec<_>>>()?
    } else if non_interactive {
        // Non-interactive without --agent: install all recommended.
        detected
            .iter()
            .filter(|d| d.recommended)
            .map(|d| d.kind)
            .collect()
    } else {
        select_agents_interactive(&detected)?
    };

    if selected_agents.is_empty() {
        println!("No agents selected.");
        return Ok(());
    }

    let scope = if let Some(s) = scope_flag {
        match s {
            "project-local" | "local" => Scope::ProjectLocal,
            "project-shared" | "project" | "shared" => Scope::ProjectShared,
            "user" => Scope::User,
            other => anyhow::bail!(
                "unknown scope: {other} (expected: project-local, project-shared, user)"
            ),
        }
    } else if non_interactive {
        Scope::ProjectLocal
    } else {
        select_scope_interactive()?
    };

    for agent_kind in &selected_agents {
        let effective_scope = if needs_scope_fallback(*agent_kind, scope) {
            if non_interactive {
                let fb = fallback_scope(*agent_kind);
                println!(
                    "  {}  {} scope unavailable for {}; falling back to {}",
                    c_yellow("⚠"),
                    scope,
                    agent_kind,
                    fb,
                );
                fb
            } else {
                match ask_scope_fallback(*agent_kind, scope)? {
                    Some(s) => s,
                    None => continue, // user chose to skip
                }
            }
        } else {
            scope
        };
        let report = install_agent(*agent_kind, effective_scope, project_root)?;
        print_report(&report);
    }

    // Install git pre-commit hook to block commits with unresolved scars.
    install_git_pre_commit_hook(project_root)?;

    println!();
    println!("  {}  {}", c_green("✓"), c_bold("kizu hooks installed"),);
    println!("  {}", c_dim("Run `kizu teardown` to remove all hooks"),);
    println!();

    Ok(())
}

// ── ANSI helpers ────────────────────────────────────────────────

fn c_bold(s: &str) -> String {
    format!("\x1b[1m{s}\x1b[0m")
}
fn c_green(s: &str) -> String {
    format!("\x1b[32m{s}\x1b[0m")
}
fn c_yellow(s: &str) -> String {
    format!("\x1b[33m{s}\x1b[0m")
}
fn c_dim(s: &str) -> String {
    format!("\x1b[2m{s}\x1b[0m")
}
fn c_magenta(s: &str) -> String {
    format!("\x1b[35m{s}\x1b[0m")
}

fn print_banner() {
    println!();
    println!("  {}  {}", c_bold(&c_magenta("傷")), c_bold("kizu init"),);
    println!(
        "  {}",
        c_dim("Hook installer for AI coding agent scar review")
    );
    println!();
}

/// Short prompt-friendly label for a support level. The `SupportLevel`
/// Display impl is intentionally verbose for error messages; here we
/// pick terse labels that fit inside the fixed column of the picker.
fn support_level_short(sl: SupportLevel) -> &'static str {
    match sl {
        SupportLevel::Full => "Full",
        SupportLevel::StopOnly => "Stop only",
        SupportLevel::PostToolOnlyBestEffort => "PostTool only",
        SupportLevel::WriteSideOnly => "Write-side only",
    }
}

/// Render the support-level pill with an ANSI color + icon. Matches the
/// pre-8b0f9dd design, now safely renderable because `src/prompt.rs`
/// measures item width via `unicode-width` (see ADR-0019).
fn support_level_colored(sl: SupportLevel) -> String {
    let label = support_level_short(sl);
    match sl {
        SupportLevel::Full => c_green(&format!("● {label}")),
        SupportLevel::StopOnly => c_yellow(&format!("◐ {label}")),
        SupportLevel::PostToolOnlyBestEffort => c_yellow(&format!("◐ {label}")),
        SupportLevel::WriteSideOnly => c_dim(&format!("○ {label}")),
    }
}

/// Render the detection state (binary + config dir presence) with a
/// single-glyph icon + color.
fn detection_status_colored(d: &DetectedAgent) -> String {
    if d.binary_found && d.config_dir_found {
        c_green("✓ detected")
    } else if d.binary_found {
        c_yellow("~ bin only")
    } else {
        c_dim("✗ not found")
    }
}

fn select_agents_interactive(detected: &[DetectedAgent]) -> Result<Vec<AgentKind>> {
    // Item layout (visible cells, not bytes — every padding uses
    // `pad_visible` so ANSI escapes inside colored spans don't inflate
    // the count):
    //
    //   <agent name, 12>  <support-level pill, 18>  <detection status>
    //
    // 18 fits the widest short pill `○ Write-side only` (17 cells) with
    // one trailing pad cell. The dialoguer era's `{:<N}` formatter
    // counted bytes (including ANSI) and misaligned these columns —
    // see ADR-0019.
    let labels: Vec<String> = detected
        .iter()
        .map(|d| {
            let sl = support_level(d.kind);
            format!(
                "{}  {}  {}",
                pad_visible(&c_bold(&d.kind.to_string()), 12),
                pad_visible(&support_level_colored(sl), 18),
                detection_status_colored(d),
            )
        })
        .collect();
    let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let defaults: Vec<bool> = detected.iter().map(|d| d.recommended).collect();

    let selections = crate::prompt::run_multi_select(
        "Select agents to install hooks for",
        &label_refs,
        &defaults,
    )?
    .ok_or_else(|| anyhow::anyhow!("agent selection cancelled"))?;

    Ok(selections.into_iter().map(|i| detected[i].kind).collect())
}

/// Pad `s` on the right with spaces so its **visible** width equals
/// `target_cells`. Never truncates (returns `s` unchanged if it already
/// exceeds the target). Uses the prompt module's visible-width helper
/// so ANSI escapes don't inflate the pad count.
fn pad_visible(s: &str, target_cells: usize) -> String {
    let w = crate::prompt::visible_width(s);
    if w >= target_cells {
        s.to_string()
    } else {
        let mut out = String::with_capacity(s.len() + (target_cells - w));
        out.push_str(s);
        for _ in 0..(target_cells - w) {
            out.push(' ');
        }
        out
    }
}

fn select_scope_interactive() -> Result<Scope> {
    let items: [String; 3] = [
        format!(
            "{}  {}",
            c_bold("project-local"),
            c_dim("(gitignored, personal) ← recommended"),
        ),
        format!(
            "{}  {}",
            c_bold("project-shared"),
            c_dim("(committed, team-shared)"),
        ),
        format!(
            "{}  {}",
            c_bold("user"),
            c_dim("(global, ~/.claude/settings.json)"),
        ),
    ];
    let item_refs: Vec<&str> = items.iter().map(String::as_str).collect();
    let selection = crate::prompt::run_select_one("Install scope", &item_refs, 0)?
        .ok_or_else(|| anyhow::anyhow!("scope selection cancelled"))?;

    Ok(match selection {
        0 => Scope::ProjectLocal,
        1 => Scope::ProjectShared,
        _ => Scope::User,
    })
}

fn print_report(report: &InstallReport) {
    let status = if report.entries_added > 0 {
        c_green(&format!("✓ {} entries added", report.entries_added))
    } else {
        c_dim(&format!(
            "– {} skipped (already installed)",
            report.entries_skipped
        ))
    };
    println!(
        "  {}  {}",
        c_bold(&format!("{:<12}", report.agent.to_string())),
        status,
    );
    for path in &report.files_modified {
        println!(
            "  {}  {}",
            c_dim("             "),
            c_dim(&format!("→ {}", path.display())),
        );
    }
    for warning in &report.warnings {
        eprintln!("  {}  {} {warning}", c_dim("             "), c_yellow("⚠"),);
    }
}

// ── Installer dispatch ──────────────────────────────────────────

/// Returns `true` when the requested scope is not natively supported
/// by this agent and a fallback choice is needed.
fn needs_scope_fallback(kind: AgentKind, requested: Scope) -> bool {
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
fn fallback_scope(kind: AgentKind) -> Scope {
    match kind {
        AgentKind::Cline => Scope::ProjectShared,
        _ => Scope::User,
    }
}

/// Interactively ask the user what to do when the chosen scope is
/// unavailable for a specific agent. Returns `None` to skip.
fn ask_scope_fallback(kind: AgentKind, requested: Scope) -> Result<Option<Scope>> {
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

fn install_agent(kind: AgentKind, scope: Scope, project_root: &Path) -> Result<InstallReport> {
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

fn install_cursor(scope: Scope, project_root: &Path) -> Result<InstallReport> {
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

#[cfg(test)]
mod tests;
