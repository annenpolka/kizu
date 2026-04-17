# kizu

Realtime diff monitor + inline scar review TUI for AI coding agents (Claude Code, Cursor, Codex, Qwen Code, Cline, Gemini).

> **Status: alpha (v0.3).** TUI, scar review, multi-agent hook integration + init/teardown, stream mode (Tab-toggle operation history), `--attach` terminal auto-split, `~/.config/kizu/config.toml`, scar undo stack, and the Claude Code plugin are all implemented. See [`docs/SPEC.md`](docs/SPEC.md) for the full specification.

## What it does

While Claude Code (or another terminal AI coding agent) edits files in another pane, kizu sits next to it and shows you what changed in real time. When something looks wrong, you press one key and a `@kizu[ask|reject|free]:` comment is written into the source file at the change site. Claude Code picks it up on the next read, or on the next `Stop` hook firing — whichever comes first — and fixes it without you having to type a sentence.

The design solves three frictions of "watching Claude Code stream output out of the corner of your eye":

1. **You miss the detail.** Streaming output flies by; the moment you think "wait, what?" the line is already gone.
2. **Articulating the problem is annoying.** You feel something is wrong but you can't put it into words quickly enough.
3. **Even when you do articulate it, the agent fixes the wrong thing.** Vague human language → vague agent interpretation.

kizu's answer is **the precision of pointing**. Capture every change, let the human point with one keystroke, and the language problem disappears.

## Phases

- **v0.1 (MVP)** — fsnotify + git diff + ratatui scroll TUI. Pure observer. No scar, no hooks.
- **v0.2** — `a`/`r`/`c`/`x`/`e`/`Space` scar keybindings, `/` search, Enter file-view zoom, `kizu init/teardown`, PostToolUse + Stop + pre-commit hooks, multi-agent support (6 agents).
- **v0.3** — Stream mode (Tab-toggle operation history driven by `hook-log-event`), `--attach` for tmux / zellij / kitty / Ghostty, `~/.config/kizu/config.toml` for keybindings / colors / debounce / terminal split, scar undo stack (`u`), adaptive `j`/`k` navigation with run magnet, Claude Code plugin (`plugin/`). _← current_

## Stack

- Rust 2024 edition
- [ratatui](https://ratatui.rs/) + [crossterm](https://docs.rs/crossterm/) for the TUI
- [notify](https://docs.rs/notify/) + [notify-debouncer-full](https://docs.rs/notify-debouncer-full/) for filesystem watching
- [syntect](https://docs.rs/syntect/) for syntax highlighting
- [clap](https://docs.rs/clap/) for CLI parsing
- `git` CLI shelled out for diff computation (`git diff --no-renames <baseline> --`)

## Build

Local workflow is driven by [`just`](https://github.com/casey/just); see `justfile` for all recipes.

```bash
just            # default: run the full CI gate (fmt-check → clippy → test → release → e2e)
just rust       # fast loop: fmt + clippy + cargo test (skip e2e)
just run        # cargo run --release against the current worktree
```

Raw cargo commands (equivalent to the relevant `just` recipes) also work:

```bash
cargo build --release
cargo test --all-targets
cargo clippy -- -D warnings
```

Requires Rust 1.94+ (edition 2024). The release profile uses `lto = "thin"`, `codegen-units = 1`, and `strip = true` for binary size.

## Documentation

The [`docs/`](docs/) directory carries the implementation context an LLM/coding agent needs to make progress without re-deriving the design:

- [`docs/SPEC.md`](docs/SPEC.md) — the canonical specification (v0.1 → v0.3, architecture, fork from `Mechachang/raw/raw--spec-kizu.md`)
- [`docs/claude-code-hooks.md`](docs/claude-code-hooks.md) — PostToolUse / Stop hook input schema, three feedback paths, infinite-loop hazard, environment variables
- [`docs/inline-scar-pattern.md`](docs/inline-scar-pattern.md) — the file-write + Stop-hook async review pattern (kizu's core mechanism)
- [`docs/deep-research-ai-agent-hooks.md`](docs/deep-research-ai-agent-hooks.md) — survey of the hook mechanisms across 10 AI coding agents (v0.2 integration map)
- [`docs/related-tools.md`](docs/related-tools.md) — diffpane / diffwatch / revdiff / watchexec+delta / hwatch / Claude Code Hooks pipeline survey
- [`docs/adr/`](docs/adr/) — Architecture Decision Records documenting the *why* behind non-reversible design choices (git CLI shell-out, notify-debouncer-full, tuistory e2e, stream mode per-operation diff, …)

## License

MIT. See [`LICENSE`](LICENSE).
