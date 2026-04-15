# ADR-0012: macOS PollWatcher の worktree 監視は compare_contents を有効にする

- **Status**: Accepted
- **Date**: 2026-04-15
- **Deciders**: annenpolka, Codex adversarial review loop
- **Extends**: [ADR-0010](0010-macos-poll-fallback-and-targeted-git-watch-roots.md), [ADR-0011](0011-common-git-root-watch-for-late-packed-refs.md)

## Context

ADR-0010 / ADR-0011 で macOS watcher は `PollWatcher` fallback に移り、git state 側の `HEAD` / `refs` / `packed-refs` は安定化した。しかし adversarial review を続けると、worktree 側にも metadata-only polling の穴が残っていることが赤テストで露出した:

- `same_size_existing_file_rewrite_emits_worktree_event`

シナリオは単純で、既存ファイル `same.txt` を `alpha\n` から `omega\n` へ **同サイズ**で上書きするだけ。これが `WatchEvent::Worktree` を出さず、2 秒待っても沈黙した。

起動直後の blind window 問題は前 commitで app 側の startup self-heal recompute で塞いでいるが、今回の failure はそれとは別物で、watcher が「steady-state なのに同サイズ rewrite を見落とす」ことを意味する。既存の reactive e2e がたまたま長い文字列 rewrite (`hi` → `rewritten content`) を使っていたため、今まで見えていなかっただけだった。

`PollWatcher` の metadata-only モードを維持する限り、same-size / same-metadata-resolution の上書きは今後も再発しうる。v0.1 の kizu は「保存したら diff が更新される」ことが存在理由なので、この穴は許容できない。

## Decision

macOS で `PollWatcher` を使う場合、**worktree watcher でも `compare_contents = true` を有効にする**。

つまり `spawn_worktree_debouncer()` は `new_kizu_debouncer(WORKTREE_DEBOUNCE, true, ...)` を使う。これで create / delete / size change だけでなく、same-size in-place rewrite も file contents の差分として検出できる。

加えて e2e も強化し、`tests/e2e/reactive.test.ts` の既存ファイル更新ケースを same-size rewrite (`println!(\"hi\")` → `println!(\"ok\")`) に変更する。unit の red test と black-box e2e の両方でこの性質を pin する。

## Consequences

- ポジティブ:
  - same-size existing-file rewrite でも `Worktree` が確実に出る。
  - reactive e2e が「たまたま size が変わる更新」しか見ていなかった穴を塞げる。
  - startup blind window の self-heal と組み合わさり、macOS PollWatcher path の correctness がかなり締まる。
- ネガティブ:
  - worktree root 全体に対して contents compare を行うため、macOS fallback の CPU / I/O コストは上がる。大きい repo では metadata-only より高価。
  - poll fallback が native watcher より重いという ADR-0010 の tradeoffをさらに強める。macOS path だけ別 budget で考える必要がある。
- 影響範囲:
  - `src/watcher.rs`: worktree debouncer config, watcher tests
  - `tests/e2e/reactive.test.ts`: same-size rewrite regression
  - `docs/adr/README.md`, `plans/v0.1-mvp.md`, `CLAUDE.md`

## Alternatives Considered

- **metadata-only のまま維持し、e2e fixture を長さの変わる rewrite に戻す**: 却下。テストを現実より弱くしてバグを隠すだけ。
- **app 側の定期 self-heal recompute で吸収する**: 却下。steady-state での same-size rewrite miss は watcher correctness の問題で、startup 補修だけでは足りない。
- **worktree も watch root を細かく分割して compare_contents を限定する**: 却下。編集対象ファイルは動的であり、v0.1 の実装面積に対して複雑すぎる。まずは correctness を優先する。

## References

- 関連 ADR: [ADR-0010](0010-macos-poll-fallback-and-targeted-git-watch-roots.md), [ADR-0011](0011-common-git-root-watch-for-late-packed-refs.md)
- 関連 ExecPlan: `plans/v0.1-mvp.md`
- 関連テスト: `watcher::tests::same_size_existing_file_rewrite_emits_worktree_event`, `tests/e2e/reactive.test.ts`
