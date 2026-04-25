use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

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
pub(crate) fn edit_insert_str(text: &mut String, cursor_pos: &mut usize, s: &str) {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextInputKeyEffect {
    Continue,
    Commit,
    Cancel,
}

pub(crate) fn handle_text_input_edit(
    key: KeyEvent,
    text: &mut String,
    cursor_pos: &mut usize,
) -> TextInputKeyEffect {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('c') => return TextInputKeyEffect::Cancel,
            KeyCode::Char('a') => *cursor_pos = 0,
            KeyCode::Char('e') => *cursor_pos = text.chars().count(),
            _ => {}
        }
        return TextInputKeyEffect::Continue;
    }
    match key.code {
        KeyCode::Esc => TextInputKeyEffect::Cancel,
        KeyCode::Enter => TextInputKeyEffect::Commit,
        KeyCode::Backspace => {
            edit_backspace(text, cursor_pos);
            TextInputKeyEffect::Continue
        }
        KeyCode::Left => {
            *cursor_pos = cursor_pos.saturating_sub(1);
            TextInputKeyEffect::Continue
        }
        KeyCode::Right => {
            *cursor_pos = (*cursor_pos + 1).min(text.chars().count());
            TextInputKeyEffect::Continue
        }
        KeyCode::Home => {
            *cursor_pos = 0;
            TextInputKeyEffect::Continue
        }
        KeyCode::End => {
            *cursor_pos = text.chars().count();
            TextInputKeyEffect::Continue
        }
        KeyCode::Char(c) => {
            edit_insert_char(text, cursor_pos, c);
            TextInputKeyEffect::Continue
        }
        _ => TextInputKeyEffect::Continue,
    }
}
