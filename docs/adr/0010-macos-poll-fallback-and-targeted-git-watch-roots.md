# ADR-0010: macOS watcher は poll fallback を使い、git state watch root を HEAD / refs / packed-refs に絞る

- **Status**: Accepted
- **Date**: 2026-04-15
- **Deciders**: annenpolka, Codex adversarial review loop

## Context

Codex による敵対的レビューの継続中、`src/watcher.rs` の正イベント系テスト 4 本がすべて落ちた。最初は `BaselineMatcher` の path compare 不整合に見えたが、切り分けのために追加した最小 smoke test (`selected_kizu_backend_smoke_receives_create_event`) で、**kizu の wrapper を一切通さない `notify-debouncer-full + RecommendedWatcher` 単体でも create event が 10 秒届かない**ことが判明した。

これは `matcher` や tokio channel の問題ではなく、少なくともこのプロジェクトの実 macOS 環境では `notify` の推奨 backend (FSEvents) を前提にできないことを意味する。v0.1 の価値は「別ペインの編集が自動で画面に現れる」ことであり、watcher が静かに死ぬ状態は MVP の存在理由そのものを壊す。

一方で、単純に macOS 全面で `PollWatcher` へ切り替え、従来どおり `.git/` 全体を recursive + `compare_contents=true` で見張る案には新しい地雷がある。`common_git_dir` には `objects/pack/*.pack` や大きい `index` が存在しうるため、**25ms〜75ms 間隔の content compare で `.git` 全木を舐めるのはコストが悪すぎる**。今回の linked worktree 失敗も、必要なのは `.git` 全体ではなく session baseline を動かす 3 クラスの path (`HEAD`, current branch ref, `packed-refs`) だけだった。

さらに review で、watcher 起動時にまだ存在しない `refs/heads/<branch>` や `packed-refs` を `canonicalize()` すると tempdir symlink (`/var` vs `/private/var`) を吸収できず、あとから作られた path と比較不能になることも露出した。

## Decision

1. **macOS では `RecommendedWatcher` ではなく `PollWatcher` を backend として使う。**
   `new_kizu_debouncer()` を導入し、`#[cfg(target_os = "macos")]` では `new_debouncer_opt::<PollWatcher, ...>` を使う。poll interval は debounce window の 1/4 (`300ms -> 75ms`, `100ms -> 25ms`) とし、総遅延を SPEC の 300ms / 100ms から大きく外さない。

2. **git state の watch root は `.git` 全体ではなく `HEAD` / `refs` / `packed-refs` に絞る。**
   `git_state_watch_roots()` を追加し、watch 対象を次の 3 本に限定する:
   - `<per-worktree git_dir>/HEAD` を file watch (`NonRecursive`, `compare_contents=true`)
   - `<common git_dir>/refs` を recursive watch (`compare_contents=true`)
   - `<common git dir>/packed-refs` が存在すれば file watch (`compare_contents=true`)、存在しなければ `<common git dir>` を `NonRecursive` で watch して `packed-refs` の birth だけ拾う

   これにより linked worktree の branch ref 更新は引き続き見える一方、`objects/pack/**` や `logs/**` を poll content compare の対象から外せる。

3. **存在しない path の canonicalization は「最も近い既存祖先を canonicalize して、欠けている tail を付け戻す」方式にする。**
   `canonicalize_or_self()` を修正し、watcher 起動時にまだ存在しない `refs/heads/<branch>` や `packed-refs` でも tempdir symlink 差を吸収できるようにする。

## Consequences

- ポジティブ:
  - macOS 環境で watcher の正イベントが実際に届くようになり、`cargo test --all-targets --all-features` と tuistory e2e が再び green になる。
  - linked worktree の commit は common git dir の branch ref 更新として見え続ける。
  - poll fallback を導入しても `.git/objects/**` を content compare しないため、性能コストを session baseline に関係する path へ限定できる。
  - `refs/heads/<branch>` や `packed-refs` が watcher 起動時に存在しなくても、あとから作られた path を正しく matcher が認識できる。
- ネガティブ:
  - watcher backend が platform-specific になる。macOS とそれ以外で実装経路が分かれるため、今後の変更では両方を意識する必要がある。
  - poll fallback は native backend より CPU を使う。ただし watch root を絞ることで、コストは worktree 全体 + git state の最小面積に抑えられる。
  - `packed-refs` がセッション中に初めて作られた場合、その後も dedicated file watch へ昇格はしない。以後の更新は common git dir root watch の metadata 変化に依存する。
- 影響範囲:
  - `src/watcher.rs`: backend selection, watch root selection, canonicalization helper, watcher tests
  - `plans/v0.1-mvp.md`: surprise / decision log
  - `docs/adr/README.md`: ADR index

## Alternatives Considered

- **RecommendedWatcher のまま test sleep だけ伸ばす**: 却下。smoke test で wrapper なしでも 10 秒無反応だったため、テスト都合ではなく backend 前提の破綻。
- **PollWatcher に切り替えるが `.git/` 全体を recursive + `compare_contents=true` のまま維持する**: 却下。大きい `index` や pack files を高頻度 hash するコストが悪すぎる。
- **notify を捨てて git state だけ自前 polling loop を書く**: 却下。`notify-debouncer-full` の debounce / aggregation を捨てるほどの価値はなく、実装面積が無駄に増える。

## References

- 関連 ADR: [ADR-0002](0002-notify-debouncer-full.md), [ADR-0005](0005-watcher-coalescing-no-ignore-filter.md), [ADR-0008](0008-dynamic-watcher-and-health-split.md)
- 関連 ExecPlan: `plans/v0.1-mvp.md`
- 関連仕様: `docs/SPEC.md` の v0.1 節, watcher debounce 値
