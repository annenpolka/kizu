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

/** Seed a repo with two files, each modified so we get two hunks. */
function seedTwoFileRepo(): Repo {
  const r = createTempRepo();
  r.write("src/auth.rs", "fn verify() {}\n");
  r.write("src/handler.rs", "fn handle() {}\n");
  r.git("add", ".");
  r.git("commit", "-q", "-m", "seed");
  // Modify both so each shows up as a separate file in the scroll.
  r.write("src/auth.rs", "fn verify() -> bool { true }\n");
  r.write("src/handler.rs", "fn handle() -> Result<()> { Ok(()) }\n");
  return r;
}

test("J / K jumps between hunks and updates the footer path", async () => {
  repo = seedTwoFileRepo();
  session = await launchKizu({ cwd: repo.path });

  // Wait for both files to appear in the scroll view before moving.
  await session.waitForText("src/handler.rs", { timeout: 10_000 });
  await session.waitForText("src/auth.rs");

  // Press SHIFT-K to jump to the previous hunk (= first file's hunk).
  // follow mode is on at launch, so the cursor starts at the newest
  // (last mtime) file's last hunk; K walks backward into the earlier
  // file. SHIFT-K is the strict "prev hunk header" motion.
  await session.press(["shift", "k"]);

  // After moving off the follow target, the footer switches to
  // `[manual]` and shows whichever file the cursor is now on.
  await session.waitForText("[manual]", { timeout: 5_000 });
  const afterPrev = await session.text({ trimEnd: true });
  expect(afterPrev).toContain("[manual]");

  // SHIFT-J walks forward: back onto the later file. Assert the
  // footer is still `[manual]` (not back to `[follow]`).
  await session.press(["shift", "j"]);
  const afterNext = await session.text({ trimEnd: true });
  expect(afterNext).toContain("[manual]");
  expect(afterNext).toContain("src/");
});

test("f restores follow mode after manual navigation", async () => {
  repo = seedTwoFileRepo();
  session = await launchKizu({ cwd: repo.path });
  await session.waitForText("src/auth.rs", { timeout: 10_000 });

  // Break follow with K, then ask for it back with f.
  await session.press(["shift", "k"]);
  await session.waitForText("[manual]", { timeout: 5_000 });
  await session.press("f");
  await session.waitForText("[follow]", { timeout: 5_000 });
});

test("space opens the file picker overlay", async () => {
  repo = seedTwoFileRepo();
  session = await launchKizu({ cwd: repo.path });
  await session.waitForText("src/auth.rs", { timeout: 10_000 });

  await session.press("space");
  // The picker renders `Files N/M` in its border title and a `>` query
  // prompt on the first row of the popup.
  await session.waitForText(/Files \d+\/\d+/, { timeout: 5_000 });
  const view = await session.text({ trimEnd: true });
  expect(view).toContain("[picker]");
  expect(view).toContain("type to filter");

  // Esc closes the picker and returns to the scroll view.
  await session.press("escape");
  await session.waitForText("[follow]", { timeout: 5_000 });
});
