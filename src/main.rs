use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

mod app;
mod attach;
mod config;
mod git;
mod highlight;
mod hook;
mod init;
mod paths;
mod prompt;
mod scar;
mod session;
mod stream;
#[cfg(test)]
mod test_support;
mod ui;
mod watcher;

#[derive(Parser, Debug)]
#[command(
    name = "kizu",
    version,
    about = "Realtime diff monitor + inline scar review TUI for AI coding agents"
)]
struct Cli {
    /// Auto-split the terminal and launch kizu in the new pane.
    #[arg(long)]
    attach: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Initialize agent hooks
    Init {
        /// Comma-separated agent names (e.g. claude-code,cursor)
        #[arg(long, value_delimiter = ',')]
        agent: Option<Vec<String>>,
        /// Install scope: project or user
        #[arg(long)]
        scope: Option<String>,
        /// Skip interactive prompts
        #[arg(long)]
        non_interactive: bool,
    },
    /// Remove all kizu hooks from detected agents
    Teardown,
    /// PostToolUse hook: scan the edited file for @kizu scars
    HookPostTool {
        #[arg(long, default_value = "claude-code")]
        agent: String,
    },
    /// Git pre-commit hook: block commit if staged files contain scars
    HookPreCommit,
    /// PostToolUse hook: async event log writer for stream mode (v0.2)
    HookLogEvent,
    /// Stop hook: block if unresolved @kizu scars remain
    HookStop {
        #[arg(long, default_value = "claude-code")]
        agent: String,
    },
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.attach {
        let config = config::load_config();
        let terminal = attach::resolve_terminal(&config.attach.terminal)?;
        let kizu_bin = std::env::current_exe().context("resolving kizu binary path")?;
        return attach::split_and_launch(terminal, &kizu_bin);
    }

    match cli.command {
        None => app::run().await,
        Some(Command::Init {
            agent,
            scope,
            non_interactive,
        }) => {
            let cwd = std::env::current_dir()?;
            let root = git::find_root(&cwd).unwrap_or(cwd);
            init::run_init(&root, agent.as_deref(), scope.as_deref(), non_interactive)
        }
        Some(Command::Teardown) => {
            let cwd = std::env::current_dir()?;
            let root = git::find_root(&cwd).unwrap_or(cwd);
            init::run_teardown(&root)
        }
        Some(Command::HookPostTool { agent }) => run_hook_post_tool(&agent),
        Some(Command::HookPreCommit) => run_hook_pre_commit(),
        Some(Command::HookLogEvent) => run_hook_log_event(),
        Some(Command::HookStop { agent }) => run_hook_stop(&agent),
    }
}

fn run_hook_post_tool(agent_str: &str) -> Result<()> {
    let agent = hook::AgentKind::from_str(agent_str)
        .ok_or_else(|| anyhow::anyhow!("unknown agent: {agent_str}"))?;
    let input = hook::parse_hook_input(agent, std::io::stdin().lock())?;

    if input.file_paths.is_empty() {
        return Ok(());
    }

    let hits = hook::scan_scars(&input.file_paths);
    if let Some(json) = hook::format_additional_context(agent, &hits) {
        println!("{json}");
    }
    Ok(())
}

fn run_hook_stop(agent_str: &str) -> Result<()> {
    let agent = hook::AgentKind::from_str(agent_str)
        .ok_or_else(|| anyhow::anyhow!("unknown agent: {agent_str}"))?;
    let input = hook::parse_hook_input(agent, std::io::stdin().lock())?;

    if input.stop_hook_active {
        return Ok(());
    }

    let cwd = input
        .cwd
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let root = git::find_root(&cwd)?;
    let changed = hook::enumerate_session_files(&root)?;
    let hits = hook::scan_scars(&changed);

    if !hits.is_empty() {
        eprint!("{}", hook::format_stop_stderr(&hits));
        std::process::exit(2);
    }
    Ok(())
}

fn run_hook_log_event() -> Result<()> {
    let input = hook::parse_hook_input(hook::AgentKind::ClaudeCode, std::io::stdin().lock())?;
    let cwd = input
        .cwd
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let root = git::find_root(&cwd).unwrap_or(cwd);
    let mut event = hook::sanitize_event(&input);
    // Ensure cwd is the git root so per-project events dir resolves correctly.
    event.cwd = root.clone();
    hook::write_event(&event)?;

    // Prune old entries. TTL defaults to 24h, overridable via env var.
    let ttl_secs: u64 = std::env::var("KIZU_EVENT_TTL_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(86400);
    hook::prune_event_log(&root, std::time::Duration::from_secs(ttl_secs), 1000)?;

    Ok(())
}

fn run_hook_pre_commit() -> Result<()> {
    use anyhow::Context;
    use std::process::Command;

    let cwd = std::env::current_dir()?;
    let root = git::find_root(&cwd)?;

    // Get staged files via NUL-delimited output to handle paths
    // with special characters safely.
    let output = Command::new("git")
        .args([
            "diff",
            "--cached",
            "--name-only",
            "-z",
            "--diff-filter=ACMR",
        ])
        .current_dir(&root)
        .output()
        .context("git diff --cached")?;

    if !output.status.success() {
        return Ok(()); // Can't determine staged files; don't block.
    }

    let staged: Vec<std::path::PathBuf> = output
        .stdout
        .split(|&b| b == 0)
        .filter(|r| !r.is_empty())
        .map(|r| root.join(String::from_utf8_lossy(r).as_ref()))
        .collect();

    if staged.is_empty() {
        return Ok(());
    }

    let hits = hook::scan_scars_from_index(&root, &staged);
    if !hits.is_empty() {
        eprintln!("kizu: commit blocked — unresolved scars in staged files:");
        for hit in &hits {
            eprintln!(
                "  {}:{} @kizu[{}]: {}",
                hit.path.display(),
                hit.line_number,
                hit.kind,
                hit.message,
            );
        }
        eprintln!("\nResolve or unstage the scars before committing.");
        std::process::exit(1);
    }
    Ok(())
}
