use crossterm::event::KeyEvent;

use crate::git::{DiffContent, FileDiff};

use super::{App, RowKind, ScrollLayout, TextInputKeyEffect, handle_text_input_edit};

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

fn find_ascii_case_insensitive(haystack: &str, needle: &str, start: usize) -> Option<usize> {
    let haystack = haystack.as_bytes();
    let needle = needle.as_bytes();
    if needle.is_empty() || start > haystack.len() || needle.len() > haystack.len() - start {
        return None;
    }
    let last_start = haystack.len() - needle.len();
    (start..=last_start).find(|&idx| haystack[idx..idx + needle.len()].eq_ignore_ascii_case(needle))
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
        let mut start = 0;
        if !case_sensitive && needle.is_ascii() && line.content.is_ascii() {
            while let Some(byte_start) = find_ascii_case_insensitive(&line.content, &needle, start)
            {
                let byte_end = byte_start + needle.len();
                out.push(MatchLocation {
                    row: row_idx,
                    byte_start,
                    byte_end,
                });
                start = byte_end;
            }
            continue;
        }

        let haystack = line.content.as_str();
        let search_needle = if case_sensitive {
            query
        } else {
            needle.as_str()
        };
        while let Some(idx) = haystack[start..].find(search_needle) {
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

impl App {
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
    /// jump the cursor to the first match **after the current cursor
    /// position** (vim-style). Wraps around to the global first match
    /// when every hit is before the cursor, so the press always lands
    /// somewhere as long as matches exist. `N` steps backward from there.
    ///
    /// Empty queries close the composer without touching confirmed
    /// state so a stray `/` + `Enter` does not wipe an existing search.
    pub fn commit_search_input(&mut self) {
        let Some(input) = self.search_input.take() else {
            return;
        };
        let query = input.query;
        if query.is_empty() {
            return;
        }
        let matches = find_matches(&self.layout, &self.files, &query);
        let cursor_row = self.scroll;
        // Pick the first match whose row is strictly after the cursor.
        // Falling back to index 0 gives wrap-around when the cursor
        // sits past the last match.
        let current = matches.iter().position(|m| m.row > cursor_row).unwrap_or(0);
        let target_row = matches.get(current).map(|m| m.row);
        self.search = Some(SearchState {
            query,
            matches,
            current,
        });
        if let Some(row) = target_row {
            self.follow_mode = false;
            self.scroll_to(row);
        }
    }

    /// `/`-composer keystroke handler. Typing appends, Backspace
    /// deletes, Enter commits, Esc cancels. Ctrl-C also cancels
    /// (matches the other modal overlays).
    pub(crate) fn handle_search_input_key(&mut self, key: KeyEvent) {
        let Some(s) = self.search_input.as_mut() else {
            return;
        };
        match handle_text_input_edit(key, &mut s.query, &mut s.cursor_pos) {
            TextInputKeyEffect::Continue => {}
            TextInputKeyEffect::Commit => self.commit_search_input(),
            TextInputKeyEffect::Cancel => self.close_search_input(),
        }
    }

    pub(crate) fn search_jump_by(&mut self, delta: isize) {
        let Some(state) = self.search.as_mut() else {
            return;
        };
        if state.matches.is_empty() {
            return;
        }
        let len = state.matches.len() as isize;
        state.current = (state.current as isize + delta).rem_euclid(len) as usize;
        let row = state.matches[state.current].row;
        self.follow_mode = false;
        self.scroll_to(row);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_case_insensitive_find_reports_original_byte_offsets() {
        let haystack = "Hello WORLD world";
        assert_eq!(find_ascii_case_insensitive(haystack, "world", 0), Some(6));
        assert_eq!(find_ascii_case_insensitive(haystack, "world", 7), Some(12));
        assert_eq!(find_ascii_case_insensitive(haystack, "world", 13), None);
    }
}
