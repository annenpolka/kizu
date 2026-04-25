use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::scar::ScarKind;

use super::{App, KeyEffect, SCAR_TEXT_ASK, SCAR_TEXT_REJECT, control_page_delta, is_quit_key};

impl App {
    /// Top-level key dispatch. Picker mode shadows the normal bindings.
    /// Returns a [`KeyEffect`] describing any post-dispatch work that
    /// the event loop must perform — currently only `R` can trigger
    /// a watcher reconfigure, but the same channel scales to future
    /// side-effects without threading explicit parameters through
    /// every handler.
    pub fn handle_key(&mut self, key: KeyEvent) -> KeyEffect {
        if self.help_overlay {
            self.handle_help_key(key);
            KeyEffect::None
        } else if self.picker.is_some() {
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

    fn handle_normal_key(&mut self, key: KeyEvent) -> KeyEffect {
        // Quit shortcuts.
        if is_quit_key(key) {
            self.should_quit = true;
            return KeyEffect::None;
        }

        // Any user keypress drops the sticky scar-focus target. Scar
        // actions (`a`/`r`/`c`/`u`) re-establish the focus later in
        // this frame via `refresh_after_scar_write`, so clearing here
        // costs nothing for them but protects every other key from
        // leaving the cursor pinned to the last scar.
        self.clear_scar_focus_on_nav();

        if let Some(delta) = control_page_delta(key) {
            self.scroll_by(delta);
            self.follow_mode = false;
            return KeyEffect::None;
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
                if self.handle_common_action_key(ch) {
                    return KeyEffect::None;
                }
                let k = &self.config.keys;
                if ch == k.follow {
                    self.follow_restore();
                } else if ch == k.picker {
                    self.open_picker();
                } else if ch == k.revert {
                    self.open_revert_confirm();
                } else if ch == k.seen {
                    self.toggle_seen_current_hunk();
                } else if ch == k.search {
                    self.open_search_input();
                } else if ch == k.search_next {
                    self.search_jump_by(1);
                } else if ch == k.search_prev {
                    self.search_jump_by(-1);
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
                }
            }
            _ => {}
        }
        KeyEffect::None
    }

    pub(crate) fn handle_common_action_key(&mut self, ch: char) -> bool {
        if ch == '?' {
            self.help_overlay = true;
        } else if ch == self.config.keys.ask {
            self.insert_canned_scar(ScarKind::Ask, SCAR_TEXT_ASK);
        } else if ch == self.config.keys.reject {
            self.insert_canned_scar(ScarKind::Reject, SCAR_TEXT_REJECT);
        } else if ch == self.config.keys.comment {
            self.open_scar_comment();
        } else if ch == self.config.keys.wrap_toggle {
            self.toggle_wrap_lines();
        } else if ch == self.config.keys.line_numbers_toggle {
            self.toggle_line_numbers();
        } else if ch == self.config.keys.undo {
            self.undo_scar();
        } else {
            return false;
        }
        true
    }

    fn handle_help_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => {
                self.help_overlay = false;
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            _ => {}
        }
    }
}
