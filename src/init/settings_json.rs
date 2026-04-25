use anyhow::{Context, Result};
use std::path::Path;

/// Merge kizu hook entries into a Claude Code / Qwen Code style
/// settings.json. Creates the file + parent dirs if missing.
///
/// Claude Code hook schema (as of 2026):
/// ```json
/// {
///   "hooks": {
///     "PostToolUse": [
///       {
///         "matcher": "Edit|Write",
///         "hooks": [
///           { "type": "command", "command": "kizu hook-post-tool ...", "timeout": 10 }
///         ]
///       }
///     ]
///   }
/// }
/// ```
/// A single hook command entry within a matcher group.
pub(in crate::init) struct HookCmd<'a> {
    pub(in crate::init) command: &'a str,
    pub(in crate::init) timeout: Option<u32>,
    pub(in crate::init) is_async: bool,
}

/// Each event holds an array of **matcher groups**, each with a
/// `matcher` string (tool name filter, `""` = match all) and a
/// `hooks` sub-array of command objects.
/// Walk a matcher-group array for one hook event and split any
/// **mixed** group (contains both kizu and user commands) into two
/// sibling groups with the same `matcher`: a user-only group and a
/// kizu-only group. This keeps `remove_kizu_hooks_from_json`'s
/// group-level removal safe — after the split, the kizu group can
/// be dropped wholesale without touching user commands.
///
/// Ordering: the resulting array keeps the original group at its
/// position (now user-only) and inserts the kizu-only group
/// immediately after it, so stable ordering is preserved and the
/// usual "append to kizu-exclusive group" lookup still finds it.
fn split_mixed_kizu_groups(arr: &mut Vec<serde_json::Value>) {
    let mut i = 0;
    while i < arr.len() {
        let Some(group_obj) = arr[i].as_object() else {
            i += 1;
            continue;
        };
        let Some(hooks_arr) = group_obj.get("hooks").and_then(|h| h.as_array()) else {
            i += 1;
            continue;
        };
        let (kizu_cmds, user_cmds): (Vec<_>, Vec<_>) = hooks_arr.iter().cloned().partition(|cmd| {
            cmd.get("command")
                .and_then(|v| v.as_str())
                .and_then(kizu_command_token)
                .is_some()
        });
        if kizu_cmds.is_empty() || user_cmds.is_empty() {
            // All-kizu or all-user — nothing to split.
            i += 1;
            continue;
        }
        // Mixed group. Rebuild into two siblings preserving the
        // matcher string and any other group-level fields.
        let matcher_val = group_obj.get("matcher").cloned();
        let mut user_group = serde_json::Map::new();
        let mut kizu_group = serde_json::Map::new();
        if let Some(m) = matcher_val {
            user_group.insert("matcher".to_string(), m.clone());
            kizu_group.insert("matcher".to_string(), m);
        }
        user_group.insert("hooks".to_string(), serde_json::Value::Array(user_cmds));
        kizu_group.insert("hooks".to_string(), serde_json::Value::Array(kizu_cmds));
        arr[i] = serde_json::Value::Object(user_group);
        arr.insert(i + 1, serde_json::Value::Object(kizu_group));
        // Skip both the user group and its new kizu sibling.
        i += 2;
    }
}

/// Extract the `hook-<name>` token from a kizu hook invocation so we
/// can reconcile by subcommand instead of by full command string.
/// Returns `None` when the command does not look like a kizu hook
/// (e.g. a user's linter), so non-kizu entries are never matched.
pub(in crate::init) fn kizu_command_token(command: &str) -> Option<String> {
    for token in command.split_whitespace() {
        if let Some(rest) = token.strip_prefix("hook-") {
            if rest.is_empty() {
                continue;
            }
            return Some(format!("hook-{rest}"));
        }
    }
    None
}

pub(in crate::init) fn contains_kizu_hook_command(text: &str) -> bool {
    text.contains("kizu hook-")
        || text
            .split_whitespace()
            .any(|token| matches!(token, "hook-post-tool" | "hook-stop" | "hook-log-event"))
}

pub(in crate::init) fn merge_hooks_into_settings(
    path: &Path,
    hooks: &[(&str, &str, &[HookCmd<'_>])], // (event_name, matcher, commands)
) -> Result<(usize, usize)> {
    let mut doc: serde_json::Value = if path.exists() {
        let content =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&content).with_context(|| format!("parsing {}", path.display()))?
    } else {
        serde_json::json!({})
    };

    let hooks_obj = doc
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("settings.json root is not an object"))?
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));

    let hooks_map = hooks_obj
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("hooks is not an object"))?;

    let mut added = 0;
    let mut skipped = 0;

    for (event_name, matcher, commands) in hooks {
        let matcher_groups = hooks_map
            .entry(*event_name)
            .or_insert_with(|| serde_json::json!([]));
        let arr = matcher_groups
            .as_array_mut()
            .ok_or_else(|| anyhow::anyhow!("hooks.{event_name} is not an array"))?;

        // Pre-pass: split any pre-existing **mixed** matcher group
        // (contains both kizu and user commands) into a user-only
        // group plus a kizu-only sibling. `remove_kizu_hooks_from_json`
        // removes any group containing a kizu command wholesale, so
        // as long as a mixed group exists kizu's teardown path will
        // still delete the user's hook. Migrating it here during
        // `kizu init` makes subsequent teardowns safe without
        // requiring the user to touch settings.json manually.
        split_mixed_kizu_groups(arr);

        // Gather all existing commands on this event so we can
        // reconcile per-command instead of per-matcher-group. This is
        // the upgrade path: a config that already contains
        // `hook-post-tool` from an older kizu install must still
        // receive new commands like `hook-log-event`.
        let existing_cmds: Vec<String> = arr
            .iter()
            .flat_map(|group| {
                group
                    .get("hooks")
                    .and_then(|h| h.as_array())
                    .into_iter()
                    .flatten()
            })
            .filter_map(|cmd| cmd.get("command").and_then(|v| v.as_str()))
            .map(|s| s.to_string())
            .collect();

        // Partition the requested commands into "already present" and
        // "missing". `kizu_command_token` extracts the subcommand
        // (`hook-post-tool`, `hook-log-event`, …) so `--agent`
        // differences or binary-path differences do not spawn a
        // duplicate entry.
        let mut missing: Vec<&HookCmd<'_>> = Vec::new();
        for cmd in commands.iter() {
            let want_token = kizu_command_token(cmd.command);
            let is_present = existing_cmds
                .iter()
                .any(|existing| want_token.is_some() && kizu_command_token(existing) == want_token);
            if is_present {
                skipped += 1;
            } else {
                missing.push(cmd);
            }
        }

        if missing.is_empty() {
            continue;
        }

        // Prefer appending to an existing matcher group that is
        // **kizu-exclusive** and shares the same `matcher`, so the
        // upgraded config stays cohesive across reruns. Groups that
        // also contain user-owned commands are intentionally skipped
        // here: `remove_kizu_hooks_from_json` drops any group that
        // holds a kizu command wholesale, so appending into a mixed
        // group would bind the user's hook to kizu's teardown path
        // and erase it on `kizu teardown`. Creating a fresh
        // kizu-exclusive group for the missing commands keeps the
        // user's hook uninvolved in kizu's install/uninstall lifecycle.
        let target_idx = arr.iter().position(|group| {
            let matches_matcher = group
                .get("matcher")
                .and_then(|v| v.as_str())
                .is_some_and(|m| m == *matcher);
            let cmds_opt = group.get("hooks").and_then(|h| h.as_array());
            let Some(cmds) = cmds_opt else {
                return false;
            };
            let has_any_kizu = cmds.iter().any(|cmd| {
                cmd.get("command")
                    .and_then(|v| v.as_str())
                    .and_then(kizu_command_token)
                    .is_some()
            });
            let all_kizu = cmds.iter().all(|cmd| {
                cmd.get("command")
                    .and_then(|v| v.as_str())
                    .and_then(kizu_command_token)
                    .is_some()
            });
            matches_matcher && has_any_kizu && all_kizu
        });

        let cmd_values: Vec<serde_json::Value> = missing
            .iter()
            .map(|cmd| {
                let mut obj = serde_json::json!({
                    "type": "command",
                    "command": cmd.command,
                });
                if let Some(t) = cmd.timeout {
                    obj["timeout"] = serde_json::json!(t);
                }
                if cmd.is_async {
                    obj["async"] = serde_json::json!(true);
                }
                obj
            })
            .collect();

        if let Some(idx) = target_idx {
            let group_hooks = arr[idx]
                .get_mut("hooks")
                .and_then(|h| h.as_array_mut())
                .ok_or_else(|| anyhow::anyhow!("hooks.{event_name}[{idx}].hooks is not array"))?;
            for v in cmd_values {
                group_hooks.push(v);
                added += 1;
            }
        } else {
            arr.push(serde_json::json!({
                "matcher": matcher,
                "hooks": cmd_values
            }));
            added += 1;
        }
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let json_str = serde_json::to_string_pretty(&doc)?;
    std::fs::write(path, json_str).with_context(|| format!("writing {}", path.display()))?;

    Ok((added, skipped))
}

pub(in crate::init) fn remove_kizu_hooks_from_json(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let content = std::fs::read_to_string(path)?;
    let mut doc: serde_json::Value = serde_json::from_str(&content)?;

    let Some(hooks) = doc.get_mut("hooks").and_then(|v| v.as_object_mut()) else {
        return Ok(false);
    };

    let is_kizu_cmd = |cmd: &serde_json::Value| -> bool {
        cmd.get("command")
            .and_then(|v| v.as_str())
            .is_some_and(contains_kizu_hook_command)
    };

    let mut removed = false;
    for (_event, entries) in hooks.iter_mut() {
        if let Some(arr) = entries.as_array_mut() {
            // Pass 1: scrub kizu entries inside every nested matcher
            // group, preserving user commands that shared the group.
            for group in arr.iter_mut() {
                if let Some(nested) = group.get_mut("hooks").and_then(|h| h.as_array_mut()) {
                    let before = nested.len();
                    nested.retain(|cmd| !is_kizu_cmd(cmd));
                    if nested.len() < before {
                        removed = true;
                    }
                }
            }
            // Pass 2: flat old-schema entries and now-empty matcher
            // groups both drop out here. Flat entries have no nested
            // `hooks` array, so they're filtered by the direct
            // `command` check; groups whose `hooks` just emptied out
            // in pass 1 are discarded now.
            let before = arr.len();
            arr.retain(|group| {
                // Flat legacy shape: { "command": "kizu hook-..." }.
                if is_kizu_cmd(group) {
                    return false;
                }
                // Empty matcher group (no hooks or hooks:[]).
                !matches!(
                    group.get("hooks").and_then(|h| h.as_array()),
                    Some(h) if h.is_empty()
                )
            });
            if arr.len() < before {
                removed = true;
            }
        }
    }

    // Clean up empty arrays and empty hooks object.
    hooks.retain(|_, v| v.as_array().is_some_and(|a| !a.is_empty()));

    if removed {
        std::fs::write(path, serde_json::to_string_pretty(&doc)?)?;
    }
    Ok(removed)
}
