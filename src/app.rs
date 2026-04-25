use anyhow::{Context, Result};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

mod file_view;
mod input;
mod layout;
mod navigation;
mod picker;
mod review;
mod runtime;
mod search;
mod stream_events;
mod text_input;
#[cfg(test)]
use crate::stream::compute_operation_diff;
pub use file_view::FileViewState;
use layout::build_scroll_layout;
pub use layout::{
    CursorPlacement, HunkAnchor, RowKind, ScrollLayout, VisualIndex, hunk_fingerprint,
    seen_hunk_fingerprint,
};
pub use picker::PickerState;
pub use review::{RevertConfirmState, ScarCommentState, ScarUndoEntry};
pub use runtime::run;
pub use search::{SearchInputState, SearchState, find_matches};
pub use stream_events::{DiffSnapshots, StreamEvent};
use text_input::{TextInputKeyEffect, edit_insert_str, handle_text_input_edit};

/// Duration of a single hunk-to-hunk scroll animation. 150 ms lands in
/// the "noticeable but not slow" band and matches the research doc.
const SCROLL_ANIM_DURATION: Duration = Duration::from_millis(150);

use crate::git::{self, FileDiff, FileStatus};
use crate::stream::build_stream_files;
use crate::watcher::{WatchEvent, WatchSource};

/// Which TUI view is currently active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    /// Main diff view — filesystem-based state view ("what does the repo look like now?").
    #[default]
    Diff,
    /// Stream mode — event-log-based operation history ("what did the agent do?").
    Stream,
}

/// Half-page scroll constant for `Ctrl-d` / `Ctrl-u`. M5+ may swap this for
/// the real viewport height once we plumb it through; until then a fixed
/// value keeps `handle_key` testable as a pure function.
const HALF_PAGE: usize = 12;

/// Canned scar body bound to the `a` key — "ask". The
/// `@kizu[ask]:` marker itself is added by
/// [`crate::scar::CommentSyntax::render_scar`],
/// so this constant holds *just the instruction text*. Plain English
/// imperatives travel across agents (Claude Code / Codex / Cursor /
/// Gemini) without translation layers — the scar is read by the
/// agent as part of the source file itself.
pub(crate) const SCAR_TEXT_ASK: &str = "explain this change";

/// Canned scar body bound to the `r` key — "reject".
pub(crate) const SCAR_TEXT_REJECT: &str = "revert this change";

#[derive(Debug, Clone)]
pub(crate) struct CachedVisualIndex {
    body_width: Option<usize>,
    index: VisualIndex,
}

/// Top-level application state. Single-threaded, mutated by the event loop
/// via `&mut self` (we run on tokio's `current_thread` flavor — see ADR-0003).
pub struct App {
    pub root: PathBuf,
    pub git_dir: PathBuf,
    /// Shared "common" git dir — equal to `git_dir` for normal repos,
    /// distinct for linked worktrees where `refs/heads/**` lives in the
    /// main repo's `.git/`. The watcher needs both to catch commits
    /// performed inside a linked worktree (ADR-0005 addendum).
    pub common_git_dir: PathBuf,
    /// Full ref name that HEAD pointed at when the session started
    /// (e.g. `refs/heads/main`). `None` if HEAD was detached. Used
    /// by the watcher to narrow the baseline-affecting path set —
    /// unrelated ref activity (remotes, tags, sibling branches) no
    /// longer raises false HEAD-dirty signals (ADR-0007).
    pub current_branch_ref: Option<String>,
    pub baseline_sha: String,
    /// Files in the diff, sorted by `mtime` ascending. The newest file is the
    /// last entry so the scroll view reads top-to-bottom chronologically.
    pub files: Vec<FileDiff>,
    /// Derived flat row plan for the scroll view. Rebuilt whenever `files`
    /// changes via `build_layout`.
    pub layout: ScrollLayout,
    /// The cursor's row index inside `layout.rows`. The renderer derives
    /// the actual viewport top from this + [`Self::cursor_placement`].
    pub scroll: usize,
    /// Intra-row visual offset of the cursor. **Always 0 in nowrap
    /// mode.** In wrap mode this is how many visual lines into the
    /// logical row at `scroll` the cursor has walked via Ctrl-d /
    /// Ctrl-u / J / K. The cursor's visual y is
    /// `VisualIndex::visual_y(scroll) + cursor_sub_row`; the
    /// placement target and the render-time arrow both respect it,
    /// so Ctrl-d inside a 200-char minified JSON edit is no longer a
    /// no-op (ADR-0009 fix). Resets to 0 on any hunk jump
    /// (`scroll_to`, `next_hunk`, `prev_hunk`, `g`, `G`, follow),
    /// wrap toggle, or layout rebuild.
    pub cursor_sub_row: usize,
    /// Where the cursor sits visually inside the viewport. Toggled by `z`.
    pub cursor_placement: CursorPlacement,
    /// Path-tracked anchor: which `(path, hunk_old_start)` the user is
    /// looking at. Lets `recompute_diff` slide `scroll` to the same hunk
    /// even when the row count has shifted.
    pub anchor: Option<HunkAnchor>,
    /// Help overlay state. `true` while the `?` keymap popup is open.
    /// The overlay shadows normal action keys until Esc / `?` / `q`
    /// closes it, so reading help never accidentally mutates files.
    pub help_overlay: bool,
    /// Modal file picker. `Some` when the user has pressed `s`.
    pub picker: Option<PickerState>,
    /// Free-text scar input overlay. `Some` when the user has pressed
    /// `c` on a scar-able row and is composing the comment body.
    pub scar_comment: Option<ScarCommentState>,
    /// Hunk-revert confirmation overlay. `Some` when the user has
    /// pressed `x` on a hunk and is being asked `(y/N)`.
    pub revert_confirm: Option<RevertConfirmState>,
    /// Transient `/` query composer. `Some` while the user is
    /// typing the search query; cleared on Enter (confirm) or Esc.
    pub search_input: Option<SearchInputState>,
    /// File-view zoom state. `Some` when the user has pressed
    /// `Enter` on a hunk and is looking at the whole file.
    pub file_view: Option<FileViewState>,
    /// Confirmed search state (query + matches + current index).
    /// Survives across normal-mode navigation so `n` / `N` can
    /// jump between hits.
    pub search: Option<SearchState>,
    /// "Seen" marks for hunks the user has visually reviewed and
    /// wants to collapse out of the attention surface (v0.4). Keyed
    /// by `(relative file path, hunk.old_start)`; the value is the
    /// hunk's content fingerprint at the moment Space was pressed.
    ///
    /// A watcher-driven recompute invalidates the mark when **either**
    /// the pre-image anchor (`old_start`) moves **or** the content
    /// fingerprint changes, so a mark only survives when the hunk is
    /// bit-for-bit the one the user saw. Seen hunks have their
    /// `DiffLine` rows omitted from the layout (only the `HunkHeader`
    /// remains). Space toggles; nothing is written to disk
    /// (see plans/v0.2.md M4, plans/v0.4.md).
    pub seen_hunks: BTreeMap<(PathBuf, usize), u64>,
    pub follow_mode: bool,
    /// Set when the most recent `compute_diff` failed. Cleared on success.
    pub last_error: Option<String>,
    /// Terminal input health. Tracked separately from `last_error`
    /// so a successful `git diff` recompute cannot silently erase the
    /// fact that keyboard input has failed.
    pub input_health: Option<String>,
    /// Set whenever HEAD/refs move; the user must press `R` to re-baseline.
    pub head_dirty: bool,
    pub should_quit: bool,
    /// Last viewport height (in rows) the renderer used. Updated through
    /// interior mutability so the next `J`/`K` press can size its scroll
    /// chunk relative to the current screen — bigger window, bigger jumps.
    /// Defaults to 24 before the first render.
    pub last_body_height: Cell<usize>,
    /// Last wrap body width the renderer used, or `None` when wrap
    /// mode is disabled. Key handlers read this to drive visual-row
    /// scroll math (see [`VisualIndex`]). Updated every frame in
    /// tandem with `last_body_height`.
    pub last_body_width: Cell<Option<usize>>,
    /// The row position the renderer actually drew the viewport at on
    /// the last frame. Matches the logical [`Self::viewport_top`] when
    /// idle; lags behind during a [`ScrollAnim`]. Used as the `from`
    /// value when a new animation kicks off (so key mashes don't
    /// snap — the next tween picks up from wherever the current one
    /// happened to be).
    pub visual_top: Cell<f32>,
    /// Wrap-mode visual coordinate cache. `VisualIndex` is O(layout rows)
    /// to build because it measures every logical row. Rendering may ask
    /// for placement more than once per frame (sticky-header decision),
    /// so cache the current width until `build_layout` invalidates it.
    pub(crate) visual_index_cache: RefCell<Option<CachedVisualIndex>>,
    /// Active viewport-top tween. `None` when the renderer should
    /// draw at the logical target.
    pub anim: Option<ScrollAnim>,
    /// Line-wrap mode. When `true`, long diff lines wrap to the
    /// viewport width (preserving the `+`/`-`/` ` prefix on every
    /// continuation row) and a `¶` marker is drawn at the end of
    /// each logical line so real newlines can be distinguished from
    /// wrap boundaries. Toggled by the `w` key.
    pub wrap_lines: bool,
    /// Line-number gutter (v0.5). When `true`, the renderer prepends a
    /// right-aligned `old | new` (diff view) or single-column (file
    /// view) gutter next to the existing cursor bar. Stream mode
    /// always suppresses this regardless of the flag because synthetic
    /// `old_start`/`new_start` values are not real file line numbers.
    /// Toggled by `#` (configurable via `keys.line_numbers_toggle`).
    pub show_line_numbers: bool,
    /// Watcher backend health, tracked **separately** from
    /// `last_error` so that a successful one-off `git diff` recompute
    /// does not silently clear a live filesystem-watcher failure
    /// (ADR-0008). `Failed` persists until a subsequent non-Error
    /// watcher event confirms recovery, or the watcher is restarted.
    pub watcher_health: WatcherHealth,
    /// Lazy-initialized syntax highlighter. Loaded on first render
    /// to avoid paying syntect's SyntaxSet load cost at startup.
    pub highlighter: std::cell::OnceCell<crate::highlight::Highlighter>,
    /// User configuration loaded from `~/.config/kizu/config.toml`.
    /// Controls keybindings, colors, debounce timing, editor command,
    /// and terminal auto-split preferences.
    pub config: crate::config::KizuConfig,
    /// Active view mode: Diff (default) or Stream.
    pub view_mode: ViewMode,
    /// Saved scroll position for the diff view, restored when Tab
    /// switches back from stream mode.
    pub saved_diff_scroll: usize,
    /// Saved scroll position for the stream view, restored when Tab
    /// switches back from diff mode.
    pub saved_stream_scroll: usize,
    /// Stream mode events, ordered by timestamp ascending.
    pub stream_events: Vec<StreamEvent>,
    /// Event-log file paths that have already been ingested by
    /// [`Self::handle_event_log`]. Prevents double-processing when
    /// `replay_events_dir` scans startup-gap files and the watcher
    /// later re-delivers the same path (some notify backends fire
    /// pre-existing files on arm). Keyed on absolute event-file
    /// path, which is stable because `hook::write_event` uses a
    /// uniqueness suffix.
    pub processed_event_paths: std::collections::HashSet<PathBuf>,
    /// Unix epoch millisecond at which this kizu TUI session started.
    /// `handle_event_log` rejects events whose `timestamp_ms` is
    /// earlier than this value so stream mode never ingests:
    /// (a) leftover events from a previous kizu session on the
    ///     same project (replacing the destructive bulk-delete of
    ///     `clean_stale_events`), or
    /// (b) any other concurrent session's historical events that
    ///     existed before this session was bootstrapped.
    pub session_start_ms: u64,
    /// Agent `session_id` this TUI is bound to. Populated on the
    /// first `handle_event_log` ingest that carries a session_id;
    /// later events with a different session_id are dropped so two
    /// concurrent agents writing to the same repo cannot cross-
    /// pollute `diff_snapshots` or the stream history. `None`
    /// before the first bound ingest; stays bound for the rest of
    /// the session.
    pub bound_session_id: Option<String>,
    /// Per-file diff snapshots used to compute per-operation diffs.
    /// Maps file path → most recent cumulative diff output. Capped
    /// via [`DiffSnapshots`] so long sessions that touch many files
    /// don't accumulate unbounded state.
    pub diff_snapshots: DiffSnapshots,
    /// Session-scoped undo stack for scar insertions. Each successful
    /// [`crate::scar::insert_scar`] pushes an entry; the `u` key pops
    /// the top and calls [`crate::scar::remove_scar`], reversing only
    /// that one write. Receipts capture the post-insert line number and
    /// rendered line so undo refuses to delete content the user edited
    /// in the meantime (scar.rs `ScarRemove::Mismatch`).
    pub scar_undo_stack: Vec<ScarUndoEntry>,
    /// Sticky cursor target set when a scar is inserted or undone.
    /// Persists across watcher-driven `recompute_diff` calls so the
    /// asynchronous filesystem notification can't snap the cursor
    /// back to the hunk header. Cleared as soon as the user presses
    /// any navigation key.
    pub scar_focus: Option<(PathBuf, usize)>,
    /// Viewport pin: when `Some(y)`, the renderer forces the viewport
    /// top so the cursor lands at visual row `y` on screen, overriding
    /// the placement-derived default. Set by `apply_computed_files`
    /// when a watcher-driven recompute relocates the anchored hunk
    /// (e.g. the anchored file floats to the end of the mtime-sorted
    /// list after being edited) — the pin keeps the user's focused
    /// hunk at the same screen row instead of sliding with the
    /// layout. Cleared on any user-initiated cursor move (`scroll_to`,
    /// `scroll_by`, `follow_restore`, `open_file_view`, ...).
    pub pinned_cursor_y: Option<usize>,
}

/// Tracks whether the underlying notify debouncers are still pushing
/// events into the channel. Decoupled from `App.last_error`: a failing
/// `compute_diff` must not pretend the watcher has recovered, and a
/// successful recompute must not pretend a dropped FSEvents queue has
/// repaired itself. See ADR-0008.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WatcherHealth {
    /// Source-scoped watcher failures. Multiple debouncers back the
    /// watcher layer (worktree + several git-state roots), so one live
    /// event must not erase the failure of a different source.
    failures: BTreeMap<WatchSource, String>,
}

impl WatcherHealth {
    pub fn record_failure(&mut self, source: WatchSource, message: String) {
        self.failures.insert(source, message);
    }

    pub fn clear_source(&mut self, source: WatchSource) {
        self.failures.remove(&source);
    }

    #[cfg(test)]
    pub fn is_healthy(&self) -> bool {
        self.failures.is_empty()
    }

    pub fn summary(&self) -> Option<String> {
        if self.failures.is_empty() {
            return None;
        }
        let mut parts = self.failures.values().cloned().collect::<Vec<_>>();
        parts.sort();
        Some(parts.join("; "))
    }

    #[cfg(test)]
    fn has_failure(&self, source: WatchSource, needle: &str) -> bool {
        self.failures
            .get(&source)
            .is_some_and(|msg| msg.contains(needle))
    }
}

/// Follow-up work the event loop must perform after dispatching a
/// key. Keeps `App::handle_key` a pure state mutator while still
/// letting specific keys request out-of-band side effects such as
/// watcher reconfiguration (ADR-0008). New variants should be added
/// here rather than threading side-effect channels through every
/// handler method.
///
/// Not `#[must_use]`: the event loop is the one caller that
/// genuinely needs to act on the effect, and tagging the enum
/// would force every existing `handle_key` test to wrap results in
/// `let _ = …` for zero actual benefit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyEffect {
    /// No extra work. Most key handlers return this.
    None,
    /// The symbolic HEAD ref has changed — the event loop must
    /// hot-swap the watcher's `BaselineMatcherInner` so subsequent
    /// branch-ref writes raise `WatchEvent::GitHead`. Without this
    /// the watcher would stay pinned to the session's startup
    /// branch after `R`.
    ReconfigureWatcher,
    /// The user pressed `e` on a scar-able row — the event loop
    /// must suspend the ratatui terminal, spawn the resolved
    /// editor, wait for it, and then re-enter the alternate screen.
    /// The `EditorInvocation` carries a fully-resolved
    /// `(program, args)` pair so the event loop does not need to
    /// re-read `$EDITOR`.
    OpenEditor(EditorInvocation),
}

/// Fully-resolved external-editor invocation. Produced inside
/// [`App::open_in_editor`] via [`build_editor_invocation`] so the
/// event loop can spawn the editor with no further parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorInvocation {
    pub program: String,
    pub args: Vec<String>,
}

/// Build an [`EditorInvocation`] from the user's `$EDITOR` value.
///
/// `$EDITOR` is split on whitespace (no shell quoting — matching
/// `git`'s `GIT_EDITOR` conventions for MVP). The first token is
/// the program; any remaining tokens are kept as leading args.
///
/// Line-number format depends on the editor:
/// - vim/nvim/vi/nano/emacs/kak use `+<line> <file>`
/// - zed/code/subl/hx/cursor and others use `<file>:<line>`
///
/// Returns `None` when `editor_env` is `None` or empty / all
/// whitespace, so callers get a single consistent "no editor
/// configured → no-op" path.
pub fn build_editor_invocation(
    editor_env: Option<&str>,
    line: usize,
    file: &Path,
) -> Option<EditorInvocation> {
    let env = editor_env?.trim();
    if env.is_empty() {
        return None;
    }
    let mut parts = env.split_whitespace().map(String::from);
    let program = parts.next()?;
    let mut args: Vec<String> = parts.collect();

    let basename = Path::new(&program)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    if uses_plus_line_format(basename) {
        args.push(format!("+{line}"));
        args.push(file.display().to_string());
    } else {
        args.push(format!("{}:{line}", file.display()));
    }

    Some(EditorInvocation { program, args })
}

/// Editors that accept `+<line> <file>` for line-jump. All others
/// default to the `<file>:<line>` convention (VS Code, Zed,
/// Sublime, Helix, Cursor, etc.).
fn uses_plus_line_format(basename: &str) -> bool {
    matches!(
        basename,
        "vim" | "nvim" | "vi" | "nano" | "emacs" | "emacsclient" | "kak" | "mg" | "nvi"
    )
}

fn is_quit_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('q'))
        || (matches!(key.code, KeyCode::Char('c')) && key.modifiers.contains(KeyModifiers::CONTROL))
}

fn control_page_delta(key: KeyEvent) -> Option<isize> {
    if !key.modifiers.contains(KeyModifiers::CONTROL) {
        return None;
    }
    match key.code {
        KeyCode::Char('d') => Some(HALF_PAGE as isize),
        KeyCode::Char('u') => Some(-(HALF_PAGE as isize)),
        _ => None,
    }
}

/// Default body height assumed before the first render has had a chance
/// to update [`App::last_body_height`]. 24 is the classic VT100 height.
const DEFAULT_BODY_HEIGHT: usize = 24;

/// Single-shot easing state for the viewport's top-row tween.
///
/// The tween sources its start point from `from` (captured at the moment
/// the animation began, in row-units) and its endpoint from the *current*
/// logical [`App::viewport_top`] — recomputed every frame so `git diff`
/// fires that shuffle the layout mid-animation still land on the right
/// row. Easing is ease-out cubic: fast at the start, settling softly.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollAnim {
    pub from: f32,
    pub start: Instant,
    pub dur: Duration,
}

impl ScrollAnim {
    /// Sample the tween at `now` against a (possibly moving) `target`.
    /// Returns `(visual, done)` where `visual` is the row position the
    /// renderer should use and `done` flips to `true` once the animation
    /// has finished (`now >= start + dur`).
    pub fn sample(&self, target: f32, now: Instant) -> (f32, bool) {
        let elapsed = now.saturating_duration_since(self.start).as_secs_f32();
        let dur_secs = self.dur.as_secs_f32().max(1e-6);
        let t = (elapsed / dur_secs).clamp(0.0, 1.0);
        // ease-out cubic: 1 - (1 - t)^3
        let inv = 1.0 - t;
        let e = 1.0 - inv * inv * inv;
        let v = self.from + (target - self.from) * e;
        (v, t >= 1.0)
    }
}

impl App {
    /// Construct an `App` for `root`. Resolves git layout, loads the initial
    /// diff, and parks the scroll cursor on the most-recently-modified hunk.
    ///
    /// **Fail-fast:** if the very first `git diff` errors out, bootstrap
    /// propagates the error instead of entering the event loop with an
    /// empty `files` snapshot. The watcher-driven path (see
    /// [`Self::recompute_diff`]) still swallows errors into `last_error`
    /// so that later transient failures preserve the last good snapshot,
    /// but an initial-load failure must never render as a silent "clean"
    /// view — the user would not be able to tell whether the worktree is
    /// actually empty or the tool is broken.
    pub fn bootstrap(root: PathBuf) -> Result<Self> {
        let root = git::find_root(&root).context("resolving worktree root")?;
        let git_dir = git::git_dir(&root).context("resolving git directory")?;
        let common_git_dir =
            git::git_common_dir(&root).context("resolving common git directory")?;
        let current_branch_ref =
            git::current_branch_ref(&root).context("resolving current branch ref")?;
        let baseline_sha = git::head_sha(&root).context("capturing baseline HEAD")?;
        // One `git diff --no-renames <baseline>` gives us both the
        // parsed FileDiff list **and** the per-file raw text. Routing
        // the raw text straight into `diff_snapshots` collapses the
        // old "compute_diff + N × diff_single_file" startup pattern
        // into a single subprocess.
        let (diff, snapshots) = match git::compute_diff_with_snapshots(&root, &baseline_sha) {
            Ok((files, snaps)) => (Ok(files), snaps),
            Err(e) => (Err(e), std::collections::HashMap::new()),
        };
        let mut app = Self::bootstrap_with_diff(
            root,
            git_dir,
            common_git_dir,
            current_branch_ref,
            baseline_sha,
            diff,
        )?;
        app.diff_snapshots.replace_from_map(snapshots);
        Ok(app)
    }

    /// Inner bootstrap: takes already-resolved git layout plus the
    /// **result** of the initial `compute_diff`. Propagates the diff
    /// error with context when it is `Err`, otherwise constructs the
    /// `App` and applies the computed files. Factored out so tests can
    /// drive both branches deterministically without spinning up a
    /// real repository.
    pub(crate) fn bootstrap_with_diff(
        root: PathBuf,
        git_dir: PathBuf,
        common_git_dir: PathBuf,
        current_branch_ref: Option<String>,
        baseline_sha: String,
        diff: Result<Vec<FileDiff>>,
    ) -> Result<Self> {
        let initial =
            diff.with_context(|| format!("initial git diff against baseline {baseline_sha}"))?;
        let config = crate::config::load_config();
        let mut app = Self {
            root,
            git_dir,
            common_git_dir,
            current_branch_ref,
            baseline_sha,
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
            last_body_height: Cell::new(DEFAULT_BODY_HEIGHT),
            last_body_width: Cell::new(None),
            visual_top: Cell::new(0.0),
            visual_index_cache: RefCell::new(None),
            anim: None,
            wrap_lines: false,
            show_line_numbers: config.line_numbers.enabled,
            watcher_health: WatcherHealth::default(),
            highlighter: std::cell::OnceCell::new(),
            config,
            view_mode: ViewMode::default(),
            saved_diff_scroll: 0,
            saved_stream_scroll: 0,
            stream_events: Vec::new(),
            processed_event_paths: std::collections::HashSet::new(),
            session_start_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
            bound_session_id: None,
            diff_snapshots: DiffSnapshots::default(),
            scar_undo_stack: Vec::new(),
            scar_focus: None,
            pinned_cursor_y: None,
        };
        // If the agent handing off to kizu published its session id
        // via the `KIZU_SESSION_ID` environment variable, pre-bind
        // the TUI to it. That shuts the first-writer-wins window
        // where a foreign concurrent agent could capture the
        // binding before our own agent's first event landed,
        // silently hiding every edit we were supposed to review.
        if let Ok(sid) = std::env::var("KIZU_SESSION_ID") {
            let trimmed = sid.trim();
            if !trimmed.is_empty() {
                app.bound_session_id = Some(trimmed.to_string());
            }
        }
        app.apply_computed_files(initial);
        Ok(app)
    }

    /// Half-page-ish chunk size used by `J`/`K` when scrolling within a
    /// long change run. Scales with the actual viewport so a 12-row pane
    /// gets 4-row chunks and a 36-row pane gets 12-row chunks. Always at
    /// least 1 so the cursor still moves on tiny terminals.
    pub fn chunk_size(&self) -> usize {
        (self.last_body_height.get() / 3).max(1)
    }

    /// Toggle the cursor placement between centered and bottom-pinned.
    /// `z` calls this — same vibe as `vim`'s `zz` (centre on cursor).
    pub fn toggle_cursor_placement(&mut self) {
        self.cursor_placement = match self.cursor_placement {
            CursorPlacement::Centered => CursorPlacement::Top,
            CursorPlacement::Top => CursorPlacement::Centered,
        };
    }

    /// Toggle line-wrap mode. `w` calls this. When on, the renderer
    /// wraps long diff lines at the viewport width and decorates every
    /// logical line end with a `¶` marker.
    ///
    /// Also kills any in-flight scroll animation: under wrap the
    /// viewport's top position is tracked in visual-row space
    /// ([`VisualIndex`]), while under nowrap it's logical rows. The
    /// two scales diverge as soon as a single diff line wraps to more
    /// than one visual row, so tweening between them would produce a
    /// disorienting jump. Clearing `anim` makes the next frame snap
    /// to the correct target instead.
    pub fn toggle_wrap_lines(&mut self) {
        self.wrap_lines = !self.wrap_lines;
        self.anim = None;
        // `cursor_sub_row` is only meaningful under wrap mode; when
        // we flip the flag, drop any intra-row offset so the cursor
        // lands cleanly on the row's first visual line under the
        // new coordinate system.
        self.cursor_sub_row = 0;
        self.reflow_file_view();
    }

    /// Toggle the line-number gutter (v0.5). `#` calls this (or
    /// whatever `keys.line_numbers_toggle` is mapped to). Only affects
    /// diff view and file view — Stream mode always suppresses the
    /// gutter regardless of the flag.
    ///
    /// Rebuilds the layout (to refresh `diff_line_numbers` / `max_line_number`)
    /// and reflows the file view (so `VisualIndex::build_lines` matches the
    /// new body_width). Codex adversarial review §Important-2 flagged
    /// that skipping either step leaves stale derived state.
    pub fn toggle_line_numbers(&mut self) {
        self.show_line_numbers = !self.show_line_numbers;
        self.build_layout();
        self.reflow_file_view();
    }

    /// Toggle between Diff and Stream view modes. Rebuilds `files`
    /// and `layout` from the appropriate data source so the existing
    /// scroll/render infrastructure handles both modes identically.
    pub fn toggle_view_mode(&mut self) {
        match self.view_mode {
            ViewMode::Diff => {
                // Save diff scroll, restore stream scroll.
                self.saved_diff_scroll = self.scroll;
                self.view_mode = ViewMode::Stream;
                let stream_files = build_stream_files(&self.stream_events);
                self.apply_computed_files(stream_files);
                let max = self.last_row_index();
                self.scroll_to(self.saved_stream_scroll.min(max));
            }
            ViewMode::Stream => {
                // Save stream scroll, restore diff scroll.
                self.saved_stream_scroll = self.scroll;
                self.view_mode = ViewMode::Diff;
                self.recompute_diff();
                let max = self.last_row_index();
                self.scroll_to(self.saved_diff_scroll.min(max));
            }
        }
    }

    /// Re-run `git diff`, populate per-file mtimes, sort files by mtime
    /// **ascending** (oldest first → newest last), rebuild the row layout,
    /// and restore the anchor. The ascending order is intentional so that
    /// the scroll view reads top-to-bottom in chronological order, like
    /// `tail -f`: the newest hunk lives at the bottom and follow mode is
    /// "pinned to the floor". On failure, record the error in `last_error`
    /// and keep the previous `files` snapshot intact.
    pub fn recompute_diff(&mut self) {
        match git::compute_diff(&self.root, &self.baseline_sha) {
            Ok(files) => self.apply_computed_files(files),
            Err(e) => {
                self.last_error = Some(format!("{e:#}"));
                // self.files / self.layout intentionally untouched.
            }
        }
    }

    /// Accept a freshly-computed file set: populate mtimes, sort, clear
    /// any prior error, rebuild layout, and refresh the anchor. Shared
    /// between [`Self::bootstrap_with_diff`] (initial load) and
    /// [`Self::recompute_diff`] (watcher-driven refreshes).
    ///
    /// Also decides whether to set [`Self::pinned_cursor_y`]: when the
    /// user is in manual mode and had the cursor on-screen before this
    /// recompute, we preserve the cursor's screen row so a watcher-
    /// driven reorder (edited file floats to the tail of the mtime
    /// sort) does not slide the viewport out from under the user.
    fn apply_computed_files(&mut self, mut files: Vec<FileDiff>) {
        // Snapshot pre-state BEFORE any mutation. `visual_top` is the
        // Cell the renderer updates each frame, so subtracting the
        // cursor's visual y from it yields the screen row we last
        // drew the cursor at.
        let pre_had_layout = !self.layout.rows.is_empty();
        let pre_scroll = self.scroll;
        let pre_visual_top = self.visual_top.get();
        let pre_body_height = self.last_body_height.get();
        let pre_body_width = self.last_body_width.get();
        let pre_cursor_visual_y: usize = match pre_body_width {
            None => pre_scroll,
            Some(_) => {
                let vi = VisualIndex::build(&self.layout, &self.files, pre_body_width);
                vi.visual_y(pre_scroll) + self.cursor_sub_row
            }
        };
        let pre_screen_y_f = pre_cursor_visual_y as f32 - pre_visual_top;
        let pre_in_viewport =
            pre_had_layout && pre_screen_y_f >= 0.0 && (pre_screen_y_f as usize) < pre_body_height;
        let pre_screen_y = pre_screen_y_f.max(0.0) as usize;

        // Snapshot the cursor's "content identity" — `(abs_path,
        // new_file_line)` when the cursor is on a DiffLine. Used as a
        // fallback when `refresh_anchor` can only offer a HunkHeader
        // row (its anchor lookup resolves hunks, not individual
        // lines) and `scar_focus` is unavailable or invalid.
        let pre_cursor_content = self.scroll_cursor_new_line();

        let picker_selected_path = self.picker_selected_path();
        // Stream mode files are already ordered by event timestamp and
        // must not have their mtime overwritten by the filesystem. Diff
        // mode files need filesystem mtime for chronological sorting.
        if self.view_mode != ViewMode::Stream {
            self.populate_mtimes(&mut files);
            files.sort_by_key(|a| a.mtime);
        }
        self.last_error = None;
        self.files = files;
        self.build_layout();
        self.refresh_picker_cursor(picker_selected_path.as_deref());
        // Layout rebuild may shift row counts and wrap geometry, so
        // any previously-stored intra-row offset is no longer valid.
        // `refresh_anchor` then repositions the cursor on the same
        // hunk if possible; the sub-row offset starts fresh there.
        self.cursor_sub_row = 0;
        self.refresh_anchor();

        // Sticky scar focus: if a scar op recently set a focus
        // target, re-apply it after `refresh_anchor` so the
        // asynchronous watcher-driven recompute doesn't snap the
        // cursor back to the hunk header. If the target line is no
        // longer representable in the layout (e.g. the surrounding
        // code was edited so the scar's new-file line number is
        // gone or now points at different content), clear the
        // focus and fall through to the pin path below so the
        // cursor doesn't jump to the hunk header.
        let mut scar_focus_applied = false;
        if let Some((abs, line)) = self.scar_focus.clone() {
            if let Some(row) = self.find_new_file_line_row(&abs, line) {
                // `scroll_to` clears `pinned_cursor_y` — scar focus
                // takes precedence over the pin, which is the right
                // call since the user's explicit action was the scar.
                self.scroll_to(row);
                scar_focus_applied = true;
            } else {
                self.scar_focus = None;
            }
        }

        // Content-preservation fallback: `refresh_anchor` can only
        // resolve `(path, hunk_old_start)` — if that resolves, the
        // cursor lands on the hunk's `@@` header row, not on the
        // specific DiffLine the user was inspecting. When the user
        // was on a DiffLine before the recompute, re-target the
        // cursor to the DiffLine at (or nearest to) the pre-recompute
        // new-file line. This avoids the "cursor snaps to @@" the
        // user sees when a scar is edited away and `scar_focus`
        // falls through.
        //
        // Uses direct `self.scroll = row` instead of `scroll_to`
        // because the pin below still needs to fire — `scroll_to`
        // would clear it.
        if !scar_focus_applied
            && let Some((abs, pre_line)) = pre_cursor_content.clone()
            && matches!(
                self.layout.rows.get(self.scroll),
                Some(RowKind::HunkHeader { .. })
            )
            && let Some(row) = self.find_nearest_new_file_line_row(&abs, pre_line)
        {
            self.scroll = row;
            self.update_anchor_from_scroll();
        }

        // Viewport pin: preserve the cursor's screen row when this
        // recompute is not user-initiated **and** scar focus didn't
        // already relocate the cursor. Sequenced after the scar
        // path so that a scar_focus whose line was edited away
        // (anchor falls back to the hunk header in `refresh_anchor`)
        // still gets its screen row preserved — otherwise the
        // cursor would visibly jump to the HunkHeader row.
        let should_pin = !self.follow_mode && !scar_focus_applied && pre_in_viewport;
        if should_pin {
            self.pinned_cursor_y = Some(pre_screen_y);
            // A pin is a snap, not a slide: cancel any in-flight
            // viewport tween so the next frame redraws at the pinned
            // target directly. Otherwise the animation would
            // interpolate from the stale pre-recompute viewport top
            // to the new pinned target, visibly undoing the pin.
            self.anim = None;
        }
        // else: pin is either already cleared by scroll_to above
        // (scar_focus_applied = true) or was never set (bootstrap
        // / follow mode / cursor off-screen) — nothing to do.

        // Rehydrate search matches: `MatchLocation.row` indexes the
        // layout, so the rebuild above silently invalidated every row
        // pointer. Re-run `find_matches` against the new layout with
        // the confirmed query so `n`/`N` keep working and the body-view
        // highlight overlay lands on the right cells. Clamp `current`
        // into range; empty matches reset to 0 so a future re-entry
        // starts from the top.
        if let Some(state) = self.search.as_mut() {
            let query = state.query.clone();
            state.matches = find_matches(&self.layout, &self.files, &query);
            state.current = if state.matches.is_empty() {
                0
            } else {
                state.current.min(state.matches.len() - 1)
            };
        }
    }

    /// Re-capture HEAD as the new baseline (R key).
    ///
    /// The reset is transactional: the new `baseline_sha` and the
    /// cleared `head_dirty` flag are only committed **after** the
    /// fresh `git diff` against that new baseline succeeds. If either
    /// `head_sha` or `compute_diff` fails, every piece of visible
    /// state is preserved so the user keeps looking at the same diff
    /// with the `HEAD*` warning still present, rather than a stale
    /// snapshot under a silently-advanced baseline.
    ///
    /// Also re-resolves the symbolic HEAD ref. If the user has
    /// switched branches since startup (or toggled detached HEAD on
    /// or off), `self.current_branch_ref` is updated and the caller
    /// must reconfigure the watcher — that's what the return value
    /// `KeyEffect::ReconfigureWatcher` signals. Without this, the
    /// watcher would stay pinned to the old branch ref and stop
    /// raising `GitHead` for commits on the new branch (ADR-0008).
    pub fn reset_baseline(&mut self) -> KeyEffect {
        let new_sha = match git::head_sha(&self.root) {
            Ok(sha) => sha,
            Err(e) => {
                self.last_error = Some(format!("R: {e:#}"));
                return KeyEffect::None;
            }
        };
        // Re-resolve the symbolic HEAD ref *before* running the
        // diff so we know whether a reconfigure will be needed once
        // the transaction commits.
        let new_branch = match git::current_branch_ref(&self.root) {
            Ok(b) => b,
            Err(e) => {
                self.last_error = Some(format!("R: {e:#}"));
                return KeyEffect::None;
            }
        };
        // Seed snapshots from the same `git diff` call that produces
        // the FileDiff list. `apply_reset` clears the map on success;
        // we populate it right after so the next event computes its
        // op_diff against a correct pre-event reference.
        let (diff, snapshots) = match git::compute_diff_with_snapshots(&self.root, &new_sha) {
            Ok((files, snaps)) => (Ok(files), Some(snaps)),
            Err(e) => (Err(e), None),
        };
        let effect = self.apply_reset(new_sha, new_branch, diff);
        if let Some(snaps) = snapshots {
            self.diff_snapshots.replace_from_map(snaps);
        }
        effect
    }

    /// Commit a freshly-resolved baseline + diff into the app. Split
    /// out from [`Self::reset_baseline`] so tests can inject a failing
    /// diff without touching the filesystem and verify that the old
    /// baseline, `head_dirty`, and `files` snapshot all survive.
    ///
    /// Returns [`KeyEffect::ReconfigureWatcher`] when the resolved
    /// branch differs from the session's previous tracking, so the
    /// event loop can hot-swap the watcher's `BaselineMatcherInner`
    /// without rebuilding the debouncers.
    pub(crate) fn apply_reset(
        &mut self,
        new_sha: String,
        new_branch: Option<String>,
        diff: Result<Vec<FileDiff>>,
    ) -> KeyEffect {
        match diff {
            Ok(files) => {
                let branch_changed = new_branch != self.current_branch_ref;
                self.baseline_sha = new_sha;
                self.current_branch_ref = new_branch;
                self.head_dirty = false;
                self.apply_computed_files(files);
                // Drop stream-mode snapshots captured against the
                // previous baseline. Every entry in `diff_snapshots`
                // was `git diff <old_baseline> -- <path>` output;
                // comparing the next hook-log-event against that
                // would misattribute lines that belong to the change
                // between baselines (not to the agent's edit).
                // The caller (`reset_baseline`) repopulates the map
                // from the same `compute_diff_with_snapshots` call
                // that produced `files`, so there's no need to run
                // a second per-file diff sweep here.
                self.diff_snapshots.clear();
                if branch_changed {
                    KeyEffect::ReconfigureWatcher
                } else {
                    KeyEffect::None
                }
            }
            Err(e) => {
                self.last_error = Some(format!("R: {e:#}"));
                // baseline_sha / current_branch_ref / head_dirty /
                // files / diff_snapshots intentionally untouched:
                // the HEAD* warning stays visible and the user keeps
                // seeing the same diff they had before R. Watcher
                // also stays pinned to the old branch, which is the
                // correct behavior for an aborted reset.
                KeyEffect::None
            }
        }
    }

    /// HEAD/refs moved without the user re-baselining yet.
    pub fn mark_head_dirty(&mut self) {
        self.head_dirty = true;
    }

    /// Fold a coalesced burst of watcher events into the app's
    /// health / refresh state and return the follow-up the event
    /// loop still needs to perform: `(needs_recompute, needs_head_dirty)`.
    ///
    /// Split out of [`run_loop`] so the state transitions can be
    /// tested without a real debouncer. Every caller of `run_loop`
    /// and every test that simulates a watcher burst must route
    /// through this method so the health / recovery rules stay
    /// consistent.
    pub fn handle_watch_burst(
        &mut self,
        events: impl IntoIterator<Item = WatchEvent>,
    ) -> (bool, bool) {
        let mut worktree = false;
        let mut head = false;
        let mut recovered_sources = Vec::new();
        let mut failed_sources: BTreeMap<WatchSource, String> = BTreeMap::new();
        for event in events {
            match event {
                WatchEvent::Worktree => {
                    worktree = true;
                    recovered_sources.push(WatchSource::Worktree);
                }
                WatchEvent::GitHead(source) => {
                    head = true;
                    recovered_sources.push(source);
                }
                WatchEvent::EventLog(path) => {
                    self.handle_event_log(path);
                }
                WatchEvent::Error { source, message } => {
                    failed_sources.insert(source, message);
                }
            }
        }
        for source in recovered_sources {
            if !failed_sources.contains_key(&source) {
                self.watcher_health.clear_source(source);
            }
        }
        if !failed_sources.is_empty() {
            // Backend failure: record it in the dedicated
            // `watcher_health` slot (NOT `last_error`) so a
            // subsequent successful recompute from some *other*
            // watcher source does not silently erase the fact that
            // live monitoring is partially dead.
            worktree = true;
            for (source, message) in failed_sources {
                self.watcher_health.record_failure(source, message);
            }
        }
        (worktree, head)
    }

    /// `(start, end_exclusive)` row range of the cursor's current hunk.
    /// Reads the range cached by `build_layout`, so render-time hunk
    /// anchoring stays O(1) even when a diff contains thousands of hunks.
    /// Returns `None` when the cursor is not inside a hunk.
    pub fn current_hunk_range(&self) -> Option<(usize, usize)> {
        let (file_idx, hunk_idx) = self.current_hunk()?;
        self.layout
            .hunk_ranges
            .get(file_idx)?
            .get(hunk_idx)
            .copied()
    }

    /// Where the renderer should park the viewport top, given a body
    /// height. Both placement modes prefer to anchor on the cursor's
    /// *whole hunk* when it fits in the viewport, so you always see
    /// the full selected change as one block.
    ///
    /// - `Centered` + short hunk: centre the hunk in the viewport,
    ///   breathing room above and below.
    /// - `Top` + short hunk: pin the hunk's **first** row (its
    ///   header) to the viewport ceiling, so the whole hunk body
    ///   flows downward from the top into the rest of the viewport.
    /// - Either mode + long hunk: fall back to the placement's raw
    ///   cursor-row rule (centred or ceiling-pinned), which is the
    ///   correct behaviour while the user is walking through a hunk
    ///   that can't fit in one screen.
    pub fn viewport_top(&self, viewport_height: usize) -> usize {
        let total = self.layout.rows.len();
        if total <= viewport_height {
            return 0;
        }
        let max_top = total - viewport_height;

        // Viewport pin: keep the cursor at the screen row captured by
        // the most recent `apply_computed_files`. Overrides both the
        // hunk-fit anchoring and the placement-mode cursor rule.
        if let Some(pinned_y) = self.pinned_cursor_y {
            return self.scroll.saturating_sub(pinned_y).min(max_top);
        }

        if let Some((hunk_top, hunk_end)) = self.current_hunk_range() {
            let hunk_size = hunk_end - hunk_top;
            if hunk_size <= viewport_height {
                let raw = match self.cursor_placement {
                    CursorPlacement::Centered => {
                        let pad = (viewport_height - hunk_size) / 2;
                        hunk_top.saturating_sub(pad)
                    }
                    CursorPlacement::Top => hunk_top,
                };
                return raw.min(max_top);
            }
        }

        // Long hunk, or cursor parked on a non-hunk row → fall back
        // to the placement's raw cursor-row rule.
        self.cursor_placement
            .viewport_top(self.scroll, total, viewport_height)
    }

    // ---- layout build / anchor ----------------------------------------

    fn populate_mtimes(&self, files: &mut [FileDiff]) {
        // Single `now` sample shared across every deleted file in this
        // batch so that a mixed edit+delete burst keeps the destructive
        // action at the top of the recency order (= bottom of the
        // ascending layout, which is where follow mode parks). A deleted
        // file has no on-disk mtime to read — the filesystem lookup
        // would fail and the pre-fix fallback pushed it to UNIX_EPOCH,
        // burying the delete under every real change.
        let now = SystemTime::now();
        for f in files {
            if matches!(f.status, FileStatus::Deleted) {
                f.mtime = now;
                continue;
            }
            f.mtime = self
                .root
                .join(&f.path)
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
        }
    }

    pub(crate) fn build_layout(&mut self) {
        self.layout = build_scroll_layout(&self.files, &self.seen_hunks);
        self.visual_index_cache.get_mut().take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{DiffContent, DiffLine, LineKind};
    use crate::scar::ScarKind;
    use crate::test_support::{
        added_hunk, added_hunk_app, added_hunk_file, app_with_files as fake_app, app_with_hunks,
        binary_file, context_hunk_file, diff_line, file_view_state, file_with_hunk, hunk,
        install_search, make_file, numbered_added_lines, prefixed_diff_lines, single_added_app,
        single_added_file, single_added_hunk_file, single_deleted_file, single_hunk_app,
        single_hunk_file,
    };
    use std::time::Duration;

    #[test]
    fn diff_snapshots_evicts_oldest_when_cap_exceeded() {
        // Long agent sessions can pile up one snapshot per unique file
        // touched. Without a cap the map grows unboundedly; a LRU-ish
        // eviction keeps working-set size predictable. The eldest
        // inserted key must be the first to go when the cap is hit.
        let mut snaps = DiffSnapshots::with_cap(3);
        snaps.insert(PathBuf::from("a"), "diff-a".into());
        snaps.insert(PathBuf::from("b"), "diff-b".into());
        snaps.insert(PathBuf::from("c"), "diff-c".into());
        assert_eq!(snaps.len(), 3);

        snaps.insert(PathBuf::from("d"), "diff-d".into());
        assert_eq!(snaps.len(), 3, "cap must hold after overflow");
        assert!(
            !snaps.contains_key(&PathBuf::from("a")),
            "eldest entry must be evicted first"
        );
        assert!(snaps.contains_key(&PathBuf::from("d")));
    }

    #[test]
    fn diff_snapshots_reinsert_refreshes_recency() {
        // When handle_event_log re-inserts the snapshot for a path it
        // already knows about, that path must move to the "most
        // recently used" end so a subsequent overflow drops an older
        // path instead.
        let mut snaps = DiffSnapshots::with_cap(3);
        snaps.insert(PathBuf::from("a"), "diff-a".into());
        snaps.insert(PathBuf::from("b"), "diff-b".into());
        snaps.insert(PathBuf::from("c"), "diff-c".into());

        // Touch "a" so it is no longer the eldest.
        snaps.insert(PathBuf::from("a"), "diff-a-updated".into());

        snaps.insert(PathBuf::from("d"), "diff-d".into());
        assert!(
            snaps.contains_key(&PathBuf::from("a")),
            "recently-touched entry must survive the next overflow"
        );
        assert!(
            !snaps.contains_key(&PathBuf::from("b")),
            "after touching a, b is now the eldest and must be dropped"
        );
        assert_eq!(
            snaps.get(&PathBuf::from("a")),
            Some(&"diff-a-updated".to_string()),
            "re-insert must keep the newer value",
        );
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn type_chars(app: &mut App, text: &str) {
        for c in text.chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
    }

    fn file_idx(app: &App, name: &str) -> usize {
        app.files
            .iter()
            .position(|f| f.path == Path::new(name))
            .unwrap_or_else(|| panic!("file {name} not in app.files"))
    }

    #[test]
    fn build_layout_produces_header_then_hunks_then_spacer_per_file() {
        let app = single_added_app("a.rs", "x");

        // Expected sequence:
        //   FileHeader, HunkHeader, DiffLine, Spacer
        assert_eq!(app.layout.rows.len(), 4);
        assert!(matches!(
            app.layout.rows[0],
            RowKind::FileHeader { file_idx: 0 }
        ));
        assert!(matches!(
            app.layout.rows[1],
            RowKind::HunkHeader {
                file_idx: 0,
                hunk_idx: 0
            }
        ));
        assert!(matches!(
            app.layout.rows[2],
            RowKind::DiffLine {
                file_idx: 0,
                hunk_idx: 0,
                line_idx: 0
            }
        ));
        assert!(matches!(app.layout.rows[3], RowKind::Spacer));
        assert_eq!(app.layout.hunk_starts, vec![1]);
        assert_eq!(app.layout.file_first_hunk, vec![Some(1)]);
    }

    #[test]
    fn build_layout_marks_binary_file_with_notice_row() {
        let app = fake_app(vec![binary_file("icon.png", 100)]);
        // FileHeader, BinaryNotice, Spacer
        assert_eq!(app.layout.rows.len(), 3);
        assert!(matches!(
            app.layout.rows[1],
            RowKind::BinaryNotice { file_idx: 0 }
        ));
        assert!(app.layout.hunk_starts.is_empty());
        assert_eq!(app.layout.file_first_hunk, vec![Some(1)]);
    }

    #[test]
    fn next_hunk_jumps_across_file_boundaries() {
        let app_files = vec![
            // a.rs: newest, 2 hunks
            make_file(
                "a.rs",
                vec![added_hunk(1, &["x"]), added_hunk(10, &["y"])],
                200,
            ),
            // b.rs: older, 1 hunk
            single_added_file("b.rs", "z", 100),
        ];
        let mut app = fake_app(app_files);
        // Three hunks total → three hunk_starts.
        assert_eq!(app.layout.hunk_starts.len(), 3);

        // Start at the very top.
        app.scroll_to(0);
        app.next_hunk();
        assert_eq!(app.scroll, app.layout.hunk_starts[0]);
        app.next_hunk();
        assert_eq!(app.scroll, app.layout.hunk_starts[1]);
        app.next_hunk();
        assert_eq!(app.scroll, app.layout.hunk_starts[2]);
        // Already past the last; stays put on the last.
        app.next_hunk();
        assert_eq!(app.scroll, app.layout.hunk_starts[2]);
    }

    #[test]
    fn prev_hunk_walks_backwards() {
        let app_files = vec![make_file(
            "a.rs",
            vec![added_hunk(1, &["x"]), added_hunk(10, &["y"])],
            100,
        )];
        let mut app = fake_app(app_files);
        let last_hunk = *app.layout.hunk_starts.last().unwrap();
        app.scroll_to(last_hunk);
        app.prev_hunk();
        assert_eq!(app.scroll, app.layout.hunk_starts[0]);
        // Already on the first; clamps.
        app.prev_hunk();
        assert_eq!(app.scroll, app.layout.hunk_starts[0]);
    }

    #[test]
    fn follow_target_row_is_last_hunk_header_of_newest_file() {
        // Follow mode should park on the **last hunk header** of
        // the newest file so the user sees the most recent @@
        // context and the diff body below it.
        let app = fake_app(vec![
            single_added_file("older.rs", "c", 100),
            make_file(
                "newest.rs",
                vec![added_hunk(1, &["a"]), added_hunk(20, &["b"])],
                300,
            ),
        ]);
        assert!(
            matches!(app.layout.rows[app.scroll], RowKind::HunkHeader { .. }),
            "follow target must be a HunkHeader row, got {:?}",
            app.layout.rows[app.scroll]
        );
        // Should be the LAST hunk header (hunk at line 20), not the first.
        let newest_idx = app.files.len() - 1;
        let last_hunk_header = app
            .layout
            .rows
            .iter()
            .enumerate()
            .rev()
            .find_map(|(i, r)| match r {
                RowKind::HunkHeader { file_idx, .. } if *file_idx == newest_idx => Some(i),
                _ => None,
            })
            .expect("newest file must have a HunkHeader");
        assert_eq!(app.scroll, last_hunk_header);
    }

    #[test]
    fn follow_target_row_lands_on_hunk_header_even_for_tall_hunk() {
        // Even with a 20-line hunk, follow parks on the hunk header
        // so the user sees the @@ context and diff body from the top.
        let huge_hunk = hunk(1, numbered_added_lines(20));
        let app = app_with_hunks("big.rs", vec![huge_hunk], 500);
        assert!(
            matches!(app.layout.rows[app.scroll], RowKind::HunkHeader { .. }),
            "follow should land on HunkHeader, got {:?}",
            app.layout.rows[app.scroll]
        );
    }

    #[test]
    fn current_file_path_reports_the_file_under_the_cursor() {
        let mut app = fake_app(vec![
            single_added_file("a.rs", "x", 200),
            single_added_file("b.rs", "y", 100),
        ]);
        // a.rs has the larger mtime → it sorts to the bottom of the
        // layout, and bootstrap follow lands on it.
        assert_eq!(app.current_file_path(), Some(Path::new("a.rs")));

        // Jump to b.rs's first hunk by looking it up by path so the test
        // doesn't rely on a specific index.
        let b = file_idx(&app, "b.rs");
        app.jump_to_file_first_hunk(b);
        assert_eq!(app.current_file_path(), Some(Path::new("b.rs")));
    }

    #[test]
    fn handle_key_j_walks_changes_and_disables_follow() {
        // Lowercase `j` is run-forward: from the file header it jumps
        // to the first hunk's header (no hunk to look inside yet),
        // then to the Added line within that hunk (the run), then to
        // the next hunk. Repeatedly pressing `j` eventually crosses
        // into the next hunk even without a "short hunk shortcut".
        let mut app = app_with_hunks(
            "a.rs",
            vec![added_hunk(1, &["x"]), added_hunk(20, &["y"])],
            100,
        );
        app.scroll_to(0);
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.scroll, app.layout.hunk_starts[0]);
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.scroll, app.layout.hunk_starts[0] + 1, "walks to run");
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.scroll, app.layout.hunk_starts[1]);
        assert!(!app.follow_mode);
    }

    #[test]
    fn l_jumps_to_next_hunk_header() {
        // v0.2 remap: `l` takes over the strict hunk-jump role the old
        // SHIFT-J used to play. Two short hunks; pressing `l` from the
        // first lands on the second; pressing `l` again stays put
        // because there is no third hunk.
        let mut app = app_with_hunks(
            "a.rs",
            vec![added_hunk(1, &["alpha"]), added_hunk(10, &["beta"])],
            100,
        );
        assert_eq!(app.layout.hunk_starts.len(), 2);
        let first_hunk = app.layout.hunk_starts[0];
        let second_hunk = app.layout.hunk_starts[1];

        app.scroll_to(first_hunk);
        app.handle_key(key(KeyCode::Char('l')));
        assert_eq!(app.scroll, second_hunk);
        assert!(!app.follow_mode);

        // No more hunks after this one → stay put.
        app.handle_key(key(KeyCode::Char('l')));
        assert_eq!(app.scroll, second_hunk);
    }

    #[test]
    fn lowercase_j_at_last_row_of_only_hunk_stays_put() {
        // Cursor parked on the bottom-most row of a long hunk. There is
        // no next hunk to walk into, so pressing `j` must be a no-op
        // instead of snapping back up to the hunk's header row — the
        // old `next_hunk` fallback to `hunk_starts.last()` made the
        // cursor leap backward, which is the opposite of what `j`
        // should mean.
        let lines = numbered_added_lines(20);
        let mut app = fake_app(vec![single_hunk_file("a.rs", lines, 100)]);
        app.last_body_height.set(15);
        let (_start, end) = app.layout.change_runs[0];
        let last = end - 1;

        app.scroll_to(last);
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(
            app.scroll, last,
            "j at the bottom of the only change run must stay put"
        );
    }

    #[test]
    fn lowercase_j_in_long_run_chunk_scrolls_through_body() {
        // Long run (20 Added lines, viewport = 15). `j` must not
        // teleport past the run body — it chunk-scrolls through it so
        // the user actually sees the change. Once the cursor reaches
        // the run's last row, the next `j` hands off to the straight
        // hunk-cross path (no trailing-context dwell).
        let lines = numbered_added_lines(20);
        let mut app = fake_app(vec![single_hunk_file("a.rs", lines, 100)]);
        app.last_body_height.set(15);
        let chunk = app.chunk_size();
        assert_eq!(chunk, 5, "viewport=15 → chunk=5");
        let (start, end) = app.layout.change_runs[0];
        let last = end - 1;

        app.scroll_to(start);
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.scroll, start + chunk, "first `j`: chunk forward");

        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.scroll, start + 2 * chunk);

        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.scroll, start + 3 * chunk);

        // Fourth press clamps to run_end - 1 (4 * 5 = 20 > 19 span).
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.scroll, last, "clamps to last row of run");

        // Fifth press: at run end, no next run, no next hunk → stay put.
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.scroll, last);
    }

    #[test]
    fn l_crosses_hunk_and_file_boundaries() {
        // v0.2 remap: `l` walks to the next hunk regardless of the
        // file boundary between them. One tiny hunk per file so the
        // jump has to cross from a.rs into b.rs.
        let mut app = fake_app(vec![
            single_added_file("a.rs", "alpha", 100),
            single_added_file("b.rs", "beta", 200),
        ]);
        assert_eq!(app.layout.hunk_starts.len(), 2);
        let first_hunk = app.layout.hunk_starts[0];
        let second_hunk = app.layout.hunk_starts[1];

        app.scroll_to(first_hunk);
        app.handle_key(key(KeyCode::Char('l')));
        assert_eq!(
            app.scroll, second_hunk,
            "`l` on a short hunk must cross hunk + file boundaries"
        );
    }

    #[test]
    fn lowercase_k_in_long_run_chunk_scrolls_back_through_body() {
        // Backward mirror of `lowercase_j_in_long_run_chunk_scrolls_through_body`.
        // A 20-line run with viewport = 15 gives chunk = 5. `k` from
        // the tail chunk-walks back until the cursor reaches the run
        // start; the next `k` falls through to the hunk header
        // (v0.4: hunk headers are landing targets). With no prev
        // hunk, a further `k` stays put.
        let lines = numbered_added_lines(20);
        let mut app = fake_app(vec![single_hunk_file("a.rs", lines, 100)]);
        app.last_body_height.set(15);
        let chunk = app.chunk_size();
        let (run_start, run_end) = app.layout.change_runs[0];
        let last = run_end - 1;
        let hunk_top = app.layout.hunk_starts[0];

        app.scroll_to(last);
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(app.scroll, last - chunk, "first `k`: chunk back");

        app.handle_key(key(KeyCode::Char('k')));
        app.handle_key(key(KeyCode::Char('k')));
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(
            app.scroll, run_start,
            "keeps chunking back; magnet snaps to run_start once it's in range"
        );

        // v0.4: from run_start, next `k` falls through to this
        // hunk's HunkHeader first (header is now a landing target).
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(
            app.scroll, hunk_top,
            "v0.4: `k` from run_start falls through to the hunk header"
        );

        // From the HunkHeader with no prev hunk, `k` is a no-op.
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(app.scroll, hunk_top, "no prev hunk → stay put");
    }

    #[test]
    fn lowercase_j_teleports_between_runs_never_landing_on_context() {
        // Hunk with three separate change runs. `j` teleports straight
        // from run to run — never parks the cursor on an intermediate
        // Context row. ScrollAnim (not tested here) supplies the
        // visual "gradual" feel via its 150ms tween.
        let lines: Vec<DiffLine> = vec![
            diff_line(LineKind::Added, "r1-a1"),   // run 1 start
            diff_line(LineKind::Deleted, "r1-d1"), // still run 1
            diff_line(LineKind::Context, "ctx1"),
            diff_line(LineKind::Added, "r2-a1"), // run 2 start
            diff_line(LineKind::Context, "ctx2"),
            diff_line(LineKind::Context, "ctx3"),
            diff_line(LineKind::Context, "ctx4"),
            diff_line(LineKind::Context, "ctx5"),
            diff_line(LineKind::Context, "ctx6"),
            diff_line(LineKind::Context, "ctx7"),
            diff_line(LineKind::Context, "ctx8"),
            diff_line(LineKind::Context, "ctx9"),
            diff_line(LineKind::Context, "ctx10"),
            diff_line(LineKind::Added, "r3-a1"), // run 3 start
            diff_line(LineKind::Context, "ctx11"),
        ];
        let mut app = fake_app(vec![single_hunk_file("a.rs", lines, 100)]);
        app.last_body_height.set(10);
        let runs: Vec<_> = app.layout.change_runs.clone();
        assert_eq!(runs.len(), 3);
        let hunk_top = app.layout.hunk_starts[0];

        app.scroll_to(hunk_top);
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.scroll, runs[0].0, "1st `j`: run 1 start");

        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.scroll, runs[1].0, "2nd `j`: run 2 start");

        // Run 3 is 10 rows past run 2; `j` still teleports straight to
        // it, skipping all intervening Context rows.
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(
            app.scroll, runs[2].0,
            "3rd `j`: teleports across context directly to run 3"
        );
    }

    #[test]
    fn lowercase_j_walks_runs_even_in_short_hunk() {
        // Even when the whole hunk fits on screen (viewport = 40, hunk
        // is ~4 rows), `j` still walks change runs — short hunks have
        // no special skip. This keeps the press count consistent with
        // what the user sees needs reviewing: 2 runs = 2 landings, not
        // "the hunk's small so let's just skip".
        let mut app = fake_app(vec![
            make_file(
                "a.rs",
                vec![hunk(
                    1,
                    vec![
                        diff_line(LineKind::Added, "a1"),
                        diff_line(LineKind::Context, "c1"),
                        diff_line(LineKind::Added, "a2"),
                    ],
                )],
                100,
            ),
            single_added_file("b.rs", "b1", 200),
        ]);
        app.last_body_height.set(40);
        let runs = app.layout.change_runs.clone();
        let second_hunk_top = app.layout.hunk_starts[1];

        app.scroll_to(app.layout.hunk_starts[0]);
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.scroll, runs[0].0, "1st `j`: run 1 start");

        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.scroll, runs[1].0, "2nd `j`: run 2 start (same hunk)");

        // 3rd press: no more runs in hunk a → crosses to hunk b's
        // header. The run inside hunk b is reached on the 4th press.
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.scroll, second_hunk_top, "3rd `j`: next hunk header");

        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.scroll, runs[2].0, "4th `j`: run in hunk 2");
    }

    #[test]
    fn lowercase_k_at_hunk_top_lands_on_prev_hunk_last_run_start() {
        // v0.4 unified navigation: from hunk2's header `k` lands on
        // the nearest backward candidate (run start ∪ hunk header).
        // In this fixture the prev hunk's only run starts at row 2
        // (closer to HH1 than HH0 at row 1), so we land on the run
        // start first. A second `k` then steps back to HH0.
        let mut app = app_with_hunks(
            "a.rs",
            vec![added_hunk(1, &["a1", "a2", "a3"]), added_hunk(10, &["b1"])],
            100,
        );
        let first_hunk_top = app.layout.hunk_starts[0];
        let second_hunk_top = app.layout.hunk_starts[1];
        let (first_hunk_last_run_start, _) = app.layout.change_runs[0];

        app.scroll_to(second_hunk_top);
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(
            app.scroll, first_hunk_last_run_start,
            "v0.4: first `k` lands on the nearest backward candidate (run start)"
        );
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(
            app.scroll, first_hunk_top,
            "v0.4: second `k` steps to the prev hunk's header"
        );
    }

    #[test]
    fn lowercase_k_skips_prev_hunk_trailing_context() {
        // v0.4: with hunk1's change followed by 5 trailing context
        // rows, `k` from hunk2's header lands on the change (run
        // start), then a second `k` steps onto hunk1's header. The
        // trailing context is implicitly skipped because it never
        // appears as a landing candidate.
        let mut lines_a: Vec<DiffLine> = vec![diff_line(LineKind::Added, "change")];
        for _ in 0..5 {
            lines_a.push(diff_line(LineKind::Context, "tail"));
        }
        let mut app = app_with_hunks("a.rs", vec![hunk(1, lines_a), added_hunk(20, &["b1"])], 100);
        let first_hunk_top = app.layout.hunk_starts[0];
        let second_hunk_top = app.layout.hunk_starts[1];
        let (first_run_start, _) = app.layout.change_runs[0];

        app.scroll_to(second_hunk_top);
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(
            app.scroll, first_run_start,
            "v0.4: first `k` lands on the prev hunk's run start"
        );
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(
            app.scroll, first_hunk_top,
            "v0.4: second `k` steps to the prev hunk's header"
        );
    }

    #[test]
    fn lowercase_j_skips_trailing_context_and_crosses_to_next_hunk() {
        // Hunk with a single change run followed by trailing context.
        // `j` from the run's sole row crosses straight into the next
        // hunk (ScrollAnim tweens the motion) — never stops on any of
        // the Context rows between.
        let mut lines: Vec<DiffLine> = vec![];
        for _ in 0..4 {
            lines.push(diff_line(LineKind::Context, "lead"));
        }
        lines.push(diff_line(LineKind::Added, "change"));
        for _ in 0..4 {
            lines.push(diff_line(LineKind::Context, "trail"));
        }
        let mut app = fake_app(vec![
            single_hunk_file("a.rs", lines, 100),
            single_added_file("b.rs", "x", 200),
        ]);
        app.last_body_height.set(10);
        let (run_start, _) = app.layout.change_runs[0];

        app.scroll_to(run_start);
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(
            app.scroll, app.layout.hunk_starts[1],
            "`j` skips trailing context and crosses directly to next hunk header"
        );
    }

    #[test]
    fn h_jumps_to_previous_hunk_header() {
        // v0.2 remap: `h` is the strict previous-hunk jump that the
        // old SHIFT-K used to do. Two short hunks, cursor on the
        // second — pressing `h` lands on the first hunk header.
        let mut app = app_with_hunks(
            "a.rs",
            vec![added_hunk(1, &["alpha"]), added_hunk(10, &["beta"])],
            100,
        );
        let first_hunk = app.layout.hunk_starts[0];
        let second_hunk = app.layout.hunk_starts[1];

        app.scroll_to(second_hunk);
        app.handle_key(key(KeyCode::Char('h')));
        assert_eq!(app.scroll, first_hunk);
    }

    #[test]
    fn shift_j_moves_cursor_down_by_exactly_one_visual_row() {
        // v0.2 remap: `J` is a one-row forward cursor move, not a
        // hunk jump. Starting at the file header row, `J` walks one
        // row at a time (header → hunk header → first diff line).
        let mut app = added_hunk_app("a.rs", 1, &["one", "two", "three"], 100);
        app.scroll_to(0);
        let before = app.scroll;
        app.handle_key(key(KeyCode::Char('J')));
        assert_eq!(app.scroll, before + 1);
        app.handle_key(key(KeyCode::Char('J')));
        assert_eq!(app.scroll, before + 2);
        assert!(!app.follow_mode);
    }

    #[test]
    fn shift_k_moves_cursor_up_by_exactly_one_visual_row() {
        // v0.2 remap: `K` is a one-row backward cursor move.
        let mut app = added_hunk_app("a.rs", 1, &["one", "two", "three"], 100);
        app.scroll_to(3);
        app.handle_key(key(KeyCode::Char('K')));
        assert_eq!(app.scroll, 2);
        app.handle_key(key(KeyCode::Char('K')));
        assert_eq!(app.scroll, 1);
        assert!(!app.follow_mode);
    }

    #[test]
    fn l_flows_from_end_of_long_hunk_into_next_hunk_header() {
        // Even from the last row of a long hunk, `l` jumps to the
        // next hunk's header. This mirrors the old SHIFT-J "flow
        // across boundary" behavior but now lives on `l`.
        let lines = numbered_added_lines(20);
        let mut app = app_with_hunks(
            "a.rs",
            vec![hunk(1, lines), added_hunk(100, &["tail"])],
            100,
        );
        app.last_body_height.set(15);
        let second_hunk = app.layout.hunk_starts[1];

        // Park on the last row of the long hunk (row 21: 1 header + 20
        // diff lines starting at row 1).
        app.scroll_to(21);
        // `l` from there must leap into the next hunk's header.
        app.handle_key(key(KeyCode::Char('l')));
        assert_eq!(app.scroll, second_hunk);
    }

    #[test]
    fn viewport_top_centers_short_hunk_inside_viewport() {
        // Layout shape (after mtime-ascending sort):
        //   0  FileHeader  before.rs
        //   1  HunkHeader
        //   2..5 four context lines
        //   6  Spacer
        //   7  FileHeader  target.rs   (← cursor will park here)
        //   8  HunkHeader
        //   9  +alpha
        //  10  +beta
        //  11  Spacer
        //  12 FileHeader  after.rs     (lots of trailing space so we
        //  13  HunkHeader               aren't clamped against max_top)
        //  14..17 four context lines
        //  18  Spacer
        // Total = 19 rows. Viewport = 9. max_top = 10.
        // Hunk spans rows [8, 11) → size 3.
        // Centring 3 rows in a 9-row viewport means
        // viewport_top = 8 - (9 - 3)/2 = 8 - 3 = 5.
        let mut app = fake_app(vec![
            context_hunk_file("before.rs", 1, &[" a", " b", " c", " d"], 100),
            added_hunk_file("target.rs", 1, &["alpha", "beta"], 200),
            context_hunk_file("after.rs", 1, &[" a", " b", " c", " d"], 300),
        ]);
        // Park the cursor on target.rs's hunk header.
        let target_hunk_row = app.layout.hunk_starts[1];
        app.scroll_to(target_hunk_row);
        let (hunk_top, hunk_end) = app.current_hunk_range().unwrap();
        assert_eq!(hunk_end - hunk_top, 3);

        let viewport = app.viewport_top(9);
        assert_eq!(
            viewport, 5,
            "expected the 3-row hunk centred at viewport_top = 5 in a 9-row viewport"
        );
    }

    #[test]
    fn viewport_top_falls_back_to_cursor_centered_for_long_hunks() {
        // Single long hunk, much taller than the viewport: should fall
        // back to centring the cursor row instead of trying to centre
        // the whole hunk.
        let lines = numbered_added_lines(40);
        let mut app = fake_app(vec![single_hunk_file("a.rs", lines, 100)]);
        let header = app.layout.hunk_starts[0];
        // Park well inside the long hunk.
        app.scroll_to(header + 20);

        let height = 12;
        let viewport = app.viewport_top(height);
        // For the long-hunk fall-through, viewport_top = cursor - height/2.
        assert_eq!(viewport, (header + 20) - height / 2);
    }

    #[test]
    fn top_mode_anchors_short_hunk_to_viewport_ceiling() {
        // Cursor on a short hunk's header in Top mode pins the hunk's
        // *first* row (its header) to the viewport ceiling so the body
        // flows downward into the rest of the viewport.
        //
        // Layout (mtime-ascending sort):
        //   0  FileHeader  before.rs
        //   1  HunkHeader
        //   2..5 four context lines
        //   6  Spacer
        //   7  FileHeader  target.rs
        //   8  HunkHeader        ← cursor parks here
        //   9  +alpha
        //  10  +beta
        //  11  Spacer
        //  12 FileHeader  after.rs
        //  13  HunkHeader
        //  14..17 four context lines
        //  18  Spacer
        // Total = 19 rows. Viewport = 9. max_top = 10.
        // target hunk spans [8, 11) → size 3.
        // Top mode pins hunk_top (8) to the viewport ceiling, so
        // viewport_top = 8.
        let mut app = fake_app(vec![
            context_hunk_file("before.rs", 1, &[" a", " b", " c", " d"], 100),
            added_hunk_file("target.rs", 1, &["alpha", "beta"], 200),
            context_hunk_file("after.rs", 1, &[" a", " b", " c", " d"], 300),
        ]);
        app.cursor_placement = CursorPlacement::Top;
        let target_hunk_row = app.layout.hunk_starts[1];
        app.scroll_to(target_hunk_row);
        let (hunk_top, hunk_end) = app.current_hunk_range().unwrap();
        assert_eq!((hunk_top, hunk_end), (8, 11));

        let viewport = app.viewport_top(9);
        assert_eq!(
            viewport, 8,
            "top mode should anchor hunk_top to the viewport ceiling"
        );
    }

    #[test]
    fn top_mode_long_hunk_still_pins_cursor_row() {
        // When hunk_size > viewport, Top mode falls back to pinning
        // the cursor row itself to the ceiling so J/K chunk scroll
        // keeps working.
        let lines = numbered_added_lines(40);
        let mut app = fake_app(vec![single_hunk_file("a.rs", lines, 100)]);
        app.cursor_placement = CursorPlacement::Top;
        let header = app.layout.hunk_starts[0];
        app.scroll_to(header + 20);

        let height = 12;
        // Long-hunk fall-through: viewport_top = cursor (cursor at row 0).
        let viewport = app.viewport_top(height);
        assert_eq!(viewport, header + 20);
    }

    #[test]
    fn viewport_top_clamps_short_hunk_centring_against_layout_edges() {
        // A short hunk near the very start of the layout: padding above
        // would push viewport_top below 0 → clamp at 0.
        let mut app = added_hunk_app("a.rs", 1, &["alpha", "beta"], 100);
        let hunk_row = app.layout.hunk_starts[0];
        app.scroll_to(hunk_row);

        // 12-row viewport, but hunk starts at row 1 (after the file
        // header). hunk_top - pad would be negative; clamped to 0.
        let viewport = app.viewport_top(12);
        assert_eq!(viewport, 0);
    }

    #[test]
    fn chunk_size_scales_with_last_body_height() {
        let app = fake_app(vec![]);
        // Default body height is 24 → chunk = 24/3 = 8.
        assert_eq!(app.chunk_size(), 8);

        // A taller pane should yield a bigger chunk.
        app.last_body_height.set(36);
        assert_eq!(app.chunk_size(), 12);

        // A tiny pane should never go below 1 row.
        app.last_body_height.set(2);
        assert_eq!(app.chunk_size(), 1);

        // Zero height (degenerate) still gives at least 1.
        app.last_body_height.set(0);
        assert_eq!(app.chunk_size(), 1);
    }

    #[test]
    fn cursor_placement_centered_keeps_cursor_in_the_middle() {
        // 100 row layout, viewport 20 rows, cursor at row 50.
        // Centered → viewport_top = 50 - 10 = 40, cursor visually at row 10.
        let placement = CursorPlacement::Centered;
        assert_eq!(placement.viewport_top(50, 100, 20), 40);
    }

    #[test]
    fn cursor_placement_centered_clamps_at_top_and_bottom() {
        let placement = CursorPlacement::Centered;
        // Cursor near the start: viewport_top can't go below 0.
        assert_eq!(placement.viewport_top(2, 100, 20), 0);
        // Cursor near the end: viewport_top clamped at total - height.
        assert_eq!(placement.viewport_top(99, 100, 20), 80);
    }

    #[test]
    fn cursor_placement_top_pins_cursor_to_the_ceiling() {
        // Cursor at row 50, viewport 20: cursor visually at row 0
        // (top of viewport), viewport_top = 50.
        let placement = CursorPlacement::Top;
        assert_eq!(placement.viewport_top(50, 100, 20), 50);
    }

    #[test]
    fn cursor_placement_top_clamps_against_max_top() {
        // Cursor near the end of the layout: Top mode would push
        // viewport_top past max_top, so it clamps.
        let placement = CursorPlacement::Top;
        assert_eq!(placement.viewport_top(95, 100, 20), 80);
    }

    #[test]
    fn cursor_placement_returns_zero_when_layout_fits_in_viewport() {
        // 5 rows, viewport 20 → no scrolling possible regardless of mode.
        assert_eq!(CursorPlacement::Centered.viewport_top(3, 5, 20), 0);
        assert_eq!(CursorPlacement::Top.viewport_top(3, 5, 20), 0);
    }

    #[test]
    fn z_key_toggles_cursor_placement() {
        let mut app = fake_app(vec![]);
        assert_eq!(app.cursor_placement, CursorPlacement::Centered);
        app.handle_key(key(KeyCode::Char('z')));
        assert_eq!(app.cursor_placement, CursorPlacement::Top);
        app.handle_key(key(KeyCode::Char('z')));
        assert_eq!(app.cursor_placement, CursorPlacement::Centered);
    }

    #[test]
    fn change_runs_collapse_consecutive_same_kind_lines_into_one_entry() {
        // Three contiguous +/- lines should be a single change run, not three.
        let app = single_hunk_app(
            "a.rs",
            1,
            vec![
                diff_line(LineKind::Added, "a"),
                diff_line(LineKind::Added, "b"),
                diff_line(LineKind::Deleted, "c"),
            ],
            100,
        );
        assert_eq!(
            app.layout.change_runs.len(),
            1,
            "expected one change run for an all-contiguous +/- block"
        );
        let (start, end) = app.layout.change_runs[0];
        assert_eq!(end - start, 3);
    }

    #[test]
    fn w_key_toggles_wrap_lines() {
        let mut app = fake_app(vec![]);
        assert!(!app.wrap_lines);
        app.handle_key(key(KeyCode::Char('w')));
        assert!(app.wrap_lines);
        app.handle_key(key(KeyCode::Char('w')));
        assert!(!app.wrap_lines);
    }

    // ---- line numbers toggle (v0.5) -----------------------------------

    #[test]
    fn toggle_line_numbers_defaults_off_and_round_trips() {
        // v0.5 plan: default is OFF so v0.4 layout stays stable for
        // users who don't opt in.
        let mut app = fake_app(vec![]);
        assert!(!app.show_line_numbers);
        app.toggle_line_numbers();
        assert!(app.show_line_numbers);
        app.toggle_line_numbers();
        assert!(!app.show_line_numbers);
    }

    #[test]
    fn pound_key_toggles_line_numbers_in_diff_view() {
        let mut app = fake_app(vec![]);
        app.handle_key(key(KeyCode::Char('#')));
        assert!(app.show_line_numbers);
        app.handle_key(key(KeyCode::Char('#')));
        assert!(!app.show_line_numbers);
    }

    #[test]
    fn build_layout_populates_diff_line_numbers_parallel_to_rows() {
        // v0.5 plan Decision Log: diff_line_numbers is a Vec parallel
        // to `rows`. RowKind::DiffLine positions carry Some((old, new));
        // all other rows (FileHeader, HunkHeader, Spacer, BinaryNotice)
        // carry None.
        let mut app = added_hunk_app("foo.rs", 10, &["a", "b"], 100);
        app.build_layout();
        let ln = &app.layout.diff_line_numbers;
        assert_eq!(
            ln.len(),
            app.layout.rows.len(),
            "diff_line_numbers must run parallel to rows"
        );
        for (i, row) in app.layout.rows.iter().enumerate() {
            match row {
                RowKind::DiffLine { .. } => assert!(
                    ln[i].is_some(),
                    "DiffLine row {i} must have a cached line-number pair"
                ),
                _ => assert!(ln[i].is_none(), "non-DiffLine row {i} must have None"),
            }
        }
    }

    #[test]
    fn build_layout_caches_correct_line_numbers_for_added_rows() {
        // hunk(10, [Added a, Added b]) under the fixture above uses
        // old_start=10, new_start=10, old_count=0, new_count=2.
        // Added #1 → (None, Some(10)); Added #2 → (None, Some(11)).
        let mut app = added_hunk_app("foo.rs", 10, &["a", "b"], 100);
        app.build_layout();
        let diff_rows: Vec<_> = app
            .layout
            .rows
            .iter()
            .enumerate()
            .filter_map(|(i, row)| matches!(row, RowKind::DiffLine { .. }).then_some(i))
            .collect();
        assert_eq!(diff_rows.len(), 2);
        assert_eq!(
            app.layout.diff_line_numbers[diff_rows[0]],
            Some((None, Some(10)))
        );
        assert_eq!(
            app.layout.diff_line_numbers[diff_rows[1]],
            Some((None, Some(11)))
        );
    }

    #[test]
    fn build_layout_max_line_number_covers_visible_rows() {
        // Single hunk with 3 rows starting at new_start=100 →
        // max_line_number must be >= 102.
        let mut app = added_hunk_app("foo.rs", 100, &["a", "b", "c"], 100);
        app.build_layout();
        assert_eq!(
            app.layout.max_line_number, 102,
            "max should be the largest line number actually rendered"
        );
    }

    #[test]
    fn build_layout_max_line_number_has_lower_bound_of_10() {
        // Tiny file with line numbers 1-3 should still clamp to 10 so
        // the gutter width stays at a stable minimum of 2 digits.
        let mut app = single_added_app("foo.rs", "a");
        app.build_layout();
        assert!(
            app.layout.max_line_number >= 10,
            "got {}, expected lower bound 10",
            app.layout.max_line_number
        );
    }

    #[test]
    fn build_layout_max_line_number_excludes_seen_hunks() {
        // v0.5 plan §Important-1 (Codex review): a seen (collapsed)
        // hunk contributes no DiffLine rows, so its line numbers must
        // not widen the gutter. Put the big-number hunk behind a seen
        // mark and assert max stays bounded by the visible hunk.
        let hunk1 = hunk(
            10,
            vec![
                diff_line(LineKind::Added, "a"),
                diff_line(LineKind::Added, "b"),
            ],
        );
        let hunk2 = added_hunk(5000, &["z"]);
        let mut app = app_with_hunks("foo.rs", vec![hunk1, hunk2.clone()], 100);
        // Mark hunk2 seen so it collapses out of the layout.
        let path = app.files[0].path.clone();
        let fp = hunk_fingerprint(&hunk2);
        app.seen_hunks.insert((path, 5000), fp);

        app.build_layout();
        assert!(
            app.layout.max_line_number < 1000,
            "seen hunk must not contribute to max; got {}",
            app.layout.max_line_number
        );
    }

    #[test]
    fn toggle_line_numbers_triggers_layout_rebuild() {
        // v0.5 plan Decision Log: `toggle_line_numbers` must rebuild
        // the layout so max_line_number and the parallel number cache
        // stay coherent with `show_line_numbers` (Phase CD).
        let mut app = fake_app(vec![single_added_hunk_file("foo.rs", 10, "a", 100)]);
        // Baseline: build_layout populated the cache.
        assert!(!app.layout.diff_line_numbers.is_empty());
        // Corrupt the layout so we can detect a rebuild on toggle.
        app.layout.diff_line_numbers.clear();
        app.layout.max_line_number = 0;
        app.toggle_line_numbers();
        assert!(
            !app.layout.diff_line_numbers.is_empty(),
            "toggle must rebuild the parallel number cache"
        );
        assert!(
            app.layout.max_line_number >= 10,
            "toggle must recompute max_line_number"
        );
    }

    #[test]
    fn pound_key_toggles_line_numbers_in_file_view() {
        // v0.5 plan §Step 4. `#` must also work inside the file view
        // (which has its own KeyCode::Char dispatch block).
        let tmp = tempfile::tempdir().expect("tmp");
        let (mut app, _abs) = revert_app_with_real_repo(
            &tmp,
            "foo.rs",
            "fn one() {}\n",
            "fn one() {}\nfn two() {}\n",
        );
        cursor_on_nth_diff_line(&mut app, 0);
        app.handle_key(key(KeyCode::Enter));
        assert!(app.file_view.is_some(), "precondition: in file view");
        assert!(!app.show_line_numbers);
        app.handle_key(key(KeyCode::Char('#')));
        assert!(
            app.show_line_numbers,
            "# must toggle line numbers from the file-view dispatch"
        );
    }

    #[test]
    fn handle_key_g_and_capital_g_move_to_top_and_bottom() {
        let mut app = added_hunk_app("a.rs", 1, &["x", "y", "z"], 100);
        app.handle_key(key(KeyCode::Char('G')));
        assert_eq!(app.scroll, app.layout.rows.len() - 2);
        assert!(
            !matches!(app.layout.rows[app.scroll], RowKind::Spacer),
            "G must land on the last content row, not the trailing spacer"
        );
        app.handle_key(key(KeyCode::Char('g')));
        assert_eq!(app.scroll, 0);
    }

    #[test]
    fn scroll_to_does_not_land_on_spacer_rows() {
        let mut app = fake_app(vec![
            single_added_file("a.rs", "x", 100),
            single_added_file("b.rs", "y", 200),
        ]);

        let spacer = app
            .layout
            .rows
            .iter()
            .position(|row| matches!(row, RowKind::Spacer))
            .expect("layout has spacer");
        app.scroll_to(spacer);

        assert!(
            !matches!(app.layout.rows[app.scroll], RowKind::Spacer),
            "scroll_to must normalize spacer targets to real content rows"
        );
    }

    #[test]
    fn scroll_by_skips_spacer_rows_in_nowrap_mode() {
        let mut app = fake_app(vec![
            single_added_file("a.rs", "x", 100),
            single_added_file("b.rs", "y", 200),
        ]);
        app.follow_mode = false;

        let first_file_last_diff = app
            .layout
            .rows
            .iter()
            .enumerate()
            .find_map(|(idx, row)| {
                matches!(row, RowKind::DiffLine { file_idx: 0, .. }).then_some(idx)
            })
            .expect("first file diff row");
        app.scroll = first_file_last_diff;

        // +1 would have landed on the inter-file spacer before the fix.
        app.scroll_by(1);
        assert!(
            !matches!(app.layout.rows[app.scroll], RowKind::Spacer),
            "scroll_by must skip cosmetic spacer rows"
        );
        assert_eq!(app.current_file_path(), Some(Path::new("b.rs")));
    }

    #[test]
    fn handle_key_f_restores_follow_mode_and_jumps_to_target() {
        let mut app = app_with_hunks(
            "a.rs",
            vec![added_hunk(1, &["x"]), added_hunk(20, &["y"])],
            100,
        );
        app.handle_key(key(KeyCode::Char('g'))); // jump to top, drops follow
        assert!(!app.follow_mode);
        app.handle_key(key(KeyCode::Char('f')));
        assert!(app.follow_mode);
        // Follow target = last HunkHeader of the newest file.
        assert!(matches!(
            app.layout.rows[app.scroll],
            RowKind::HunkHeader { .. }
        ));
    }

    #[test]
    fn handle_key_q_and_ctrl_c_quit_in_normal_mode() {
        let mut app = fake_app(vec![]);
        app.handle_key(key(KeyCode::Char('q')));
        assert!(app.should_quit);

        let mut app = fake_app(vec![]);
        app.handle_key(ctrl('c'));
        assert!(app.should_quit);
    }

    #[test]
    fn question_mark_opens_help_overlay_and_esc_closes_it() {
        let mut app = single_added_app("a.rs", "x");
        assert!(!app.help_overlay);

        app.handle_key(key(KeyCode::Char('?')));
        assert!(app.help_overlay);

        app.handle_key(key(KeyCode::Esc));
        assert!(!app.help_overlay);
    }

    #[test]
    fn help_overlay_shadows_normal_keys_until_closed() {
        let mut app = single_added_app("a.rs", "x");
        app.handle_key(key(KeyCode::Char('?')));
        app.handle_key(key(KeyCode::Char('s')));
        assert!(
            app.picker.is_none(),
            "help overlay must consume normal-mode action keys"
        );
        assert!(app.help_overlay);

        app.handle_key(key(KeyCode::Char('?')));
        assert!(!app.help_overlay);
    }

    #[test]
    fn s_opens_picker_and_esc_closes_it() {
        // v0.2 remap: picker trigger moved from `Space` to `s` so
        // `Space` is free for the scar "seen" mark (wired up in a
        // later M4 slice).
        let mut app = single_added_app("a.rs", "x");
        app.handle_key(key(KeyCode::Char('s')));
        assert!(app.picker.is_some());

        app.handle_key(key(KeyCode::Esc));
        assert!(app.picker.is_none());
    }

    #[test]
    fn space_toggles_seen_mark_on_current_hunk() {
        // M4 slice 6 + v0.4 collapse: Space flips the cursor's
        // enclosing hunk into and out of the "seen" set. Pure TUI
        // state — no file write, no picker. In v0.4 Space also
        // collapses the DiffLine rows and snaps the cursor to the
        // hunk header when its old row disappears from the layout.
        let mut app = single_added_app("a.rs", "x");
        cursor_on_nth_diff_line(&mut app, 0);

        app.handle_key(key(KeyCode::Char(' ')));

        assert!(
            app.hunk_is_seen(0, 0),
            "Space must toggle the current hunk into the seen set"
        );
        assert!(app.picker.is_none(), "Space must not open the picker");
        assert!(
            matches!(
                app.layout.rows.get(app.scroll),
                Some(RowKind::HunkHeader {
                    file_idx: 0,
                    hunk_idx: 0
                })
            ),
            "collapsing a seen hunk must land the cursor on its HunkHeader"
        );

        // Second press removes the mark and re-expands the hunk.
        app.handle_key(key(KeyCode::Char(' ')));
        assert!(
            !app.hunk_is_seen(0, 0),
            "a second Space must remove the seen mark"
        );
        let diff_rows = app
            .layout
            .rows
            .iter()
            .filter(|r| matches!(r, RowKind::DiffLine { .. }))
            .count();
        assert_eq!(diff_rows, 1, "unmarking must re-expand the hunk body");
    }

    #[test]
    fn space_on_file_header_row_is_noop() {
        let mut app = single_added_app("a.rs", "x");
        let header_row = file_header_row(&app);
        app.scroll_to(header_row);

        app.handle_key(key(KeyCode::Char(' ')));

        assert!(
            app.seen_hunks.is_empty(),
            "file-header Space must not add anything to seen_hunks"
        );
    }

    #[test]
    fn seen_mark_persists_when_hunk_fingerprint_unchanged() {
        // v0.4: seen_hunks is keyed by (path, hunk.old_start) and
        // valued by the hunk content fingerprint. A watcher-driven
        // recompute that rebuilds the FileDiff list without moving
        // the pre-image anchor **and** without altering the hunk's
        // lines must leave the mark in place.
        let mut app = fake_app(vec![single_added_hunk_file("a.rs", 42, "x", 100)]);
        cursor_on_nth_diff_line(&mut app, 0);
        app.handle_key(key(KeyCode::Char(' ')));
        assert!(app.hunk_is_seen(0, 0));

        // Rebuild an identical diff: same old_start, same lines.
        let fresh = vec![single_added_hunk_file("a.rs", 42, "x", 100)];
        app.apply_computed_files(fresh);

        assert!(
            app.hunk_is_seen(0, 0),
            "recompute with identical hunk fingerprint must preserve the seen mark"
        );
    }

    #[test]
    fn rebuild_layout_hides_difflines_for_seen_hunk() {
        // v0.4: marking a hunk as seen must collapse its DiffLine
        // rows out of the layout so only the hunk header survives.
        // The file header and spacer stay put.
        let mut app = added_hunk_app("a.rs", 1, &["x1", "x2", "x3"], 100);
        cursor_on_nth_diff_line(&mut app, 0);

        let diff_rows_before = app
            .layout
            .rows
            .iter()
            .filter(|r| matches!(r, RowKind::DiffLine { .. }))
            .count();
        assert_eq!(
            diff_rows_before, 3,
            "precondition: fixture should layout 3 DiffLine rows"
        );

        app.handle_key(key(KeyCode::Char(' ')));

        let diff_rows_after = app
            .layout
            .rows
            .iter()
            .filter(|r| matches!(r, RowKind::DiffLine { .. }))
            .count();
        assert_eq!(
            diff_rows_after, 0,
            "DiffLine rows must be absent for seen hunk"
        );

        let header_rows = app
            .layout
            .rows
            .iter()
            .filter(|r| matches!(r, RowKind::HunkHeader { .. }))
            .count();
        assert_eq!(
            header_rows, 1,
            "HunkHeader must remain present for the seen hunk"
        );
    }

    #[test]
    fn j_from_seen_hunk_header_lands_on_next_expanded_hunks_header() {
        // v0.4 mirror of the `k` case: from hunk0 (seen) the `j`
        // key must stop on hunk1's HunkHeader, not dive into
        // hunk1's first change-run start.
        let mut app = app_with_hunks(
            "a.rs",
            vec![added_hunk(1, &["a1"]), added_hunk(10, &["b1", "b2"])],
            100,
        );
        let hh0 = hunk_header_row(&app, 0);
        app.scroll_to(hh0);
        app.handle_key(key(KeyCode::Char(' ')));
        assert!(app.hunk_is_seen(0, 0));

        let hh0 = hunk_header_row(&app, 0);
        app.scroll_to(hh0);

        app.handle_key(key(KeyCode::Char('j')));

        assert_cursor_on_hunk_header(&app, 1, "j must land on hunk 1's HunkHeader");
    }

    #[test]
    fn k_from_seen_hunk_header_walks_through_prev_expanded_hunk() {
        // v0.4 unified navigation: from hunk1 (seen) `k` first
        // lands on hunk0's run start (nearest backward landing),
        // then a second `k` steps onto hunk0's header. Both stops
        // are reachable — earlier implementations that made `k`
        // skip straight to the prev hunk's header meant the
        // expanded hunk's content couldn't be visited on the way
        // back.
        let mut app = app_with_hunks(
            "a.rs",
            vec![added_hunk(1, &["a1", "a2"]), added_hunk(10, &["b1"])],
            100,
        );
        let hh1 = hunk_header_row(&app, 1);
        app.scroll_to(hh1);
        app.handle_key(key(KeyCode::Char(' ')));
        assert!(app.hunk_is_seen(0, 1));

        let hh1 = hunk_header_row(&app, 1);
        app.scroll_to(hh1);

        // First `k`: hunk0's run start (closer than HH0).
        app.handle_key(key(KeyCode::Char('k')));
        assert!(
            matches!(
                app.layout.rows.get(app.scroll),
                Some(RowKind::DiffLine {
                    file_idx: 0,
                    hunk_idx: 0,
                    ..
                })
            ),
            "k #1: expected hunk0 run start, got {:?}",
            app.layout.rows.get(app.scroll)
        );

        // Second `k`: hunk0's header.
        app.handle_key(key(KeyCode::Char('k')));
        assert_cursor_on_hunk_header(&app, 0, "k #2: expected hunk0 header");
    }

    #[test]
    fn j_walks_through_multiple_seen_hunks_one_by_one() {
        let mut app = app_with_hunks(
            "a.rs",
            vec![
                added_hunk(1, &["a"]),
                added_hunk(10, &["b"]),
                added_hunk(20, &["c"]),
            ],
            100,
        );
        for i in 0..3 {
            app.scroll_to(hunk_header_row(&app, i));
            app.handle_key(key(KeyCode::Char(' ')));
        }

        app.scroll_to(hunk_header_row(&app, 0));

        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(
            app.current_hunk(),
            Some((0, 1)),
            "j #1: must land on hunk 1, got {:?}",
            app.current_hunk()
        );

        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(
            app.current_hunk(),
            Some((0, 2)),
            "j #2: must land on hunk 2, got {:?}",
            app.current_hunk()
        );
    }

    #[test]
    fn k_walks_through_multiple_seen_hunks_one_by_one() {
        // v0.4 investigation: 3 consecutive seen hunks — the user's
        // report was "k doesn't stop on hunk headers". Press `k`
        // repeatedly from the last hunk's header and expect to
        // land on hunk1's header, then hunk0's header, then no-op.
        let mut app = app_with_hunks(
            "a.rs",
            vec![
                added_hunk(1, &["a"]),
                added_hunk(10, &["b"]),
                added_hunk(20, &["c"]),
            ],
            100,
        );
        // Seen all three.
        for i in 0..3 {
            app.scroll_to(hunk_header_row(&app, i));
            app.handle_key(key(KeyCode::Char(' ')));
        }
        assert!(app.hunk_is_seen(0, 0) && app.hunk_is_seen(0, 1) && app.hunk_is_seen(0, 2));

        // Cursor on hunk 2's HunkHeader.
        app.scroll_to(hunk_header_row(&app, 2));

        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(
            app.current_hunk(),
            Some((0, 1)),
            "k #1: must land on hunk 1, got {:?}",
            app.current_hunk()
        );

        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(
            app.current_hunk(),
            Some((0, 0)),
            "k #2: must land on hunk 0, got {:?}",
            app.current_hunk()
        );
    }

    #[test]
    fn k_walks_seen_to_expanded_via_hunk_headers() {
        // v0.4: with hunk0 seen and hunk1 expanded, `k` from hunk1's
        // first DiffLine lands on hunk1's own HunkHeader first
        // (header-as-landing), and the next `k` crosses to hunk0's
        // HunkHeader.
        let mut app = app_with_hunks(
            "a.rs",
            vec![added_hunk(1, &["a1"]), added_hunk(10, &["b1", "b2"])],
            100,
        );
        cursor_on_nth_diff_line(&mut app, 0);
        app.handle_key(key(KeyCode::Char(' ')));
        assert!(app.hunk_is_seen(0, 0));

        let hh1 = hunk_header_row(&app, 1);
        app.scroll_to(hh1 + 1);
        assert_eq!(app.current_hunk(), Some((0, 1)));

        app.handle_key(key(KeyCode::Char('k')));
        assert_cursor_on_hunk_header(&app, 1, "k #1: hunk1 HunkHeader first");

        app.handle_key(key(KeyCode::Char('k')));
        assert_cursor_on_hunk_header(&app, 0, "k #2: crosses to hunk0 HunkHeader");
    }

    #[test]
    fn collapsing_hunk_keeps_cursor_on_that_hunks_header_not_a_neighbor() {
        // v0.4 regression guard: when Space collapses a hunk with
        // several DiffLine rows, the old scroll index can coincide
        // with a *different* hunk's DiffLine once the layout
        // shortens. Naïvely trusting "rows[scroll] is a DiffLine
        // → cursor is fine" silently teleports the cursor to the
        // neighbor. The collapsed hunk's HunkHeader must win.
        let mut app = app_with_hunks(
            "a.rs",
            vec![added_hunk(1, &["a1", "a2", "a3"]), added_hunk(10, &["b1"])],
            100,
        );
        // Park the cursor on the 2nd DiffLine of hunk 0 (a2).
        cursor_on_nth_diff_line(&mut app, 1);
        assert_eq!(app.current_hunk(), Some((0, 0)));

        app.handle_key(key(KeyCode::Char(' ')));

        assert_eq!(
            app.current_hunk(),
            Some((0, 0)),
            "cursor must stay on the collapsed hunk, not drift into a neighbor"
        );
        assert!(
            matches!(
                app.layout.rows.get(app.scroll),
                Some(RowKind::HunkHeader {
                    file_idx: 0,
                    hunk_idx: 0
                })
            ),
            "cursor row must be hunk 0's HunkHeader, got {:?}",
            app.layout.rows.get(app.scroll)
        );
    }

    #[test]
    fn seen_mark_clears_when_hunk_content_changes() {
        // v0.4: the seen mark is bound to the hunk's content
        // fingerprint, not just its pre-image anchor. A recompute
        // that keeps `old_start` fixed but alters any line's
        // content must invalidate the mark so the reader is forced
        // to re-read the new diff.
        let mut app = fake_app(vec![single_added_hunk_file("a.rs", 42, "x", 100)]);
        cursor_on_nth_diff_line(&mut app, 0);
        app.handle_key(key(KeyCode::Char(' ')));
        assert!(app.hunk_is_seen(0, 0));

        let fresh = vec![single_added_hunk_file("a.rs", 42, "y", 100)];
        app.apply_computed_files(fresh);

        assert!(
            !app.hunk_is_seen(0, 0),
            "content change must auto-clear the seen mark"
        );
        let diff_rows = app
            .layout
            .rows
            .iter()
            .filter(|r| matches!(r, RowKind::DiffLine { .. }))
            .count();
        assert!(
            diff_rows >= 1,
            "DiffLine rows must be re-expanded when the mark clears"
        );
    }

    #[test]
    fn picker_filters_by_substring_case_insensitively() {
        let mut app = fake_app(vec![
            single_added_file("src/Auth.rs", "x", 300),
            single_added_file("src/handler.rs", "y", 200),
            single_added_file("tests/auth_test.rs", "z", 100),
        ]);
        app.open_picker();
        type_chars(&mut app, "auth");
        let results = app.picker_results();
        // src/Auth.rs and tests/auth_test.rs match; src/handler.rs does not.
        assert_eq!(results.len(), 2);
        let paths: Vec<_> = results.iter().map(|&i| app.files[i].path.clone()).collect();
        assert!(paths.contains(&PathBuf::from("src/Auth.rs")));
        assert!(paths.contains(&PathBuf::from("tests/auth_test.rs")));
    }

    #[test]
    fn picker_enter_jumps_to_selected_file_first_hunk_and_closes() {
        let mut app = fake_app(vec![
            single_added_file("newest.rs", "x", 300),
            single_added_hunk_file("older.rs", 50, "y", 100),
        ]);
        app.open_picker();
        // picker_results runs newest-first regardless of how files are
        // stored internally, so cursor 0 = newest.rs and cursor 1 = older.rs.
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Enter));

        assert!(app.picker.is_none());
        let older = file_idx(&app, "older.rs");
        let expected = app.layout.file_first_hunk[older].unwrap();
        assert_eq!(app.scroll, expected);
        assert_eq!(app.current_file_path(), Some(Path::new("older.rs")));
    }

    #[test]
    fn populate_mtimes_keeps_deleted_files_recent_so_follow_mode_lands_on_them() {
        // Regression for the Codex finding: a freshly-deleted file used
        // to fall to `UNIX_EPOCH` because `metadata()` failed, which
        // sorted it to the very **top** of the mtime-ascending layout
        // and pushed follow mode onto the newest surviving file. That
        // hid destructive actions in the exact moment they most needed
        // to be visible.
        //
        // Setup: one real file on disk with its mtime backdated into
        // the early 70s, plus one deleted file whose path does not
        // exist. After bootstrap the deleted file must sort **last**
        // (= newest) and follow mode must park on it.
        let tmp = tempfile::tempdir().expect("create tempdir");
        let kept_path = tmp.path().join("kept.rs");
        std::fs::write(&kept_path, "hi\n").expect("write kept");
        let ancient = SystemTime::UNIX_EPOCH + Duration::from_secs(60 * 60 * 24);
        let f = std::fs::File::options()
            .write(true)
            .open(&kept_path)
            .expect("reopen kept for mtime set");
        f.set_modified(ancient).expect("backdate kept.rs");

        let kept = single_added_file("kept.rs", "hi2", 0);
        let gone = single_deleted_file("gone.rs", "bye", 0);

        let app = App::bootstrap_with_diff(
            tmp.path().to_path_buf(),
            tmp.path().join(".git"),
            tmp.path().join(".git"),
            Some("refs/heads/main".into()),
            "abcdef1234567890abcdef1234567890abcdef12".into(),
            Ok(vec![kept, gone]),
        )
        .expect("bootstrap succeeds");

        assert_eq!(
            app.files.last().map(|f| f.path.as_path()),
            Some(Path::new("gone.rs")),
            "deleted file must land at the newest end of the mtime sort"
        );
        assert_eq!(
            app.current_file_path(),
            Some(Path::new("gone.rs")),
            "follow mode must keep the deletion on screen"
        );
    }

    #[test]
    fn apply_reset_preserves_old_state_when_diff_fails() {
        // Regression for the adversarial finding on `reset_baseline`:
        // the old implementation assigned `baseline_sha` and cleared
        // `head_dirty` BEFORE running `git diff`, so a transient diff
        // failure left the user staring at a stale `files` snapshot
        // under a silently-advanced baseline with no `HEAD*` warning
        // to signal that the reset never actually landed.
        //
        // `apply_reset` now takes the diff `Result` directly so we
        // can exercise the failure path without touching the
        // filesystem. Every piece of baseline-adjacent state must
        // survive a failed reset unchanged.
        let mut app = fake_app(vec![
            single_added_file("older.rs", "keep me", 100),
            single_added_hunk_file("newer.rs", 2, "also keep", 200),
        ]);
        let old_sha = app.baseline_sha.clone();
        let old_files = app.files.clone();
        let old_branch = app.current_branch_ref.clone();
        app.head_dirty = true;

        let effect = app.apply_reset(
            "feedfacefeedfacefeedfacefeedfacefeedface".into(),
            Some("refs/heads/feature".into()),
            Err(anyhow::anyhow!("simulated git diff failure")),
        );
        assert_eq!(
            effect,
            KeyEffect::None,
            "a failed reset must not ask the loop to reconfigure the watcher — \
             doing so would leave the watcher pointing at a branch the user \
             never actually reached"
        );

        assert_eq!(
            app.baseline_sha, old_sha,
            "baseline_sha must not advance when the post-reset diff fails"
        );
        assert_eq!(
            app.current_branch_ref, old_branch,
            "current_branch_ref must not advance when the post-reset diff fails"
        );
        assert!(
            app.head_dirty,
            "head_dirty must survive a failed reset so the HEAD* warning stays visible"
        );
        assert_eq!(
            app.files, old_files,
            "files snapshot must be preserved when the post-reset diff fails"
        );
        let err = app
            .last_error
            .as_deref()
            .expect("failed reset must record last_error");
        assert!(
            err.starts_with("R:"),
            "last_error must carry the `R:` prefix so the footer identifies the source: {err}"
        );
    }

    #[test]
    fn apply_reset_reports_reconfigure_watcher_when_branch_changes() {
        // ADR-0008 fix: if the user checked out a different branch
        // after starting kizu, `R` must not only update the baseline
        // SHA but also signal the event loop that the watcher's
        // branch tracking needs to move with it. Otherwise the
        // watcher stays pinned to the startup branch and silently
        // stops firing `GitHead` for future commits.
        let mut app = single_added_app("a.rs", "x");
        assert_eq!(
            app.current_branch_ref.as_deref(),
            Some("refs/heads/main"),
            "fake_app defaults to main for determinism"
        );

        let effect = app.apply_reset(
            "feedfacefeedfacefeedfacefeedfacefeedface".into(),
            Some("refs/heads/feature".into()),
            Ok(Vec::new()),
        );
        assert_eq!(
            effect,
            KeyEffect::ReconfigureWatcher,
            "branch change must request a watcher reconfigure"
        );
        assert_eq!(
            app.current_branch_ref.as_deref(),
            Some("refs/heads/feature"),
            "current_branch_ref must advance to the new branch once the reset commits"
        );
    }

    #[test]
    fn apply_reset_signals_reconfigure_on_attach_detach_transitions() {
        // Transitioning from attached to detached HEAD (and back) is
        // a branch-set change from the matcher's perspective —
        // previously matched `refs/heads/main` now becomes `None`,
        // and only the per-worktree HEAD file matters. The reset
        // path must surface that so the watcher drops the stale
        // branch ref.
        let mut app = single_added_app("a.rs", "x");

        // main → detached
        let effect = app.apply_reset(
            "feedfacefeedfacefeedfacefeedfacefeedface".into(),
            None,
            Ok(Vec::new()),
        );
        assert_eq!(effect, KeyEffect::ReconfigureWatcher);
        assert!(app.current_branch_ref.is_none());

        // detached → main
        let effect = app.apply_reset(
            "0123456701234567012345670123456701234567".into(),
            Some("refs/heads/main".into()),
            Ok(Vec::new()),
        );
        assert_eq!(effect, KeyEffect::ReconfigureWatcher);
        assert_eq!(app.current_branch_ref.as_deref(), Some("refs/heads/main"));
    }

    #[test]
    fn apply_reset_commits_new_baseline_when_diff_succeeds() {
        // Dual of the above: the happy path must still swap the
        // baseline, clear head_dirty, and install the new file set so
        // a successful reset is visibly a reset and not a no-op.
        let mut app = single_added_app("old.rs", "stale");
        app.head_dirty = true;
        app.last_error = Some("stale error".into());

        let new_file = single_added_file("fresh.rs", "fresh", 500);
        let new_sha = "feedfacefeedfacefeedfacefeedfacefeedface".to_string();
        // Same branch as the existing fake_app default — a successful
        // reset that does NOT switch branches should report
        // `KeyEffect::None` (no reconfigure needed).
        let effect = app.apply_reset(
            new_sha.clone(),
            Some("refs/heads/main".into()),
            Ok(vec![new_file]),
        );
        assert_eq!(effect, KeyEffect::None);

        assert_eq!(app.baseline_sha, new_sha);
        assert!(!app.head_dirty, "successful reset must clear head_dirty");
        assert!(
            app.last_error.is_none(),
            "successful reset must clear prior last_error"
        );
        assert_eq!(
            app.files
                .iter()
                .map(|f| f.path.as_path())
                .collect::<Vec<_>>(),
            vec![Path::new("fresh.rs")]
        );
    }

    #[test]
    fn bootstrap_with_diff_propagates_initial_compute_diff_error() {
        // If the very first `git diff` fails, bootstrap must abort — we
        // refuse to enter the event loop in a state where the main pane
        // would render as "clean" rooted in a silent error.
        let diff: Result<Vec<FileDiff>> = Err(anyhow::anyhow!("object file missing"));
        let result = App::bootstrap_with_diff(
            PathBuf::from("/tmp/fake"),
            PathBuf::from("/tmp/fake/.git"),
            PathBuf::from("/tmp/fake/.git"),
            Some("refs/heads/main".into()),
            "abcdef1234567890abcdef1234567890abcdef12".into(),
            diff,
        );
        let err = match result {
            Ok(_) => panic!("initial compute_diff failure must be propagated"),
            Err(e) => e,
        };
        let chain = format!("{err:#}");
        assert!(
            chain.contains("initial git diff"),
            "error chain should mention the initial git diff context, got: {chain}"
        );
        assert!(
            chain.contains("object file missing"),
            "error chain should preserve the underlying cause, got: {chain}"
        );
    }

    #[test]
    fn bootstrap_with_diff_applies_successful_diff_and_clears_error_state() {
        // Success path: bootstrap populates files, sorts them ascending by
        // mtime, builds a layout, and lands on the follow target.
        let diff = Ok(vec![
            single_added_file("newer.rs", "a", 200),
            single_added_file("older.rs", "b", 100),
        ]);
        let app = App::bootstrap_with_diff(
            PathBuf::from("/tmp/fake"),
            PathBuf::from("/tmp/fake/.git"),
            PathBuf::from("/tmp/fake/.git"),
            Some("refs/heads/main".into()),
            "abcdef1234567890abcdef1234567890abcdef12".into(),
            diff,
        )
        .expect("bootstrap should succeed on Ok diff");
        assert_eq!(app.files.len(), 2);
        assert!(app.last_error.is_none());
        assert!(app.follow_mode);
        assert!(
            !app.layout.rows.is_empty(),
            "layout should be built from the initial diff"
        );
    }

    #[test]
    fn bootstrap_with_diff_reads_expected_session_id_from_env() {
        // First-writer-wins binding can silently attach the TUI to
        // the wrong agent when a second agent in the same repo fires
        // faster than the one the user is attached to. Pre-binding
        // the expected `session_id` at bootstrap (here via the
        // `KIZU_SESSION_ID` environment variable) closes that
        // window. Once bound, `handle_event_log` drops events from
        // any other session instead of auto-binding to the first
        // arrival.
        //
        // Serialized via a static Mutex so cargo's parallel runner
        // doesn't interleave `set_var` / `remove_var` with other
        // env-touching tests in this file.
        use std::sync::Mutex;
        static ENV_LOCK: Mutex<()> = Mutex::new(());
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        unsafe { std::env::set_var("KIZU_SESSION_ID", "agent-xyz") };
        let diff = Ok(vec![single_added_file("a.rs", "a", 100)]);
        let app = App::bootstrap_with_diff(
            PathBuf::from("/tmp/fake"),
            PathBuf::from("/tmp/fake/.git"),
            PathBuf::from("/tmp/fake/.git"),
            Some("refs/heads/main".into()),
            "abcdef1234567890abcdef1234567890abcdef12".into(),
            diff,
        )
        .expect("bootstrap must succeed");
        unsafe { std::env::remove_var("KIZU_SESSION_ID") };

        assert_eq!(
            app.bound_session_id,
            Some("agent-xyz".to_string()),
            "bootstrap must pre-bind the TUI to the env-provided session_id"
        );
    }

    #[test]
    fn picker_enter_disables_follow_mode_so_selection_survives_recompute() {
        // bootstrap lands in follow mode. A picker selection is an
        // explicit manual navigation — the next recompute must not yank
        // the user back to the newest file's last hunk.
        let mut app = fake_app(vec![
            single_added_file("newest.rs", "x", 300),
            single_added_hunk_file("older.rs", 50, "y", 100),
        ]);
        assert!(app.follow_mode, "bootstrap starts in follow mode");

        app.open_picker();
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.current_file_path(), Some(Path::new("older.rs")));
        assert!(
            !app.follow_mode,
            "picker Enter is a manual navigation and must disable follow mode"
        );

        // Simulate a watcher-driven recompute (another file write bumping
        // newest.rs again, picking up a second hunk). refresh_anchor
        // should honour the anchor on older.rs instead of snapping us back
        // to newest.rs's last hunk.
        let newest = file_idx(&app, "newest.rs");
        app.files[newest] = make_file(
            "newest.rs",
            vec![added_hunk(1, &["x"]), added_hunk(30, &["z"])],
            400,
        );
        app.files.sort_by_key(|a| a.mtime);
        app.build_layout();
        app.refresh_anchor();

        assert_eq!(
            app.current_file_path(),
            Some(Path::new("older.rs")),
            "picker-selected file must survive a subsequent recompute"
        );
    }

    #[test]
    fn picker_cursor_tracks_same_file_across_recompute_reordering() {
        let mut app = fake_app(vec![
            single_added_file("newest.rs", "x", 300),
            single_added_file("older.rs", "y", 100),
        ]);

        app.open_picker();
        app.handle_key(key(KeyCode::Down));
        let before = app
            .picker_selected_path()
            .expect("picker target before recompute");
        assert_eq!(before, PathBuf::from("older.rs"));

        // Recompute adds a brand-new newest file. The filtered results
        // reorder newest-first, so a cursor tracked only by index would now
        // point at a different file.
        app.apply_computed_files(vec![
            single_added_file("brand_new.rs", "z", 400),
            single_added_file("newest.rs", "x", 300),
            single_added_file("older.rs", "y", 100),
        ]);

        let after = app
            .picker_selected_path()
            .expect("picker target after recompute");
        assert_eq!(
            after,
            PathBuf::from("older.rs"),
            "picker cursor must stay on the same file even when results reorder"
        );
    }

    #[test]
    fn refresh_anchor_keeps_us_on_the_same_hunk_after_recompute() {
        // First snapshot: 2 files, scroll parked on b.rs's hunk.
        let mut app = fake_app(vec![
            single_added_file("a.rs", "x", 200),
            single_added_hunk_file("b.rs", 42, "y", 100),
        ]);
        // Move to b.rs's hunk by path lookup and disable follow so the
        // anchor stays put.
        let b = file_idx(&app, "b.rs");
        app.jump_to_file_first_hunk(b);
        app.follow_mode = false;
        app.update_anchor_from_scroll();
        let anchor_before = app.anchor.clone().expect("anchor set");
        assert_eq!(anchor_before.path, PathBuf::from("b.rs"));
        assert_eq!(anchor_before.hunk_old_start, 42);

        // Simulate a recompute by appending a new (older) file. The list
        // is re-sorted ascending; b.rs stays in the layout but its row
        // index moves. The anchor must still resolve to it.
        app.files.push(single_added_file("c.rs", "z", 50));
        app.files.sort_by_key(|x| x.mtime);
        app.build_layout();
        app.refresh_anchor();

        assert_eq!(app.current_file_path(), Some(Path::new("b.rs")));
    }

    #[test]
    fn refresh_anchor_keeps_manual_mode_on_same_file_when_hunk_identity_changes() {
        let mut app = fake_app(vec![
            single_added_file("newest.rs", "x", 300),
            single_added_hunk_file("older.rs", 50, "y", 100),
        ]);

        let older = file_idx(&app, "older.rs");
        app.jump_to_file_first_hunk(older);
        app.follow_mode = false;
        app.update_anchor_from_scroll();
        assert_eq!(app.current_file_path(), Some(Path::new("older.rs")));

        // Same file survives, but the old hunk identity no longer does
        // (e.g. git merged/split hunks after a nearby edit). Manual mode
        // should stay on the same file instead of snapping to the newest
        // file's follow target.
        app.files[older] = single_added_hunk_file("older.rs", 99, "y2", 100);
        app.build_layout();
        app.refresh_anchor();

        assert_eq!(
            app.current_file_path(),
            Some(Path::new("older.rs")),
            "manual mode must stay on the same file when only hunk identity changes"
        );
    }

    #[test]
    fn refresh_anchor_prefers_nearest_hunk_within_same_file() {
        let mut app = app_with_hunks(
            "only.rs",
            vec![added_hunk(10, &["first"]), added_hunk(50, &["second"])],
            100,
        );

        app.scroll_to(
            app.layout
                .rows
                .iter()
                .position(|row| {
                    matches!(
                        row,
                        RowKind::HunkHeader {
                            file_idx: 0,
                            hunk_idx: 1
                        }
                    )
                })
                .expect("second hunk header"),
        );
        app.follow_mode = false;
        app.update_anchor_from_scroll();

        app.files[0] = make_file(
            "only.rs",
            vec![
                added_hunk(10, &["first"]),
                added_hunk(60, &["second shifted"]),
            ],
            100,
        );
        app.build_layout();
        app.refresh_anchor();

        let (_, hunk_idx) = app.current_hunk().expect("cursor on hunk");
        assert_eq!(
            hunk_idx, 1,
            "manual fallback should stay near the previously viewed hunk, not jump to the file's first hunk"
        );
    }

    #[test]
    fn apply_computed_files_pins_cursor_screen_row_across_reorder() {
        // Scenario: user is parked on a hunk in b.rs. A watcher-driven
        // recompute fires because b.rs was edited, bumping its mtime so
        // it now sorts to the end of the ascending list. Without the
        // pin, `refresh_anchor` follows the hunk to its new row index,
        // which slides the viewport — jarring when the user was only
        // reviewing that one change.
        //
        // Expected: cursor continues to point at the same hunk identity
        // AND the viewport top shifts so the cursor lands at the same
        // screen row as before the recompute.
        let mut body_lines = prefixed_diff_lines(LineKind::Context, "ctx ", 30);
        body_lines.push(diff_line(LineKind::Added, "y"));
        let mut app = fake_app(vec![
            single_added_file("a.rs", "x", 200),
            make_file("b.rs", vec![hunk(42, body_lines.clone())], 100),
        ]);

        // fake_app sorts ascending by mtime, so a.rs (200) is at the
        // bottom and b.rs (100) is at the top. Park on b.rs's only
        // hunk and pin the app into manual mode.
        let b = file_idx(&app, "b.rs");
        app.jump_to_file_first_hunk(b);
        app.follow_mode = false;
        app.update_anchor_from_scroll();

        // Skip populate_mtimes / sort in apply_computed_files so the
        // test controls the post-recompute order deterministically.
        app.view_mode = ViewMode::Stream;

        // Simulate one render cycle: pick a viewport height, let the
        // renderer resolve viewport_top, and stash that top as
        // visual_top (the Cell the renderer updates every frame).
        let body_height = 24;
        app.last_body_height.set(body_height);
        let initial_top = app.viewport_top(body_height);
        app.visual_top.set(initial_top as f32);
        let initial_screen_row = app
            .scroll
            .checked_sub(initial_top)
            .expect("cursor above viewport_top — test setup wrong");

        // Fresh file set: same two files, but b.rs has been edited
        // and its mtime bumped past a.rs's, so its position in the
        // layout moves from first to last.
        let fresh = vec![
            single_added_file("a.rs", "x", 200),
            make_file("b.rs", vec![hunk(42, body_lines.clone())], 400),
        ];
        app.apply_computed_files(fresh);

        // Anchor still points at the same hunk in b.rs.
        assert_eq!(app.current_file_path(), Some(Path::new("b.rs")));

        // Viewport top is chosen so the cursor lands at the same
        // screen row it was on before the recompute — the hallmark
        // of the pin.
        let new_top = app.viewport_top(body_height);
        let new_screen_row = app
            .scroll
            .checked_sub(new_top)
            .expect("cursor above viewport_top after recompute");
        assert_eq!(
            new_screen_row, initial_screen_row,
            "cursor must stay at the same screen row across a \
             reorder-driven apply_computed_files"
        );
    }

    #[test]
    fn scar_line_edited_away_does_not_snap_cursor_to_hunk_header() {
        // Reproduces the user-reported bug: user inserts a scar, then
        // edits the scar away (or shifts it). The watcher-driven
        // recompute should leave the cursor on a DiffLine near the
        // scar's position, not on the hunk's `@@` header row.
        //
        // Current chain:
        //   - refresh_anchor maps anchor → HunkHeader row
        //   - scar_focus's `find_new_file_line_row` returns None because
        //     the scar line is gone → scar_focus cleared
        //   - pin preserves screen row, but the LOGICAL cursor is still
        //     on HunkHeader (placed by refresh_anchor)
        // Expected: cursor lands on a DiffLine at (or near) the
        // pre-edit new-file line.
        let pre_edit_body = vec![
            diff_line(LineKind::Added, "line one"),
            diff_line(LineKind::Added, "scar target"),
            diff_line(LineKind::Added, "line three"),
        ];
        let mut app = fake_app(vec![single_hunk_file("b.rs", pre_edit_body, 100)]);

        // Park cursor on the "scar target" DiffLine (hunk_idx=0,
        // line_idx=1) and pretend a scar was just inserted there —
        // scar_focus points at new-file line 2.
        let scar_target_row = app
            .layout
            .rows
            .iter()
            .position(|r| {
                matches!(
                    r,
                    RowKind::DiffLine {
                        file_idx: 0,
                        hunk_idx: 0,
                        line_idx: 1,
                    },
                )
            })
            .expect("scar target DiffLine row must exist");
        app.scroll_to(scar_target_row);
        app.follow_mode = false;
        let abs = app.root.join("b.rs");
        app.scar_focus = Some((abs, 2));

        // Prime visual_top as a render would.
        let body_height = 20;
        app.last_body_height.set(body_height);
        let initial_top = app.viewport_top(body_height);
        app.visual_top.set(initial_top as f32);

        // Edit: scar target is removed. The hunk now has only two
        // lines. find_new_file_line_row(path, 2) will still find a
        // line ("line three" at new-line 2) OR the hunk may have
        // shrunk — depends on the edit. To reliably reproduce the
        // "line gone" branch we cut the hunk down to one line so
        // new-line 2 is past the end.
        let edited_body = vec![diff_line(LineKind::Added, "line one")];
        let fresh = vec![single_hunk_file("b.rs", edited_body, 100)];
        app.view_mode = ViewMode::Stream;
        app.apply_computed_files(fresh);

        // The bug: cursor on HunkHeader row after the recompute.
        let landed = app
            .layout
            .rows
            .get(app.scroll)
            .cloned()
            .expect("cursor on some row");
        assert!(
            !matches!(landed, RowKind::HunkHeader { .. }),
            "cursor must not land on a @@ HunkHeader after the scar line is edited away; \
             landed on {landed:?}"
        );
    }

    // ---- scroll animation --------------------------------------------

    #[test]
    fn scroll_anim_sample_at_start_returns_from_not_done() {
        let start = Instant::now();
        let anim = ScrollAnim {
            from: 10.0,
            start,
            dur: Duration::from_millis(150),
        };
        let (v, done) = anim.sample(20.0, start);
        assert!((v - 10.0).abs() < 1e-4, "expected 10.0, got {v}");
        assert!(!done);
    }

    #[test]
    fn scroll_anim_sample_at_duration_returns_target_done() {
        let start = Instant::now();
        let anim = ScrollAnim {
            from: 10.0,
            start,
            dur: Duration::from_millis(150),
        };
        let (v, done) = anim.sample(20.0, start + Duration::from_millis(150));
        assert!((v - 20.0).abs() < 1e-4, "expected 20.0, got {v}");
        assert!(done);
    }

    #[test]
    fn scroll_anim_sample_past_halfway_is_biased_toward_target() {
        // ease-out cubic: e(0.5) = 1 - 0.5^3 = 0.875
        let start = Instant::now();
        let anim = ScrollAnim {
            from: 0.0,
            start,
            dur: Duration::from_millis(100),
        };
        let (v, done) = anim.sample(10.0, start + Duration::from_millis(50));
        assert!((v - 8.75).abs() < 1e-3, "expected ~8.75 at t=0.5, got {v}");
        assert!(!done);
    }

    #[test]
    fn scroll_anim_sample_handles_moving_target_mid_tween() {
        let start = Instant::now();
        let anim = ScrollAnim {
            from: 0.0,
            start,
            dur: Duration::from_millis(100),
        };
        // Target moved from 10 to 20 mid-animation.
        let (v, _) = anim.sample(20.0, start + Duration::from_millis(50));
        // e(0.5) = 0.875, so v = 0 + (20 - 0) * 0.875 = 17.5
        assert!((v - 17.5).abs() < 1e-3, "expected ~17.5, got {v}");
    }

    #[test]
    fn scroll_to_starts_animation_when_row_changes() {
        let mut app = app_with_hunks(
            "a.rs",
            vec![added_hunk(1, &["x"]), added_hunk(10, &["y1", "y2"])],
            100,
        );
        app.anim = None;
        app.scroll = 0;
        app.scroll_to(3);
        assert!(app.anim.is_some(), "anim should be set after scroll_to");
    }

    #[test]
    fn scroll_to_does_not_start_animation_on_noop() {
        let mut app = single_added_app("a.rs", "x");
        app.anim = None;
        let current = app.scroll;
        app.scroll_to(current);
        assert!(app.anim.is_none(), "no-op scroll must not start anim");
    }

    #[test]
    fn scroll_to_carries_current_visual_into_animation_from() {
        let mut app = app_with_hunks(
            "a.rs",
            vec![added_hunk(1, &["x"]), added_hunk(20, &["y1", "y2"])],
            100,
        );
        app.scroll = 0;
        app.anim = None;
        app.visual_top.set(7.25);
        app.scroll_to(3);
        let from = app.anim.as_ref().expect("anim set").from;
        assert!((from - 7.25).abs() < 1e-4);
    }

    #[test]
    fn tick_anim_clears_anim_once_duration_elapsed() {
        let mut app = single_added_app("a.rs", "x");
        let start = Instant::now() - Duration::from_millis(500);
        app.anim = Some(ScrollAnim {
            from: 0.0,
            start,
            dur: Duration::from_millis(150),
        });
        let still_running = app.tick_anim(Instant::now());
        assert!(!still_running);
        assert!(app.anim.is_none());
    }

    #[test]
    fn tick_anim_keeps_anim_while_still_running() {
        let mut app = single_added_app("a.rs", "x");
        let start = Instant::now();
        app.anim = Some(ScrollAnim {
            from: 0.0,
            start,
            dur: Duration::from_millis(150),
        });
        let still_running = app.tick_anim(start + Duration::from_millis(50));
        assert!(still_running);
        assert!(app.anim.is_some());
    }

    #[test]
    fn visual_viewport_top_matches_target_when_idle() {
        // Build a multi-file layout so the viewport has something to center.
        let app = fake_app(vec![
            file_with_hunk(
                "a.rs",
                hunk(1, prefixed_diff_lines(LineKind::Added, "a", 8)),
                100,
            ),
            file_with_hunk(
                "b.rs",
                hunk(1, prefixed_diff_lines(LineKind::Added, "b", 8)),
                200,
            ),
        ]);
        // Idle: no anim. visual_viewport_top should equal viewport_top.
        let target = app.viewport_top(9);
        let visual = app.visual_viewport_top(9, Instant::now());
        assert_eq!(visual, target);
    }

    #[test]
    fn visual_viewport_top_tweens_between_from_and_target() {
        let mut app = fake_app(vec![
            file_with_hunk(
                "a.rs",
                hunk(1, prefixed_diff_lines(LineKind::Added, "a", 8)),
                100,
            ),
            file_with_hunk(
                "b.rs",
                hunk(1, prefixed_diff_lines(LineKind::Added, "b", 8)),
                200,
            ),
        ]);
        // Park scroll at a later row so target != 0.
        app.scroll = app.layout.rows.len() - 1;
        let target = app.viewport_top(9) as f32;
        assert!(target > 0.0);

        let start = Instant::now();
        app.anim = Some(ScrollAnim {
            from: 0.0,
            start,
            dur: Duration::from_millis(100),
        });
        // Sample at t=0.5: e(0.5) = 0.875, so visual ≈ 0.875 * target
        let v = app.visual_viewport_top(9, start + Duration::from_millis(50));
        let expected = (target * 0.875).round() as usize;
        assert_eq!(v, expected);
    }

    // ---- wrap-mode visual scroll model (ADR-0007) --------------------

    /// Build an app with a single file containing one diff line whose
    /// content is `width * wrap_factor` characters long — so at wrap
    /// body_width=`width` the one logical DiffLine produces `wrap_factor`
    /// visual rows. Used by the wrap regression tests below.
    fn wrap_regression_app(wrap_factor: usize, width: usize) -> App {
        let content: String = std::iter::repeat_n('x', width * wrap_factor).collect();
        single_added_app("a.rs", &content)
    }

    #[test]
    fn visual_index_nowrap_is_identity() {
        // With body_width=None every logical row is exactly one
        // visual row, so the prefix is [0, 1, …, n] and visual_y is
        // the identity. This is the invariant that keeps every
        // nowrap test numerically unchanged after the rework.
        let app = wrap_regression_app(4, 10);
        let vi = VisualIndex::build(&app.layout, &app.files, None);
        assert_eq!(vi.total_visual(), app.layout.rows.len());
        for i in 0..app.layout.rows.len() {
            assert_eq!(vi.visual_y(i), i, "nowrap visual_y must be identity");
            assert_eq!(vi.visual_height(i), 1);
        }
    }

    #[test]
    fn visual_index_wrap_expands_long_diff_lines() {
        // 40 chars of content at body_width=10 must produce 4 visual
        // rows for the single wrapped DiffLine. Non-diff rows (file
        // header, hunk header, spacer) still contribute exactly 1.
        let app = wrap_regression_app(4, 10);
        let vi = VisualIndex::build(&app.layout, &app.files, Some(10));

        // Find the one DiffLine row in the layout.
        let diff_row = diff_line_row(&app, 0);
        assert_eq!(
            vi.visual_height(diff_row),
            4,
            "40 chars at width 10 = 4 visual rows"
        );

        // logical_at must round-trip: the first visual y inside the
        // diff row maps back to that row with skip=0, and the second
        // visual y maps to the same row with skip=1.
        let base = vi.visual_y(diff_row);
        assert_eq!(vi.logical_at(base), (diff_row, 0));
        assert_eq!(vi.logical_at(base + 1), (diff_row, 1));
        assert_eq!(vi.logical_at(base + 3), (diff_row, 3));
    }

    #[test]
    fn viewport_placement_keeps_cursor_visible_across_wrapped_preceding_rows() {
        // Adversarial case for Codex finding #3: the cursor sits
        // just after a very long wrapped DiffLine. Under the old
        // logical-row scroll model, `viewport_top` would put the
        // wrapped line right at the top, let it consume the entire
        // viewport in visual rows, and push the cursor OFF the
        // bottom. With visual-row placement the cursor must always
        // fall inside the viewport.
        //
        // Build a layout with two diff rows: a heavily-wrapped one,
        // then a short one the cursor sits on.
        let long_content: String = std::iter::repeat_n('x', 80).collect();
        let short_content = "short".to_string();
        let mut app = added_hunk_app(
            "a.rs",
            1,
            &[long_content.as_str(), short_content.as_str()],
            100,
        );
        // Park the cursor on the second (short) diff row.
        let short_row = app
            .layout
            .rows
            .iter()
            .enumerate()
            .filter_map(|(i, r)| match r {
                RowKind::DiffLine { line_idx, .. } if *line_idx == 1 => Some(i),
                _ => None,
            })
            .next()
            .expect("second diff row");
        app.scroll = short_row;
        app.follow_mode = false;

        let body_width = Some(10);
        let body_height = 6;
        app.last_body_width.set(body_width);
        app.last_body_height.set(body_height);

        let (top_row, skip_visual) =
            app.viewport_placement(body_height, body_width, Instant::now());
        // With 80 chars at width 10 the long line occupies 8 visual
        // rows. Viewport is only 6 tall. If placement parked at
        // `top_row = 0, skip = 0`, the cursor would be at visual y
        // 8 (after FileHeader + HunkHeader + 8 wrap rows) and never
        // render. The new placement must push the viewport forward
        // far enough that the cursor's visual y falls inside [0, 6).
        let vi = VisualIndex::build(&app.layout, &app.files, body_width);
        let cursor_y = vi.visual_y(app.scroll);
        let viewport_top_y = vi.visual_y(top_row) + skip_visual;
        assert!(
            cursor_y >= viewport_top_y && cursor_y < viewport_top_y + body_height,
            "cursor at visual y {cursor_y} must sit inside viewport \
             [y={viewport_top_y}, h={body_height}); got top_row={top_row} skip={skip_visual}"
        );
    }

    #[test]
    fn scroll_by_in_wrap_mode_advances_by_visual_rows_not_logical() {
        // Under wrap, `scroll_by(delta)` must treat `delta` as
        // visual rows so Ctrl-d/Ctrl-u move a screenful's worth of
        // visible lines — not a screenful of logical rows, which in
        // a long wrapped hunk could teleport the cursor past the
        // whole block in one press.
        let mut app = wrap_regression_app(6, 10); // 60 chars → 6 visual rows
        app.last_body_width.set(Some(10));
        app.last_body_height.set(6);
        app.follow_mode = false;

        // Park cursor on the file header (row 0, visual y 0).
        app.scroll = 0;

        // Advance by 3 visual rows. Layout: [FileHeader, HunkHeader,
        // DiffLine(6 visual rows), …]. Visual ys: 0, 1, 2, 3, 4, 5,
        // 6, 7. Visual y = 3 falls inside the DiffLine at logical
        // row 2, with skip=1. `scroll_by` lands on logical row 2
        // with `cursor_sub_row = 1` (ADR-0009 fix).
        app.scroll_by(3);
        assert_eq!(
            app.scroll, 2,
            "scroll_by(3) in wrap mode should land on the diff row at visual y 3"
        );
        assert_eq!(
            app.cursor_sub_row, 1,
            "cursor_sub_row must capture the intra-row visual offset"
        );
    }

    #[test]
    fn scroll_by_in_wrap_mode_walks_inside_a_single_long_wrapped_line() {
        // Regression for Codex round-4 finding #1: on a single
        // long wrapped diff line (minified JSON / 1-line edit) the
        // old wrap-mode `scroll_by` discarded the intra-row offset
        // returned by `VisualIndex::logical_at`, so any target y
        // landing inside the SAME wrapped logical row resolved to
        // the same logical row and `scroll_to` became a no-op.
        // The user could never walk through the wrapped content.
        //
        // Setup: one diff line that wraps to 10 visual rows (100
        // chars at body width 10). Park the cursor on row 2 (the
        // DiffLine) with cursor_sub_row = 0 and call scroll_by(3).
        // The logical row must stay 2 but cursor_sub_row must
        // advance to 3 — visible evidence that the cursor actually
        // moved.
        let mut app = wrap_regression_app(10, 10);
        app.last_body_width.set(Some(10));
        app.last_body_height.set(6);
        app.follow_mode = false;

        // Find the diff row and park on its first visual line.
        let diff_row = diff_line_row(&app, 0);
        app.scroll = diff_row;
        app.cursor_sub_row = 0;

        // Walk 3 visual rows forward inside the same wrapped line.
        app.scroll_by(3);
        assert_eq!(
            app.scroll, diff_row,
            "visual walk inside a long wrapped line must stay on the same logical row"
        );
        assert_eq!(
            app.cursor_sub_row, 3,
            "cursor_sub_row must advance to 3 so the cursor is genuinely moving"
        );

        // One more walk of 4 → sub_row = 7, still same logical row.
        app.scroll_by(4);
        assert_eq!(app.scroll, diff_row);
        assert_eq!(app.cursor_sub_row, 7);
    }

    #[test]
    fn scroll_to_always_resets_cursor_sub_row() {
        // Every jump-to-row operation (next_hunk, prev_hunk, g, G,
        // follow) funnels through `scroll_to`, which must land on
        // the destination row's first visual line. The sub-row
        // offset only makes sense for in-place wrap walks.
        let mut app = wrap_regression_app(10, 10);
        app.last_body_width.set(Some(10));
        app.cursor_sub_row = 5;
        app.scroll_to(0);
        assert_eq!(app.cursor_sub_row, 0);
    }

    #[test]
    fn toggle_wrap_lines_resets_cursor_sub_row() {
        // Wrap toggle changes the coordinate system entirely — any
        // intra-row offset captured under the old mode has no
        // meaning under the new one. Drop it to land cleanly.
        let mut app = wrap_regression_app(10, 10);
        app.cursor_sub_row = 4;
        app.toggle_wrap_lines();
        assert_eq!(app.cursor_sub_row, 0);
    }

    // ---- watcher health decoupling (ADR-0008) ------------------------

    #[test]
    fn handle_watch_burst_records_failure_in_watcher_health_not_last_error() {
        // Regression for Codex round-3 finding: the previous design
        // wrote watcher backend failures into `last_error`, so a
        // subsequent successful `recompute_diff` would silently
        // clear them via `apply_computed_files`. The new design
        // parks backend failures in a dedicated `watcher_health`
        // slot, which survives diff success and only clears when
        // a non-Error event proves the backend is alive again.
        let mut app = fake_app(vec![]);
        assert!(app.watcher_health.is_healthy());

        let (need_recompute, need_head_dirty) = app.handle_watch_burst([WatchEvent::Error {
            source: WatchSource::Worktree,
            message: "watcher [worktree]: fsevents dropped".into(),
        }]);
        assert!(
            need_recompute,
            "backend failure must force a recompute so the UI falls back to fresh data"
        );
        assert!(!need_head_dirty);
        assert!(
            app.watcher_health
                .has_failure(WatchSource::Worktree, "fsevents dropped"),
            "error must land in watcher_health, not last_error"
        );
        assert!(
            app.last_error.is_none(),
            "last_error must stay untouched — it's the diff-level error slot"
        );
    }

    #[test]
    fn watcher_health_survives_successful_recompute_through_apply_computed_files() {
        // The core decoupling: a diff computation succeeding must
        // NOT imply the watcher recovered. This test pins the
        // invariant that `apply_computed_files` leaves
        // `watcher_health` alone.
        let mut app = fake_app(vec![]);
        app.watcher_health.record_failure(
            WatchSource::GitRefs,
            "watcher [git.refs]: kqueue overflow".into(),
        );

        // Directly exercise apply_computed_files with a fresh
        // successful payload. The pre-rework bug cleared
        // watcher_health via the same code path that clears
        // last_error.
        app.apply_computed_files(vec![single_added_file("a.rs", "x", 100)]);

        assert!(
            app.watcher_health
                .has_failure(WatchSource::GitRefs, "kqueue overflow"),
            "a successful diff recompute must not imply watcher recovery"
        );
    }

    #[test]
    fn input_health_survives_successful_recompute_through_apply_computed_files() {
        let mut app = fake_app(vec![]);
        app.input_health = Some("input: stream hiccup".into());

        app.apply_computed_files(vec![single_added_file("a.rs", "x", 100)]);

        assert_eq!(
            app.input_health.as_deref(),
            Some("input: stream hiccup"),
            "a successful diff recompute must not imply input recovery"
        );
    }

    #[test]
    fn handle_watch_burst_clears_failed_health_for_the_same_source_only() {
        let mut app = fake_app(vec![]);
        app.watcher_health.record_failure(
            WatchSource::Worktree,
            "watcher [worktree]: transient".into(),
        );

        let (need_recompute, _) = app.handle_watch_burst([WatchEvent::Worktree]);
        assert!(need_recompute, "Worktree event still triggers a recompute");
        assert!(
            app.watcher_health.is_healthy(),
            "a live event from the same source must clear that source's failure"
        );
    }

    #[test]
    fn handle_watch_burst_does_not_flip_healthy_on_mixed_bursts() {
        // When a single coalesced burst contains BOTH a live event
        // and an Error, the Error wins: the backend may have failed
        // after emitting the earlier event and we can't prove
        // recovery from a burst that ends in failure. Precedence
        // goes to the pessimistic state.
        let mut app = fake_app(vec![]);
        app.handle_watch_burst([
            WatchEvent::Worktree,
            WatchEvent::Error {
                source: WatchSource::Worktree,
                message: "watcher [worktree]: late failure".into(),
            },
        ]);
        assert!(
            app.watcher_health
                .has_failure(WatchSource::Worktree, "late failure"),
            "a burst that includes an Error for a source must keep that source failed"
        );
    }

    #[test]
    fn handle_watch_burst_does_not_clear_git_failure_when_worktree_recovers() {
        let mut app = fake_app(vec![]);
        app.watcher_health.record_failure(
            WatchSource::GitRefs,
            "watcher [git.refs]: still dead".into(),
        );

        let (need_recompute, need_head_dirty) = app.handle_watch_burst([WatchEvent::Worktree]);
        assert!(need_recompute);
        assert!(!need_head_dirty);
        assert!(
            app.watcher_health
                .has_failure(WatchSource::GitRefs, "still dead"),
            "worktree recovery must not clear an unrelated git watcher failure"
        );
    }

    #[test]
    fn handle_watch_burst_does_not_clear_other_git_source_failure() {
        let mut app = fake_app(vec![]);
        app.watcher_health.record_failure(
            WatchSource::GitCommonRoot,
            "watcher [git.root]: still dead".into(),
        );

        let (_, need_head_dirty) =
            app.handle_watch_burst([WatchEvent::GitHead(WatchSource::GitRefs)]);
        assert!(need_head_dirty);
        assert!(
            app.watcher_health
                .has_failure(WatchSource::GitCommonRoot, "still dead"),
            "a GitHead from one git source must not clear a different git source failure"
        );
    }

    #[test]
    fn toggle_wrap_lines_clears_in_flight_scroll_animation() {
        // Wrap toggling changes the coordinate system that anim
        // tweens live in. The cleanest thing to do is snap: clear
        // the anim so the next frame draws at the new target and
        // no disorienting cross-system tween ever shows up.
        let mut app = wrap_regression_app(2, 10);
        app.anim = Some(ScrollAnim {
            from: 5.0,
            start: Instant::now(),
            dur: Duration::from_millis(150),
        });
        app.toggle_wrap_lines();
        assert!(
            app.anim.is_none(),
            "wrap toggle must clear scroll animation to avoid cross-coordinate tween"
        );
    }

    // ---- M4: scar dispatch (a / r canned insertion) -----------------

    /// Build an `App` backed by a real tempdir on disk so `insert_scar`
    /// can actually read + write the target file. The source files and
    /// the `FileDiff` layout are kept in sync by hand — enough to
    /// exercise the `a` / `r` keybinding end-to-end without booting a
    /// full git repo.
    fn scar_app_with_real_fs(
        tmp: &tempfile::TempDir,
        rel_path: &str,
        source: &str,
        hunk_new_start: usize,
        lines: Vec<DiffLine>,
    ) -> App {
        let abs = tmp.path().join(rel_path);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(&abs, source).expect("seed source file");

        let file = make_file(rel_path, vec![hunk(hunk_new_start, lines)], 100);
        let mut app = fake_app(vec![file]);
        app.root = tmp.path().to_path_buf();
        app
    }

    fn scar_app_with_line(
        tmp: &tempfile::TempDir,
        rel_path: &str,
        source: &str,
        hunk_new_start: usize,
        kind: LineKind,
        line: &str,
    ) -> App {
        scar_app_with_real_fs(
            tmp,
            rel_path,
            source,
            hunk_new_start,
            vec![diff_line(kind, line)],
        )
    }

    fn scar_app_with_added_line(
        tmp: &tempfile::TempDir,
        rel_path: &str,
        source: &str,
        hunk_new_start: usize,
        line: &str,
    ) -> App {
        scar_app_with_line(tmp, rel_path, source, hunk_new_start, LineKind::Added, line)
    }

    fn scar_app_with_context_line(
        tmp: &tempfile::TempDir,
        rel_path: &str,
        source: &str,
        hunk_new_start: usize,
        line: &str,
    ) -> App {
        scar_app_with_line(
            tmp,
            rel_path,
            source,
            hunk_new_start,
            LineKind::Context,
            line,
        )
    }

    /// Park the cursor on the Nth DiffLine row in the layout (0-indexed
    /// across the whole scroll, not per file). Panics if there aren't
    /// enough DiffLine rows — the tests control the layout exactly so
    /// this is a loud-failure helper on purpose.
    fn cursor_on_nth_diff_line(app: &mut App, n: usize) {
        let mut seen = 0;
        for (i, row) in app.layout.rows.iter().enumerate() {
            if matches!(row, RowKind::DiffLine { .. }) {
                if seen == n {
                    app.scroll_to(i);
                    return;
                }
                seen += 1;
            }
        }
        panic!("layout has fewer than {} DiffLine rows", n + 1);
    }

    fn read_temp_file(tmp: &tempfile::TempDir, rel_path: &str) -> String {
        std::fs::read_to_string(tmp.path().join(rel_path)).expect("read")
    }

    fn open_scar_comment_app(
        tmp: &tempfile::TempDir,
        rel_path: &str,
        source: &str,
        hunk_new_start: usize,
        line: &str,
    ) -> App {
        let mut app = scar_app_with_added_line(tmp, rel_path, source, hunk_new_start, line);
        cursor_on_nth_diff_line(&mut app, 0);
        app.handle_key(key(KeyCode::Char('c')));
        app
    }

    #[test]
    fn handle_key_a_inserts_ask_scar_above_cursor_line() {
        let tmp = tempfile::tempdir().expect("tmp");
        // Simulate a diff where line 2 of main.rs was newly added. The
        // cursor lands on that added row, and pressing `a` should insert
        // the canned "ask" scar directly above it.
        let mut app = scar_app_with_added_line(
            &tmp,
            "src/main.rs",
            "fn one() {}\nfn two() {}\n",
            2,
            "fn two() {}",
        );
        cursor_on_nth_diff_line(&mut app, 0);

        app.handle_key(key(KeyCode::Char('a')));

        let after = read_temp_file(&tmp, "src/main.rs");
        assert_eq!(
            after, "fn one() {}\n// @kizu[ask]: explain this change\nfn two() {}\n",
            "`a` key must insert the canned ask scar above the cursor row",
        );
        assert!(
            app.last_error.is_none(),
            "successful scar insert must not touch last_error"
        );
    }

    #[test]
    fn handle_key_r_inserts_reject_scar_above_cursor_line() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut app = scar_app_with_added_line(
            &tmp,
            "auth.py",
            "def main():\n    return 1\n",
            2,
            "    return 1",
        );
        cursor_on_nth_diff_line(&mut app, 0);

        app.handle_key(key(KeyCode::Char('r')));

        let after = read_temp_file(&tmp, "auth.py");
        assert_eq!(
            after, "def main():\n    # @kizu[reject]: revert this change\n    return 1\n",
            "`r` key must insert the canned reject scar using python # syntax",
        );
    }

    #[test]
    fn handle_key_a_is_noop_when_cursor_is_on_a_file_header_row() {
        // File header rows have no hunk id → `scar_target_line`
        // returns None → `a` must be a no-op. The source file on
        // disk stays untouched and no error is recorded.
        let tmp = tempfile::tempdir().expect("tmp");
        let original = "fn one() {}\n";
        let mut app = scar_app_with_added_line(&tmp, "lib.rs", original, 1, "fn one() {}");
        // Park the cursor on the FileHeader row explicitly.
        let file_header_row = file_header_row(&app);
        app.scroll_to(file_header_row);

        app.handle_key(key(KeyCode::Char('a')));

        let after = read_temp_file(&tmp, "lib.rs");
        assert_eq!(after, original, "header-row `a` must not touch the file");
        assert!(app.last_error.is_none(), "header-row `a` is a clean no-op");
    }

    #[test]
    fn handle_key_a_surfaces_insert_failure_on_last_error() {
        // Point `file.path` at a path that does not exist on disk.
        // `insert_scar` will fail inside the read phase, and the
        // dispatch must surface that through `last_error` without
        // panicking.
        let tmp = tempfile::tempdir().expect("tmp");
        let file = single_added_file("ghost.rs", "fn missing()", 100);
        let mut app = fake_app(vec![file]);
        app.root = tmp.path().to_path_buf();
        cursor_on_nth_diff_line(&mut app, 0);

        app.handle_key(key(KeyCode::Char('a')));

        assert!(
            app.last_error
                .as_deref()
                .is_some_and(|msg| msg.starts_with("scar:")),
            "missing-file scar failure must land on last_error, got {:?}",
            app.last_error
        );
    }

    #[test]
    fn scar_target_line_maps_hunk_header_to_first_changed_line_no_context() {
        // Hunk starts immediately with Added lines (no leading context).
        // The first changed line IS new_start, so the result equals new_start.
        let mut app = added_hunk_app("a.rs", 42, &["first", "second"], 100);
        let header_row = hunk_header_row(&app, 0);
        app.scroll_to(header_row);
        let (_, line) = app.scar_target_line().expect("target");
        assert_eq!(
            line, 42,
            "no-context hunk header → first changed line = new_start"
        );
    }

    #[test]
    fn scar_target_line_maps_hunk_header_skipping_leading_context() {
        // Hunk has 2 leading Context lines before the first Added line.
        // The scar should land above the Added line, not above the context.
        // new_start=10, context, context, added → target = 10 + 2 = 12.
        let mut app = single_hunk_app(
            "a.rs",
            10,
            vec![
                diff_line(LineKind::Context, "ctx1"),
                diff_line(LineKind::Context, "ctx2"),
                diff_line(LineKind::Added, "new_stuff"),
                diff_line(LineKind::Context, "ctx3"),
            ],
            100,
        );
        let header_row = hunk_header_row(&app, 0);
        app.scroll_to(header_row);
        let (_, line) = app.scar_target_line().expect("target");
        assert_eq!(
            line, 12,
            "hunk header with 2 leading context lines → first changed line at new_start+2"
        );
    }

    #[test]
    fn handle_key_a_on_hunk_header_writes_scar_above_first_hunk_line() {
        // Real tempdir end-to-end: cursor on the hunk header row,
        // press `a`, and the source file should now carry the
        // canned ask scar directly above the first body line.
        let tmp = tempfile::tempdir().expect("tmp");
        let mut app = scar_app_with_added_line(&tmp, "src/lib.rs", "line_a\nline_b\n", 2, "line_b");
        let header_row = hunk_header_row(&app, 0);
        app.scroll_to(header_row);

        app.handle_key(key(KeyCode::Char('a')));

        let after = read_temp_file(&tmp, "src/lib.rs");
        assert_eq!(
            after, "line_a\n// @kizu[ask]: explain this change\nline_b\n",
            "`a` on a hunk header must drop the scar above hunk.new_start",
        );
    }

    // ---- M4 slice 3: `c` free-text scar overlay --------------------

    #[test]
    fn handle_key_c_opens_scar_comment_overlay_with_captured_target() {
        let tmp = tempfile::tempdir().expect("tmp");
        let app = open_scar_comment_app(
            &tmp,
            "src/foo.rs",
            "fn alpha() {}\nfn beta() {}\n",
            2,
            "fn beta() {}",
        );

        let state = app
            .scar_comment
            .as_ref()
            .expect("`c` must open the comment overlay on a diff row");
        assert_eq!(state.body, "", "body starts empty");
        assert_eq!(state.target_line, 2, "captures current diff-row line");
        assert_eq!(
            state.target_path,
            tmp.path().join("src/foo.rs"),
            "captures absolute target path"
        );
        let after = read_temp_file(&tmp, "src/foo.rs");
        assert_eq!(
            after, "fn alpha() {}\nfn beta() {}\n",
            "`c` must not touch the file until `Enter` commits"
        );
    }

    #[test]
    fn handle_key_c_is_noop_on_file_header_row() {
        let tmp = tempfile::tempdir().expect("tmp");
        let original = "fn one() {}\n";
        let mut app = scar_app_with_added_line(&tmp, "lib.rs", original, 1, "fn one() {}");
        let header_row = file_header_row(&app);
        app.scroll_to(header_row);

        app.handle_key(key(KeyCode::Char('c')));

        assert!(
            app.scar_comment.is_none(),
            "file-header `c` must not open the overlay"
        );
    }

    #[test]
    fn scar_comment_typing_appends_characters_to_body() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut app = open_scar_comment_app(&tmp, "a.rs", "x\ny\n", 2, "y");

        app.handle_key(key(KeyCode::Char('h')));
        app.handle_key(key(KeyCode::Char('i')));
        app.handle_key(key(KeyCode::Char('!')));

        let state = app.scar_comment.as_ref().expect("still open");
        assert_eq!(state.body, "hi!");
    }

    #[test]
    fn scar_comment_backspace_deletes_last_character() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut app = open_scar_comment_app(&tmp, "a.rs", "x\ny\n", 2, "y");
        type_chars(&mut app, "ab");

        app.handle_key(key(KeyCode::Backspace));
        let state = app.scar_comment.as_ref().expect("still open");
        assert_eq!(state.body, "a");
    }

    #[test]
    fn scar_comment_esc_cancels_without_writing_to_file() {
        let tmp = tempfile::tempdir().expect("tmp");
        let original = "fn one() {}\nfn two() {}\n";
        let mut app = open_scar_comment_app(&tmp, "cancel.rs", original, 2, "fn two() {}");
        type_chars(&mut app, "dont");

        app.handle_key(key(KeyCode::Esc));

        assert!(app.scar_comment.is_none(), "Esc closes the overlay");
        let after = read_temp_file(&tmp, "cancel.rs");
        assert_eq!(after, original, "cancel must not touch the file");
        assert!(app.last_error.is_none(), "cancel is not an error");
    }

    #[test]
    fn scar_comment_enter_commits_free_scar_above_target_line() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut app = open_scar_comment_app(
            &tmp,
            "commit.rs",
            "fn one() {}\nfn two() {}\n",
            2,
            "fn two() {}",
        );
        type_chars(&mut app, "why two?");

        app.handle_key(key(KeyCode::Enter));

        assert!(app.scar_comment.is_none(), "commit closes the overlay");
        let after = read_temp_file(&tmp, "commit.rs");
        assert_eq!(
            after, "fn one() {}\n// @kizu[free]: why two?\nfn two() {}\n",
            "Enter must write a free-scar above the captured target line"
        );
    }

    #[test]
    fn scar_comment_enter_on_empty_body_is_cancel() {
        let tmp = tempfile::tempdir().expect("tmp");
        let original = "fn one() {}\nfn two() {}\n";
        let mut app = open_scar_comment_app(&tmp, "empty.rs", original, 2, "fn two() {}");
        app.handle_key(key(KeyCode::Enter));

        assert!(
            app.scar_comment.is_none(),
            "empty commit closes the overlay"
        );
        let after = read_temp_file(&tmp, "empty.rs");
        assert_eq!(after, original, "empty body must not write a blank scar");
    }

    #[test]
    fn normal_keys_are_inert_while_scar_comment_overlay_is_open() {
        // While the overlay is open, typing `q` must accumulate into
        // the body instead of quitting the app. Proves the router
        // correctly parks normal-mode dispatch behind the overlay.
        let tmp = tempfile::tempdir().expect("tmp");
        let mut app = open_scar_comment_app(&tmp, "quit.rs", "x\ny\n", 2, "y");

        app.handle_key(key(KeyCode::Char('q')));

        assert!(!app.should_quit, "q while overlay open must not quit");
        let state = app.scar_comment.as_ref().expect("still open");
        assert_eq!(state.body, "q");
    }

    // ---- M4c: Enter file-view zoom ---------------------------------

    #[test]
    fn enter_transitions_to_file_view_from_hunk() {
        let tmp = tempfile::tempdir().expect("tmp");
        let (mut app, _abs) = revert_app_with_real_repo(
            &tmp,
            "foo.rs",
            "fn one() {}\n",
            "fn one() {}\nfn two() {}\n",
        );
        cursor_on_nth_diff_line(&mut app, 0);
        let before_scroll = app.scroll;

        app.handle_key(key(KeyCode::Enter));

        let fv = app.file_view.as_ref().expect("file view opened");
        assert_eq!(fv.path, PathBuf::from("foo.rs"));
        assert_eq!(fv.return_scroll, before_scroll);
        assert_eq!(fv.lines.len(), 2, "file has 2 lines");
        assert_eq!(fv.lines[0], "fn one() {}");
        assert_eq!(fv.lines[1], "fn two() {}");
        assert!(
            fv.line_bg.contains_key(&1),
            "line 1 (fn two) should have added bg"
        );
        assert!(!fv.line_bg.contains_key(&0), "line 0 is context — no bg");
    }

    #[test]
    fn enter_file_view_starts_at_cursor_not_hunk_header() {
        // Context + two added lines. The diff surfaces all 3 as
        // DiffLine rows: [Context "fn one", Added "fn two", Added
        // "fn three"] with `hunk.new_start = 1`. Parking the cursor
        // on the 3rd DiffLine (Added "fn three" → new-file line 3)
        // must take file view to 0-indexed line 2, NOT to 0 (the old
        // behavior which snapped to `hunk.new_start - 1`).
        let tmp = tempfile::tempdir().expect("tmp");
        let (mut app, _abs) = revert_app_with_real_repo(
            &tmp,
            "foo.rs",
            "fn one() {}\n",
            "fn one() {}\nfn two() {}\nfn three() {}\n",
        );
        cursor_on_nth_diff_line(&mut app, 2);

        app.handle_key(key(KeyCode::Enter));

        let fv = app.file_view.as_ref().expect("file view opened");
        assert_eq!(
            fv.cursor, 2,
            "file view cursor must track the diff cursor's new-file line (was snapping to hunk.new_start - 1 = 0)",
        );
    }

    #[test]
    fn file_view_esc_returns_to_scroll_and_restores_cursor() {
        let tmp = tempfile::tempdir().expect("tmp");
        let (mut app, _abs) = revert_app_with_real_repo(
            &tmp,
            "foo.rs",
            "fn one() {}\n",
            "fn one() {}\nfn two() {}\n",
        );
        cursor_on_nth_diff_line(&mut app, 0);
        let saved = app.scroll;

        app.handle_key(key(KeyCode::Enter));
        assert!(app.file_view.is_some());

        app.handle_key(key(KeyCode::Esc));
        assert!(app.file_view.is_none(), "Esc closes file view");
        assert_eq!(app.scroll, saved, "cursor restored");
    }

    #[test]
    fn file_view_enter_also_exits() {
        let tmp = tempfile::tempdir().expect("tmp");
        let (mut app, _abs) = revert_app_with_real_repo(
            &tmp,
            "foo.rs",
            "fn one() {}\n",
            "fn one() {}\nfn two() {}\n",
        );
        cursor_on_nth_diff_line(&mut app, 0);
        app.handle_key(key(KeyCode::Enter)); // open
        app.handle_key(key(KeyCode::Enter)); // close
        assert!(app.file_view.is_none());
    }

    #[test]
    fn file_view_j_k_chunk_scroll_and_shift_j_k_single_row() {
        let tmp = tempfile::tempdir().expect("tmp");
        let (mut app, _abs) =
            revert_app_with_real_repo(&tmp, "foo.rs", "a\nb\nc\n", "a\nb\nc\nd\n");
        cursor_on_nth_diff_line(&mut app, 0);
        app.handle_key(key(KeyCode::Enter));
        let start = app.file_view.as_ref().unwrap().cursor;

        // j moves by chunk_size (viewport/3, at least 1)
        let chunk = app.chunk_size();
        app.handle_key(key(KeyCode::Char('j')));
        let after_j = app.file_view.as_ref().unwrap().cursor;
        assert_eq!(after_j, (start + chunk).min(3));

        // k reverses it
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(app.file_view.as_ref().unwrap().cursor, start);

        // J moves exactly 1 row
        app.handle_key(key(KeyCode::Char('J')));
        assert_eq!(app.file_view.as_ref().unwrap().cursor, start + 1);

        // K reverses 1 row
        app.handle_key(key(KeyCode::Char('K')));
        assert_eq!(app.file_view.as_ref().unwrap().cursor, start);
    }

    #[test]
    fn file_view_wrap_mode_walks_visual_rows_inside_long_line() {
        let tmp = tempfile::tempdir().expect("tmp");
        let long = format!("const DATA: &str = {:?};", "0123456789".repeat(12));
        let after = format!("{long}\n");
        let (mut app, _abs) = revert_app_with_real_repo(&tmp, "foo.rs", "", &after);
        cursor_on_nth_diff_line(&mut app, 0);
        app.handle_key(key(KeyCode::Enter));
        app.last_body_height.set(5);
        app.file_view
            .as_ref()
            .expect("file view")
            .last_body_width
            .set(20);

        app.handle_key(key(KeyCode::Char('w')));
        app.handle_key(key(KeyCode::Char('J')));

        let fv = app.file_view.as_ref().expect("file view still open");
        assert_eq!(
            fv.cursor, 0,
            "wrap-mode J should stay on the same logical line when only the visual sub-row changes",
        );
        assert_eq!(
            fv.cursor_sub_row, 1,
            "wrap-mode J should advance to the next visual row inside the long file-view line",
        );
    }

    #[test]
    #[allow(non_snake_case)]
    fn file_view_g_goes_to_top_and_G_to_bottom() {
        let tmp = tempfile::tempdir().expect("tmp");
        let (mut app, _abs) =
            revert_app_with_real_repo(&tmp, "foo.rs", "a\nb\nc\n", "a\nb\nc\nd\n");
        cursor_on_nth_diff_line(&mut app, 0);
        app.handle_key(key(KeyCode::Enter));

        app.handle_key(key(KeyCode::Char('G')));
        assert_eq!(app.file_view.as_ref().unwrap().cursor, 3); // 4 lines, 0-indexed last = 3

        app.handle_key(key(KeyCode::Char('g')));
        assert_eq!(app.file_view.as_ref().unwrap().cursor, 0);
    }

    #[test]
    fn enter_is_noop_on_file_header_row() {
        let tmp = tempfile::tempdir().expect("tmp");
        let (mut app, _abs) = revert_app_with_real_repo(
            &tmp,
            "foo.rs",
            "fn one() {}\n",
            "fn one() {}\nfn two() {}\n",
        );
        let header_row = file_header_row(&app);
        app.scroll_to(header_row);

        app.handle_key(key(KeyCode::Enter));
        assert!(app.file_view.is_none());
    }

    // ---- M4b slice 1: `/` search + first-match jump ---------------

    fn find_first_row_matching<F: Fn(&RowKind) -> bool>(app: &App, f: F) -> usize {
        app.layout.rows.iter().position(f).expect("row exists")
    }

    fn file_header_row(app: &App) -> usize {
        find_first_row_matching(app, |r| matches!(r, RowKind::FileHeader { .. }))
    }

    fn diff_line_row(app: &App, line_idx: usize) -> usize {
        find_first_row_matching(
            app,
            |r| matches!(r, RowKind::DiffLine { line_idx: idx, .. } if *idx == line_idx),
        )
    }

    fn first_diff_row_with_kind(app: &App, kind: LineKind) -> usize {
        find_first_row_matching(app, |r| {
            if let RowKind::DiffLine {
                file_idx,
                hunk_idx,
                line_idx,
            } = r
            {
                app.files
                    .get(*file_idx)
                    .and_then(|f| match &f.content {
                        DiffContent::Text(hunks) => hunks
                            .get(*hunk_idx)
                            .and_then(|h| h.lines.get(*line_idx))
                            .map(|l| l.kind == kind),
                        _ => None,
                    })
                    .unwrap_or(false)
            } else {
                false
            }
        })
    }

    fn hunk_header_row(app: &App, hunk_idx: usize) -> usize {
        find_first_row_matching(
            app,
            |r| matches!(r, RowKind::HunkHeader { file_idx: 0, hunk_idx: idx } if *idx == hunk_idx),
        )
    }

    fn assert_cursor_on_hunk_header(app: &App, hunk_idx: usize, context: &str) {
        assert!(
            matches!(
                app.layout.rows.get(app.scroll),
                Some(RowKind::HunkHeader { file_idx: 0, hunk_idx: idx }) if *idx == hunk_idx
            ),
            "{context}, got {:?}",
            app.layout.rows.get(app.scroll)
        );
    }

    #[test]
    fn find_matches_returns_empty_for_empty_query() {
        let app = single_added_app("a.rs", "hello world");
        let m = find_matches(&app.layout, &app.files, "");
        assert!(m.is_empty());
    }

    #[test]
    fn find_matches_finds_substring_case_insensitive_when_query_is_lowercase() {
        let app = single_hunk_app(
            "a.rs",
            1,
            vec![
                diff_line(LineKind::Added, "Hello WORLD"),
                diff_line(LineKind::Context, "no match here"),
                diff_line(LineKind::Added, "World wide"),
            ],
            100,
        );
        let m = find_matches(&app.layout, &app.files, "world");
        assert_eq!(m.len(), 2, "smart-case lowercase query matches both rows");
        assert!(m.iter().all(|loc| loc.byte_start < loc.byte_end));
    }

    #[test]
    fn find_matches_is_case_sensitive_when_query_has_uppercase() {
        let app = added_hunk_app("a.rs", 1, &["hello World", "hello world"], 100);
        let m = find_matches(&app.layout, &app.files, "World");
        assert_eq!(m.len(), 1, "uppercase query is case-sensitive");
    }

    #[test]
    fn find_matches_captures_multiple_hits_on_one_row() {
        let app = single_added_app("a.rs", "foo foo foo");
        let m = find_matches(&app.layout, &app.files, "foo");
        assert_eq!(m.len(), 3);
        assert_eq!(m[0].byte_start, 0);
        assert_eq!(m[1].byte_start, 4);
        assert_eq!(m[2].byte_start, 8);
    }

    #[test]
    fn slash_opens_search_input_composer() {
        let mut app = single_added_app("a.rs", "x");

        app.handle_key(key(KeyCode::Char('/')));

        assert!(app.search_input.is_some(), "/ must open the composer");
        assert_eq!(app.search_input.as_ref().unwrap().query, "");
    }

    #[test]
    fn search_input_typing_appends_to_query_and_backspace_deletes() {
        let mut app = single_added_app("a.rs", "x");
        app.handle_key(key(KeyCode::Char('/')));
        type_chars(&mut app, "foo");
        assert_eq!(app.search_input.as_ref().unwrap().query, "foo");
        app.handle_key(key(KeyCode::Backspace));
        assert_eq!(app.search_input.as_ref().unwrap().query, "fo");
    }

    #[test]
    fn search_input_esc_cancels_without_installing_search_state() {
        let mut app = single_added_app("a.rs", "foo");
        app.handle_key(key(KeyCode::Char('/')));
        app.handle_key(key(KeyCode::Char('f')));
        app.handle_key(key(KeyCode::Esc));
        assert!(app.search_input.is_none());
        assert!(app.search.is_none());
    }

    #[test]
    fn search_input_enter_commits_and_jumps_cursor_to_first_match() {
        let mut app = added_hunk_app("a.rs", 1, &["alpha", "beta", "gamma"], 100);
        // Park the cursor on the first diff row (alpha).
        cursor_on_nth_diff_line(&mut app, 0);

        app.handle_key(key(KeyCode::Char('/')));
        type_chars(&mut app, "beta");
        app.handle_key(key(KeyCode::Enter));

        assert!(app.search_input.is_none(), "composer closed on commit");
        let state = app.search.as_ref().expect("search installed");
        assert_eq!(state.matches.len(), 1);
        assert_eq!(state.current, 0);
        // Cursor landed on the "beta" row — not the first diff row.
        let beta_row = diff_line_row(&app, 1);
        assert_eq!(app.scroll, beta_row);
        assert!(!app.follow_mode, "manual jump drops follow mode");
    }

    #[test]
    fn search_input_enter_with_empty_query_does_not_wipe_existing_search() {
        let mut app = single_added_app("a.rs", "alpha");
        // Pre-install a fake confirmed search state.
        install_search(&mut app, "alpha", 0);

        app.handle_key(key(KeyCode::Char('/')));
        app.handle_key(key(KeyCode::Enter)); // empty body

        assert!(
            app.search.is_some(),
            "empty-query commit must preserve prior search state"
        );
    }

    // ---- M4b slice 2: n/N navigation ------------------------------

    fn commit_search(app: &mut App, query: &str) {
        app.handle_key(key(KeyCode::Char('/')));
        type_chars(app, query);
        app.handle_key(key(KeyCode::Enter));
    }

    #[test]
    fn search_jump_next_walks_matches_in_order() {
        let mut app = added_hunk_app("a.rs", 1, &["foo", "bar", "foo", "foo"], 100);
        // Park the cursor on the file header (row 0) so commit picks
        // match 0 (the first match after the cursor in layout order).
        app.scroll = 0;
        commit_search(&mut app, "foo");

        // After commit, current = 0. Advance twice: 0 → 1 → 2.
        app.handle_key(key(KeyCode::Char('n')));
        let mid = app.search.as_ref().unwrap().current;
        app.handle_key(key(KeyCode::Char('n')));
        let tail = app.search.as_ref().unwrap().current;
        assert_eq!(mid, 1);
        assert_eq!(tail, 2);
    }

    #[test]
    fn search_jump_next_wraps_around_at_end() {
        let mut app = added_hunk_app("a.rs", 1, &["foo", "foo"], 100);
        app.scroll = 0;
        commit_search(&mut app, "foo");

        // current=0 → n → 1 → n → 0 (wrap)
        app.handle_key(key(KeyCode::Char('n')));
        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(app.search.as_ref().unwrap().current, 0);
    }

    #[test]
    fn search_jump_prev_wraps_around_at_start() {
        let mut app = added_hunk_app("a.rs", 1, &["foo", "foo", "foo"], 100);
        app.scroll = 0;
        commit_search(&mut app, "foo");

        // current=0 → N → 2 (wrap to tail)
        app.handle_key(key(KeyCode::Char('N')));
        assert_eq!(app.search.as_ref().unwrap().current, 2);
    }

    #[test]
    fn search_jump_next_is_noop_when_no_search_state() {
        let mut app = single_added_app("a.rs", "foo");
        cursor_on_nth_diff_line(&mut app, 0);
        let before = app.scroll;

        app.handle_key(key(KeyCode::Char('n')));

        assert!(app.search.is_none());
        assert_eq!(app.scroll, before, "stray `n` must not move the cursor");
    }

    #[test]
    fn search_matches_rehydrate_after_recompute_preserves_query() {
        // A watcher-driven recompute rebuilds the layout (row indices
        // change) so stale `MatchLocation.row` values would point at
        // the wrong content. After `apply_computed_files`, the search
        // must re-run `find_matches` against the fresh layout, and the
        // confirmed query must survive so `n`/`N` still work.
        let mut app = single_added_app("a.rs", "foo bar");
        install_search(&mut app, "foo", 0);
        let pre_row = app.search.as_ref().unwrap().matches[0].row;

        // Simulate a watcher-driven recompute that prepends a new file
        // so every layout row index downstream of it shifts.
        app.apply_computed_files(vec![
            context_hunk_file("b.rs", 1, &["ctx"], 50),
            single_added_file("a.rs", "foo bar", 100),
        ]);

        let state = app.search.as_ref().expect("search survives recompute");
        assert_eq!(state.query, "foo");
        assert_eq!(
            state.matches.len(),
            1,
            "recomputed matches must point at the new layout",
        );
        let post_row = state.matches[0].row;
        assert_ne!(
            post_row, pre_row,
            "layout rebuild should have shifted the match row; rehydrate must track",
        );
        assert!(
            matches!(
                app.layout.rows.get(post_row),
                Some(RowKind::DiffLine { .. })
            ),
            "rehydrated match must index a DiffLine row in the new layout",
        );
    }

    #[test]
    fn search_commit_starts_from_first_match_after_cursor() {
        // vim-style `/`: commit jumps to the first match strictly
        // after the cursor position, not the global first match.
        let mut app = added_hunk_app("a.rs", 1, &["foo one", "mid", "foo two", "foo three"], 100);
        // Cursor on the middle diff line (between first and third foo).
        cursor_on_nth_diff_line(&mut app, 1);
        let cursor_row = app.scroll;

        commit_search(&mut app, "foo");

        let state = app.search.as_ref().expect("search installed");
        assert_eq!(state.matches.len(), 3);
        // `current` must point at the first match whose row > cursor_row.
        assert!(
            state.matches[state.current].row > cursor_row,
            "expected current match after cursor row {}, got row {} (idx {})",
            cursor_row,
            state.matches[state.current].row,
            state.current,
        );
        // Specifically: the middle cursor is between match 0 ("foo one")
        // and match 1 ("foo two"). After-cursor = match 1.
        assert_eq!(state.current, 1);
    }

    #[test]
    fn search_commit_wraps_to_first_match_when_cursor_is_past_all_matches() {
        // When no match lives after the cursor, wrap around to the
        // global first match so `/foo<Enter>` always lands on SOMETHING
        // (never a no-op with matches.len() > 0).
        let mut app = added_hunk_app("a.rs", 1, &["foo a", "foo b", "trailing"], 100);
        // Cursor sits AFTER both matches.
        cursor_on_nth_diff_line(&mut app, 2);

        commit_search(&mut app, "foo");

        let state = app.search.as_ref().expect("search installed");
        assert_eq!(state.matches.len(), 2);
        assert_eq!(state.current, 0, "wrap-around lands on first match");
    }

    #[test]
    fn search_matches_rehydrate_clamps_current_when_matches_shrink() {
        // Before recompute: 2 matches, current=1. After recompute the
        // underlying file drops one match. `current` must clamp into
        // range so `n`/`N` never panic.
        let mut app = single_added_app("a.rs", "foo foo");
        let match_count = install_search(&mut app, "foo", 1);
        assert_eq!(match_count, 2);

        // Recompute with only one `foo` remaining.
        app.apply_computed_files(vec![single_added_file("a.rs", "foo bar", 100)]);

        let state = app.search.as_ref().unwrap();
        assert_eq!(state.matches.len(), 1);
        assert!(
            state.current < state.matches.len(),
            "current ({}) must be < matches.len ({})",
            state.current,
            state.matches.len(),
        );
    }

    // ---- M4 slice 5: `e` external editor --------------------------

    #[test]
    fn build_editor_invocation_vim_uses_plus_line_format() {
        let inv = build_editor_invocation(Some("vim"), 42, Path::new("/tmp/foo.rs"))
            .expect("some invocation");
        assert_eq!(inv.program, "vim");
        assert_eq!(inv.args, vec!["+42", "/tmp/foo.rs"]);
    }

    #[test]
    fn build_editor_invocation_nvim_preserves_leading_args_and_plus_line() {
        let inv = build_editor_invocation(Some("nvim -f"), 7, Path::new("x.rs")).unwrap();
        assert_eq!(inv.program, "nvim");
        assert_eq!(inv.args, vec!["-f", "+7", "x.rs"]);
    }

    #[test]
    fn build_editor_invocation_zed_uses_colon_line_format() {
        let inv = build_editor_invocation(Some("zed"), 10, Path::new("a.rs")).unwrap();
        assert_eq!(inv.program, "zed");
        assert_eq!(inv.args, vec!["a.rs:10"]);
    }

    #[test]
    fn build_editor_invocation_code_with_flags_uses_colon_format() {
        let inv = build_editor_invocation(Some("code --wait --new-window"), 1, Path::new("a.rs"))
            .unwrap();
        assert_eq!(inv.program, "code");
        assert_eq!(inv.args, vec!["--wait", "--new-window", "a.rs:1"]);
    }

    #[test]
    fn build_editor_invocation_helix_uses_colon_format() {
        let inv = build_editor_invocation(Some("hx"), 5, Path::new("b.rs")).unwrap();
        assert_eq!(inv.program, "hx");
        assert_eq!(inv.args, vec!["b.rs:5"]);
    }

    #[test]
    fn build_editor_invocation_nano_uses_plus_line_format() {
        let inv = build_editor_invocation(Some("nano"), 3, Path::new("c.py")).unwrap();
        assert_eq!(inv.program, "nano");
        assert_eq!(inv.args, vec!["+3", "c.py"]);
    }

    #[test]
    fn build_editor_invocation_returns_none_when_env_is_unset() {
        assert!(build_editor_invocation(None, 1, Path::new("x.rs")).is_none());
    }

    #[test]
    fn build_editor_invocation_returns_none_when_env_is_blank() {
        assert!(build_editor_invocation(Some("   "), 1, Path::new("x.rs")).is_none());
        assert!(build_editor_invocation(Some(""), 1, Path::new("x.rs")).is_none());
    }

    #[test]
    fn open_in_editor_pairs_cursor_target_line_with_env_program() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut app = scar_app_with_added_line(&tmp, "src/bar.rs", "a\nb\n", 2, "b");
        cursor_on_nth_diff_line(&mut app, 0);

        let inv = app.open_in_editor(Some("vim")).expect("invocation");
        assert_eq!(inv.program, "vim");
        assert_eq!(inv.args.len(), 2);
        assert_eq!(inv.args[0], "+2");
        assert_eq!(
            inv.args[1],
            tmp.path().join("src/bar.rs").display().to_string()
        );
    }

    #[test]
    fn open_in_editor_returns_none_when_cursor_is_on_file_header() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut app = scar_app_with_added_line(&tmp, "lib.rs", "x\n", 1, "x");
        let header = file_header_row(&app);
        app.scroll_to(header);

        assert!(app.open_in_editor(Some("vim")).is_none());
    }

    #[test]
    fn open_in_editor_returns_none_when_env_is_empty() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut app = scar_app_with_added_line(&tmp, "a.rs", "x\n", 1, "x");
        cursor_on_nth_diff_line(&mut app, 0);
        assert!(app.open_in_editor(None).is_none());
    }

    // ---- M4 slice 4: `x` hunk revert confirmation dialog ----------

    /// Build a real git repo in `tmp` with a single committed file,
    /// modify it so there's a one-line diff, bootstrap an App
    /// against it, and return both the App and the worktree file
    /// path. Lets `x`-key tests exercise the real `git apply
    /// --reverse` path end-to-end.
    fn revert_app_with_real_repo(
        tmp: &tempfile::TempDir,
        rel_path: &str,
        committed: &str,
        modified: &str,
    ) -> (App, PathBuf) {
        use std::process::Command;
        let repo = tmp.path();
        let run = |args: &[&str]| {
            let status = Command::new("git")
                .args(args)
                .current_dir(repo)
                .status()
                .expect("git");
            assert!(status.success(), "git {args:?} failed");
        };
        run(&["init", "--quiet", "--initial-branch=main"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "kizu test"]);

        let abs = repo.join(rel_path);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent).expect("parent");
        }
        std::fs::write(&abs, committed).expect("seed");
        run(&["add", rel_path]);
        run(&["commit", "--quiet", "-m", "seed"]);
        std::fs::write(&abs, modified).expect("modify");

        let app = App::bootstrap(repo.to_path_buf()).expect("bootstrap");
        (app, abs)
    }

    #[test]
    fn handle_key_x_opens_revert_confirm_overlay_without_touching_file() {
        let tmp = tempfile::tempdir().expect("tmp");
        let (mut app, abs) = revert_app_with_real_repo(
            &tmp,
            "foo.rs",
            "fn one() {}\n",
            "fn one() {}\nfn two() {}\n",
        );
        cursor_on_nth_diff_line(&mut app, 0);

        app.handle_key(key(KeyCode::Char('x')));

        let state = app
            .revert_confirm
            .as_ref()
            .expect("x must open the confirmation overlay");
        assert_eq!(state.file_path, PathBuf::from("foo.rs"));
        assert_eq!(
            std::fs::read_to_string(&abs).expect("read"),
            "fn one() {}\nfn two() {}\n",
            "opening the overlay must not touch the file"
        );
    }

    #[test]
    fn revert_confirm_n_cancels_without_reverting() {
        let tmp = tempfile::tempdir().expect("tmp");
        let (mut app, abs) = revert_app_with_real_repo(
            &tmp,
            "foo.rs",
            "fn one() {}\n",
            "fn one() {}\nfn two() {}\n",
        );
        cursor_on_nth_diff_line(&mut app, 0);
        app.handle_key(key(KeyCode::Char('x')));

        app.handle_key(key(KeyCode::Char('n')));

        assert!(app.revert_confirm.is_none(), "`n` must close the overlay");
        assert_eq!(
            std::fs::read_to_string(&abs).expect("read"),
            "fn one() {}\nfn two() {}\n",
            "`n` must not touch the worktree"
        );
        assert!(app.last_error.is_none());
    }

    #[test]
    fn revert_confirm_esc_cancels_without_reverting() {
        let tmp = tempfile::tempdir().expect("tmp");
        let (mut app, abs) = revert_app_with_real_repo(
            &tmp,
            "foo.rs",
            "fn one() {}\n",
            "fn one() {}\nfn two() {}\n",
        );
        cursor_on_nth_diff_line(&mut app, 0);
        app.handle_key(key(KeyCode::Char('x')));
        app.handle_key(key(KeyCode::Esc));

        assert!(app.revert_confirm.is_none());
        assert_eq!(
            std::fs::read_to_string(&abs).expect("read"),
            "fn one() {}\nfn two() {}\n",
            "Esc must not touch the worktree"
        );
    }

    #[test]
    fn revert_confirm_y_reverts_hunk_on_disk() {
        let tmp = tempfile::tempdir().expect("tmp");
        let (mut app, abs) = revert_app_with_real_repo(
            &tmp,
            "foo.rs",
            "fn one() {}\n",
            "fn one() {}\nfn two() {}\n",
        );
        cursor_on_nth_diff_line(&mut app, 0);
        app.handle_key(key(KeyCode::Char('x')));

        app.handle_key(key(KeyCode::Char('y')));

        assert!(
            app.revert_confirm.is_none(),
            "confirm must close the overlay"
        );
        assert_eq!(
            std::fs::read_to_string(&abs).expect("read"),
            "fn one() {}\n",
            "`y` must run git apply --reverse on the target hunk"
        );
        assert!(
            app.last_error.is_none(),
            "successful revert leaves last_error clean"
        );
    }

    #[test]
    fn revert_confirm_enter_also_confirms() {
        let tmp = tempfile::tempdir().expect("tmp");
        let (mut app, abs) = revert_app_with_real_repo(
            &tmp,
            "foo.rs",
            "fn one() {}\n",
            "fn one() {}\nfn two() {}\n",
        );
        cursor_on_nth_diff_line(&mut app, 0);
        app.handle_key(key(KeyCode::Char('x')));
        app.handle_key(key(KeyCode::Enter));

        assert!(app.revert_confirm.is_none());
        assert_eq!(
            std::fs::read_to_string(&abs).expect("read"),
            "fn one() {}\n"
        );
    }

    #[test]
    fn handle_key_x_on_file_header_row_is_noop() {
        let tmp = tempfile::tempdir().expect("tmp");
        let (mut app, _abs) = revert_app_with_real_repo(
            &tmp,
            "foo.rs",
            "fn one() {}\n",
            "fn one() {}\nfn two() {}\n",
        );
        let file_header_row = file_header_row(&app);
        app.scroll_to(file_header_row);

        app.handle_key(key(KeyCode::Char('x')));

        assert!(
            app.revert_confirm.is_none(),
            "x on the file header must not open the overlay"
        );
    }

    #[test]
    fn normal_keys_are_inert_while_revert_confirm_overlay_is_open() {
        let tmp = tempfile::tempdir().expect("tmp");
        let (mut app, _abs) = revert_app_with_real_repo(
            &tmp,
            "foo.rs",
            "fn one() {}\n",
            "fn one() {}\nfn two() {}\n",
        );
        cursor_on_nth_diff_line(&mut app, 0);
        app.handle_key(key(KeyCode::Char('x')));

        // `q` while the overlay is open must CANCEL the dialog, not quit.
        app.handle_key(key(KeyCode::Char('q')));

        assert!(!app.should_quit, "q while overlay open must not quit");
        assert!(
            app.revert_confirm.is_none(),
            "any key other than y/Y/Enter closes the dialog"
        );
    }

    #[test]
    fn scar_target_line_maps_cursor_on_deleted_line_to_next_live_line() {
        // hunk: Added "x" (new file line 10), Deleted "y" (no new pos),
        //       Added "z" (new file line 11). Cursor on the Deleted
        //       row should resolve to line 11 — the replacement.
        let mut app = single_hunk_app(
            "a.rs",
            10,
            vec![
                diff_line(LineKind::Added, "x"),
                diff_line(LineKind::Deleted, "y"),
                diff_line(LineKind::Added, "z"),
            ],
            100,
        );
        // Cursor on the Deleted row (2nd diff line in the hunk = nth=1).
        cursor_on_nth_diff_line(&mut app, 1);
        let (_, line) = app.scar_target_line().expect("target");
        assert_eq!(
            line, 11,
            "deleted-row cursor must map to the next live line"
        );
    }

    #[test]
    fn scar_target_line_on_all_deleted_hunk_returns_hunk_new_start() {
        // A pure-deletion hunk has no Added/Context lines in the
        // new file. The cursor on any deleted row should still
        // resolve to `hunk.new_start` — the position in the new
        // file where the deletion gap sits. The scar will land
        // above that line (which may be a surviving neighbour or
        // the end of the file).
        let mut app = single_hunk_app(
            "a.rs",
            5,
            vec![
                diff_line(LineKind::Deleted, "gone_a"),
                diff_line(LineKind::Deleted, "gone_b"),
                diff_line(LineKind::Deleted, "gone_c"),
            ],
            100,
        );
        // Cursor on the first deleted row.
        cursor_on_nth_diff_line(&mut app, 0);
        let (_, line) = app.scar_target_line().expect("target");
        assert_eq!(
            line, 5,
            "pure-deletion hunk cursor must resolve to hunk.new_start"
        );

        // Middle deleted row — same target.
        cursor_on_nth_diff_line(&mut app, 1);
        let (_, line) = app.scar_target_line().expect("target");
        assert_eq!(line, 5);

        // Last deleted row — same target.
        cursor_on_nth_diff_line(&mut app, 2);
        let (_, line) = app.scar_target_line().expect("target");
        assert_eq!(line, 5);
    }

    #[test]
    fn scar_on_deleted_line_writes_above_next_surviving_line() {
        // End-to-end: commit "a\nb\nc\n", worktree becomes "a\nc\n"
        // (line "b" deleted). Cursor on the deleted "b" row, press
        // `a` → scar should land above line 2 of the new file
        // (which is "c", the survivor after the deletion).
        let tmp = tempfile::tempdir().expect("tmp");
        let (mut app, abs) = revert_app_with_real_repo(&tmp, "del.rs", "a\nb\nc\n", "a\nc\n");
        // Find the deleted row (LineKind::Deleted for "b").
        let del_row = first_diff_row_with_kind(&app, LineKind::Deleted);
        app.scroll_to(del_row);

        app.handle_key(key(KeyCode::Char('a')));

        let after = std::fs::read_to_string(&abs).expect("read back");
        assert_eq!(
            after, "a\n// @kizu[ask]: explain this change\nc\n",
            "scar on a deleted row must land above the next surviving line"
        );
    }

    #[test]
    fn scar_on_all_deleted_hunk_writes_at_deletion_point() {
        // Commit "a\nb\nc\nd\n", worktree "a\nd\n" (lines b,c
        // deleted). The hunk's new_start points at the gap between
        // "a" and "d". Scar should land above line 2 of the new
        // file (which is "d").
        let tmp = tempfile::tempdir().expect("tmp");
        let (mut app, abs) = revert_app_with_real_repo(&tmp, "gap.rs", "a\nb\nc\nd\n", "a\nd\n");
        // Park on the first deleted row.
        let del_row = first_diff_row_with_kind(&app, LineKind::Deleted);
        app.scroll_to(del_row);

        app.handle_key(key(KeyCode::Char('a')));

        let after = std::fs::read_to_string(&abs).expect("read back");
        assert_eq!(
            after, "a\n// @kizu[ask]: explain this change\nd\n",
            "scar on all-deleted hunk must land at the deletion gap"
        );
    }

    // ---- scar undo stack ----

    #[test]
    fn insert_canned_scar_pushes_entry_to_undo_stack() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut app = scar_app_with_added_line(
            &tmp,
            "src/main.rs",
            "fn a() {}\nfn b() {}\n",
            2,
            "fn b() {}",
        );
        cursor_on_nth_diff_line(&mut app, 0);
        app.insert_canned_scar(ScarKind::Ask, SCAR_TEXT_ASK);

        assert_eq!(app.scar_undo_stack.len(), 1);
        let entry = &app.scar_undo_stack[0];
        assert_eq!(entry.line_1indexed, 2);
        assert_eq!(entry.rendered, "// @kizu[ask]: explain this change");
        assert_eq!(entry.path, tmp.path().join("src/main.rs"));
    }

    #[test]
    fn idempotent_scar_reinsert_does_not_grow_undo_stack() {
        // Pre-seed the file with the same scar one line above the
        // intended target. `insert_scar` sees the duplicate and
        // returns `None`, so the undo stack must stay empty (no
        // phantom entry that would otherwise cause `u` to "undo" a
        // write that never happened).
        let tmp = tempfile::tempdir().expect("tmp");
        let mut app = scar_app_with_context_line(
            &tmp,
            "src/main.rs",
            "fn a() {}\n// @kizu[ask]: explain this change\nfn b() {}\n",
            1,
            "fn a() {}",
        );
        // Use file-view mode to target line 3 (where `fn b` lives)
        // deterministically — the line above is the pre-existing scar.
        app.file_view = Some(file_view_state(
            "src/main.rs",
            vec![
                "fn a() {}".into(),
                "// @kizu[ask]: explain this change".into(),
                "fn b() {}".into(),
            ],
            2,
            true,
        ));
        app.insert_canned_scar(ScarKind::Ask, SCAR_TEXT_ASK);
        assert!(
            app.scar_undo_stack.is_empty(),
            "no-op insert must not push an undo entry"
        );
    }

    #[test]
    fn undo_scar_on_empty_stack_is_noop() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut app =
            scar_app_with_context_line(&tmp, "src/main.rs", "fn a() {}\n", 1, "fn a() {}");
        app.undo_scar();
        assert!(app.last_error.is_none(), "empty undo must not error");
    }

    #[test]
    fn undo_scar_removes_the_last_inserted_line() {
        let tmp = tempfile::tempdir().expect("tmp");
        let abs = tmp.path().join("src/main.rs");
        let before = "fn a() {}\nfn b() {}\n".to_string();
        let mut app = scar_app_with_added_line(&tmp, "src/main.rs", &before, 2, "fn b() {}");
        cursor_on_nth_diff_line(&mut app, 0);
        app.insert_canned_scar(ScarKind::Ask, SCAR_TEXT_ASK);
        let inserted = std::fs::read_to_string(&abs).expect("read after insert");
        assert_ne!(inserted, before);

        app.undo_scar();

        let after_undo = std::fs::read_to_string(&abs).expect("read after undo");
        assert_eq!(after_undo, before, "undo must restore the original file");
        assert!(app.scar_undo_stack.is_empty());
    }

    #[test]
    fn undo_scar_mismatch_surfaces_on_last_error_and_pops_stack() {
        let tmp = tempfile::tempdir().expect("tmp");
        let abs = tmp.path().join("src/main.rs");
        let mut app = scar_app_with_added_line(
            &tmp,
            "src/main.rs",
            "fn a() {}\nfn b() {}\n",
            2,
            "fn b() {}",
        );
        cursor_on_nth_diff_line(&mut app, 0);
        app.insert_canned_scar(ScarKind::Ask, SCAR_TEXT_ASK);
        // User edits the scar line between insert and undo.
        std::fs::write(&abs, "fn a() {}\n// @kizu[ask]: USER EDIT\nfn b() {}\n").expect("rewrite");

        app.undo_scar();

        assert!(
            app.last_error
                .as_deref()
                .map(|s| s.contains("edited"))
                .unwrap_or(false),
            "mismatched undo must set a last_error with 'edited', got {:?}",
            app.last_error,
        );
        assert!(app.scar_undo_stack.is_empty(), "entry must still pop");
    }

    #[test]
    fn undo_unwinds_multiple_inserts_in_reverse_order() {
        let tmp = tempfile::tempdir().expect("tmp");
        let abs = tmp.path().join("src/main.rs");
        let before = "fn a() {}\nfn b() {}\nfn c() {}\n".to_string();
        let mut app = scar_app_with_added_line(&tmp, "src/main.rs", &before, 2, "fn b() {}");
        cursor_on_nth_diff_line(&mut app, 0);
        // First insertion above line 2.
        app.insert_canned_scar(ScarKind::Ask, SCAR_TEXT_ASK);
        // Reuse the diff view layout — second insertion at same logical
        // position now lands above what shifted to line 3 (the scar
        // occupies line 2).
        app.insert_canned_scar(ScarKind::Reject, SCAR_TEXT_REJECT);
        assert_eq!(app.scar_undo_stack.len(), 2);

        // LIFO: the second scar must come off first.
        app.undo_scar();
        app.undo_scar();

        let after = std::fs::read_to_string(&abs).expect("read back");
        assert_eq!(after, before, "two undos must fully restore the file");
        assert!(app.scar_undo_stack.is_empty());
    }

    #[test]
    fn file_view_scar_target_line_is_cursor_plus_one() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut app =
            scar_app_with_added_line(&tmp, "src/main.rs", "line1\nline2\nline3\n", 1, "line1");
        // Fake a file-view state directly (bypassing open_file_view's
        // hunk-centering logic). `cursor: 1` is 0-indexed → scar
        // targets 1-indexed line 2.
        app.file_view = Some(file_view_state(
            "src/main.rs",
            vec!["line1".into(), "line2".into(), "line3".into()],
            1,
            true,
        ));
        let (path, line) = app.scar_target_line().expect("target");
        assert_eq!(line, 2);
        assert_eq!(path, tmp.path().join("src/main.rs"));
    }

    #[test]
    fn file_view_a_key_inserts_scar_at_cursor_line_and_u_undoes() {
        let tmp = tempfile::tempdir().expect("tmp");
        let abs = tmp.path().join("src/main.rs");
        let before = "line1\nline2\nline3\n".to_string();
        let mut app = scar_app_with_added_line(&tmp, "src/main.rs", &before, 1, "line1");
        // Enter file view programmatically (don't rely on the Enter
        // key, which requires the diff layout to have a hunk under
        // the cursor).
        app.file_view = Some(file_view_state(
            "src/main.rs",
            vec!["line1".into(), "line2".into(), "line3".into()],
            1,
            true,
        ));

        // `a` in file view must route to insert_canned_scar via
        // handle_file_view_key.
        app.handle_key(key(KeyCode::Char('a')));
        let inserted = std::fs::read_to_string(&abs).expect("read after insert");
        assert_eq!(
            inserted, "line1\n// @kizu[ask]: explain this change\nline2\nline3\n",
            "`a` in file view must scar above the cursor's 1-indexed line"
        );
        assert_eq!(app.scar_undo_stack.len(), 1);

        // `u` in file view reverses that one write.
        app.handle_key(key(KeyCode::Char('u')));
        let undone = std::fs::read_to_string(&abs).expect("read after undo");
        assert_eq!(undone, before, "`u` in file view must undo the scar");
        assert!(app.scar_undo_stack.is_empty());
    }

    #[test]
    fn scar_focus_sticks_across_subsequent_recomputes() {
        // Regression: the watcher fires a second `recompute_diff` ~300ms
        // after the scar write, which used to yank the cursor back to
        // the hunk header via `refresh_anchor`. A sticky `scar_focus`
        // pin should survive both that second recompute and any further
        // ones until the user explicitly navigates.
        let mut app = added_hunk_app(
            "src/main.rs",
            2,
            &["// @kizu[ask]: explain this change", "fn two() {}"],
            100,
        );
        app.scar_focus = Some((PathBuf::from("/tmp/fake/src/main.rs"), 2));
        // Directly drive `apply_computed_files` with the same file
        // set — simulates a watcher tick that re-delivers the diff.
        // The sticky focus must push the scroll to the scar row (line
        // 2 = the Added "// @kizu[ask]..." line), not the hunk header.
        let files_snapshot = app.files.clone();
        app.apply_computed_files(files_snapshot.clone());
        let scroll_after_first = app.scroll;
        assert!(
            matches!(
                app.layout.rows[scroll_after_first],
                RowKind::DiffLine { .. }
            ),
            "first recompute must land on a DiffLine, not a header"
        );
        // Second apply (another watcher tick): focus still sticks.
        app.apply_computed_files(files_snapshot);
        assert_eq!(
            app.scroll, scroll_after_first,
            "repeated recomputes must keep the cursor on the scar line"
        );
        assert!(app.scar_focus.is_some(), "focus persists until user nav");
    }

    #[test]
    fn scar_focus_cleared_by_navigation_keys() {
        // After any user navigation in normal mode, the sticky focus
        // pin is released so subsequent recomputes follow normal
        // anchoring rules (the user has explicitly moved elsewhere).
        let mut app = single_added_app("src/main.rs", "a");
        app.scar_focus = Some((PathBuf::from("/tmp/fake/src/main.rs"), 1));
        app.handle_key(key(KeyCode::Char('j')));
        assert!(
            app.scar_focus.is_none(),
            "j must clear scar_focus so the next recompute doesn't pull the cursor back"
        );
    }

    // ---- stream mode tests ----

    fn make_stream_event(tool: &str, path: &str, diff: Option<&str>, ts: u64) -> StreamEvent {
        let mut per_file = std::collections::BTreeMap::new();
        if let Some(d) = diff {
            per_file.insert(PathBuf::from(path), d.to_string());
        }
        StreamEvent {
            metadata: crate::hook::SanitizedEvent {
                session_id: None,
                hook_event_name: "PostToolUse".into(),
                tool_name: Some(tool.into()),
                file_paths: vec![PathBuf::from(path)],
                cwd: PathBuf::from("/tmp"),
                timestamp_ms: ts,
            },
            per_file_diffs: per_file,
        }
    }

    #[test]
    fn handle_event_log_skips_files_outside_project_root() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut app = fake_app(vec![]);
        app.root = tmp.path().to_path_buf();

        // Write an event file whose file_path is outside the project root.
        let events_dir = tmp.path().join("events");
        std::fs::create_dir_all(&events_dir).unwrap();
        let event = crate::hook::SanitizedEvent {
            session_id: None,
            hook_event_name: "PostToolUse".into(),
            tool_name: Some("Write".into()),
            file_paths: vec![PathBuf::from("/home/user/.config/kizu/config.toml")],
            cwd: tmp.path().to_path_buf(),
            timestamp_ms: 1000,
        };
        let json = serde_json::to_string(&event).unwrap();
        let event_path = events_dir.join("1000-Write.json");
        std::fs::write(&event_path, &json).unwrap();

        app.handle_event_log(event_path);
        assert!(
            app.stream_events.is_empty(),
            "events for files outside project root should be ignored"
        );
    }

    #[test]
    fn handle_event_log_accepts_files_inside_project_root() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut app = fake_app(vec![]);
        app.root = tmp.path().to_path_buf();

        let events_dir = tmp.path().join("events");
        std::fs::create_dir_all(&events_dir).unwrap();
        let event = crate::hook::SanitizedEvent {
            session_id: None,
            hook_event_name: "PostToolUse".into(),
            tool_name: Some("Write".into()),
            file_paths: vec![tmp.path().join("src/main.rs")],
            cwd: tmp.path().to_path_buf(),
            timestamp_ms: 2000,
        };
        let json = serde_json::to_string(&event).unwrap();
        let event_path = events_dir.join("2000-Write.json");
        std::fs::write(&event_path, &json).unwrap();

        app.handle_event_log(event_path);
        assert_eq!(
            app.stream_events.len(),
            1,
            "events for files inside project root should be accepted"
        );
    }

    #[test]
    fn build_stream_files_converts_events_to_file_diffs() {
        let events = vec![
            make_stream_event(
                "Write",
                "src/auth.rs",
                Some("+fn verify() {}\n+  ok\n"),
                1700000000000,
            ),
            make_stream_event("Edit", "src/main.rs", Some("+use auth;\n"), 1700000001000),
        ];
        let files = build_stream_files(&events);
        assert_eq!(files.len(), 2);
        // First event
        assert_eq!(files[0].path, PathBuf::from("src/auth.rs"));
        assert_eq!(files[0].added, 2);
        assert!(files[0].header_prefix.as_ref().unwrap().contains("Write"));
        // Second event
        assert_eq!(files[1].path, PathBuf::from("src/main.rs"));
        assert_eq!(files[1].added, 1);
        assert!(files[1].header_prefix.as_ref().unwrap().contains("Edit"));
    }

    #[test]
    fn build_stream_files_empty_diff_produces_empty_hunk() {
        let events = vec![make_stream_event("Write", "a.rs", None, 1000)];
        let files = build_stream_files(&events);
        assert_eq!(files.len(), 1);
        match &files[0].content {
            DiffContent::Text(hunks) => assert!(hunks.is_empty()),
            _ => panic!("expected Text"),
        }
    }

    #[test]
    fn build_stream_files_produces_one_entry_per_path_for_multi_file_event() {
        // A single MultiEdit / multi-file Write touches several paths.
        // Every path must get its own FileDiff so the stream view does
        // not collapse secondary files onto the first path's header.
        let mut per_file = std::collections::BTreeMap::new();
        per_file.insert(
            PathBuf::from("src/a.rs"),
            "diff --git a/src/a.rs b/src/a.rs\n@@ -0,0 +1,1 @@\n+a\n".to_string(),
        );
        per_file.insert(
            PathBuf::from("src/b.rs"),
            "diff --git a/src/b.rs b/src/b.rs\n@@ -0,0 +1,1 @@\n+b\n".to_string(),
        );
        let events = vec![StreamEvent {
            metadata: crate::hook::SanitizedEvent {
                session_id: None,
                hook_event_name: "PostToolUse".into(),
                tool_name: Some("MultiEdit".into()),
                file_paths: vec![PathBuf::from("src/a.rs"), PathBuf::from("src/b.rs")],
                cwd: PathBuf::from("/tmp"),
                timestamp_ms: 1_700_000_000_000,
            },
            per_file_diffs: per_file,
        }];
        let files = build_stream_files(&events);
        assert_eq!(files.len(), 2, "one FileDiff per touched path");
        let paths: Vec<&PathBuf> = files.iter().map(|f| &f.path).collect();
        assert!(paths.contains(&&PathBuf::from("src/a.rs")));
        assert!(paths.contains(&&PathBuf::from("src/b.rs")));
        // Each must carry a non-empty hunk (their own `+` line).
        for f in &files {
            match &f.content {
                DiffContent::Text(hunks) => {
                    assert!(
                        !hunks.is_empty(),
                        "per-path FileDiff must have its own hunk, got empty for {:?}",
                        f.path
                    );
                }
                _ => panic!("expected Text content"),
            }
        }
    }

    #[test]
    fn toggle_view_mode_switches_between_diff_and_stream() {
        let mut app = fake_app(vec![]);
        assert_eq!(app.view_mode, ViewMode::Diff);
        app.toggle_view_mode();
        assert_eq!(app.view_mode, ViewMode::Stream);
        app.toggle_view_mode();
        assert_eq!(app.view_mode, ViewMode::Diff);
    }

    #[test]
    fn tab_key_toggles_view_mode() {
        let mut app = fake_app(vec![]);
        assert_eq!(app.view_mode, ViewMode::Diff);
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.view_mode, ViewMode::Stream);
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.view_mode, ViewMode::Diff);
    }

    #[test]
    fn compute_operation_diff_returns_new_lines_only() {
        let prev = "+added line 1\n context\n";
        let curr = "+added line 1\n+added line 2\n context\n";
        let op = super::compute_operation_diff(prev, curr);
        assert_eq!(op, "+added line 2\n");
    }

    #[test]
    fn compute_operation_diff_empty_when_identical() {
        let prev = "+line 1\n+line 2\n";
        let op = super::compute_operation_diff(prev, prev);
        assert!(op.is_empty());
    }

    #[test]
    fn compute_operation_diff_preserves_duplicate_added_lines() {
        // Edit adds another identical `}` line on top of an already-added `}`.
        // A set-based diff would drop this because `+}` is already in prev;
        // a multiset/count-based diff must preserve the second copy.
        let prev = "+fn a() {}\n+}\n";
        let curr = "+fn a() {}\n+}\n+}\n";
        let op = super::compute_operation_diff(prev, curr);
        assert_eq!(op, "+}\n", "second duplicate added line must survive");
    }

    #[test]
    fn compute_operation_diff_preserves_duplicate_blank_lines() {
        // Many real edits add blank lines. prev already has one blank,
        // curr has two. The NEW blank line must appear in op_diff.
        let prev = "+foo\n+\n bar\n";
        let curr = "+foo\n+\n+\n bar\n";
        let op = super::compute_operation_diff(prev, curr);
        assert_eq!(op, "+\n", "second blank-line addition must survive");
    }

    #[test]
    fn apply_reset_clears_stale_diff_snapshots() {
        // Previously, `R` would rewrite `baseline_sha` + `files` but
        // leave `diff_snapshots` pinned to the OLD baseline. The next
        // hook-log-event for a file in the map would then compute
        // op_diff against an outdated snapshot — semantic garbage.
        // The fix: clear the map on every successful reset so the
        // next event rebuilds from the new baseline.
        let mut app = fake_app(vec![]);
        app.diff_snapshots
            .insert(PathBuf::from("stale.rs"), "OLD\n".to_string());
        assert!(!app.diff_snapshots.is_empty());

        // Simulate a successful reset to a new baseline + branch.
        let effect = app.apply_reset(
            "new-sha-xxx".to_string(),
            Some("refs/heads/main".to_string()),
            Ok(Vec::new()),
        );
        assert_eq!(effect, super::KeyEffect::None);
        assert!(
            app.diff_snapshots.is_empty(),
            "stale diff snapshots must be dropped after a baseline reset"
        );
    }

    #[test]
    fn apply_reset_failure_preserves_diff_snapshots() {
        // If the reset transaction fails (new SHA unresolvable, etc.)
        // the app keeps showing the old diff — and must therefore
        // keep the snapshots that were valid against the OLD baseline,
        // otherwise the very next event would misattribute lines.
        let mut app = fake_app(vec![]);
        app.diff_snapshots
            .insert(PathBuf::from("keep.rs"), "content\n".to_string());

        let effect = app.apply_reset("new-sha".to_string(), None, Err(anyhow::anyhow!("boom")));
        assert_eq!(effect, super::KeyEffect::None);
        assert_eq!(
            app.diff_snapshots.get(&PathBuf::from("keep.rs")),
            Some(&"content\n".to_string()),
            "failed reset must not touch snapshot state",
        );
    }

    #[test]
    fn handle_event_log_accepts_path_with_symlink_variant() {
        // On macOS `/tmp` is a symlink to `/private/tmp` (and similarly
        // `/var/folders` → `/private/var/folders`). `git rev-parse
        // --show-toplevel` canonicalizes, so `app.root` ends up on the
        // `/private/...` side. But an agent hook that records the
        // current working directory may still write file_paths on the
        // symlinked side. A naive `starts_with` comparison silently
        // drops those events — which is exactly what the e2e stream
        // tests hit on macOS runners. `handle_event_log` must
        // canonicalize before matching so symlink-variant paths are
        // accepted.
        let tmp = tempfile::tempdir().unwrap();
        let Ok(canonical_root) = tmp.path().canonicalize() else {
            return; // tempdir not canonicalizable; nothing to test.
        };
        if tmp.path() == canonical_root {
            return; // No symlink divergence on this runner; skip.
        }

        let mut app = fake_app(vec![]);
        app.root = canonical_root.clone();

        let file_canonical = canonical_root.join("a.rs");
        std::fs::write(&file_canonical, "").unwrap();
        let symlinked_file = tmp.path().join("a.rs");

        let events_dir = canonical_root.join("events");
        std::fs::create_dir_all(&events_dir).unwrap();
        let event = crate::hook::SanitizedEvent {
            session_id: None,
            hook_event_name: "PostToolUse".into(),
            tool_name: Some("Write".into()),
            file_paths: vec![symlinked_file],
            cwd: canonical_root.clone(),
            timestamp_ms: 3000,
        };
        let event_path = events_dir.join("3000-Write.json");
        std::fs::write(&event_path, serde_json::to_string(&event).unwrap()).unwrap();

        app.handle_event_log(event_path);
        assert_eq!(
            app.stream_events.len(),
            1,
            "event whose file_path resolves to a path inside the canonical root must be accepted"
        );
    }

    #[test]
    fn handle_event_log_preserves_snapshot_on_git_error() {
        // When `git diff` fails (e.g. baseline SHA is missing after a
        // rebase that garbage-collected the old object), the previous
        // file snapshot MUST NOT be clobbered — otherwise the next
        // event for the same file will compute op_diff against an
        // empty baseline and emit the entire cumulative diff as
        // "what this operation changed", which is wrong.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let mut app = fake_app(vec![]);
        app.root = root.to_path_buf();
        // Bogus baseline — every `git diff <bogus> -- file` call will fail.
        app.baseline_sha = "0000000000000000000000000000000000000000".to_string();

        let file = root.join("foo.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();

        // Seed a realistic prior snapshot as if a previous event had
        // captured the cumulative baseline → current diff.
        let prev_diff = "@@ -1 +1 @@\n-fn main() {}\n+fn main() { 1 }\n".to_string();
        app.diff_snapshots.insert(file.clone(), prev_diff.clone());

        let events_dir = root.join("events");
        std::fs::create_dir_all(&events_dir).unwrap();
        let event = crate::hook::SanitizedEvent {
            session_id: None,
            hook_event_name: "PostToolUse".into(),
            tool_name: Some("Edit".into()),
            file_paths: vec![file.clone()],
            cwd: root.to_path_buf(),
            timestamp_ms: 1000,
        };
        let event_path = events_dir.join("1000-Edit.json");
        std::fs::write(&event_path, serde_json::to_string(&event).unwrap()).unwrap();

        app.handle_event_log(event_path);

        assert_eq!(
            app.diff_snapshots.get(&file),
            Some(&prev_diff),
            "snapshot must survive a failing `git diff` so the next event is still accurate"
        );
    }

    #[test]
    fn handle_event_log_filters_by_explicit_bound_session_id() {
        // Under concurrent agent activity on the same repo, a foreign
        // session's events must not pollute the stream or
        // `diff_snapshots`. When the TUI was started with a
        // preconfigured `bound_session_id` (via `KIZU_SESSION_ID`
        // or a future CLI flag), `handle_event_log` drops events
        // from any other session. Auto-binding was intentionally
        // removed because it locked onto whichever session fired
        // first — often the wrong one under real concurrency.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp
            .path()
            .canonicalize()
            .unwrap_or_else(|_| tmp.path().to_path_buf());
        let mut app = fake_app(vec![]);
        app.root = root.clone();
        app.bound_session_id = Some("session-A".into());
        let events_dir = root.join("events");
        std::fs::create_dir_all(&events_dir).unwrap();

        // Matching session: accepted.
        let a = crate::hook::SanitizedEvent {
            session_id: Some("session-A".into()),
            hook_event_name: "PostToolUse".into(),
            tool_name: Some("Edit".into()),
            file_paths: vec![root.join("src/a.rs")],
            cwd: root.clone(),
            timestamp_ms: 10_000,
        };
        let a_path = events_dir.join("10000-Edit-aaa.json");
        std::fs::write(&a_path, serde_json::to_string(&a).unwrap()).unwrap();
        app.handle_event_log(a_path);
        assert_eq!(
            app.stream_events.len(),
            1,
            "event matching the bound session must be ingested"
        );

        // Foreign session: dropped.
        let b = crate::hook::SanitizedEvent {
            session_id: Some("session-B".into()),
            hook_event_name: "PostToolUse".into(),
            tool_name: Some("Edit".into()),
            file_paths: vec![root.join("src/a.rs")],
            cwd: root.clone(),
            timestamp_ms: 11_000,
        };
        let b_path = events_dir.join("11000-Edit-bbb.json");
        std::fs::write(&b_path, serde_json::to_string(&b).unwrap()).unwrap();
        app.handle_event_log(b_path);
        assert_eq!(
            app.stream_events.len(),
            1,
            "foreign-session event must not advance stream or diff_snapshots"
        );
    }

    #[test]
    fn handle_event_log_accepts_any_session_when_unbound() {
        // When no explicit binding exists (no env / no CLI), we
        // accept every session_id instead of auto-latching to the
        // first we see. This keeps concurrent-session trap-free:
        // users who want strict filtering opt in by setting
        // `KIZU_SESSION_ID`.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp
            .path()
            .canonicalize()
            .unwrap_or_else(|_| tmp.path().to_path_buf());
        let mut app = fake_app(vec![]);
        app.root = root.clone();
        assert!(app.bound_session_id.is_none());

        let events_dir = root.join("events");
        std::fs::create_dir_all(&events_dir).unwrap();
        for (i, sid) in ["session-A", "session-B"].iter().enumerate() {
            let ev = crate::hook::SanitizedEvent {
                session_id: Some((*sid).into()),
                hook_event_name: "PostToolUse".into(),
                tool_name: Some("Edit".into()),
                file_paths: vec![root.join(format!("src/{i}.rs"))],
                cwd: root.clone(),
                timestamp_ms: 20_000 + i as u64,
            };
            let path = events_dir.join(format!("2000{i}-Edit-{sid}.json"));
            std::fs::write(&path, serde_json::to_string(&ev).unwrap()).unwrap();
            app.handle_event_log(path);
        }
        assert_eq!(
            app.stream_events.len(),
            2,
            "unbound TUI must accept events from any session"
        );
        assert!(
            app.bound_session_id.is_none(),
            "unbound state must stay unbound; no auto-latch"
        );
    }

    #[test]
    fn handle_event_log_filters_out_events_predating_session_start() {
        // Two kizu sessions on the same repo: session B must not
        // ingest session A's historical events. The earlier
        // implementation used `clean_stale_events` to delete the
        // shared per-project events directory at startup, which
        // destroyed session A's live history. The replacement is a
        // timestamp filter: events whose `timestamp_ms` is older
        // than this session's start are silently dropped without
        // touching the shared files on disk.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp
            .path()
            .canonicalize()
            .unwrap_or_else(|_| tmp.path().to_path_buf());
        let mut app = fake_app(vec![]);
        app.root = root.clone();
        app.session_start_ms = 5_000;

        let events_dir = root.join("events");
        std::fs::create_dir_all(&events_dir).unwrap();
        let old_event = crate::hook::SanitizedEvent {
            session_id: Some("other-agent-session".into()),
            hook_event_name: "PostToolUse".into(),
            tool_name: Some("Edit".into()),
            file_paths: vec![root.join("src/a.rs")],
            cwd: root.clone(),
            timestamp_ms: 1_000, // earlier than session_start_ms
        };
        let old_path = events_dir.join("1000-Edit-xyz.json");
        std::fs::write(&old_path, serde_json::to_string(&old_event).unwrap()).unwrap();

        app.handle_event_log(old_path.clone());

        assert!(
            app.stream_events.is_empty(),
            "pre-session events must be filtered out of stream mode"
        );
        // The file itself must not be deleted — other sessions may
        // still own it. Only the ingest is suppressed.
        assert!(
            old_path.exists(),
            "filter must be non-destructive: leave the event file in place"
        );
    }

    #[test]
    fn replay_events_dir_ingests_files_written_during_startup_gap() {
        // During startup there is a window between
        // `clean_stale_events` and `watcher::start`. Any event
        // file written in that gap is never delivered via the
        // watcher, so the next event for that file would include
        // the dropped operation's contents in its `op_diff`.
        // A replay step must drain the directory once at startup
        // so no event is silently skipped.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp
            .path()
            .canonicalize()
            .unwrap_or_else(|_| tmp.path().to_path_buf());
        let mut app = fake_app(vec![]);
        app.root = root.clone();

        let events_dir = root.join("events");
        std::fs::create_dir_all(&events_dir).unwrap();
        let event = crate::hook::SanitizedEvent {
            session_id: None,
            hook_event_name: "PostToolUse".into(),
            tool_name: Some("Write".into()),
            file_paths: vec![root.join("src/a.rs")],
            cwd: root.clone(),
            timestamp_ms: 6000,
        };
        let event_path = events_dir.join("6000-Write-abc.json");
        std::fs::write(&event_path, serde_json::to_string(&event).unwrap()).unwrap();

        // `replay_events_dir` must scan the directory and feed each
        // event through `handle_event_log`, just as the watcher
        // would once it is armed.
        app.replay_events_dir(&events_dir);

        assert_eq!(
            app.stream_events.len(),
            1,
            "replay must ingest the pre-existing event, got: {:?}",
            app.stream_events
                .iter()
                .map(|e| e.metadata.tool_name.as_deref().unwrap_or("?"))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn replay_events_dir_orders_same_millisecond_events_chronologically() {
        // Two events in the same millisecond must replay in the
        // order they were written, not in alphabetical order by
        // tool name. The previous implementation sorted the
        // directory by filename, so `<ms>-<tool>-<uniq>.json`
        // placed `Edit` before `Write` regardless of which event
        // actually fired first. `handle_event_log` advances
        // `diff_snapshots` as it goes, so an out-of-order replay
        // fabricates the operation diff for whichever event lands
        // on the wrong side of the split.
        //
        // Assertion: `handle_event_log` is called in write order so
        // `stream_events` appears in the same order as the on-disk
        // write sequence (which we control via the uniqueness
        // suffix in the filename).
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp
            .path()
            .canonicalize()
            .unwrap_or_else(|_| tmp.path().to_path_buf());
        let mut app = fake_app(vec![]);
        app.root = root.clone();

        let events_dir = root.join("events");
        std::fs::create_dir_all(&events_dir).unwrap();

        // Same millisecond, different tool names. The *intended*
        // write order is `Write` first, then `Edit` — encoded by
        // a monotonic uniqueness prefix `001` < `002`.
        let first = crate::hook::SanitizedEvent {
            session_id: None,
            hook_event_name: "PostToolUse".into(),
            tool_name: Some("Write".into()),
            file_paths: vec![root.join("src/x.rs")],
            cwd: root.clone(),
            timestamp_ms: 20_000,
        };
        let second = crate::hook::SanitizedEvent {
            session_id: None,
            hook_event_name: "PostToolUse".into(),
            tool_name: Some("Edit".into()),
            file_paths: vec![root.join("src/x.rs")],
            cwd: root.clone(),
            timestamp_ms: 20_000,
        };
        // Use the production filename layout `<ms>-<tool>-<uniq>.json`
        // with tool names picked so filename lex sort disagrees with
        // write order: `Edit` < `Write` alphabetically, but the
        // intended chronological order is `Write` first. Replay
        // must honour the write order (derived from on-disk mtime)
        // rather than naïve filename sort.
        let first_path = events_dir.join("20000-Write-aaa.json");
        let second_path = events_dir.join("20000-Edit-zzz.json");
        std::fs::write(&first_path, serde_json::to_string(&first).unwrap()).unwrap();
        // Ensure distinct on-disk mtimes so the replay's tie-break
        // reflects the actual sequence.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&second_path, serde_json::to_string(&second).unwrap()).unwrap();

        app.replay_events_dir(&events_dir);

        let tools: Vec<&str> = app
            .stream_events
            .iter()
            .filter_map(|e| e.metadata.tool_name.as_deref())
            .collect();
        assert_eq!(
            tools,
            vec!["Write", "Edit"],
            "replay must honour monotonic write order, got {tools:?}"
        );
    }

    #[test]
    fn replay_events_dir_does_not_double_process_already_seen_events() {
        // If `replay_events_dir` runs and then the watcher also
        // delivers the same event (because the watcher was armed
        // after the file already existed on some notify backends),
        // the event must be recorded exactly once — otherwise
        // stream history shows phantom duplicates.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp
            .path()
            .canonicalize()
            .unwrap_or_else(|_| tmp.path().to_path_buf());
        let mut app = fake_app(vec![]);
        app.root = root.clone();

        let events_dir = root.join("events");
        std::fs::create_dir_all(&events_dir).unwrap();
        let event = crate::hook::SanitizedEvent {
            session_id: None,
            hook_event_name: "PostToolUse".into(),
            tool_name: Some("Edit".into()),
            file_paths: vec![root.join("src/b.rs")],
            cwd: root.clone(),
            timestamp_ms: 7000,
        };
        let event_path = events_dir.join("7000-Edit-def.json");
        std::fs::write(&event_path, serde_json::to_string(&event).unwrap()).unwrap();

        app.replay_events_dir(&events_dir);
        // Simulate the watcher later delivering the same file.
        app.handle_event_log(event_path.clone());

        assert_eq!(
            app.stream_events.len(),
            1,
            "same event must not be recorded twice"
        );
    }

    #[test]
    fn handle_event_log_rejects_parent_traversal_relative_paths() {
        // `normalize_event_path` must not accept a relative path that
        // escapes the repo via `..`. The earlier implementation only
        // checked `root.join(p).starts_with(root)` lexically, so
        // `../outside.rs` slipped through (the joined string starts
        // with `root` before lexical resolution). Any such path
        // would pollute stream mode with rows that sit outside the
        // worktree and whose git-diff lookups always fail.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp
            .path()
            .canonicalize()
            .unwrap_or_else(|_| tmp.path().to_path_buf());
        let mut app = fake_app(vec![]);
        app.root = root.clone();

        let events_dir = root.join("events");
        std::fs::create_dir_all(&events_dir).unwrap();
        let event = crate::hook::SanitizedEvent {
            session_id: None,
            hook_event_name: "PostToolUse".into(),
            tool_name: Some("Write".into()),
            // Traversal path — lexically begins with root but the
            // `..` escapes the worktree.
            file_paths: vec![PathBuf::from("../outside.rs")],
            cwd: root.clone(),
            timestamp_ms: 5000,
        };
        let event_path = events_dir.join("5000-Write.json");
        std::fs::write(&event_path, serde_json::to_string(&event).unwrap()).unwrap();

        app.handle_event_log(event_path);

        assert!(
            app.stream_events.is_empty(),
            "traversal path must not pass the repo-root filter, got {:?}",
            app.stream_events
                .iter()
                .map(|e| &e.metadata.file_paths)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn handle_event_log_matches_seeded_snapshot_via_repo_relative_key() {
        // `seed_diff_snapshots` keys the map by repo-relative paths
        // (from `git::compute_diff`). Agent hooks, however, usually
        // emit **absolute** paths. Without normalization, the seeded
        // `src/foo.rs` entry is never found for an event on
        // `/<root>/src/foo.rs`, so the first event for a
        // pre-existing dirty file shows the entire cumulative diff
        // as one tool call instead of only the delta.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp
            .path()
            .canonicalize()
            .unwrap_or_else(|_| tmp.path().to_path_buf());
        let mut app = fake_app(vec![]);
        app.root = root.clone();

        // Seed map with the repo-relative key, mirroring the
        // `git::compute_diff` output shape that `seed_diff_snapshots`
        // uses in production.
        let rel = PathBuf::from("src/foo.rs");
        let seeded = "diff --git a/src/foo.rs b/src/foo.rs\n@@ -1 +1 @@\n-old\n+seed\n".to_string();
        app.diff_snapshots.insert(rel.clone(), seeded.clone());

        // The agent supplies an absolute path on the repo root.
        let abs = root.join(&rel);
        let events_dir = root.join("events");
        std::fs::create_dir_all(&events_dir).unwrap();
        let event = crate::hook::SanitizedEvent {
            session_id: None,
            hook_event_name: "PostToolUse".into(),
            tool_name: Some("Edit".into()),
            file_paths: vec![abs.clone()],
            cwd: root.clone(),
            timestamp_ms: 4000,
        };
        let event_path = events_dir.join("4000-Edit.json");
        std::fs::write(&event_path, serde_json::to_string(&event).unwrap()).unwrap();

        app.handle_event_log(event_path);

        // After the event fires, both the lookup side and the
        // insert side must use the repo-relative key so a subsequent
        // seed/event for the same file lands on the same entry.
        assert_eq!(
            app.diff_snapshots.keys().collect::<Vec<_>>(),
            vec![&rel],
            "snapshot map must not split one file across raw and repo-relative keys, \
             found keys: {:?}",
            app.diff_snapshots.keys().collect::<Vec<_>>()
        );
        // The seeded op_diff was `-old / +seed`. Regardless of what
        // the current `git diff` returns (likely Err because no real
        // repo), the seeded snapshot must have been consulted — so
        // there must be exactly one StreamEvent and it must be keyed
        // on the repo-relative path.
        assert_eq!(app.stream_events.len(), 1);
        let recorded = &app.stream_events[0];
        let paths: Vec<&PathBuf> = recorded.metadata.file_paths.iter().collect();
        assert_eq!(
            paths,
            vec![&rel],
            "StreamEvent.metadata.file_paths must be stored repo-relative, got {paths:?}"
        );
    }

    #[test]
    fn compute_operation_diff_preserves_repeated_context_lines() {
        // Two different hunks may share an identical context line
        // (e.g. a closing brace). If prev has one hunk containing ` }`
        // and curr adds a second hunk that also contains ` }`, the
        // second occurrence in curr must appear in op_diff.
        let prev = " }\n";
        let curr = " }\n+new\n }\n";
        let op = super::compute_operation_diff(prev, curr);
        // op contains +new and the second occurrence of context ` }`.
        assert!(op.contains("+new\n"));
        assert!(
            op.matches(" }\n").count() == 1,
            "one of the two context ` }}` lines is new to curr; exactly one copy should appear"
        );
    }
}
