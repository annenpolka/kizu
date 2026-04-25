use anyhow::Result;
use std::path::Path;

mod detect;
mod install;
mod pre_commit;
mod settings_json;
mod teardown;
mod types;

pub use detect::detect_agents;
use install::{ask_scope_fallback, fallback_scope, install_agent, needs_scope_fallback};
#[cfg(test)]
use install::{install_cursor, kizu_hook_command_with_bin};
use pre_commit::install_git_pre_commit_hook;
#[cfg(test)]
use pre_commit::pre_commit_shim_body;
pub(crate) use pre_commit::shell_single_quote;
#[cfg(test)]
use settings_json::{HookCmd, merge_hooks_into_settings, remove_kizu_hooks_from_json};
pub use teardown::run_teardown;
#[cfg(test)]
use teardown::teardown_cursor_user_hooks;
pub use types::{AgentKind, DetectedAgent, InstallReport, Scope, SupportLevel, support_level};

// ── M6: agent detection ─────────────────────────────────────────

// ── M7: scope + install ─────────────────────────────────────────

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

#[cfg(test)]
mod tests;
