#!/usr/bin/env bash
# Stand-in for a real AI coding agent while recording the kizu demo GIF.
# Makes a sequence of edits across multiple files, emitting
# PostToolUse-shaped events to `kizu hook-log-event` so that kizu's
# Stream mode populates as if a real agent were driving.
#
# Intended entry point: docs/media/demo.tape (run via `just demo`).

set -euo pipefail

root="$(pwd)"
main="src/main.rs"
util="src/util.rs"
readme="README.md"

log_event() {
    local tool="$1" rel="$2"
    # Claude Code's PostToolUse JSON shape. kizu hook-log-event
    # sanitizes this and writes it under <state_dir>/events/<hash>/.
    printf '{"tool_name":"%s","tool_input":{"file_path":"%s/%s"},"tool_response":"ok","cwd":"%s"}\n' \
        "$tool" "$root" "$rel" "$root" \
        | kizu hook-log-event >/dev/null 2>&1 || true
}

echo "[fake-agent] starting"
sleep 1

echo "[fake-agent] edit 1: greeting in main.rs"
cat > "$main" <<'EOF'
fn main() {
    let greeting = "hello";
    println!("{}", greeting);
}
EOF
log_event Edit "$main"
sleep 2

echo "[fake-agent] edit 2: helper in util.rs"
cat > "$util" <<'EOF'
pub fn shout(s: &str) -> String {
    format!("{}!!", s.to_uppercase())
}
EOF
log_event Edit "$util"
sleep 2

echo "[fake-agent] edit 3: wire util into main.rs (with a bug)"
cat > "$main" <<'EOF'
mod util;

fn main() {
    let greeting = "hello";
    let count = 1 / 0;
    println!("{} x{}", util::shout(greeting), count);
}
EOF
log_event Edit "$main"
sleep 2

echo "[fake-agent] edit 4: document usage in README"
cat > "$readme" <<'EOF'
# demo

`cargo run` prints a loud greeting.
EOF
log_event Write "$readme"

echo "[fake-agent] done — lingering for the Stream pane"
sleep 60
