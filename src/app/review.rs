use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::git::{self, DiffContent, LineKind};
use crate::scar::{ScarKind, ScarRemove, insert_scar, remove_scar};

use super::{
    App, EditorInvocation, RowKind, TextInputKeyEffect, ViewMode, build_editor_invocation,
    edit_insert_str, handle_text_input_edit, hunk_fingerprint, seen_hunk_fingerprint,
};

#[derive(Debug, Clone)]
pub struct ScarUndoEntry {
    pub path: PathBuf,
    pub line_1indexed: usize,
    pub rendered: String,
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
    pub cursor_pos: usize,
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
    /// Stable hunk identity captured when the confirmation overlay
    /// opened. Used in `confirm_revert` to re-resolve the hunk by
    /// content identity if watcher refreshes reorder or rebuild the
    /// layout while the overlay is visible.
    pub hunk_old_start: usize,
}

impl App {
    /// Drop any pending scar-focus target. Called from navigation
    /// key dispatch so the user's explicit movement "sticks" — the
    /// next watcher-driven recompute won't yank them back to the
    /// scar line they've since moved past.
    pub(crate) fn clear_scar_focus_on_nav(&mut self) {
        self.scar_focus = None;
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
        if self.view_mode == ViewMode::Stream {
            return;
        }
        let Some((path, line)) = self.scar_target_line() else {
            return;
        };
        match insert_scar(&path, line, kind, body) {
            Ok(Some(receipt)) => {
                let focus = Some((path.clone(), receipt.line_1indexed));
                self.scar_undo_stack.push(ScarUndoEntry {
                    path,
                    line_1indexed: receipt.line_1indexed,
                    rendered: receipt.rendered,
                });
                self.refresh_after_scar_write(focus);
            }
            Ok(None) => {}
            Err(err) => {
                self.last_error = Some(format!("scar: {err:#}"));
            }
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
        match insert_scar(&state.target_path, state.target_line, ScarKind::Free, body) {
            Ok(Some(receipt)) => {
                let focus = Some((state.target_path.clone(), receipt.line_1indexed));
                self.scar_undo_stack.push(ScarUndoEntry {
                    path: state.target_path,
                    line_1indexed: receipt.line_1indexed,
                    rendered: receipt.rendered,
                });
                self.refresh_after_scar_write(focus);
            }
            Ok(None) => {}
            Err(err) => {
                self.last_error = Some(format!("scar: {err:#}"));
            }
        }
    }

    fn refresh_after_scar_write(&mut self, focus: Option<(PathBuf, usize)>) {
        self.scar_focus = focus;
        if let Ok(files) = git::compute_diff(&self.root, &self.baseline_sha) {
            self.apply_computed_files(files);
        }
        if let Some(fv) = self.file_view.as_mut() {
            let abs = self.root.join(&fv.path);
            if let Ok(content) = std::fs::read_to_string(&abs) {
                fv.lines = content.lines().map(String::from).collect();
                let max = fv.lines.len().saturating_sub(1);
                if fv.cursor > max {
                    fv.cursor = max;
                }
            }
        }
    }

    pub(crate) fn find_new_file_line_row(&self, abs: &Path, line_1indexed: usize) -> Option<usize> {
        let rel = abs.strip_prefix(&self.root).unwrap_or(abs);
        let file_idx = self.files.iter().position(|f| f.path == rel)?;
        let DiffContent::Text(hunks) = &self.files[file_idx].content else {
            return None;
        };
        for (hunk_idx, hunk) in hunks.iter().enumerate() {
            let mut new_line = hunk.new_start;
            for (offset, dl) in hunk.lines.iter().enumerate() {
                if matches!(dl.kind, LineKind::Deleted) {
                    continue;
                }
                if new_line == line_1indexed {
                    return self.layout.rows.iter().position(|r| {
                        matches!(
                            r,
                            RowKind::DiffLine {
                                file_idx: f,
                                hunk_idx: hi,
                                line_idx: li,
                            } if *f == file_idx && *hi == hunk_idx && *li == offset,
                        )
                    });
                }
                new_line += 1;
            }
        }
        None
    }

    pub(crate) fn scroll_cursor_new_line(&self) -> Option<(PathBuf, usize)> {
        let row = self.layout.rows.get(self.scroll)?;
        let RowKind::DiffLine {
            file_idx,
            hunk_idx,
            line_idx,
        } = *row
        else {
            return None;
        };
        let file = self.files.get(file_idx)?;
        let DiffContent::Text(hunks) = &file.content else {
            return None;
        };
        let hunk = hunks.get(hunk_idx)?;
        let mut new_line = hunk.new_start;
        for (i, dl) in hunk.lines.iter().enumerate() {
            if i == line_idx {
                return Some((self.root.join(&file.path), new_line));
            }
            if !matches!(dl.kind, LineKind::Deleted) {
                new_line += 1;
            }
        }
        None
    }

    pub(crate) fn find_nearest_new_file_line_row(
        &self,
        abs: &Path,
        target_line: usize,
    ) -> Option<usize> {
        let rel = abs.strip_prefix(&self.root).unwrap_or(abs);
        let file_idx = self.files.iter().position(|f| f.path == rel)?;
        let DiffContent::Text(hunks) = &self.files[file_idx].content else {
            return None;
        };
        let mut best: Option<(usize, usize, usize)> = None;
        for (hunk_idx, hunk) in hunks.iter().enumerate() {
            let mut new_line = hunk.new_start;
            for (offset, dl) in hunk.lines.iter().enumerate() {
                if matches!(dl.kind, LineKind::Deleted) {
                    continue;
                }
                let distance = new_line.abs_diff(target_line);
                if best.is_none_or(|(d, _, _)| distance < d) {
                    best = Some((distance, hunk_idx, offset));
                }
                new_line += 1;
            }
        }
        let (_, hunk_idx, line_idx) = best?;
        self.layout.rows.iter().position(|r| {
            matches!(
                r,
                RowKind::DiffLine {
                    file_idx: f,
                    hunk_idx: h,
                    line_idx: l,
                } if *f == file_idx && *h == hunk_idx && *l == line_idx,
            )
        })
    }

    pub fn undo_scar(&mut self) {
        let Some(entry) = self.scar_undo_stack.pop() else {
            return;
        };
        match remove_scar(&entry.path, entry.line_1indexed, &entry.rendered) {
            Ok(ScarRemove::Removed) => {
                self.refresh_after_scar_write(Some((entry.path.clone(), entry.line_1indexed)));
            }
            Ok(ScarRemove::Mismatch) => {
                self.last_error = Some(format!(
                    "undo: line {} in {} was edited — skipped",
                    entry.line_1indexed,
                    entry.path.display(),
                ));
            }
            Ok(ScarRemove::OutOfRange) => {
                self.last_error = Some(format!(
                    "undo: {} has fewer than {} lines — skipped",
                    entry.path.display(),
                    entry.line_1indexed,
                ));
            }
            Err(err) => {
                self.last_error = Some(format!("undo: {err:#}"));
            }
        }
    }

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
        if self.seen_hunks.remove(&key).is_none() {
            let fp = hunk_fingerprint(hunk);
            self.seen_hunks.insert(key, fp);
        }
        let target_hunk = (file_idx, hunk_idx);
        self.build_layout();
        let cursor_hunk = match self.layout.rows.get(self.scroll) {
            Some(RowKind::HunkHeader { file_idx, hunk_idx }) => Some((*file_idx, *hunk_idx)),
            Some(RowKind::DiffLine {
                file_idx, hunk_idx, ..
            }) => Some((*file_idx, *hunk_idx)),
            _ => None,
        };
        if cursor_hunk != Some(target_hunk)
            && let Some(row) = self.layout.rows.iter().position(|r| {
                matches!(
                    r,
                    RowKind::HunkHeader { file_idx: f, hunk_idx: h }
                        if (*f, *h) == target_hunk
                )
            })
        {
            self.scroll_to(row);
        }
    }

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
        let Some(marked_fp) = seen_hunk_fingerprint(&self.seen_hunks, &file.path, hunk.old_start)
        else {
            return false;
        };
        let current_fp = self
            .layout
            .hunk_fingerprints
            .get(file_idx)
            .and_then(|fps| fps.get(hunk_idx))
            .copied()
            .flatten()
            .unwrap_or_else(|| hunk_fingerprint(hunk));
        marked_fp == current_fp
    }

    pub fn open_in_editor(&self, editor_env: Option<&str>) -> Option<EditorInvocation> {
        if self.view_mode == ViewMode::Stream {
            return None;
        }
        let (path, line) = self.scar_target_line()?;
        build_editor_invocation(editor_env, line, &path)
    }

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

    pub fn close_revert_confirm(&mut self) {
        self.revert_confirm = None;
    }

    pub fn confirm_revert(&mut self) {
        let Some(state) = self.revert_confirm.take() else {
            return;
        };
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

    pub(crate) fn handle_revert_confirm_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            self.close_revert_confirm();
            return;
        }
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => self.confirm_revert(),
            _ => self.close_revert_confirm(),
        }
    }

    pub fn handle_paste(&mut self, text: &str) {
        if let Some(state) = self.scar_comment.as_mut() {
            edit_insert_str(&mut state.body, &mut state.cursor_pos, text);
        } else if let Some(state) = self.search_input.as_mut() {
            edit_insert_str(&mut state.query, &mut state.cursor_pos, text);
        }
    }

    pub(crate) fn handle_scar_comment_key(&mut self, key: KeyEvent) {
        let Some(s) = self.scar_comment.as_mut() else {
            return;
        };
        match handle_text_input_edit(key, &mut s.body, &mut s.cursor_pos) {
            TextInputKeyEffect::Continue => {}
            TextInputKeyEffect::Commit => self.commit_scar_comment(),
            TextInputKeyEffect::Cancel => self.close_scar_comment(),
        }
    }

    pub fn scar_target_line(&self) -> Option<(PathBuf, usize)> {
        if let Some(fv) = &self.file_view {
            return Some((self.root.join(&fv.path), fv.cursor + 1));
        }
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

        let Some(line_idx) = diff_line_idx else {
            for (offset, dl) in hunk.lines.iter().enumerate() {
                if !matches!(dl.kind, LineKind::Context) {
                    return Some((self.root.join(&file.path), hunk.new_start + offset));
                }
            }
            return Some((self.root.join(&file.path), hunk.new_start));
        };

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
}
