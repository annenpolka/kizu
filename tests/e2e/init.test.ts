import { afterEach, expect, setDefaultTimeout, test } from "bun:test";
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import type { Session } from "tuistory";
import { createTempRepo, launchKizu, KIZU_BIN, type Repo } from "./helpers";
import { execFileSync } from "node:child_process";

// The interactive init test launches a PTY and waits for prompt rendering.
// Bun's default 5s per-test timeout is too tight for PTY startup + rendering
// under load. Match the 15s used by reactive.test.ts.
setDefaultTimeout(15_000);

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
    "--scope", "project-local",
    "--non-interactive",
  ], { cwd: repo.path, encoding: "utf8" });

  const settingsPath = join(repo.path, ".claude", "settings.local.json");
  expect(existsSync(settingsPath)).toBe(true);

  const settings = JSON.parse(readFileSync(settingsPath, "utf8"));
  expect(settings.hooks).toBeDefined();
  expect(settings.hooks.PostToolUse).toBeArray();
  expect(settings.hooks.Stop).toBeArray();

  // New matcher-group schema: each entry has matcher + hooks array.
  const postGroup = settings.hooks.PostToolUse[0];
  expect(postGroup.matcher).toBe("Edit|Write|MultiEdit");
  expect(postGroup.hooks).toBeArray();
  // Project-local scope single-quotes the kizu binary path so spaces
  // / shell metachars in the install path don't break hook startup.
  expect(postGroup.hooks[0].command).toContain("kizu' hook-post-tool");
  expect(postGroup.hooks[0].command).toContain("--agent claude-code");

  const stopGroup = settings.hooks.Stop[0];
  expect(stopGroup.hooks[0].command).toContain("kizu' hook-stop");
});

test("non-interactive init is idempotent (skips on second run)", async () => {
  repo = createTempRepo();

  const run = () => execFileSync(KIZU_BIN, [
    "init", "--agent", "claude-code", "--scope", "project-local", "--non-interactive",
  ], { cwd: repo.path, encoding: "utf8" });

  run(); // first
  run(); // second — should not duplicate

  const settings = JSON.parse(
    readFileSync(join(repo.path, ".claude", "settings.local.json"), "utf8")
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
        { matcher: "", hooks: [{ type: "command", command: "my-linter --check", timeout: 5 }] },
        { matcher: "Edit|Write", hooks: [{ type: "command", command: "kizu hook-post-tool --agent claude-code", timeout: 10 }] },
      ],
      Stop: [
        { matcher: "", hooks: [{ type: "command", command: "kizu hook-stop --agent claude-code", timeout: 10 }] },
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
  expect(after.hooks.PostToolUse[0].hooks[0].command).toBe("my-linter --check");
  // Stop array was entirely kizu → key removed.
  expect(after.hooks.Stop).toBeUndefined();
});

/**
 * Set up an interactive `kizu init` session with Claude Code detected
 * (stub `claude` binary on PATH + `.claude/` dir in repo) so the
 * MultiSelect has at least one recommended item.
 */
async function launchInteractiveInit(opts?: { cols?: number; rows?: number }): Promise<{ session: Session; repo: Repo }> {
  const repo = createTempRepo();
  execFileSync("mkdir", ["-p", join(repo.path, ".claude")]);

  const stubBin = join(repo.path, ".stub-bin");
  execFileSync("mkdir", ["-p", stubBin]);
  execFileSync("bash", ["-c", `printf '#!/bin/sh\\nexit 0\\n' > "${stubBin}/claude" && chmod +x "${stubBin}/claude"`]);
  const minimalPath = [stubBin, "/usr/bin", "/bin", "/usr/local/bin"].join(":");

  const session = await launchKizu({
    cwd: repo.path,
    args: ["init"],
    cols: opts?.cols ?? 100,
    rows: opts?.rows ?? 30,
    env: { PATH: minimalPath },
  });
  return { session, repo };
}

test("interactive init shows agent selection prompt", async () => {
  ({ session, repo } = await launchInteractiveInit());

  await session.waitForText("Select agents", { timeout: 10_000 });
  const view = await session.text({ trimEnd: true });
  expect(view).toContain("Claude Code");

  // Press Enter to accept defaults, then expect scope prompt.
  await session.press("enter");
  await session.waitForText("Install scope", { timeout: 5_000 });

  // Press Enter to accept default (project-local).
  await session.press("enter");

  // Wait for completion.
  await session.waitForText("entries added", { timeout: 5_000 });
});

test("interactive init shows support-level and detection icons", async () => {
  // Post-ADR-0019: the custom prompt re-enables colored support-level
  // pills (● / ◐ / ○) and detection status glyphs (✓ / ~ / ✗) that
  // dialoguer could not render without scroll drift.
  ({ session, repo } = await launchInteractiveInit());

  await session.waitForText("Select agents", { timeout: 10_000 });
  const view = await session.text({ trimEnd: true });

  // Exactly one of the support-level glyphs must be present per row.
  expect(view).toMatch(/[●◐○]/);
  // At least one detection status glyph must be present. (Claude Code
  // row will be `✓ detected` when binary_found + config_dir_found;
  // others will be `✗ not found` since we set a minimal PATH.)
  expect(view).toMatch(/[✓~✗]/);
});

test("interactive init uses ballot-box checkboxes (not emoji)", async () => {
  // Preference captured during v0.3 dev: the checkbox column uses
  // text-style ☐/☑ (U+2610/U+2611, 1-cell each) rather than emoji
  // ⬜/✅ (2-cell), which keeps the picker tight and alignment
  // predictable across terminals.
  ({ session, repo } = await launchInteractiveInit());

  await session.waitForText("Select agents", { timeout: 10_000 });
  const view = await session.text({ trimEnd: true });

  // Claude Code is recommended (stub binary + .claude/ dir), so at
  // least one row starts checked. Verify both glyphs render.
  expect(view).toMatch(/[☐☑]/);
  // Explicitly reject the emoji variants so a future refactor doesn't
  // silently revert the choice.
  expect(view).not.toMatch(/[⬜✅]/);
});

test("interactive init does not drift the banner on repeated keypresses", async () => {
  // Dialoguer 0.11/0.12 would scroll the banner off-screen when items
  // carried ANSI escapes (see ADR-0019). The custom prompt must leave
  // the banner ("傷  kizu init") in place after many keypresses.
  ({ session, repo } = await launchInteractiveInit());

  await session.waitForText("kizu init", { timeout: 10_000 });
  await session.waitForText("Select agents", { timeout: 5_000 });

  // Press `j` 20 times, then `k` 20 times. The prompt has only a
  // handful of rows; without drift, the banner must remain visible.
  for (let i = 0; i < 20; i++) await session.press("j");
  for (let i = 0; i < 20; i++) await session.press("k");

  const view = await session.text({ trimEnd: true });
  expect(view).toContain("kizu init");
  expect(view).toContain("Select agents");
});

test("interactive scope prompt highlights project-local as default", async () => {
  ({ session, repo } = await launchInteractiveInit());

  await session.waitForText("Select agents", { timeout: 10_000 });
  await session.press("enter"); // accept default agents
  await session.waitForText("Install scope", { timeout: 5_000 });

  const view = await session.text({ trimEnd: true });
  // Cursor mark `>` must appear on the project-local row (= the first
  // item in the scope list). We pin the *line*: "> project-local ..."
  expect(view).toMatch(/>\s+project-local/);
});
