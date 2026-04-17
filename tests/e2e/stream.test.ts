import { afterEach, expect, test } from "bun:test";
import { execFileSync } from "node:child_process";
import type { Session } from "tuistory";
import { createTempRepo, launchKizu, KIZU_BIN, type Repo } from "./helpers";

let session: Session | null = null;
let repo: Repo | null = null;

afterEach(() => {
  session?.close();
  repo?.cleanup();
  session = null;
  repo = null;
});

/** Send a hook-log-event to the kizu binary, simulating a PostToolUse hook. */
function sendHookLogEvent(cwd: string, toolName: string, filePath: string) {
  const json = JSON.stringify({
    session_id: "e2e-test",
    hook_event_name: "PostToolUse",
    tool_name: toolName,
    tool_input: { file_path: filePath },
    cwd,
  });
  execFileSync(KIZU_BIN, ["hook-log-event"], {
    input: json,
    cwd,
    env: { ...process.env, PATH: process.env.PATH ?? "" },
  });
}

test("Tab toggles to stream mode and back", async () => {
  repo = createTempRepo();
  repo.write("a.rs", "fn a() {}\n");
  repo.git("add", "a.rs");
  repo.git("commit", "-q", "-m", "seed");
  repo.write("a.rs", "fn a() { 1 }\n");

  session = await launchKizu({ cwd: repo.path });
  await session.waitForText("a.rs", { timeout: 10_000 });

  // Footer shows [follow] in diff mode.
  let view = await session.text({ trimEnd: true });
  expect(view).toContain("[follow]");

  // Tab to stream mode.
  await session.press("tab");
  await session.waitForText("[stream]", { timeout: 5_000 });
  view = await session.text({ trimEnd: true });
  expect(view).toContain("[stream]");

  // Tab back to diff mode.
  await session.press("tab");
  await session.waitForText("[follow]", { timeout: 5_000 });
  view = await session.text({ trimEnd: true });
  expect(view).toContain("a.rs");
});

test("stream mode shows events from hook-log-event", async () => {
  repo = createTempRepo();
  repo.write("a.rs", "fn a() {}\n");
  repo.git("add", "a.rs");
  repo.git("commit", "-q", "-m", "seed");

  session = await launchKizu({ cwd: repo.path });
  await session.waitForText("No changes since baseline", { timeout: 10_000 });

  // Modify the file and send a hook-log-event.
  repo.write("a.rs", "fn a() { 1 }\n");
  sendHookLogEvent(repo.path, "Write", `${repo.path}/a.rs`);

  // Wait for the diff view to pick up the filesystem change.
  await session.waitForText("a.rs", { timeout: 10_000 });

  // Switch to stream mode.
  await session.press("tab");
  await session.waitForText("[stream]", { timeout: 5_000 });

  // The event should appear with the tool name.
  const view = await session.text({ trimEnd: true });
  expect(view).toContain("Write");
  expect(view).toContain("a.rs");
});

test("Tab back from stream lands on the file from the stream event", async () => {
  repo = createTempRepo();
  // Two files — we want to verify that Tab back from stream mode
  // lands on the file the stream cursor was pointing at, not just
  // the first file.
  repo.write("aaa.rs", "fn a() {}\n");
  repo.write("zzz.rs", "fn z() {}\n");
  repo.git("add", ".");
  repo.git("commit", "-q", "-m", "seed");

  session = await launchKizu({ cwd: repo.path });
  await session.waitForText("No changes since baseline", { timeout: 10_000 });

  // Edit both files to create diffs.
  repo.write("aaa.rs", "fn a() { 1 }\n");
  repo.write("zzz.rs", "fn z() { 2 }\n");

  // Send a hook-log-event only for zzz.rs.
  sendHookLogEvent(repo.path, "Edit", `${repo.path}/zzz.rs`);

  // Wait for diff view to pick up changes.
  await session.waitForText("zzz.rs", { timeout: 10_000 });

  // Tab to stream mode — the event for zzz.rs should be visible.
  await session.press("tab");
  await session.waitForText("[stream]", { timeout: 5_000 });
  await session.waitForText("zzz.rs", { timeout: 5_000 });

  // Tab back to diff mode — cursor should land near zzz.rs.
  await session.press("tab");
  await session.waitForText("[follow]", { timeout: 5_000 });
  const view = await session.text({ trimEnd: true });
  // The viewport should contain zzz.rs (the file from the stream event).
  expect(view).toContain("zzz.rs");
});

test("stream j navigates events and Tab back lands on selected file", async () => {
  repo = createTempRepo();
  repo.write("a.rs", "fn a() {}\n");
  repo.write("b.rs", "fn b() {}\n");
  repo.git("add", ".");
  repo.git("commit", "-q", "-m", "seed");

  session = await launchKizu({ cwd: repo.path });
  await session.waitForText("No changes since baseline", { timeout: 10_000 });

  // Create edits and events for both files.
  repo.write("a.rs", "fn a() { 1 }\n");
  sendHookLogEvent(repo.path, "Write", `${repo.path}/a.rs`);
  await session.waitForText("a.rs", { timeout: 10_000 });

  repo.write("b.rs", "fn b() { 2 }\n");
  sendHookLogEvent(repo.path, "Edit", `${repo.path}/b.rs`);
  await new Promise((r) => setTimeout(r, 500));

  // Tab to stream — should show both events.
  await session.press("tab");
  await session.waitForText("[stream]", { timeout: 5_000 });
  let view = await session.text({ trimEnd: true });
  expect(view).toContain("Write");
  expect(view).toContain("Edit");

  // Navigate to the second event (b.rs) with j.
  await session.press("j");
  await new Promise((r) => setTimeout(r, 300));

  // Tab back — should land near b.rs.
  await session.press("tab");
  await session.waitForText("b.rs", { timeout: 10_000 });
  view = await session.text({ trimEnd: true });
  expect(view).toContain("b.rs");
});

test("Tab to stream with no events shows empty message", async () => {
  repo = createTempRepo();
  session = await launchKizu({ cwd: repo.path });
  await session.waitForText("No changes since baseline", { timeout: 10_000 });

  // Tab to stream with zero events.
  await session.press("tab");
  await session.waitForText("[stream]", { timeout: 5_000 });
  const view = await session.text({ trimEnd: true });
  // Empty stream: no files in layout, should show the empty state.
  expect(view).toContain("[stream]");
  // Should NOT show diff-mode content.
  expect(view).not.toContain("[follow]");
});

test("stream mode is not disrupted by worktree changes", async () => {
  repo = createTempRepo();
  repo.write("a.rs", "fn a() {}\n");
  repo.git("add", "a.rs");
  repo.git("commit", "-q", "-m", "seed");

  session = await launchKizu({ cwd: repo.path });
  await session.waitForText("No changes since baseline", { timeout: 10_000 });

  // Edit a file and send a hook-log-event AFTER kizu is running.
  repo.write("a.rs", "fn a() { 1 }\n");
  sendHookLogEvent(repo.path, "Write", `${repo.path}/a.rs`);

  // Wait for diff view to pick up the change.
  await session.waitForText("a.rs", { timeout: 10_000 });

  // Switch to stream mode.
  await session.press("tab");
  await session.waitForText("[stream]", { timeout: 5_000 });
  await session.waitForText("Write", { timeout: 5_000 });

  // Now modify another file while in stream mode.
  repo.write("b.rs", "fn b() {}\n");

  // Wait a bit for the watcher to fire.
  await new Promise((r) => setTimeout(r, 1500));

  // Stream mode should still be active — NOT overwritten by diff.
  const view = await session.text({ trimEnd: true });
  expect(view).toContain("[stream]");
  // The stream view should still show our event, not the diff view.
  expect(view).toContain("Write");
});
