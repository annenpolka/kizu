//! Minimal interactive prompt for `kizu init` (single-select and multi-select).
//!
//! Built directly on `crossterm` + `unicode-width`; intentionally does not
//! depend on `dialoguer`. See ADR-0019 for the rationale.
//!
//! The rendering layer is split into pure functions (`render_select_frame`,
//! `render_multi_frame`, `apply_select_key`, `apply_multi_key`) that take a
//! state and produce a frame / next outcome. The I/O layer (`run_select_one`,
//! `run_multi_select`) drives those pure functions over a `crossterm` event
//! loop with a `RawModeGuard` RAII wrapper.
//!
//! Contract: items may contain ANSI SGR escapes. Visible width is computed by
//! stripping CSI sequences and applying `unicode-width`. Lines never wrap —
//! overflow is truncated with `…` at the cell boundary.

use anyhow::{Context, Result, bail};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    style::Print,
    terminal::{Clear, ClearType, disable_raw_mode, enable_raw_mode},
};
use std::io::{self, IsTerminal, Write};
use unicode_width::UnicodeWidthStr;

// ── Pure: width / truncation ────────────────────────────────────────

/// Strip ANSI CSI sequences (ESC '[' ... final-byte in 0x40..=0x7e) from
/// a string. Lone ESC bytes without `[` are dropped. All other characters
/// are preserved verbatim so multibyte (e.g. CJK) content survives.
pub(crate) fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if let Some(&'[') = chars.peek() {
                chars.next();
                for c2 in chars.by_ref() {
                    if (0x40..=0x7e).contains(&(c2 as u32)) {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// Visible cell width of a string, ignoring ANSI SGR escapes.
pub(crate) fn visible_width(s: &str) -> usize {
    UnicodeWidthStr::width(strip_ansi(s).as_str())
}

/// Truncate `s` to fit within `max` display cells.
///
/// If the visible width of `s` exceeds `max`, keep as many leading cells as
/// possible and append `…` (1 cell). ANSI escapes have zero width and are
/// preserved through the cut. A closing `\x1b[0m` is appended after `…` if
/// the original string contained any ANSI sequence, so colors don't bleed.
pub(crate) fn truncate_to_width(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if visible_width(s) <= max {
        return s.to_string();
    }
    // Walk characters; copy ANSI escapes through untouched; stop collecting
    // visible cells when budget - 1 is reached (leave room for the ellipsis).
    let budget = max.saturating_sub(1);
    let mut out = String::new();
    let mut used: usize = 0;
    let mut saw_ansi = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if let Some(&'[') = chars.peek() {
                saw_ansi = true;
                out.push(c);
                out.push(chars.next().unwrap()); // '['
                for c2 in chars.by_ref() {
                    out.push(c2);
                    let cp = c2 as u32;
                    if (0x40..=0x7e).contains(&cp) {
                        break;
                    }
                }
                continue;
            }
            continue; // lone ESC: drop
        }
        let w = UnicodeWidthStr::width(c.to_string().as_str());
        if used + w > budget {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    if saw_ansi {
        out.push_str("\x1b[0m");
    }
    out
}

// ── Pure: key event mapping ─────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromptKey {
    Up,
    Down,
    Home,
    End,
    Toggle,
    Confirm,
    Cancel,
    Other,
}

pub(crate) fn map_key(ev: KeyEvent) -> PromptKey {
    // Ignore key release / repeat; only act on Press (or KeyEventKind::Press
    // default on non-Windows where kind is always Press).
    if ev.kind == KeyEventKind::Release {
        return PromptKey::Other;
    }
    if ev.modifiers.contains(KeyModifiers::CONTROL) {
        return match ev.code {
            KeyCode::Char('c') | KeyCode::Char('C') | KeyCode::Char('d') | KeyCode::Char('D') => {
                PromptKey::Cancel
            }
            KeyCode::Char('n') | KeyCode::Char('N') => PromptKey::Down,
            KeyCode::Char('p') | KeyCode::Char('P') => PromptKey::Up,
            _ => PromptKey::Other,
        };
    }
    match ev.code {
        KeyCode::Up | KeyCode::Char('k') => PromptKey::Up,
        KeyCode::Down | KeyCode::Char('j') => PromptKey::Down,
        KeyCode::Home | KeyCode::Char('g') => PromptKey::Home,
        KeyCode::End | KeyCode::Char('G') => PromptKey::End,
        KeyCode::Char(' ') => PromptKey::Toggle,
        KeyCode::Enter => PromptKey::Confirm,
        KeyCode::Esc | KeyCode::Char('q') => PromptKey::Cancel,
        _ => PromptKey::Other,
    }
}

// ── Pure: state + apply ─────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    Continue,
    Confirm,
    Cancel,
}

pub(crate) struct SelectState<'a> {
    pub prompt: &'a str,
    pub items: &'a [&'a str],
    pub cursor: usize,
}

impl<'a> SelectState<'a> {
    pub(crate) fn new(prompt: &'a str, items: &'a [&'a str], default: usize) -> Self {
        let cursor = default.min(items.len().saturating_sub(1));
        Self {
            prompt,
            items,
            cursor,
        }
    }
}

pub(crate) fn apply_select_key(state: &mut SelectState, key: PromptKey) -> Outcome {
    let n = state.items.len();
    if n == 0 {
        return Outcome::Cancel;
    }
    match key {
        PromptKey::Up => {
            state.cursor = if state.cursor == 0 {
                n - 1
            } else {
                state.cursor - 1
            };
            Outcome::Continue
        }
        PromptKey::Down => {
            state.cursor = (state.cursor + 1) % n;
            Outcome::Continue
        }
        PromptKey::Home => {
            state.cursor = 0;
            Outcome::Continue
        }
        PromptKey::End => {
            state.cursor = n - 1;
            Outcome::Continue
        }
        PromptKey::Confirm => Outcome::Confirm,
        PromptKey::Cancel => Outcome::Cancel,
        PromptKey::Toggle | PromptKey::Other => Outcome::Continue,
    }
}

pub(crate) struct MultiSelectState<'a> {
    pub prompt: &'a str,
    pub items: &'a [&'a str],
    pub cursor: usize,
    pub checked: Vec<bool>,
}

impl<'a> MultiSelectState<'a> {
    pub(crate) fn new(prompt: &'a str, items: &'a [&'a str], defaults: &[bool]) -> Self {
        let mut checked = vec![false; items.len()];
        for (i, &d) in defaults.iter().enumerate().take(items.len()) {
            checked[i] = d;
        }
        Self {
            prompt,
            items,
            cursor: 0,
            checked,
        }
    }
}

pub(crate) fn apply_multi_key(state: &mut MultiSelectState, key: PromptKey) -> Outcome {
    let n = state.items.len();
    if n == 0 {
        return Outcome::Cancel;
    }
    match key {
        PromptKey::Up => {
            state.cursor = if state.cursor == 0 {
                n - 1
            } else {
                state.cursor - 1
            };
            Outcome::Continue
        }
        PromptKey::Down => {
            state.cursor = (state.cursor + 1) % n;
            Outcome::Continue
        }
        PromptKey::Home => {
            state.cursor = 0;
            Outcome::Continue
        }
        PromptKey::End => {
            state.cursor = n - 1;
            Outcome::Continue
        }
        PromptKey::Toggle => {
            state.checked[state.cursor] = !state.checked[state.cursor];
            Outcome::Continue
        }
        PromptKey::Confirm => Outcome::Confirm,
        PromptKey::Cancel => Outcome::Cancel,
        PromptKey::Other => Outcome::Continue,
    }
}

// ── Pure: rendering ─────────────────────────────────────────────────

/// Render a select (single-choice) frame. Returns one `String` per line.
/// Each returned line fits within `term_width` display cells.
pub(crate) fn render_select_frame(state: &SelectState, term_width: usize) -> Vec<String> {
    let mut out = Vec::with_capacity(state.items.len() + 2);
    out.push(truncate_to_width(
        &format!("\x1b[36m?\x1b[0m \x1b[1m{}\x1b[0m", state.prompt),
        term_width,
    ));
    for (i, item) in state.items.iter().enumerate() {
        let marker = if i == state.cursor {
            "\x1b[36m>\x1b[0m"
        } else {
            " "
        };
        // Marker is 1 cell, space is 1 cell → 2-cell prefix.
        let budget = term_width.saturating_sub(2);
        let body = truncate_to_width(item, budget);
        let line = if i == state.cursor {
            // Bold the selected item's body.
            format!("{} \x1b[1m{}\x1b[0m", marker, body)
        } else {
            format!("{} {}", marker, body)
        };
        out.push(line);
    }
    out.push(truncate_to_width(
        "\x1b[2m  (j/k or ↑/↓ to move, enter to confirm, esc to cancel)\x1b[0m",
        term_width,
    ));
    out
}

/// Render a multi-select frame. Each line fits within `term_width`.
///
/// Layout per row:
///   `{cursor_mark} {checkbox} {body}`
///
/// Cursor mark, checkbox, and separators are all 1 cell (ballot-box
/// checkboxes ☐/☑ are EAW=N). Prefix = 1 + 1 + 1 + 1 = 4 cells.
pub(crate) fn render_multi_frame(state: &MultiSelectState, term_width: usize) -> Vec<String> {
    let mut out = Vec::with_capacity(state.items.len() + 2);
    out.push(truncate_to_width(
        &format!("\x1b[36m?\x1b[0m \x1b[1m{}\x1b[0m", state.prompt),
        term_width,
    ));
    for (i, item) in state.items.iter().enumerate() {
        let cursor_mark = if i == state.cursor { ">" } else { " " };
        let check = if state.checked[i] {
            "\x1b[32m☑\x1b[0m"
        } else {
            "☐"
        };
        let prefix_cells = 4;
        let budget = term_width.saturating_sub(prefix_cells);
        let body = truncate_to_width(item, budget);
        let cursor_colored = if i == state.cursor {
            format!("\x1b[36m{}\x1b[0m", cursor_mark)
        } else {
            cursor_mark.to_string()
        };
        out.push(format!("{} {} {}", cursor_colored, check, body));
    }
    out.push(truncate_to_width(
        "\x1b[2m  (j/k to move, space to toggle, enter to confirm, esc to cancel)\x1b[0m",
        term_width,
    ));
    out
}

// ── I/O layer ───────────────────────────────────────────────────────

/// RAII guard that disables raw mode on drop.
struct RawModeGuard;

impl RawModeGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

fn term_width() -> usize {
    crossterm::terminal::size()
        .map(|(c, _)| c as usize)
        .unwrap_or(80)
}

fn ensure_tty() -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!(
            "interactive prompt requires a TTY on stdin/stdout; \
             pass --non-interactive and the relevant flags for scripted use"
        );
    }
    Ok(())
}

fn write_frame(out: &mut impl Write, lines: &[String]) -> io::Result<()> {
    for (i, line) in lines.iter().enumerate() {
        // Move to column 0 for each line, write content, clear to EOL (so
        // stale characters from previous longer lines don't linger), then
        // newline (except last, to avoid an extra scroll).
        execute!(
            out,
            cursor::MoveToColumn(0),
            Print(line),
            Clear(ClearType::UntilNewLine)
        )?;
        if i + 1 < lines.len() {
            writeln!(out)?;
        }
    }
    out.flush()
}

fn clear_frame(out: &mut impl Write, height: usize) -> io::Result<()> {
    if height == 0 {
        return Ok(());
    }
    // Cursor is on the last rendered line. Move to its start, clear down.
    execute!(
        out,
        cursor::MoveToColumn(0),
        cursor::MoveUp((height - 1) as u16),
        Clear(ClearType::FromCursorDown)
    )
}

/// Single-choice interactive prompt. `Ok(None)` = user cancelled.
pub fn run_select_one(prompt: &str, items: &[&str], default: usize) -> Result<Option<usize>> {
    if items.is_empty() {
        bail!("run_select_one requires at least one item");
    }
    ensure_tty()?;
    let mut state = SelectState::new(prompt, items, default);
    let _guard = RawModeGuard::enter().context("failed to enter raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, cursor::Hide).ok();

    let mut prev_height = 0usize;
    let result = loop {
        let lines = render_select_frame(&state, term_width());
        if prev_height > 0 {
            clear_frame(&mut stdout, prev_height).ok();
        }
        write_frame(&mut stdout, &lines).ok();
        prev_height = lines.len();

        match event::read().context("failed to read key event")? {
            Event::Key(key) => match apply_select_key(&mut state, map_key(key)) {
                Outcome::Continue => continue,
                Outcome::Confirm => break Some(state.cursor),
                Outcome::Cancel => break None,
            },
            Event::Resize(_, _) => continue, // next iteration redraws
            _ => continue,
        }
    };

    clear_frame(&mut stdout, prev_height).ok();
    execute!(stdout, cursor::Show).ok();
    Ok(result)
}

/// Multi-choice interactive prompt. `Ok(None)` = user cancelled.
pub fn run_multi_select(
    prompt: &str,
    items: &[&str],
    defaults: &[bool],
) -> Result<Option<Vec<usize>>> {
    if items.is_empty() {
        bail!("run_multi_select requires at least one item");
    }
    ensure_tty()?;
    let mut state = MultiSelectState::new(prompt, items, defaults);
    let _guard = RawModeGuard::enter().context("failed to enter raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, cursor::Hide).ok();

    let mut prev_height = 0usize;
    let result = loop {
        let lines = render_multi_frame(&state, term_width());
        if prev_height > 0 {
            clear_frame(&mut stdout, prev_height).ok();
        }
        write_frame(&mut stdout, &lines).ok();
        prev_height = lines.len();

        match event::read().context("failed to read key event")? {
            Event::Key(key) => match apply_multi_key(&mut state, map_key(key)) {
                Outcome::Continue => continue,
                Outcome::Confirm => {
                    break Some(
                        state
                            .checked
                            .iter()
                            .enumerate()
                            .filter_map(|(i, &c)| if c { Some(i) } else { None })
                            .collect(),
                    );
                }
                Outcome::Cancel => break None,
            },
            Event::Resize(_, _) => continue,
            _ => continue,
        }
    };

    clear_frame(&mut stdout, prev_height).ok();
    execute!(stdout, cursor::Show).ok();
    Ok(result)
}

// ── Tests (pure layer only; I/O layer exercised via e2e) ────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    // ── width / truncation ──────────────────────────────────────────

    #[test]
    fn visible_width_counts_ascii() {
        assert_eq!(visible_width("abc"), 3);
        assert_eq!(visible_width(""), 0);
    }

    #[test]
    fn visible_width_ignores_ansi_sgr() {
        assert_eq!(visible_width("\x1b[32mgreen\x1b[0m"), 5);
        assert_eq!(visible_width("\x1b[1;36m?\x1b[0m foo"), 5);
    }

    #[test]
    fn visible_width_handles_cjk_as_two_cells() {
        assert_eq!(visible_width("漢字"), 4);
        assert_eq!(visible_width("\x1b[31m傷\x1b[0m"), 2);
    }

    #[test]
    fn checkbox_ballot_pair_is_one_cell_each() {
        // ☐ (U+2610) and ☑ (U+2611) are EAW=N per Unicode → 1 cell each
        // per `unicode-width`. We pick these (over emoji like ⬜/✅ that
        // render as 2-cell wide) to keep the prompt tight and render
        // predictably in every monospace terminal.
        assert_eq!(UnicodeWidthStr::width("☐"), 1);
        assert_eq!(UnicodeWidthStr::width("☑"), 1);
    }

    #[test]
    fn truncate_to_width_passthrough_when_fits() {
        assert_eq!(truncate_to_width("hello", 10), "hello");
        assert_eq!(truncate_to_width("hello", 5), "hello");
    }

    #[test]
    fn truncate_to_width_adds_ellipsis_when_overflow() {
        assert_eq!(truncate_to_width("abcdef", 4), "abc…");
    }

    #[test]
    fn truncate_to_width_preserves_ansi_and_resets() {
        let out = truncate_to_width("\x1b[32mabcdef\x1b[0m", 4);
        // Visible: "abc…" = 4 cells. ANSI seen, so trailing reset is appended.
        assert_eq!(out, "\x1b[32mabc…\x1b[0m");
        assert_eq!(visible_width(&out), 4);
    }

    // ── key mapping ─────────────────────────────────────────────────

    #[test]
    fn map_key_arrow_and_vim_nav() {
        assert_eq!(map_key(key(KeyCode::Up)), PromptKey::Up);
        assert_eq!(map_key(key(KeyCode::Down)), PromptKey::Down);
        assert_eq!(map_key(key(KeyCode::Char('k'))), PromptKey::Up);
        assert_eq!(map_key(key(KeyCode::Char('j'))), PromptKey::Down);
    }

    #[test]
    fn map_key_space_toggle_enter_confirm() {
        assert_eq!(map_key(key(KeyCode::Char(' '))), PromptKey::Toggle);
        assert_eq!(map_key(key(KeyCode::Enter)), PromptKey::Confirm);
    }

    #[test]
    fn map_key_esc_q_ctrl_c_cancel() {
        assert_eq!(map_key(key(KeyCode::Esc)), PromptKey::Cancel);
        assert_eq!(map_key(key(KeyCode::Char('q'))), PromptKey::Cancel);
        assert_eq!(map_key(ctrl(KeyCode::Char('c'))), PromptKey::Cancel);
        assert_eq!(map_key(ctrl(KeyCode::Char('d'))), PromptKey::Cancel);
    }

    #[test]
    fn map_key_ignores_release() {
        let mut ev = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        ev.kind = KeyEventKind::Release;
        assert_eq!(map_key(ev), PromptKey::Other);
    }

    // ── SelectState ─────────────────────────────────────────────────

    #[test]
    fn select_default_clamps_into_range() {
        let items = ["a", "b", "c"];
        let items_slice: Vec<&str> = items.to_vec();
        let state = SelectState::new("pick", &items_slice, 99);
        assert_eq!(state.cursor, 2);
    }

    #[test]
    fn apply_select_key_down_wraps() {
        let items = ["a", "b", "c"];
        let items_slice: Vec<&str> = items.to_vec();
        let mut state = SelectState::new("pick", &items_slice, 2);
        apply_select_key(&mut state, PromptKey::Down);
        assert_eq!(state.cursor, 0);
    }

    #[test]
    fn apply_select_key_up_wraps_from_top() {
        let items = ["a", "b", "c"];
        let items_slice: Vec<&str> = items.to_vec();
        let mut state = SelectState::new("pick", &items_slice, 0);
        apply_select_key(&mut state, PromptKey::Up);
        assert_eq!(state.cursor, 2);
    }

    #[test]
    fn apply_select_key_confirm_and_cancel() {
        let items = ["a"];
        let items_slice: Vec<&str> = items.to_vec();
        let mut state = SelectState::new("pick", &items_slice, 0);
        assert_eq!(
            apply_select_key(&mut state, PromptKey::Confirm),
            Outcome::Confirm
        );
        assert_eq!(
            apply_select_key(&mut state, PromptKey::Cancel),
            Outcome::Cancel
        );
    }

    // ── MultiSelectState ────────────────────────────────────────────

    #[test]
    fn multi_defaults_populate_checked() {
        let items = ["a", "b", "c"];
        let items_slice: Vec<&str> = items.to_vec();
        let state = MultiSelectState::new("pick", &items_slice, &[true, false, true]);
        assert_eq!(state.checked, vec![true, false, true]);
    }

    #[test]
    fn apply_multi_key_toggle_flips_current_row() {
        let items = ["a", "b"];
        let items_slice: Vec<&str> = items.to_vec();
        let mut state = MultiSelectState::new("pick", &items_slice, &[false, false]);
        state.cursor = 1;
        apply_multi_key(&mut state, PromptKey::Toggle);
        assert_eq!(state.checked, vec![false, true]);
        apply_multi_key(&mut state, PromptKey::Toggle);
        assert_eq!(state.checked, vec![false, false]);
    }

    #[test]
    fn apply_multi_key_confirm_preserves_selection_order() {
        let items = ["a", "b", "c"];
        let items_slice: Vec<&str> = items.to_vec();
        let mut state = MultiSelectState::new("pick", &items_slice, &[true, false, true]);
        // Verify checked at cursor toggles independently.
        state.cursor = 1;
        apply_multi_key(&mut state, PromptKey::Toggle);
        assert_eq!(state.checked, vec![true, true, true]);
    }

    // ── rendering ───────────────────────────────────────────────────

    #[test]
    fn render_select_frame_has_prompt_items_and_help() {
        let items = ["apple", "banana"];
        let items_slice: Vec<&str> = items.to_vec();
        let state = SelectState::new("Pick one", &items_slice, 0);
        let lines = render_select_frame(&state, 80);
        // 1 prompt + 2 items + 1 help = 4 lines
        assert_eq!(lines.len(), 4);
        assert!(lines[0].contains("Pick one"));
        assert!(lines[1].contains("apple"));
        assert!(lines[2].contains("banana"));
    }

    #[test]
    fn render_select_frame_marks_cursor_row() {
        let items = ["a", "b"];
        let items_slice: Vec<&str> = items.to_vec();
        let state = SelectState::new("p", &items_slice, 1);
        let lines = render_select_frame(&state, 40);
        // cursor row has the `>` marker (inside ANSI), non-cursor has plain space.
        assert!(lines[2].contains('>'));
        assert!(!lines[1].contains('>'));
    }

    #[test]
    fn render_multi_frame_shows_checkbox_state() {
        let items = ["one", "two"];
        let items_slice: Vec<&str> = items.to_vec();
        let state = MultiSelectState::new("p", &items_slice, &[true, false]);
        let lines = render_multi_frame(&state, 40);
        assert!(
            lines[1].contains("☑"),
            "checked row missing ☑: {:?}",
            lines[1]
        );
        assert!(
            lines[2].contains("☐"),
            "unchecked row missing ☐: {:?}",
            lines[2]
        );
    }

    #[test]
    fn render_frames_fit_in_term_width() {
        // Label long enough to force truncation.
        let long = "x".repeat(200);
        let items_owned = [long];
        let items: Vec<&str> = items_owned.iter().map(String::as_str).collect();
        let state = SelectState::new("p", &items, 0);
        let lines = render_select_frame(&state, 20);
        for line in &lines {
            assert!(
                visible_width(line) <= 20,
                "line exceeds 20 cells: visible={}, raw={:?}",
                visible_width(line),
                line
            );
        }
    }

    #[test]
    fn render_multi_frame_fits_in_term_width() {
        let long = "y".repeat(200);
        let items_owned = [long];
        let items: Vec<&str> = items_owned.iter().map(String::as_str).collect();
        let state = MultiSelectState::new("p", &items, &[true]);
        let lines = render_multi_frame(&state, 24);
        for line in &lines {
            assert!(visible_width(line) <= 24);
        }
    }

    // ── strip_ansi ──────────────────────────────────────────────────

    #[test]
    fn strip_ansi_removes_csi_sequences() {
        assert_eq!(strip_ansi("\x1b[1;32mbold-green\x1b[0m"), "bold-green");
        assert_eq!(strip_ansi("plain"), "plain");
        assert_eq!(strip_ansi("\x1b[31ma\x1b[0mb\x1b[32mc\x1b[0m"), "abc");
    }
}
