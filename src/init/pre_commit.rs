use anyhow::Result;
use std::path::Path;

use super::{Scope, kizu_bin_for_scope};

/// Kizu-managed shim marker embedded in generated pre-commit hooks.
const KIZU_SHIM_MARKER: &str = "# kizu-managed-shim";

/// Wrap `s` in POSIX single quotes, escaping any interior single
/// quotes with the standard `'\''` sequence. Produces a token that
/// `sh` always parses as exactly one literal argument, regardless of
/// spaces, `$`, `"`, `\`, `*`, etc. Kizu binaries installed under
/// paths like `/Users/John Doe/.cargo/bin/kizu` would otherwise
/// wordsplit in the generated pre-commit shim.
///
/// Shared with [`crate::attach`] so the Ghostty `osascript` builder
/// reuses the same quoting contract.
pub(crate) fn shell_single_quote(s: &str) -> String {
    let escaped = s.replace('\'', r"'\''");
    format!("'{escaped}'")
}

/// Render the `/bin/sh` shim body that `.git/hooks/pre-commit`
/// writes. Extracted from `install_git_pre_commit_hook` so the
/// quoting contract can be unit-tested without touching the
/// filesystem.
pub(super) fn pre_commit_shim_body(bin: &str, has_user_hook: bool) -> String {
    let bin_q = shell_single_quote(bin);
    if has_user_hook {
        format!(
            "#!/bin/sh\n{KIZU_SHIM_MARKER}\nset -e\n\
             # Run the original user hook first.\n\
             \"$(dirname \"$0\")/pre-commit.user\" \"$@\"\n\
             # Then run kizu scar guard.\n\
             {bin_q} hook-pre-commit\n"
        )
    } else {
        format!(
            "#!/bin/sh\n{KIZU_SHIM_MARKER}\nset -e\n\
             # kizu scar guard\n\
             {bin_q} hook-pre-commit\n"
        )
    }
}

/// Install a kizu-managed pre-commit shim that guarantees
/// `kizu hook-pre-commit` always runs, even when the repo has a
/// pre-existing hook script that may contain `exit`/`exec`.
///
/// Strategy:
/// - **No existing hook**: write a simple shim.
/// - **Existing hook is already kizu-managed**: no-op.
/// - **Existing non-kizu hook**: rename it to `pre-commit.user`,
///   then write a shim that calls the original *and* kizu. Both
///   must succeed (fail-fast with `set -e`).
pub(super) fn install_git_pre_commit_hook(project_root: &Path) -> Result<()> {
    let git_dir = crate::git::git_dir(project_root)?;
    let hooks_dir = git_dir.join("hooks");
    std::fs::create_dir_all(&hooks_dir)?;
    let hook_path = hooks_dir.join("pre-commit");

    if hook_path.exists() {
        let content = std::fs::read_to_string(&hook_path)?;
        if content.contains(KIZU_SHIM_MARKER) {
            println!("  git pre-commit hook: already installed");
            return Ok(());
        }
        // Existing non-kizu hook → rename and wrap.
        let user_hook = hooks_dir.join("pre-commit.user");
        if user_hook.exists() {
            anyhow::bail!(
                "cannot install pre-commit shim: backup path already exists at {}\n\
                 Remove or rename it manually, then re-run `kizu init`.",
                user_hook.display()
            );
        }
        std::fs::rename(&hook_path, &user_hook)?;
        let bin = kizu_bin_for_scope(Scope::ProjectLocal);
        let shim = pre_commit_shim_body(&bin, true);
        std::fs::write(&hook_path, shim)?;
        println!(
            "  git pre-commit hook: wrapped existing hook → {}",
            user_hook.display()
        );
    } else {
        let bin = kizu_bin_for_scope(Scope::ProjectLocal);
        let shim = pre_commit_shim_body(&bin, false);
        std::fs::write(&hook_path, shim)?;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook_path, std::fs::Permissions::from_mode(0o755))?;
    }

    println!(
        "  git pre-commit hook: installed at {}",
        hook_path.display()
    );
    Ok(())
}

pub(super) fn remove_git_pre_commit_hook(project_root: &Path) -> Result<bool> {
    let git_dir = match crate::git::git_dir(project_root) {
        Ok(d) => d,
        Err(_) => return Ok(false),
    };
    let hooks_dir = git_dir.join("hooks");
    let hook_path = hooks_dir.join("pre-commit");
    if !hook_path.exists() {
        return Ok(false);
    }
    let content = std::fs::read_to_string(&hook_path)?;
    if !content.contains("kizu hook-pre-commit") && !content.contains(KIZU_SHIM_MARKER) {
        return Ok(false);
    }

    // Remove the kizu shim.
    std::fs::remove_file(&hook_path)?;

    // Restore the original user hook if it was renamed by install.
    let user_hook = hooks_dir.join("pre-commit.user");
    if user_hook.exists() {
        std::fs::rename(&user_hook, &hook_path)?;
    }

    Ok(true)
}
