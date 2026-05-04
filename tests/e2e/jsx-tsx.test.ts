import { afterEach, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import type { Session } from "tuistory";
import { createTempRepo, KIZU_BIN, launchKizu, type Repo } from "./helpers";

let session: Session | null = null;
let repo: Repo | null = null;

afterEach(() => {
  session?.close();
  repo?.cleanup();
  session = null;
  repo = null;
});

function seedTsxChildrenDiff(repo: Repo) {
  repo.write(
    "src/Counter.tsx",
    "export function Counter({ count }: { count: number }) {\n  return (\n    <section>\n    </section>\n  );\n}\n"
  );
  repo.git("add", "src/Counter.tsx");
  repo.git("commit", "-q", "-m", "seed tsx");
  repo.write(
    "src/Counter.tsx",
    "export function Counter({ count }: { count: number }) {\n  return (\n    <section>\n      <p>Count: {count}</p>\n    </section>\n  );\n}\n"
  );
}

test("a key inserts JSX block scar in TSX children and u undoes it", async () => {
  repo = createTempRepo();
  seedTsxChildrenDiff(repo);

  session = await launchKizu({ cwd: repo.path });
  await session.waitForText("src/Counter.tsx", { timeout: 10_000 });
  await session.waitForText("<p>Count", { timeout: 10_000 });

  await session.press("j");
  await session.press("a");
  await Bun.sleep(300);

  const scarred = readFileSync(join(repo.path, "src/Counter.tsx"), "utf8");
  expect(scarred).toContain("{/* @kizu[ask]: explain this change */}");
  expect(scarred).toContain(
    "      {/* @kizu[ask]: explain this change */}\n      <p>Count: {count}</p>"
  );

  await session.press("u");
  await Bun.sleep(300);

  const undone = readFileSync(join(repo.path, "src/Counter.tsx"), "utf8");
  expect(undone).not.toContain("@kizu");
});

test("hook-stop detects JSX block scars", () => {
  repo = createTempRepo();
  repo.write(
    "src/Counter.tsx",
    "export function Counter() {\n  return (\n    <section>\n      {/* @kizu[ask]: explain this change */}\n      <p>Count</p>\n    </section>\n  );\n}\n"
  );
  repo.git("add", "src/Counter.tsx");
  repo.git("commit", "-q", "-m", "seed jsx scar");

  const input = JSON.stringify({
    hook_event_name: "Stop",
    stop_hook_active: false,
    cwd: repo.path,
  });
  const result = spawnSync(KIZU_BIN, ["hook-stop"], {
    cwd: repo.path,
    input,
    encoding: "utf8",
    env: {
      ...process.env,
      KIZU_STATE_DIR: repo.statePath,
    },
  });

  expect(result.status).toBe(2);
  expect(result.stderr).toContain("src/Counter.tsx:4");
  expect(result.stderr).toContain("@kizu[ask]");
  expect(result.stderr).toContain("explain this change");
});
