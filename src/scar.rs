//! scar (`@kizu[...]:` inline comment) core — M2/M3 of the v0.2 ExecPlan.
//!
//! Pure, dependency-free logic for picking the right source-level
//! comment syntax for a given file path and for rendering +
//! inserting a `@kizu[<kind>]: <body>` scar. The bracket tag makes
//! the format parse with a single regex (`/@kizu\[(\w+)\]:\s*(.*)/`)
//! so the future hook layer can extract category without a
//! per-language tokenizer. The app layer calls [`insert_scar`] from
//! its normal-mode `a` / `r` / `c` key dispatch (M4 of v0.2).

use std::path::Path;

use anyhow::{Context, Result};

use crate::language::js_ts::{JsTsScarStyle, dialect_for_path, scar_placement_for_line};

/// The lexical shape of a single-line or block comment in the target
/// language. `open` is the leading marker (`//`, `#`, `<!--`, …) and
/// `close` is the trailing marker for languages that need one
/// (`-->`, `*/`). Languages with line comments leave `close` at
/// `None`.
///
/// The two fields are `&'static str` because the syntax table is
/// hard-coded — every variant we ship comes from a fixed list and
/// there is no need for per-file allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommentSyntax {
    pub open: &'static str,
    pub close: Option<&'static str>,
}

impl CommentSyntax {
    /// Wrap `body` in this comment syntax. The leading open marker
    /// and optional close marker are always added; the body itself
    /// is passed through verbatim, so callers that want structured
    /// content ([`CommentSyntax::render_scar`]) build it on top.
    pub fn wrap(&self, body: &str) -> String {
        match self.close {
            Some(close) => format!("{} {} {}", self.open, body, close),
            None => format!("{} {}", self.open, body),
        }
    }

    /// Render a kizu scar line in this comment syntax.
    ///
    /// The final shape is `<open> @kizu[<kind>]: <body> <close?>`.
    /// The bracketed kind tag makes the line parse cleanly with a
    /// single regex (`/@kizu\[(\w+)\]:\s*(.*)/`) and keeps category
    /// extraction language-independent — the hook layer can list
    /// open scars by category without a per-language tokenizer.
    ///
    /// Examples:
    ///
    /// - Rust / TS / Go: `// @kizu[ask]: explain this change`
    /// - Python / YAML: `# @kizu[reject]: revert this change`
    /// - HTML / XML: `<!-- @kizu[free]: why is this here? -->`
    /// - CSS / SCSS: `/* @kizu[ask]: explain this change */`
    /// - SQL / Lua / Haskell: `-- @kizu[reject]: revert this change`
    pub fn render_scar(&self, kind: ScarKind, body: &str) -> String {
        self.wrap(&format!("@kizu[{}]: {body}", kind.tag()))
    }
}

/// The three canned categories a scar can carry.
///
/// - `Ask` is bound to the `a` key; the canned body asks the agent
///   to explain the change.
/// - `Reject` is bound to `r`; asks the agent to revert the change.
/// - `Free` is the free-text `c` key — the body is whatever the
///   user typed into the scar comment prompt, and the `free` tag
///   lets the hook layer distinguish "I wrote this by hand" from
///   the canned variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScarKind {
    Ask,
    Reject,
    Free,
}

impl ScarKind {
    /// Lowercase tag used inside the `@kizu[...]:` bracket. Stable
    /// across the whole codebase — changing it would invalidate
    /// every scar already written into a repo.
    pub fn tag(self) -> &'static str {
        match self {
            ScarKind::Ask => "ask",
            ScarKind::Reject => "reject",
            ScarKind::Free => "free",
        }
    }
}

const SLASH_SLASH: CommentSyntax = CommentSyntax {
    open: "//",
    close: None,
};
const HASH: CommentSyntax = CommentSyntax {
    open: "#",
    close: None,
};
const HTML: CommentSyntax = CommentSyntax {
    open: "<!--",
    close: Some("-->"),
};
const CSS: CommentSyntax = CommentSyntax {
    open: "/*",
    close: Some("*/"),
};
const DASH_DASH: CommentSyntax = CommentSyntax {
    open: "--",
    close: None,
};
const JSX_BLOCK: CommentSyntax = CommentSyntax {
    open: "{/*",
    close: Some("*/}"),
};

/// Detect the comment syntax for a file path, using its extension.
///
/// The mapping follows `docs/inline-scar-pattern.md` and the v0.2
/// ExecPlan's M2 table. Extension matching is case-insensitive so
/// `Makefile.RS` or `script.PY` behave like their lowercase
/// counterparts. Files with no extension — or any extension the
/// table does not cover — fall back to the `#` syntax, which
/// compiles in the widest range of scripting and data languages
/// (shell, Python, YAML, TOML) and is therefore the least
/// disruptive default.
pub fn detect_comment_syntax(path: &Path) -> CommentSyntax {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return HASH;
    };
    let ext_lower = ext.to_ascii_lowercase();
    match ext_lower.as_str() {
        "rs" | "ts" | "tsx" | "js" | "jsx" | "java" | "go" | "c" | "cpp" | "cc" | "h" | "hpp"
        | "swift" | "kt" | "kts" | "scala" | "dart" => SLASH_SLASH,
        "rb" | "py" | "sh" | "bash" | "zsh" | "fish" | "yaml" | "yml" | "toml" | "ini" | "conf"
        | "r" | "pl" | "ex" | "exs" => HASH,
        "html" | "htm" | "xml" | "svg" | "vue" | "md" => HTML,
        "css" | "scss" | "sass" | "less" => CSS,
        "sql" | "lua" | "hs" | "ada" | "sqlite" => DASH_DASH,
        _ => HASH,
    }
}

/// Insert a scar comment on the line directly above `line_number`.
///
/// `line_number` is the 1-indexed source line the scar is commenting
/// *about* — the rendered `@kizu[<kind>]: <body>` line lands
/// immediately above it so the reader sees the note first and then
/// the code it annotates. `kind` selects the canned category tag
/// and `body` is the human instruction text; `insert_scar` wraps
/// the pair in the file's comment syntax (via
/// [`detect_comment_syntax`] + [`CommentSyntax::render_scar`])
/// before writing.
///
/// # Idempotency
///
/// If the line directly above the insertion point is already the
/// *same* scar (same kind, same body, trimmed), this is a no-op.
/// This makes `insert_scar` safe to call repeatedly from the app
/// loop without stacking duplicate comments — a property we rely on
/// when a single keypress triggers both the write and the
/// watcher-driven recompute that follows.
///
/// # Line endings
///
/// The file's existing line endings are preserved. If the current
/// content contains `\r\n` anywhere, the scar line is emitted with a
/// `\r\n` terminator; otherwise plain `\n` is used. Mixed-ending
/// files are rare enough in practice that we don't try to detect a
/// dominant ending — the CRLF branch wins if any CRLF is present.
///
/// # Clamping
///
/// `line_number` values past the end of the file are clamped so the
/// scar lands at the end instead of erroring. `line_number == 0` is
/// treated as 1 (insert at file start).
///
/// # Errors
///
/// Receipt of a successful scar insertion, for undo / audit.
///
/// `rendered` is the exact comment line written (without trailing
/// newline) — `remove_scar` uses it to verify the line still matches
/// before deleting, so a user edit between insert and undo is detected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScarInsert {
    pub line_1indexed: usize,
    pub rendered: String,
}

/// Insert a kizu scar above `line_number` (1-indexed) in `path`.
///
/// Returns:
/// - `Ok(Some(ScarInsert))` — scar was written; receipt captures the
///   exact rendered line + its 1-indexed position post-insert, so
///   callers can store it for undo.
/// - `Ok(None)` — idempotent no-op (an identical scar already sits
///   immediately above the target line).
/// - `Err(...)` — file read / write failed. Callers surface this via
///   `App.last_error` rather than panicking.
pub fn insert_scar(
    path: &Path,
    line_number: usize,
    kind: ScarKind,
    body: &str,
) -> Result<Option<ScarInsert>> {
    let original = std::fs::read_to_string(path)
        .with_context(|| format!("reading {} for scar insertion", path.display()))?;
    let (syntax, line_number) = if let Some(dialect) = dialect_for_path(path) {
        let placement = scar_placement_for_line(dialect, &original, line_number)?;
        let syntax = match placement.style {
            JsTsScarStyle::LineComment => SLASH_SLASH,
            JsTsScarStyle::JsxBlockComment => JSX_BLOCK,
        };
        (syntax, placement.insert_before_line_1indexed)
    } else {
        (detect_comment_syntax(path), line_number)
    };
    let scar_body = syntax.render_scar(kind, body);
    let newline = if original.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };

    // `split_inclusive('\n')` keeps each line's own terminator, so we
    // can rebuild the file without normalizing endings across the
    // non-edited region.
    let lines: Vec<&str> = original.split_inclusive('\n').collect();
    let line_count = lines.len();
    let target = line_number.max(1);
    let insert_at = target.saturating_sub(1).min(line_count);

    // Inherit indentation so the scar reads as an inline comment on
    // the code it annotates. Prefer the target line's own indent;
    // fall back to the nearest non-blank line (forward first, then
    // back) when the target itself is blank. A fully blank surround
    // yields an empty prefix, which matches "no indent".
    let indent = inherited_indent(&lines, insert_at);
    let scar_line = if indent.is_empty() {
        scar_body.clone()
    } else {
        format!("{indent}{scar_body}")
    };

    // Idempotent guard: if the line immediately above the insertion
    // point is already the same scar (modulo leading whitespace), leave
    // the file untouched. The trim pass on both sides handles the case
    // where the existing scar was written with a different indent.
    if insert_at > 0 {
        let prev_raw = lines[insert_at - 1];
        let prev_trimmed = prev_raw.trim_end_matches('\n').trim_end_matches('\r');
        if prev_trimmed.trim() == scar_body.trim() {
            return Ok(None);
        }
    }

    let mut out = String::with_capacity(original.len() + scar_line.len() + newline.len() * 2);
    for line in &lines[..insert_at] {
        out.push_str(line);
    }
    // If appending after a line that lacks a trailing newline (e.g.
    // EOF without final newline), add one so the scar starts on its
    // own line instead of splicing into the previous content.
    if insert_at > 0 && !lines[insert_at - 1].ends_with('\n') {
        out.push_str(newline);
    }
    out.push_str(&scar_line);
    out.push_str(newline);
    for line in &lines[insert_at..] {
        out.push_str(line);
    }

    write_preserving_mtime(path, out.as_bytes())
        .with_context(|| format!("writing {} with scar inserted", path.display()))?;
    // Post-insert: the scar occupies 1-indexed line `insert_at + 1`,
    // which equals the clamped `target`.
    Ok(Some(ScarInsert {
        line_1indexed: insert_at + 1,
        rendered: scar_line,
    }))
}

/// Write `content` to `path` while keeping the file's modified time at
/// its pre-write value. A scar insert/remove is an annotation, not a
/// code edit — preserving mtime prevents the scarred file from floating
/// to the tail of kizu's mtime-sorted file list, which would visibly
/// jerk the hunk to the bottom of follow mode.
fn write_preserving_mtime(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let pre_mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    std::fs::write(path, content)?;
    if let Some(mtime) = pre_mtime
        && let Ok(f) = std::fs::File::options().write(true).open(path)
    {
        let _ = f.set_times(std::fs::FileTimes::new().set_modified(mtime));
    }
    Ok(())
}

/// Return the leading whitespace string to reuse for a scar inserted
/// at `insert_at` (0-indexed slot). Strategy:
/// 1. Look at the line *at* `insert_at` (the target, which the scar
///    will sit directly above). If it's non-blank, use its indent.
/// 2. Otherwise scan forward, then backward, for the nearest non-blank
///    line and use its indent.
/// 3. Fall back to empty string if the file is entirely blank.
fn inherited_indent(lines: &[&str], insert_at: usize) -> String {
    fn line_indent(line: &str) -> Option<String> {
        let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
        if trimmed.trim().is_empty() {
            return None;
        }
        let indent: String = trimmed
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        Some(indent)
    }
    if let Some(target) = lines.get(insert_at)
        && let Some(ind) = line_indent(target)
    {
        return ind;
    }
    // Forward scan.
    for line in lines.iter().skip(insert_at + 1) {
        if let Some(ind) = line_indent(line) {
            return ind;
        }
    }
    // Backward scan.
    for line in lines[..insert_at].iter().rev() {
        if let Some(ind) = line_indent(line) {
            return ind;
        }
    }
    String::new()
}

/// Outcome of a `remove_scar` attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScarRemove {
    /// The line matched `expected` and was deleted.
    Removed,
    /// The line at `line_1indexed` no longer matches `expected` —
    /// the user edited the file between insert and undo. The file is
    /// left untouched.
    Mismatch,
    /// `line_1indexed` is past the end of the file (e.g. the file
    /// was truncated after the insert). Left untouched.
    OutOfRange,
}

/// Remove a previously-inserted scar line if, and only if, the line
/// at `line_1indexed` still matches `expected` (the trimmed scar
/// body).
///
/// This is the undo primitive used by the app layer. It refuses to
/// delete content the user has since changed, so an undo pressed
/// after an unrelated edit is a safe no-op that surfaces through
/// the returned `ScarRemove` variant.
pub fn remove_scar(path: &Path, line_1indexed: usize, expected: &str) -> Result<ScarRemove> {
    let original = std::fs::read_to_string(path)
        .with_context(|| format!("reading {} for scar removal", path.display()))?;
    let lines: Vec<&str> = original.split_inclusive('\n').collect();
    if line_1indexed == 0 || line_1indexed > lines.len() {
        return Ok(ScarRemove::OutOfRange);
    }
    let target_idx = line_1indexed - 1;
    let line_raw = lines[target_idx];
    let line_trimmed = line_raw.trim_end_matches('\n').trim_end_matches('\r');
    if line_trimmed.trim() != expected.trim() {
        return Ok(ScarRemove::Mismatch);
    }
    let mut out = String::with_capacity(original.len().saturating_sub(line_raw.len()));
    for (idx, line) in lines.iter().enumerate() {
        if idx == target_idx {
            continue;
        }
        out.push_str(line);
    }
    write_preserving_mtime(path, out.as_bytes())
        .with_context(|| format!("writing {} with scar removed", path.display()))?;
    Ok(ScarRemove::Removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::SystemTime;

    fn syntax(path: &str) -> CommentSyntax {
        detect_comment_syntax(&PathBuf::from(path))
    }

    #[test]
    fn detect_slash_slash_for_c_family_and_web_script_languages() {
        for path in [
            "src/main.rs",
            "src/app.ts",
            "src/widget.tsx",
            "src/lib.js",
            "src/lib.jsx",
            "src/Foo.java",
            "src/server.go",
            "src/a.c",
            "src/a.cpp",
            "src/a.cc",
            "src/a.h",
            "src/a.hpp",
            "src/Contact.swift",
            "src/Foo.kt",
            "build.kts",
            "src/Foo.scala",
            "src/main.dart",
        ] {
            assert_eq!(
                syntax(path),
                SLASH_SLASH,
                "{path} should use // line comments"
            );
        }
    }

    #[test]
    fn detect_hash_for_script_and_data_languages() {
        for path in [
            "app/server.rb",
            "scripts/run.py",
            "scripts/run.sh",
            "scripts/run.bash",
            "scripts/run.zsh",
            "scripts/run.fish",
            "config/app.yaml",
            "config/app.yml",
            "config/app.toml",
            "config/app.ini",
            "config/app.conf",
            "analysis.r",
            "tool.pl",
            "lib/mod.ex",
            "lib/mod.exs",
        ] {
            assert_eq!(syntax(path), HASH, "{path} should use # line comments");
        }
    }

    #[test]
    fn detect_html_style_for_markup_and_markdown() {
        for path in [
            "web/index.html",
            "web/index.htm",
            "data/tree.xml",
            "assets/icon.svg",
            "web/App.vue",
            "README.md",
        ] {
            assert_eq!(
                syntax(path),
                HTML,
                "{path} should use <!-- --> block comments"
            );
        }
    }

    #[test]
    fn detect_c_block_for_stylesheets() {
        for path in [
            "web/style.css",
            "web/theme.scss",
            "web/theme.sass",
            "web/theme.less",
        ] {
            assert_eq!(syntax(path), CSS, "{path} should use /* */ comments");
        }
    }

    #[test]
    fn detect_dash_dash_for_sql_lua_haskell_ada() {
        for path in [
            "db/migrate.sql",
            "db/schema.sqlite",
            "src/main.lua",
            "src/Main.hs",
            "src/proc.ada",
        ] {
            assert_eq!(
                syntax(path),
                DASH_DASH,
                "{path} should use -- line comments"
            );
        }
    }

    #[test]
    fn unknown_extension_falls_back_to_hash() {
        for path in [
            "weird.zzz",
            "data.bin",
            "Dockerfile.template",
            "unknown.q9q9q9",
        ] {
            assert_eq!(syntax(path), HASH, "{path} should fall back to # comment");
        }
    }

    #[test]
    fn no_extension_falls_back_to_hash() {
        for path in ["Makefile", "Dockerfile", "LICENSE", "README"] {
            assert_eq!(
                syntax(path),
                HASH,
                "{path} without extension should fall back to # comment"
            );
        }
    }

    #[test]
    fn extension_matching_is_case_insensitive() {
        assert_eq!(syntax("src/MAIN.RS"), SLASH_SLASH);
        assert_eq!(syntax("config.YAML"), HASH);
        assert_eq!(syntax("page.HTML"), HTML);
        assert_eq!(syntax("theme.CSS"), CSS);
        assert_eq!(syntax("migrate.SQL"), DASH_DASH);
    }

    #[test]
    fn render_scar_line_comment_emits_kizu_bracket_tag() {
        assert_eq!(
            SLASH_SLASH.render_scar(ScarKind::Ask, "explain this change"),
            "// @kizu[ask]: explain this change"
        );
        assert_eq!(
            HASH.render_scar(ScarKind::Reject, "revert this change"),
            "# @kizu[reject]: revert this change"
        );
        assert_eq!(
            DASH_DASH.render_scar(ScarKind::Free, "why here?"),
            "-- @kizu[free]: why here?"
        );
    }

    #[test]
    fn render_scar_block_comment_wraps_kizu_bracket_tag() {
        assert_eq!(
            HTML.render_scar(ScarKind::Ask, "explain"),
            "<!-- @kizu[ask]: explain -->"
        );
        assert_eq!(
            CSS.render_scar(ScarKind::Free, "explain"),
            "/* @kizu[free]: explain */"
        );
    }

    #[test]
    fn render_scar_preserves_unicode_and_whitespace_inside_body() {
        assert_eq!(
            SLASH_SLASH.render_scar(ScarKind::Free, "日本語 with spaces  and 記号！"),
            "// @kizu[free]: 日本語 with spaces  and 記号！"
        );
    }

    #[test]
    fn scar_kind_tag_is_stable_across_all_variants() {
        assert_eq!(ScarKind::Ask.tag(), "ask");
        assert_eq!(ScarKind::Reject.tag(), "reject");
        assert_eq!(ScarKind::Free.tag(), "free");
    }

    // --- M3: insert_scar ------------------------------------------

    use std::fs;
    use tempfile::TempDir;

    /// Helper: write `content` to `<tmp>/<name>` and return the path.
    fn write_tmp(dir: &TempDir, name: &str, content: &str) -> PathBuf {
        let path = dir.path().join(name);
        fs::write(&path, content).expect("write tmp file");
        path
    }

    #[test]
    fn insert_scar_drops_comment_one_line_above_target_in_rust_file() {
        let dir = TempDir::new().expect("tmp");
        let path = write_tmp(
            &dir,
            "main.rs",
            "fn main() {\n    let x = 1;\n    let y = 2;\n}\n",
        );
        insert_scar(&path, 3, ScarKind::Ask, "explain this change").expect("insert");
        let after = fs::read_to_string(&path).expect("read back");
        // Indent inherited from the target line (`    let y = 2;`).
        assert_eq!(
            after,
            "fn main() {\n    let x = 1;\n    // @kizu[ask]: explain this change\n    let y = 2;\n}\n"
        );
    }

    #[test]
    fn insert_scar_uses_python_hash_syntax_for_py_file() {
        let dir = TempDir::new().expect("tmp");
        let path = write_tmp(&dir, "app.py", "def main():\n    return 1\n");
        insert_scar(&path, 2, ScarKind::Free, "why?").expect("insert");
        let after = fs::read_to_string(&path).expect("read back");
        assert_eq!(
            after,
            "def main():\n    # @kizu[free]: why?\n    return 1\n"
        );
    }

    #[test]
    fn insert_scar_uses_html_block_syntax_for_html_file() {
        let dir = TempDir::new().expect("tmp");
        let path = write_tmp(&dir, "page.html", "<div>\n  <p>hi</p>\n</div>\n");
        insert_scar(&path, 2, ScarKind::Free, "check layout").expect("insert");
        let after = fs::read_to_string(&path).expect("read back");
        assert_eq!(
            after,
            "<div>\n  <!-- @kizu[free]: check layout -->\n  <p>hi</p>\n</div>\n"
        );
    }

    #[test]
    fn insert_scar_preserves_crlf_line_endings() {
        let dir = TempDir::new().expect("tmp");
        let path = write_tmp(&dir, "main.rs", "fn a() {}\r\nfn b() {}\r\n");
        insert_scar(&path, 2, ScarKind::Free, "look").expect("insert");
        let after = fs::read_to_string(&path).expect("read back");
        assert_eq!(after, "fn a() {}\r\n// @kizu[free]: look\r\nfn b() {}\r\n");
    }

    #[test]
    fn insert_scar_preserves_lf_line_endings_when_no_crlf_present() {
        let dir = TempDir::new().expect("tmp");
        let path = write_tmp(&dir, "main.rs", "fn a() {}\nfn b() {}\n");
        insert_scar(&path, 2, ScarKind::Free, "look").expect("insert");
        let after = fs::read_to_string(&path).expect("read back");
        assert_eq!(after, "fn a() {}\n// @kizu[free]: look\nfn b() {}\n");
    }

    #[test]
    fn insert_scar_is_idempotent_when_same_scar_is_already_above_target() {
        let dir = TempDir::new().expect("tmp");
        let path = write_tmp(
            &dir,
            "main.rs",
            "fn a() {}\n// @kizu[free]: look\nfn b() {}\n",
        );
        // Target is line 3 (`fn b()`). The line above already holds
        // the identical scar, so a second insert must be a no-op.
        insert_scar(&path, 3, ScarKind::Free, "look").expect("second insert");
        let after = fs::read_to_string(&path).expect("read back");
        assert_eq!(after, "fn a() {}\n// @kizu[free]: look\nfn b() {}\n");
    }

    #[test]
    fn insert_scar_with_line_number_1_prepends_to_file_start() {
        let dir = TempDir::new().expect("tmp");
        let path = write_tmp(&dir, "main.rs", "fn a() {}\nfn b() {}\n");
        insert_scar(&path, 1, ScarKind::Free, "root").expect("insert");
        let after = fs::read_to_string(&path).expect("read back");
        assert_eq!(after, "// @kizu[free]: root\nfn a() {}\nfn b() {}\n");
    }

    #[test]
    fn insert_scar_clamps_line_number_past_file_end_to_file_end() {
        let dir = TempDir::new().expect("tmp");
        let path = write_tmp(&dir, "main.rs", "fn a() {}\nfn b() {}\n");
        // Line 999 does not exist; scar should land at the tail.
        insert_scar(&path, 999, ScarKind::Free, "tail").expect("insert");
        let after = fs::read_to_string(&path).expect("read back");
        assert_eq!(after, "fn a() {}\nfn b() {}\n// @kizu[free]: tail\n");
    }

    #[test]
    fn insert_scar_errors_gracefully_on_missing_file() {
        let dir = TempDir::new().expect("tmp");
        let ghost = dir.path().join("nope.rs");
        let err = insert_scar(&ghost, 1, ScarKind::Free, "x").expect_err("missing file");
        let message = format!("{err:#}");
        assert!(
            message.contains("reading"),
            "error should mention the read phase, got: {message}"
        );
    }

    #[test]
    fn insert_scar_at_eof_without_trailing_newline_adds_separator() {
        let dir = TempDir::new().expect("tmp");
        // File ends without a trailing newline.
        let path = write_tmp(&dir, "main.rs", "fn a() {}");
        insert_scar(&path, 999, ScarKind::Ask, "eof").expect("insert");
        let after = fs::read_to_string(&path).expect("read back");
        // The scar must be on its own line, not spliced into "fn a() {}".
        assert_eq!(after, "fn a() {}\n// @kizu[ask]: eof\n");
    }

    #[test]
    fn insert_scar_inherits_leading_whitespace_of_target_line() {
        // The scar should match the indentation of the line it
        // comments on, so it reads like a real inline comment rather
        // than a loose left-edge annotation.
        let dir = TempDir::new().expect("tmp");
        let path = write_tmp(
            &dir,
            "main.rs",
            "fn main() {\n    let x = 1;\n    let y = 2;\n}\n",
        );
        insert_scar(&path, 2, ScarKind::Ask, "why one?")
            .expect("insert")
            .expect("receipt");
        let after = fs::read_to_string(&path).expect("read back");
        assert_eq!(
            after,
            "fn main() {\n    // @kizu[ask]: why one?\n    let x = 1;\n    let y = 2;\n}\n"
        );
    }

    #[test]
    fn insert_scar_inherits_tab_indent() {
        let dir = TempDir::new().expect("tmp");
        let path = write_tmp(&dir, "main.go", "func main() {\n\tfmt.Println()\n}\n");
        insert_scar(&path, 2, ScarKind::Free, "n")
            .expect("insert")
            .expect("receipt");
        let after = fs::read_to_string(&path).expect("read back");
        assert_eq!(
            after,
            "func main() {\n\t// @kizu[free]: n\n\tfmt.Println()\n}\n"
        );
    }

    #[test]
    fn insert_scar_skips_blank_target_and_uses_nearest_non_blank_indent() {
        // A blank target line gives no indentation to inherit. Use the
        // *following* non-blank line's indent (or the previous one if
        // there isn't any).
        let dir = TempDir::new().expect("tmp");
        let path = write_tmp(&dir, "main.rs", "fn main() {\n\n    let x = 1;\n}\n");
        // Target line 2 (the blank). We expect the scar to be inserted
        // above the blank, using the next non-blank's indent ("    ").
        insert_scar(&path, 2, ScarKind::Ask, "up")
            .expect("insert")
            .expect("receipt");
        let after = fs::read_to_string(&path).expect("read back");
        assert_eq!(
            after,
            "fn main() {\n    // @kizu[ask]: up\n\n    let x = 1;\n}\n"
        );
    }

    #[test]
    fn remove_scar_tolerates_indented_scar_line() {
        // After the indent change `remove_scar`'s `expected` string
        // and the on-disk line both carry the same leading whitespace.
        // The receipt stores the exact indented line, so undo still
        // matches byte-for-byte (trimmed comparison).
        let dir = TempDir::new().expect("tmp");
        let path = write_tmp(&dir, "main.rs", "fn a() {\n    let x = 1;\n}\n");
        let receipt = insert_scar(&path, 2, ScarKind::Ask, "why?")
            .expect("insert")
            .expect("receipt");
        let outcome = remove_scar(&path, receipt.line_1indexed, &receipt.rendered).expect("remove");
        assert_eq!(outcome, ScarRemove::Removed);
        let after = fs::read_to_string(&path).expect("read back");
        assert_eq!(after, "fn a() {\n    let x = 1;\n}\n");
    }

    #[test]
    fn insert_scar_respects_unknown_extension_by_falling_back_to_hash() {
        let dir = TempDir::new().expect("tmp");
        let path = write_tmp(&dir, "notes.zzz", "first line\nsecond line\n");
        insert_scar(&path, 2, ScarKind::Free, "n").expect("insert");
        let after = fs::read_to_string(&path).expect("read back");
        assert_eq!(after, "first line\n# @kizu[free]: n\nsecond line\n");
    }

    #[test]
    fn insert_scar_returns_receipt_with_post_insert_line_number() {
        let dir = TempDir::new().expect("tmp");
        let path = write_tmp(&dir, "main.rs", "fn a() {}\nfn b() {}\n");
        // Target line 2 → scar goes above `fn b`. Post-insert the scar
        // line is at 1-indexed position 2; `fn b()` shifts to 3.
        let receipt = insert_scar(&path, 2, ScarKind::Ask, "look")
            .expect("insert")
            .expect("receipt on actual write");
        assert_eq!(receipt.line_1indexed, 2);
        assert_eq!(receipt.rendered, "// @kizu[ask]: look");
    }

    #[test]
    fn insert_scar_returns_none_when_idempotent_noop() {
        let dir = TempDir::new().expect("tmp");
        let path = write_tmp(
            &dir,
            "main.rs",
            "fn a() {}\n// @kizu[free]: look\nfn b() {}\n",
        );
        let outcome = insert_scar(&path, 3, ScarKind::Free, "look").expect("second insert");
        assert!(outcome.is_none(), "idempotent path should report no-op");
    }

    #[test]
    fn remove_scar_deletes_line_when_expected_matches() {
        let dir = TempDir::new().expect("tmp");
        let path = write_tmp(
            &dir,
            "main.rs",
            "fn a() {}\n// @kizu[ask]: look\nfn b() {}\n",
        );
        let outcome = remove_scar(&path, 2, "// @kizu[ask]: look").expect("remove");
        assert_eq!(outcome, ScarRemove::Removed);
        let after = fs::read_to_string(&path).expect("read back");
        assert_eq!(after, "fn a() {}\nfn b() {}\n");
    }

    #[test]
    fn remove_scar_refuses_when_user_edited_the_line() {
        let dir = TempDir::new().expect("tmp");
        let path = write_tmp(
            &dir,
            "main.rs",
            "fn a() {}\n// @kizu[ask]: edited by user\nfn b() {}\n",
        );
        let outcome = remove_scar(&path, 2, "// @kizu[ask]: look").expect("remove");
        assert_eq!(outcome, ScarRemove::Mismatch);
        // File untouched.
        let after = fs::read_to_string(&path).expect("read back");
        assert_eq!(
            after,
            "fn a() {}\n// @kizu[ask]: edited by user\nfn b() {}\n"
        );
    }

    #[test]
    fn remove_scar_reports_out_of_range_when_file_was_truncated() {
        let dir = TempDir::new().expect("tmp");
        let path = write_tmp(&dir, "main.rs", "fn a() {}\n");
        let outcome = remove_scar(&path, 5, "anything").expect("remove");
        assert_eq!(outcome, ScarRemove::OutOfRange);
    }

    #[test]
    fn remove_scar_preserves_crlf_endings_of_surrounding_lines() {
        let dir = TempDir::new().expect("tmp");
        let path = write_tmp(
            &dir,
            "main.rs",
            "fn a() {}\r\n// @kizu[free]: look\r\nfn b() {}\r\n",
        );
        let outcome = remove_scar(&path, 2, "// @kizu[free]: look").expect("remove");
        assert_eq!(outcome, ScarRemove::Removed);
        let after = fs::read_to_string(&path).expect("read back");
        assert_eq!(after, "fn a() {}\r\nfn b() {}\r\n");
    }

    #[test]
    fn insert_then_remove_using_receipt_round_trips() {
        let dir = TempDir::new().expect("tmp");
        let path = write_tmp(&dir, "main.rs", "fn a() {}\nfn b() {}\n");
        let before = fs::read_to_string(&path).expect("read before");
        let receipt = insert_scar(&path, 2, ScarKind::Ask, "look")
            .expect("insert")
            .expect("receipt");
        let outcome = remove_scar(&path, receipt.line_1indexed, &receipt.rendered).expect("remove");
        assert_eq!(outcome, ScarRemove::Removed);
        let after = fs::read_to_string(&path).expect("read after");
        assert_eq!(after, before);
    }

    /// A scar insert is a review annotation, not a code edit. The on-disk
    /// mtime must therefore survive the write so that kizu's mtime-sorted
    /// file list does not float the scarred file to the "latest" slot —
    /// the hunk would visibly jump to the bottom of the follow-mode
    /// viewport otherwise.
    #[test]
    fn insert_scar_preserves_file_mtime() {
        let dir = TempDir::new().expect("tmp");
        let path = write_tmp(&dir, "main.rs", "fn a() {}\nfn b() {}\n");
        // Back-date the file so "now" and "pre-insert mtime" are
        // distinguishable even on coarse-resolution filesystems.
        let pre = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let f = fs::File::options()
            .write(true)
            .open(&path)
            .expect("open for set_times");
        f.set_times(fs::FileTimes::new().set_modified(pre))
            .expect("set pre mtime");
        drop(f);

        insert_scar(&path, 2, ScarKind::Ask, "look")
            .expect("insert")
            .expect("receipt");

        let post = fs::metadata(&path)
            .expect("metadata")
            .modified()
            .expect("mtime");
        assert_eq!(
            post, pre,
            "scar insert should preserve the file's modified time"
        );
    }

    mod jsx_tsx_scar_placement {
        use super::*;

        #[test]
        fn insert_scar_uses_jsx_block_comment_for_jsx_children() {
            let dir = TempDir::new().expect("tmp");
            let path = write_tmp(
                &dir,
                "Counter.tsx",
                "export function Counter({ count }: { count: number }) {\n  return (\n    <section>\n      <p>Count: {count}</p>\n    </section>\n  );\n}\n",
            );

            let receipt = insert_scar(&path, 4, ScarKind::Ask, "explain this change")
                .expect("insert")
                .expect("receipt");

            let after = fs::read_to_string(&path).expect("read back");
            assert_eq!(
                after,
                "export function Counter({ count }: { count: number }) {\n  return (\n    <section>\n      {/* @kizu[ask]: explain this change */}\n      <p>Count: {count}</p>\n    </section>\n  );\n}\n"
            );
            assert_eq!(
                receipt.rendered,
                "      {/* @kizu[ask]: explain this change */}"
            );
        }

        #[test]
        fn insert_scar_uses_slash_slash_for_ts_expression() {
            let dir = TempDir::new().expect("tmp");
            let path = write_tmp(
                &dir,
                "Counter.tsx",
                "export function Counter({ count }: { count: number }) {\n  const label: string = String(count);\n  return <p>{label}</p>;\n}\n",
            );

            insert_scar(&path, 2, ScarKind::Ask, "explain this change")
                .expect("insert")
                .expect("receipt");

            let after = fs::read_to_string(&path).expect("read back");
            assert_eq!(
                after,
                "export function Counter({ count }: { count: number }) {\n  // @kizu[ask]: explain this change\n  const label: string = String(count);\n  return <p>{label}</p>;\n}\n"
            );
        }

        #[test]
        fn insert_scar_relocates_from_jsx_opening_tag_attribute() {
            let dir = TempDir::new().expect("tmp");
            let path = write_tmp(
                &dir,
                "ButtonPanel.tsx",
                "export function Panel() {\n  return (\n    <Button\n      kind=\"primary\"\n      onClick={() => save()}\n    >\n      Save\n    </Button>\n  );\n}\n",
            );

            let receipt = insert_scar(&path, 4, ScarKind::Ask, "explain this change")
                .expect("insert")
                .expect("receipt");

            let after = fs::read_to_string(&path).expect("read back");
            assert_eq!(
                after,
                "export function Panel() {\n  return (\n    {/* @kizu[ask]: explain this change */}\n    <Button\n      kind=\"primary\"\n      onClick={() => save()}\n    >\n      Save\n    </Button>\n  );\n}\n"
            );
            assert_eq!(receipt.line_1indexed, 3);
        }

        #[test]
        fn insert_scar_handles_jsx_fragment_child() {
            let dir = TempDir::new().expect("tmp");
            let path = write_tmp(
                &dir,
                "Fragment.tsx",
                "export function Fragment() {\n  return (\n    <>\n      <span>One</span>\n    </>\n  );\n}\n",
            );

            insert_scar(&path, 4, ScarKind::Free, "why here?")
                .expect("insert")
                .expect("receipt");

            let after = fs::read_to_string(&path).expect("read back");
            assert_eq!(
                after,
                "export function Fragment() {\n  return (\n    <>\n      {/* @kizu[free]: why here? */}\n      <span>One</span>\n    </>\n  );\n}\n"
            );
        }

        #[test]
        fn insert_scar_returns_error_when_tsx_parse_is_unrecoverable() {
            let dir = TempDir::new().expect("tmp");
            let path = write_tmp(
                &dir,
                "Broken.tsx",
                "export function Broken() {\n  return (\n    <section>\n      <p>Broken\n    </section>\n  );\n}\n",
            );

            let err = insert_scar(&path, 4, ScarKind::Ask, "explain this change")
                .expect_err("broken TSX should not receive a best-effort scar");
            let message = format!("{err:#}");
            assert!(
                message.contains("could not safely place JSX/TSX scar"),
                "error should explain safe placement failure, got: {message}"
            );
        }
    }

    /// Symmetric with `insert_scar_preserves_file_mtime`: undoing a scar
    /// (`u` key) must also leave mtime untouched, so that
    /// insert-then-remove round-trips are invisible to the mtime sort.
    #[test]
    fn remove_scar_preserves_file_mtime() {
        let dir = TempDir::new().expect("tmp");
        let path = write_tmp(
            &dir,
            "main.rs",
            "fn a() {}\n// @kizu[ask]: look\nfn b() {}\n",
        );
        let pre = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let f = fs::File::options()
            .write(true)
            .open(&path)
            .expect("open for set_times");
        f.set_times(fs::FileTimes::new().set_modified(pre))
            .expect("set pre mtime");
        drop(f);

        let outcome = remove_scar(&path, 2, "// @kizu[ask]: look").expect("remove");
        assert_eq!(outcome, ScarRemove::Removed);

        let post = fs::metadata(&path)
            .expect("metadata")
            .modified()
            .expect("mtime");
        assert_eq!(
            post, pre,
            "scar remove should preserve the file's modified time"
        );
    }
}
