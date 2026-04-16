use anyhow::{Context, Result, anyhow};
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Duration of a single hunk-to-hunk scroll animation. 150 ms lands in
/// the "noticeable but not slow" band and matches the research doc.
const SCROLL_ANIM_DURATION: Duration = Duration::from_millis(150);

use crate::git::{self, DiffContent, FileDiff, FileStatus, LineKind};
use crate::hook::SanitizedEvent;
use crate::scar::ScarKind;
use crate::watcher::{self, WatchEvent, WatchSource};

/// Which TUI view is currently active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    /// Main diff view — filesystem-based state view ("what does the repo look like now?").
    #[default]
    Diff,
    /// Stream mode — event-log-based operation history ("what did the agent do?").
    Stream,
}

/// One entry in the stream mode view. Combines the sanitized event
/// metadata (from `hook-log-event`) with optionally captured diff
/// snapshots for per-operation diff display.
#[derive(Debug, Clone)]
pub struct StreamEvent {
    pub metadata: SanitizedEvent,
    /// Per-operation diff captured by the TUI in real-time.
    /// `None` for events that occurred before the TUI was started.
    pub diff_snapshot: Option<String>,
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
    /// wants to hide from the attention surface. Keyed by
    /// `(relative file path, hunk.old_start)` so the mark survives
    /// a watcher-driven `compute_diff` as long as the hunk's
    /// pre-image anchor doesn't move — same fingerprint used by
    /// [`HunkAnchor`]. Space toggles; nothing is written to disk
    /// (see plans/v0.2.md M4).
    pub seen_hunks: BTreeSet<(PathBuf, usize)>,
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
    /// Active viewport-top tween. `None` when the renderer should
    /// draw at the logical target.
    pub anim: Option<ScrollAnim>,
    /// Line-wrap mode. When `true`, long diff lines wrap to the
    /// viewport width (preserving the `+`/`-`/` ` prefix on every
    /// continuation row) and a `¶` marker is drawn at the end of
    /// each logical line so real newlines can be distinguished from
    /// wrap boundaries. Toggled by the `w` key.
    pub wrap_lines: bool,
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
    /// Stream mode events, ordered by timestamp ascending.
    pub stream_events: Vec<StreamEvent>,
    /// Per-file diff snapshots used to compute per-operation diffs.
    /// Maps file path → most recent cumulative diff output.
    pub diff_snapshots: std::collections::HashMap<PathBuf, String>,
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

/// Insert a single character at `cursor_pos` (char index) and advance
/// the cursor. Works correctly with multi-byte characters.
fn edit_insert_char(text: &mut String, cursor_pos: &mut usize, c: char) {
    let byte_idx = text
        .char_indices()
        .nth(*cursor_pos)
        .map(|(i, _)| i)
        .unwrap_or(text.len());
    text.insert(byte_idx, c);
    *cursor_pos += 1;
}

/// Insert a string at `cursor_pos` (char index) and advance the
/// cursor by the number of inserted characters.
fn edit_insert_str(text: &mut String, cursor_pos: &mut usize, s: &str) {
    let byte_idx = text
        .char_indices()
        .nth(*cursor_pos)
        .map(|(i, _)| i)
        .unwrap_or(text.len());
    text.insert_str(byte_idx, s);
    *cursor_pos += s.chars().count();
}

/// Delete the character before `cursor_pos` and move the cursor back.
fn edit_backspace(text: &mut String, cursor_pos: &mut usize) {
    if *cursor_pos == 0 {
        return;
    }
    let remove_idx = *cursor_pos - 1;
    let byte_range = text
        .char_indices()
        .nth(remove_idx)
        .map(|(i, c)| i..i + c.len_utf8());
    if let Some(range) = byte_range {
        text.drain(range);
        *cursor_pos -= 1;
    }
}

/// Two ways the renderer can park the cursor inside the viewport.
/// Defaults to [`CursorPlacement::Centered`]; `z` toggles to
/// [`CursorPlacement::Top`] (the cursor sits at the viewport ceiling
/// and the selected hunk body reads downward from there — the
/// natural direction for diff reading).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorPlacement {
    Centered,
    Top,
}

impl CursorPlacement {
    /// Compute the viewport's top-row index given the cursor's logical
    /// row, the total layout size, and the viewport height. The result
    /// is clamped to `[0, total - height]` so we never reveal phantom
    /// rows past either end of the layout.
    pub fn viewport_top(self, cursor: usize, total: usize, height: usize) -> usize {
        if total <= height {
            return 0;
        }
        let max_top = total - height;
        let raw = match self {
            CursorPlacement::Centered => cursor.saturating_sub(height / 2),
            // Cursor at viewport row 0. The selected hunk flows
            // downward from there into the body.
            CursorPlacement::Top => cursor,
        };
        raw.min(max_top)
    }

    /// Short human label used in the footer indicator.
    pub fn label(self) -> &'static str {
        match self {
            CursorPlacement::Centered => "center",
            CursorPlacement::Top => "top",
        }
    }
}

/// Pre-computed layout for the scroll view. Built once per `recompute_diff`,
/// then sliced into a viewport at render time.
#[derive(Debug, Default, Clone)]
pub struct ScrollLayout {
    /// Every visible row in order.
    pub rows: Vec<RowKind>,
    /// `rows` indices that point at a `HunkHeader` — used by `j/k` to jump
    /// hunk-by-hunk regardless of how many context lines sit in between.
    pub hunk_starts: Vec<usize>,
    /// For each file in `App.files`, the row index of its first hunk header
    /// (or the file header for binaries / empty hunks). `None` only when the
    /// layout build couldn't produce any anchorable row for that file.
    pub file_first_hunk: Vec<Option<usize>>,
    /// `file_of_row[i]` is the index into `App.files` for whichever file row
    /// `i` belongs to. The footer reads this to display the current file.
    pub file_of_row: Vec<usize>,
    /// `(start, end_exclusive)` row spans of every contiguous `+`/`-` block
    /// across the entire layout. `J` / `K` walk these spans in *both*
    /// directions: short runs collapse to a one-press jump, long runs are
    /// walked in [`App::chunk_size`]-sized scroll chunks (= viewport
    /// height / 3), and once the cursor passes the end of a run the next
    /// press flows into the next run even when that run lives in a
    /// different file.
    pub change_runs: Vec<(usize, usize)>,
}

/// Default body height assumed before the first render has had a chance
/// to update [`App::last_body_height`]. 24 is the classic VT100 height.
const DEFAULT_BODY_HEIGHT: usize = 24;

/// Per-render map from logical row index → visual y offset, computed
/// against the current wrap body width. Every frame the renderer
/// rebuilds a fresh index (cheap: O(rows) with the 2000-row cap from
/// `SCROLL_ROW_LIMIT`) so scroll math can talk about visual y instead
/// of logical rows.
///
/// The key invariant: `prefix[i]` is the visual y-offset where logical
/// row `i` begins, and `prefix[i+1] - prefix[i]` is the visual height
/// of row `i`. In **nowrap** mode every row is exactly 1 visual row
/// tall, so `prefix` is `[0, 1, 2, …, n]` and `visual_y(row)` is the
/// identity — all existing logical-row tests stay numerically correct.
/// In **wrap** mode, diff lines whose content exceeds `body_width`
/// contribute multiple visual rows and the prefix becomes non-trivial.
///
/// Animation (`ScrollAnim`) and viewport placement operate over this
/// coordinate space, not logical rows. That's the crux of the wrap-mode
/// fix — logical-row scrolling against wrap rendering was pushing the
/// cursor off-screen because a few wrapped rows ahead of the cursor
/// could silently consume the entire viewport before the cursor's
/// logical row was ever emitted.
#[derive(Debug, Clone)]
pub struct VisualIndex {
    /// Cumulative visual y-offsets, length `rows.len() + 1`.
    /// `prefix[rows.len()]` is the total visual height of the layout.
    prefix: Vec<usize>,
    /// Wrap body width this index was built against. `None` means
    /// nowrap, in which case `prefix` is the identity mapping — kept
    /// on the value so downstream code (and tests) can tell at a
    /// glance whether visual and logical coordinates coincide.
    #[allow(dead_code)]
    pub body_width: Option<usize>,
}

impl VisualIndex {
    /// Build a fresh prefix sum against the current layout and the
    /// supplied wrap body width. Pass `None` for nowrap mode; the
    /// resulting index acts as the identity and keeps the legacy
    /// logical-row scroll model intact.
    pub fn build(layout: &ScrollLayout, files: &[FileDiff], body_width: Option<usize>) -> Self {
        let n = layout.rows.len();
        let mut prefix = Vec::with_capacity(n + 1);
        prefix.push(0);
        let mut acc = 0usize;
        for row in &layout.rows {
            let h = Self::row_visual_height(row, files, body_width);
            acc += h;
            prefix.push(acc);
        }
        Self { prefix, body_width }
    }

    /// Visual y offset where logical row `row_idx` begins.
    pub fn visual_y(&self, row_idx: usize) -> usize {
        self.prefix.get(row_idx).copied().unwrap_or(0)
    }

    /// Visual-row height of logical row `row_idx`. Falls back to 1
    /// for out-of-range indices so callers don't need to bounds-check.
    pub fn visual_height(&self, row_idx: usize) -> usize {
        match (self.prefix.get(row_idx), self.prefix.get(row_idx + 1)) {
            (Some(&a), Some(&b)) => b - a,
            _ => 1,
        }
    }

    /// Total visual height of the layout.
    pub fn total_visual(&self) -> usize {
        self.prefix.last().copied().unwrap_or(0)
    }

    /// Given a visual y offset, return `(logical_row, skip_within_row)`
    /// where `logical_row` is the logical row that contains y and
    /// `skip_within_row` is how many visual lines of that row sit at
    /// or above y. Used by the renderer to begin drawing mid-row
    /// when wrap pushes the viewport's top into the middle of a
    /// wrapped diff line.
    pub fn logical_at(&self, y: usize) -> (usize, usize) {
        if self.prefix.len() < 2 {
            return (0, 0);
        }
        // Clamp past-the-end to the last row's final visual line.
        let total = self.total_visual();
        if y >= total {
            let last = self.prefix.len() - 2;
            return (last, self.visual_height(last).saturating_sub(1));
        }
        // Binary search: smallest `i` such that prefix[i+1] > y.
        let mut lo = 0usize;
        let mut hi = self.prefix.len() - 1;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.prefix[mid + 1] > y {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        let within = y - self.prefix[lo];
        (lo, within)
    }

    fn row_visual_height(row: &RowKind, files: &[FileDiff], body_width: Option<usize>) -> usize {
        let Some(width) = body_width else {
            return 1;
        };
        let RowKind::DiffLine {
            file_idx,
            hunk_idx,
            line_idx,
        } = row
        else {
            return 1;
        };
        let Some(file) = files.get(*file_idx) else {
            return 1;
        };
        let DiffContent::Text(hunks) = &file.content else {
            return 1;
        };
        let Some(hunk) = hunks.get(*hunk_idx) else {
            return 1;
        };
        let Some(line) = hunk.lines.get(*line_idx) else {
            return 1;
        };
        let chars = line.content.chars().count();
        if chars == 0 {
            1
        } else {
            chars.div_ceil(width.max(1))
        }
    }
}

/// One displayable row in the scroll view. The renderer turns each variant
/// into a styled `Line`; the App layer cares about `(file_idx, hunk_idx)`
/// for navigation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowKind {
    /// `path  ── status ── +A/-D ── mtime`
    FileHeader { file_idx: usize },
    /// `@@ -... +... @@`
    HunkHeader { file_idx: usize, hunk_idx: usize },
    /// One ` `/`+`/`-` line within a hunk.
    DiffLine {
        file_idx: usize,
        hunk_idx: usize,
        line_idx: usize,
    },
    /// `[binary file - diff suppressed]`
    BinaryNotice { file_idx: usize },
    /// Visual breathing room between files.
    Spacer,
}

/// Identifies "the hunk the user is looking at" across `recompute_diff`.
/// `hunk_old_start` is enough of a fingerprint to find the same hunk even
/// when neighbouring hunks shift around.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HunkAnchor {
    pub path: PathBuf,
    pub hunk_old_start: usize,
}

/// Modal file picker state. `cursor` indexes into `picker_results()`, not
/// into `App.files` directly.
#[derive(Debug, Clone, Default)]
pub struct PickerState {
    pub query: String,
    pub cursor: usize,
}

/// Free-text scar input overlay. The `c` key enters this mode when the
/// cursor is on a scar-able row; `Enter` commits the accumulated
/// [`Self::body`] as a `@kizu[free]:` scar above the target line and
/// `Esc` cancels without touching the file. The target is captured at
/// entry time (not re-read on commit) so that a watcher-driven diff
/// recompute during typing cannot silently retarget the write.
#[derive(Debug, Clone)]
pub struct ScarCommentState {
    pub target_path: PathBuf,
    pub target_line: usize,
    pub body: String,
    /// Cursor position as a **char index** (not byte offset).
    /// 0 = before the first character, `body.chars().count()` = end.
    pub cursor_pos: usize,
}

/// One hit inside the scroll layout. `row` is the logical layout
/// row index (suitable for `scroll_to`); `byte_start` / `byte_end`
/// delimit the match inside the row's diff-line content for inline
/// highlighting in a later M4b slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchLocation {
    pub row: usize,
    pub byte_start: usize,
    pub byte_end: usize,
}

/// Active search query + the row hits it produced, plus a cursor
/// into that hit list that `n` / `N` advance. Created by confirming
/// the [`SearchInputState`] composer; lives until the next `/` or
/// until a recompute invalidates the row indices.
#[derive(Debug, Clone)]
pub struct SearchState {
    // Reserved for M4b UI slice (footer echo + recompute
    // rehydration). Dead_code for now so clippy-as-error builds
    // don't fail between slices.
    #[allow(dead_code)]
    pub query: String,
    pub matches: Vec<MatchLocation>,
    pub current: usize,
}

/// Transient query-composing overlay. `/` opens it, typing appends
/// to `query`, Backspace deletes, Enter confirms into a
/// [`SearchState`], Esc cancels without touching the confirmed state.
#[derive(Debug, Clone, Default)]
pub struct SearchInputState {
    pub query: String,
    /// Cursor position as a char index within `query`.
    pub cursor_pos: usize,
}

/// Find every occurrence of `query` across the **DiffLine** rows of
/// `layout`, in row order. Empty queries return an empty vector so
/// callers can treat "no matches" and "no query" identically.
///
/// Case handling is **smart case** (vim-style): a query with no
/// uppercase characters matches case-insensitively, anything with
/// at least one uppercase character matches case-sensitively.
/// `byte_end` is guaranteed to be a UTF-8 char boundary because
/// `str::find` always returns a char-boundary-aligned index.
pub fn find_matches(layout: &ScrollLayout, files: &[FileDiff], query: &str) -> Vec<MatchLocation> {
    if query.is_empty() {
        return Vec::new();
    }
    let case_sensitive = query.chars().any(|c| c.is_uppercase());
    let needle: String = if case_sensitive {
        query.to_string()
    } else {
        query.to_lowercase()
    };

    let mut out = Vec::new();
    for (row_idx, row) in layout.rows.iter().enumerate() {
        let RowKind::DiffLine {
            file_idx,
            hunk_idx,
            line_idx,
        } = row
        else {
            continue;
        };
        let Some(file) = files.get(*file_idx) else {
            continue;
        };
        let DiffContent::Text(hunks) = &file.content else {
            continue;
        };
        let Some(hunk) = hunks.get(*hunk_idx) else {
            continue;
        };
        let Some(line) = hunk.lines.get(*line_idx) else {
            continue;
        };

        // For smart-case insensitive matching we lowercase the
        // haystack too. `str::to_lowercase` can change byte length
        // under Unicode (e.g. `İ` → `i̇`), so we fall back to
        // ASCII-only needles for the insensitive path to keep
        // byte offsets meaningful. Non-ASCII lowercase queries
        // degrade to case-sensitive matching, which is a clean
        // failure mode.
        let ascii_only = needle.is_ascii() && line.content.is_ascii();
        let (haystack, search_needle): (String, String) = if case_sensitive || !ascii_only {
            (line.content.clone(), needle.clone())
        } else {
            (line.content.to_ascii_lowercase(), needle.clone())
        };

        let mut start = 0;
        while let Some(idx) = haystack[start..].find(&search_needle) {
            let byte_start = start + idx;
            let byte_end = byte_start + search_needle.len();
            out.push(MatchLocation {
                row: row_idx,
                byte_start,
                byte_end,
            });
            if byte_end == start {
                // Defensive: empty needles already bail at the
                // top, but if a future code path sends an empty
                // after normalization we must not spin forever.
                break;
            }
            start = byte_end;
        }
    }
    out
}

/// Full-file zoom view entered via `Enter` on a hunk. The user
/// sees the entire worktree file with diff-touched lines
/// highlighted in `BG_ADDED` / `BG_DELETED`. `Esc` or `Enter`
/// returns to the normal scroll view at the cursor position
/// captured at entry time.
///
/// Navigation uses the same keys as normal mode (`j`/`k`/`J`/`K`/
/// `g`/`G`/`Ctrl-d`/`Ctrl-u`) but addresses 1-indexed file lines
/// instead of layout rows. Scar keys are deferred to a later
/// slice.
#[derive(Debug, Clone)]
pub struct FileViewState {
    #[allow(dead_code)]
    pub file_idx: usize,
    pub path: PathBuf,
    pub return_scroll: usize,
    pub lines: Vec<String>,
    pub line_bg: std::collections::HashMap<usize, ratatui::style::Color>,
    pub cursor: usize,
    pub scroll_top: usize,
}

/// Confirmation overlay for hunk revert (`x` key). Holds the
/// `(file_idx, hunk_idx)` captured the moment the user pressed `x`
/// so a watcher-driven recompute while the dialog is open cannot
/// re-target the operation. `y`/`Y`/`Enter` confirms and runs
/// `git apply --reverse`; any other key closes the overlay without
/// touching the worktree. See plans/v0.2.md M4 Decision Log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevertConfirmState {
    pub file_idx: usize,
    pub hunk_idx: usize,
    pub file_path: PathBuf,
    /// Stable hunk identity: `old_start` captured when the dialog was
    /// opened. Used in `confirm_revert` to re-resolve the hunk by
    /// identity (path + old_start) instead of trusting the stale
    /// index pair, which can drift after a watcher-driven refresh.
    pub hunk_old_start: usize,
}

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
        let diff = git::compute_diff(&root, &baseline_sha);
        Self::bootstrap_with_diff(
            root,
            git_dir,
            common_git_dir,
            current_branch_ref,
            baseline_sha,
            diff,
        )
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
            picker: None,
            scar_comment: None,
            revert_confirm: None,
            file_view: None,
            search_input: None,
            search: None,
            seen_hunks: BTreeSet::new(),
            follow_mode: true,
            last_error: None,
            input_health: None,
            head_dirty: false,
            should_quit: false,
            last_body_height: Cell::new(DEFAULT_BODY_HEIGHT),
            last_body_width: Cell::new(None),
            visual_top: Cell::new(0.0),
            anim: None,
            wrap_lines: false,
            watcher_health: WatcherHealth::default(),
            highlighter: std::cell::OnceCell::new(),
            config,
            view_mode: ViewMode::default(),
            stream_events: Vec::new(),
            diff_snapshots: std::collections::HashMap::new(),
        };
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
    }

    /// Toggle between Diff and Stream view modes. Rebuilds `files`
    /// and `layout` from the appropriate data source so the existing
    /// scroll/render infrastructure handles both modes identically.
    pub fn toggle_view_mode(&mut self) {
        match self.view_mode {
            ViewMode::Diff => {
                self.view_mode = ViewMode::Stream;
                let stream_files = build_stream_files(&self.stream_events);
                self.apply_computed_files(stream_files);
            }
            ViewMode::Stream => {
                // Capture context from the stream cursor for position
                // estimation after switching back to diff mode.
                let target_path = self.current_file_path().map(|p| p.to_path_buf());
                let target_content = self.current_diff_line_content();
                self.view_mode = ViewMode::Diff;
                self.recompute_diff();
                if let Some(path) = target_path {
                    // Try content-based match first (survives line shifts),
                    // fall back to file header if the content can't be found.
                    if let Some(ref content) = target_content {
                        if !self.scroll_to_diff_content(&path, content) {
                            self.scroll_to_file(&path);
                        }
                    } else {
                        self.scroll_to_file(&path);
                    }
                }
            }
        }
    }

    /// Handle a new event-log file notification. Reads the event file,
    /// captures the per-operation diff snapshot, and appends to
    /// `stream_events`. Failures are silently ignored (non-critical).
    pub fn handle_event_log(&mut self, path: PathBuf) {
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return,
        };
        let event: SanitizedEvent = match serde_json::from_str(&content) {
            Ok(e) => e,
            Err(_) => return,
        };

        // Capture per-operation diff for each affected file.
        let mut operation_diff = String::new();

        for file_path in &event.file_paths {
            let current_diff = git::diff_single_file(&self.root, &self.baseline_sha, file_path)
                .unwrap_or_default();

            if let Some(prev) = self.diff_snapshots.get(file_path) {
                // We have a previous snapshot — compute the delta.
                let op_diff = compute_operation_diff(prev, &current_diff);
                if !op_diff.is_empty() {
                    if !operation_diff.is_empty() {
                        operation_diff.push('\n');
                    }
                    operation_diff.push_str(&op_diff);
                }
            }
            // If no previous snapshot exists (new file or first edit),
            // we record the current state as the baseline and produce
            // no diff for this event. The next event on this file will
            // correctly show only its delta.

            self.diff_snapshots.insert(file_path.clone(), current_diff);
        }

        let stream_event = StreamEvent {
            metadata: event,
            diff_snapshot: if operation_diff.is_empty() {
                None
            } else {
                Some(operation_diff)
            },
        };
        self.stream_events.push(stream_event);

        // If in stream mode, rebuild files/layout to include the new event.
        if self.view_mode == ViewMode::Stream {
            let stream_files = build_stream_files(&self.stream_events);
            self.apply_computed_files(stream_files);
        }
    }

    /// Remove stale event-log files from `<state_dir>/events/`.
    /// Called once at TUI startup so the stream view starts clean —
    /// events from before this session cannot carry a per-operation
    /// diff and would only show "[diff not captured]" noise.
    pub fn clean_stale_events(&self) {
        let dir = match crate::paths::events_dir(&self.root) {
            Some(d) if d.is_dir() => d,
            _ => return,
        };
        let Ok(read_dir) = std::fs::read_dir(&dir) else {
            return;
        };
        for entry in read_dir.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with('.') {
                continue;
            }
            if name_str.ends_with(".json") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    /// Seed `diff_snapshots` with the current cumulative diff for every
    /// file in `self.files`. Called once at startup so the first stream
    /// event correctly shows only the per-operation delta, not the
    /// entire session's cumulative diff.
    pub fn seed_diff_snapshots(&mut self) {
        for file in &self.files {
            let diff = git::diff_single_file(&self.root, &self.baseline_sha, &file.path)
                .unwrap_or_default();
            if !diff.is_empty() {
                self.diff_snapshots.insert(file.path.clone(), diff);
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
    fn apply_computed_files(&mut self, mut files: Vec<FileDiff>) {
        let picker_selected_path = self.picker_selected_path();
        // Stream mode files are already ordered by event timestamp and
        // must not have their mtime overwritten by the filesystem. Diff
        // mode files need filesystem mtime for chronological sorting.
        if self.view_mode != ViewMode::Stream {
            self.populate_mtimes(&mut files);
            files.sort_by(|a, b| a.mtime.cmp(&b.mtime));
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
        let diff = git::compute_diff(&self.root, &new_sha);
        self.apply_reset(new_sha, new_branch, diff)
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
                if branch_changed {
                    KeyEffect::ReconfigureWatcher
                } else {
                    KeyEffect::None
                }
            }
            Err(e) => {
                self.last_error = Some(format!("R: {e:#}"));
                // baseline_sha / current_branch_ref / head_dirty /
                // files intentionally untouched: the HEAD* warning
                // stays visible and the user keeps seeing the same
                // diff they had before R. Watcher also stays pinned
                // to the old branch, which is the correct behavior
                // for an aborted reset.
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

    /// Top-level key dispatch. Picker mode shadows the normal bindings.
    /// Returns a [`KeyEffect`] describing any post-dispatch work that
    /// the event loop must perform — currently only `R` can trigger
    /// a watcher reconfigure, but the same channel scales to future
    /// side-effects without threading explicit parameters through
    /// every handler.
    pub fn handle_key(&mut self, key: KeyEvent) -> KeyEffect {
        if self.picker.is_some() {
            self.handle_picker_key(key);
            KeyEffect::None
        } else if self.scar_comment.is_some() {
            self.handle_scar_comment_key(key);
            KeyEffect::None
        } else if self.revert_confirm.is_some() {
            self.handle_revert_confirm_key(key);
            KeyEffect::None
        } else if self.search_input.is_some() {
            self.handle_search_input_key(key);
            KeyEffect::None
        } else if self.file_view.is_some() {
            self.handle_file_view_key(key)
        } else {
            self.handle_normal_key(key)
        }
    }

    // ---- normal-mode keys --------------------------------------------

    fn handle_normal_key(&mut self, key: KeyEvent) -> KeyEffect {
        // Quit shortcuts.
        if matches!(key.code, KeyCode::Char('q'))
            || (matches!(key.code, KeyCode::Char('c'))
                && key.modifiers.contains(KeyModifiers::CONTROL))
        {
            self.should_quit = true;
            return KeyEffect::None;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('d') => {
                    self.scroll_by(HALF_PAGE as isize);
                    self.follow_mode = false;
                    return KeyEffect::None;
                }
                KeyCode::Char('u') => {
                    self.scroll_by(-(HALF_PAGE as isize));
                    self.follow_mode = false;
                    return KeyEffect::None;
                }
                _ => {}
            }
        }

        match key.code {
            // Lowercase `j`/`k` + arrows are the *daily driver*: adaptive
            // motion that reads like continuous scrolling in long hunks
            // (chunk scroll) but collapses to a one-press hunk jump in
            // short hunks.
            //
            // v0.2 key remap (ADR-0015 / plans/v0.2.md M4):
            // - `J` / `K` now move the cursor by **exactly one visual row**.
            //   The old hunk-header jump behavior was relocated to `l` /
            //   `h` so add/delete scar decisions can be made row-by-row.
            // - `l` / `h` strictly jump to the next / previous hunk header,
            //   mirroring the pre-v0.2 `J` / `K` binding.
            // - Picker open moved from `Space` to `s` so `Space` can be
            //   used for the scar "seen" mark (wired up in a later M4 slice).
            KeyCode::Char('j') | KeyCode::Down => {
                self.next_change();
                self.follow_mode = false;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.prev_change();
                self.follow_mode = false;
            }
            KeyCode::Char('J') => {
                self.scroll_by(1);
                // Snap for 1-row moves: the 150ms ease-out tween
                // restarts on every key-repeat tick, causing visible
                // jitter when holding J/K. Clearing the animation
                // makes rapid single-row scrolling buttery smooth.
                self.anim = None;
                self.follow_mode = false;
            }
            KeyCode::Char('K') => {
                self.scroll_by(-1);
                self.anim = None;
                self.follow_mode = false;
            }
            KeyCode::Char('l') => {
                self.next_hunk();
                self.follow_mode = false;
            }
            KeyCode::Char('h') => {
                self.prev_hunk();
                self.follow_mode = false;
            }
            KeyCode::Char('g') => {
                self.scroll_to(0);
                self.follow_mode = false;
            }
            KeyCode::Char('G') => {
                self.scroll_to(self.last_row_index());
                self.follow_mode = false;
            }
            KeyCode::Tab => {
                self.toggle_view_mode();
            }
            KeyCode::Enter => {
                self.open_file_view();
            }
            KeyCode::Char(ch) => {
                // Remappable keys resolved via config. Navigation
                // keys (j/k/J/K/h/l/g/G) are handled above; these
                // are the action keys that users can remap in
                // ~/.config/kizu/config.toml.
                let k = &self.config.keys;
                if ch == k.follow {
                    self.follow_restore();
                } else if ch == k.picker {
                    self.open_picker();
                } else if ch == k.ask {
                    self.insert_canned_scar(ScarKind::Ask, SCAR_TEXT_ASK);
                } else if ch == k.reject {
                    self.insert_canned_scar(ScarKind::Reject, SCAR_TEXT_REJECT);
                } else if ch == k.comment {
                    self.open_scar_comment();
                } else if ch == k.revert {
                    self.open_revert_confirm();
                } else if ch == k.seen {
                    self.toggle_seen_current_hunk();
                } else if ch == k.search {
                    self.open_search_input();
                } else if ch == k.search_next {
                    self.search_jump_next();
                } else if ch == k.search_prev {
                    self.search_jump_prev();
                } else if ch == k.editor {
                    // Read `$EDITOR` at dispatch time (not at bootstrap)
                    // so users who `export EDITOR=` mid-session pick up
                    // the new value without restarting kizu.
                    let editor_cmd = if self.config.editor.command.is_empty() {
                        std::env::var("EDITOR").ok()
                    } else {
                        Some(self.config.editor.command.clone())
                    };
                    if let Some(inv) = self.open_in_editor(editor_cmd.as_deref()) {
                        return KeyEffect::OpenEditor(inv);
                    }
                } else if ch == k.reset_baseline {
                    return self.reset_baseline();
                } else if ch == k.cursor_placement {
                    self.toggle_cursor_placement();
                } else if ch == k.wrap_toggle {
                    self.toggle_wrap_lines();
                }
            }
            _ => {}
        }
        KeyEffect::None
    }

    // ---- picker-mode keys --------------------------------------------

    fn handle_picker_key(&mut self, key: KeyEvent) {
        // Ctrl-* shortcuts: navigation + cancel. Picker uses fzf-style
        // bindings so any non-control char (including 'j' / 'k') is a
        // filter character.
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('n') | KeyCode::Char('j') => self.picker_cursor_down(),
                KeyCode::Char('p') | KeyCode::Char('k') => self.picker_cursor_up(),
                KeyCode::Char('c') => self.close_picker(),
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Esc => self.close_picker(),
            KeyCode::Enter => {
                let results = self.picker_results();
                let cursor = self.picker.as_ref().map(|p| p.cursor).unwrap_or(0);
                let target = results.get(cursor).copied();
                self.close_picker();
                if let Some(file_idx) = target {
                    // Picker selection is an explicit manual navigation:
                    // a subsequent watcher-driven recompute must not
                    // snap the viewport back to the newest file via
                    // follow mode. Drop follow before jumping so the
                    // anchor captured by `scroll_to` sticks.
                    self.follow_mode = false;
                    self.jump_to_file_first_hunk(file_idx);
                }
            }
            KeyCode::Up => self.picker_cursor_up(),
            KeyCode::Down => self.picker_cursor_down(),
            KeyCode::Backspace => {
                if let Some(picker) = self.picker.as_mut() {
                    picker.query.pop();
                    picker.cursor = 0;
                }
            }
            KeyCode::Char(c) => {
                if let Some(picker) = self.picker.as_mut() {
                    picker.query.push(c);
                    picker.cursor = 0;
                }
            }
            _ => {}
        }
    }

    // ---- picker helpers ----------------------------------------------

    pub fn open_picker(&mut self) {
        self.picker = Some(PickerState::default());
    }

    pub fn close_picker(&mut self) {
        self.picker = None;
    }

    /// Indices into `self.files` for the picker's filtered view. The picker
    /// follows the file-picker convention of **newest first** even though
    /// `self.files` itself is stored in ascending mtime order: this way an
    /// empty-query → `Enter` lands on whatever the agent just touched.
    pub fn picker_results(&self) -> Vec<usize> {
        let needle = match &self.picker {
            Some(p) if !p.query.is_empty() => p.query.to_lowercase(),
            _ => return (0..self.files.len()).rev().collect(),
        };
        self.files
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, f)| f.path.to_string_lossy().to_lowercase().contains(&needle))
            .map(|(i, _)| i)
            .collect()
    }

    fn picker_selected_path(&self) -> Option<PathBuf> {
        let picker = self.picker.as_ref()?;
        let results = self.picker_results();
        let file_idx = results.get(picker.cursor).copied()?;
        self.files.get(file_idx).map(|f| f.path.clone())
    }

    fn refresh_picker_cursor(&mut self, selected_path: Option<&Path>) {
        let Some(cursor) = self.picker.as_ref().map(|p| p.cursor) else {
            return;
        };
        let results = self.picker_results();
        let new_cursor = if results.is_empty() {
            0
        } else if let Some(path) = selected_path {
            results
                .iter()
                .position(|&idx| self.files.get(idx).is_some_and(|f| f.path == path))
                .unwrap_or_else(|| cursor.min(results.len() - 1))
        } else {
            cursor.min(results.len() - 1)
        };
        if let Some(picker) = self.picker.as_mut() {
            picker.cursor = new_cursor;
        }
    }

    fn picker_cursor_down(&mut self) {
        let len = self.picker_results().len();
        if let Some(picker) = self.picker.as_mut()
            && len > 0
            && picker.cursor + 1 < len
        {
            picker.cursor += 1;
        }
    }

    fn picker_cursor_up(&mut self) {
        if let Some(picker) = self.picker.as_mut() {
            picker.cursor = picker.cursor.saturating_sub(1);
        }
    }

    // ---- navigation helpers ------------------------------------------

    pub fn scroll_by(&mut self, delta: isize) {
        let last = self.last_row_index();
        let body_width = self.last_body_width.get();
        if body_width.is_none() {
            // Nowrap fast path: one logical row == one visual row,
            // sub-row is always 0. Delegate to `scroll_to` which
            // resets `cursor_sub_row` unconditionally.
            let next = (self.scroll as isize + delta).clamp(0, last as isize) as usize;
            let next = self.normalize_row_target(next, delta.signum());
            self.scroll_to(next);
            return;
        }
        // Wrap mode: `delta` is interpreted as **visual rows** and
        // the cursor's position is the sum of its logical-row visual
        // y and its intra-row `cursor_sub_row`. ADR-0009 fix: the
        // previous implementation discarded the intra-row offset
        // returned by `VisualIndex::logical_at`, so Ctrl-d inside a
        // single long wrapped line stayed pinned to the same logical
        // row — `scroll_to(row)` treated the move as a no-op and
        // the user could never walk through a minified JSON edit.
        //
        // The fix routes wrap-mode navigation through `scroll_to_visual`
        // which preserves the sub-row offset so the visual cursor
        // genuinely advances.
        let vi = VisualIndex::build(&self.layout, &self.files, body_width);
        let cur_y = vi.visual_y(self.scroll) + self.cursor_sub_row;
        let new_y = (cur_y as isize + delta).max(0) as usize;
        let clamped = new_y.min(vi.total_visual().saturating_sub(1));
        let (target_row, target_sub) = vi.logical_at(clamped);
        let target_row = self.normalize_row_target(target_row.min(last), delta.signum());
        self.scroll_to_visual(target_row, target_sub, &vi);
    }

    /// Wrap-aware cursor move that preserves an intra-row visual
    /// offset. Nowrap callers must keep going through [`Self::scroll_to`]
    /// because they have no `VisualIndex` to clamp against and would
    /// just set `cursor_sub_row` to 0 anyway.
    ///
    /// Behaves like [`Self::scroll_to`] for the row side: starts a
    /// fresh animation when either the logical row or the sub-row
    /// actually changes, and updates the anchor. `sub_row` is
    /// clamped to the target row's visual height so callers can
    /// pass a speculative value without risking an out-of-range
    /// cursor.
    pub(crate) fn scroll_to_visual(&mut self, row: usize, sub_row: usize, vi: &VisualIndex) {
        let last = self.last_row_index();
        let target_row = row.min(last);
        let row_height = vi.visual_height(target_row).max(1);
        let clamped_sub = sub_row.min(row_height - 1);
        if (target_row, clamped_sub) != (self.scroll, self.cursor_sub_row) {
            self.anim = Some(ScrollAnim {
                from: self.visual_top.get(),
                start: Instant::now(),
                dur: SCROLL_ANIM_DURATION,
            });
        }
        self.scroll = target_row;
        self.cursor_sub_row = clamped_sub;
        self.update_anchor_from_scroll();
    }

    /// Animated scroll: move the cursor row to `row` and kick off a
    /// viewport-top tween from the currently drawn visual position.
    /// No animation is started when `row` is already the cursor row
    /// (a no-op), which keeps idle frames free of needless ticks.
    ///
    /// Also resets `cursor_sub_row` to 0 — every caller of
    /// `scroll_to` is a "jump to a specific row" operation (next
    /// hunk, previous hunk, g, G, follow restore, picker jump,
    /// anchor restore) and those should all land on the first
    /// visual line of the destination logical row. Wrap-mode
    /// **intra-row** walks go through [`Self::scroll_to_visual`]
    /// instead.
    pub fn scroll_to(&mut self, row: usize) {
        let last = self.last_row_index();
        let target = self.normalize_row_target(row.min(last), 1);
        if target != self.scroll || self.cursor_sub_row != 0 {
            self.anim = Some(ScrollAnim {
                from: self.visual_top.get(),
                start: Instant::now(),
                dur: SCROLL_ANIM_DURATION,
            });
        }
        self.scroll = target;
        self.cursor_sub_row = 0;
        self.update_anchor_from_scroll();
    }

    fn normalize_row_target(&self, row: usize, preferred_direction: isize) -> usize {
        if !matches!(self.layout.rows.get(row), Some(RowKind::Spacer)) {
            return row;
        }

        if preferred_direction >= 0
            && let Some(next) = self
                .layout
                .rows
                .iter()
                .enumerate()
                .skip(row + 1)
                .find_map(|(idx, r)| (!matches!(r, RowKind::Spacer)).then_some(idx))
        {
            return next;
        }
        if let Some(prev) = self.layout.rows[..row]
            .iter()
            .rposition(|r| !matches!(r, RowKind::Spacer))
        {
            return prev;
        }
        row
    }

    /// Mark the active animation as finished if enough time has passed.
    /// Returns `true` while an animation is still running, `false` once
    /// the run loop can stop scheduling frame ticks. Pure (`&mut self`
    /// only for the clear side-effect) so tests can inject `now`.
    pub fn tick_anim(&mut self, now: Instant) -> bool {
        let Some(anim) = self.anim else {
            return false;
        };
        let done = now.saturating_duration_since(anim.start) >= anim.dur;
        if done {
            self.anim = None;
            false
        } else {
            true
        }
    }

    /// Animated variant of [`Self::viewport_top`]: feeds the logical
    /// target through the active [`ScrollAnim`], sampling at `now`.
    /// Stores the result in [`Self::visual_top`] so the next animation
    /// kick-off starts from the exact row the last frame drew.
    ///
    /// This is the **nowrap** helper: it operates purely in logical
    /// row units and is retained for the existing centering/hunk-anchor
    /// tests plus nowrap renders. Wrap renders go through
    /// [`Self::viewport_placement`] instead, which speaks visual y.
    pub fn visual_viewport_top(&self, viewport_height: usize, now: Instant) -> usize {
        let target = self.viewport_top(viewport_height) as f32;
        let visual = match self.anim.as_ref() {
            Some(anim) => anim.sample(target, now).0,
            None => target,
        };
        self.visual_top.set(visual);
        visual.round().max(0.0) as usize
    }

    /// Compute the viewport's top position for the current render,
    /// returning `(top_row, skip_visual)` where `top_row` is the first
    /// logical layout row to draw and `skip_visual` is the number of
    /// visual lines of `top_row` the renderer should discard off the
    /// top so that the cursor lands at its desired placement target.
    ///
    /// In **nowrap** mode every logical row is one visual row tall,
    /// so the result is always `(visual_viewport_top(h), 0)` — the
    /// legacy scroll model is preserved byte-for-byte. In **wrap**
    /// mode `viewport_placement` converts the hunk-anchored placement
    /// logic from logical-row space into visual-row space via
    /// [`VisualIndex`]; the cursor's first visual row always lands at
    /// the centre-of-viewport (or viewport ceiling under `Top`
    /// placement), regardless of how much the preceding diff content
    /// wraps. Animation is preserved across the transition: the tween
    /// runs in visual y, which in nowrap collapses to logical rows
    /// and matches the pre-rework behaviour numerically.
    pub fn viewport_placement(
        &self,
        viewport_height: usize,
        body_width: Option<usize>,
        now: Instant,
    ) -> (usize, usize) {
        let Some(_width) = body_width else {
            // Nowrap fast path — identical to the old visual_viewport_top.
            return (self.visual_viewport_top(viewport_height, now), 0);
        };
        let vi = VisualIndex::build(&self.layout, &self.files, body_width);
        let target_y = self.placement_target_visual_y(viewport_height, &vi);
        let sampled_y = match self.anim.as_ref() {
            Some(anim) => anim.sample(target_y as f32, now).0,
            None => target_y as f32,
        };
        self.visual_top.set(sampled_y);
        let y = sampled_y.round().max(0.0) as usize;
        vi.logical_at(y)
    }

    /// Visual-y coordinate of the viewport's top edge under wrap mode,
    /// chosen so that the cursor (or its enclosing hunk) lands at the
    /// current [`CursorPlacement`]'s preferred target. Mirrors the
    /// nowrap [`Self::viewport_top`] hunk-anchoring logic, but in
    /// visual-row units.
    fn placement_target_visual_y(&self, viewport_height: usize, vi: &VisualIndex) -> usize {
        let total_visual = vi.total_visual();
        if total_visual <= viewport_height {
            return 0;
        }
        let max_top_y = total_visual - viewport_height;

        // Hunk-fits-in-viewport case: anchor the entire hunk at the
        // placement target so the user always sees the full selected
        // change as a single block, matching nowrap behaviour.
        if let Some((hunk_top, hunk_end)) = self.current_hunk_range() {
            let hunk_visual = vi.visual_y(hunk_end).saturating_sub(vi.visual_y(hunk_top));
            if hunk_visual <= viewport_height {
                let hunk_top_y = vi.visual_y(hunk_top);
                let desired = match self.cursor_placement {
                    CursorPlacement::Centered => {
                        let pad = (viewport_height - hunk_visual) / 2;
                        hunk_top_y.saturating_sub(pad)
                    }
                    CursorPlacement::Top => hunk_top_y,
                };
                return desired.min(max_top_y);
            }
        }

        // Long-hunk / non-hunk fallback: place the cursor at the
        // placement target, measured in visual y. ADR-0009 fix:
        // include `cursor_sub_row` so intra-row walks through a
        // wrapped diff line actually move the viewport instead of
        // parking it at the logical row's first visual line.
        let cursor_y = vi.visual_y(self.scroll) + self.cursor_sub_row;
        let desired = match self.cursor_placement {
            CursorPlacement::Centered => {
                // Keep the cursor's current visual row at mid-viewport.
                // `cursor_sub_row` is already the intra-row offset, so
                // the 1-row cursor height is the right subtraction
                // here — wrap-continuation lines below the cursor
                // are drawn by the renderer.
                cursor_y.saturating_sub(viewport_height.saturating_sub(1) / 2)
            }
            CursorPlacement::Top => cursor_y,
        };
        desired.min(max_top_y)
    }

    fn last_row_index(&self) -> usize {
        self.layout
            .rows
            .iter()
            .rposition(|row| !matches!(row, RowKind::Spacer))
            .unwrap_or(0)
    }

    pub fn next_hunk(&mut self) {
        // Only advance when there is actually a hunk after the cursor.
        // The previous fallback to `hunk_starts.last()` caused `j` to
        // jump **backward** whenever the cursor sat past the final hunk
        // header (e.g. on the last diff line of a long hunk), which is
        // the opposite of what "next" should mean.
        if let Some(&row) = self
            .layout
            .hunk_starts
            .iter()
            .find(|&&start| start > self.scroll)
        {
            self.scroll_to(row);
        }
    }

    pub fn prev_hunk(&mut self) {
        if let Some(&row) = self
            .layout
            .hunk_starts
            .iter()
            .rev()
            .find(|&&start| start < self.scroll)
        {
            self.scroll_to(row);
        } else if let Some(&row) = self.layout.hunk_starts.first() {
            self.scroll_to(row);
        }
    }

    /// `J` — adaptive forward motion that switches between scroll and
    /// hunk jump based on the current hunk's size.
    ///
    /// - **Short hunk** (fits in the viewport): instant jump to the next
    ///   hunk's first row, same as `j`. No chunk-scroll mid-hunk, so
    ///   walking dense edit clusters doesn't fragment into micro-presses.
    /// - **Long hunk** (taller than the viewport): scroll forward by
    ///   [`Self::chunk_size`] rows, clamped to the hunk's last row. Once
    ///   the cursor lands on the last row, the next `J` flows into the
    ///   next hunk's first row, so walking through a 200-line hunk ends
    ///   at the next hunk without any extra key press.
    /// - Crosses file boundaries the same way [`Self::next_hunk`] does,
    ///   so `J` keeps flowing through the whole scroll.
    pub fn next_change(&mut self) {
        let cursor = self.scroll;
        let viewport = self.last_body_height.get().max(1);
        let body_width = self.last_body_width.get();
        if let Some((hunk_top, hunk_end)) = self.current_hunk_range() {
            let last_row = hunk_end.saturating_sub(1);
            // Measure hunk size in **visual** rows so a single
            // wrapped long line doesn't falsely register as "short
            // hunk, just jump". Under nowrap the VisualIndex is the
            // identity and this collapses to the old logical-row
            // check.
            let vi = VisualIndex::build(&self.layout, &self.files, body_width);
            let hunk_visual = vi.visual_y(hunk_end).saturating_sub(vi.visual_y(hunk_top));
            // Visual position inside the hunk: logical row y plus
            // intra-row offset. The cursor can now walk through a
            // single wrapped diff line (ADR-0009 fix), so we must
            // compare against the hunk's visual *last line*, not
            // its last logical row.
            let cur_y = vi.visual_y(cursor) + self.cursor_sub_row;
            let hunk_last_y = vi
                .visual_y(hunk_end)
                .saturating_sub(1)
                .max(vi.visual_y(hunk_top));
            let at_hunk_end = cur_y >= hunk_last_y;
            if hunk_visual > viewport && !at_hunk_end {
                // Advance by a visual chunk, clamp to the hunk's
                // last visual line so the cursor never escapes the
                // current hunk on an intra-hunk step. `scroll_to_visual`
                // preserves the resolved sub-row so a walk inside a
                // long wrapped line actually moves the visible
                // cursor.
                let target_y = (cur_y + self.chunk_size()).min(hunk_last_y);
                let (target_row, target_sub) = vi.logical_at(target_y);
                let clamped_row = target_row.min(last_row);
                if body_width.is_some() {
                    self.scroll_to_visual(clamped_row, target_sub, &vi);
                } else {
                    self.scroll_to(clamped_row);
                }
                return;
            }
        }
        self.next_hunk();
    }

    /// `K` — adaptive backward motion. Mirror of [`Self::next_change`].
    pub fn prev_change(&mut self) {
        let cursor = self.scroll;
        let viewport = self.last_body_height.get().max(1);
        let body_width = self.last_body_width.get();
        if let Some((hunk_top, hunk_end)) = self.current_hunk_range() {
            let vi = VisualIndex::build(&self.layout, &self.files, body_width);
            let hunk_visual = vi.visual_y(hunk_end).saturating_sub(vi.visual_y(hunk_top));
            let cur_y = vi.visual_y(cursor) + self.cursor_sub_row;
            let hunk_top_y = vi.visual_y(hunk_top);
            let at_hunk_top = cur_y <= hunk_top_y;
            if hunk_visual > viewport && !at_hunk_top {
                let target_y = cur_y.saturating_sub(self.chunk_size()).max(hunk_top_y);
                let (target_row, target_sub) = vi.logical_at(target_y);
                let clamped_row = target_row.max(hunk_top);
                if body_width.is_some() {
                    self.scroll_to_visual(clamped_row, target_sub, &vi);
                } else {
                    self.scroll_to(clamped_row);
                }
                return;
            }
        }
        self.prev_hunk();
    }

    pub fn jump_to_file_first_hunk(&mut self, file_idx: usize) {
        if let Some(Some(row)) = self.layout.file_first_hunk.get(file_idx).copied() {
            self.scroll_to(row);
        }
    }

    pub fn follow_restore(&mut self) {
        self.follow_mode = true;
        if let Some(idx) = self.follow_target_row() {
            self.scroll_to(idx);
        }
    }

    /// Row that "follow mode" parks the scroll cursor on: the **last
    /// visible content row** of the newest file (files are sorted
    /// mtime-ascending, so the last file is the most recently touched
    /// one). Walks `layout.rows` from the end and returns the first
    /// non-Spacer row whose `file_of_row` matches the newest file.
    /// This lands on the actual last diff line of the last hunk, the
    /// place a `tail -f`-style monitor expects to see.
    ///
    /// ADR-0009 fix: the previous implementation returned the newest
    /// hunk's **header** row (`layout.hunk_starts.last()`), which for
    /// any hunk taller than the viewport pinned follow mode to the
    /// top of the hunk and hid the newest added / deleted lines. That
    /// broke the core monitoring contract exactly when large edits
    /// were landing.
    fn follow_target_row(&self) -> Option<usize> {
        if self.files.is_empty() {
            return None;
        }
        let newest = self.files.len() - 1;
        // Walk from the end of the layout to find the last content
        // row belonging to the newest file. `file_of_row[i]` carries
        // the owning file for every row type; Spacer rows are
        // excluded because they are cosmetic inter-file padding and
        // do not belong to any file's change set.
        for (i, &file_idx) in self.layout.file_of_row.iter().enumerate().rev() {
            if file_idx == newest && !matches!(self.layout.rows.get(i), Some(RowKind::Spacer)) {
                return Some(i);
            }
        }
        // Fallbacks mirror the legacy behaviour: if the walk above
        // turns up nothing (file has no diffable content — binary,
        // empty, …), try the file's first-hunk entry, then the
        // absolute last row. Either is preferable to returning None.
        self.layout
            .file_first_hunk
            .last()
            .copied()
            .flatten()
            .or_else(|| self.layout.rows.len().checked_sub(1))
    }

    /// File index that the row at `self.scroll` belongs to.
    pub fn current_file_idx(&self) -> Option<usize> {
        self.layout.file_of_row.get(self.scroll).copied()
    }

    pub fn current_file_path(&self) -> Option<&Path> {
        self.current_file_idx()
            .and_then(|i| self.files.get(i))
            .map(|f| f.path.as_path())
    }

    /// `(file_idx, hunk_idx)` of the hunk the cursor is currently inside,
    /// or `None` when scroll is parked on a non-hunk row (file header,
    /// binary notice, spacer). The renderer uses this to pick the bright
    /// style for selected hunk rows and DIM for everyone else.
    pub fn current_hunk(&self) -> Option<(usize, usize)> {
        match self.layout.rows.get(self.scroll)? {
            RowKind::HunkHeader { file_idx, hunk_idx } => Some((*file_idx, *hunk_idx)),
            RowKind::DiffLine {
                file_idx, hunk_idx, ..
            } => Some((*file_idx, *hunk_idx)),
            _ => None,
        }
    }

    /// Scroll to the first row of the file matching `path`. No-op if
    /// the file is not in the current layout.
    /// Extract the content of the diff line at the current scroll
    /// position. Returns the raw `DiffLine.content` (without `+`/`-`
    /// prefix) for content-based matching when switching view modes.
    pub fn current_diff_line_content(&self) -> Option<String> {
        let row = self.layout.rows.get(self.scroll)?;
        if let RowKind::DiffLine {
            file_idx,
            hunk_idx,
            line_idx,
        } = *row
        {
            let file = self.files.get(file_idx)?;
            let DiffContent::Text(hunks) = &file.content else {
                return None;
            };
            let line = hunks.get(hunk_idx)?.lines.get(line_idx)?;
            if !matches!(line.kind, LineKind::Context) {
                return Some(line.content.clone());
            }
        }
        // If on a hunk header, grab the first changed line in the hunk.
        if let RowKind::HunkHeader {
            file_idx, hunk_idx, ..
        } = *row
        {
            let file = self.files.get(file_idx)?;
            let DiffContent::Text(hunks) = &file.content else {
                return None;
            };
            for dl in &hunks.get(hunk_idx)?.lines {
                if !matches!(dl.kind, LineKind::Context) {
                    return Some(dl.content.clone());
                }
            }
        }
        None
    }

    /// Scroll to a diff line in `path` whose content matches `needle`.
    /// Returns `true` if a match was found and scrolled to.
    pub fn scroll_to_diff_content(&mut self, path: &Path, needle: &str) -> bool {
        let needle = needle.trim();
        if needle.is_empty() {
            return false;
        }
        for (i, row) in self.layout.rows.iter().enumerate() {
            if let RowKind::DiffLine {
                file_idx,
                hunk_idx,
                line_idx,
            } = *row
            {
                let Some(file) = self.files.get(file_idx) else {
                    continue;
                };
                if file.path != path {
                    continue;
                }
                let DiffContent::Text(hunks) = &file.content else {
                    continue;
                };
                let Some(line) = hunks.get(hunk_idx).and_then(|h| h.lines.get(line_idx)) else {
                    continue;
                };
                if line.content.trim() == needle {
                    self.scroll_to(i);
                    return true;
                }
            }
        }
        false
    }

    pub fn scroll_to_file(&mut self, path: &Path) {
        for (i, row) in self.layout.rows.iter().enumerate() {
            if let RowKind::FileHeader { file_idx } = row
                && self.files.get(*file_idx).is_some_and(|f| f.path == path)
            {
                self.scroll_to(i);
                return;
            }
        }
    }

    /// Insert a scar of the given `kind` with `body` as the human
    /// text, at the cursor's current position. No-op when the
    /// cursor is not on a diff row (file header, hunk header,
    /// spacer, binary notice). Write failures from
    /// [`crate::scar::insert_scar`] are captured in `last_error` so
    /// the footer surfaces them instead of panicking. The watcher
    /// picks up the resulting write on its next tick and re-runs
    /// `compute_diff`, which shows the new scar line in place.
    pub fn insert_canned_scar(&mut self, kind: ScarKind, body: &str) {
        // Stream mode shows historical diffs with synthetic line numbers;
        // scar insertion would target nonsensical positions. Switch to
        // diff mode to scar the current file state.
        if self.view_mode == ViewMode::Stream {
            return;
        }
        let Some((path, line)) = self.scar_target_line() else {
            return;
        };
        if let Err(err) = crate::scar::insert_scar(&path, line, kind, body) {
            self.last_error = Some(format!("scar: {err:#}"));
        }
    }

    /// Enter free-text scar input mode. Captures the current
    /// cursor's target `(path, line)` so a watcher-driven recompute
    /// while the user is typing cannot retarget the write. No-op
    /// when the cursor is not on a scar-able row.
    pub fn open_scar_comment(&mut self) {
        if self.view_mode == ViewMode::Stream {
            return;
        }
        let Some((target_path, target_line)) = self.scar_target_line() else {
            return;
        };
        self.scar_comment = Some(ScarCommentState {
            target_path,
            target_line,
            body: String::new(),
            cursor_pos: 0,
        });
    }

    /// Abort free-text scar input without writing anything.
    pub fn close_scar_comment(&mut self) {
        self.scar_comment = None;
    }

    /// Commit the currently-composed free-text scar, if any. Empty
    /// body is treated as a cancel (so double-`Enter` on an empty
    /// input does not write a blank scar). Write failures land on
    /// `last_error` with the same `scar:` prefix used by the canned
    /// `a` / `r` dispatch.
    pub fn commit_scar_comment(&mut self) {
        let Some(state) = self.scar_comment.take() else {
            return;
        };
        let body = state.body.trim();
        if body.is_empty() {
            return;
        }
        if let Err(err) =
            crate::scar::insert_scar(&state.target_path, state.target_line, ScarKind::Free, body)
        {
            self.last_error = Some(format!("scar: {err:#}"));
        }
    }

    /// Toggle the "seen" mark on the hunk the cursor is currently
    /// inside. No-op when the cursor is on a file header, spacer,
    /// binary notice, or any other row with no enclosing hunk.
    /// Pure state change — nothing is written to disk (the mark
    /// lives only for the session, see plans/v0.2.md M4).
    pub fn toggle_seen_current_hunk(&mut self) {
        let Some((file_idx, hunk_idx)) = self.current_hunk() else {
            return;
        };
        let Some(file) = self.files.get(file_idx) else {
            return;
        };
        let DiffContent::Text(hunks) = &file.content else {
            return;
        };
        let Some(hunk) = hunks.get(hunk_idx) else {
            return;
        };
        let key = (file.path.clone(), hunk.old_start);
        if !self.seen_hunks.remove(&key) {
            self.seen_hunks.insert(key);
        }
    }

    /// Open the full-file zoom view for the cursor's current hunk.
    /// Reads the worktree file, builds a line_bg map from diff
    /// hunks, and parks the viewport so the hunk's first line is
    /// visible. No-op when the cursor is not on a text hunk, or
    /// the file cannot be read.
    pub fn open_file_view(&mut self) {
        use ratatui::style::Color;
        use std::collections::HashMap;

        let Some((file_idx, _hunk_idx)) = self.current_hunk() else {
            return;
        };
        let Some(file) = self.files.get(file_idx) else {
            return;
        };
        let DiffContent::Text(hunks) = &file.content else {
            return;
        };

        let abs = self.root.join(&file.path);
        let content = match std::fs::read_to_string(&abs) {
            Ok(c) => c,
            Err(e) => {
                self.last_error = Some(format!("file view: {e}"));
                return;
            }
        };
        let lines: Vec<String> = content.lines().map(String::from).collect();

        let mut line_bg: HashMap<usize, Color> = HashMap::new();
        for hunk in hunks {
            let mut new_line = hunk.new_start; // 1-indexed
            for dl in &hunk.lines {
                match dl.kind {
                    LineKind::Added => {
                        if new_line >= 1 && (new_line - 1) < lines.len() {
                            line_bg.insert(new_line - 1, self.config.colors.bg_added_color());
                        }
                        new_line += 1;
                    }
                    LineKind::Context => {
                        new_line += 1;
                    }
                    LineKind::Deleted => {
                        // Deleted lines don't exist in the worktree;
                        // they're not rendered in file view.
                    }
                }
            }
        }

        // Find cursor's hunk new_start to center the initial viewport.
        let initial_cursor = self
            .current_hunk()
            .and_then(|(_, hi)| hunks.get(hi))
            .map(|h| h.new_start.saturating_sub(1))
            .unwrap_or(0)
            .min(lines.len().saturating_sub(1));

        self.file_view = Some(FileViewState {
            file_idx,
            path: file.path.clone(),
            return_scroll: self.scroll,
            lines,
            line_bg,
            cursor: initial_cursor,
            scroll_top: initial_cursor.saturating_sub(self.last_body_height.get() / 2),
        });
    }

    /// Close the file view and restore the normal-mode cursor to
    /// the position it was at when the user entered.
    pub fn close_file_view(&mut self) {
        if let Some(state) = self.file_view.take() {
            self.scroll_to(state.return_scroll);
        }
    }

    /// Keystroke handler for the file-view zoom mode. Supports
    /// `Enter`/`Esc` to exit, `j`/`k`/`J`/`K` for cursor
    /// movement, `g`/`G` for top/bottom, and `q` to quit.
    fn handle_file_view_key(&mut self, key: KeyEvent) -> KeyEffect {
        if matches!(key.code, KeyCode::Char('q'))
            || (matches!(key.code, KeyCode::Char('c'))
                && key.modifiers.contains(KeyModifiers::CONTROL))
        {
            self.should_quit = true;
            return KeyEffect::None;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('d') => {
                    self.file_view_scroll_by(HALF_PAGE as isize);
                    return KeyEffect::None;
                }
                KeyCode::Char('u') => {
                    self.file_view_scroll_by(-(HALF_PAGE as isize));
                    return KeyEffect::None;
                }
                _ => {}
            }
        }
        match key.code {
            KeyCode::Enter | KeyCode::Esc => self.close_file_view(),
            // j/k: chunk scroll (viewport/3), matching normal-mode
            // adaptive-motion feel. J/K: exact 1-row move.
            KeyCode::Char('j') | KeyCode::Down => {
                let chunk = self.chunk_size() as isize;
                self.file_view_scroll_by(chunk);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                let chunk = self.chunk_size() as isize;
                self.file_view_scroll_by(-chunk);
            }
            KeyCode::Char('J') => {
                self.file_view_scroll_by(1);
            }
            KeyCode::Char('K') => {
                self.file_view_scroll_by(-1);
            }
            KeyCode::Char('g') => {
                self.file_view_goto(0);
            }
            KeyCode::Char('G') => {
                let last = self
                    .file_view
                    .as_ref()
                    .map(|fv| fv.lines.len().saturating_sub(1))
                    .unwrap_or(0);
                self.file_view_goto(last);
            }
            KeyCode::Char('e') => {
                // Open external editor at the file-view cursor's
                // 1-indexed line. Uses the same path stored in
                // FileViewState so the editor opens the exact file.
                let env = std::env::var("EDITOR").ok();
                if let Some(fv) = self.file_view.as_ref() {
                    let line_1indexed = fv.cursor + 1;
                    let abs = self.root.join(&fv.path);
                    if let Some(inv) = build_editor_invocation(env.as_deref(), line_1indexed, &abs)
                    {
                        return KeyEffect::OpenEditor(inv);
                    }
                }
            }
            _ => {}
        }
        KeyEffect::None
    }

    fn file_view_scroll_by(&mut self, delta: isize) {
        let Some(fv) = self.file_view.as_mut() else {
            return;
        };
        let max = fv.lines.len().saturating_sub(1);
        let new = (fv.cursor as isize + delta).clamp(0, max as isize) as usize;
        fv.cursor = new;
        let vh = self.last_body_height.get();
        if fv.cursor < fv.scroll_top {
            fv.scroll_top = fv.cursor;
        } else if fv.cursor >= fv.scroll_top + vh {
            fv.scroll_top = fv.cursor.saturating_sub(vh - 1);
        }
    }

    fn file_view_goto(&mut self, line: usize) {
        let Some(fv) = self.file_view.as_mut() else {
            return;
        };
        let max = fv.lines.len().saturating_sub(1);
        fv.cursor = line.min(max);
        let vh = self.last_body_height.get();
        if fv.cursor < fv.scroll_top {
            fv.scroll_top = fv.cursor;
        } else if fv.cursor >= fv.scroll_top + vh {
            fv.scroll_top = fv.cursor.saturating_sub(vh - 1);
        }
    }

    /// Enter the `/` search-query composer. Any previously
    /// confirmed [`SearchState`] is left untouched until the user
    /// actually commits the new query with Enter — Esc restores
    /// everything, vim-style.
    pub fn open_search_input(&mut self) {
        self.search_input = Some(SearchInputState::default());
    }

    /// Abort the query composer without touching confirmed state.
    pub fn close_search_input(&mut self) {
        self.search_input = None;
    }

    /// Commit the composed query: run [`find_matches`] against the
    /// current layout, install the resulting `SearchState`, and
    /// jump the cursor to the first match (if any). Empty queries
    /// close the composer without touching confirmed state so a
    /// stray `/` + `Enter` does not wipe an existing search.
    pub fn commit_search_input(&mut self) {
        let Some(input) = self.search_input.take() else {
            return;
        };
        let query = input.query;
        if query.is_empty() {
            return;
        }
        let matches = find_matches(&self.layout, &self.files, &query);
        let first_row = matches.first().map(|m| m.row);
        self.search = Some(SearchState {
            query,
            matches,
            current: 0,
        });
        if let Some(row) = first_row {
            self.follow_mode = false;
            self.scroll_to(row);
        }
    }

    /// `/`-composer keystroke handler. Typing appends, Backspace
    /// deletes, Enter commits, Esc cancels. Ctrl-C also cancels
    /// (matches the other modal overlays).
    fn handle_search_input_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('c') => self.close_search_input(),
                KeyCode::Char('a') => {
                    if let Some(s) = self.search_input.as_mut() {
                        s.cursor_pos = 0;
                    }
                }
                KeyCode::Char('e') => {
                    if let Some(s) = self.search_input.as_mut() {
                        s.cursor_pos = s.query.chars().count();
                    }
                }
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Esc => self.close_search_input(),
            KeyCode::Enter => self.commit_search_input(),
            KeyCode::Backspace => {
                if let Some(s) = self.search_input.as_mut() {
                    edit_backspace(&mut s.query, &mut s.cursor_pos);
                }
            }
            KeyCode::Left => {
                if let Some(s) = self.search_input.as_mut() {
                    s.cursor_pos = s.cursor_pos.saturating_sub(1);
                }
            }
            KeyCode::Right => {
                if let Some(s) = self.search_input.as_mut() {
                    s.cursor_pos = (s.cursor_pos + 1).min(s.query.chars().count());
                }
            }
            KeyCode::Home => {
                if let Some(s) = self.search_input.as_mut() {
                    s.cursor_pos = 0;
                }
            }
            KeyCode::End => {
                if let Some(s) = self.search_input.as_mut() {
                    s.cursor_pos = s.query.chars().count();
                }
            }
            KeyCode::Char(c) => {
                if let Some(s) = self.search_input.as_mut() {
                    edit_insert_char(&mut s.query, &mut s.cursor_pos, c);
                }
            }
            _ => {}
        }
    }

    /// `n` — advance to the next confirmed search hit and jump the
    /// cursor to its row. Wraps around at the end. No-op when
    /// there is no confirmed search or it has zero matches.
    pub fn search_jump_next(&mut self) {
        let Some(state) = self.search.as_mut() else {
            return;
        };
        if state.matches.is_empty() {
            return;
        }
        state.current = (state.current + 1) % state.matches.len();
        let row = state.matches[state.current].row;
        self.follow_mode = false;
        self.scroll_to(row);
    }

    /// `N` — step back to the previous confirmed search hit,
    /// wrapping to the tail at the start.
    pub fn search_jump_prev(&mut self) {
        let Some(state) = self.search.as_mut() else {
            return;
        };
        if state.matches.is_empty() {
            return;
        }
        state.current = if state.current == 0 {
            state.matches.len() - 1
        } else {
            state.current - 1
        };
        let row = state.matches[state.current].row;
        self.follow_mode = false;
        self.scroll_to(row);
    }

    /// Read helper: does `(file_idx, hunk_idx)` currently carry a
    /// "seen" mark? Used by the renderer to decorate the hunk
    /// header without learning the fingerprint scheme.
    pub fn hunk_is_seen(&self, file_idx: usize, hunk_idx: usize) -> bool {
        let Some(file) = self.files.get(file_idx) else {
            return false;
        };
        let DiffContent::Text(hunks) = &file.content else {
            return false;
        };
        let Some(hunk) = hunks.get(hunk_idx) else {
            return false;
        };
        self.seen_hunks
            .contains(&(file.path.clone(), hunk.old_start))
    }

    /// Resolve the cursor's current target (path + 1-indexed line)
    /// into an [`EditorInvocation`], reading the user's `$EDITOR`
    /// preference from `editor_env`. Returns `None` when the cursor
    /// is not on a scar-able row **or** the environment has no
    /// editor configured — in either case the `e` key should be a
    /// silent no-op.
    pub fn open_in_editor(&self, editor_env: Option<&str>) -> Option<EditorInvocation> {
        if self.view_mode == ViewMode::Stream {
            return None;
        }
        let (path, line) = self.scar_target_line()?;
        build_editor_invocation(editor_env, line, &path)
    }

    /// Enter the hunk-revert confirmation overlay. No-op when the
    /// cursor is not inside a diff hunk (file headers, spacers,
    /// binary notices) or when the enclosing file is not text.
    pub fn open_revert_confirm(&mut self) {
        if self.view_mode == ViewMode::Stream {
            return;
        }
        let Some((file_idx, hunk_idx)) = self.current_hunk() else {
            return;
        };
        let Some(file) = self.files.get(file_idx) else {
            return;
        };
        let DiffContent::Text(hunks) = &file.content else {
            return;
        };
        let Some(hunk) = hunks.get(hunk_idx) else {
            return;
        };
        self.revert_confirm = Some(RevertConfirmState {
            file_idx,
            hunk_idx,
            file_path: file.path.clone(),
            hunk_old_start: hunk.old_start,
        });
    }

    /// Abort the revert overlay without touching the worktree.
    pub fn close_revert_confirm(&mut self) {
        self.revert_confirm = None;
    }

    /// Commit the revert: build a single-hunk patch from the
    /// captured `(file_idx, hunk_idx)` and run
    /// `git apply --reverse`. Closes the overlay unconditionally;
    /// write failures surface on `last_error` with the `revert:`
    /// prefix so the footer flags them.
    pub fn confirm_revert(&mut self) {
        let Some(state) = self.revert_confirm.take() else {
            return;
        };
        // Re-resolve the hunk by stable identity (path + old_start)
        // instead of trusting the saved indices, which may have
        // drifted after a watcher-driven refresh.
        let hunk = self
            .files
            .iter()
            .find(|f| f.path == state.file_path)
            .and_then(|f| match &f.content {
                DiffContent::Text(hunks) => {
                    hunks.iter().find(|h| h.old_start == state.hunk_old_start)
                }
                _ => None,
            });
        let Some(hunk) = hunk else {
            self.last_error = Some("revert: hunk no longer present".into());
            return;
        };
        let patch = git::build_hunk_patch(&state.file_path, hunk);
        if let Err(err) = git::revert_hunk(&self.root, &patch) {
            self.last_error = Some(format!("revert: {err:#}"));
        }
    }

    /// Keystroke handler for the revert confirmation overlay.
    /// `y` / `Y` / `Enter` confirms; every other key (including
    /// `n` / `N` / `Esc` and stray navigation keys) cancels. This
    /// is intentional — the overlay is a hard "did you mean to do
    /// this?" gate, and any ambiguous input should bias toward
    /// "no" so the user's work is preserved.
    fn handle_revert_confirm_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            self.close_revert_confirm();
            return;
        }
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => self.confirm_revert(),
            _ => self.close_revert_confirm(),
        }
    }

    /// Keystroke handler used while the scar-comment overlay is
    /// active. Typing characters appends to the body, Backspace
    /// deletes one char, Enter commits, Esc cancels.
    /// Handle pasted text (from bracketed paste or IME commit).
    /// Routes to whichever text-input overlay is currently active.
    /// No-op when no text input is open — stray pastes in normal
    /// mode are silently ignored.
    pub fn handle_paste(&mut self, text: &str) {
        if let Some(state) = self.scar_comment.as_mut() {
            edit_insert_str(&mut state.body, &mut state.cursor_pos, text);
        } else if let Some(state) = self.search_input.as_mut() {
            edit_insert_str(&mut state.query, &mut state.cursor_pos, text);
        }
    }

    fn handle_scar_comment_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('c') => self.close_scar_comment(),
                KeyCode::Char('a') => {
                    if let Some(s) = self.scar_comment.as_mut() {
                        s.cursor_pos = 0;
                    }
                }
                KeyCode::Char('e') => {
                    if let Some(s) = self.scar_comment.as_mut() {
                        s.cursor_pos = s.body.chars().count();
                    }
                }
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Esc => self.close_scar_comment(),
            KeyCode::Enter => self.commit_scar_comment(),
            KeyCode::Backspace => {
                if let Some(s) = self.scar_comment.as_mut() {
                    edit_backspace(&mut s.body, &mut s.cursor_pos);
                }
            }
            KeyCode::Left => {
                if let Some(s) = self.scar_comment.as_mut() {
                    s.cursor_pos = s.cursor_pos.saturating_sub(1);
                }
            }
            KeyCode::Right => {
                if let Some(s) = self.scar_comment.as_mut() {
                    s.cursor_pos = (s.cursor_pos + 1).min(s.body.chars().count());
                }
            }
            KeyCode::Home => {
                if let Some(s) = self.scar_comment.as_mut() {
                    s.cursor_pos = 0;
                }
            }
            KeyCode::End => {
                if let Some(s) = self.scar_comment.as_mut() {
                    s.cursor_pos = s.body.chars().count();
                }
            }
            KeyCode::Char(c) => {
                if let Some(s) = self.scar_comment.as_mut() {
                    edit_insert_char(&mut s.body, &mut s.cursor_pos, c);
                }
            }
            _ => {}
        }
    }

    /// Resolve the cursor's current row to an absolute file path and a
    /// 1-indexed **new-file** line number, suitable for
    /// [`crate::scar::insert_scar`].
    ///
    /// - Returns `None` when the cursor is parked on a row that has no
    ///   scar-able location: file headers (they point at a whole file,
    ///   not a specific line), spacers, binary notices, or files whose
    ///   content is binary / truncated.
    /// - For a cursor on a **hunk header** row, returns the hunk's
    ///   `new_start` so `insert_scar` drops the scar immediately above
    ///   the first line of the hunk body. This is what the user wants
    ///   when they hit `a` / `r` with the cursor on the `@@` header
    ///   of a multi-line hunk: one scar that annotates the whole block.
    /// - For a cursor on an **Added or Context line**, returns the
    ///   new-file line number of that line itself. The scar lands
    ///   directly above it, matching the "comment above the code"
    ///   convention.
    /// - For a cursor on a **Deleted line**, the line has no new-file
    ///   position; we return the new-file line number of the next
    ///   non-deleted line in the same hunk (i.e. the line that
    ///   effectively "replaces" it). If the deleted run reaches the
    ///   hunk tail we fall through to the same offset, which matches
    ///   "insert the scar right after the deletion block".
    pub fn scar_target_line(&self) -> Option<(PathBuf, usize)> {
        let row = self.layout.rows.get(self.scroll)?;
        let (file_idx, hunk_idx, diff_line_idx) = match *row {
            RowKind::DiffLine {
                file_idx,
                hunk_idx,
                line_idx,
            } => (file_idx, hunk_idx, Some(line_idx)),
            RowKind::HunkHeader { file_idx, hunk_idx } => (file_idx, hunk_idx, None),
            _ => return None,
        };
        let file = self.files.get(file_idx)?;
        let DiffContent::Text(hunks) = &file.content else {
            return None;
        };
        let hunk = hunks.get(hunk_idx)?;

        // Hunk header cursor: target the first *changed* line (Added
        // or Deleted) in the hunk, skipping leading Context lines. If
        // the hunk is all-context (unlikely but possible in corner
        // cases), fall back to `new_start`. This places the scar
        // directly above the change rather than above distant context.
        let Some(line_idx) = diff_line_idx else {
            for (offset, dl) in hunk.lines.iter().enumerate() {
                if !matches!(dl.kind, LineKind::Context) {
                    return Some((self.root.join(&file.path), hunk.new_start + offset));
                }
            }
            return Some((self.root.join(&file.path), hunk.new_start));
        };

        // Diff line cursor: walk to compute the new-file line number.
        let mut offset: usize = 0;
        for (i, line) in hunk.lines.iter().enumerate() {
            if i > line_idx {
                break;
            }
            let is_deleted = matches!(line.kind, LineKind::Deleted);
            if i == line_idx {
                return Some((self.root.join(&file.path), hunk.new_start + offset));
            }
            if !is_deleted {
                offset += 1;
            }
        }
        None
    }

    /// `(start, end_exclusive)` row range of the cursor's current hunk.
    /// Walks `layout.rows` from the start of the hunk header through
    /// every consecutive `DiffLine` belonging to the same hunk. Returns
    /// `None` when the cursor is not inside a hunk.
    pub fn current_hunk_range(&self) -> Option<(usize, usize)> {
        let (file_idx, hunk_idx) = self.current_hunk()?;
        let mut start = None;
        let mut end = None;
        for (i, row) in self.layout.rows.iter().enumerate() {
            let belongs = match row {
                RowKind::HunkHeader {
                    file_idx: f,
                    hunk_idx: h,
                } => *f == file_idx && *h == hunk_idx,
                RowKind::DiffLine {
                    file_idx: f,
                    hunk_idx: h,
                    ..
                } => *f == file_idx && *h == hunk_idx,
                _ => false,
            };
            if belongs {
                if start.is_none() {
                    start = Some(i);
                }
                end = Some(i + 1);
            } else if start.is_some() {
                // Already walked past the hunk's last row.
                break;
            }
        }
        Some((start?, end?))
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
        let mut layout = ScrollLayout {
            file_first_hunk: vec![None; self.files.len()],
            ..ScrollLayout::default()
        };

        for (file_idx, file) in self.files.iter().enumerate() {
            let header_row = layout.rows.len();
            layout.rows.push(RowKind::FileHeader { file_idx });

            match &file.content {
                DiffContent::Binary => {
                    let notice_row = layout.rows.len();
                    layout.rows.push(RowKind::BinaryNotice { file_idx });
                    layout.file_first_hunk[file_idx] = Some(notice_row);
                }
                DiffContent::Text(hunks) => {
                    if hunks.is_empty() {
                        // Treat the file header itself as the anchor row when
                        // there are no hunks at all (extremely rare in
                        // practice; happens for empty `git diff` payloads).
                        layout.file_first_hunk[file_idx] = Some(header_row);
                    } else {
                        let first_hunk_row = layout.rows.len();
                        layout.file_first_hunk[file_idx] = Some(first_hunk_row);
                        for (hunk_idx, hunk) in hunks.iter().enumerate() {
                            let row = layout.rows.len();
                            layout.rows.push(RowKind::HunkHeader { file_idx, hunk_idx });
                            layout.hunk_starts.push(row);
                            for line_idx in 0..hunk.lines.len() {
                                layout.rows.push(RowKind::DiffLine {
                                    file_idx,
                                    hunk_idx,
                                    line_idx,
                                });
                            }
                        }
                    }
                }
            }

            layout.rows.push(RowKind::Spacer);
        }

        // Fill the file_of_row map by walking rows once.
        layout.file_of_row = layout
            .rows
            .iter()
            .scan(0usize, |last_file, row| {
                let f = match row {
                    RowKind::FileHeader { file_idx } => *file_idx,
                    RowKind::HunkHeader { file_idx, .. } => *file_idx,
                    RowKind::DiffLine { file_idx, .. } => *file_idx,
                    RowKind::BinaryNotice { file_idx } => *file_idx,
                    RowKind::Spacer => *last_file,
                };
                *last_file = f;
                Some(f)
            })
            .collect();

        // Detect change-run spans: a "change run" is a maximal contiguous
        // range of `+`/`-` DiffLine rows. We record `(start, end_exclusive)`
        // pairs; `J`/`K` use these to know when they are *inside* a run
        // (and should scroll within it) versus *between* runs (and should
        // jump to the next/prev run).
        let mut current_run_start: Option<usize> = None;
        for (row_idx, row) in layout.rows.iter().enumerate() {
            let is_change = match row {
                RowKind::DiffLine {
                    file_idx,
                    hunk_idx,
                    line_idx,
                } => match &self.files[*file_idx].content {
                    DiffContent::Text(hunks) => {
                        hunks[*hunk_idx].lines[*line_idx].kind != LineKind::Context
                    }
                    DiffContent::Binary => false,
                },
                _ => false,
            };
            match (is_change, current_run_start) {
                (true, None) => {
                    current_run_start = Some(row_idx);
                }
                (false, Some(start)) => {
                    layout.change_runs.push((start, row_idx));
                    current_run_start = None;
                }
                _ => {}
            }
        }
        if let Some(start) = current_run_start {
            layout.change_runs.push((start, layout.rows.len()));
        }

        self.layout = layout;
    }

    /// Slide `scroll` to the row of `self.anchor` in the new layout.
    /// In follow mode (or when the anchor is gone) re-anchor to the
    /// follow target instead.
    pub(crate) fn refresh_anchor(&mut self) {
        if self.layout.rows.is_empty() {
            self.scroll = 0;
            self.anchor = None;
            return;
        }

        if !self.follow_mode {
            if let Some(anchor) = self.anchor.clone() {
                if let Some(row) = self.find_anchor_row(&anchor) {
                    self.scroll = row;
                    return;
                }
                if let Some(row) = self.find_anchor_file_row(&anchor) {
                    self.scroll = row;
                    self.update_anchor_from_scroll();
                    return;
                }
            }
            // Manual mode should preserve the user's approximate viewport
            // position even when the anchored file disappeared entirely.
            // Falling back to follow mode here would silently violate the
            // "manual" contract and snap to the newest file.
            self.scroll = self.scroll.min(self.last_row_index());
            self.update_anchor_from_scroll();
            return;
        }

        // Follow-mode (or anchor missing): jump to the follow target.
        if let Some(target) = self.follow_target_row() {
            self.scroll = target;
        } else {
            self.scroll = 0;
        }
        self.update_anchor_from_scroll();
    }

    fn find_anchor_row(&self, anchor: &HunkAnchor) -> Option<usize> {
        // Find the file index for the anchor path.
        let file_idx = self.files.iter().position(|f| f.path == anchor.path)?;

        // Walk the file's hunks to find one whose old_start matches.
        match &self.files[file_idx].content {
            DiffContent::Text(hunks) => {
                let target_hunk = hunks
                    .iter()
                    .position(|h| h.old_start == anchor.hunk_old_start)?;
                // Now walk the layout to find the matching HunkHeader row.
                self.layout.rows.iter().position(|row| {
                    matches!(
                        row,
                        RowKind::HunkHeader { file_idx: f, hunk_idx } if *f == file_idx && *hunk_idx == target_hunk
                    )
                })
            }
            DiffContent::Binary => self.layout.file_first_hunk.get(file_idx).copied().flatten(),
        }
    }

    fn find_anchor_file_row(&self, anchor: &HunkAnchor) -> Option<usize> {
        let file_idx = self.files.iter().position(|f| f.path == anchor.path)?;
        match &self.files[file_idx].content {
            DiffContent::Text(hunks) if !hunks.is_empty() => {
                let nearest_hunk = hunks
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, hunk)| hunk.old_start.abs_diff(anchor.hunk_old_start))
                    .map(|(idx, _)| idx)?;
                self.layout.rows.iter().position(|row| {
                    matches!(
                        row,
                        RowKind::HunkHeader { file_idx: f, hunk_idx } if *f == file_idx && *hunk_idx == nearest_hunk
                    )
                })
            }
            _ => self
                .layout
                .file_first_hunk
                .get(file_idx)
                .copied()
                .flatten()
                .or_else(|| {
                    self.layout.rows.iter().position(|row| {
                        matches!(
                            row,
                            RowKind::FileHeader { file_idx: f } if *f == file_idx
                        )
                    })
                }),
        }
    }

    fn update_anchor_from_scroll(&mut self) {
        let Some(row) = self.layout.rows.get(self.scroll) else {
            self.anchor = None;
            return;
        };
        let (file_idx, hunk_idx) = match row {
            RowKind::HunkHeader { file_idx, hunk_idx } => (*file_idx, Some(*hunk_idx)),
            RowKind::DiffLine {
                file_idx, hunk_idx, ..
            } => (*file_idx, Some(*hunk_idx)),
            RowKind::BinaryNotice { file_idx } | RowKind::FileHeader { file_idx } => {
                (*file_idx, None)
            }
            RowKind::Spacer => {
                if let Some(file_idx) = self.layout.file_of_row.get(self.scroll).copied() {
                    (file_idx, None)
                } else {
                    self.anchor = None;
                    return;
                }
            }
        };

        let path = self.files.get(file_idx).map(|f| f.path.clone());
        match (path, hunk_idx) {
            (Some(path), Some(hunk_idx)) => {
                if let Some(file) = self.files.get(file_idx)
                    && let DiffContent::Text(hunks) = &file.content
                    && let Some(hunk) = hunks.get(hunk_idx)
                {
                    self.anchor = Some(HunkAnchor {
                        path,
                        hunk_old_start: hunk.old_start,
                    });
                }
            }
            (Some(path), None) => {
                self.anchor = Some(HunkAnchor {
                    path,
                    hunk_old_start: 0,
                });
            }
            (None, _) => self.anchor = None,
        }
    }
}

/// Async event loop. See ADR-0003 / ADR-0005.
/// Convert stream events into virtual [`FileDiff`] entries so the
/// existing scroll infrastructure can render them identically to
/// git diff output. Each event becomes one `FileDiff` with:
/// - `header_prefix`: "HH:MM:SS Tool" for display in the file header
/// - `path`: the edited file path
/// - `content`: parsed diff lines from the captured snapshot
pub fn build_stream_files(events: &[StreamEvent]) -> Vec<FileDiff> {
    events
        .iter()
        .enumerate()
        .map(|(i, ev)| {
            let ts = ev.metadata.timestamp_ms;
            let time_str = crate::ui::format_local_time(ts);
            let tool = ev.metadata.tool_name.as_deref().unwrap_or("?");
            let prefix = format!("{time_str} {tool}");
            let path = ev.metadata.file_paths.first().cloned().unwrap_or_default();

            // Use event index * 10000 as old_start so hunk anchors
            // (keyed on path + old_start) stay unique even when
            // multiple events edit the same file.
            let (hunks, added, deleted) = match &ev.diff_snapshot {
                Some(diff_text) if !diff_text.is_empty() => {
                    parse_stream_diff_to_hunk(diff_text, i * 10000 + 1)
                }
                _ => (vec![], 0, 0),
            };

            FileDiff {
                path,
                status: git::FileStatus::Modified,
                added,
                deleted,
                content: git::DiffContent::Text(hunks),
                mtime: SystemTime::UNIX_EPOCH + Duration::from_millis(ev.metadata.timestamp_ms),
                header_prefix: Some(prefix),
            }
        })
        .collect()
}

/// Parse raw diff text (from a stream event snapshot) into a single
/// `Hunk` with `DiffLine` entries. Hunk header lines (`@@`) are
/// skipped; `+`/`-`/` ` prefix determines `LineKind`.
fn parse_stream_diff_to_hunk(diff_text: &str, old_start: usize) -> (Vec<git::Hunk>, usize, usize) {
    let mut lines = Vec::new();
    let mut added = 0usize;
    let mut deleted = 0usize;
    for raw in diff_text.lines() {
        if raw.starts_with("@@")
            || raw.starts_with("diff ")
            || raw.starts_with("---")
            || raw.starts_with("+++")
            || raw.starts_with("index ")
        {
            continue;
        }
        let (kind, content) = if let Some(rest) = raw.strip_prefix('+') {
            added += 1;
            (LineKind::Added, rest.to_string())
        } else if let Some(rest) = raw.strip_prefix('-') {
            deleted += 1;
            (LineKind::Deleted, rest.to_string())
        } else if let Some(rest) = raw.strip_prefix(' ') {
            (LineKind::Context, rest.to_string())
        } else {
            (LineKind::Context, raw.to_string())
        };
        lines.push(git::DiffLine {
            kind,
            content,
            has_trailing_newline: true,
        });
    }
    if lines.is_empty() {
        return (vec![], 0, 0);
    }
    let hunk = git::Hunk {
        old_start,
        old_count: deleted,
        new_start: old_start,
        new_count: added,
        lines,
        context: None,
    };
    (vec![hunk], added, deleted)
}

/// Compute the "operation diff" — the lines in `current` that are
/// not in `previous`. This is a simple set-difference on diff lines,
/// not a true diff-of-diff. Good enough for showing what changed in
/// one Write/Edit operation when we have cumulative diff snapshots
/// before and after the operation.
fn compute_operation_diff(previous: &str, current: &str) -> String {
    use std::collections::HashSet;
    let prev_lines: HashSet<&str> = previous.lines().collect();
    let mut result = String::new();
    for line in current.lines() {
        if !prev_lines.contains(line) {
            result.push_str(line);
            result.push('\n');
        }
    }
    result
}

pub async fn run() -> Result<()> {
    use std::io::Write;
    let log_path = std::env::var("KIZU_STARTUP_TIMING_FILE").ok();
    let stage = |label: &str, t: Instant| {
        if let Some(path) = log_path.as_deref()
            && let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
        {
            let _ = writeln!(f, "[kizu-startup] {label:<28} +{:?}", t.elapsed());
        }
    };
    let t_total = Instant::now();
    let cwd = std::env::current_dir().context("reading current directory")?;
    stage("current_dir", t_total);
    let mut terminal = ratatui::try_init().context("initializing terminal")?;
    // Enable bracketed paste so terminals send IME-committed text
    // (e.g. Japanese kanji) as Event::Paste instead of individual
    // keystrokes. Without this, IME composition is invisible in
    // raw mode and committed text may arrive garbled.
    {
        use crossterm::ExecutableCommand;
        let _ = std::io::stdout().execute(crossterm::event::EnableBracketedPaste);
    }
    stage("ratatui::try_init", t_total);
    let result = async {
        // Show something immediately, even before the initial bootstrap
        // `git diff` completes. On large repos this avoids a black screen
        // during the synchronous bootstrap shell-outs.
        terminal
            .draw(|frame| {
                let area = frame.area();
                frame.render_widget(
                    ratatui::widgets::Paragraph::new("Loading kizu...")
                        .alignment(ratatui::layout::Alignment::Center),
                    area,
                );
            })
            .context("ratatui loading draw")?;
        stage("draw Loading...", t_total);

        let t_bootstrap = Instant::now();
        let mut app = App::bootstrap(cwd)?;
        stage("App::bootstrap", t_bootstrap);

        // Write session file so the Stop hook can scope its scan
        // to files changed since this baseline. Best-effort: a
        // failure here is not fatal to the TUI itself.
        if let Err(e) = crate::session::write_session(&app.root, &app.baseline_sha) {
            eprintln!("warning: failed to write kizu session file: {e}");
        }

        // Clean stale events from before this session so stream
        // mode starts fresh — old events can't carry diffs.
        app.clean_stale_events();
        // Seed diff snapshots so the first stream event shows only
        // the per-operation delta, not the entire cumulative diff.
        app.seed_diff_snapshots();

        // Draw one static frame before watcher startup. On macOS the
        // PollWatcher fallback may take noticeable time to arm because it
        // performs an initial scan; showing the bootstrap snapshot first
        // keeps startup feeling immediate instead of blank-screening until
        // watcher init finishes.
        terminal
            .draw(|frame| crate::ui::render(frame, &app))
            .context("ratatui initial draw")?;
        stage("draw bootstrap snapshot", t_total);

        let t_watcher = Instant::now();
        let mut watch = watcher::start(
            &app.root,
            &app.git_dir,
            &app.common_git_dir,
            app.current_branch_ref.as_deref(),
        )?;
        stage("watcher::start", t_watcher);
        stage("total before loop", t_total);
        let result = run_loop(&mut terminal, &mut app, &mut watch).await;
        crate::session::remove_session(&app.root);
        result
    }
    .await;
    {
        use crossterm::ExecutableCommand;
        let _ = std::io::stdout().execute(crossterm::event::DisableBracketedPaste);
    }
    let _ = ratatui::try_restore();
    result
}

async fn run_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    watch: &mut watcher::WatchHandle,
) -> Result<()> {
    use crossterm::event::{Event, EventStream};
    use futures_util::StreamExt;
    use tokio::time::{MissedTickBehavior, interval, sleep};

    let mut events = EventStream::new();

    // ~60 fps frame tick. Only polled inside `select!` when an animation
    // is live — idle frames never pay the cost. `Skip` means a long
    // idle gap doesn't turn into a burst of catch-up ticks once the
    // user kicks off a new animation.
    let mut frame = interval(Duration::from_millis(16));
    frame.set_missed_tick_behavior(MissedTickBehavior::Skip);

    // notify backends can have a short arm-up window right after startup.
    // Without a one-shot self-heal refresh, an edit that lands during that
    // gap can be missed forever until the *next* filesystem event. The
    // existing watcher tests used `sleep(150ms)` to paper over this; the app
    // should instead recover on its own.
    let startup_refresh = sleep(Duration::from_millis(400));
    tokio::pin!(startup_refresh);
    let mut startup_refresh_pending = true;

    while !app.should_quit {
        // Draw at the top of the loop so the bootstrap state is visible
        // before we ever block on `select!`.
        terminal
            .draw(|frame| crate::ui::render(frame, app))
            .context("ratatui draw")?;

        // Retire finished animations after the frame that showed their
        // final position — the next frame will then draw the static
        // target without another tween sample.
        app.tick_anim(Instant::now());

        tokio::select! {
            event = events.next() => {
                match event {
                    Some(Ok(Event::Key(key))) => {
                        app.input_health = None;
                        let effect = app.handle_key(key);
                        match effect {
                            KeyEffect::OpenEditor(inv) => {
                                if let Err(err) = run_external_editor(terminal, inv) {
                                    app.last_error = Some(format!("editor: {err:#}"));
                                }
                            }
                            other => apply_key_effect(other, app, watch),
                        }
                    }
                    Some(Ok(Event::Paste(text))) => {
                        app.input_health = None;
                        app.handle_paste(&text);
                    }
                    Some(Ok(_)) => {
                        app.input_health = None;
                    }
                    Some(Err(e)) => {
                        app.input_health = Some(format!("input: {e}"));
                    }
                    None => {
                        app.input_health = Some("input: event stream ended".into());
                        app.should_quit = true;
                    }
                }
            }
            watch_event = watch.events.recv() => {
                if let Some(first) = watch_event {
                    startup_refresh_pending = false;
                    // Drain any events that piled up behind `first` and
                    // hand the whole burst to `handle_watch_burst` so the
                    // coalescing + health-transition rules stay testable
                    // in one place.
                    let mut burst: Vec<WatchEvent> = vec![first];
                    while let Ok(more) = watch.events.try_recv() {
                        burst.push(more);
                    }
                    let (need_recompute, need_head_dirty) = app.handle_watch_burst(burst);
                    if need_recompute {
                        watch.refresh_worktree_watches();
                        // In stream mode, don't overwrite files/layout with
                        // git diff — the scroll view shows stream events.
                        // The diff will be refreshed when the user tabs back.
                        if app.view_mode != ViewMode::Stream {
                            app.recompute_diff();
                        }
                    }
                    if need_head_dirty {
                        app.mark_head_dirty();
                    }
                } else {
                    app.last_error = Some("watcher: event channel closed".into());
                    app.should_quit = true;
                }
            }
            _ = &mut startup_refresh, if startup_refresh_pending => {
                startup_refresh_pending = false;
                if app.view_mode != ViewMode::Stream {
                    app.recompute_diff();
                }
            }
            _ = frame.tick(), if app.anim.is_some() => {
                // The tick itself carries no payload — falling through
                // the bottom of the select! loops back to the `draw`
                // call at the top, which is the whole point.
            }
        }
    }

    Ok(())
}

/// Dispatch post-key-handler side effects back onto the watcher.
/// Factored out so `run_loop` stays focused on the event-loop
/// plumbing and tests can reason about the effect contract without
/// spinning up a real terminal.
fn apply_key_effect(effect: KeyEffect, app: &App, watch: &watcher::WatchHandle) {
    match effect {
        KeyEffect::None => {}
        KeyEffect::ReconfigureWatcher => {
            watch.update_current_branch_ref(app.current_branch_ref.as_deref());
        }
        KeyEffect::OpenEditor(_) => {
            // Handled inline inside `run_loop`: the editor
            // spawn needs mutable access to the terminal for the
            // suspend/resume dance, which this `&App` /
            // `&WatchHandle` helper cannot provide. Any
            // `OpenEditor` that reaches this arm is a caller
            // bug.
            debug_assert!(
                false,
                "OpenEditor must be handled by run_loop, not apply_key_effect"
            );
        }
    }
}

/// Suspend the ratatui terminal, run an external editor
/// synchronously, then re-enter the alternate screen and force a
/// full repaint on the next draw tick. Blocks the event loop for
/// the editor's lifetime — intentional, because the user is
/// inside the editor anyway and no diff-view update would be
/// visible under it.
fn run_external_editor(
    terminal: &mut ratatui::DefaultTerminal,
    invocation: EditorInvocation,
) -> Result<()> {
    use crossterm::{
        ExecutableCommand,
        event::{DisableBracketedPaste, EnableBracketedPaste},
        terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
    };
    use std::io::stdout;

    // Tear down the ratatui terminal state so the child editor
    // sees a plain cooked terminal. Errors here are surfaced —
    // half-suspended state is worse than not launching the editor
    // at all.
    disable_raw_mode().context("disable raw mode before editor")?;
    let mut out = stdout();
    out.execute(LeaveAlternateScreen)
        .context("leave alternate screen before editor")?;
    out.execute(DisableBracketedPaste).ok();

    let status = std::process::Command::new(&invocation.program)
        .args(&invocation.args)
        .status()
        .with_context(|| format!("spawning editor `{}`", invocation.program));

    // Always re-arm the alternate screen + raw mode even if the
    // spawn itself failed. Otherwise a mistyped `$EDITOR` would
    // leave the user stranded at a raw-mode prompt.
    enable_raw_mode().context("re-enable raw mode after editor")?;
    stdout()
        .execute(EnterAlternateScreen)
        .context("re-enter alternate screen after editor")?;
    stdout().execute(EnableBracketedPaste).ok();
    terminal.clear().ok();

    let status = status?;
    if !status.success() {
        return Err(anyhow!(
            "editor `{}` exited with status {}",
            invocation.program,
            status
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{DiffContent, DiffLine, FileStatus, Hunk, LineKind};
    use std::time::Duration;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn diff_line(kind: LineKind, content: &str) -> DiffLine {
        DiffLine {
            kind,
            content: content.to_string(),
            has_trailing_newline: true,
        }
    }

    fn hunk(old_start: usize, lines: Vec<DiffLine>) -> Hunk {
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

    fn make_file(name: &str, hunks: Vec<Hunk>, secs: u64) -> FileDiff {
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

    fn binary_file(name: &str, secs: u64) -> FileDiff {
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

    /// Build an `App` against `/tmp/fake` with no real filesystem use; the
    /// `populate_mtimes` step is bypassed by writing `mtime` directly on the
    /// fixtures. Files are sorted in ascending mtime order to match the
    /// real `recompute_diff` path.
    fn fake_app(files: Vec<FileDiff>) -> App {
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
            picker: None,
            scar_comment: None,
            revert_confirm: None,
            file_view: None,
            search_input: None,
            search: None,
            seen_hunks: BTreeSet::new(),
            follow_mode: true,
            last_error: None,
            input_health: None,
            head_dirty: false,
            should_quit: false,
            last_body_height: Cell::new(DEFAULT_BODY_HEIGHT),
            last_body_width: Cell::new(None),
            visual_top: Cell::new(0.0),
            anim: None,
            wrap_lines: false,
            watcher_health: WatcherHealth::default(),
            highlighter: std::cell::OnceCell::new(),
            config: crate::config::KizuConfig::default(),
            view_mode: ViewMode::default(),
            stream_events: Vec::new(),
            diff_snapshots: std::collections::HashMap::new(),
        };
        app.files = files;
        app.files.sort_by(|a, b| a.mtime.cmp(&b.mtime));
        app.build_layout();
        app.refresh_anchor();
        app
    }

    fn file_idx(app: &App, name: &str) -> usize {
        app.files
            .iter()
            .position(|f| f.path == Path::new(name))
            .unwrap_or_else(|| panic!("file {name} not in app.files"))
    }

    #[test]
    fn build_layout_produces_header_then_hunks_then_spacer_per_file() {
        let app = fake_app(vec![make_file(
            "a.rs",
            vec![hunk(1, vec![diff_line(LineKind::Added, "x")])],
            100,
        )]);

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
                vec![
                    hunk(1, vec![diff_line(LineKind::Added, "x")]),
                    hunk(10, vec![diff_line(LineKind::Added, "y")]),
                ],
                200,
            ),
            // b.rs: older, 1 hunk
            make_file(
                "b.rs",
                vec![hunk(1, vec![diff_line(LineKind::Added, "z")])],
                100,
            ),
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
            vec![
                hunk(1, vec![diff_line(LineKind::Added, "x")]),
                hunk(10, vec![diff_line(LineKind::Added, "y")]),
            ],
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
    fn follow_target_row_is_last_diff_row_of_newest_file() {
        // ADR-0009 fix: follow must park on the **last content row**
        // of the newest file, not on the newest hunk's header. With
        // the old behaviour a tall last hunk would pin the viewport
        // to the top of the hunk and hide the newest added/deleted
        // lines — the opposite of what `tail -f`-style monitoring
        // should do.
        //
        // newest.rs has the largest mtime → ends up at the bottom of
        // the mtime-ascending layout. Its second hunk's DiffLine is
        // the very last row; follow should land there.
        let app = fake_app(vec![
            make_file(
                "older.rs",
                vec![hunk(1, vec![diff_line(LineKind::Added, "c")])],
                100,
            ),
            make_file(
                "newest.rs",
                vec![
                    hunk(1, vec![diff_line(LineKind::Added, "a")]),
                    hunk(20, vec![diff_line(LineKind::Added, "b")]),
                ],
                300,
            ),
        ]);
        assert!(
            matches!(app.layout.rows[app.scroll], RowKind::DiffLine { .. }),
            "follow target must be an actual DiffLine row, got {:?}",
            app.layout.rows[app.scroll]
        );
        // The last DiffLine of the newest file is the target. Trailing
        // Spacer rows are cosmetic padding and must be skipped by
        // `follow_target_row`.
        let newest_idx = app.files.len() - 1;
        let last_diff_in_newest = app
            .layout
            .rows
            .iter()
            .enumerate()
            .rev()
            .find_map(|(i, r)| match r {
                RowKind::DiffLine { file_idx, .. } if *file_idx == newest_idx => Some(i),
                _ => None,
            })
            .expect("newest file must contain at least one DiffLine");
        assert_eq!(app.scroll, last_diff_in_newest);
    }

    #[test]
    fn follow_target_row_reveals_tail_of_tall_last_hunk() {
        // Regression for Codex round-4 finding: under the old design
        // a tall final hunk would pin follow to its header row, so
        // the newest ~hunk_size - viewport lines of the edit were
        // always off-screen. A 20-line hunk is the minimal reproducer:
        // follow must park on the 20th DiffLine, not the hunk header.
        let huge_hunk = hunk(
            1,
            (0..20)
                .map(|i| diff_line(LineKind::Added, &format!("line {i}")))
                .collect(),
        );
        let app = fake_app(vec![make_file("big.rs", vec![huge_hunk], 500)]);
        assert!(matches!(
            app.layout.rows[app.scroll],
            RowKind::DiffLine { line_idx: 19, .. }
        ));
    }

    #[test]
    fn current_file_path_reports_the_file_under_the_cursor() {
        let mut app = fake_app(vec![
            make_file(
                "a.rs",
                vec![hunk(1, vec![diff_line(LineKind::Added, "x")])],
                200,
            ),
            make_file(
                "b.rs",
                vec![hunk(1, vec![diff_line(LineKind::Added, "y")])],
                100,
            ),
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
    fn handle_key_j_jumps_to_next_hunk_and_disables_follow() {
        // After M4v.swap, lowercase `j` is hunk-forward.
        let mut app = fake_app(vec![make_file(
            "a.rs",
            vec![
                hunk(1, vec![diff_line(LineKind::Added, "x")]),
                hunk(20, vec![diff_line(LineKind::Added, "y")]),
            ],
            100,
        )]);
        app.scroll_to(0);
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.scroll, app.layout.hunk_starts[0]);
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
        let mut app = fake_app(vec![make_file(
            "a.rs",
            vec![
                hunk(1, vec![diff_line(LineKind::Added, "alpha")]),
                hunk(10, vec![diff_line(LineKind::Added, "beta")]),
            ],
            100,
        )]);
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
        let lines: Vec<DiffLine> = (0..20)
            .map(|i| diff_line(LineKind::Added, &format!("line {i}")))
            .collect();
        let mut app = fake_app(vec![make_file("a.rs", vec![hunk(1, lines)], 100)]);
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
    fn lowercase_j_in_long_hunk_scrolls_by_chunk() {
        // Force a small body height so the chunk size is exactly 5 rows
        // (15 / 3 = 5) — this way the test's expected scroll positions
        // are fixed regardless of the DEFAULT_BODY_HEIGHT used outside
        // of tests.
        let lines: Vec<DiffLine> = (0..20)
            .map(|i| diff_line(LineKind::Added, &format!("line {i}")))
            .collect();
        let mut app = fake_app(vec![make_file("a.rs", vec![hunk(1, lines)], 100)]);
        app.last_body_height.set(15);
        let chunk = app.chunk_size();
        assert_eq!(chunk, 5);
        let (start, end) = app.layout.change_runs[0];
        let last = end - 1;

        app.scroll_to(start);
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.scroll, start + chunk);

        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.scroll, start + 2 * chunk);

        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.scroll, start + 3 * chunk);

        // Subsequent presses clamp at the last row of the run.
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.scroll, last);
    }

    #[test]
    fn l_crosses_hunk_and_file_boundaries() {
        // v0.2 remap: `l` walks to the next hunk regardless of the
        // file boundary between them. One tiny hunk per file so the
        // jump has to cross from a.rs into b.rs.
        let mut app = fake_app(vec![
            make_file(
                "a.rs",
                vec![hunk(1, vec![diff_line(LineKind::Added, "alpha")])],
                100,
            ),
            make_file(
                "b.rs",
                vec![hunk(1, vec![diff_line(LineKind::Added, "beta")])],
                200,
            ),
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
    fn lowercase_k_in_long_hunk_walks_back_by_chunk() {
        let lines: Vec<DiffLine> = (0..20)
            .map(|i| diff_line(LineKind::Added, &format!("line {i}")))
            .collect();
        let mut app = fake_app(vec![make_file("a.rs", vec![hunk(1, lines)], 100)]);
        app.last_body_height.set(15);
        let chunk = app.chunk_size();
        // Hunk spans header + 20 diff rows → [1, 22). viewport = 15 < 21,
        // so `k` chunk-scrolls back clamped to the header row.
        let hunk_top = app.layout.hunk_starts[0];
        let last = 21;

        app.scroll_to(last);
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(app.scroll, last - chunk);

        // Continue back; scroll must stay within the hunk's row range,
        // flooring at the hunk header.
        app.handle_key(key(KeyCode::Char('k')));
        app.handle_key(key(KeyCode::Char('k')));
        app.handle_key(key(KeyCode::Char('k')));
        assert!(app.scroll >= hunk_top);
    }

    #[test]
    fn h_jumps_to_previous_hunk_header() {
        // v0.2 remap: `h` is the strict previous-hunk jump that the
        // old SHIFT-K used to do. Two short hunks, cursor on the
        // second — pressing `h` lands on the first hunk header.
        let mut app = fake_app(vec![make_file(
            "a.rs",
            vec![
                hunk(1, vec![diff_line(LineKind::Added, "alpha")]),
                hunk(10, vec![diff_line(LineKind::Added, "beta")]),
            ],
            100,
        )]);
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
        let mut app = fake_app(vec![make_file(
            "a.rs",
            vec![hunk(
                1,
                vec![
                    diff_line(LineKind::Added, "one"),
                    diff_line(LineKind::Added, "two"),
                    diff_line(LineKind::Added, "three"),
                ],
            )],
            100,
        )]);
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
        let mut app = fake_app(vec![make_file(
            "a.rs",
            vec![hunk(
                1,
                vec![
                    diff_line(LineKind::Added, "one"),
                    diff_line(LineKind::Added, "two"),
                    diff_line(LineKind::Added, "three"),
                ],
            )],
            100,
        )]);
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
        let lines: Vec<DiffLine> = (0..20)
            .map(|i| diff_line(LineKind::Added, &format!("line {i}")))
            .collect();
        let mut app = fake_app(vec![make_file(
            "a.rs",
            vec![
                hunk(1, lines),
                hunk(100, vec![diff_line(LineKind::Added, "tail")]),
            ],
            100,
        )]);
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
            make_file(
                "before.rs",
                vec![hunk(
                    1,
                    vec![
                        diff_line(LineKind::Context, " a"),
                        diff_line(LineKind::Context, " b"),
                        diff_line(LineKind::Context, " c"),
                        diff_line(LineKind::Context, " d"),
                    ],
                )],
                100,
            ),
            make_file(
                "target.rs",
                vec![hunk(
                    1,
                    vec![
                        diff_line(LineKind::Added, "alpha"),
                        diff_line(LineKind::Added, "beta"),
                    ],
                )],
                200,
            ),
            make_file(
                "after.rs",
                vec![hunk(
                    1,
                    vec![
                        diff_line(LineKind::Context, " a"),
                        diff_line(LineKind::Context, " b"),
                        diff_line(LineKind::Context, " c"),
                        diff_line(LineKind::Context, " d"),
                    ],
                )],
                300,
            ),
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
        let lines: Vec<DiffLine> = (0..40)
            .map(|i| diff_line(LineKind::Added, &format!("line {i}")))
            .collect();
        let mut app = fake_app(vec![make_file("a.rs", vec![hunk(1, lines)], 100)]);
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
            make_file(
                "before.rs",
                vec![hunk(
                    1,
                    vec![
                        diff_line(LineKind::Context, " a"),
                        diff_line(LineKind::Context, " b"),
                        diff_line(LineKind::Context, " c"),
                        diff_line(LineKind::Context, " d"),
                    ],
                )],
                100,
            ),
            make_file(
                "target.rs",
                vec![hunk(
                    1,
                    vec![
                        diff_line(LineKind::Added, "alpha"),
                        diff_line(LineKind::Added, "beta"),
                    ],
                )],
                200,
            ),
            make_file(
                "after.rs",
                vec![hunk(
                    1,
                    vec![
                        diff_line(LineKind::Context, " a"),
                        diff_line(LineKind::Context, " b"),
                        diff_line(LineKind::Context, " c"),
                        diff_line(LineKind::Context, " d"),
                    ],
                )],
                300,
            ),
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
        let lines: Vec<DiffLine> = (0..40)
            .map(|i| diff_line(LineKind::Added, &format!("line {i}")))
            .collect();
        let mut app = fake_app(vec![make_file("a.rs", vec![hunk(1, lines)], 100)]);
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
        let mut app = fake_app(vec![make_file(
            "a.rs",
            vec![hunk(
                1,
                vec![
                    diff_line(LineKind::Added, "alpha"),
                    diff_line(LineKind::Added, "beta"),
                ],
            )],
            100,
        )]);
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
        let app = fake_app(vec![make_file(
            "a.rs",
            vec![hunk(
                1,
                vec![
                    diff_line(LineKind::Added, "a"),
                    diff_line(LineKind::Added, "b"),
                    diff_line(LineKind::Deleted, "c"),
                ],
            )],
            100,
        )]);
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

    #[test]
    fn handle_key_g_and_capital_g_move_to_top_and_bottom() {
        let mut app = fake_app(vec![make_file(
            "a.rs",
            vec![hunk(
                1,
                vec![
                    diff_line(LineKind::Added, "x"),
                    diff_line(LineKind::Added, "y"),
                    diff_line(LineKind::Added, "z"),
                ],
            )],
            100,
        )]);
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
            make_file(
                "a.rs",
                vec![hunk(1, vec![diff_line(LineKind::Added, "x")])],
                100,
            ),
            make_file(
                "b.rs",
                vec![hunk(1, vec![diff_line(LineKind::Added, "y")])],
                200,
            ),
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
            make_file(
                "a.rs",
                vec![hunk(1, vec![diff_line(LineKind::Added, "x")])],
                100,
            ),
            make_file(
                "b.rs",
                vec![hunk(1, vec![diff_line(LineKind::Added, "y")])],
                200,
            ),
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
        let mut app = fake_app(vec![make_file(
            "a.rs",
            vec![
                hunk(1, vec![diff_line(LineKind::Added, "x")]),
                hunk(20, vec![diff_line(LineKind::Added, "y")]),
            ],
            100,
        )]);
        app.handle_key(key(KeyCode::Char('g'))); // jump to top, drops follow
        assert!(!app.follow_mode);
        app.handle_key(key(KeyCode::Char('f')));
        assert!(app.follow_mode);
        // ADR-0009 fix: follow target = last **DiffLine** of the
        // newest file's last hunk, not the hunk header. This is the
        // row that actually shows the newest edit.
        assert!(matches!(
            app.layout.rows[app.scroll],
            RowKind::DiffLine { .. }
        ));
        let newest = app.files.len() - 1;
        let last_diff = app
            .layout
            .rows
            .iter()
            .enumerate()
            .rev()
            .find_map(|(i, r)| match r {
                RowKind::DiffLine { file_idx, .. } if *file_idx == newest => Some(i),
                _ => None,
            })
            .expect("newest file has a DiffLine");
        assert_eq!(app.scroll, last_diff);
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
    fn s_opens_picker_and_esc_closes_it() {
        // v0.2 remap: picker trigger moved from `Space` to `s` so
        // `Space` is free for the scar "seen" mark (wired up in a
        // later M4 slice).
        let mut app = fake_app(vec![make_file(
            "a.rs",
            vec![hunk(1, vec![diff_line(LineKind::Added, "x")])],
            100,
        )]);
        app.handle_key(key(KeyCode::Char('s')));
        assert!(app.picker.is_some());

        app.handle_key(key(KeyCode::Esc));
        assert!(app.picker.is_none());
    }

    #[test]
    fn space_toggles_seen_mark_on_current_hunk() {
        // M4 slice 6: Space flips the cursor's enclosing hunk
        // into and out of the "seen" set. Pure TUI state — no
        // file write, no cursor movement, no picker.
        let mut app = fake_app(vec![make_file(
            "a.rs",
            vec![hunk(1, vec![diff_line(LineKind::Added, "x")])],
            100,
        )]);
        cursor_on_nth_diff_line(&mut app, 0);
        let before_scroll = app.scroll;

        app.handle_key(key(KeyCode::Char(' ')));

        assert!(
            app.hunk_is_seen(0, 0),
            "Space must toggle the current hunk into the seen set"
        );
        assert!(app.picker.is_none(), "Space must not open the picker");
        assert_eq!(app.scroll, before_scroll, "Space must not move cursor");

        // Second press removes the mark.
        app.handle_key(key(KeyCode::Char(' ')));
        assert!(
            !app.hunk_is_seen(0, 0),
            "a second Space must remove the seen mark"
        );
    }

    #[test]
    fn space_on_file_header_row_is_noop() {
        let mut app = fake_app(vec![make_file(
            "a.rs",
            vec![hunk(1, vec![diff_line(LineKind::Added, "x")])],
            100,
        )]);
        let header_row = app
            .layout
            .rows
            .iter()
            .position(|r| matches!(r, RowKind::FileHeader { .. }))
            .expect("header");
        app.scroll_to(header_row);

        app.handle_key(key(KeyCode::Char(' ')));

        assert!(
            app.seen_hunks.is_empty(),
            "file-header Space must not add anything to seen_hunks"
        );
    }

    #[test]
    fn seen_mark_persists_across_a_recompute_that_preserves_hunk_old_start() {
        // seen_hunks is keyed by (path, hunk.old_start) so a
        // watcher-driven recompute that rebuilds the FileDiff
        // list without moving the hunk's pre-image anchor must
        // leave the mark in place.
        let mut app = fake_app(vec![make_file(
            "a.rs",
            vec![hunk(42, vec![diff_line(LineKind::Added, "x")])],
            100,
        )]);
        cursor_on_nth_diff_line(&mut app, 0);
        app.handle_key(key(KeyCode::Char(' ')));
        assert!(app.hunk_is_seen(0, 0));

        let fresh = vec![make_file(
            "a.rs",
            vec![hunk(42, vec![diff_line(LineKind::Added, "y")])],
            100,
        )];
        app.apply_computed_files(fresh);

        assert!(
            app.hunk_is_seen(0, 0),
            "recompute that preserves hunk.old_start must preserve the seen mark"
        );
    }

    #[test]
    fn picker_filters_by_substring_case_insensitively() {
        let mut app = fake_app(vec![
            make_file(
                "src/Auth.rs",
                vec![hunk(1, vec![diff_line(LineKind::Added, "x")])],
                300,
            ),
            make_file(
                "src/handler.rs",
                vec![hunk(1, vec![diff_line(LineKind::Added, "y")])],
                200,
            ),
            make_file(
                "tests/auth_test.rs",
                vec![hunk(1, vec![diff_line(LineKind::Added, "z")])],
                100,
            ),
        ]);
        app.open_picker();
        for c in "auth".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
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
            make_file(
                "newest.rs",
                vec![hunk(1, vec![diff_line(LineKind::Added, "x")])],
                300,
            ),
            make_file(
                "older.rs",
                vec![hunk(50, vec![diff_line(LineKind::Added, "y")])],
                100,
            ),
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

        let kept = FileDiff {
            path: PathBuf::from("kept.rs"),
            status: FileStatus::Modified,
            added: 1,
            deleted: 0,
            content: DiffContent::Text(vec![hunk(1, vec![diff_line(LineKind::Added, "hi2")])]),
            mtime: SystemTime::UNIX_EPOCH,
            header_prefix: None,
        };
        let gone = FileDiff {
            path: PathBuf::from("gone.rs"),
            status: FileStatus::Deleted,
            added: 0,
            deleted: 1,
            content: DiffContent::Text(vec![hunk(1, vec![diff_line(LineKind::Deleted, "bye")])]),
            mtime: SystemTime::UNIX_EPOCH,
            header_prefix: None,
        };

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
            make_file(
                "older.rs",
                vec![hunk(1, vec![diff_line(LineKind::Added, "keep me")])],
                100,
            ),
            make_file(
                "newer.rs",
                vec![hunk(2, vec![diff_line(LineKind::Added, "also keep")])],
                200,
            ),
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
        let mut app = fake_app(vec![make_file(
            "a.rs",
            vec![hunk(1, vec![diff_line(LineKind::Added, "x")])],
            100,
        )]);
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
        let mut app = fake_app(vec![make_file(
            "a.rs",
            vec![hunk(1, vec![diff_line(LineKind::Added, "x")])],
            100,
        )]);

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
        let mut app = fake_app(vec![make_file(
            "old.rs",
            vec![hunk(1, vec![diff_line(LineKind::Added, "stale")])],
            100,
        )]);
        app.head_dirty = true;
        app.last_error = Some("stale error".into());

        let new_file = FileDiff {
            path: PathBuf::from("fresh.rs"),
            status: FileStatus::Modified,
            added: 1,
            deleted: 0,
            content: DiffContent::Text(vec![hunk(1, vec![diff_line(LineKind::Added, "fresh")])]),
            mtime: SystemTime::UNIX_EPOCH + Duration::from_secs(500),
            header_prefix: None,
        };
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
            make_file(
                "newer.rs",
                vec![hunk(1, vec![diff_line(LineKind::Added, "a")])],
                200,
            ),
            make_file(
                "older.rs",
                vec![hunk(1, vec![diff_line(LineKind::Added, "b")])],
                100,
            ),
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
    fn picker_enter_disables_follow_mode_so_selection_survives_recompute() {
        // bootstrap lands in follow mode. A picker selection is an
        // explicit manual navigation — the next recompute must not yank
        // the user back to the newest file's last hunk.
        let mut app = fake_app(vec![
            make_file(
                "newest.rs",
                vec![hunk(1, vec![diff_line(LineKind::Added, "x")])],
                300,
            ),
            make_file(
                "older.rs",
                vec![hunk(50, vec![diff_line(LineKind::Added, "y")])],
                100,
            ),
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
            vec![
                hunk(1, vec![diff_line(LineKind::Added, "x")]),
                hunk(30, vec![diff_line(LineKind::Added, "z")]),
            ],
            400,
        );
        app.files.sort_by(|a, b| a.mtime.cmp(&b.mtime));
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
            make_file(
                "newest.rs",
                vec![hunk(1, vec![diff_line(LineKind::Added, "x")])],
                300,
            ),
            make_file(
                "older.rs",
                vec![hunk(1, vec![diff_line(LineKind::Added, "y")])],
                100,
            ),
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
            make_file(
                "brand_new.rs",
                vec![hunk(1, vec![diff_line(LineKind::Added, "z")])],
                400,
            ),
            make_file(
                "newest.rs",
                vec![hunk(1, vec![diff_line(LineKind::Added, "x")])],
                300,
            ),
            make_file(
                "older.rs",
                vec![hunk(1, vec![diff_line(LineKind::Added, "y")])],
                100,
            ),
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
            make_file(
                "a.rs",
                vec![hunk(1, vec![diff_line(LineKind::Added, "x")])],
                200,
            ),
            make_file(
                "b.rs",
                vec![hunk(42, vec![diff_line(LineKind::Added, "y")])],
                100,
            ),
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
        app.files.push(make_file(
            "c.rs",
            vec![hunk(1, vec![diff_line(LineKind::Added, "z")])],
            50, // older than b.rs
        ));
        app.files.sort_by(|x, y| x.mtime.cmp(&y.mtime));
        app.build_layout();
        app.refresh_anchor();

        assert_eq!(app.current_file_path(), Some(Path::new("b.rs")));
    }

    #[test]
    fn refresh_anchor_keeps_manual_mode_on_same_file_when_hunk_identity_changes() {
        let mut app = fake_app(vec![
            make_file(
                "newest.rs",
                vec![hunk(1, vec![diff_line(LineKind::Added, "x")])],
                300,
            ),
            make_file(
                "older.rs",
                vec![hunk(50, vec![diff_line(LineKind::Added, "y")])],
                100,
            ),
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
        app.files[older] = make_file(
            "older.rs",
            vec![hunk(99, vec![diff_line(LineKind::Added, "y2")])],
            100,
        );
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
        let mut app = fake_app(vec![make_file(
            "only.rs",
            vec![
                hunk(10, vec![diff_line(LineKind::Added, "first")]),
                hunk(50, vec![diff_line(LineKind::Added, "second")]),
            ],
            100,
        )]);

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
                hunk(10, vec![diff_line(LineKind::Added, "first")]),
                hunk(60, vec![diff_line(LineKind::Added, "second shifted")]),
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
        let mut app = fake_app(vec![make_file(
            "a.rs",
            vec![
                hunk(1, vec![diff_line(LineKind::Added, "x")]),
                hunk(
                    10,
                    vec![
                        diff_line(LineKind::Added, "y1"),
                        diff_line(LineKind::Added, "y2"),
                    ],
                ),
            ],
            100,
        )]);
        app.anim = None;
        app.scroll = 0;
        app.scroll_to(3);
        assert!(app.anim.is_some(), "anim should be set after scroll_to");
    }

    #[test]
    fn scroll_to_does_not_start_animation_on_noop() {
        let mut app = fake_app(vec![make_file(
            "a.rs",
            vec![hunk(1, vec![diff_line(LineKind::Added, "x")])],
            100,
        )]);
        app.anim = None;
        let current = app.scroll;
        app.scroll_to(current);
        assert!(app.anim.is_none(), "no-op scroll must not start anim");
    }

    #[test]
    fn scroll_to_carries_current_visual_into_animation_from() {
        let mut app = fake_app(vec![make_file(
            "a.rs",
            vec![
                hunk(1, vec![diff_line(LineKind::Added, "x")]),
                hunk(
                    20,
                    vec![
                        diff_line(LineKind::Added, "y1"),
                        diff_line(LineKind::Added, "y2"),
                    ],
                ),
            ],
            100,
        )]);
        app.scroll = 0;
        app.anim = None;
        app.visual_top.set(7.25);
        app.scroll_to(3);
        let from = app.anim.as_ref().expect("anim set").from;
        assert!((from - 7.25).abs() < 1e-4);
    }

    #[test]
    fn tick_anim_clears_anim_once_duration_elapsed() {
        let mut app = fake_app(vec![make_file(
            "a.rs",
            vec![hunk(1, vec![diff_line(LineKind::Added, "x")])],
            100,
        )]);
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
        let mut app = fake_app(vec![make_file(
            "a.rs",
            vec![hunk(1, vec![diff_line(LineKind::Added, "x")])],
            100,
        )]);
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
            make_file(
                "a.rs",
                vec![hunk(
                    1,
                    (0..8)
                        .map(|i| diff_line(LineKind::Added, &format!("a{i}")))
                        .collect(),
                )],
                100,
            ),
            make_file(
                "b.rs",
                vec![hunk(
                    1,
                    (0..8)
                        .map(|i| diff_line(LineKind::Added, &format!("b{i}")))
                        .collect(),
                )],
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
            make_file(
                "a.rs",
                vec![hunk(
                    1,
                    (0..8)
                        .map(|i| diff_line(LineKind::Added, &format!("a{i}")))
                        .collect(),
                )],
                100,
            ),
            make_file(
                "b.rs",
                vec![hunk(
                    1,
                    (0..8)
                        .map(|i| diff_line(LineKind::Added, &format!("b{i}")))
                        .collect(),
                )],
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
        fake_app(vec![make_file(
            "a.rs",
            vec![hunk(1, vec![diff_line(LineKind::Added, &content)])],
            100,
        )])
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
        let diff_row = app
            .layout
            .rows
            .iter()
            .position(|r| matches!(r, RowKind::DiffLine { .. }))
            .expect("layout must contain a DiffLine");
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
        let mut app = fake_app(vec![make_file(
            "a.rs",
            vec![hunk(
                1,
                vec![
                    diff_line(LineKind::Added, &long_content),
                    diff_line(LineKind::Added, &short_content),
                ],
            )],
            100,
        )]);
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
        let diff_row = app
            .layout
            .rows
            .iter()
            .position(|r| matches!(r, RowKind::DiffLine { .. }))
            .expect("layout has a DiffLine");
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
        app.apply_computed_files(vec![make_file(
            "a.rs",
            vec![hunk(1, vec![diff_line(LineKind::Added, "x")])],
            100,
        )]);

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

        app.apply_computed_files(vec![make_file(
            "a.rs",
            vec![hunk(1, vec![diff_line(LineKind::Added, "x")])],
            100,
        )]);

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

    #[test]
    fn handle_key_a_inserts_ask_scar_above_cursor_line() {
        let tmp = tempfile::tempdir().expect("tmp");
        // Simulate a diff where line 2 of main.rs was newly added. The
        // cursor lands on that added row, and pressing `a` should insert
        // the canned "ask" scar directly above it.
        let mut app = scar_app_with_real_fs(
            &tmp,
            "src/main.rs",
            "fn one() {}\nfn two() {}\n",
            2,
            vec![diff_line(LineKind::Added, "fn two() {}")],
        );
        cursor_on_nth_diff_line(&mut app, 0);

        app.handle_key(key(KeyCode::Char('a')));

        let after = std::fs::read_to_string(tmp.path().join("src/main.rs")).expect("read back");
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
        let mut app = scar_app_with_real_fs(
            &tmp,
            "auth.py",
            "def main():\n    return 1\n",
            2,
            vec![diff_line(LineKind::Added, "    return 1")],
        );
        cursor_on_nth_diff_line(&mut app, 0);

        app.handle_key(key(KeyCode::Char('r')));

        let after = std::fs::read_to_string(tmp.path().join("auth.py")).expect("read back");
        assert_eq!(
            after, "def main():\n# @kizu[reject]: revert this change\n    return 1\n",
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
        let mut app = scar_app_with_real_fs(
            &tmp,
            "lib.rs",
            original,
            1,
            vec![diff_line(LineKind::Added, "fn one() {}")],
        );
        // Park the cursor on the FileHeader row explicitly.
        let file_header_row = app
            .layout
            .rows
            .iter()
            .position(|r| matches!(r, RowKind::FileHeader { .. }))
            .expect("file header exists");
        app.scroll_to(file_header_row);

        app.handle_key(key(KeyCode::Char('a')));

        let after = std::fs::read_to_string(tmp.path().join("lib.rs")).expect("read back");
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
        let file = make_file(
            "ghost.rs",
            vec![hunk(1, vec![diff_line(LineKind::Added, "fn missing()")])],
            100,
        );
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
        let mut app = fake_app(vec![make_file(
            "a.rs",
            vec![hunk(
                42,
                vec![
                    diff_line(LineKind::Added, "first"),
                    diff_line(LineKind::Added, "second"),
                ],
            )],
            100,
        )]);
        let header_row = app
            .layout
            .rows
            .iter()
            .position(|r| matches!(r, RowKind::HunkHeader { .. }))
            .expect("hunk header row exists");
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
        let mut app = fake_app(vec![make_file(
            "a.rs",
            vec![hunk(
                10,
                vec![
                    diff_line(LineKind::Context, "ctx1"),
                    diff_line(LineKind::Context, "ctx2"),
                    diff_line(LineKind::Added, "new_stuff"),
                    diff_line(LineKind::Context, "ctx3"),
                ],
            )],
            100,
        )]);
        let header_row = app
            .layout
            .rows
            .iter()
            .position(|r| matches!(r, RowKind::HunkHeader { .. }))
            .expect("hunk header row exists");
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
        let mut app = scar_app_with_real_fs(
            &tmp,
            "src/lib.rs",
            "line_a\nline_b\n",
            2,
            vec![diff_line(LineKind::Added, "line_b")],
        );
        let header_row = app
            .layout
            .rows
            .iter()
            .position(|r| matches!(r, RowKind::HunkHeader { .. }))
            .expect("hunk header row exists");
        app.scroll_to(header_row);

        app.handle_key(key(KeyCode::Char('a')));

        let after = std::fs::read_to_string(tmp.path().join("src/lib.rs")).expect("read back");
        assert_eq!(
            after, "line_a\n// @kizu[ask]: explain this change\nline_b\n",
            "`a` on a hunk header must drop the scar above hunk.new_start",
        );
    }

    // ---- M4 slice 3: `c` free-text scar overlay --------------------

    #[test]
    fn handle_key_c_opens_scar_comment_overlay_with_captured_target() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut app = scar_app_with_real_fs(
            &tmp,
            "src/foo.rs",
            "fn alpha() {}\nfn beta() {}\n",
            2,
            vec![diff_line(LineKind::Added, "fn beta() {}")],
        );
        cursor_on_nth_diff_line(&mut app, 0);

        app.handle_key(key(KeyCode::Char('c')));

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
        let after = std::fs::read_to_string(tmp.path().join("src/foo.rs")).expect("read");
        assert_eq!(
            after, "fn alpha() {}\nfn beta() {}\n",
            "`c` must not touch the file until `Enter` commits"
        );
    }

    #[test]
    fn handle_key_c_is_noop_on_file_header_row() {
        let tmp = tempfile::tempdir().expect("tmp");
        let original = "fn one() {}\n";
        let mut app = scar_app_with_real_fs(
            &tmp,
            "lib.rs",
            original,
            1,
            vec![diff_line(LineKind::Added, "fn one() {}")],
        );
        let header_row = app
            .layout
            .rows
            .iter()
            .position(|r| matches!(r, RowKind::FileHeader { .. }))
            .expect("file header exists");
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
        let mut app = scar_app_with_real_fs(
            &tmp,
            "a.rs",
            "x\ny\n",
            2,
            vec![diff_line(LineKind::Added, "y")],
        );
        cursor_on_nth_diff_line(&mut app, 0);
        app.handle_key(key(KeyCode::Char('c')));

        app.handle_key(key(KeyCode::Char('h')));
        app.handle_key(key(KeyCode::Char('i')));
        app.handle_key(key(KeyCode::Char('!')));

        let state = app.scar_comment.as_ref().expect("still open");
        assert_eq!(state.body, "hi!");
    }

    #[test]
    fn scar_comment_backspace_deletes_last_character() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut app = scar_app_with_real_fs(
            &tmp,
            "a.rs",
            "x\ny\n",
            2,
            vec![diff_line(LineKind::Added, "y")],
        );
        cursor_on_nth_diff_line(&mut app, 0);
        app.handle_key(key(KeyCode::Char('c')));
        for ch in "ab".chars() {
            app.handle_key(key(KeyCode::Char(ch)));
        }

        app.handle_key(key(KeyCode::Backspace));
        let state = app.scar_comment.as_ref().expect("still open");
        assert_eq!(state.body, "a");
    }

    #[test]
    fn scar_comment_esc_cancels_without_writing_to_file() {
        let tmp = tempfile::tempdir().expect("tmp");
        let original = "fn one() {}\nfn two() {}\n";
        let mut app = scar_app_with_real_fs(
            &tmp,
            "cancel.rs",
            original,
            2,
            vec![diff_line(LineKind::Added, "fn two() {}")],
        );
        cursor_on_nth_diff_line(&mut app, 0);
        app.handle_key(key(KeyCode::Char('c')));
        for ch in "dont".chars() {
            app.handle_key(key(KeyCode::Char(ch)));
        }

        app.handle_key(key(KeyCode::Esc));

        assert!(app.scar_comment.is_none(), "Esc closes the overlay");
        let after = std::fs::read_to_string(tmp.path().join("cancel.rs")).expect("read");
        assert_eq!(after, original, "cancel must not touch the file");
        assert!(app.last_error.is_none(), "cancel is not an error");
    }

    #[test]
    fn scar_comment_enter_commits_free_scar_above_target_line() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut app = scar_app_with_real_fs(
            &tmp,
            "commit.rs",
            "fn one() {}\nfn two() {}\n",
            2,
            vec![diff_line(LineKind::Added, "fn two() {}")],
        );
        cursor_on_nth_diff_line(&mut app, 0);
        app.handle_key(key(KeyCode::Char('c')));
        for ch in "why two?".chars() {
            app.handle_key(key(KeyCode::Char(ch)));
        }

        app.handle_key(key(KeyCode::Enter));

        assert!(app.scar_comment.is_none(), "commit closes the overlay");
        let after = std::fs::read_to_string(tmp.path().join("commit.rs")).expect("read");
        assert_eq!(
            after, "fn one() {}\n// @kizu[free]: why two?\nfn two() {}\n",
            "Enter must write a free-scar above the captured target line"
        );
    }

    #[test]
    fn scar_comment_enter_on_empty_body_is_cancel() {
        let tmp = tempfile::tempdir().expect("tmp");
        let original = "fn one() {}\nfn two() {}\n";
        let mut app = scar_app_with_real_fs(
            &tmp,
            "empty.rs",
            original,
            2,
            vec![diff_line(LineKind::Added, "fn two() {}")],
        );
        cursor_on_nth_diff_line(&mut app, 0);
        app.handle_key(key(KeyCode::Char('c')));
        app.handle_key(key(KeyCode::Enter));

        assert!(
            app.scar_comment.is_none(),
            "empty commit closes the overlay"
        );
        let after = std::fs::read_to_string(tmp.path().join("empty.rs")).expect("read");
        assert_eq!(after, original, "empty body must not write a blank scar");
    }

    #[test]
    fn normal_keys_are_inert_while_scar_comment_overlay_is_open() {
        // While the overlay is open, typing `q` must accumulate into
        // the body instead of quitting the app. Proves the router
        // correctly parks normal-mode dispatch behind the overlay.
        let tmp = tempfile::tempdir().expect("tmp");
        let mut app = scar_app_with_real_fs(
            &tmp,
            "quit.rs",
            "x\ny\n",
            2,
            vec![diff_line(LineKind::Added, "y")],
        );
        cursor_on_nth_diff_line(&mut app, 0);
        app.handle_key(key(KeyCode::Char('c')));

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
        let header_row = app
            .layout
            .rows
            .iter()
            .position(|r| matches!(r, RowKind::FileHeader { .. }))
            .expect("header");
        app.scroll_to(header_row);

        app.handle_key(key(KeyCode::Enter));
        assert!(app.file_view.is_none());
    }

    // ---- M4b slice 1: `/` search + first-match jump ---------------

    fn find_first_row_matching<F: Fn(&RowKind) -> bool>(app: &App, f: F) -> usize {
        app.layout.rows.iter().position(f).expect("row exists")
    }

    #[test]
    fn find_matches_returns_empty_for_empty_query() {
        let app = fake_app(vec![make_file(
            "a.rs",
            vec![hunk(1, vec![diff_line(LineKind::Added, "hello world")])],
            100,
        )]);
        let m = find_matches(&app.layout, &app.files, "");
        assert!(m.is_empty());
    }

    #[test]
    fn find_matches_finds_substring_case_insensitive_when_query_is_lowercase() {
        let app = fake_app(vec![make_file(
            "a.rs",
            vec![hunk(
                1,
                vec![
                    diff_line(LineKind::Added, "Hello WORLD"),
                    diff_line(LineKind::Context, "no match here"),
                    diff_line(LineKind::Added, "World wide"),
                ],
            )],
            100,
        )]);
        let m = find_matches(&app.layout, &app.files, "world");
        assert_eq!(m.len(), 2, "smart-case lowercase query matches both rows");
        assert!(m.iter().all(|loc| loc.byte_start < loc.byte_end));
    }

    #[test]
    fn find_matches_is_case_sensitive_when_query_has_uppercase() {
        let app = fake_app(vec![make_file(
            "a.rs",
            vec![hunk(
                1,
                vec![
                    diff_line(LineKind::Added, "hello World"),
                    diff_line(LineKind::Added, "hello world"),
                ],
            )],
            100,
        )]);
        let m = find_matches(&app.layout, &app.files, "World");
        assert_eq!(m.len(), 1, "uppercase query is case-sensitive");
    }

    #[test]
    fn find_matches_captures_multiple_hits_on_one_row() {
        let app = fake_app(vec![make_file(
            "a.rs",
            vec![hunk(1, vec![diff_line(LineKind::Added, "foo foo foo")])],
            100,
        )]);
        let m = find_matches(&app.layout, &app.files, "foo");
        assert_eq!(m.len(), 3);
        assert_eq!(m[0].byte_start, 0);
        assert_eq!(m[1].byte_start, 4);
        assert_eq!(m[2].byte_start, 8);
    }

    #[test]
    fn slash_opens_search_input_composer() {
        let mut app = fake_app(vec![make_file(
            "a.rs",
            vec![hunk(1, vec![diff_line(LineKind::Added, "x")])],
            100,
        )]);

        app.handle_key(key(KeyCode::Char('/')));

        assert!(app.search_input.is_some(), "/ must open the composer");
        assert_eq!(app.search_input.as_ref().unwrap().query, "");
    }

    #[test]
    fn search_input_typing_appends_to_query_and_backspace_deletes() {
        let mut app = fake_app(vec![make_file(
            "a.rs",
            vec![hunk(1, vec![diff_line(LineKind::Added, "x")])],
            100,
        )]);
        app.handle_key(key(KeyCode::Char('/')));
        for c in "foo".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(app.search_input.as_ref().unwrap().query, "foo");
        app.handle_key(key(KeyCode::Backspace));
        assert_eq!(app.search_input.as_ref().unwrap().query, "fo");
    }

    #[test]
    fn search_input_esc_cancels_without_installing_search_state() {
        let mut app = fake_app(vec![make_file(
            "a.rs",
            vec![hunk(1, vec![diff_line(LineKind::Added, "foo")])],
            100,
        )]);
        app.handle_key(key(KeyCode::Char('/')));
        app.handle_key(key(KeyCode::Char('f')));
        app.handle_key(key(KeyCode::Esc));
        assert!(app.search_input.is_none());
        assert!(app.search.is_none());
    }

    #[test]
    fn search_input_enter_commits_and_jumps_cursor_to_first_match() {
        let mut app = fake_app(vec![make_file(
            "a.rs",
            vec![hunk(
                1,
                vec![
                    diff_line(LineKind::Added, "alpha"),
                    diff_line(LineKind::Added, "beta"),
                    diff_line(LineKind::Added, "gamma"),
                ],
            )],
            100,
        )]);
        // Park the cursor on the first diff row (alpha).
        cursor_on_nth_diff_line(&mut app, 0);

        app.handle_key(key(KeyCode::Char('/')));
        for c in "beta".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        app.handle_key(key(KeyCode::Enter));

        assert!(app.search_input.is_none(), "composer closed on commit");
        let state = app.search.as_ref().expect("search installed");
        assert_eq!(state.matches.len(), 1);
        assert_eq!(state.current, 0);
        // Cursor landed on the "beta" row — not the first diff row.
        let beta_row =
            find_first_row_matching(&app, |r| matches!(r, RowKind::DiffLine { line_idx: 1, .. }));
        assert_eq!(app.scroll, beta_row);
        assert!(!app.follow_mode, "manual jump drops follow mode");
    }

    #[test]
    fn search_input_enter_with_empty_query_does_not_wipe_existing_search() {
        let mut app = fake_app(vec![make_file(
            "a.rs",
            vec![hunk(1, vec![diff_line(LineKind::Added, "alpha")])],
            100,
        )]);
        // Pre-install a fake confirmed search state.
        app.search = Some(SearchState {
            query: "alpha".into(),
            matches: vec![MatchLocation {
                row: 0,
                byte_start: 0,
                byte_end: 5,
            }],
            current: 0,
        });

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
        for c in query.chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        app.handle_key(key(KeyCode::Enter));
    }

    #[test]
    fn search_jump_next_walks_matches_in_order() {
        let mut app = fake_app(vec![make_file(
            "a.rs",
            vec![hunk(
                1,
                vec![
                    diff_line(LineKind::Added, "foo"),
                    diff_line(LineKind::Added, "bar"),
                    diff_line(LineKind::Added, "foo"),
                    diff_line(LineKind::Added, "foo"),
                ],
            )],
            100,
        )]);
        cursor_on_nth_diff_line(&mut app, 0);
        commit_search(&mut app, "foo");

        // After commit, current = 0 (first foo row). Advance twice.
        app.handle_key(key(KeyCode::Char('n')));
        let mid = app.search.as_ref().unwrap().current;
        app.handle_key(key(KeyCode::Char('n')));
        let tail = app.search.as_ref().unwrap().current;
        assert_eq!(mid, 1);
        assert_eq!(tail, 2);
    }

    #[test]
    fn search_jump_next_wraps_around_at_end() {
        let mut app = fake_app(vec![make_file(
            "a.rs",
            vec![hunk(
                1,
                vec![
                    diff_line(LineKind::Added, "foo"),
                    diff_line(LineKind::Added, "foo"),
                ],
            )],
            100,
        )]);
        cursor_on_nth_diff_line(&mut app, 0);
        commit_search(&mut app, "foo");

        // current=0 → n → 1 → n → 0 (wrap)
        app.handle_key(key(KeyCode::Char('n')));
        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(app.search.as_ref().unwrap().current, 0);
    }

    #[test]
    fn search_jump_prev_wraps_around_at_start() {
        let mut app = fake_app(vec![make_file(
            "a.rs",
            vec![hunk(
                1,
                vec![
                    diff_line(LineKind::Added, "foo"),
                    diff_line(LineKind::Added, "foo"),
                    diff_line(LineKind::Added, "foo"),
                ],
            )],
            100,
        )]);
        cursor_on_nth_diff_line(&mut app, 0);
        commit_search(&mut app, "foo");

        // current=0 → N → 2 (wrap to tail)
        app.handle_key(key(KeyCode::Char('N')));
        assert_eq!(app.search.as_ref().unwrap().current, 2);
    }

    #[test]
    fn search_jump_next_is_noop_when_no_search_state() {
        let mut app = fake_app(vec![make_file(
            "a.rs",
            vec![hunk(1, vec![diff_line(LineKind::Added, "foo")])],
            100,
        )]);
        cursor_on_nth_diff_line(&mut app, 0);
        let before = app.scroll;

        app.handle_key(key(KeyCode::Char('n')));

        assert!(app.search.is_none());
        assert_eq!(app.scroll, before, "stray `n` must not move the cursor");
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
        let mut app = scar_app_with_real_fs(
            &tmp,
            "src/bar.rs",
            "a\nb\n",
            2,
            vec![diff_line(LineKind::Added, "b")],
        );
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
        let mut app = scar_app_with_real_fs(
            &tmp,
            "lib.rs",
            "x\n",
            1,
            vec![diff_line(LineKind::Added, "x")],
        );
        let header = app
            .layout
            .rows
            .iter()
            .position(|r| matches!(r, RowKind::FileHeader { .. }))
            .expect("header");
        app.scroll_to(header);

        assert!(app.open_in_editor(Some("vim")).is_none());
    }

    #[test]
    fn open_in_editor_returns_none_when_env_is_empty() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut app = scar_app_with_real_fs(
            &tmp,
            "a.rs",
            "x\n",
            1,
            vec![diff_line(LineKind::Added, "x")],
        );
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
        let file_header_row = app
            .layout
            .rows
            .iter()
            .position(|r| matches!(r, RowKind::FileHeader { .. }))
            .expect("file header exists");
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
        let mut app = fake_app(vec![make_file(
            "a.rs",
            vec![hunk(
                10,
                vec![
                    diff_line(LineKind::Added, "x"),
                    diff_line(LineKind::Deleted, "y"),
                    diff_line(LineKind::Added, "z"),
                ],
            )],
            100,
        )]);
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
        let mut app = fake_app(vec![make_file(
            "a.rs",
            vec![hunk(
                5,
                vec![
                    diff_line(LineKind::Deleted, "gone_a"),
                    diff_line(LineKind::Deleted, "gone_b"),
                    diff_line(LineKind::Deleted, "gone_c"),
                ],
            )],
            100,
        )]);
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
        let del_row = app
            .layout
            .rows
            .iter()
            .position(|r| {
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
                                .map(|l| l.kind == LineKind::Deleted),
                            _ => None,
                        })
                        .unwrap_or(false)
                } else {
                    false
                }
            })
            .expect("a deleted row exists");
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
        let del_row = app
            .layout
            .rows
            .iter()
            .position(|r| {
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
                                .map(|l| l.kind == LineKind::Deleted),
                            _ => None,
                        })
                        .unwrap_or(false)
                } else {
                    false
                }
            })
            .expect("deleted row");
        app.scroll_to(del_row);

        app.handle_key(key(KeyCode::Char('a')));

        let after = std::fs::read_to_string(&abs).expect("read back");
        assert_eq!(
            after, "a\n// @kizu[ask]: explain this change\nd\n",
            "scar on all-deleted hunk must land at the deletion gap"
        );
    }

    // ---- stream mode tests ----

    fn make_stream_event(tool: &str, path: &str, diff: Option<&str>, ts: u64) -> StreamEvent {
        StreamEvent {
            metadata: crate::hook::SanitizedEvent {
                session_id: None,
                hook_event_name: "PostToolUse".into(),
                tool_name: Some(tool.into()),
                file_paths: vec![PathBuf::from(path)],
                cwd: PathBuf::from("/tmp"),
                timestamp_ms: ts,
            },
            diff_snapshot: diff.map(String::from),
        }
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
}
