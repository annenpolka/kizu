# ADR-0011: common git-dir root watch を常設し late packed-refs rewrite を拾う

- **Status**: Accepted
- **Date**: 2026-04-15
- **Deciders**: annenpolka, Codex adversarial review loop
- **Extends**: [ADR-0010](0010-macos-poll-fallback-and-targeted-git-watch-roots.md)

## Context

ADR-0010 で macOS watcher は `PollWatcher` fallback へ切り替え、git state watch root を `HEAD` / `refs` / `packed-refs` に絞った。そこで `packed-refs` が起動時に存在しない場合は common git dir root を `NonRecursive` で watch し、file birth だけ拾う設計にしていた。

Codex の次ラウンドの敵対的レビューで、これでもまだ穴があることが赤テストで露出した:

- `packed_refs_rewrites_after_birth_still_emit_head_event`

シナリオはこう:

1. watcher 起動時には `packed-refs` が存在しない
2. セッション中に `packed-refs` が作られる
3. その後、同じ `packed-refs` が in-place rewrite される

現行設計では phase 2 が落ちた。理由は単純で、`packed-refs` 専用 watcher は起動時にしか張られず、fallback の common git dir root watcher は `compare_contents = false` だったため、**file birth は見えても、その後の同一パス rewrite は保証されていなかった**。

これは ADR-0010 の negative consequence に書いた懸念が即座に実害化した形で、watch root の表現がまだ「起動時点のファイル存在」に引きずられていたと言える。

## Decision

common git dir root (`<common_git_dir>/`) を **常設の non-recursive watcher** として持ち、`compare_contents = true` にする。

具体的な git state watch root は次の 3 本に固定する:

1. `<per-worktree git_dir>/HEAD` (`NonRecursive`, `compare_contents=true`)
2. `<common_git_dir>/refs` (`Recursive`, `compare_contents=true`)
3. `<common_git_dir>/` (`NonRecursive`, `compare_contents=true`)

`packed-refs` 専用 watcher の有無で分岐しない。`packed-refs` が最初から存在しても、後から生えても、更新されても、すべて common git-dir root watcher で拾う。

Matcher は引き続き `HEAD` / current branch ref / `packed-refs` だけを baseline-affecting path として扱うので、root watcher が `config` や `description` を見ても `GitHead` には昇格しない。

## Consequences

- ポジティブ:
  - `packed-refs` が late birth した後の in-place rewrite も `GitHead` として観測できる。
  - watch root の shape が起動時のファイル存在に依存しなくなり、設計が単純になる。
  - `objects/**` や `logs/**` は相変わらず recursive poll 対象に入らないので、ADR-0010 の性能防衛は維持される。
- ネガティブ:
  - common git dir root 直下の file contents を常時 compare するため、`config` や `description` など無関係な root-level file にも poll のコストがかかる。ただし root 直下だけで、recursive `.git` walk よりははるかに小さい。
  - `HEAD` と common root watcher の両方が root-level files を見うるため、burst には重複イベントが混ざりうる。app 側の drain + coalescing 前提で吸収する。
- 影響範囲:
  - `src/watcher.rs`: `git_state_watch_roots()`, watcher tests
  - `docs/adr/README.md`: ADR index
  - `plans/v0.1-mvp.md`: surprise / decision log

## Alternatives Considered

- **`packed-refs` が作られた瞬間に watcher を動的に張り替える**: 却下。専用 watcher を hot-add するための状態管理が増えるわりに、common root non-recursive watch で十分に解決できる。
- **common git-dir root は watch するが `compare_contents = false` のまま維持する**: 却下。今回の failure がまさにこれで、birth 後の in-place rewrite を保証できない。
- **`packed-refs` 専用 watcher と common root watcher を両方持つ**: 却下。意味論が重複し、起動時存在の分岐も残る。常設 common root の方が単純。

## References

- 関連 ADR: [ADR-0008](0008-dynamic-watcher-and-health-split.md), [ADR-0010](0010-macos-poll-fallback-and-targeted-git-watch-roots.md)
- 関連 ExecPlan: `plans/v0.1-mvp.md`
- 関連テスト: `watcher::tests::packed_refs_rewrites_after_birth_still_emit_head_event`
