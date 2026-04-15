import { afterEach, expect, setDefaultTimeout, test } from "bun:test";
import type { Session } from "tuistory";
import { createTempRepo, launchKizu, type Repo } from "./helpers";

let session: Session | null = null;
let repo: Repo | null = null;

// These scenarios intentionally wait up to 10s for watcher-driven UI updates.
// Bun's default 5s per-test timeout can kill the test harness before
// `waitForText(..., { timeout: 10_000 })` gets a chance to finish under CI
// load or when the macOS PollWatcher fallback is active.
setDefaultTimeout(15_000);

afterEach(() => {
  session?.close();
  repo?.cleanup();
  session = null;
  repo = null;
});

test("filesystem writes during the session appear without user input", async () => {
  repo = createTempRepo();
  session = await launchKizu({ cwd: repo.path });

  // Cold launch: empty repo → empty-state banner.
  await session.waitForText("No changes since baseline", { timeout: 10_000 });

  // Drop a brand new file into the worktree. notify-debouncer-full
  // has a 300 ms debounce window (ADR-0002) so there's a delay before
  // the worktree event reaches kizu; waitForText's internal polling
  // covers it.
  repo.write("notes/scratch.md", "# scratch\n\nfirst line\n");
  await session.waitForText("notes/scratch.md", { timeout: 10_000 });

  const view = await session.text({ trimEnd: true });
  expect(view).toContain("notes/scratch.md");
  // The session added-line count should move off zero.
  expect(view).not.toContain("+0 -0");
});

test("editing an existing file replaces same-size diff content reactively", async () => {
  repo = createTempRepo();
  repo.write("src/app.rs", "fn main() {}\n");
  repo.git("add", ".");
  repo.git("commit", "-q", "-m", "seed");
  repo.write("src/app.rs", "fn main() { println!(\"hi\"); }\n");

  session = await launchKizu({ cwd: repo.path });
  await session.waitForText("hi", { timeout: 10_000 });

  // Now change the file again — kizu must pick up the new content
  // and remove the old "hi" string from the display.
  repo.write("src/app.rs", "fn main() { println!(\"ok\"); }\n");
  await session.waitForText('println!("ok")', { timeout: 10_000 });

  const view = await session.text({ trimEnd: true });
  expect(view).toContain('println!("ok")');
  expect(view).not.toContain("println!(\"hi\")");
});
