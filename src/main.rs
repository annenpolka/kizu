use anyhow::Result;
use clap::{Parser, Subcommand};

mod app;
mod git;
mod hook;
mod init;
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
