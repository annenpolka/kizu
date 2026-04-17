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

/// Kizu-managed shim marker embedded in generated pre-commit hooks.
const KIZU_SHIM_MARKER: &str = "# kizu-managed-shim";

/// Wrap `s` in POSIX single quotes, escaping any interior single
/// quotes with the standard `'\''` sequence. Produces a token that
/// `sh` always parses as exactly one literal argument, regardless of
/// spaces, `$`, `"`, `\`, `*`, etc. Kizu binaries installed under
/// paths like `/Users/John Doe/.cargo/bin/kizu` would otherwise
/// wordsplit in the generated pre-commit shim.
fn shell_single_quote(s: &str) -> String {
    let escaped = s.replace('\'', r"'\''");
    format!("'{escaped}'")
}

/// Render the `/bin/sh` shim body that `.git/hooks/pre-commit`
/// writes. Extracted from `install_git_pre_commit_hook` so the
/// quoting contract can be unit-tested without touching the
/// filesystem.
fn pre_commit_shim_body(bin: &str, has_user_hook: bool) -> String {
    let bin_q = shell_single_quote(bin);
    if has_user_hook {
        format!(
            "#!/bin/sh\n{KIZU_SHIM_MARKER}\nset -e\n\
             # Run the original user hook first.\n\
             \"$(dirname \"$0\")/pre-commit.user\" \"$@\"\n\
             # Then run kizu scar guard.\n\
             {bin_q} hook-pre-commit\n"
        )
    } else {
        format!(
            "#!/bin/sh\n{KIZU_SHIM_MARKER}\nset -e\n\
             # kizu scar guard\n\
             {bin_q} hook-pre-commit\n"
        )
    }
}

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
        let bin = kizu_bin_for_scope(Scope::ProjectLocal);
        let shim = pre_commit_shim_body(&bin, true);
        std::fs::write(&hook_path, shim)?;
        println!(
            "  git pre-commit hook: wrapped existing hook → {}",
            user_hook.display()
        );
    } else {
        let bin = kizu_bin_for_scope(Scope::ProjectLocal);
        let shim = pre_commit_shim_body(&bin, false);
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
/// A single hook command entry within a matcher group.
struct HookCmd<'a> {
    command: &'a str,
    timeout: Option<u32>,
    is_async: bool,
}

/// Each event holds an array of **matcher groups**, each with a
/// `matcher` string (tool name filter, `""` = match all) and a
/// `hooks` sub-array of command objects.
/// Extract the `hook-<name>` token from a kizu hook invocation so we
/// can reconcile by subcommand instead of by full command string.
/// Returns `None` when the command does not look like a kizu hook
/// (e.g. a user's linter), so non-kizu entries are never matched.
fn kizu_command_token(command: &str) -> Option<String> {
    for token in command.split_whitespace() {
        if let Some(rest) = token.strip_prefix("hook-") {
            if rest.is_empty() {
                continue;
            }
            return Some(format!("hook-{rest}"));
        }
    }
    None
}

fn merge_hooks_into_settings(
    path: &Path,
    hooks: &[(&str, &str, &[HookCmd<'_>])], // (event_name, matcher, commands)
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

    for (event_name, matcher, commands) in hooks {
        let matcher_groups = hooks_map
            .entry(*event_name)
            .or_insert_with(|| serde_json::json!([]));
        let arr = matcher_groups
            .as_array_mut()
            .ok_or_else(|| anyhow::anyhow!("hooks.{event_name} is not an array"))?;

        // Gather all existing commands on this event so we can
        // reconcile per-command instead of per-matcher-group. This is
        // the upgrade path: a config that already contains
        // `hook-post-tool` from an older kizu install must still
        // receive new commands like `hook-log-event`.
        let existing_cmds: Vec<String> = arr
            .iter()
            .flat_map(|group| {
                group
                    .get("hooks")
                    .and_then(|h| h.as_array())
                    .into_iter()
                    .flatten()
            })
            .filter_map(|cmd| cmd.get("command").and_then(|v| v.as_str()))
            .map(|s| s.to_string())
            .collect();

        // Partition the requested commands into "already present" and
        // "missing". `kizu_command_token` extracts the subcommand
        // (`hook-post-tool`, `hook-log-event`, …) so `--agent`
        // differences or binary-path differences do not spawn a
        // duplicate entry.
        let mut missing: Vec<&HookCmd<'_>> = Vec::new();
        for cmd in commands.iter() {
            let want_token = kizu_command_token(cmd.command);
            let is_present = existing_cmds
                .iter()
                .any(|existing| want_token.is_some() && kizu_command_token(existing) == want_token);
            if is_present {
                skipped += 1;
            } else {
                missing.push(cmd);
            }
        }

        if missing.is_empty() {
            continue;
        }

        // Prefer appending to an existing matcher group that already
        // holds a kizu command with the same `matcher`, so the
        // upgraded config remains a single cohesive group instead of
        // splitting kizu's hooks across two siblings.
        let target_idx = arr.iter().position(|group| {
            let matches_matcher = group
                .get("matcher")
                .and_then(|v| v.as_str())
                .is_some_and(|m| m == *matcher);
            let has_kizu = group
                .get("hooks")
                .and_then(|h| h.as_array())
                .is_some_and(|cmds| {
                    cmds.iter().any(|cmd| {
                        cmd.get("command")
                            .and_then(|v| v.as_str())
                            .and_then(kizu_command_token)
                            .is_some()
                    })
                });
            matches_matcher && has_kizu
        });

        let cmd_values: Vec<serde_json::Value> = missing
            .iter()
            .map(|cmd| {
                let mut obj = serde_json::json!({
                    "type": "command",
                    "command": cmd.command,
                });
                if let Some(t) = cmd.timeout {
                    obj["timeout"] = serde_json::json!(t);
                }
                if cmd.is_async {
                    obj["async"] = serde_json::json!(true);
                }
                obj
            })
            .collect();

        if let Some(idx) = target_idx {
            let group_hooks = arr[idx]
                .get_mut("hooks")
                .and_then(|h| h.as_array_mut())
                .ok_or_else(|| anyhow::anyhow!("hooks.{event_name}[{idx}].hooks is not array"))?;
            for v in cmd_values {
                group_hooks.push(v);
                added += 1;
            }
        } else {
            arr.push(serde_json::json!({
                "matcher": matcher,
                "hooks": cmd_values
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
    let bin = kizu_bin_for_scope(scope);
    let post_cmd = format!("{bin} hook-post-tool --agent claude-code");
    let log_cmd = format!("{bin} hook-log-event");
    let stop_cmd = format!("{bin} hook-stop --agent claude-code");
    let hooks: &[(&str, &str, &[HookCmd<'_>])] = &[
        (
            "PostToolUse",
            "Edit|Write|MultiEdit",
            &[
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
            ],
        ),
        (
            "Stop",
            "",
            &[HookCmd {
                command: &stop_cmd,
                timeout: Some(10),
                is_async: false,
            }],
        ),
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

    let bin = kizu_bin_for_scope(scope);
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
    let bin = kizu_bin_for_scope(scope);
    let stop_cmd = format!("{bin} hook-stop --agent codex");
    let hooks: &[(&str, &str, &[HookCmd<'_>])] = &[(
        "Stop",
        "",
        &[HookCmd {
            command: &stop_cmd,
            timeout: Some(10),
            is_async: false,
        }],
    )];
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
    let bin = kizu_bin_for_scope(scope);
    let post_cmd = format!("{bin} hook-post-tool --agent qwen");
    let log_cmd = format!("{bin} hook-log-event");
    let stop_cmd = format!("{bin} hook-stop --agent qwen");
    let hooks: &[(&str, &str, &[HookCmd<'_>])] = &[
        (
            "PostToolUse",
            "Edit|Write|MultiEdit",
            &[
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
            ],
        ),
        (
            "Stop",
            "",
            &[HookCmd {
                command: &stop_cmd,
                timeout: Some(10),
                is_async: false,
            }],
        ),
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
                    &[
                        HookCmd {
                            command: "kizu hook-post-tool --agent claude-code",
                            timeout: Some(10),
                            is_async: false,
                        },
                        HookCmd {
                            command: "kizu hook-log-event",
                            timeout: None,
                            is_async: true,
                        },
                    ],
                ),
                (
                    "Stop",
                    "",
                    &[HookCmd {
                        command: "kizu hook-stop --agent claude-code",
                        timeout: Some(10),
                        is_async: false,
                    }],
                ),
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
        assert_eq!(cmds.len(), 2);
        assert_eq!(cmds[0]["type"].as_str().unwrap(), "command");
        assert!(
            cmds[0]["command"]
                .as_str()
                .unwrap()
                .contains("kizu hook-post-tool")
        );
        assert!(cmds[0].get("async").is_none());
        assert_eq!(cmds[1]["async"].as_bool(), Some(true));
        assert!(
            cmds[1]["command"]
                .as_str()
                .unwrap()
                .contains("hook-log-event")
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
                &[HookCmd {
                    command: "kizu hook-post-tool --agent claude-code",
                    timeout: Some(10),
                    is_async: false,
                }],
            )],
        )
        .unwrap();

        assert_eq!(added, 0);
        assert_eq!(skipped, 1);
    }

    #[test]
    fn merge_hooks_adds_missing_commands_to_existing_kizu_group() {
        // Upgrade path: a user installed kizu from main (which only had
        // `hook-post-tool`), then re-runs `kizu init` on v0.3. The new
        // async `hook-log-event` must be appended even though a kizu
        // command is already present — otherwise stream mode stays
        // inert after the upgrade.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        fs::write(
            &path,
            r#"{"hooks":{"PostToolUse":[{"matcher":"Edit|Write|MultiEdit","hooks":[{"type":"command","command":"kizu hook-post-tool --agent claude-code","timeout":10}]}]}}"#,
        )
        .unwrap();

        merge_hooks_into_settings(
            &path,
            &[(
                "PostToolUse",
                "Edit|Write|MultiEdit",
                &[
                    HookCmd {
                        command: "kizu hook-post-tool --agent claude-code",
                        timeout: Some(10),
                        is_async: false,
                    },
                    HookCmd {
                        command: "kizu hook-log-event",
                        timeout: None,
                        is_async: true,
                    },
                ],
            )],
        )
        .unwrap();

        let doc: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let post = doc["hooks"]["PostToolUse"].as_array().unwrap();
        let cmds: Vec<&str> = post
            .iter()
            .flat_map(|g| g["hooks"].as_array().into_iter().flatten())
            .filter_map(|c| c["command"].as_str())
            .collect();
        assert!(
            cmds.iter().any(|c| c.contains("hook-post-tool")),
            "pre-existing hook-post-tool must remain: {cmds:?}"
        );
        assert!(
            cmds.iter().any(|c| c.contains("hook-log-event")),
            "missing hook-log-event must be appended on rerun: {cmds:?}"
        );
        // The duplicate `hook-post-tool` must not be added twice.
        let post_tool_count = cmds.iter().filter(|c| c.contains("hook-post-tool")).count();
        assert_eq!(post_tool_count, 1, "duplicate must be suppressed");
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
                &[HookCmd {
                    command: "kizu hook-post-tool --agent claude-code",
                    timeout: Some(10),
                    is_async: false,
                }],
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
    fn shell_single_quote_wraps_and_escapes_embedded_quotes() {
        // Plain path: wrapped only.
        assert_eq!(
            super::shell_single_quote("/usr/bin/kizu"),
            "'/usr/bin/kizu'"
        );
        // Path with a space: still one literal token after the shim parses it.
        assert_eq!(
            super::shell_single_quote("/Users/John Doe/kizu"),
            "'/Users/John Doe/kizu'"
        );
        // Path containing a single quote gets the standard '\'' escape.
        assert_eq!(
            super::shell_single_quote("/home/ev'an/kizu"),
            r"'/home/ev'\''an/kizu'"
        );
    }

    #[test]
    fn pre_commit_shim_body_quotes_bin_with_spaces() {
        let shim = super::pre_commit_shim_body("/Users/John Doe/kizu", false);
        // The shim must contain the quoted form so `/bin/sh` does
        // not wordsplit at the space.
        assert!(
            shim.contains("'/Users/John Doe/kizu' hook-pre-commit"),
            "shim body should quote the binary path; got:\n{shim}"
        );
        // And must NOT contain the unquoted form that would break.
        assert!(
            !shim.contains("/Users/John Doe/kizu hook-pre-commit"),
            "shim body must not embed the unquoted path; got:\n{shim}"
        );
    }

    #[test]
    fn pre_commit_shim_body_with_user_hook_still_quotes_bin() {
        let shim = super::pre_commit_shim_body("/p with space/kizu", true);
        assert!(shim.contains("'/p with space/kizu' hook-pre-commit"));
        assert!(shim.contains("pre-commit.user"));
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

    /// The interactive agent picker pads two columns: agent name to 12
    /// cells, support-level pill to 18 cells. Both paddings must be
    /// done via `pad_visible` (not `{:<N}`) so ANSI escapes don't
    /// inflate the count. Cf. ADR-0019.
    #[test]
    fn agent_label_columns_are_visually_aligned() {
        use crate::prompt::visible_width;

        let detected = AgentKind::all()
            .iter()
            .map(|&kind| DetectedAgent {
                kind,
                binary_found: matches!(kind, AgentKind::ClaudeCode),
                config_dir_found: matches!(kind, AgentKind::ClaudeCode),
                recommended: matches!(kind, AgentKind::ClaudeCode),
            })
            .collect::<Vec<_>>();

        // Build the labels exactly as `select_agents_interactive` would.
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

        // For each label, the prefix up to where the **third** column
        // begins must land at exactly 12 + 2 + 18 + 2 = 34 cells.
        let third_col_start_cells = 12 + 2 + 18 + 2;
        for (d, label) in detected.iter().zip(labels.iter()) {
            let status = detection_status_colored(d);
            let total = visible_width(label);
            let status_w = visible_width(&status);
            assert_eq!(
                total.checked_sub(status_w),
                Some(third_col_start_cells),
                "misaligned row for {:?}: total={} status_w={} label={:?}",
                d.kind,
                total,
                status_w,
                label,
            );
        }
    }
}
