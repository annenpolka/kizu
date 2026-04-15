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

test("R re-baselines HEAD and clears the diff", async () => {
  repo = createTempRepo();
  repo.write("src/app.rs", "fn main() {}\n");
  repo.git("add", ".");
  repo.git("commit", "-q", "-m", "seed");
  repo.write(
    "src/app.rs",
    "fn main() { println!(\"before reset\"); }\n",
  );

  session = await launchKizu({ cwd: repo.path });
  await session.waitForText("before reset", { timeout: 10_000 });

  // Commit the change inside the live session — kizu's HEAD moves.
  repo.git("add", ".");
  repo.git("commit", "-q", "-m", "freeze");

  // With the old baseline still active, kizu still shows the change
  // as a diff against the *original* HEAD. `R` must reset to the new
  // HEAD and collapse the view back to the empty state.
  await session.press(["shift", "r"]);
  await session.waitForText("No changes since baseline", { timeout: 10_000 });

  const view = await session.text({ trimEnd: true });
  expect(view).toContain("No changes since baseline");
  expect(view).not.toContain("before reset");
});
