# ADR-0013: watcher health は source-aware に追跡する

- **Status**: Accepted
- **Date**: 2026-04-15
- **Deciders**: annenpolka, Codex adversarial review loop
- **Extends**: [ADR-0008](0008-dynamic-watcher-and-health-split.md)

## Context

ADR-0008 で `watcher_health` を `last_error` から分離したが、その状態は単一の `Healthy | Failed(String)` enum だった。これは「watcher 全体が 1 本のデバウンサである」なら十分だが、現在の kizu watcher はそうではない。

実際には以下の複数ソースが同時に動いている:

- worktree watcher
- per-worktree `HEAD`
- common git `refs`
- common git-dir root

敵対的レビューで見えた問題は、**どれか 1 本の live event が来るだけで watcher 全体を Healthy に戻してしまう**ことだった。例えば:

1. `git.refs` watcher が死ぬ
2. その後 worktree watcher は普通に `Worktree` を送り続ける
3. 現行実装は `Worktree` を見て `watcher_health = Healthy` に戻す

この時点で git baseline drift detection はまだ壊れているのに、footer から警告が消える。これは partial failure を full recovery に見せる誤診で、監視ツールとして危険。

## Decision

`watcher_health` は **source-aware** に追跡する。

具体的には:

1. `WatchSource` enum を導入する
   - `Worktree`
   - `GitPerWorktreeHead`
   - `GitRefs`
   - `GitCommonRoot`

2. `WatchEvent` に source を持たせる
   - `Worktree`
   - `GitHead(WatchSource)`
   - `Error { source: WatchSource, message: String }`

3. `WatcherHealth` は `BTreeMap<WatchSource, String>` とする
   - 同じ source から live event が来たときだけ、その source の failure を clear
   - mixed burst で同じ source の success と error が同居したら error を優先
   - footer は failure map が空でなければ `⚠ WATCHER` を出し、message 群を連結して表示する

## Consequences

- ポジティブ:
  - partial failure が他 source の live event で隠れなくなる。
  - watcher health の recovery 条件が source 単位で明確になる。
  - future で git watcher root を増減させても、health model がそのまま拡張できる。
- ネガティブ:
  - `WatchEvent` と `WatcherHealth` の形が少し重くなる。
  - footer の error text が長くなりやすい。
  - git source が複数あるため、1 つの GitHead event では「別の git source の recovery」は証明できないという、より厳密な運用になる。
- 影響範囲:
  - `src/watcher.rs`: source-tagged event emission
  - `src/app.rs`: `WatcherHealth`, `handle_watch_burst` state machine
  - `src/ui.rs`: footer rendering

## Alternatives Considered

- **単一 enum のまま維持し、「どれか 1 つ live event が来たら Healthy」**: 却下。partial failure を隠す。
- **worktree / git_dir の 2 層だけで追跡する**: 却下。git watcher 自体が複数 root なので、1 本の git source event では別 root の recovery を証明できない。
- **watcher error は一度出たら restart まで sticky**: 却下。transient recovery が見えていても warning が消えず、今度は pessimistic すぎる。

## References

- 関連 ADR: [ADR-0008](0008-dynamic-watcher-and-health-split.md), [ADR-0010](0010-macos-poll-fallback-and-targeted-git-watch-roots.md), [ADR-0011](0011-common-git-root-watch-for-late-packed-refs.md)
- 関連 ExecPlan: `plans/v0.1-mvp.md`
- 関連テスト: `handle_watch_burst_does_not_clear_git_failure_when_worktree_recovers`, `handle_watch_burst_does_not_clear_other_git_source_failure`
