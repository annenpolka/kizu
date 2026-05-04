use std::cell::Cell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::style::Color;

use crate::git::{DiffContent, LineKind};

use super::{
    App, KeyEffect, SCROLL_ANIM_DURATION, ScrollAnim, VisualIndex, build_editor_invocation,
    control_page_delta, is_quit_key,
};

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
    pub path: PathBuf,
    pub return_scroll: usize,
    pub content: String,
    pub lines: Vec<String>,
    pub line_bg: HashMap<usize, Color>,
    pub cursor: usize,
    /// Intra-line visual offset of the cursor. **Always 0 in nowrap
    /// mode.** In wrap mode this is how many wrapped visual rows into
    /// `lines[cursor]` the user has walked via `J` / `K` / Ctrl-d /
    /// Ctrl-u.
    pub cursor_sub_row: usize,
    /// Top of the file-view viewport in **visual-row** coordinates.
    /// In nowrap mode this is identical to the top logical line
    /// index; in wrap mode it can land in the middle of a long line.
    pub scroll_top: usize,
    /// Easing tween for the file view's scroll_top, matching the
    /// main diff view's 150ms ease-out cubic animation.
    pub anim: Option<ScrollAnim>,
    /// Last rendered scroll position (in row units). Used as the
    /// tween's start point when a new animation begins.
    pub visual_top: f32,
    /// Last rendered file-view body width in cells (left gutter
    /// excluded). The wrap toggle and visual-row navigation reuse it
    /// so key handling can stay in sync with the renderer.
    pub last_body_width: Cell<usize>,
    /// v0.5 M2: whether the on-disk file ends with an LF. When false
    /// the renderer draws a Yellow `∅` at the end of the last line
    /// so the user sees the git `\ No newline at end of file` signal.
    /// Determined from the raw file bytes at `open_file_view` time so
    /// `lines: Vec<String>` (which discards the terminal delimiter)
    /// still carries the information.
    pub last_line_has_trailing_newline: bool,
}

fn scroll_top_to_keep_visible(scroll_top: usize, cursor_y: usize, viewport_height: usize) -> usize {
    let viewport_height = viewport_height.max(1);
    if cursor_y < scroll_top {
        cursor_y
    } else if cursor_y >= scroll_top + viewport_height {
        cursor_y.saturating_sub(viewport_height - 1)
    } else {
        scroll_top
    }
}

fn update_file_view_scroll_anim(fv: &mut FileViewState, old_top: usize, animate: bool) {
    if animate && fv.scroll_top != old_top {
        fv.anim = Some(ScrollAnim {
            from: fv.visual_top,
            start: Instant::now(),
            dur: SCROLL_ANIM_DURATION,
        });
    } else if !animate {
        fv.anim = None;
        fv.visual_top = fv.scroll_top as f32;
    }
}

impl App {
    /// Open the full-file zoom view for the cursor's current hunk.
    /// Reads the worktree file, builds a line_bg map from diff
    /// hunks, and parks the viewport so the **cursor's current
    /// new-file line** is visible (not the hunk header). That way
    /// zooming into a hunk keeps the reader on whatever row they
    /// were already inspecting instead of snapping back to the top
    /// of the hunk. No-op when the cursor is not on a text hunk,
    /// or the file cannot be read.
    pub fn open_file_view(&mut self) {
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
        // v0.5 M2: `content.lines()` discards the trailing delimiter,
        // so interrogate the raw string to decide whether to draw the
        // EOF-no-newline marker. Empty files are treated as
        // "no newline" only if they are non-empty without trailing LF;
        // a literally empty file has no last line to mark.
        let last_line_has_trailing_newline = content.is_empty() || content.ends_with('\n');

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

        // Inherit the cursor's current new-file line instead of
        // snapping to the hunk header: `scar_target_line` already
        // does this mapping (DiffLine → new-file line, HunkHeader →
        // first changed line). Fall back to the hunk's `new_start`
        // when the cursor is on a row with no mapping, e.g. a file
        // header.
        let target_1indexed = self
            .scar_target_line()
            .map(|(_, line)| line)
            .or_else(|| {
                self.current_hunk()
                    .and_then(|(_, hi)| hunks.get(hi))
                    .map(|h| h.new_start)
            })
            .unwrap_or(1);
        let initial_cursor = target_1indexed
            .saturating_sub(1)
            .min(lines.len().saturating_sub(1));

        let guessed_body_width = self.last_body_width.get().unwrap_or(1).max(1);
        let scroll_top = if self.wrap_lines {
            let vi = VisualIndex::build_lines(&lines, Some(guessed_body_width));
            vi.visual_y(initial_cursor)
                .saturating_sub(self.last_body_height.get() / 2)
        } else {
            initial_cursor.saturating_sub(self.last_body_height.get() / 2)
        };
        self.file_view = Some(FileViewState {
            path: file.path.clone(),
            return_scroll: self.scroll,
            content,
            lines,
            line_bg,
            cursor: initial_cursor,
            cursor_sub_row: 0,
            scroll_top,
            anim: None,
            visual_top: scroll_top as f32,
            last_body_width: Cell::new(guessed_body_width),
            last_line_has_trailing_newline,
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
    pub(crate) fn handle_file_view_key(&mut self, key: KeyEvent) -> KeyEffect {
        if is_quit_key(key) {
            self.should_quit = true;
            return KeyEffect::None;
        }
        // Same sticky-focus discipline as normal mode: every keypress
        // drops the scar-focus pin; scar action keys re-establish it.
        self.clear_scar_focus_on_nav();
        if let Some(delta) = control_page_delta(key) {
            self.file_view_scroll_by(delta, true);
            return KeyEffect::None;
        }
        match key.code {
            KeyCode::Enter | KeyCode::Esc => self.close_file_view(),
            // j/k: chunk scroll (viewport/3), matching normal-mode
            // adaptive-motion feel. J/K: exact 1-row move.
            KeyCode::Char('j') | KeyCode::Down => {
                let chunk = self.chunk_size() as isize;
                self.file_view_scroll_by(chunk, true);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                let chunk = self.chunk_size() as isize;
                self.file_view_scroll_by(-chunk, true);
            }
            KeyCode::Char('J') => {
                self.file_view_scroll_by(1, false);
            }
            KeyCode::Char('K') => {
                self.file_view_scroll_by(-1, false);
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
            // Scar operations reuse the diff-view handlers, which
            // already consult `scar_target_line()` — and that function
            // is now file-view aware. Config bindings apply (so a user
            // who remaps `ask` to `A` gets the new key here too).
            KeyCode::Char(ch) => {
                self.handle_common_action_key(ch);
            }
            _ => {}
        }
        KeyEffect::None
    }

    /// Re-center the file view around the current cursor after any
    /// change that invalidates visual-row coordinates — wrap toggle
    /// flips the coordinate system between logical and visual rows;
    /// line-number toggle / Stream-mode switch change `body_width`
    /// and therefore the visual index. Drop any stale intra-line
    /// offset and clear the animation so the next frame snaps cleanly
    /// onto the new scale.
    pub(crate) fn reflow_file_view(&mut self) {
        let viewport_height = self.last_body_height.get().max(1);
        let wrap_lines = self.wrap_lines;
        let Some(fv) = self.file_view.as_mut() else {
            return;
        };
        fv.cursor_sub_row = 0;
        let body_width = wrap_lines.then_some(fv.last_body_width.get().max(1));
        let vi = VisualIndex::build_lines(&fv.lines, body_width);
        let cursor_y = vi.visual_y(fv.cursor);
        let max_top = vi.total_visual().saturating_sub(1);
        fv.scroll_top = cursor_y.saturating_sub(viewport_height / 2).min(max_top);
        fv.anim = None;
        fv.visual_top = fv.scroll_top as f32;
    }

    fn file_view_scroll_by(&mut self, delta: isize, animate: bool) {
        let viewport_height = self.last_body_height.get().max(1);
        let wrap_lines = self.wrap_lines;
        let Some(fv) = self.file_view.as_mut() else {
            return;
        };
        if wrap_lines {
            let vi = VisualIndex::build_lines(&fv.lines, Some(fv.last_body_width.get().max(1)));
            let cur_y = vi.visual_y(fv.cursor) + fv.cursor_sub_row;
            let new_y = (cur_y as isize + delta).max(0) as usize;
            let clamped = new_y.min(vi.total_visual().saturating_sub(1));
            let (new_cursor, new_sub) = vi.logical_at(clamped);
            fv.cursor = new_cursor;
            fv.cursor_sub_row = new_sub;
            let old_top = fv.scroll_top;
            fv.scroll_top = scroll_top_to_keep_visible(fv.scroll_top, clamped, viewport_height);
            update_file_view_scroll_anim(fv, old_top, animate);
            return;
        }

        let max = fv.lines.len().saturating_sub(1);
        let new = (fv.cursor as isize + delta).clamp(0, max as isize) as usize;
        fv.cursor = new;
        fv.cursor_sub_row = 0;
        let old_top = fv.scroll_top;
        fv.scroll_top = scroll_top_to_keep_visible(fv.scroll_top, fv.cursor, viewport_height);
        update_file_view_scroll_anim(fv, old_top, animate);
    }

    /// Advance the file-view scroll animation by one frame.
    /// Updates `visual_top` and clears `anim` when the tween finishes.
    pub fn tick_file_view_anim(&mut self) {
        let Some(fv) = self.file_view.as_mut() else {
            return;
        };
        let Some(anim) = &fv.anim else {
            return;
        };
        let (v, done) = anim.sample(fv.scroll_top as f32, Instant::now());
        fv.visual_top = v;
        if done {
            fv.anim = None;
        }
    }

    fn file_view_goto(&mut self, line: usize) {
        let viewport_height = self.last_body_height.get().max(1);
        let wrap_lines = self.wrap_lines;
        let Some(fv) = self.file_view.as_mut() else {
            return;
        };
        let max = fv.lines.len().saturating_sub(1);
        fv.cursor = line.min(max);
        fv.cursor_sub_row = 0;
        let cursor_y = if wrap_lines {
            let vi = VisualIndex::build_lines(&fv.lines, Some(fv.last_body_width.get().max(1)));
            vi.visual_y(fv.cursor)
        } else {
            fv.cursor
        };
        fv.scroll_top = scroll_top_to_keep_visible(fv.scroll_top, cursor_y, viewport_height);
        // g/G are instant jumps — no animation.
        fv.anim = None;
        fv.visual_top = fv.scroll_top as f32;
    }
}
