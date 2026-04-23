use std::cell::Cell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use crate::app::{
    App, CursorPlacement, DiffSnapshots, FileViewState, ScrollLayout, SearchState, ViewMode,
    WatcherHealth,
};
use crate::git::{DiffContent, DiffLine, FileDiff, FileStatus, Hunk, LineKind};

pub(crate) fn diff_line(kind: LineKind, content: &str) -> DiffLine {
    DiffLine {
        kind,
        content: content.to_string(),
        has_trailing_newline: true,
    }
}

pub(crate) fn hunk(old_start: usize, lines: Vec<DiffLine>) -> Hunk {
    let added = lines.iter().filter(|l| l.kind == LineKind::Added).count();
    let deleted = lines.iter().filter(|l| l.kind == LineKind::Deleted).count();
    Hunk {
        old_start,
        old_count: deleted,
        new_start: old_start,
        new_count: added,
        lines,
        context: None,
    }
}

pub(crate) fn make_file(name: &str, hunks: Vec<Hunk>, secs: u64) -> FileDiff {
    let added: usize = hunks
        .iter()
        .flat_map(|h| h.lines.iter())
        .filter(|l| l.kind == LineKind::Added)
        .count();
    let deleted: usize = hunks
        .iter()
        .flat_map(|h| h.lines.iter())
        .filter(|l| l.kind == LineKind::Deleted)
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

pub(crate) fn single_hunk_file(name: &str, lines: Vec<DiffLine>, secs: u64) -> FileDiff {
    make_file(name, vec![hunk(1, lines)], secs)
}

pub(crate) fn single_added_hunk_file(
    name: &str,
    old_start: usize,
    text: &str,
    secs: u64,
) -> FileDiff {
    make_file(
        name,
        vec![hunk(old_start, vec![diff_line(LineKind::Added, text)])],
        secs,
    )
}

pub(crate) fn single_added_file(name: &str, text: &str, secs: u64) -> FileDiff {
    single_added_hunk_file(name, 1, text, secs)
}

pub(crate) fn single_added_app(name: &str, text: &str) -> App {
    app_with_files(vec![single_added_file(name, text, 100)])
}

pub(crate) fn app_with_file(file: FileDiff) -> App {
    app_with_files(vec![file])
}

pub(crate) fn app_with_hunks(name: &str, hunks: Vec<Hunk>, secs: u64) -> App {
    app_with_file(make_file(name, hunks, secs))
}

pub(crate) fn binary_file(name: &str, secs: u64) -> FileDiff {
    FileDiff {
        path: PathBuf::from(name),
        status: FileStatus::Modified,
        added: 0,
        deleted: 0,
        content: DiffContent::Binary,
        mtime: SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
        header_prefix: None,
    }
}

pub(crate) fn file_view_state(
    path: &str,
    lines: Vec<String>,
    cursor: usize,
    last_line_has_trailing_newline: bool,
) -> FileViewState {
    FileViewState {
        path: PathBuf::from(path),
        return_scroll: 0,
        lines,
        line_bg: HashMap::new(),
        cursor,
        cursor_sub_row: 0,
        scroll_top: 0,
        anim: None,
        visual_top: 0.0,
        last_body_width: Cell::new(1),
        last_line_has_trailing_newline,
    }
}

pub(crate) fn install_search(app: &mut App, query: &str, current: usize) -> usize {
    let matches = crate::app::find_matches(&app.layout, &app.files, query);
    let len = matches.len();
    app.search = Some(SearchState {
        query: query.to_string(),
        matches,
        current,
    });
    len
}

/// Build an `App` against `/tmp/fake` with no real filesystem use.
/// Files are sorted in ascending mtime order to match `recompute_diff`.
pub(crate) fn app_with_files(files: Vec<FileDiff>) -> App {
    let mut app = App {
        root: PathBuf::from("/tmp/fake"),
        git_dir: PathBuf::from("/tmp/fake/.git"),
        common_git_dir: PathBuf::from("/tmp/fake/.git"),
        current_branch_ref: Some("refs/heads/main".into()),
        baseline_sha: "abcdef1234567890abcdef1234567890abcdef12".into(),
        files: Vec::new(),
        layout: ScrollLayout::default(),
        scroll: 0,
        cursor_sub_row: 0,
        cursor_placement: CursorPlacement::Centered,
        anchor: None,
        help_overlay: false,
        picker: None,
        scar_comment: None,
        revert_confirm: None,
        file_view: None,
        search_input: None,
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
    app.files = files;
    app.files.sort_by_key(|a| a.mtime);
    app.build_layout();
    app.refresh_anchor();
    app
}
