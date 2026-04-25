use super::*;
use std::fs;

#[test]
fn merge_hooks_creates_settings_with_matcher_group_schema() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join(".claude").join("settings.json");

    let (added, skipped) = merge_hooks_into_settings(
        &path,
        &[
            (
                "PostToolUse",
                "Edit|Write",
                &[
                    HookCmd {
                        command: "kizu hook-post-tool --agent claude-code",
                        timeout: Some(10),
                        is_async: false,
                    },
                    HookCmd {
                        command: "kizu hook-log-event",
                        timeout: None,
                        is_async: true,
                    },
                ],
            ),
            (
                "Stop",
                "",
                &[HookCmd {
                    command: "kizu hook-stop --agent claude-code",
                    timeout: Some(10),
                    is_async: false,
                }],
            ),
        ],
    )
    .unwrap();

    assert_eq!(added, 2);
    assert_eq!(skipped, 0);
    let doc: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let post = &doc["hooks"]["PostToolUse"].as_array().unwrap()[0];
    assert_eq!(post["matcher"].as_str().unwrap(), "Edit|Write");
    let cmds = post["hooks"].as_array().unwrap();
    assert_eq!(cmds.len(), 2);
    assert_eq!(cmds[0]["type"].as_str().unwrap(), "command");
    assert!(
        cmds[0]["command"]
            .as_str()
            .unwrap()
            .contains("kizu hook-post-tool")
    );
    assert!(cmds[0].get("async").is_none());
    assert_eq!(cmds[1]["async"].as_bool(), Some(true));
    assert!(
        cmds[1]["command"]
            .as_str()
            .unwrap()
            .contains("hook-log-event")
    );
}

#[test]
fn merge_hooks_skips_duplicate_kizu_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("settings.json");
    // Pre-existing kizu hook in new matcher-group schema.
    fs::write(
            &path,
            r#"{"hooks":{"PostToolUse":[{"matcher":"Edit|Write","hooks":[{"type":"command","command":"kizu hook-post-tool --agent claude-code","timeout":10}]}]}}"#,
        )
        .unwrap();

    let (added, skipped) = merge_hooks_into_settings(
        &path,
        &[(
            "PostToolUse",
            "Edit|Write",
            &[HookCmd {
                command: "kizu hook-post-tool --agent claude-code",
                timeout: Some(10),
                is_async: false,
            }],
        )],
    )
    .unwrap();

    assert_eq!(added, 0);
    assert_eq!(skipped, 1);
}

#[test]
fn merge_hooks_adds_missing_commands_to_existing_kizu_group() {
    // Upgrade path: a user installed kizu from main (which only had
    // `hook-post-tool`), then re-runs `kizu init` on v0.3. The new
    // async `hook-log-event` must be appended even though a kizu
    // command is already present — otherwise stream mode stays
    // inert after the upgrade.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("settings.json");
    fs::write(
            &path,
            r#"{"hooks":{"PostToolUse":[{"matcher":"Edit|Write|MultiEdit","hooks":[{"type":"command","command":"kizu hook-post-tool --agent claude-code","timeout":10}]}]}}"#,
        )
        .unwrap();

    merge_hooks_into_settings(
        &path,
        &[(
            "PostToolUse",
            "Edit|Write|MultiEdit",
            &[
                HookCmd {
                    command: "kizu hook-post-tool --agent claude-code",
                    timeout: Some(10),
                    is_async: false,
                },
                HookCmd {
                    command: "kizu hook-log-event",
                    timeout: None,
                    is_async: true,
                },
            ],
        )],
    )
    .unwrap();

    let doc: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let post = doc["hooks"]["PostToolUse"].as_array().unwrap();
    let cmds: Vec<&str> = post
        .iter()
        .flat_map(|g| g["hooks"].as_array().into_iter().flatten())
        .filter_map(|c| c["command"].as_str())
        .collect();
    assert!(
        cmds.iter().any(|c| c.contains("hook-post-tool")),
        "pre-existing hook-post-tool must remain: {cmds:?}"
    );
    assert!(
        cmds.iter().any(|c| c.contains("hook-log-event")),
        "missing hook-log-event must be appended on rerun: {cmds:?}"
    );
    // The duplicate `hook-post-tool` must not be added twice.
    let post_tool_count = cmds.iter().filter(|c| c.contains("hook-post-tool")).count();
    assert_eq!(post_tool_count, 1, "duplicate must be suppressed");
}

#[test]
fn teardown_only_preserves_user_hooks_in_legacy_mixed_group() {
    // Rollback path: a user upgraded from an older kizu that
    // did not split mixed groups. They then run `kizu teardown`
    // *without* first running `kizu init`, so the migration
    // pre-pass never touches the file. `remove_kizu_hooks_from_json`
    // must still scrub only kizu commands and leave the user's
    // linter intact — dropping the whole group would silently
    // delete user config.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("settings.json");
    fs::write(
        &path,
        r#"{"hooks":{"PostToolUse":[{"matcher":"Edit|Write","hooks":[
                {"type":"command","command":"kizu hook-post-tool --agent claude-code","timeout":10},
                {"type":"command","command":"my-user-linter","timeout":5}
            ]}]}}"#,
    )
    .unwrap();

    let removed = remove_kizu_hooks_from_json(&path).unwrap();
    assert!(removed, "teardown must report that something was removed");

    let doc: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let arr = doc
        .get("hooks")
        .and_then(|h| h.get("PostToolUse"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let all_cmds: Vec<String> = arr
        .iter()
        .flat_map(|g| g["hooks"].as_array().cloned().unwrap_or_default())
        .filter_map(|c| c["command"].as_str().map(String::from))
        .collect();
    assert!(
        all_cmds.iter().any(|c| c.contains("my-user-linter")),
        "user linter must survive direct teardown of a legacy mixed group, remaining: {all_cmds:?}"
    );
    assert!(
        !all_cmds.iter().any(|c| c.contains("kizu hook-")),
        "no kizu command must remain after teardown, remaining: {all_cmds:?}"
    );
}

#[test]
fn init_then_teardown_preserves_user_hook_in_pre_existing_mixed_group() {
    // The realistic upgrade path: a user's settings.json already
    // has a mixed matcher group `[kizu hook-post-tool,
    // my-user-linter]` from an older install plus a manual
    // addition. `kizu init` must migrate that mixed group into
    // a kizu-only group (carrying the pre-existing kizu command
    // with it) and a user-only group, so that later
    // `remove_kizu_hooks_from_json` removes only the kizu-only
    // group and leaves `my-user-linter` alone.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("settings.json");
    fs::write(
        &path,
        r#"{"hooks":{"PostToolUse":[{"matcher":"Edit|Write|MultiEdit","hooks":[
                {"type":"command","command":"kizu hook-post-tool --agent claude-code","timeout":10},
                {"type":"command","command":"my-user-linter","timeout":5}
            ]}]}}"#,
    )
    .unwrap();

    // Simulate a `kizu init` rerun with the v0.3 hook set.
    merge_hooks_into_settings(
        &path,
        &[(
            "PostToolUse",
            "Edit|Write|MultiEdit",
            &[
                HookCmd {
                    command: "kizu hook-post-tool --agent claude-code",
                    timeout: Some(10),
                    is_async: false,
                },
                HookCmd {
                    command: "kizu hook-log-event",
                    timeout: None,
                    is_async: true,
                },
            ],
        )],
    )
    .unwrap();

    // Now run teardown.
    remove_kizu_hooks_from_json(&path).unwrap();

    let doc: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let arr = doc
        .get("hooks")
        .and_then(|h| h.get("PostToolUse"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let remaining_cmds: Vec<String> = arr
        .iter()
        .flat_map(|g| g["hooks"].as_array().cloned().unwrap_or_default())
        .filter_map(|c| c["command"].as_str().map(String::from))
        .collect();

    assert!(
        remaining_cmds.iter().any(|c| c.contains("my-user-linter")),
        "user linter must survive `init` → `teardown`, remaining: {remaining_cmds:?}"
    );
    assert!(
        !remaining_cmds.iter().any(|c| c.contains("kizu hook-")),
        "no kizu command must remain after teardown, remaining: {remaining_cmds:?}"
    );
}

#[test]
fn merge_hooks_does_not_append_into_mixed_user_and_kizu_group() {
    // If a user has added their own hook to a matcher group that
    // also contains a kizu command, a rerun of `kizu init` must
    // NOT append new kizu commands into that mixed group — doing
    // so lets `teardown` later erase the user's hook because
    // `remove_kizu_hooks_from_json` drops any group containing a
    // kizu command. Instead, create a new kizu-exclusive group.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("settings.json");
    fs::write(
        &path,
        r#"{"hooks":{"PostToolUse":[{"matcher":"Edit|Write|MultiEdit","hooks":[
                {"type":"command","command":"kizu hook-post-tool --agent claude-code","timeout":10},
                {"type":"command","command":"my-user-linter","timeout":5}
            ]}]}}"#,
    )
    .unwrap();

    merge_hooks_into_settings(
        &path,
        &[(
            "PostToolUse",
            "Edit|Write|MultiEdit",
            &[
                HookCmd {
                    command: "kizu hook-post-tool --agent claude-code",
                    timeout: Some(10),
                    is_async: false,
                },
                HookCmd {
                    command: "kizu hook-log-event",
                    timeout: None,
                    is_async: true,
                },
            ],
        )],
    )
    .unwrap();

    let doc: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let arr = doc["hooks"]["PostToolUse"].as_array().unwrap();

    // The original mixed group must remain untouched: it still
    // contains exactly the original kizu hook-post-tool AND the
    // user's linter, and it did NOT grow a new kizu command.
    let mixed = &arr[0];
    let mixed_cmds: Vec<&str> = mixed["hooks"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["command"].as_str())
        .collect();
    assert!(
        mixed_cmds.iter().any(|c| c.contains("my-user-linter")),
        "mixed group must keep the user linter, got {mixed_cmds:?}"
    );
    assert!(
        !mixed_cmds.iter().any(|c| c.contains("hook-log-event")),
        "new kizu command must NOT be appended into a mixed group: {mixed_cmds:?}"
    );

    // The missing kizu command must still be installed — it
    // lives in a fresh kizu-exclusive group, so teardown can
    // remove it without touching the user's linter.
    let all_cmds: Vec<&str> = arr
        .iter()
        .flat_map(|g| g["hooks"].as_array().into_iter().flatten())
        .filter_map(|c| c["command"].as_str())
        .collect();
    assert!(
        all_cmds.iter().any(|c| c.contains("hook-log-event")),
        "hook-log-event must still be installed somewhere, got {all_cmds:?}"
    );
}

#[test]
fn merge_hooks_preserves_existing_non_kizu_matcher_groups() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("settings.json");
    fs::write(
            &path,
            r#"{"hooks":{"PostToolUse":[{"matcher":"","hooks":[{"type":"command","command":"my-linter","timeout":5}]}]}}"#,
        )
        .unwrap();

    merge_hooks_into_settings(
        &path,
        &[(
            "PostToolUse",
            "Edit|Write",
            &[HookCmd {
                command: "kizu hook-post-tool --agent claude-code",
                timeout: Some(10),
                is_async: false,
            }],
        )],
    )
    .unwrap();

    let doc: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let arr = doc["hooks"]["PostToolUse"].as_array().unwrap();
    assert_eq!(arr.len(), 2, "existing matcher group must be preserved");
    assert!(
        arr[0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("my-linter")
    );
}

#[test]
fn remove_kizu_hooks_strips_nested_kizu_matcher_groups() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("settings.json");
    fs::write(
            &path,
            r#"{"hooks":{"PostToolUse":[{"matcher":"","hooks":[{"type":"command","command":"my-linter"}]},{"matcher":"Edit|Write","hooks":[{"type":"command","command":"kizu hook-post-tool --agent claude-code"}]}],"Stop":[{"matcher":"","hooks":[{"type":"command","command":"kizu hook-stop --agent claude-code"}]}]}}"#,
        )
        .unwrap();

    let removed = remove_kizu_hooks_from_json(&path).unwrap();
    assert!(removed);

    let doc: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let post = doc["hooks"]["PostToolUse"].as_array().unwrap();
    assert_eq!(post.len(), 1);
    assert!(
        post[0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("my-linter")
    );
    // Stop array was entirely kizu → key removed.
    assert!(doc["hooks"].get("Stop").is_none());
}

#[test]
fn remove_kizu_hooks_returns_false_when_no_kizu_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("settings.json");
    fs::write(
            &path,
            r#"{"hooks":{"PostToolUse":[{"matcher":"","hooks":[{"type":"command","command":"my-linter"}]}]}}"#,
        )
        .unwrap();

    let removed = remove_kizu_hooks_from_json(&path).unwrap();
    assert!(!removed);
}

#[test]
fn remove_kizu_hooks_returns_false_for_missing_file() {
    let removed = remove_kizu_hooks_from_json(Path::new("/nonexistent/settings.json")).unwrap();
    assert!(!removed);
}

#[test]
fn kizu_hook_command_quotes_path_for_local_and_user_scopes() {
    // Agent hook backends run `command` through a shell. If the
    // kizu binary lives at `/Users/John Doe/.cargo/bin/kizu`,
    // emitting the raw path into `format!("{bin} hook-...")`
    // yields a command where `sh` word-splits on the space and
    // tries to exec the wrong argv[0]. Generated commands must
    // therefore shell-quote the path for project-local / user
    // scopes; project-shared keeps the bare `kizu` token since
    // the binary is expected on PATH.
    let with_space = "/Users/John Doe/.cargo/bin/kizu";
    let local = super::kizu_hook_command_with_bin(
        super::Scope::ProjectLocal,
        with_space,
        "hook-post-tool --agent claude-code",
    );
    assert!(
        local.starts_with(r"'/Users/John Doe/.cargo/bin/kizu'"),
        "project-local path with space must be single-quoted, got {local}"
    );
    assert!(
        local.ends_with(" hook-post-tool --agent claude-code"),
        "subcommand must follow the quoted path unchanged, got {local}"
    );

    // A single quote inside the path must get the `'\''` escape.
    let with_quote = "/home/ev'an/kizu";
    let user = super::kizu_hook_command_with_bin(super::Scope::User, with_quote, "hook-log-event");
    assert!(
        user.starts_with(r"'/home/ev'\''an/kizu'"),
        "embedded single quote must use `'\\''` escape, got {user}"
    );

    // Project-shared stays bare — this path is committed and is
    // expected to resolve via PATH on every contributor's box.
    let shared = super::kizu_hook_command_with_bin(
        super::Scope::ProjectShared,
        "kizu",
        "hook-stop --agent claude-code",
    );
    assert_eq!(shared, "kizu hook-stop --agent claude-code");
}

#[test]
fn shell_single_quote_wraps_and_escapes_embedded_quotes() {
    // Plain path: wrapped only.
    assert_eq!(
        super::shell_single_quote("/usr/bin/kizu"),
        "'/usr/bin/kizu'"
    );
    // Path with a space: still one literal token after the shim parses it.
    assert_eq!(
        super::shell_single_quote("/Users/John Doe/kizu"),
        "'/Users/John Doe/kizu'"
    );
    // Path containing a single quote gets the standard '\'' escape.
    assert_eq!(
        super::shell_single_quote("/home/ev'an/kizu"),
        r"'/home/ev'\''an/kizu'"
    );
}

#[test]
fn pre_commit_shim_body_quotes_bin_with_spaces() {
    let shim = super::pre_commit_shim_body("/Users/John Doe/kizu", false);
    // The shim must contain the quoted form so `/bin/sh` does
    // not wordsplit at the space.
    assert!(
        shim.contains("'/Users/John Doe/kizu' hook-pre-commit"),
        "shim body should quote the binary path; got:\n{shim}"
    );
    // And must NOT contain the unquoted form that would break.
    assert!(
        !shim.contains("/Users/John Doe/kizu hook-pre-commit"),
        "shim body must not embed the unquoted path; got:\n{shim}"
    );
}

#[test]
fn pre_commit_shim_body_with_user_hook_still_quotes_bin() {
    let shim = super::pre_commit_shim_body("/p with space/kizu", true);
    assert!(shim.contains("'/p with space/kizu' hook-pre-commit"));
    assert!(shim.contains("pre-commit.user"));
}

#[test]
fn install_cursor_writes_hook_log_event_for_stream_mode() {
    // Cursor is advertised as `SupportLevel::Full`, which implies
    // stream mode works — stream mode only works when the
    // `hook-log-event` hook fires on every edit. Without it,
    // `afterFileEdit` only runs `hook-post-tool` (scar scan)
    // and no event file is ever written, leaving the Stream
    // view permanently empty for Cursor sessions. Install must
    // wire `hook-log-event` alongside the existing scar hook.
    let tmp = tempfile::tempdir().unwrap();
    let report = super::install_cursor(super::Scope::ProjectLocal, tmp.path()).unwrap();
    assert!(
        report.entries_added > 0,
        "fresh install must add at least one entry"
    );
    let path = tmp.path().join(".cursor").join("hooks.json");
    let doc: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let after_edit = doc["hooks"]["afterFileEdit"]
        .as_array()
        .expect("afterFileEdit must be an array");
    let commands: Vec<&str> = after_edit
        .iter()
        .filter_map(|e| e["command"].as_str())
        .collect();
    assert!(
        commands.iter().any(|c| c.contains("hook-log-event")),
        "afterFileEdit must install hook-log-event for stream mode, got {commands:?}"
    );
    assert!(
        commands.iter().any(|c| c.contains("hook-post-tool")),
        "afterFileEdit must also keep the scar scan hook, got {commands:?}"
    );
}

#[test]
fn teardown_removes_cursor_user_scope_hooks_json() {
    // `install_cursor` writes to `~/.cursor/hooks.json` for
    // `Scope::User`, but the earlier teardown path only scrubbed
    // `<project>/.cursor/hooks.json`. A user who installed Cursor
    // hooks globally was told teardown found nothing while the
    // global afterFileEdit/stop hooks kept firing in every later
    // Cursor session. Teardown must remove the user-scope file
    // too, using the same path install wrote to.
    let tmp = tempfile::tempdir().unwrap();
    let fake_home = tmp.path();
    let cursor_dir = fake_home.join(".cursor");
    fs::create_dir_all(&cursor_dir).unwrap();
    let hooks_path = cursor_dir.join("hooks.json");
    fs::write(
            &hooks_path,
            r#"{"version":1,"hooks":{"afterFileEdit":[{"command":"kizu hook-post-tool --agent cursor","timeout":10}],"stop":[{"command":"kizu hook-stop --agent cursor","timeout":10}]}}"#,
        )
        .unwrap();

    let removed =
        super::teardown_cursor_user_hooks(fake_home).expect("user-scope teardown must succeed");
    assert!(
        removed,
        "teardown must report removal of the user-scope cursor hooks file"
    );

    // After removal, the file no longer carries kizu entries.
    let doc: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&hooks_path).unwrap()).unwrap();
    let all_cmds: Vec<String> = doc["hooks"]
        .as_object()
        .into_iter()
        .flat_map(|m| m.values())
        .flat_map(|v| v.as_array().cloned().unwrap_or_default())
        .filter_map(|c| c["command"].as_str().map(String::from))
        .collect();
    assert!(
        !all_cmds.iter().any(|c| c.contains("kizu hook-")),
        "no kizu command must remain in user-scope Cursor hooks, got {all_cmds:?}"
    );
}

#[test]
fn teardown_removes_codex_project_scoped_hooks_json() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // Simulate a Codex project-scoped install: <repo>/.codex/hooks.json
    let codex_dir = root.join(".codex");
    fs::create_dir_all(&codex_dir).unwrap();
    let hooks_path = codex_dir.join("hooks.json");
    fs::write(
            &hooks_path,
            r#"{"hooks":{"Stop":[{"matcher":"","hooks":[{"type":"command","command":"kizu hook-stop --agent codex","timeout":10}]}]}}"#,
        )
        .unwrap();

    // Verify removal works via the same function teardown uses.
    let removed = remove_kizu_hooks_from_json(&hooks_path).unwrap();
    assert!(removed, "should remove kizu hooks from .codex/hooks.json");

    // After removal the hooks object should be empty.
    let doc: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&hooks_path).unwrap()).unwrap();
    let hooks = doc["hooks"].as_object().unwrap();
    assert!(hooks.is_empty(), "all kizu entries should be gone");
}

/// The interactive agent picker pads two columns: agent name to 12
/// cells, support-level pill to 18 cells. Both paddings must be
/// done via `pad_visible` (not `{:<N}`) so ANSI escapes don't
/// inflate the count. Cf. ADR-0019.
#[test]
fn agent_label_columns_are_visually_aligned() {
    use crate::prompt::visible_width;

    let detected = AgentKind::all()
        .iter()
        .map(|&kind| DetectedAgent {
            kind,
            binary_found: matches!(kind, AgentKind::ClaudeCode),
            config_dir_found: matches!(kind, AgentKind::ClaudeCode),
            recommended: matches!(kind, AgentKind::ClaudeCode),
        })
        .collect::<Vec<_>>();

    // Build the labels exactly as `select_agents_interactive` would.
    let labels: Vec<String> = detected
        .iter()
        .map(|d| {
            let sl = support_level(d.kind);
            format!(
                "{}  {}  {}",
                pad_visible(&c_bold(&d.kind.to_string()), 12),
                pad_visible(&support_level_colored(sl), 18),
                detection_status_colored(d),
            )
        })
        .collect();

    // For each label, the prefix up to where the **third** column
    // begins must land at exactly 12 + 2 + 18 + 2 = 34 cells.
    let third_col_start_cells = 12 + 2 + 18 + 2;
    for (d, label) in detected.iter().zip(labels.iter()) {
        let status = detection_status_colored(d);
        let total = visible_width(label);
        let status_w = visible_width(&status);
        assert_eq!(
            total.checked_sub(status_w),
            Some(third_col_start_cells),
            "misaligned row for {:?}: total={} status_w={} label={:?}",
            d.kind,
            total,
            status_w,
            label,
        );
    }
}
