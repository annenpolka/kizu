# ADR-0009: Visual cursor sub-row, self-consistent sticky header, content-row follow target

- **Status**: Accepted
- **Date**: 2026-04-15
- **Deciders**: annenpolka, Claude (Codex adversarial review loop, round 4)
- **Extends**: [ADR-0007](0007-visual-row-scroll-and-session-scoped-watcher.md)

## Context

ADR-0007 migrated the render layer to a visual-row coordinate system via `VisualIndex`, but kept `app.scroll` as a **logical row index**. That compromise was meant to keep existing logical-row tests numerically stable. Codex round-4 adversarial review found three bugs that all trace back to the same place: **the navigation and follow layers never moved from logical to visual coordinates, so any wrap-mode operation that needs sub-row precision silently degrades to a no-op or hides content**.

The three findings:

1. **Wrap-mode scrolling collapses back to the same logical row inside a long wrapped line** (`src/app.rs:879-889`). `scroll_by(delta)` in wrap mode computes a visual target y, calls `VisualIndex::logical_at(y)`, and **discards the intra-row sub-offset**. Any target landing inside the same wrapped logical line resolves to the same logical row, `scroll_to` treats it as a no-op, the cursor can't walk through minified JSON / single-line edits / any very long diff line. The same lossy conversion appears in `next_change` / `prev_change` long-hunk branches.

2. **Follow mode targets the newest hunk's header row, not its last visible change** (`src/app.rs:1139-1156`). `follow_target_row` returns `layout.hunk_starts.last()`, which is a `HunkHeader` row. When the newest hunk is taller than the viewport, follow pins the view to the top of the hunk and the actual newest added/deleted lines live off-screen. The "tail -f"-style monitoring metaphor that is kizu's reason to exist **breaks exactly at the moment large edits land**. This was a latent bug older than ADR-0007 — the test coverage simply never exercised the `hunk_end > viewport_height` case.

3. **Sticky-header eligibility is computed against the wrong viewport height** (`src/ui.rs:72-111`). The previous flow computed a provisional top with the *full* `area.height`, decided stickiness from it, and only then reserved a row for the sticky banner and recomputed the top. At boundaries — especially in the wrap / long-hunk cases where `placement_target_visual_y` depends on `viewport_height` — the recomputed top could diverge from the provisional one, so the sticky decision was occasionally based on a viewport the renderer was not actually going to draw. The observable symptom is a disappearing hunk header.

Finding 1 is ADR-0007's unfinished migration coming due. Finding 2 is a latent bug exposed by the same adversarial pressure. Finding 3 is partially a consequence of ADR-0007's visual placement (since `placement_target_visual_y(h)` is now height-sensitive in ways it wasn't before) and partially a latent two-pass-non-convergence. All three are the same shape: **coordinate systems that should have been aligned diverge at a boundary**, and the tests that pinned behavior at the center of the coordinate space didn't catch it.

## Decision

1. **Promote the cursor to a (logical_row, visual_sub_row) pair.** Add `App.cursor_sub_row: usize` (default 0) alongside `App.scroll`. Its meaning: "how many visual lines into the logical row at `scroll` the cursor has walked via Ctrl-d / Ctrl-u / J / K". Always 0 in nowrap mode and after any "jump to row" operation. The cursor's visual y is `VisualIndex::visual_y(scroll) + cursor_sub_row`, and `placement_target_visual_y` uses that sum when deciding where to park the viewport.

   Introduce `App::scroll_to_visual(row, sub_row, &VisualIndex)` for wrap-aware intra-row motion. It clamps `sub_row` to the target row's visual height, starts a fresh scroll animation when either component actually changes, and updates the anchor. `scroll_by` in wrap mode routes through it; `next_change` / `prev_change` long-hunk branches route through it; the hunk-end detection in those branches now compares against the hunk's visual *last line*, not its last logical row.

   `scroll_to(row)` remains the "jump to row" entry point and always sets `cursor_sub_row = 0`. `toggle_wrap_lines` resets `cursor_sub_row` because the coordinate system has changed. `apply_computed_files` (layout rebuild) also resets it because wrap geometry depends on `layout.rows`.

   Render layer: `render_row` now takes `cursor_sub: Option<usize>` instead of `is_cursor: bool`. The cursor arrow `▶` in `render_diff_line_wrapped` lands on visual sub-row `cursor_sub`, not always on the first visual row of a wrapped block. `None` means "not the cursor row".

2. **Point follow mode at the last visible content row of the newest file.** `follow_target_row` walks `layout.file_of_row` backward from the end, returning the first non-`Spacer` row whose owning file is the newest file. This lands on the last `DiffLine` of the last hunk (or a `BinaryNotice` / `HunkHeader` for edge-case files with no diff content), which is what a `tail -f`-style monitor should show. Fallbacks mirror the legacy behavior: if the walk finds nothing, try `file_first_hunk.last()`, then the absolute last row.

   The bootstrap follow test is updated to assert follow lands on a `DiffLine` row, not a `HunkHeader`, and a new regression test exercises the long-hunk case (20-line hunk, follow must land on the 20th `DiffLine`).

3. **Decide sticky header against the body height that will actually be rendered.** `render_scroll` now computes sticky via a pessimistic peek: if there's a candidate header row and the area has at least 2 rows, compute placement with `body = area.height - 1` first. If the header row sits above that peeked top, sticky wins and the render commits to `body = area.height - 1`. Otherwise sticky is off and a second placement call commits to `body = area.height`. The final call is authoritative for the `visual_top` animation side-effect — peek calls that happened during the decision are harmless because the final call overwrites the stored state.

   The existing `sticky_hunk_header_appears_when_cursor_is_below_it` test keeps its contract, and a new `sticky_header_decision_agrees_with_final_body_height` test covers the boundary (20-line hunk, tight 8-row viewport, cursor mid-hunk) where the two-pass non-convergence was exposed.

## Consequences

- **Positive**:
  - Wrap-mode navigation genuinely moves through long wrapped lines. `Ctrl-d` / `Ctrl-u` / `J` / `K` inside a 200-char minified JSON edit advance the visible cursor instead of no-op'ing on the same logical row. The user can actually inspect what the change says.
  - The cursor arrow marker follows the user's walk: `▶` lands on the visual sub-row the cursor is actually at, which is the only way the user can tell the scroll is working at all inside a wrapped block.
  - Follow mode honors its advertised contract: the newest edit is visible, even when the newest hunk is taller than the viewport. `tail -f`-style monitoring finally matches its docstring.
  - Sticky header decisions and final viewport geometry agree, so the hunk header never flickers out at boundaries. The flow is self-consistent in one pass.
  - Existing tests stay stable: `cursor_sub_row = 0` is the default and every logical-row-centric assertion continues to pass. Nowrap mode is numerically unchanged. The new tests pin the three previously unconstrained behaviors.
- **Negative**:
  - `App` state now carries an extra coordinate (`cursor_sub_row`) that must be kept in sync with `scroll` across every action. The centralization through `scroll_to` / `scroll_to_visual` makes this manageable, but every new key handler must remember which entry point to use. The invariant is documented on the field and ADR-0007 + ADR-0009 together are the canonical reference.
  - Wrap mode toggle discards the intra-row offset, so the cursor "snaps" visually when flipping wrap on/off. This is the right thing to do — the old offset has no meaning under the new coordinate system — but it is a visible discontinuity.
  - Sticky header decision now calls `viewport_placement` twice in the worst case (peek reduced + commit full). Both calls are O(rows) and the 2000-row cap keeps this negligible; the second call overwrites `visual_top` cleanly.
  - `render_row`'s signature changed from `is_cursor: bool` to `cursor_sub: Option<usize>`. Downstream test helpers had to thread `None` for non-cursor rows. This was contained to two call sites.
- **Scope**:
  - `src/app.rs`: `cursor_sub_row` field, `scroll_to` reset, `scroll_to_visual` helper, `scroll_by` wrap branch rewrite, `next_change` / `prev_change` wrap branches rewrite, `placement_target_visual_y` includes sub-row, `toggle_wrap_lines` resets, `apply_computed_files` resets, `follow_target_row` rewrite, regression tests.
  - `src/ui.rs`: sticky header decision rewrite, `render_row` / `render_diff_line_wrapped` signature change, regression test.
  - No changes in `src/watcher.rs`, `src/git.rs`. No ADR supersession.

## Alternatives Considered

- **Finding 1: promote `app.scroll` itself to a visual y (single `usize` in visual-row units)**. Rejected: every `current_hunk` / `current_file_idx` / anchor / layout query would need to map visual y back to logical row on every call, which is O(log n) via `VisualIndex::logical_at` but adds state dependencies everywhere. The `(logical_row, sub_row)` pair is strictly cheaper — logical-row queries stay O(1) on `scroll`, sub-row only costs anything when the renderer or navigation actually cares.
- **Finding 1: compute `cursor_sub_row` lazily from `visual_top.get()` at read time**. Rejected: `visual_top` is the viewport's visual y, not the cursor's, and they are independent under animation. Storing sub-row explicitly is the clean separation.
- **Finding 2: sort `layout.hunk_starts` descending and pick a different last entry**. Rejected: the problem isn't the order, it's that `hunk_starts` contains header rows by construction. The fix has to look at `layout.rows` directly to find the last diff row of the newest file.
- **Finding 3: iterate sticky decision to convergence instead of peek + commit**. Rejected: sticky is a binary state, the only way it flips twice is if the placement geometry has a fixed point at exactly the reduced height, which can't actually happen with integer-row viewports. Two placement calls are always enough; iterating adds complexity for no measurable benefit.
- **Finding 3: move sticky reservation into `App::viewport_placement` so the UI doesn't need to know**. Rejected: the sticky decision depends on **whether the renderer will draw a banner**, which is a pure render concern. Pushing it into `viewport_placement` would couple the app state machine to ratatui's rendering model.

## References

- 関連 ADR: [ADR-0007](0007-visual-row-scroll-and-session-scoped-watcher.md) (visual-row render, sticky header initial implementation), [ADR-0008](0008-dynamic-watcher-and-health-split.md) (dynamic watcher + health split)
- Codex adversarial review round 4 (foreground run, 2026-04-15)
- 関連仕様: `docs/SPEC.md` — follow mode semantics, wrap mode
