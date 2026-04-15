import { afterEach, expect, test } from "bun:test";
import type { Session } from "tuistory";
import { createTempRepo, launchKizu, type Repo } from "./helpers";

let session: Session | null = null;
let repo: Repo | null = null;

afterEach(() => {
  session?.close();
  repo?.cleanup();
  session = null;
  repo = null;
});

/**
 * tuistory's `only: { foreground: ... }` filter matches exact hex
 * strings, but ratatui + crossterm emits plain ANSI 31/32 without
 * tagging them to a specific hex — so the style filter never fires
 * for the basic palette. Unit tests (`render_scroll_lines_carry_
 * added_and_deleted_colors`) already cover the actual colour
 * rendering. These e2e tests pin the **visible text layout** so a
 * break in the diff prefix contract shows up at the black-box level.
 */
test("added + and deleted - lines render next to their content", async () => {
  repo = createTempRepo();
  repo.write("src/app.rs", "old content line\n");
  repo.git("add", ".");
  repo.git("commit", "-q", "-m", "seed");
  repo.write("src/app.rs", "new content line\n");

  session = await launchKizu({ cwd: repo.path });
  await session.waitForText("new content line", { timeout: 10_000 });

  const view = await session.text({ trimEnd: true });
  // Added prefix must sit immediately before the new content.
  expect(view).toMatch(/\+new content line/);
  // Deleted prefix must sit immediately before the removed content.
  expect(view).toMatch(/-old content line/);
  // Session counts should show 1 added, 1 deleted.
  expect(view).toContain("+1");
  expect(view).toContain("-1");
});

test("footer mode indicators switch on toggle keys", async () => {
  repo = createTempRepo();
  repo.write("src/app.rs", "fn main() {}\n");
  repo.git("add", ".");
  repo.git("commit", "-q", "-m", "seed");
  repo.write("src/app.rs", "fn main() { println!(\"demo\"); }\n");

  session = await launchKizu({ cwd: repo.path });
  await session.waitForText("demo", { timeout: 10_000 });

  // `z center` + `w nowrap` are the defaults.
  let view = await session.text({ trimEnd: true });
  expect(view).toContain("z center");
  expect(view).toContain("w nowrap");

  // Toggle both modes; the footer spans should flip.
  await session.press("z");
  await session.press("w");
  await session.waitForText("w wrap", { timeout: 5_000 });
  view = await session.text({ trimEnd: true });
  expect(view).toContain("z top");
  expect(view).toContain("w wrap");
});
