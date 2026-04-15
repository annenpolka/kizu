use anyhow::Result;

use crate::git::FileDiff;

pub struct App {
    pub baseline_sha: Option<String>,
    pub files: Vec<FileDiff>,
    pub selected: usize,
    pub follow_mode: bool,
}

impl App {
    pub fn new() -> Self {
        Self {
            baseline_sha: None,
            files: Vec::new(),
            selected: 0,
            follow_mode: true,
        }
    }
}

pub fn run() -> Result<()> {
    let _app = App::new();
    // TODO v0.1:
    //   1. resolve git root from cwd
    //   2. capture baseline HEAD sha
    //   3. spawn watcher::start() with debounce
    //   4. spawn ratatui terminal loop, handle Crossterm events
    //   5. on file event -> git::compute_diff(baseline) -> update App.files
    //   6. follow mode: jump selection to most recently-changed file
    println!("kizu v0.1 skeleton — TUI not yet wired up. See docs/SPEC.md");
    Ok(())
}
