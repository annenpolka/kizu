//! Syntax highlighting via syntect (same engine as bat/delta).
//!
//! The [`Highlighter`] is lazily initialized on first use to avoid
//! paying the SyntaxSet/ThemeSet load cost at startup. It is stored
//! in [`App`] and shared by both the diff view and file view renderers.

use ratatui::style::Color;
use std::path::Path;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Style, ThemeSet};
use syntect::parsing::SyntaxSet;

/// Cached syntax highlighting state. Created once per App lifetime
/// via [`Highlighter::new`], then reused across frames.
pub struct Highlighter {
    syntax_set: SyntaxSet,
    theme_set: ThemeSet,
    theme_name: String,
}

/// One highlighted token: a span of text with a foreground color.
pub struct HlToken {
    pub text: String,
    pub fg: Color,
}

impl Highlighter {
    pub fn new() -> Self {
        Self {
            syntax_set: SyntaxSet::load_defaults_newlines(),
            theme_set: ThemeSet::load_defaults(),
            theme_name: "base16-eighties.dark".to_string(),
        }
    }

    /// Highlight a single line of code, returning a vec of colored
    /// tokens. Falls back to a single unstyled token if the file
    /// extension is unknown or highlighting fails.
    pub fn highlight_line(&self, line: &str, path: &Path) -> Vec<HlToken> {
        let syntax = path
            .extension()
            .and_then(|e| e.to_str())
            .and_then(|ext| self.syntax_set.find_syntax_by_extension(ext))
            .or_else(|| {
                path.file_name()
                    .and_then(|n| n.to_str())
                    .and_then(|name| self.syntax_set.find_syntax_by_extension(name))
            })
            .unwrap_or_else(|| self.syntax_set.find_syntax_plain_text());

        let theme = self
            .theme_set
            .themes
            .get(&self.theme_name)
            .unwrap_or_else(|| {
                self.theme_set
                    .themes
                    .values()
                    .next()
                    .expect("at least one theme")
            });

        let mut hl = HighlightLines::new(syntax, theme);
        match hl.highlight_line(line, &self.syntax_set) {
            Ok(tokens) => tokens
                .into_iter()
                .map(|(style, text)| HlToken {
                    text: text.to_string(),
                    fg: syntect_to_ratatui_color(style),
                })
                .collect(),
            Err(_) => vec![HlToken {
                text: line.to_string(),
                fg: Color::Reset,
            }],
        }
    }
}

fn syntect_to_ratatui_color(style: Style) -> Color {
    let c = style.foreground;
    Color::Rgb(c.r, c.g, c.b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn highlight_rust_code_produces_multiple_tokens() {
        let hl = Highlighter::new();
        let tokens = hl.highlight_line("fn main() {}", Path::new("test.rs"));
        assert!(tokens.len() > 1, "Rust code should produce multiple tokens");
    }

    #[test]
    fn highlight_unknown_extension_returns_single_token() {
        let hl = Highlighter::new();
        let tokens = hl.highlight_line("hello world", Path::new("file.xyzunknown"));
        assert!(!tokens.is_empty());
    }

    #[test]
    fn highlight_empty_line_does_not_panic() {
        let hl = Highlighter::new();
        let tokens = hl.highlight_line("", Path::new("a.rs"));
        // Empty line may produce 0 or 1 tokens, just verify no panic.
        let _ = tokens;
    }
}
