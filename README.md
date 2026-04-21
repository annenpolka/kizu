# kizu

[![Crates.io](https://img.shields.io/crates/v/kizu.svg)](https://crates.io/crates/kizu)
[![License: MIT](https://img.shields.io/crates/l/kizu.svg)](LICENSE)
[![CI](https://github.com/annenpolka/kizu/actions/workflows/ci.yml/badge.svg)](https://github.com/annenpolka/kizu/actions/workflows/ci.yml)

[English](README.md) | [日本語](README.ja.md)

> Realtime diff monitor + inline scar review TUI for AI coding agents — Claude Code, Cursor, Codex, Qwen Code, Cline, Gemini.

![kizu demo](docs/media/demo.gif)

## What it does

While a terminal AI coding agent (Claude Code, Cursor, …) edits files in one pane, kizu sits in another and shows you what changed in real time. When something looks wrong, you press one key and a `@kizu[ask|reject|free]:` comment is written into the source file at the change site. The agent picks it up on its next `PostToolUse` fire — or, failing that, when its `Stop` hook is forced to keep working — and fixes it without you having to type a sentence.

kizu is designed around three frictions of watching an agent stream output out of the corner of your eye:

1. **You miss the detail.** Streaming output flies by; the moment you think "wait, what?" the line is already gone.
2. **Articulating the problem is annoying.** You feel something is wrong but you can't put it into words quickly enough.
3. **Even when you do articulate it, the agent fixes the wrong thing.** Vague human prose becomes vague agent interpretation.

kizu's answer is **the precision of pointing**: capture every change, let the human point with one keystroke, and the language problem disappears. The name _kizu_ (傷) is Japanese for "wound" or "scar" — every questionable change leaves a small, visible mark in the source that has to be healed before the agent can move on.

## Install

### From crates.io

```bash
cargo install kizu
```

### From source

```bash
git clone https://github.com/annenpolka/kizu
cd kizu
cargo install --path .
```

### Requirements

- Rust 1.94+ (edition 2024)
- `git` CLI on `PATH`
- macOS or Linux. Windows is untested. `--attach` for Ghostty is macOS-only (AppleScript); tmux / zellij / kitty splits work on both macOS and Linux.

## Quickstart

Start by running kizu as a passive observer — no hooks, no scars, just a diff pane that follows your working tree.

```bash
cd path/to/your/repo
kizu
```

Let your agent edit files in another pane; kizu redraws on every change. Press `q` to quit.

That alone solves the first friction: you stop losing the "wait, what?" moments. Once that feels useful, wire up the scar workflow so you can _react_ to a change in one keystroke (see [AI agent integration](#ai-agent-integration) below).

## Usage

### Three views

kizu is a single TUI with three modes. `Tab` toggles between Main and Stream; `Enter` zooms into File. Press `?` at any time to open a help overlay with the full keybind list. The _session baseline_ referenced below is the `HEAD` SHA at the moment you launched kizu (or the last `R` press), so "what changed" always means "what's changed since you started reviewing."

| Key | View | What you see |
|-----|------|--------------|
| _(default)_ | **Main diff** | Per-file hunks based on `git diff <session-baseline>`. Added lines on dark green, deleted on dark red. Hunks are merged the way git sees them. |
| `Tab` | **Stream mode** | Per-operation history of what the agent actually did — one entry per file-edit tool call (Claude Code / Qwen: `Write` / `Edit` / `MultiEdit`; Cursor: `afterFileEdit`). Backed by the `hook-log-event` JSON log, not by git. |
| `Enter` | **File view** | The whole file under the cursor, with added lines highlighted inline. Useful when hunk context isn't enough. `Enter` / `Esc` closes. |

### Scars

A _scar_ is an inline comment kizu writes directly into your source file, using the target language's comment syntax. Each scar carries a kind tag (`ask` / `reject` / `free`) and a one-line body:

```rust
// @kizu[ask]: explain this change
```

```python
# @kizu[reject]: revert this change
```

```html
<!-- @kizu[free]: elaborate on the edge case here -->
```

Three kinds, each bound to a single key:

- **`ask`** (`a`) — a question. Inserts the canned body `explain this change`; no prompt, no typing. The agent is expected to answer inline and resume.
- **`reject`** (`r`) — a veto. Inserts the canned body `revert this change`. The agent is expected to undo the edit.
- **`free`** (`c`) — freeform. Opens an input field where you type the body yourself; anything goes.

`a` and `r` are deliberately type-free: the whole point of pointing is that you don't have to compose a sentence. If you need nuance, reach for `c`. Scars are idempotent — pressing `a` twice on the same line is a no-op. `u` undoes the last scar you wrote in this session.

The feedback path back to the agent has two channels:

- **PostToolUse hook** fires after each edit. If the just-edited file contains a scar, the hook surfaces it to the agent as `additionalContext` so the agent sees it on its very next step.
- **Stop hook** fires when the agent tries to finish its turn. If any unresolved scar remains in the repo, the hook exits non-zero and the agent is forced to keep working.

### More

- **`--attach`** — run `kizu --attach` from inside an agent pane to auto-split the terminal (tmux / zellij / kitty / Ghostty) and launch kizu in the new pane. Detection falls back to `$TMUX` → `$ZELLIJ` → `$KITTY_LISTEN_ON` → `$TERM_PROGRAM=ghostty`; override with `[attach].terminal` in the config.
- **Seen / fold (`Space`)** — marks the current hunk as reviewed and collapses its body to just the header. If the hunk's content changes later, kizu fingerprints the previous body and auto-expands it so you never miss a follow-up edit. Purely TUI-local — nothing is written to the file.
- **Search (`/` `n` `N`)** — smart-case search across the current view, with matches highlighted in the body. `n` / `N` jump between matches with wrap-around and show a position indicator.
- **Line numbers (`#`)** — toggles a worktree-side line-number gutter in the Main and File views. Stream mode suppresses line numbers because its synthetic per-operation diffs don't correspond to real file line numbers. Default state and key are configurable (`[line_numbers].enabled`, `[keys].line_numbers_toggle`).
- **Scar undo (`u`)** — a session-local stack that reverses just the most recent scar write, matching text-editor undo ergonomics.
- **Baseline reset (`R`)** — rebinds the diff baseline to the current `HEAD`. Useful after you commit mid-session and want kizu to forget the already-reviewed changes.
- **Follow (`f`)** — toggles whether kizu auto-scrolls to the newest change vs. keeps the cursor pinned.
- **Help overlay (`?`)** — opens a two-column keybind reference. `?` / `Esc` / `q` closes it. The footer itself is responsive — on narrow terminals it collapses to status only, so the help overlay is the canonical key reference.

## Keybinds

Press `?` inside kizu for the live two-column reference; the tables below mirror it.

### Scar
| Key | Action |
|-----|--------|
| `a` | Insert `@kizu[ask]:` scar above the current line |
| `r` | Insert `@kizu[reject]:` scar |
| `c` | Open comment input and insert a freeform `@kizu[free]:` scar |
| `x` | Revert the current hunk (`git checkout -- <file>` at hunk scope) |
| `e` | Open `$EDITOR` at the current hunk |
| `Space` | Mark the current hunk as seen — collapses the body to the header; auto-expands if the hunk changes later |
| `u` | Undo the most recent scar insertion |

### Navigation
| Key | Action |
|-----|--------|
| `j` / `↓` | Next line (adaptive: snaps through runs of unchanged lines) |
| `k` / `↑` | Previous line |
| `J` | Down one line (fine-grained) |
| `K` | Up one line |
| `Ctrl-d` | Half page down |
| `Ctrl-u` | Half page up |
| `g` | Top of diff |
| `G` | Bottom of diff |
| `h` | Previous file |
| `l` | Next file |
| `s` | Open file picker |

### Search
| Key | Action |
|-----|--------|
| `/` | Open search input (smart-case) |
| `n` | Next match (wraps) |
| `N` | Previous match |

### View
| Key | Action |
|-----|--------|
| `Tab` | Toggle Main diff ↔ Stream mode |
| `Enter` | Open File view for the current file |
| `Esc` | Close File view / cancel input |
| `w` | Toggle line wrap |
| `z` | Toggle cursor placement style |
| `f` | Toggle follow (auto-scroll) |
| `#` | Toggle line-number gutter (Main + File view) |
| `?` | Open help overlay |

### Session
| Key | Action |
|-----|--------|
| `R` | Reset diff baseline to current `HEAD` |
| `q` / `Ctrl-C` | Quit |

## Configuration

kizu reads `~/.config/kizu/config.toml` (override with `$KIZU_CONFIG`). Every field is optional — a partial TOML merges cleanly with the defaults below.

```toml
# ~/.config/kizu/config.toml — all defaults shown

[keys]
ask                 = "a"
reject              = "r"
comment             = "c"
revert              = "x"
editor              = "e"
seen                = " "
follow              = "f"
search              = "/"
search_next         = "n"
search_prev         = "N"
picker              = "s"
reset_baseline      = "R"
cursor_placement    = "z"
wrap_toggle         = "w"
undo                = "u"
line_numbers_toggle = "#"

[colors]
bg_added   = [10, 50, 10]    # dark green, delta-style
bg_deleted = [60, 10, 10]    # dark red

[timing]
debounce_worktree_ms = 300    # worktree file changes
debounce_git_dir_ms  = 100    # HEAD / refs / packed-refs

[editor]
command = ""                  # empty = use $EDITOR

[attach]
terminal = ""                 # empty = auto-detect; "tmux" | "zellij" | "kitty" | "ghostty"

[line_numbers]
enabled = false               # start with the gutter off; `#` toggles at runtime
```

Non-character keys (`Enter`, `Tab`, arrows, `Ctrl-*`) are not remappable.

## AI agent integration

kizu wires itself into an agent's hook system with one command:

```bash
kizu init
```

The interactive flow detects installed agents, asks which scope to install into (`project-local`, `project-shared`, or `user`), and writes the hook config into the right settings file. Non-interactive usage:

```bash
kizu init --agent claude-code --scope project-local --non-interactive
```

`kizu teardown` removes everything kizu installed, across all agents and scopes.

### Supported agents

Each agent's host gives kizu a different amount of surface area, so the hook set `kizu init` actually installs varies. "Full" means both a PostToolUse-equivalent and a Stop-equivalent — that is, scars show up mid-turn _and_ block the agent from ending the turn with unresolved review requests.

| Agent identifier | Support level | What `kizu init` installs |
|------------------|---------------|---------------------------|
| `claude-code` | Full | PostToolUse (scar notify + async event log) + Stop gate, with `session_id` binding |
| `cursor` | Full | `afterFileEdit` (scar notify + event log) + `stop` gate in `.cursor/hooks.json`, `session_id` binding |
| `qwen` | Full | PostToolUse (scar notify + event log) + Stop gate |
| `codex` | Stop only | Stop gate only — Codex's PreTool / PostTool events fire for Bash tools alone, so there's nothing to hook on file edits |
| `cline` | PostToolUse only (best-effort) | File-based `.clinerules/hooks/PostToolUse`; no Stop gate, so unresolved scars _cannot_ block task completion |
| `gemini` | Write-side only | No host-side install — Gemini CLI exposes no hook mechanism yet. You still get the diff pane and can write scars; pipe-based stream integration is planned |

Across every agent, `kizu init` also installs one repo-wide git `pre-commit` shim (see below), regardless of which agent(s) you picked.

See [`docs/deep-research-ai-agent-hooks.md`](docs/deep-research-ai-agent-hooks.md) for the full per-agent survey and [`docs/claude-code-hooks.md`](docs/claude-code-hooks.md) for the Claude Code hook schema and infinite-loop pitfalls.

### Hook roles

`kizu init` wires up three distinct concerns. Which of the first two actually land depends on the agent's hook surface (see the table above); the third is a git hook and is installed once per repo.

- **PostToolUse** (per-agent, where supported) → `kizu hook-post-tool` (per-file scar notification) + `kizu hook-log-event` (async event log that feeds Stream mode).
- **Stop** (per-agent, where supported) → `kizu hook-stop` scans tracked + untracked files; any unresolved `@kizu[...]` blocks the agent from finishing. For agents without a Stop hook (Cline, Gemini), scar resolution is best-effort — the git `pre-commit` is the only remaining safety net.
- **Git `pre-commit`** (repo-wide, installed once) → `kizu hook-pre-commit` blocks `git commit` when staged files still contain scars, so reviews can't escape into a commit by accident. The shim is kizu-managed: if `.git/hooks/pre-commit` already exists, it's renamed to `pre-commit.user` and chained from the new shim.

## Stack

- Rust 2024 edition
- [ratatui](https://ratatui.rs/) + [crossterm](https://docs.rs/crossterm/) for the TUI
- [notify](https://docs.rs/notify/) + [notify-debouncer-full](https://docs.rs/notify-debouncer-full/) for filesystem watching
- `git` CLI shelled out for diff computation (`git diff --no-renames <baseline> --`) — see [ADR-0001](docs/adr/0001-git-cli-shell-out.md)

## Development

Local workflow is driven by [`just`](https://github.com/casey/just); see [`justfile`](justfile) for all recipes.

```bash
just            # default: full CI gate (fmt-check → clippy → test → release → e2e)
just rust       # fast loop: fmt + clippy + cargo test (skip e2e)
just e2e        # release build + tuistory e2e (bun test)
just run        # cargo run --release against the current worktree
```

Raw cargo commands work too:

```bash
cargo build --release
cargo test --all-targets
cargo clippy -- -D warnings
```

Architecture, design decisions, and the canonical specification:

- [`docs/SPEC.md`](docs/SPEC.md) — full specification: architecture, TUI and hook-layer schemas
- [`docs/adr/`](docs/adr/) — Architecture Decision Records for non-reversible design choices (git CLI shell-out, notify-debouncer-full, tuistory e2e, stream mode, …)
- [`docs/inline-scar-pattern.md`](docs/inline-scar-pattern.md) — the file-write + Stop-hook async review pattern (kizu's core mechanism)
- [`docs/related-tools.md`](docs/related-tools.md) — survey of diffpane / diffwatch / revdiff / watchexec+delta / hwatch / Claude Code Hooks pipelines

Issues and PRs welcome.

## License

MIT. See [`LICENSE`](LICENSE).
