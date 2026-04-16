---
description: Show kizu session status and manage the realtime diff monitor
allowed-tools: Bash, Read
---

# /kizu — Session Status & Management

Check the current kizu session status, including baseline SHA, active scars, and stream event count.

## Usage

Run `kizu` subcommands to inspect session state:

```bash
# Check if a kizu TUI session is active for this project
if [ -d "${XDG_STATE_HOME:-$HOME/Library/Application Support}/kizu/sessions" ]; then
  ls -la "${XDG_STATE_HOME:-$HOME/Library/Application Support}/kizu/sessions/"
fi

# Count pending scars in the working tree
git diff --name-only HEAD -- | xargs grep -l '@kizu\[' 2>/dev/null || echo "No pending scars"

# Count stream events
EVENT_DIR="${XDG_STATE_HOME:-$HOME/Library/Application Support}/kizu/events"
if [ -d "$EVENT_DIR" ]; then
  echo "Stream events: $(ls "$EVENT_DIR" | wc -l | tr -d ' ')"
else
  echo "No stream events directory"
fi
```

Report the results to the user in a concise summary.
