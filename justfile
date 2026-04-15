# kizu task runner — mirrors the CI sequence in
# `.github/workflows/ci.yml` so `just` locally == green PR remotely.
#
# Run `just` with no args to see the list of recipes, or `just ci` to
# run the full gate. `just rust` is the fast inner loop (fmt + lint +
# cargo test) when you don't need the release binary or e2e coverage.

set shell := ["bash", "-euo", "pipefail", "-c"]

e2e_dir := "tests/e2e"
release_bin := "target/release/kizu"

# Default recipe: full CI chain.
default: ci

# Full CI gate: fmt-check → clippy → cargo test → release → e2e.
ci: fmt-check lint test release e2e

# Fast inner loop: format, lint, unit tests. Skips release + e2e.
rust: fmt lint test

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

lint:
    cargo clippy --all-targets --all-features -- -D warnings

test:
    cargo test --all-targets --all-features

release:
    cargo build --release --locked

# tuistory e2e. Builds release first so the tests run against a fresh
# binary, then hits bun with the frozen lockfile (matches CI).
e2e: release
    cd {{ e2e_dir }} && bun install --frozen-lockfile
    cd {{ e2e_dir }} && KIZU_BIN="$(pwd)/../../{{ release_bin }}" bun test

# Non-frozen install, for refreshing e2e deps locally.
e2e-install:
    cd {{ e2e_dir }} && bun install

# Launch kizu against the current worktree (release build).
run:
    cargo run --release

# Remove cargo artifacts + e2e node_modules.
clean:
    cargo clean
    rm -rf {{ e2e_dir }}/node_modules
