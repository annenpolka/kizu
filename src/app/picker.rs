use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::App;

/// Modal file picker state. `cursor` indexes into `picker_results()`, not
/// into `App.files` directly.
#[derive(Debug, Clone, Default)]
pub struct PickerState {
    pub query: String,
    pub cursor: usize,
}

impl App {
    pub(crate) fn handle_picker_key(&mut self, key: KeyEvent) {
        // Ctrl-* shortcuts: navigation + cancel. Picker uses fzf-style
        // bindings so any non-control char (including 'j' / 'k') is a
        // filter character.
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('n') | KeyCode::Char('j') => self.move_picker_cursor(1),
                KeyCode::Char('p') | KeyCode::Char('k') => self.move_picker_cursor(-1),
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
            KeyCode::Up => self.move_picker_cursor(-1),
            KeyCode::Down => self.move_picker_cursor(1),
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

    pub(crate) fn picker_selected_path(&self) -> Option<PathBuf> {
        let picker = self.picker.as_ref()?;
        let results = self.picker_results();
        let file_idx = results.get(picker.cursor).copied()?;
        self.files.get(file_idx).map(|f| f.path.clone())
    }

    pub(crate) fn refresh_picker_cursor(&mut self, selected_path: Option<&Path>) {
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

    fn move_picker_cursor(&mut self, delta: isize) {
        let len = self.picker_results().len();
        if len == 0 {
            return;
        }
        if let Some(picker) = self.picker.as_mut() {
            let max = len as isize - 1;
            picker.cursor = (picker.cursor as isize + delta).clamp(0, max) as usize;
        }
    }
}
