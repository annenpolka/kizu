use anyhow::Result;
use clap::{Parser, Subcommand};

mod app;
mod git;
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
    /// Initialize Claude Code hooks in .claude/settings.json (v0.2)
    Init,
    /// Remove Claude Code hooks (v0.2)
    Teardown,
    /// PostToolUse hook entry: synchronous single-file scar grep (v0.2)
    HookPostTool,
    /// PostToolUse hook entry: async event log writer for stream mode (v0.2)
    HookLogEvent,
    /// Stop hook entry: detect outstanding @review: scars (v0.2)
    HookStop,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        None => app::run().await,
        Some(Command::Init) => unimplemented!("v0.2: kizu init"),
        Some(Command::Teardown) => unimplemented!("v0.2: kizu teardown"),
        Some(Command::HookPostTool) => unimplemented!("v0.2: kizu hook-post-tool"),
        Some(Command::HookLogEvent) => unimplemented!("v0.2: kizu hook-log-event"),
        Some(Command::HookStop) => unimplemented!("v0.2: kizu hook-stop"),
    }
}
