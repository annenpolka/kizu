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
    // Wired up in the later M4 slice that adds the `c` free-text
    // input mode. Until then `Free` is exercised only by the
    // render / insert unit tests, so dead_code would otherwise
    // fire — explicit allow keeps the variant visible in the
    // API surface without hiding it behind a conditional compile.
    #[allow(dead_code)]
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
/// Returns an error when the file cannot be read (missing,
/// permission-denied, non-UTF-8 in v0.2 scope) or the write-back
/// fails (read-only mount, disk full). The caller is expected to
/// surface this error through `App.last_error` rather than panic.
pub fn insert_scar(path: &Path, line_number: usize, kind: ScarKind, body: &str) -> Result<()> {
    let syntax = detect_comment_syntax(path);
    let scar_body = syntax.render_scar(kind, body);
    let original = std::fs::read_to_string(path)
        .with_context(|| format!("reading {} for scar insertion", path.display()))?;
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

    // Idempotent guard: if the line immediately above the insertion
    // point is already the same scar, leave the file untouched so
    // repeated keypresses don't stack duplicates.
    if insert_at > 0 {
        let prev_raw = lines[insert_at - 1];
        let prev_trimmed = prev_raw.trim_end_matches('\n').trim_end_matches('\r');
        if prev_trimmed.trim() == scar_body.trim() {
            return Ok(());
        }
    }

    let mut out = String::with_capacity(original.len() + scar_body.len() + newline.len() * 2);
    for line in &lines[..insert_at] {
        out.push_str(line);
    }
    // If appending after a line that lacks a trailing newline (e.g.
    // EOF without final newline), add one so the scar starts on its
    // own line instead of splicing into the previous content.
    if insert_at > 0 && !lines[insert_at - 1].ends_with('\n') {
        out.push_str(newline);
    }
    out.push_str(&scar_body);
    out.push_str(newline);
    for line in &lines[insert_at..] {
        out.push_str(line);
    }

    std::fs::write(path, out)
        .with_context(|| format!("writing {} with scar inserted", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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
        assert_eq!(
            after,
            "fn main() {\n    let x = 1;\n// @kizu[ask]: explain this change\n    let y = 2;\n}\n"
        );
    }

    #[test]
    fn insert_scar_uses_python_hash_syntax_for_py_file() {
        let dir = TempDir::new().expect("tmp");
        let path = write_tmp(&dir, "app.py", "def main():\n    return 1\n");
        insert_scar(&path, 2, ScarKind::Free, "why?").expect("insert");
        let after = fs::read_to_string(&path).expect("read back");
        assert_eq!(after, "def main():\n# @kizu[free]: why?\n    return 1\n");
    }

    #[test]
    fn insert_scar_uses_html_block_syntax_for_html_file() {
        let dir = TempDir::new().expect("tmp");
        let path = write_tmp(&dir, "page.html", "<div>\n  <p>hi</p>\n</div>\n");
        insert_scar(&path, 2, ScarKind::Free, "check layout").expect("insert");
        let after = fs::read_to_string(&path).expect("read back");
        assert_eq!(
            after,
            "<div>\n<!-- @kizu[free]: check layout -->\n  <p>hi</p>\n</div>\n"
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
    fn insert_scar_respects_unknown_extension_by_falling_back_to_hash() {
        let dir = TempDir::new().expect("tmp");
        let path = write_tmp(&dir, "notes.zzz", "first line\nsecond line\n");
        insert_scar(&path, 2, ScarKind::Free, "n").expect("insert");
        let after = fs::read_to_string(&path).expect("read back");
        assert_eq!(after, "first line\n# @kizu[free]: n\nsecond line\n");
    }
}
