use anyhow::Result;
use clap::{Parser, Subcommand};

mod app;
mod git;
mod hook;
mod scar;
mod ui;
mod watcher;

#[derive(Parser, Debug)]
#[command(
    name = "kizu",
    version,
    about = "Realtime diff monitor + inline scar review TUI for AI coding agents"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Initialize agent hooks (v0.2)
    Init,
    /// Remove agent hooks (v0.2)
    Teardown,
    /// PostToolUse hook: scan the edited file for @kizu scars
    HookPostTool {
        #[arg(long, default_value = "claude-code")]
        agent: String,
    },
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

    match cli.command {
        None => app::run().await,
        Some(Command::Init) => unimplemented!("v0.2: kizu init"),
        Some(Command::Teardown) => unimplemented!("v0.2: kizu teardown"),
        Some(Command::HookPostTool { agent }) => run_hook_post_tool(&agent),
        Some(Command::HookLogEvent) => unimplemented!("v0.2: kizu hook-log-event"),
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
    if let Some(json) = hook::format_additional_context(&hits) {
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
    let changed = hook::enumerate_changed_files(&root)?;
    let hits = hook::scan_scars(&changed);

    if !hits.is_empty() {
        eprint!("{}", hook::format_stop_stderr(&hits));
        std::process::exit(2);
    }
    Ok(())
}
