//! Syntax highlighting via syntect (same engine as bat/delta).
//!
//! The [`Highlighter`] is lazily initialized on first use to avoid
//! paying the SyntaxSet/ThemeSet load cost at startup. It is stored
//! in [`App`] and shared by both the diff view and file view renderers.

use ratatui::style::Color;
use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
};
use syntect::easy::HighlightLines;
use syntect::highlighting::{Style, ThemeSet};
use syntect::parsing::SyntaxSet;
use tree_sitter_highlight::HighlightEvent;

use crate::language::js_ts::{dialect_for_path, highlight_configuration};

const HIGHLIGHT_CACHE_CAP: usize = 8_192;
const DOCUMENT_HIGHLIGHT_CACHE_CAP: usize = 64;

/// Cached syntax highlighting state. Created once per App lifetime
/// via [`Highlighter::new`], then reused across frames.
pub struct Highlighter {
    syntax_set: SyntaxSet,
    theme_set: ThemeSet,
    theme_name: String,
    cache: RefCell<HighlightCache>,
    doc_cache: RefCell<HighlightDocumentCache>,
}

/// One highlighted token: a span of text with a foreground color.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HlToken {
    pub text: String,
    pub fg: Color,
}

#[derive(Clone)]
pub struct HighlightedDocument {
    pub lines: Vec<Vec<HlToken>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct HighlightCacheKey {
    path: PathBuf,
    line: String,
}

#[derive(Default)]
struct HighlightCache {
    map: HashMap<HighlightCacheKey, Vec<HlToken>>,
    order: VecDeque<HighlightCacheKey>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct HighlightDocumentCacheKey {
    path: PathBuf,
    content: String,
}

#[derive(Default)]
struct HighlightDocumentCache {
    map: HashMap<HighlightDocumentCacheKey, HighlightedDocument>,
    order: VecDeque<HighlightDocumentCacheKey>,
}

impl HighlightCache {
    fn get(&self, key: &HighlightCacheKey) -> Option<Vec<HlToken>> {
        self.map.get(key).cloned()
    }

    fn insert(&mut self, key: HighlightCacheKey, tokens: Vec<HlToken>) {
        if let Some(slot) = self.map.get_mut(&key) {
            *slot = tokens;
            return;
        }
        self.order.push_back(key.clone());
        self.map.insert(key, tokens);
        while self.map.len() > HIGHLIGHT_CACHE_CAP {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.map.remove(&oldest);
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.map.len()
    }
}

impl HighlightDocumentCache {
    fn get(&self, key: &HighlightDocumentCacheKey) -> Option<HighlightedDocument> {
        self.map.get(key).cloned()
    }

    fn insert(&mut self, key: HighlightDocumentCacheKey, document: HighlightedDocument) {
        if let Some(slot) = self.map.get_mut(&key) {
            *slot = document;
            return;
        }
        self.order.push_back(key.clone());
        self.map.insert(key, document);
        while self.map.len() > DOCUMENT_HIGHLIGHT_CACHE_CAP {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.map.remove(&oldest);
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.map.len()
    }
}

impl Highlighter {
    pub fn new() -> Self {
        Self {
            syntax_set: SyntaxSet::load_defaults_newlines(),
            theme_set: ThemeSet::load_defaults(),
            theme_name: "base16-eighties.dark".to_string(),
            cache: RefCell::new(HighlightCache::default()),
            doc_cache: RefCell::new(HighlightDocumentCache::default()),
        }
    }

    /// Highlight a single line of code, returning a vec of colored
    /// tokens. Falls back to a single unstyled token if the file
    /// extension is unknown or highlighting fails.
    pub fn highlight_line(&self, line: &str, path: &Path) -> Vec<HlToken> {
        let cache_key = HighlightCacheKey {
            path: path.to_path_buf(),
            line: line.to_string(),
        };
        if let Some(tokens) = self.cache.borrow().get(&cache_key) {
            return tokens;
        }
        let tokens = self.highlight_line_uncached(line, path);
        self.cache.borrow_mut().insert(cache_key, tokens.clone());
        tokens
    }

    pub fn highlight_document(&self, content: &str, path: &Path) -> HighlightedDocument {
        let cache_key = HighlightDocumentCacheKey {
            path: path.to_path_buf(),
            content: content.to_string(),
        };
        if let Some(document) = self.doc_cache.borrow().get(&cache_key) {
            return document;
        }
        if let Some(dialect) = dialect_for_path(path)
            && let Ok(document) = self.highlight_js_ts_document(content, dialect)
        {
            self.doc_cache
                .borrow_mut()
                .insert(cache_key, document.clone());
            return document;
        }
        let document = self.highlight_document_with_syntect(content, path);
        self.doc_cache
            .borrow_mut()
            .insert(cache_key, document.clone());
        document
    }

    fn highlight_js_ts_document(
        &self,
        content: &str,
        dialect: crate::language::js_ts::JsTsDialect,
    ) -> anyhow::Result<HighlightedDocument> {
        let mut config = highlight_configuration(dialect)?;
        config.configure(TREE_SITTER_HIGHLIGHT_NAMES);

        let mut highlighter = tree_sitter_highlight::Highlighter::new();
        let events = highlighter.highlight(&config, content.as_bytes(), None, |_| None)?;
        let mut lines = vec![Vec::new()];
        let mut color_stack = vec![Color::Reset];
        let mut current_fg = Color::Reset;

        for event in events {
            match event? {
                HighlightEvent::Source { start, end } => {
                    if let Some(text) = content.get(start..end) {
                        append_highlighted_source(&mut lines, text, current_fg);
                    }
                }
                HighlightEvent::HighlightStart(highlight) => {
                    color_stack.push(current_fg);
                    current_fg = tree_sitter_highlight_color(highlight.0);
                }
                HighlightEvent::HighlightEnd => {
                    current_fg = color_stack.pop().unwrap_or(Color::Reset);
                }
            }
        }

        Ok(HighlightedDocument {
            lines: finalize_document_lines(lines, content),
        })
    }

    fn highlight_document_with_syntect(&self, content: &str, path: &Path) -> HighlightedDocument {
        HighlightedDocument {
            lines: content
                .lines()
                .map(|line| self.highlight_line(line, path))
                .collect(),
        }
    }

    fn highlight_line_uncached(&self, line: &str, path: &Path) -> Vec<HlToken> {
        let ext = path.extension().and_then(|e| e.to_str());
        let syntax = ext
            .and_then(|e| self.syntax_set.find_syntax_by_extension(e))
            // Fallback: syntect's defaults lack TypeScript, TSX, Vue,
            // Svelte, etc. Map them to the nearest available syntax.
            .or_else(|| {
                let fallback = match ext {
                    Some("ts" | "mts" | "cts") => Some("js"),
                    Some("tsx") => Some("jsx"),
                    Some("vue" | "svelte") => Some("html"),
                    Some("jsonc") => Some("json"),
                    _ => None,
                };
                fallback.and_then(|f| self.syntax_set.find_syntax_by_extension(f))
            })
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

    #[cfg(test)]
    fn cache_len(&self) -> usize {
        self.cache.borrow().len()
    }

    #[cfg(test)]
    pub(crate) fn document_cache_len(&self) -> usize {
        self.doc_cache.borrow().len()
    }
}

const TREE_SITTER_HIGHLIGHT_NAMES: &[&str] = &[
    "attribute",
    "comment",
    "constant",
    "constructor",
    "function",
    "function.method",
    "keyword",
    "number",
    "operator",
    "property",
    "punctuation",
    "punctuation.bracket",
    "string",
    "tag",
    "type",
    "type.builtin",
    "variable",
    "variable.parameter",
];

fn tree_sitter_highlight_color(index: usize) -> Color {
    match TREE_SITTER_HIGHLIGHT_NAMES.get(index).copied() {
        Some("attribute" | "property") => Color::Cyan,
        Some("comment") => Color::DarkGray,
        Some("constant" | "number") => Color::Yellow,
        Some("constructor" | "function" | "function.method") => Color::Blue,
        Some("keyword" | "operator") => Color::Magenta,
        Some("punctuation" | "punctuation.bracket") => Color::Gray,
        Some("string") => Color::Green,
        Some("tag") => Color::LightBlue,
        Some("type" | "type.builtin") => Color::LightYellow,
        Some("variable.parameter") => Color::LightCyan,
        Some("variable") => Color::Reset,
        _ => Color::Reset,
    }
}

fn append_highlighted_source(lines: &mut Vec<Vec<HlToken>>, text: &str, fg: Color) {
    for (idx, part) in text.split('\n').enumerate() {
        if idx > 0 {
            lines.push(Vec::new());
        }
        if !part.is_empty() {
            push_token(lines.last_mut().expect("at least one line"), part, fg);
        }
    }
}

fn push_token(line: &mut Vec<HlToken>, text: &str, fg: Color) {
    if let Some(last) = line.last_mut()
        && last.fg == fg
    {
        last.text.push_str(text);
        return;
    }
    line.push(HlToken {
        text: text.to_string(),
        fg,
    });
}

fn finalize_document_lines(mut lines: Vec<Vec<HlToken>>, content: &str) -> Vec<Vec<HlToken>> {
    if content.is_empty() {
        return Vec::new();
    }
    if content.ends_with('\n') && lines.last().is_some_and(Vec::is_empty) {
        lines.pop();
    }
    lines
}

impl Default for Highlighter {
    fn default() -> Self {
        Self::new()
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
    fn highlight_typescript_code_produces_multiple_tokens() {
        let hl = Highlighter::new();
        let tokens = hl.highlight_line("const x: number = 42;", Path::new("app.ts"));
        assert!(
            tokens.len() > 1,
            "TypeScript should produce multiple tokens, got {} — syntect may not recognise .ts",
            tokens.len()
        );
    }

    #[test]
    fn ts_falls_back_to_js_highlighting() {
        let hl = Highlighter::new();
        // Even though syntect doesn't natively support .ts,
        // our fallback maps it to JavaScript syntax.
        let tokens = hl.highlight_line("const x: number = 42;", Path::new("app.ts"));
        assert!(
            tokens.len() > 1,
            ".ts should produce highlighted tokens via JS fallback, got {}",
            tokens.len()
        );
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

    #[test]
    fn highlight_line_reuses_cached_tokens_for_same_path_and_content() {
        let hl = Highlighter::new();
        let path = Path::new("test.rs");

        let first = hl.highlight_line("fn main() {}", path);
        assert_eq!(hl.cache_len(), 1);
        let second = hl.highlight_line("fn main() {}", path);

        assert_eq!(hl.cache_len(), 1, "same line should hit cache");
        assert_eq!(first.len(), second.len());
        assert!(
            first
                .iter()
                .zip(second.iter())
                .all(|(a, b)| a.text == b.text && a.fg == b.fg),
            "cached tokens must preserve text and colors"
        );
    }

    mod tsx_document_highlight {
        use super::*;

        fn distinct_non_reset_colors(tokens: &[HlToken]) -> usize {
            let mut colors = tokens
                .iter()
                .filter_map(|token| (token.fg != Color::Reset).then_some(token.fg))
                .collect::<Vec<_>>();
            colors.sort_by_key(|color| format!("{color:?}"));
            colors.dedup();
            colors.len()
        }

        #[test]
        fn tsx_document_highlight_distinguishes_jsx_tag_and_ts_keyword() {
            let hl = Highlighter::new();
            let source = "export function Counter({ count }: { count: number }) {\n  return (\n    <section className=\"counter\">\n      <p>Count: {count}</p>\n    </section>\n  );\n}\n";

            let doc = hl.highlight_document(source, Path::new("Counter.tsx"));

            assert_eq!(doc.lines.len(), source.lines().count());
            assert!(
                distinct_non_reset_colors(&doc.lines[0]) > 1,
                "TypeScript declaration line should carry keyword/type colors: {:?}",
                doc.lines[0]
            );
            assert!(
                doc.lines[2]
                    .iter()
                    .any(|token| token.text.contains("section") && token.fg != Color::Reset),
                "JSX tag name should be highlighted: {:?}",
                doc.lines[2]
            );
        }

        #[test]
        fn tsx_document_highlight_keeps_multiline_jsx_context() {
            let hl = Highlighter::new();
            let source = "export const button = (\n  <Button\n    kind=\"primary\"\n    onClick={() => save()}\n  />\n);\n";

            let doc = hl.highlight_document(source, Path::new("Button.tsx"));

            assert!(
                doc.lines[2]
                    .iter()
                    .any(|token| token.text.contains("kind") && token.fg != Color::Reset),
                "attribute line should keep JSX context across lines: {:?}",
                doc.lines[2]
            );
        }

        #[test]
        fn highlight_line_keeps_syntect_fallback_for_rust() {
            let hl = Highlighter::new();
            let doc = hl.highlight_document("fn main() {}\n", Path::new("main.rs"));

            assert_eq!(doc.lines.len(), 1);
            assert!(
                doc.lines[0].len() > 1,
                "Rust document highlight should keep syntect fallback"
            );
        }
    }
}
