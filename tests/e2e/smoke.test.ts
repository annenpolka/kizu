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

test("launches against a clean repo and shows the empty-state banner", async () => {
  repo = createTempRepo();
  session = await launchKizu({ cwd: repo.path });

  // No modifications yet → kizu renders the empty-state placeholder.
  await session.waitForText("No changes since baseline", { timeout: 10_000 });

  // Footer always shows the follow-mode indicator while idle.
  const view = await session.text({ trimEnd: true });
  expect(view).toContain("[follow]");
  expect(view).toContain("0 files");
});

test("shows a modified file in the scroll view", async () => {
  repo = createTempRepo();
  repo.write("src/auth.rs", "fn verify() {}\n");
  repo.git("add", "src/auth.rs");
  repo.git("commit", "-q", "-m", "seed auth");
  // Dirty the worktree AFTER the commit so `git diff HEAD` picks it up.
  repo.write("src/auth.rs", "fn verify() -> bool { true }\n");

  session = await launchKizu({ cwd: repo.path });
  await session.waitForText("src/auth.rs", { timeout: 10_000 });

  const view = await session.text({ trimEnd: true });
  expect(view).toContain("src/auth.rs");
  // Session counts reflect a real diff.
  expect(view).not.toContain("0 files");
});

test("q exits cleanly", async () => {
  repo = createTempRepo();
  session = await launchKizu({ cwd: repo.path });
  await session.waitForText("No changes since baseline", { timeout: 10_000 });

  await session.press("q");
  // After `q`, kizu restores the terminal and exits. `close()` must
  // not throw even if the process is already gone.
  session.close();
  session = null;
});
