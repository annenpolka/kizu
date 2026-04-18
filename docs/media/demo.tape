# kizu demo — recorded with VHS (https://github.com/charmbracelet/vhs).
#
# Regenerate with `just demo` from the repo root. Requires `vhs` and
# `tmux` on PATH and a release build of kizu reachable as `kizu`.
#
# Layout: tmux with a vertical split. Left pane runs kizu against a
# throwaway /tmp/kizu-demo repo. Right pane runs scripts/demo/fake-agent.sh,
# which stands in for a real AI coding agent by writing files at scripted
# intervals and emitting PostToolUse log events to `kizu hook-log-event`
# so that the Stream-mode pane populates too.

Output docs/media/demo.gif

Require kizu
Require tmux

Set Shell "bash"
Set FontSize 14
Set Width 1400
Set Height 780
Set Padding 20
Set TypingSpeed 60ms

# ─── Hidden setup: capture repo path, build a scratch worktree ───
Hide
Type "export REPO=$(pwd)"
Enter
Type "rm -rf /tmp/kizu-demo && mkdir -p /tmp/kizu-demo/src && cd /tmp/kizu-demo"
Enter
Type "git init -q && git config user.email demo@kizu && git config user.name demo"
Enter
Type 'printf "fn main() {}\n" > src/main.rs'
Enter
Type 'printf "pub fn noop() {}\n" > src/util.rs'
Enter
Type 'printf "# demo\n" > README.md'
Enter
Type "git add . && git commit -q -m init"
Enter
Type "clear"
Enter
Show

# ─── Start tmux, split vertically ───
Type "tmux new-session -s kizu-demo"
Enter
Sleep 900ms
Ctrl+B
Type "%"
Sleep 500ms

# ─── Right pane: fake agent ───
Type "bash $REPO/scripts/demo/fake-agent.sh"
Enter
Sleep 400ms

# ─── Focus back to left pane ───
Ctrl+B
Type "o"
Sleep 400ms

# ─── Left pane: kizu ───
Type "kizu"
Enter
Sleep 9s

# Follow mode pins the cursor to the newest change, so we move up to
# reach earlier hunks.
Type "k"
Sleep 450ms
Type "k"
Sleep 450ms
Type "k"
Sleep 450ms

# Insert an ask scar at the buggy line.
Type "a"
Sleep 2000ms

# Move to another line, then open a freeform comment scar with `c`.
Type "k"
Sleep 400ms
Type "k"
Sleep 400ms
Type "c"
Sleep 500ms
Type "revisit this once util lands"
Sleep 1500ms
Enter
Sleep 2500ms

# Jump across files with h (previous) / l (next).
Type "h"
Sleep 1200ms
Type "h"
Sleep 1200ms
Type "l"
Sleep 800ms
Type "l"
Sleep 800ms

# Tab → Stream mode: per-operation history from hook-log-event.
Tab
Sleep 3500ms

# Tab back → Main diff view.
Tab
Sleep 1200ms

# Quit kizu.
Type "q"
Sleep 600ms

# ─── Hidden teardown ───
Hide
Type "tmux kill-session -t kizu-demo 2>/dev/null; rm -rf /tmp/kizu-demo"
Enter
