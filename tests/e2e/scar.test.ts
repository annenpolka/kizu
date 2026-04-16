import { afterEach, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
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

function seedOneLineDiff(repo: Repo) {
  repo.write("src/main.rs", "fn one() {}\n");
  repo.git("add", "src/main.rs");
  repo.git("commit", "-q", "-m", "seed");
  repo.write("src/main.rs", "fn one() {}\nfn two() {}\n");
}

test("a key inserts ask scar above the cursor line", async () => {
  repo = createTempRepo();
  seedOneLineDiff(repo);

  session = await launchKizu({ cwd: repo.path });
  await session.waitForText("src/main.rs", { timeout: 10_000 });

  // Navigate to a diff line and press a.
  await session.press("j");
  await session.press("a");

  // Wait a beat for the file write to land.
  await Bun.sleep(200);

  const content = readFileSync(join(repo.path, "src/main.rs"), "utf8");
  expect(content).toContain("@kizu[ask]: explain this change");
});

test("c key opens scar comment, Enter commits free scar", async () => {
  repo = createTempRepo();
  seedOneLineDiff(repo);

  session = await launchKizu({ cwd: repo.path });
  await session.waitForText("src/main.rs", { timeout: 10_000 });

  await session.press("j");
  await session.press("c");
  await session.waitForText("[scar]", { timeout: 5_000 });

  // Type a message.
  for (const ch of "test note") {
    await session.press(ch);
  }
  await session.press("enter");

  await Bun.sleep(200);

  const content = readFileSync(join(repo.path, "src/main.rs"), "utf8");
  expect(content).toContain("@kizu[free]: test note");
});

test("x key reverts hunk after y confirmation", async () => {
  repo = createTempRepo();
  seedOneLineDiff(repo);

  session = await launchKizu({ cwd: repo.path });
  await session.waitForText("src/main.rs", { timeout: 10_000 });

  await session.press("j");
  await session.press("x");
  await session.waitForText("[revert?]", { timeout: 5_000 });

  await session.press("y");

  // Wait for git apply --reverse.
  await Bun.sleep(500);

  const content = readFileSync(join(repo.path, "src/main.rs"), "utf8");
  expect(content).toBe("fn one() {}\n");
  expect(content).not.toContain("fn two");
});

test("e key with EDITOR=true does not panic", async () => {
  repo = createTempRepo();
  seedOneLineDiff(repo);

  session = await launchKizu({
    cwd: repo.path,
    env: { EDITOR: "true" },
  });
  await session.waitForText("src/main.rs", { timeout: 10_000 });

  await session.press("j");
  await session.press("e");

  // `true` exits immediately; kizu should re-enter alternate screen.
  // Wait for the TUI to stabilize back.
  await session.waitForText("src/main.rs", { timeout: 10_000 });

  // Verify kizu is still alive.
  const view = await session.text({ trimEnd: true });
  expect(view).toContain("src/main.rs");
});
