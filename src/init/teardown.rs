use anyhow::Result;
use std::path::Path;

use super::pre_commit::remove_git_pre_commit_hook;
use super::settings_json::{contains_kizu_hook_command, remove_kizu_hooks_from_json};
use super::{AgentKind, c_bold, c_dim, c_green, c_magenta, detect_agents};

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
        let agent_label = agent.kind.to_string();

        if let Some(dir) = agent.kind.project_config_dir() {
            for filename in ["settings.json", "settings.local.json"] {
                let path = project_root.join(dir).join(filename);
                if remove_json_hooks_with_report(&agent_label, &path)? {
                    agent_removed = true;
                    any_removed = true;
                }
            }
        }
        if let Some(dir) = agent.kind.user_config_dir() {
            let path = dir.join("settings.json");
            if remove_json_hooks_with_report(&agent_label, &path)? {
                agent_removed = true;
                any_removed = true;
            }
        }
        if agent.kind == AgentKind::Cursor {
            let path = project_root.join(".cursor").join("hooks.json");
            if remove_json_hooks_with_report("Cursor", &path)? {
                agent_removed = true;
                any_removed = true;
            }
            // User-scope Cursor install lives at ~/.cursor/hooks.json,
            // which `AgentKind::user_config_dir()` intentionally
            // returns `None` for (Cursor uses hooks.json, not the
            // settings.json shape the generic path handles). Install
            // writes there; teardown must match.
            if let Some(home) = dirs::home_dir() {
                let path = home.join(".cursor").join("hooks.json");
                if remove_json_hooks_with_report("Cursor", &path)? {
                    agent_removed = true;
                    any_removed = true;
                }
            }
        }
        if agent.kind == AgentKind::Codex {
            // Codex project-scoped install writes to <repo>/.codex/hooks.json
            // which is not covered by project_config_dir() (returns None for Codex).
            let path = project_root.join(".codex").join("hooks.json");
            if remove_json_hooks_with_report("Codex CLI", &path)? {
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
                if contains_kizu_hook_command(&content) {
                    let cleaned: String = content
                        .lines()
                        .filter(|l| !contains_kizu_hook_command(l))
                        .collect::<Vec<_>>()
                        .join("\n");
                    if cleaned.trim().is_empty() || cleaned.trim() == "#!/bin/sh" {
                        std::fs::remove_file(&hook_file)?;
                    } else {
                        std::fs::write(&hook_file, cleaned + "\n")?;
                    }
                    print_removed("Cline", &hook_file);
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

fn print_removed(agent_label: &str, path: &Path) {
    println!(
        "  {}  {}  {}",
        c_bold(&format!("{agent_label:<12}")),
        c_green("✓ removed"),
        c_dim(&format!("→ {}", path.display())),
    );
}

fn remove_json_hooks_with_report(agent_label: &str, path: &Path) -> Result<bool> {
    let removed = remove_kizu_hooks_from_json(path)?;
    if removed {
        print_removed(agent_label, path);
    }
    Ok(removed)
}

/// Scrub kizu hook entries from `<home>/.cursor/hooks.json`.
#[cfg(test)]
pub(super) fn teardown_cursor_user_hooks(home: &Path) -> Result<bool> {
    let path = home.join(".cursor").join("hooks.json");
    remove_kizu_hooks_from_json(&path)
}
