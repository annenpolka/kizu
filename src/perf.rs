use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::Result;
use ratatui::{Terminal, backend::TestBackend};

use crate::app::{
    App, CursorPlacement, DiffSnapshots, ScrollLayout, StreamEvent, ViewMode, WatcherHealth,
};
use crate::git::{DiffContent, DiffLine, FileDiff, FileStatus, Hunk, LineKind};
use crate::hook::{AgentKind, NormalizedHookInput};

pub fn many_hunk_app(hunks: usize, lines_per_hunk: usize, line_width: usize) -> App {
    app_with_files(vec![many_hunk_file(
        "src/generated.rs",
        hunks,
        lines_per_hunk,
        line_width,
        100,
    )])
}

pub fn many_hunk_file(
    name: &str,
    hunks: usize,
    lines_per_hunk: usize,
    line_width: usize,
    secs: u64,
) -> FileDiff {
    let hunks = (0..hunks)
        .map(|idx| {
            let old_start = idx * 10 + 1;
            let lines = (0..lines_per_hunk)
                .map(|line_idx| DiffLine {
                    kind: LineKind::Added,
                    content: bench_line(idx, line_idx, line_width),
                    has_trailing_newline: true,
                })
                .collect::<Vec<_>>();
            Hunk {
                old_start,
                old_count: 0,
                new_start: old_start,
                new_count: lines_per_hunk,
                lines,
                context: Some(format!("fn generated_{idx}()")),
            }
        })
        .collect::<Vec<_>>();
    make_file(name, hunks, secs)
}

pub fn rebuild_layout(app: &mut App) {
    app.build_layout();
}

pub fn large_unified_diff(
    files: usize,
    hunks_per_file: usize,
    lines_per_hunk: usize,
    line_width: usize,
) -> String {
    let mut raw = String::new();
    for file_idx in 0..files {
        let path = format!("src/generated_{file_idx}.rs");
        raw.push_str(&format!(
            "diff --git a/{path} b/{path}\nindex 1111111..2222222 100644\n--- a/{path}\n+++ b/{path}\n"
        ));
        for hunk_idx in 0..hunks_per_file {
            let old_start = hunk_idx * 10 + 1;
            raw.push_str(&format!(
                "@@ -{old_start},0 +{old_start},{lines_per_hunk} @@ fn generated_{hunk_idx}()\n"
            ));
            for line_idx in 0..lines_per_hunk {
                raw.push('+');
                raw.push_str(&bench_line(hunk_idx, line_idx, line_width));
                raw.push('\n');
            }
        }
    }
    raw
}

pub fn parse_unified_diff_for_bench(raw: &str) -> Vec<FileDiff> {
    crate::git::parse_unified_diff(raw).expect("synthetic unified diff must parse")
}

pub fn render_frame_for_bench(app: &App, width: u16, height: u16) {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test backend terminal");
    terminal
        .draw(|frame| crate::ui::render(frame, app))
        .expect("render perf frame");
}

pub fn operation_diff_for_bench(previous: &str, current: &str) -> String {
    crate::stream::compute_operation_diff(previous, current)
}

pub fn build_stream_files_for_bench(events: &[StreamEvent]) -> Vec<FileDiff> {
    crate::stream::build_stream_files(events)
}

pub fn hook_payload_json(paths: usize) -> String {
    let edits = (0..paths)
        .map(|idx| format!(r#"{{"file_path":"src/file_{idx}.rs"}}"#))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"{{
  "session_id":"bench-session",
  "hook_event_name":"PostToolUse",
  "tool_name":"MultiEdit",
  "cwd":"/tmp/kizu-perf",
  "tool_input":{{"edits":[{edits}]}}
}}"#
    )
}

pub fn parse_hook_payload_for_bench(payload: &str) -> NormalizedHookInput {
    crate::hook::parse_hook_input(AgentKind::ClaudeCode, payload.as_bytes())
        .expect("synthetic hook payload must parse")
}

pub fn sanitize_hook_event_for_bench(input: &NormalizedHookInput) -> crate::hook::SanitizedEvent {
    crate::hook::sanitize_event(input)
}

pub fn write_scar_target(dir: &Path, lines: usize) -> Result<PathBuf> {
    let path = dir.join("scar_target.rs");
    let content = (0..lines)
        .map(|idx| format!("let value_{idx} = {idx};\n"))
        .collect::<String>();
    std::fs::write(&path, content)?;
    Ok(path)
}

pub fn scan_scar_files_for_bench(paths: &[PathBuf]) -> Vec<crate::hook::ScarHit> {
    crate::hook::scan_scars(paths)
}

pub fn highlight_lines_for_bench(
    highlighter: &crate::highlight::Highlighter,
    path: &Path,
    lines: &[String],
) -> usize {
    lines
        .iter()
        .map(|line| highlighter.highlight_line(line, path).len())
        .sum()
}

pub fn synthetic_source_lines(lines: usize, line_width: usize) -> Vec<String> {
    (0..lines)
        .map(|idx| bench_line(idx, idx, line_width))
        .collect()
}

pub fn stream_events(
    count: usize,
    paths_per_event: usize,
    lines_per_diff: usize,
) -> Vec<StreamEvent> {
    (0..count)
        .map(|event_idx| {
            let mut per_file_diffs = BTreeMap::new();
            let file_paths = (0..paths_per_event)
                .map(|path_idx| {
                    let path = PathBuf::from(format!("src/stream_{event_idx}_{path_idx}.rs"));
                    let mut diff = String::from("@@ -1,0 +1,1 @@\n");
                    for line_idx in 0..lines_per_diff {
                        diff.push('+');
                        diff.push_str(&bench_line(event_idx, line_idx, 48));
                        diff.push('\n');
                    }
                    per_file_diffs.insert(path.clone(), diff);
                    path
                })
                .collect::<Vec<_>>();
            StreamEvent {
                metadata: crate::hook::SanitizedEvent {
                    session_id: Some("bench-session".into()),
                    hook_event_name: "PostToolUse".into(),
                    tool_name: Some("Edit".into()),
                    file_paths,
                    cwd: PathBuf::from("/tmp/kizu-perf"),
                    timestamp_ms: 1_700_000_000_000 + event_idx as u64,
                },
                per_file_diffs,
            }
        })
        .collect()
}

fn bench_line(hunk_idx: usize, line_idx: usize, line_width: usize) -> String {
    let prefix = format!("let generated_{hunk_idx}_{line_idx} = ");
    let fill_len = line_width.saturating_sub(prefix.len());
    format!("{prefix}\"{}\";", "x".repeat(fill_len))
}

fn make_file(name: &str, hunks: Vec<Hunk>, secs: u64) -> FileDiff {
    let added = hunks
        .iter()
        .flat_map(|h| h.lines.iter())
        .filter(|line| line.kind == LineKind::Added)
        .count();
    let deleted = hunks
        .iter()
        .flat_map(|h| h.lines.iter())
        .filter(|line| line.kind == LineKind::Deleted)
        .count();
    FileDiff {
        path: PathBuf::from(name),
        status: FileStatus::Modified,
        added,
        deleted,
        content: DiffContent::Text(hunks),
        mtime: SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
        header_prefix: None,
    }
}

fn app_with_files(mut files: Vec<FileDiff>) -> App {
    files.sort_by_key(|file| file.mtime);
    let mut app = App {
        root: PathBuf::from("/tmp/kizu-perf"),
        git_dir: PathBuf::from("/tmp/kizu-perf/.git"),
        common_git_dir: PathBuf::from("/tmp/kizu-perf/.git"),
        current_branch_ref: Some("refs/heads/main".into()),
        baseline_sha: "abcdef1234567890abcdef1234567890abcdef12".into(),
        files,
        layout: ScrollLayout::default(),
        scroll: 0,
        cursor_sub_row: 0,
        cursor_placement: CursorPlacement::Centered,
        anchor: None,
        help_overlay: false,
        picker: None,
        scar_comment: None,
        revert_confirm: None,
        search_input: None,
        file_view: None,
        search: None,
        seen_hunks: BTreeMap::new(),
        follow_mode: true,
        last_error: None,
        input_health: None,
        head_dirty: false,
        should_quit: false,
        last_body_height: Cell::new(24),
        last_body_width: Cell::new(None),
        visual_top: Cell::new(0.0),
        visual_index_cache: RefCell::new(None),
        anim: None,
        wrap_lines: false,
        show_line_numbers: false,
        watcher_health: WatcherHealth::default(),
        highlighter: std::cell::OnceCell::new(),
        config: crate::config::KizuConfig::default(),
        view_mode: ViewMode::default(),
        saved_diff_scroll: 0,
        saved_stream_scroll: 0,
        stream_events: Vec::new(),
        processed_event_paths: HashSet::new(),
        session_start_ms: 0,
        bound_session_id: None,
        diff_snapshots: DiffSnapshots::default(),
        scar_undo_stack: Vec::new(),
        scar_focus: None,
        pinned_cursor_y: None,
    };
    app.build_layout();
    app.refresh_anchor();
    app
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::DiffContent;

    #[test]
    fn perf_fixture_many_hunk_app_has_requested_shape() {
        let app = many_hunk_app(8, 4, 32);

        assert_eq!(app.files.len(), 1);
        let DiffContent::Text(hunks) = &app.files[0].content else {
            panic!("perf fixture must be text diff");
        };
        assert_eq!(hunks.len(), 8);
        assert_eq!(hunks[0].lines.len(), 4);
        assert!(!app.layout.rows.is_empty());
        assert_eq!(app.layout.hunk_starts.len(), 8);
    }

    #[test]
    fn perf_fixture_large_unified_diff_parses_to_requested_shape() {
        let raw = large_unified_diff(2, 3, 4, 24);
        let files = parse_unified_diff_for_bench(&raw);

        assert_eq!(files.len(), 2);
        for file in files {
            let DiffContent::Text(hunks) = file.content else {
                panic!("perf fixture must parse text diff");
            };
            assert_eq!(hunks.len(), 3);
            assert_eq!(hunks[0].lines.len(), 4);
        }
    }
}
