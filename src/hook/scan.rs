use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use super::ScarHit;

/// Compiled once per process. Hook subcommands fire on every
/// PostToolUse / Stop invocation, so recompiling this regex per call
/// would bill the cost to Claude Code's tool-use latency.
static SCAR_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^\s*(?://|#|--|/\*|<!--|\{/\*)\s*@kizu\[(\w+)\]:\s*(.*)")
        .expect("scar regex")
});

/// Grep every file in `paths` for `@kizu[...]` scars. Returns all
/// hits across all files, in order. Files that cannot be read (e.g.
/// deleted between the scan and the grep) are silently skipped.
///
/// Only matches scars that appear as the primary content of a
/// comment line (preceded by `//`, `#`, `--`, `/*`, or `<!--`
/// after optional whitespace). This avoids false positives from
/// string literals in test code that happen to contain `@kizu[`.
pub fn scan_scars(paths: &[PathBuf]) -> Vec<ScarHit> {
    let mut hits = Vec::new();
    for path in paths {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let fence_aware = is_fenced_code_aware(path);
        hits.extend(scan_content_for_scars(
            &SCAR_RE,
            &content,
            path,
            fence_aware,
        ));
    }
    hits
}

/// Returns `true` for file extensions where fenced code blocks can
/// contain scar-like examples that should not be treated as live scars.
fn is_fenced_code_aware(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| matches!(ext, "md" | "txt" | "rst" | "adoc"))
}

/// Scan `content` for scar hits, skipping matches inside Markdown
/// fenced code blocks (`` ``` `` / `~~~`) when `fence_aware` is true.
fn scan_content_for_scars(
    re: &regex::Regex,
    content: &str,
    path: &Path,
    fence_aware: bool,
) -> Vec<ScarHit> {
    let mut hits = Vec::new();
    let mut in_fence = false;
    for (i, line) in content.lines().enumerate() {
        if fence_aware {
            let trimmed = line.trim_start();
            if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
                in_fence = !in_fence;
                continue;
            }
            if in_fence {
                continue;
            }
        }
        if let Some(caps) = re.captures(line) {
            hits.push(ScarHit {
                path: path.to_path_buf(),
                line_number: i + 1,
                kind: caps[1].to_string(),
                message: trim_scar_message(&caps[2]),
            });
        }
    }
    hits
}

fn trim_scar_message(raw: &str) -> String {
    let mut message = raw.trim();
    for suffix in ["*/}", "*/", "-->"] {
        if let Some(stripped) = message.strip_suffix(suffix) {
            message = stripped.trim_end();
            break;
        }
    }
    message.to_string()
}

/// Grep staged (index) contents of `paths` for `@kizu[...]` scars.
/// Unlike [`scan_scars`] which reads the worktree, this reads the
/// staged blob via `git show :<path>` so that worktree edits after
/// staging don't mask scars that are actually about to be committed.
pub fn scan_scars_from_index(root: &Path, paths: &[PathBuf]) -> Vec<ScarHit> {
    let mut hits = Vec::new();
    for path in paths {
        let rel = match path.strip_prefix(root) {
            Ok(r) => r,
            Err(_) => path.as_path(),
        };
        let output = match std::process::Command::new("git")
            .args(["show", &format!(":{}", rel.display())])
            .current_dir(root)
            .output()
        {
            Ok(o) if o.status.success() => o,
            _ => continue,
        };
        let content = String::from_utf8_lossy(&output.stdout);
        let fence_aware = is_fenced_code_aware(path);
        hits.extend(scan_content_for_scars(
            &SCAR_RE,
            &content,
            path,
            fence_aware,
        ));
    }
    hits
}
