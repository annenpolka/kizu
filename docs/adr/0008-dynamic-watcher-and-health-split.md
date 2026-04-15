# ADR-0008: Dynamic watcher reconfiguration + health decoupled from diff errors

- **Status**: Accepted
- **Date**: 2026-04-15
- **Deciders**: annenpolka, Claude (Codex adversarial review loop, round 3)
- **Supersedes (Consequences section of)**: [ADR-0007](0007-visual-row-scroll-and-session-scoped-watcher.md)

## Context

ADR-0007 introduced two things in the same PR:

1. `BaselineMatcher` captured at watcher startup to narrow `GitHead` detection to the session's specific branch ref.
2. `WatchEvent::Error(String)` → `App.last_error` + forced recompute so backend failures would no longer silently drift the UI.

Codex round-3 adversarial review found two emergent bugs that are actually **interaction failures between those two changes and the rest of the loop**, not fresh code defects:

1. **`R` re-baselines the SHA but leaves the watcher pinned to the startup branch** (`src/app.rs:505-514`).
   `BaselineMatcher` was captured once, by value, and moved into each debouncer closure. `reset_baseline()` in ADR-0007 intentionally kept this "frozen" as a tradeoff (see 0007 Consequences). But in practice a long-running session often looks like: (a) start kizu on branch A, (b) `git checkout B` in another terminal, (c) press `R` to re-baseline. After that sequence the watcher still matches on `refs/heads/A`. New commits on B update `refs/heads/B`, the matcher ignores them, `GitHead` never fires, `head_dirty` stops tracking drift. The monitoring contract silently dies.

2. **Watcher backend failures are erased by a successful one-off recompute** (`src/app.rs:1401-1413`).
   ADR-0007's fix wrote watcher backend errors into `last_error` and forced a recompute. But `apply_computed_files()` on the success path clears `last_error` (that's its job for transient diff errors). So the sequence (a) FSEvents drops, (b) `WatchEvent::Error("…")`, (c) forced recompute succeeds → leaves `watcher_health = dead but last_error = cleared`. The UI silently claims everything is fine and returns to stale-snapshot behavior — exactly the failure mode ADR-0007 claimed to eliminate, just harder to notice.

A third related finding is independent of 0007 but shares the same "state assumption vs reality" shape:

3. **`compute_diff()` decodes git stdout with strict `String::from_utf8`** (`src/git.rs:75-88`).
   A single tracked file in a legacy encoding (Shift-JIS, Latin-1, …) makes the whole refresh fail and freezes the UI on stale data. This is especially asymmetric because the untracked path already preserves non-UTF-8 bytes literally.

The three findings share a common shape: the previous design treated inherently *dynamic* properties of the session — the current branch, the liveness of the filesystem backend, the encoding of an individual file — as static invariants captured at a single point in time.

## Decision

1. **Make `BaselineMatcher` runtime-mutable through shared ownership.**
   Replace the by-value `BaselineMatcher` struct with `SharedMatcher = Arc<RwLock<BaselineMatcherInner>>`. Every debouncer closure clones the `Arc`, read-locks on each event. `WatchHandle` stores the same `Arc` plus the `git_dir` / `common_git_dir` needed to rebuild the inner, and exposes `WatchHandle::update_current_branch_ref(Option<&str>)` which write-locks and hot-swaps the inner without touching the debouncers or losing the event queue.

   App layer propagates branch changes via a new `KeyEffect` enum returned from `handle_key`. `reset_baseline()` re-resolves the symbolic HEAD ref, updates `App.current_branch_ref` if it differs, and returns `KeyEffect::ReconfigureWatcher`. The event loop's key arm reads the effect and calls `watch.update_current_branch_ref(...)`. Failed resets return `KeyEffect::None` so the watcher never points at a branch the user did not actually reach.

   This supersedes ADR-0007's "session frozen" trade-off. ADR-0007's Consequences section noted that "sessions-long checkout to a new branch is the user's responsibility to re-baseline" — that framing papered over the fact that re-baselining was incomplete because it never touched the watcher. With dynamic matcher reconfiguration, `R` becomes a true reset and the assumption is lifted.

2. **Decouple watcher health from diff error state via `WatcherHealth` enum.**
   Add `App.watcher_health: WatcherHealth` (default `Healthy`). `WatchEvent::Error(msg)` writes `Failed(msg)` here rather than into `last_error`. The footer renders `⚠ WATCHER` with precedence over the diff error marker so a dead backend cannot be masked by a successful recompute.

   Add a recovery signal: any non-Error watcher event arriving after `Failed` flips the state back to `Healthy` — direct evidence that the debouncer is still producing signals. If a coalesced burst contains both live events *and* an Error, the pessimistic state wins (`Failed`) because we can't prove the error is past.

   Extract the coalescing + health transitions into `App::handle_watch_burst(events)` so the rules stay testable without a real debouncer. `run_loop` drains events into a `Vec<WatchEvent>` and hands them to this method.

3. **Lossy-decode `git diff` stdout in `compute_diff`.**
   Replace `String::from_utf8(output.stdout)?` with `String::from_utf8_lossy(&output.stdout)`. Invalid UTF-8 bytes become U+FFFD in the display; the refresh itself stays alive. This matches the tracked path to the byte-oriented untracked handling already in place (`bytes_to_path`, `list_untracked` with `-z`).

## Consequences

- **Positive**:
  - `R` after `git checkout` correctly refreshes the watcher's branch tracking. New commits on the new branch raise `GitHead` as expected; the monitoring contract holds for the entire session lifetime, not just until the first checkout.
  - Attach → detach (and back) transitions are handled symmetrically by the same code path: `Option<String>` comparison catches the state change regardless of direction.
  - Watcher health is now a first-class, persistent state. A dropped FSEvents queue shows `⚠ WATCHER` in the footer and stays there until a real recovery signal lands. A `git diff` error still uses `last_error` and can be cleared independently, so the two failure modes never shadow each other.
  - Non-UTF-8 tracked files no longer block refreshes. The tool stays live through legacy-encoded fixtures, which is the common "I opened kizu on a repo with one weird binary-ish file" failure pattern.
  - `KeyEffect` gives future key handlers a clean side-effect channel without threading more `&mut watch` parameters into `handle_key`. New variants (e.g. process spawning, shell-out) can land without touching every handler method.
  - The watcher event handling is now testable without a real debouncer via `App::handle_watch_burst`. Three new unit tests pin the health-vs-error invariant.
- **Negative**:
  - `WatchHandle` now carries shared state (`Arc<RwLock<_>>`) instead of being a pure destructor-only wrapper. Slightly more surface area, but the lock is read-heavy (one write per `R` that changes branches, many reads per second at worst under file churn) and contention is effectively zero.
  - Every watcher event incurs an `Arc::clone` and a `read()` unlock. Measured contention is negligible at the 2000-row / 100-300 ms debounce cap.
  - The health state machine now has an edge case: if the debouncer keeps producing Error events without interspersed live events, recovery never fires and the UI stays in `Failed` forever. This is correct — persistent backend failure should surface persistently. Manual `R` (which forces a recompute) does not by itself clear `watcher_health`; only a live event or a restart does.
  - Lossy decode hides encoding errors behind U+FFFD markers in the rendered diff. A non-UTF-8 byte inside a tracked file will look mangled in the viewer. Accepting this is the right trade-off: the alternative (strict decode, refresh fails, user blind) is strictly worse.
- **Scope**:
  - `src/watcher.rs`: `SharedMatcher` type alias, `BaselineMatcherInner` renamed, `WatchHandle::update_current_branch_ref`, read-locking in the git-dir callback
  - `src/app.rs`: `WatcherHealth` enum, `KeyEffect` enum, `handle_key` / `handle_normal_key` return KeyEffect, `reset_baseline` re-resolves branch and returns effect, `apply_reset` takes `new_branch`, `handle_watch_burst` extracted, run_loop's key and watcher arms route through the new plumbing, `apply_key_effect` helper
  - `src/ui.rs`: footer renders `⚠ WATCHER` with precedence
  - `src/git.rs`: `compute_diff` lossy decode

## Alternatives Considered

- **Finding 1: rebuild the entire watcher on `R`**. Rejected: drops the event queue during the rebuild window and loses any events that arrive mid-swap. Hot-swapping via `Arc<RwLock<_>>` is cleaner and zero-downtime. The rebuild approach also does not help with the subsidiary question of how to propagate the change from `App` to the watcher — it still needs either `&mut WatchHandle` access or an effect channel.
- **Finding 1: poll `rev-parse HEAD` on every `GitHead` event to decide if the baseline actually moved**. Rejected: adds a `git` shell-out to every event and does not address the ADR-0007 narrowing. The whole point of `BaselineMatcher` is to filter *before* invoking git. Keeping the filter static while querying git every time defeats both.
- **Finding 2: keep watcher errors in `last_error` but tag them with a "sticky" flag**. Rejected: smears two different concerns (transient compute errors vs backend liveness) into one field and makes the UI footer logic progressively more tangled. A separate `watcher_health` enum is the minimal clean split.
- **Finding 2: treat every non-Error event as recovery unconditionally**. Rejected: a coalesced burst containing both live events and an Error must leave the state Failed, because we can't prove the error is past. The precedence rule is baked into `handle_watch_burst`.
- **Finding 3: byte-oriented diff parsing instead of lossy decode**. Rejected: rewriting `parse_unified_diff` against raw bytes is invasive and the user-visible benefit is tiny (U+FFFD shows up instead of reading bytes as their legacy encoding). Lossy decode is two lines and a test. If a future requirement needs round-trip-accurate non-UTF-8 content, the parser can be promoted then.

## References

- 関連 ADR: [ADR-0005](0005-watcher-coalescing-no-ignore-filter.md) (common git dir watching), [ADR-0007](0007-visual-row-scroll-and-session-scoped-watcher.md) (session-scoped matcher, wrap scroll)
- Codex adversarial review round 3 (`bbwqexj44`, 2026-04-15)
- 関連仕様: `docs/SPEC.md` — 監視対象, `R` semantics
