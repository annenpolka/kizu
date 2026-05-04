use super::*;
use crate::test_support::ENV_LOCK;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[test]
fn parse_hook_input_extracts_claude_code_post_tool_use() {
    let json = r#"{
        "session_id": "abc123",
        "hook_event_name": "PostToolUse",
        "tool_name": "Write",
        "tool_input": { "file_path": "/tmp/foo.rs", "content": "fn main() {}" },
        "tool_response": "ok",
        "cwd": "/home/user/project",
        "stop_hook_active": false
    }"#;
    let input = parse_hook_input(AgentKind::ClaudeCode, json.as_bytes()).unwrap();
    assert_eq!(input.session_id.as_deref(), Some("abc123"));
    assert_eq!(input.hook_event_name, "PostToolUse");
    assert_eq!(input.tool_name.as_deref(), Some("Write"));
    assert_eq!(input.file_paths, vec![PathBuf::from("/tmp/foo.rs")]);
    assert_eq!(input.cwd.as_deref(), Some(Path::new("/home/user/project")));
    assert!(!input.stop_hook_active);
}

#[test]
fn parse_hook_input_extracts_cline_path_field() {
    let json = r#"{
        "hook_event_name": "PostToolUse",
        "tool_name": "Write",
        "tool_input": { "path": "/tmp/cline.py", "content": "print(1)" },
        "cwd": "/home/user"
    }"#;
    let input = parse_hook_input(AgentKind::Cline, json.as_bytes()).unwrap();
    assert_eq!(input.file_paths, vec![PathBuf::from("/tmp/cline.py")]);
}

#[test]
fn parse_hook_input_extracts_cursor_file_path_field() {
    let json = r#"{
        "hook_event_name": "PostToolUse",
        "tool_name": "Edit",
        "tool_input": { "filePath": "/tmp/cursor.ts", "content": "const x = 1" },
        "cwd": "/home/user"
    }"#;
    let input = parse_hook_input(AgentKind::Cursor, json.as_bytes()).unwrap();
    assert_eq!(input.file_paths, vec![PathBuf::from("/tmp/cursor.ts")]);
}

#[test]
fn parse_hook_input_extracts_cursor_multi_file_edits_array() {
    let json = r#"{
        "hook_event_name": "PostToolUse",
        "tool_name": "MultiEdit",
        "tool_input": {
            "edits": [
                { "filePath": "/tmp/a.ts", "content": "a" },
                { "filePath": "/tmp/b.ts", "content": "b" }
            ]
        },
        "cwd": "/home/user"
    }"#;
    let input = parse_hook_input(AgentKind::Cursor, json.as_bytes()).unwrap();
    assert_eq!(
        input.file_paths,
        vec![PathBuf::from("/tmp/a.ts"), PathBuf::from("/tmp/b.ts")]
    );
}

#[test]
fn parse_hook_input_deduplicates_paths_from_scalar_and_edits() {
    let json = r#"{
        "hook_event_name": "PostToolUse",
        "tool_name": "Edit",
        "tool_input": {
            "filePath": "/tmp/a.ts",
            "edits": [
                { "filePath": "/tmp/a.ts", "content": "dup" }
            ]
        },
        "cwd": "/home/user"
    }"#;
    let input = parse_hook_input(AgentKind::Cursor, json.as_bytes()).unwrap();
    assert_eq!(input.file_paths, vec![PathBuf::from("/tmp/a.ts")]);
}

#[test]
fn parse_hook_input_extracts_stop_event() {
    let json = r#"{
        "hook_event_name": "Stop",
        "stop_hook_active": true,
        "cwd": "/tmp"
    }"#;
    let input = parse_hook_input(AgentKind::ClaudeCode, json.as_bytes()).unwrap();
    assert_eq!(input.hook_event_name, "Stop");
    assert!(input.stop_hook_active);
    assert!(input.file_paths.is_empty());
}

#[test]
fn scan_scars_finds_ask_reject_and_free() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("a.rs");
    fs::write(
        &file,
        "fn main() {}\n// @kizu[ask]: explain this change\n// @kizu[reject]: revert this change\n// @kizu[free]: custom note\n",
    ).unwrap();

    let hits = scan_scars(std::slice::from_ref(&file));

    assert_eq!(hits.len(), 3);
    assert_eq!(hits[0].kind, "ask");
    assert_eq!(hits[0].message, "explain this change");
    assert_eq!(hits[0].line_number, 2);
    assert_eq!(hits[1].kind, "reject");
    assert_eq!(hits[2].kind, "free");
    assert_eq!(hits[2].message, "custom note");
}

#[test]
fn scan_scars_handles_python_comment_syntax() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("x.py");
    fs::write(&file, "def f():\n# @kizu[ask]: why?\n    pass\n").unwrap();

    let hits = scan_scars(&[file]);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].kind, "ask");
    assert_eq!(hits[0].message, "why?");
}

#[test]
fn scan_scars_skips_missing_files_without_panic() {
    let hits = scan_scars(&[PathBuf::from("/nonexistent/ghost.rs")]);
    assert!(hits.is_empty());
}

#[test]
fn scan_scars_returns_empty_when_no_scars_present() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("clean.rs");
    fs::write(&file, "fn main() {}\n").unwrap();

    let hits = scan_scars(&[file]);
    assert!(hits.is_empty());
}

#[test]
fn scan_scars_skips_fenced_code_blocks_in_markdown() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("spec.md");
    fs::write(
        &file,
        "# Spec\n\nExample:\n\n```html\n<!-- @kizu[ask]: example in fence -->\n```\n\nEnd.\n",
    )
    .unwrap();

    let hits = scan_scars(&[file]);
    assert!(hits.is_empty(), "fenced code block scar should be ignored");
}

#[test]
fn scan_scars_detects_real_scar_outside_fence_in_markdown() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("notes.md");
    fs::write(
        &file,
        "# Notes\n\n<!-- @kizu[ask]: real scar outside fence -->\n\n```\n<!-- @kizu[ask]: example -->\n```\n",
    )
    .unwrap();

    let hits = scan_scars(&[file]);
    assert_eq!(hits.len(), 1, "only the scar outside the fence");
    assert_eq!(hits[0].message, "real scar outside fence");
    assert_eq!(hits[0].line_number, 3);
}

#[test]
fn scan_scars_finds_jsx_block_comment() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("Counter.tsx");
    fs::write(
        &file,
        "export function Counter() {\n  return (\n    <section>\n      {/* @kizu[ask]: explain this change */}\n      <p>Count</p>\n    </section>\n  );\n}\n",
    )
    .unwrap();

    let hits = scan_scars(std::slice::from_ref(&file));

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].kind, "ask");
    assert_eq!(hits[0].message, "explain this change");
    assert_eq!(hits[0].line_number, 4);
}

#[test]
fn scan_scars_does_not_skip_fences_in_non_markdown_files() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("a.rs");
    // Rust files should not get fence-aware treatment even if
    // they happen to contain triple backticks in a string.
    fs::write(
        &file,
        "let s = \"```\";\n// @kizu[ask]: real scar\nlet t = \"```\";\n",
    )
    .unwrap();

    let hits = scan_scars(&[file]);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].kind, "ask");
}

#[test]
fn format_additional_context_claude_code_envelope() {
    let hits = vec![ScarHit {
        path: PathBuf::from("src/foo.rs"),
        line_number: 10,
        kind: "ask".into(),
        message: "explain this".into(),
    }];
    let json_str = format_additional_context(AgentKind::ClaudeCode, &hits).expect("non-empty");
    let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
    let ctx = parsed["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(ctx.contains("src/foo.rs:10"));
    assert!(ctx.contains("@kizu[ask]"));
}

#[test]
fn format_additional_context_cursor_envelope() {
    let hits = vec![ScarHit {
        path: PathBuf::from("src/foo.rs"),
        line_number: 5,
        kind: "reject".into(),
        message: "revert".into(),
    }];
    let json_str = format_additional_context(AgentKind::Cursor, &hits).expect("non-empty");
    let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
    // Cursor uses flat "additional_context" key, not hookSpecificOutput.
    let ctx = parsed["additional_context"].as_str().unwrap();
    assert!(ctx.contains("src/foo.rs:5"));
    assert!(ctx.contains("@kizu[reject]"));
    assert!(parsed.get("hookSpecificOutput").is_none());
}

#[test]
fn format_additional_context_codex_envelope() {
    let hits = vec![ScarHit {
        path: PathBuf::from("lib.py"),
        line_number: 3,
        kind: "free".into(),
        message: "note".into(),
    }];
    let json_str = format_additional_context(AgentKind::Codex, &hits).expect("non-empty");
    let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
    // Codex uses flat "additionalContext" key.
    let ctx = parsed["additionalContext"].as_str().unwrap();
    assert!(ctx.contains("lib.py:3"));
    assert!(ctx.contains("@kizu[free]"));
    assert!(parsed.get("hookSpecificOutput").is_none());
}

#[test]
fn format_additional_context_qwen_uses_claude_code_envelope() {
    let hits = vec![ScarHit {
        path: PathBuf::from("a.rs"),
        line_number: 1,
        kind: "ask".into(),
        message: "why".into(),
    }];
    let json_str = format_additional_context(AgentKind::QwenCode, &hits).expect("non-empty");
    let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
    assert!(
        parsed["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .is_some()
    );
}

#[test]
fn format_additional_context_cline_uses_claude_code_envelope() {
    let hits = vec![ScarHit {
        path: PathBuf::from("a.rs"),
        line_number: 1,
        kind: "ask".into(),
        message: "why".into(),
    }];
    let json_str = format_additional_context(AgentKind::Cline, &hits).expect("non-empty");
    let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
    assert!(
        parsed["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .is_some()
    );
}

#[test]
fn format_additional_context_returns_none_when_no_hits() {
    assert!(format_additional_context(AgentKind::ClaudeCode, &[]).is_none());
}

#[test]
fn format_stop_stderr_lists_all_hits() {
    let hits = vec![
        ScarHit {
            path: PathBuf::from("a.rs"),
            line_number: 1,
            kind: "ask".into(),
            message: "why".into(),
        },
        ScarHit {
            path: PathBuf::from("b.py"),
            line_number: 5,
            kind: "reject".into(),
            message: "revert".into(),
        },
    ];
    let stderr = format_stop_stderr(&hits);
    assert!(stderr.contains("a.rs:1"));
    assert!(stderr.contains("b.py:5"));
    assert!(stderr.contains("Unresolved kizu scars"));
}

#[test]
fn agent_kind_from_str_matches_common_names() {
    assert_eq!(
        "claude-code".parse::<AgentKind>(),
        Ok(AgentKind::ClaudeCode)
    );
    assert_eq!("claude".parse::<AgentKind>(), Ok(AgentKind::ClaudeCode));
    assert_eq!("cursor".parse::<AgentKind>(), Ok(AgentKind::Cursor));
    assert_eq!("codex".parse::<AgentKind>(), Ok(AgentKind::Codex));
    assert_eq!("qwen".parse::<AgentKind>(), Ok(AgentKind::QwenCode));
    assert_eq!("cline".parse::<AgentKind>(), Ok(AgentKind::Cline));
    assert_eq!("unknown".parse::<AgentKind>(), Err(()));
}

#[test]
fn sanitize_event_strips_content_and_adds_timestamp() {
    let input = NormalizedHookInput {
        session_id: Some("sess-1".to_string()),
        hook_event_name: "PostToolUse".to_string(),
        tool_name: Some("Edit".to_string()),
        file_paths: vec![PathBuf::from("/tmp/foo.rs")],
        cwd: Some(PathBuf::from("/tmp/project")),
        stop_hook_active: false,
    };
    let event = sanitize_event(&input);
    assert_eq!(event.session_id.as_deref(), Some("sess-1"));
    assert_eq!(event.hook_event_name, "PostToolUse");
    assert_eq!(event.tool_name.as_deref(), Some("Edit"));
    assert_eq!(event.file_paths, vec![PathBuf::from("/tmp/foo.rs")]);
    assert_eq!(event.cwd, PathBuf::from("/tmp/project"));
    assert!(event.timestamp_ms > 0);
}

#[test]
fn sanitize_event_serialized_json_has_no_content_fields() {
    let input = NormalizedHookInput {
        session_id: Some("sess-2".to_string()),
        hook_event_name: "PostToolUse".to_string(),
        tool_name: Some("Write".to_string()),
        file_paths: vec![PathBuf::from("/tmp/bar.rs")],
        cwd: Some(PathBuf::from("/tmp")),
        stop_hook_active: false,
    };
    let event = sanitize_event(&input);
    let json = serde_json::to_string(&event).unwrap();
    // Verify no content/response/prompt fields leak through.
    assert!(!json.contains("\"content\""));
    assert!(!json.contains("\"new_string\""));
    assert!(!json.contains("\"old_string\""));
    assert!(!json.contains("\"output\""));
    assert!(!json.contains("\"prompt\""));
    // Verify expected fields are present.
    assert!(json.contains("\"session_id\""));
    assert!(json.contains("\"tool_name\""));
    assert!(json.contains("\"file_paths\""));
    assert!(json.contains("\"timestamp_ms\""));
}

#[test]
fn write_event_creates_file_with_correct_content() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("KIZU_STATE_DIR", tmp.path().to_str().unwrap()) };

    let event = SanitizedEvent {
        session_id: Some("test-sess".to_string()),
        hook_event_name: "PostToolUse".to_string(),
        tool_name: Some("Edit".to_string()),
        file_paths: vec![PathBuf::from("/tmp/foo.rs")],
        cwd: PathBuf::from("/tmp"),
        timestamp_ms: 1700000000000,
    };
    let path = write_event(&event).unwrap();

    unsafe { std::env::remove_var("KIZU_STATE_DIR") };

    assert!(path.exists());
    let name = path.file_name().unwrap().to_string_lossy().to_string();
    assert!(
        name.starts_with("1700000000000-Edit-"),
        "filename must begin with `<ms>-<tool>-` and carry a uniqueness suffix, got {name}"
    );
    assert!(
        name.ends_with(".json"),
        "filename must end with `.json`, got {name}"
    );

    let content = std::fs::read_to_string(&path).unwrap();
    let parsed: SanitizedEvent = serde_json::from_str(&content).unwrap();
    assert_eq!(parsed, event);
}

#[test]
fn write_event_same_millisecond_produces_distinct_files() {
    // Two hook invocations in the same millisecond with the same
    // tool name must NOT overwrite each other. The earlier
    // implementation used `<ms>-<tool>.json` with a predictable
    // `.{filename}.tmp` scratch path, so two concurrent writes
    // raced on both the temp and destination names — one event
    // silently vanished, poisoning later per-operation diffs
    // because the next event for the same file diffed against
    // the wrong prior snapshot.
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("KIZU_STATE_DIR", tmp.path().to_str().unwrap()) };

    let event_a = SanitizedEvent {
        session_id: None,
        hook_event_name: "PostToolUse".to_string(),
        tool_name: Some("Edit".to_string()),
        file_paths: vec![PathBuf::from("/tmp/a.rs")],
        cwd: PathBuf::from("/tmp"),
        timestamp_ms: 1_800_000_000_000,
    };
    let event_b = SanitizedEvent {
        session_id: None,
        hook_event_name: "PostToolUse".to_string(),
        tool_name: Some("Edit".to_string()),
        file_paths: vec![PathBuf::from("/tmp/b.rs")],
        cwd: PathBuf::from("/tmp"),
        timestamp_ms: 1_800_000_000_000, // same ms
    };
    let path_a = write_event(&event_a).unwrap();
    let path_b = write_event(&event_b).unwrap();

    unsafe { std::env::remove_var("KIZU_STATE_DIR") };

    assert_ne!(
        path_a, path_b,
        "same-millisecond writes must land in distinct files, got {path_a:?} vs {path_b:?}"
    );
    assert!(path_a.exists(), "first event file missing: {path_a:?}");
    assert!(path_b.exists(), "second event file missing: {path_b:?}");
}

#[test]
fn write_event_sets_0600_permissions() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("KIZU_STATE_DIR", tmp.path().to_str().unwrap()) };

    let event = SanitizedEvent {
        session_id: None,
        hook_event_name: "PostToolUse".to_string(),
        tool_name: Some("Write".to_string()),
        file_paths: vec![],
        cwd: PathBuf::from("/tmp"),
        timestamp_ms: 1700000000001,
    };
    let path = write_event(&event).unwrap();

    unsafe { std::env::remove_var("KIZU_STATE_DIR") };

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}

#[test]
fn prune_removes_old_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let events_dir = tmp.path().join("events");
    std::fs::create_dir_all(&events_dir).unwrap();

    // Write an "old" event (timestamp 1000, effectively ancient).
    let old_file = events_dir.join("1000-Edit.json");
    std::fs::write(&old_file, "{}").unwrap();

    // Write a "recent" event.
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let new_file = events_dir.join(format!("{now_ms}-Write.json"));
    std::fs::write(&new_file, "{}").unwrap();

    let removed = prune_event_log_in(&events_dir, Duration::from_secs(3600), 1000).unwrap();

    assert_eq!(removed, 1);
    assert!(!old_file.exists());
    assert!(new_file.exists());
}

#[test]
fn prune_enforces_max_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let events_dir = tmp.path().join("events");
    std::fs::create_dir_all(&events_dir).unwrap();

    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    // Create 5 recent events.
    for i in 0..5 {
        let file = events_dir.join(format!("{}-Edit.json", now_ms + i));
        std::fs::write(&file, "{}").unwrap();
    }

    // Prune with max_entries = 3.
    let removed = prune_event_log_in(&events_dir, Duration::from_secs(86400), 3).unwrap();

    assert_eq!(removed, 2);
    let remaining: Vec<_> = std::fs::read_dir(&events_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(remaining.len(), 3);
}

#[test]
fn prune_returns_zero_when_events_dir_missing() {
    let removed = prune_event_log_in(
        Path::new("/nonexistent/path"),
        Duration::from_secs(3600),
        1000,
    )
    .unwrap();
    assert_eq!(removed, 0);
}

#[test]
fn scan_scars_from_index_reads_staged_blob_not_worktree() {
    use std::process::Command;

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // Set up a git repo.
    Command::new("git")
        .args(["init"])
        .current_dir(root)
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(root)
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(root)
        .output()
        .unwrap();

    let file = root.join("a.rs");

    // Write file with a scar and stage it.
    fs::write(&file, "fn main() {}\n// @kizu[ask]: staged scar\n").unwrap();
    Command::new("git")
        .args(["add", "a.rs"])
        .current_dir(root)
        .output()
        .unwrap();

    // Now remove the scar from the worktree (but leave it staged).
    fs::write(&file, "fn main() {}\n").unwrap();

    // scan_scars (worktree) should find nothing.
    let worktree_hits = scan_scars(std::slice::from_ref(&file));
    assert!(worktree_hits.is_empty(), "worktree should be clean");

    // scan_scars_from_index should still find the staged scar.
    let index_hits = scan_scars_from_index(root, &[file]);
    assert_eq!(index_hits.len(), 1);
    assert_eq!(index_hits[0].kind, "ask");
    assert_eq!(index_hits[0].message, "staged scar");
}

#[test]
fn scan_scars_from_index_finds_jsx_block_comment() {
    use std::process::Command;

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    Command::new("git")
        .args(["init"])
        .current_dir(root)
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(root)
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(root)
        .output()
        .unwrap();

    let file = root.join("Counter.tsx");
    fs::write(
        &file,
        "export function Counter() {\n  return (\n    <section>\n      {/* @kizu[ask]: staged jsx scar */}\n      <p>Count</p>\n    </section>\n  );\n}\n",
    )
    .unwrap();
    Command::new("git")
        .args(["add", "Counter.tsx"])
        .current_dir(root)
        .output()
        .unwrap();

    let index_hits = scan_scars_from_index(root, &[file]);

    assert_eq!(index_hits.len(), 1);
    assert_eq!(index_hits[0].kind, "ask");
    assert_eq!(index_hits[0].message, "staged jsx scar");
}
