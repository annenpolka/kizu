use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// Shared, runtime-mutable handle to the baseline path set. Every
/// debouncer callback holds a clone of this `Arc` and read-locks on
/// each event; the app layer can hot-swap the inner value through
/// [`WatchHandle::update_current_branch_ref`].
pub(crate) type SharedMatcher = Arc<RwLock<BaselineMatcherInner>>;

/// Set of git-dir paths that, when touched, genuinely indicate the
/// session baseline SHA has drifted. Captured at watcher startup
/// **and refreshed at runtime** whenever `R` discovers a new
/// symbolic HEAD (ADR-0008). Paths are canonicalized so byte
/// comparisons work across symlinked tempdirs (e.g. macOS
/// `/var/folders` → `/private/var/folders`).
#[derive(Debug, Clone)]
pub(crate) struct BaselineMatcherInner {
    /// `<per-worktree git_dir>/HEAD` — moves on `git checkout`, or
    /// on reseating HEAD to a different branch via `symbolic-ref`.
    head_file: PathBuf,
    /// `<common git_dir>/refs/heads/<current branch>` — moves on
    /// `git commit`, `git reset`, or any direct ref write. `None`
    /// when HEAD is detached: in that case the session baseline is
    /// a raw SHA and only `head_file` can move it (via checkout).
    branch_ref: Option<PathBuf>,
    /// `<common git_dir>/packed-refs` — touched when loose refs get
    /// packed, which can atomically replace the loose branch ref
    /// file with an entry inside packed-refs. Tracking this catches
    /// the corner case where a `git pack-refs` happens between two
    /// HEAD movements.
    packed_refs: PathBuf,
}

impl BaselineMatcherInner {
    pub(crate) fn new(
        git_dir: &Path,
        common_git_dir: &Path,
        current_branch_ref: Option<&str>,
    ) -> Self {
        let head_file = canonicalize_or_self(&git_dir.join("HEAD"));
        let branch_ref = current_branch_ref.map(|r| {
            // `r` looks like `refs/heads/foo/bar` — split on `/` and
            // join to preserve nested branch names on platforms where
            // path joining with a multi-segment string works differently.
            let mut p = common_git_dir.to_path_buf();
            for segment in r.split('/') {
                p.push(segment);
            }
            canonicalize_or_self(&p)
        });
        let packed_refs = canonicalize_or_self(&common_git_dir.join("packed-refs"));
        Self {
            head_file,
            branch_ref,
            packed_refs,
        }
    }

    pub(crate) fn matches(&self, path: &Path) -> bool {
        let p = canonicalize_or_self(path);
        p == self.head_file
            || self.branch_ref.as_ref().is_some_and(|r| p == *r)
            || p == self.packed_refs
    }
}

pub(crate) fn canonicalize_or_self(p: &Path) -> PathBuf {
    if let Ok(canonical) = p.canonicalize() {
        return canonical;
    }

    // Some paths we must compare against legitimately do not exist yet
    // when the watcher starts: a freshly checked-out branch can create
    // `refs/heads/<branch>` after startup, and packed-refs can be born
    // later via `git pack-refs`. Canonicalizing only existing ancestors
    // keeps symlinked temp roots (`/var` vs `/private/var`) stable while
    // preserving the not-yet-created tail we still need to match.
    let mut missing_tail = Vec::new();
    let mut cursor = p;
    while let Some(parent) = cursor.parent() {
        let Some(name) = cursor.file_name() else {
            break;
        };
        missing_tail.push(name.to_os_string());
        if let Ok(mut canonical_parent) = parent.canonicalize() {
            for segment in missing_tail.iter().rev() {
                canonical_parent.push(segment);
            }
            return canonical_parent;
        }
        cursor = parent;
    }

    p.to_path_buf()
}
