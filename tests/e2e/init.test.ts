import { afterEach, expect, test } from "bun:test";
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import type { Session } from "tuistory";
import { createTempRepo, launchKizu, KIZU_BIN, type Repo } from "./helpers";
import { execFileSync } from "node:child_process";

let session: Session | null = null;
let repo: Repo | null = null;

afterEach(() => {
  session?.close();
  repo?.cleanup();
  session = null;
  repo = null;
});

test("non-interactive init creates claude-code hooks in project scope", async () => {
  repo = createTempRepo();

  // Run kizu init synchronously (non-interactive, no pty needed).
  execFileSync(KIZU_BIN, [
    "init",
    "--agent", "claude-code",
    "--scope", "project",
    "--non-interactive",
  ], { cwd: repo.path, encoding: "utf8" });

  const settingsPath = join(repo.path, ".claude", "settings.json");
  expect(existsSync(settingsPath)).toBe(true);

  const settings = JSON.parse(readFileSync(settingsPath, "utf8"));
  expect(settings.hooks).toBeDefined();
  expect(settings.hooks.PostToolUse).toBeArray();
  expect(settings.hooks.Stop).toBeArray();

  const postCmd = settings.hooks.PostToolUse[0]?.command;
  expect(postCmd).toContain("kizu hook-post-tool");
  expect(postCmd).toContain("--agent claude-code");

  const stopCmd = settings.hooks.Stop[0]?.command;
  expect(stopCmd).toContain("kizu hook-stop");
});

test("non-interactive init is idempotent (skips on second run)", async () => {
  repo = createTempRepo();

  const run = () => execFileSync(KIZU_BIN, [
    "init", "--agent", "claude-code", "--scope", "project", "--non-interactive",
  ], { cwd: repo.path, encoding: "utf8" });

  run(); // first
  run(); // second — should not duplicate

  const settings = JSON.parse(
    readFileSync(join(repo.path, ".claude", "settings.json"), "utf8")
  );
  expect(settings.hooks.PostToolUse).toHaveLength(1);
  expect(settings.hooks.Stop).toHaveLength(1);
});

test("teardown removes kizu hooks and preserves user hooks", async () => {
  repo = createTempRepo();

  // Seed a settings.json with a user hook + kizu hooks.
  const dir = join(repo.path, ".claude");
  execFileSync("mkdir", ["-p", dir]);
  const settingsPath = join(dir, "settings.json");
  const initial = {
    hooks: {
      PostToolUse: [
        { command: "my-linter --check", timeout: 5 },
        { command: "kizu hook-post-tool --agent claude-code", timeout: 10 },
      ],
      Stop: [
        { command: "kizu hook-stop --agent claude-code", timeout: 10 },
      ],
    },
  };
  Bun.write(settingsPath, JSON.stringify(initial));

  execFileSync(KIZU_BIN, ["teardown"], {
    cwd: repo.path,
    encoding: "utf8",
  });

  const after = JSON.parse(readFileSync(settingsPath, "utf8"));
  // kizu entries removed, user linter preserved.
  expect(after.hooks.PostToolUse).toHaveLength(1);
  expect(after.hooks.PostToolUse[0].command).toBe("my-linter --check");
  // Stop array was entirely kizu → key removed.
  expect(after.hooks.Stop).toBeUndefined();
});

test("interactive init shows agent selection prompt", async () => {
  repo = createTempRepo();
  // Create .claude/ dir so Claude Code appears as "config found".
  execFileSync("mkdir", ["-p", join(repo.path, ".claude")]);

  session = await launchKizu({
    cwd: repo.path,
    args: ["init"],
    cols: 100,
    rows: 30,
  });

  // dialoguer MultiSelect should show the agent list.
  await session.waitForText("Select agents", { timeout: 10_000 });
  const view = await session.text({ trimEnd: true });
  expect(view).toContain("Claude Code");

  // Press Enter to accept defaults, then expect scope prompt.
  await session.press("enter");
  await session.waitForText("Install scope", { timeout: 5_000 });

  // Press Enter to accept default (project).
  await session.press("enter");

  // Wait for completion.
  await session.waitForText("entries added", { timeout: 5_000 });
});
