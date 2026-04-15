# docs/

Implementation context for kizu. These documents are the source material an LLM agent (or a returning human) needs in order to make progress on the codebase without re-deriving the design.

| File | Role |
|:---|:---|
| [`SPEC.md`](SPEC.md) | **Canonical specification.** v0.1 → v0.3, architecture, two data sources (filesystem state vs. PostToolUse event log), scar feature, hook design, CLI surface, terminal integration. Source of truth — when this file disagrees with code, the code is wrong. |
| [`claude-code-hooks.md`](claude-code-hooks.md) | PostToolUse / Stop hook reference. Input JSON schema, three feedback paths (exit 0 stdout / exit 2 stderr / additionalContext JSON), `stop_hook_active` infinite-loop hazard, `$CLAUDE_TOOL_INPUT_FILE_PATH` environment variables, settings.json layout. Needed for v0.2 hook subcommands. |
| [`inline-scar-pattern.md`](inline-scar-pattern.md) | The file-write + Stop-hook async review pattern. Why same-process Esc + send-keys was rejected, why two-layer hook structure, language→comment-syntax mapping, derivative variants (revert+scar, ghost prompt queue, tap). The core mechanism kizu implements. |
| [`related-tools.md`](related-tools.md) | Survey of existing tools in the same category — diffpane (the closest reference, Go/bubbletea), diffwatch ×2, revdiff, watchexec+delta, hwatch, Claude Code Hooks pipelines, Ghostty/tmux integration. Use this to understand what kizu is *not* and where it borrows from. |

## When implementing a feature

1. Read `SPEC.md` for the contract.
2. If the feature touches a hook, read `claude-code-hooks.md` for the constraint set.
3. If the feature is the scar mechanism, read `inline-scar-pattern.md`.
4. If you're tempted to invent something the spec doesn't ask for, skim `related-tools.md` first — it probably already exists in another tool, and the right move is either to depend on it or to deliberately differentiate.

## Provenance

The wiki versions of `claude-code-hooks.md`, `inline-scar-pattern.md`, and `related-tools.md` live in `Mechachang/wiki/` (the author's LLM Wiki vault). `SPEC.md` mirrors `Mechachang/raw/raw--spec-kizu.md` verbatim. If you update the spec inside this repo, mirror the change back to the wiki so the design history stays coherent.
