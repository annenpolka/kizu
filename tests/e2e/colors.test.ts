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
 * ADR-0014: diff rows are painted with a delta-style background
 * color (`BG_ADDED` / `BG_DELETED`) instead of a literal `+`/`-`
 * prefix. tuistory's `only: { foreground: ... }` filter matches
 * exact hex strings and ratatui + crossterm emits plain ANSI for
 * the standard palette, so the coloured cells don't show up in the
 * style filter — but we can pin the **text layout contract**:
 * after the refactor, there MUST NOT be a `+` or `-` prefix right
 * before the added / deleted body. Rust unit tests
 * (`render_scroll_lines_use_background_color_for_added_and_deleted`,
 * `render_scroll_lines_omit_plus_minus_prefix`) cover the actual
 * Style.bg assertions.
 */
test("diff rows render content without +/- prefix markers", async () => {
  repo = createTempRepo();
  repo.write("src/app.rs", "old content line\n");
  repo.git("add", ".");
  repo.git("commit", "-q", "-m", "seed");
  repo.write("src/app.rs", "new content line\n");

  session = await launchKizu({ cwd: repo.path });
  await session.waitForText("new content line", { timeout: 10_000 });

  const view = await session.text({ trimEnd: true });
  // Both lines should be visible as bare body text (no prefix column).
  expect(view).toContain("new content line");
  expect(view).toContain("old content line");
  // And critically, neither must carry a `+`/`-` sign immediately
  // adjacent to its content — the background colour does that job now.
  expect(view).not.toMatch(/\+new content line/);
  expect(view).not.toMatch(/-old content line/);
  // Session counts in the footer still use `+N` / `-N` formatting.
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

test("file view wrap toggle reveals long-line continuation", async () => {
  repo = createTempRepo();
  const long = Array.from({ length: 20 }, (_, i) =>
    `segment${i.toString().padStart(2, "0")}`
  ).join("-");
  const lateSlice = "segment17";
  repo.write("src/app.rs", "fn main() {}\n");
  repo.git("add", ".");
  repo.git("commit", "-q", "-m", "seed");
  repo.write("src/app.rs", `const DATA: &str = "${long}";\n`);

  session = await launchKizu({ cwd: repo.path, cols: 50, rows: 20 });
  await session.waitForText("const DATA", { timeout: 10_000 });

  await session.press("j");
  await session.press("enter");
  await session.waitForText("[file view]", { timeout: 5_000 });

  let view = await session.text({ trimEnd: true });
  expect(view).toContain("w nowrap");
  expect(view).not.toContain(lateSlice);

  await session.press("w");
  await session.waitForText("w wrap", { timeout: 5_000 });

  view = await session.text({ trimEnd: true });
  expect(view).toContain("w wrap");
  expect(view).toContain(lateSlice);
});
