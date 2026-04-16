use anyhow::{Context, Result};
use std::fmt;
use std::path::{Path, PathBuf};

// ── M6: agent detection ─────────────────────────────────────────

/// Supported AI coding agents for hook installation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentKind {
    ClaudeCode,
    Cursor,
    Codex,
    QwenCode,
    Cline,
    Gemini,
}

impl fmt::Display for AgentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ClaudeCode => write!(f, "Claude Code"),
            Self::Cursor => write!(f, "Cursor"),
            Self::Codex => write!(f, "Codex CLI"),
            Self::QwenCode => write!(f, "Qwen Code"),
            Self::Cline => write!(f, "Cline"),
            Self::Gemini => write!(f, "Gemini CLI"),
        }
    }
}

impl AgentKind {
    pub fn all() -> &'static [AgentKind] {
        &[
            Self::ClaudeCode,
            Self::Cursor,
            Self::Codex,
            Self::QwenCode,
            Self::Cline,
            Self::Gemini,
        ]
    }

    /// CLI name for `--agent` flag parsing.
    #[allow(dead_code)]
    pub fn cli_name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Cursor => "cursor",
            Self::Codex => "codex",
            Self::QwenCode => "qwen",
            Self::Cline => "cline",
            Self::Gemini => "gemini",
        }
    }

    pub fn from_cli_name(s: &str) -> Option<Self> {
        match s {
            "claude-code" | "claude" => Some(Self::ClaudeCode),
            "cursor" => Some(Self::Cursor),
            "codex" => Some(Self::Codex),
            "qwen" | "qwen-code" => Some(Self::QwenCode),
            "cline" => Some(Self::Cline),
            "gemini" => Some(Self::Gemini),
            _ => None,
        }
    }

    fn binary_name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude",
            Self::Cursor => "cursor",
            Self::Codex => "codex",
            Self::QwenCode => "qwen",
            Self::Cline => "cline", // not a real binary, detected by config dir
            Self::Gemini => "gemini",
        }
    }

    /// Project-local config directory (relative to worktree root).
    /// `None` if this agent only has a user-level config.
    fn project_config_dir(self) -> Option<&'static str> {
        match self {
            Self::ClaudeCode => Some(".claude"),
            Self::Cursor => Some(".cursor"),
            Self::QwenCode => Some(".qwen"),
            Self::Cline => Some(".clinerules"),
            Self::Codex | Self::Gemini => None,
        }
    }

    /// User-level config directory (absolute). `None` if this agent
    /// only has project-level config.
    fn user_config_dir(self) -> Option<PathBuf> {
        let home = dirs::home_dir()?;
        match self {
            Self::Codex => Some(home.join(".codex")),
            Self::Gemini => Some(home.join(".gemini")),
            Self::ClaudeCode => Some(home.join(".claude")),
            Self::Cursor => None, // cursor user config is different path
            Self::QwenCode => None,
            Self::Cline => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupportLevel {
    /// PostToolUse + Stop hooks both available.
    Full,
    /// Only Stop hook (Codex: PreTool/PostTool Bash-only).
    StopOnly,
    /// PostToolUse only, no Stop gate (Cline).
    PostToolOnlyBestEffort,
    /// No hook mechanism; stream/scar-only (Gemini).
    WriteSideOnly,
}

impl fmt::Display for SupportLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full => write!(f, "Full"),
            Self::StopOnly => write!(f, "Stop only"),
            Self::PostToolOnlyBestEffort => write!(f, "PostTool best-effort: no Stop gate"),
            Self::WriteSideOnly => write!(f, "Write-side only"),
        }
    }
}

pub fn support_level(kind: AgentKind) -> SupportLevel {
    match kind {
        AgentKind::ClaudeCode | AgentKind::Cursor | AgentKind::QwenCode => SupportLevel::Full,
        AgentKind::Codex => SupportLevel::StopOnly,
        AgentKind::Cline => SupportLevel::PostToolOnlyBestEffort,
        AgentKind::Gemini => SupportLevel::WriteSideOnly,
    }
}

#[derive(Debug, Clone)]
pub struct DetectedAgent {
    pub kind: AgentKind,
    pub binary_found: bool,
    pub config_dir_found: bool,
    pub recommended: bool,
    pub support_level: SupportLevel,
}

/// Detect which AI coding agents are available on this system.
/// Checks binary existence via `which` and config directory presence.
pub fn detect_agents(project_root: &Path) -> Vec<DetectedAgent> {
    AgentKind::all()
        .iter()
        .map(|&kind| {
            let binary_found = which::which(kind.binary_name()).is_ok();
            let config_dir_found = kind
                .project_config_dir()
                .map(|d| project_root.join(d).is_dir())
                .unwrap_or(false)
                || kind.user_config_dir().map(|d| d.is_dir()).unwrap_or(false);
            let sl = support_level(kind);
            let recommended =
                binary_found && config_dir_found && !matches!(sl, SupportLevel::WriteSideOnly);
            DetectedAgent {
                kind,
                binary_found,
                config_dir_found,
                recommended,
                support_level: sl,
            }
        })
        .collect()
}

// ── M7: scope + install ─────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// `.claude/settings.local.json` etc. — gitignored, personal.
    ProjectLocal,
    /// `.claude/settings.json` etc. — committed, team-shared.
    ProjectShared,
    /// `~/.claude/settings.json` etc. — global user config.
    User,
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProjectLocal => write!(f, "project-local"),
            Self::ProjectShared => write!(f, "project-shared"),
            Self::User => write!(f, "user"),
        }
    }
}

#[derive(Debug)]
pub struct InstallReport {
    pub agent: AgentKind,
    pub files_modified: Vec<PathBuf>,
    pub entries_added: usize,
    pub entries_skipped: usize,
    pub warnings: Vec<String>,
}

/// Resolve the kizu binary path for embedding in hook commands.
/// Prefers the current executable's absolute path so hooks work
/// even when `kizu` is not globally on PATH. Falls back to bare
/// `kizu` if the executable path cannot be determined.
fn kizu_bin() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(String::from))
        .unwrap_or_else(|| "kizu".to_string())
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

    // Validate scope compatibility for every selected agent before
    // writing anything. This prevents partial installs when one
    // agent rejects the chosen scope after earlier agents already
    // had their configs mutated.
    for agent_kind in &selected_agents {
        if let Some(reason) = scope_incompatible(*agent_kind, scope) {
            anyhow::bail!(
                "{agent_kind} is incompatible with scope {scope}: {reason}\n\
                 Choose a different scope or deselect {agent_kind}.",
            );
        }
    }

    for agent_kind in &selected_agents {
        let report = install_agent(*agent_kind, scope, project_root)?;
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
fn c_cyan(s: &str) -> String {
    format!("\x1b[36m{s}\x1b[0m")
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

fn support_level_colored(sl: SupportLevel) -> String {
    match sl {
        SupportLevel::Full => c_green(&format!("● {sl}")),
        SupportLevel::StopOnly => c_yellow(&format!("◐ {sl}")),
        SupportLevel::PostToolOnlyBestEffort => c_yellow(&format!("◐ {sl}")),
        SupportLevel::WriteSideOnly => c_dim(&format!("○ {sl}")),
    }
}

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
    use dialoguer::{MultiSelect, theme::ColorfulTheme};

    let items: Vec<String> = detected
        .iter()
        .map(|d| {
            format!(
                "{}  {}  {}",
                c_bold(&format!("{:<12}", d.kind.to_string())),
                support_level_colored(d.support_level),
                detection_status_colored(d),
            )
        })
        .collect();

    let defaults: Vec<bool> = detected.iter().map(|d| d.recommended).collect();

    let selections = MultiSelect::with_theme(&ColorfulTheme::default())
        .with_prompt(format!(
            "{}  {}",
            c_cyan("?"),
            c_bold("Select agents to install hooks for"),
        ))
        .items(&items)
        .defaults(&defaults)
        .interact()
        .context("agent selection cancelled")?;

    Ok(selections.into_iter().map(|i| detected[i].kind).collect())
}

fn select_scope_interactive() -> Result<Scope> {
    use dialoguer::{Select, theme::ColorfulTheme};

    let items = [
        format!(
            "{}  {}",
            c_bold("project-local"),
            c_dim("(gitignored · .claude/settings.local.json) ← recommended"),
        ),
        format!(
            "{}  {}",
            c_bold("project-shared"),
            c_dim("(committed · .claude/settings.json)"),
        ),
        format!(
            "{}  {}",
            c_bold("user"),
            c_dim("(global · ~/.claude/settings.json)"),
        ),
    ];
    let selection = Select::with_theme(&ColorfulTheme::default())
        .with_prompt(format!("{}  {}", c_cyan("?"), c_bold("Install scope"),))
        .items(&items)
        .default(0)
        .interact()
        .context("scope selection cancelled")?;

    Ok(if selection == 0 {
        Scope::ProjectLocal
    } else if selection == 1 {
        Scope::ProjectShared
    } else {
        Scope::User
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

/// Kizu-managed shim marker embedded in generated pre-commit hooks.
const KIZU_SHIM_MARKER: &str = "# kizu-managed-shim";

/// Install a kizu-managed pre-commit shim that guarantees
/// `kizu hook-pre-commit` always runs, even when the repo has a
/// pre-existing hook script that may contain `exit`/`exec`.
///
/// Strategy:
/// - **No existing hook**: write a simple shim.
/// - **Existing hook is already kizu-managed**: no-op.
/// - **Existing non-kizu hook**: rename it to `pre-commit.user`,
///   then write a shim that calls the original *and* kizu. Both
///   must succeed (fail-fast with `set -e`).
fn install_git_pre_commit_hook(project_root: &Path) -> Result<()> {
    let git_dir = crate::git::git_dir(project_root)?;
    let hooks_dir = git_dir.join("hooks");
    std::fs::create_dir_all(&hooks_dir)?;
    let hook_path = hooks_dir.join("pre-commit");

    if hook_path.exists() {
        let content = std::fs::read_to_string(&hook_path)?;
        if content.contains(KIZU_SHIM_MARKER) {
            println!("  git pre-commit hook: already installed");
            return Ok(());
        }
        // Existing non-kizu hook → rename and wrap.
        let user_hook = hooks_dir.join("pre-commit.user");
        if user_hook.exists() {
            anyhow::bail!(
                "cannot install pre-commit shim: backup path already exists at {}\n\
                 Remove or rename it manually, then re-run `kizu init`.",
                user_hook.display()
            );
        }
        std::fs::rename(&hook_path, &user_hook)?;
        let bin = kizu_bin();
        let shim = format!(
            "#!/bin/sh\n{KIZU_SHIM_MARKER}\nset -e\n\
             # Run the original user hook first.\n\
             \"$(dirname \"$0\")/pre-commit.user\" \"$@\"\n\
             # Then run kizu scar guard.\n\
             {bin} hook-pre-commit\n"
        );
        std::fs::write(&hook_path, shim)?;
        println!(
            "  git pre-commit hook: wrapped existing hook → {}",
            user_hook.display()
        );
    } else {
        let bin = kizu_bin();
        let shim = format!(
            "#!/bin/sh\n{KIZU_SHIM_MARKER}\nset -e\n\
             # kizu scar guard\n\
             {bin} hook-pre-commit\n"
        );
        std::fs::write(&hook_path, shim)?;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook_path, std::fs::Permissions::from_mode(0o755))?;
    }

    println!(
        "  git pre-commit hook: installed at {}",
        hook_path.display()
    );
    Ok(())
}

/// Returns `Some(reason)` if `kind` cannot be installed at `scope`.
///
/// Only Claude Code is known to support `settings.local.json` (the
/// `project-local` gitignored variant). Qwen reads `.qwen/settings.json`
/// only, so `project-local` would write a file Qwen never loads.
fn scope_incompatible(kind: AgentKind, scope: Scope) -> Option<&'static str> {
    match (kind, scope) {
        (AgentKind::Cursor, Scope::User) => Some("Cursor only supports project-level hooks"),
        (AgentKind::Cursor, Scope::ProjectLocal) => {
            Some("Cursor has no local-only config; hooks.json is always committable")
        }
        (AgentKind::Codex, Scope::ProjectLocal) => {
            Some("Codex has no local-only config; .codex/hooks.json is always committable")
        }
        (AgentKind::Cline, Scope::User | Scope::ProjectLocal) => {
            Some("Cline uses file-based project hooks only")
        }
        (AgentKind::QwenCode, Scope::ProjectLocal) => {
            Some("Qwen Code does not read settings.local.json; use project-shared or user")
        }
        (AgentKind::Gemini, _) => Some("Gemini CLI has no hook mechanism"),
        _ => None,
    }
}

fn install_agent(kind: AgentKind, scope: Scope, project_root: &Path) -> Result<InstallReport> {
    match kind {
        AgentKind::ClaudeCode => install_claude_code(scope, project_root),
        AgentKind::Cursor => install_cursor(scope, project_root),
        AgentKind::Codex => install_codex(scope, project_root),
        AgentKind::QwenCode => install_qwen(scope, project_root),
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

/// Merge kizu hook entries into a Claude Code / Qwen Code style
/// settings.json. Creates the file + parent dirs if missing.
///
/// Claude Code hook schema (as of 2026):
/// ```json
/// {
///   "hooks": {
///     "PostToolUse": [
///       {
///         "matcher": "Edit|Write",
///         "hooks": [
///           { "type": "command", "command": "kizu hook-post-tool ...", "timeout": 10 }
///         ]
///       }
///     ]
///   }
/// }
/// ```
/// Each event holds an array of **matcher groups**, each with a
/// `matcher` string (tool name filter, `""` = match all) and a
/// `hooks` sub-array of command objects.
fn merge_hooks_into_settings(
    path: &Path,
    hooks: &[(&str, &str, &str)], // (event_name, matcher, command)
) -> Result<(usize, usize)> {
    let mut doc: serde_json::Value = if path.exists() {
        let content =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&content).with_context(|| format!("parsing {}", path.display()))?
    } else {
        serde_json::json!({})
    };

    let hooks_obj = doc
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("settings.json root is not an object"))?
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));

    let hooks_map = hooks_obj
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("hooks is not an object"))?;

    let mut added = 0;
    let mut skipped = 0;

    for &(event_name, matcher, command) in hooks {
        let matcher_groups = hooks_map
            .entry(event_name)
            .or_insert_with(|| serde_json::json!([]));
        let arr = matcher_groups
            .as_array_mut()
            .ok_or_else(|| anyhow::anyhow!("hooks.{event_name} is not an array"))?;

        // Check if any existing matcher group already has a kizu hook.
        let already = arr.iter().any(|group| {
            group
                .get("hooks")
                .and_then(|h| h.as_array())
                .is_some_and(|cmds| {
                    cmds.iter().any(|cmd| {
                        cmd.get("command")
                            .and_then(|v| v.as_str())
                            .is_some_and(|c| {
                                c.contains("kizu hook-")
                                    || c.contains(" hook-post-tool")
                                    || c.contains(" hook-stop")
                            })
                    })
                })
        });

        if already {
            skipped += 1;
        } else {
            arr.push(serde_json::json!({
                "matcher": matcher,
                "hooks": [
                    {
                        "type": "command",
                        "command": command,
                        "timeout": 10
                    }
                ]
            }));
            added += 1;
        }
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let json_str = serde_json::to_string_pretty(&doc)?;
    std::fs::write(path, json_str).with_context(|| format!("writing {}", path.display()))?;

    Ok((added, skipped))
}

// ── Per-agent installers ────────────────────────────────────────

fn install_claude_code(scope: Scope, project_root: &Path) -> Result<InstallReport> {
    let path = config_path(AgentKind::ClaudeCode, scope, project_root)?;
    let bin = kizu_bin();
    let post_cmd = format!("{bin} hook-post-tool --agent claude-code");
    let stop_cmd = format!("{bin} hook-stop --agent claude-code");
    let hooks = &[
        ("PostToolUse", "Edit|Write|MultiEdit", post_cmd.as_str()),
        ("Stop", "", stop_cmd.as_str()),
    ];
    let (added, skipped) = merge_hooks_into_settings(&path, hooks)?;
    Ok(InstallReport {
        agent: AgentKind::ClaudeCode,
        files_modified: vec![path],
        entries_added: added,
        entries_skipped: skipped,
        warnings: vec![],
    })
}

fn install_cursor(scope: Scope, project_root: &Path) -> Result<InstallReport> {
    // Cursor uses .cursor/hooks.json (not settings.json).
    let dir = match scope {
        Scope::ProjectLocal | Scope::ProjectShared => project_root.join(".cursor"),
        Scope::User => anyhow::bail!("Cursor only supports project-level hooks"),
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

    let bin = kizu_bin();
    let post_cmd = format!("{bin} hook-post-tool --agent cursor");
    let stop_cmd = format!("{bin} hook-stop --agent cursor");
    let entries = &[
        ("afterFileEdit", post_cmd.as_str()),
        ("stop", stop_cmd.as_str()),
    ];

    let mut added = 0;
    let mut skipped = 0;
    for &(event, command) in entries {
        let arr = hooks_map
            .entry(event)
            .or_insert_with(|| serde_json::json!([]))
            .as_array_mut()
            .ok_or_else(|| anyhow::anyhow!("hooks.{event} is not an array"))?;

        let already = arr.iter().any(|e| {
            e.get("command").and_then(|v| v.as_str()).is_some_and(|c| {
                c.contains("kizu hook-")
                    || c.contains(" hook-post-tool")
                    || c.contains(" hook-stop")
            })
        });
        if already {
            skipped += 1;
        } else {
            arr.push(serde_json::json!({"command": command, "timeout": 10}));
            added += 1;
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

fn install_codex(scope: Scope, project_root: &Path) -> Result<InstallReport> {
    let path = match scope {
        Scope::ProjectLocal | Scope::ProjectShared => {
            project_root.join(".codex").join("hooks.json")
        }
        Scope::User => dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("cannot determine home dir"))?
            .join(".codex")
            .join("hooks.json"),
    };
    // Codex: Stop only (PreTool/PostTool is Bash-only).
    let bin = kizu_bin();
    let stop_cmd = format!("{bin} hook-stop --agent codex");
    let hooks = &[("Stop", "", stop_cmd.as_str())];
    let (added, skipped) = merge_hooks_into_settings(&path, hooks)?;
    Ok(InstallReport {
        agent: AgentKind::Codex,
        files_modified: vec![path],
        entries_added: added,
        entries_skipped: skipped,
        warnings: vec![
            "Codex PreTool/PostTool currently only matches Bash tools; Stop hook only.".into(),
        ],
    })
}

fn install_qwen(scope: Scope, project_root: &Path) -> Result<InstallReport> {
    let path = config_path(AgentKind::QwenCode, scope, project_root)?;
    let bin = kizu_bin();
    let post_cmd = format!("{bin} hook-post-tool --agent qwen");
    let stop_cmd = format!("{bin} hook-stop --agent qwen");
    let hooks = &[
        ("PostToolUse", "Edit|Write|MultiEdit", post_cmd.as_str()),
        ("Stop", "", stop_cmd.as_str()),
    ];
    let (added, skipped) = merge_hooks_into_settings(&path, hooks)?;
    Ok(InstallReport {
        agent: AgentKind::QwenCode,
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
        if content.contains("hook-post-tool") || content.contains("hook-stop") {
            skipped = 1;
        } else {
            // Append to existing hook script.
            let mut new = content;
            if !new.ends_with('\n') {
                new.push('\n');
            }
            new.push_str(&format!("{} hook-post-tool --agent cline\n", kizu_bin()));
            std::fs::write(&hook_file, new)?;
            added = 1;
        }
    } else {
        std::fs::write(
            &hook_file,
            format!("#!/bin/sh\n{} hook-post-tool --agent cline\n", kizu_bin()),
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

// ── M8: teardown ────────────────────────────────────────────────

pub fn run_teardown(project_root: &Path) -> Result<()> {
    println!();
    println!(
        "  {}  {}",
        c_bold(&c_magenta("傷")),
        c_bold("kizu teardown"),
    );
    println!();

    let detected = detect_agents(project_root);
    let mut any_removed = false;

    for agent in &detected {
        let mut agent_removed = false;

        if let Some(dir) = agent.kind.project_config_dir() {
            for filename in ["settings.json", "settings.local.json"] {
                let path = project_root.join(dir).join(filename);
                if remove_kizu_hooks_from_json(&path)? {
                    println!(
                        "  {}  {}  {}",
                        c_bold(&format!("{:<12}", agent.kind.to_string())),
                        c_green("✓ removed"),
                        c_dim(&format!("→ {}", path.display())),
                    );
                    agent_removed = true;
                    any_removed = true;
                }
            }
        }
        if let Some(dir) = agent.kind.user_config_dir() {
            let path = dir.join("settings.json");
            if remove_kizu_hooks_from_json(&path)? {
                println!(
                    "  {}  {}  {}",
                    c_bold(&format!("{:<12}", agent.kind.to_string())),
                    c_green("✓ removed"),
                    c_dim(&format!("→ {}", path.display())),
                );
                agent_removed = true;
                any_removed = true;
            }
        }
        if agent.kind == AgentKind::Cursor {
            let path = project_root.join(".cursor").join("hooks.json");
            if remove_kizu_hooks_from_json(&path)? {
                println!(
                    "  {}  {}  {}",
                    c_bold(&format!("{:<12}", "Cursor")),
                    c_green("✓ removed"),
                    c_dim(&format!("→ {}", path.display())),
                );
                agent_removed = true;
                any_removed = true;
            }
        }
        if agent.kind == AgentKind::Codex {
            // Codex project-scoped install writes to <repo>/.codex/hooks.json
            // which is not covered by project_config_dir() (returns None for Codex).
            let path = project_root.join(".codex").join("hooks.json");
            if remove_kizu_hooks_from_json(&path)? {
                println!(
                    "  {}  {}  {}",
                    c_bold(&format!("{:<12}", "Codex CLI")),
                    c_green("✓ removed"),
                    c_dim(&format!("→ {}", path.display())),
                );
                agent_removed = true;
                any_removed = true;
            }
        }
        if agent.kind == AgentKind::Cline {
            let hook_file = project_root
                .join(".clinerules")
                .join("hooks")
                .join("PostToolUse");
            if hook_file.exists() {
                let content = std::fs::read_to_string(&hook_file)?;
                if content.contains("hook-post-tool") || content.contains("hook-stop") {
                    let cleaned: String = content
                        .lines()
                        .filter(|l| !l.contains("hook-post-tool") && !l.contains("hook-stop"))
                        .collect::<Vec<_>>()
                        .join("\n");
                    if cleaned.trim().is_empty() || cleaned.trim() == "#!/bin/sh" {
                        std::fs::remove_file(&hook_file)?;
                    } else {
                        std::fs::write(&hook_file, cleaned + "\n")?;
                    }
                    println!(
                        "  {}  {}  {}",
                        c_bold(&format!("{:<12}", "Cline")),
                        c_green("✓ removed"),
                        c_dim(&format!("→ {}", hook_file.display())),
                    );
                    agent_removed = true;
                    any_removed = true;
                }
            }
        }

        if !agent_removed && (agent.binary_found || agent.config_dir_found) {
            println!(
                "  {}  {}",
                c_bold(&format!("{:<12}", agent.kind.to_string())),
                c_dim("– no kizu hooks found"),
            );
        }
    }

    // Remove git pre-commit hook.
    if remove_git_pre_commit_hook(project_root)? {
        println!(
            "  {}  {}",
            c_bold(&format!("{:<12}", "git")),
            c_green("✓ pre-commit hook removed"),
        );
        any_removed = true;
    }

    // Remove session file.
    crate::session::remove_session(project_root);

    println!();
    if any_removed {
        println!("  {}  {}", c_green("✓"), c_bold("kizu hooks removed"));
    } else {
        println!(
            "  {}  {}",
            c_dim("–"),
            c_dim("No kizu hooks found to remove"),
        );
    }
    println!();

    Ok(())
}

/// Remove kizu's pre-commit hook and restore the user's original if
/// it was wrapped by the shim installer.
fn remove_git_pre_commit_hook(project_root: &Path) -> Result<bool> {
    let git_dir = match crate::git::git_dir(project_root) {
        Ok(d) => d,
        Err(_) => return Ok(false),
    };
    let hooks_dir = git_dir.join("hooks");
    let hook_path = hooks_dir.join("pre-commit");
    if !hook_path.exists() {
        return Ok(false);
    }
    let content = std::fs::read_to_string(&hook_path)?;
    if !content.contains("kizu hook-pre-commit") && !content.contains(KIZU_SHIM_MARKER) {
        return Ok(false);
    }

    // Remove the kizu shim.
    std::fs::remove_file(&hook_path)?;

    // Restore the original user hook if it was renamed by install.
    let user_hook = hooks_dir.join("pre-commit.user");
    if user_hook.exists() {
        std::fs::rename(&user_hook, &hook_path)?;
    }

    Ok(true)
}

/// Remove all hook entries whose `command` starts with `kizu hook-`
/// from a JSON settings file. Returns `true` if anything was removed.
fn remove_kizu_hooks_from_json(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let content = std::fs::read_to_string(path)?;
    let mut doc: serde_json::Value = serde_json::from_str(&content)?;

    let Some(hooks) = doc.get_mut("hooks").and_then(|v| v.as_object_mut()) else {
        return Ok(false);
    };

    let mut removed = false;
    for (_event, entries) in hooks.iter_mut() {
        if let Some(arr) = entries.as_array_mut() {
            let before = arr.len();
            // New schema: each element is a matcher group with a
            // `hooks` sub-array. Remove groups that contain kizu commands.
            arr.retain(|group| {
                // Old flat schema: { "command": "kizu hook-..." }
                let flat_kizu = group
                    .get("command")
                    .and_then(|v| v.as_str())
                    .is_some_and(|c| {
                        c.contains("kizu hook-")
                            || c.contains(" hook-post-tool")
                            || c.contains(" hook-stop")
                    });
                // New nested schema: { "matcher": "...", "hooks": [{ "command": "kizu hook-..." }] }
                let nested_kizu =
                    group
                        .get("hooks")
                        .and_then(|h| h.as_array())
                        .is_some_and(|cmds| {
                            cmds.iter().any(|cmd| {
                                cmd.get("command")
                                    .and_then(|v| v.as_str())
                                    .is_some_and(|c| {
                                        c.contains("kizu hook-")
                                            || c.contains(" hook-post-tool")
                                            || c.contains(" hook-stop")
                                    })
                            })
                        });
                !flat_kizu && !nested_kizu
            });
            if arr.len() < before {
                removed = true;
            }
        }
    }

    // Clean up empty arrays and empty hooks object.
    hooks.retain(|_, v| v.as_array().is_some_and(|a| !a.is_empty()));

    if removed {
        std::fs::write(path, serde_json::to_string_pretty(&doc)?)?;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn merge_hooks_creates_settings_with_matcher_group_schema() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".claude").join("settings.json");

        let (added, skipped) = merge_hooks_into_settings(
            &path,
            &[
                (
                    "PostToolUse",
                    "Edit|Write",
                    "kizu hook-post-tool --agent claude-code",
                ),
                ("Stop", "", "kizu hook-stop --agent claude-code"),
            ],
        )
        .unwrap();

        assert_eq!(added, 2);
        assert_eq!(skipped, 0);
        let doc: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let post = &doc["hooks"]["PostToolUse"].as_array().unwrap()[0];
        assert_eq!(post["matcher"].as_str().unwrap(), "Edit|Write");
        let cmds = post["hooks"].as_array().unwrap();
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0]["type"].as_str().unwrap(), "command");
        assert!(
            cmds[0]["command"]
                .as_str()
                .unwrap()
                .contains("kizu hook-post-tool")
        );
    }

    #[test]
    fn merge_hooks_skips_duplicate_kizu_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        // Pre-existing kizu hook in new matcher-group schema.
        fs::write(
            &path,
            r#"{"hooks":{"PostToolUse":[{"matcher":"Edit|Write","hooks":[{"type":"command","command":"kizu hook-post-tool --agent claude-code","timeout":10}]}]}}"#,
        )
        .unwrap();

        let (added, skipped) = merge_hooks_into_settings(
            &path,
            &[(
                "PostToolUse",
                "Edit|Write",
                "kizu hook-post-tool --agent claude-code",
            )],
        )
        .unwrap();

        assert_eq!(added, 0);
        assert_eq!(skipped, 1);
    }

    #[test]
    fn merge_hooks_preserves_existing_non_kizu_matcher_groups() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        fs::write(
            &path,
            r#"{"hooks":{"PostToolUse":[{"matcher":"","hooks":[{"type":"command","command":"my-linter","timeout":5}]}]}}"#,
        )
        .unwrap();

        merge_hooks_into_settings(
            &path,
            &[(
                "PostToolUse",
                "Edit|Write",
                "kizu hook-post-tool --agent claude-code",
            )],
        )
        .unwrap();

        let doc: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let arr = doc["hooks"]["PostToolUse"].as_array().unwrap();
        assert_eq!(arr.len(), 2, "existing matcher group must be preserved");
        assert!(
            arr[0]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .contains("my-linter")
        );
    }

    #[test]
    fn remove_kizu_hooks_strips_nested_kizu_matcher_groups() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        fs::write(
            &path,
            r#"{"hooks":{"PostToolUse":[{"matcher":"","hooks":[{"type":"command","command":"my-linter"}]},{"matcher":"Edit|Write","hooks":[{"type":"command","command":"kizu hook-post-tool --agent claude-code"}]}],"Stop":[{"matcher":"","hooks":[{"type":"command","command":"kizu hook-stop --agent claude-code"}]}]}}"#,
        )
        .unwrap();

        let removed = remove_kizu_hooks_from_json(&path).unwrap();
        assert!(removed);

        let doc: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let post = doc["hooks"]["PostToolUse"].as_array().unwrap();
        assert_eq!(post.len(), 1);
        assert!(
            post[0]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .contains("my-linter")
        );
        // Stop array was entirely kizu → key removed.
        assert!(doc["hooks"].get("Stop").is_none());
    }

    #[test]
    fn remove_kizu_hooks_returns_false_when_no_kizu_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        fs::write(
            &path,
            r#"{"hooks":{"PostToolUse":[{"matcher":"","hooks":[{"type":"command","command":"my-linter"}]}]}}"#,
        )
        .unwrap();

        let removed = remove_kizu_hooks_from_json(&path).unwrap();
        assert!(!removed);
    }

    #[test]
    fn remove_kizu_hooks_returns_false_for_missing_file() {
        let removed = remove_kizu_hooks_from_json(Path::new("/nonexistent/settings.json")).unwrap();
        assert!(!removed);
    }

    #[test]
    fn teardown_removes_codex_project_scoped_hooks_json() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // Simulate a Codex project-scoped install: <repo>/.codex/hooks.json
        let codex_dir = root.join(".codex");
        fs::create_dir_all(&codex_dir).unwrap();
        let hooks_path = codex_dir.join("hooks.json");
        fs::write(
            &hooks_path,
            r#"{"hooks":{"Stop":[{"matcher":"","hooks":[{"type":"command","command":"kizu hook-stop --agent codex","timeout":10}]}]}}"#,
        )
        .unwrap();

        // Verify removal works via the same function teardown uses.
        let removed = remove_kizu_hooks_from_json(&hooks_path).unwrap();
        assert!(removed, "should remove kizu hooks from .codex/hooks.json");

        // After removal the hooks object should be empty.
        let doc: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&hooks_path).unwrap()).unwrap();
        let hooks = doc["hooks"].as_object().unwrap();
        assert!(hooks.is_empty(), "all kizu entries should be gone");
    }
}
