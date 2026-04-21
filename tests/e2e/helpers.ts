import { launchTerminal, type Session } from "tuistory";
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";

/**
 * Absolute path to the kizu binary under test.
 *
 * CI sets `KIZU_BIN` explicitly (see `.github/workflows/ci.yml`). Local
 * runs default to `../../target/release/kizu`, which must have been
 * built by `cargo build --release --locked` beforehand.
 */
export const KIZU_BIN: string = process.env.KIZU_BIN
  ? resolve(process.env.KIZU_BIN)
  : resolve(import.meta.dir, "../../target/release/kizu");

/**
 * A temporary git repository for a single test.
 *
 * - `path` is the absolute worktree root, safe to pass as `cwd`.
 * - `write` drops a file (creating parent dirs) into the worktree.
 * - `git` runs `git <args>` inside the worktree.
 * - `cleanup` removes the directory; idempotent.
 */
export type Repo = {
  path: string;
  statePath: string;
  write(rel: string, content: string): void;
  git(...args: string[]): string;
  cleanup(): void;
};

/**
 * Create a brand-new git repo rooted at a fresh temp directory. The
 * repo starts with a single empty commit so `HEAD` is always a real
 * SHA (the empty-tree fallback path is tested separately by the Rust
 * unit tests — keeping e2e fixtures on the happy path keeps failures
 * localisable).
 */
export function createTempRepo(): Repo {
  const path = mkdtempSync(join(tmpdir(), "kizu-e2e-"));
  const statePath = `${path}-state`;
  const git = (...args: string[]): string =>
    execFileSync("git", args, { cwd: path, encoding: "utf8" }).trimEnd();

  git("init", "-q", "-b", "main");
  git("config", "user.email", "test@example.com");
  git("config", "user.name", "kizu-e2e");
  git("commit", "-q", "--allow-empty", "-m", "initial");

  return {
    path,
    statePath,
    git,
    write(rel, content) {
      const abs = join(path, rel);
      mkdirSync(dirname(abs), { recursive: true });
      writeFileSync(abs, content);
    },
    cleanup() {
      rmSync(path, { recursive: true, force: true });
      rmSync(statePath, { recursive: true, force: true });
    },
  };
}

/**
 * Launch kizu inside `opts.cwd` with a fixed terminal size. The
 * resulting session must be `.close()`-d by the caller (typical
 * pattern: capture in a `let` and close from `afterEach`).
 */
export async function launchKizu(opts: {
  cwd: string;
  args?: string[];
  cols?: number;
  rows?: number;
  env?: Record<string, string>;
}): Promise<Session> {
  return await launchTerminal({
    command: KIZU_BIN,
    args: opts.args ?? [],
    cols: opts.cols ?? 100,
    rows: opts.rows ?? 30,
    cwd: opts.cwd,
    env: {
      PATH: process.env.PATH ?? "",
      HOME: process.env.HOME ?? "",
      KIZU_STATE_DIR: `${opts.cwd}-state`,
      TERM: "xterm-256color",
      LC_ALL: "C.UTF-8",
      LANG: "C.UTF-8",
      ...opts.env,
    },
  });
}
