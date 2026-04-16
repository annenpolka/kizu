# ADR-0015: worktree watch から build output を除外する

- **Status**: Proposed
- **Date**: 2026-04-15
- **Deciders**: annenpolka, Codex

## Context

kizu の startup freeze を `src/app.rs::run()` の timing instrumentation (`KIZU_STARTUP_TIMING_FILE=/tmp/kizu-timing.log`) で切り分けたところ、起動時の大半を占めていたのは `watcher::start` の **+3.515001583s** だった。他の startup step はすべて 42ms 未満で、体感上の「起動してから 3.5 秒固まる」は watcher 初期化に集中している。

原因は `src/watcher.rs::spawn_worktree_debouncer()` が worktree root 全体を `RecursiveMode::Recursive` で watch していたことにある。ADR-0012 により macOS の worktree watcher は `PollWatcher + compare_contents=true` を採用しており、PollWatcher は初回 baseline を作るために watch 対象を走査し、各 file contents を比較できる状態にする。今回の repo では `target/` が 42,647 files / 約 1.3 GB あり、この初回 scan が startup を支配していた。

これは単なる benchmark 上のノイズではなく、kizu の UX を壊す実害がある。v0.1 の kizu は「起動したらすぐ bootstrap snapshot が見え、そのまま reactive に diff が追従する」ことが価値であり、巨大な build output を持つ repo で毎回 3.5 秒待たされるのは明確な regression である。ADR-0010/0012/0013 が要求する macOS PollWatcher fallback, `compare_contents=true`, source-aware health を維持したまま、この initial scan の面積だけを減らす必要がある。

## Decision

worktree watcher は root 全体を recursive watch しない。代わりに次の 2 層構成にする。

1. worktree root 自体は `NonRecursive` で watch する
2. `std::fs::read_dir(root)` で top-level child を列挙し、**directory のみ**を対象に、除外リストに含まれない child へ `Recursive` watch を追加する

除外リストは hardcoded short list とし、`.gitignore` は読まないし `git check-ignore` にも shell out しない。これは ADR-0005 の「kizu 側に ignore filter を持たない」を維持しつつ、watcher 起動時の scan cost を common build/cache directory に限定して外すためである。

最終的な除外名は次のとおり:

- `.git`
- `target`
- `node_modules`
- `.direnv`
- `.venv`
- `dist`
- `build`
- `.next`
- `.turbo`
- `.cache`
- `.gradle`
- `.mvn`
- `.idea`
- `.vscode`
- `__pycache__`

`.git/` は既に ADR-0010/0011 の git state watch roots (`HEAD`, `refs`, common git root) で別経路により監視されているため、worktree walker から外しても coverage は失わない。root-level file write は root の `NonRecursive` watch で引き続き観測する。`read_dir(root)` に失敗した場合は panic せず、既存の `WatchEvent::Error { source: Worktree, ... }` 経路で app に surfacing し、watcher health に載せる。

## Consequences

- ポジティブ: startup 時に `target/` や `node_modules/` など巨大 directory tree の initial content scan を避けられる。今回の freeze は watcher::start の 3.5 秒に集中していたため、これらを外すことで watcher startup は file-count ratio に見合って大きく短縮されるはずである。
- ネガティブ: top-level directory 名が除外リストと一致する場合、その配下の source 変更は worktree watcher では見えない。たとえば本当に `target/` という名前の source directory を持つ project では変更が観測されない。
- 影響範囲: 問題が最も顕著なのは macOS PollWatcher path だが、watch root の削減自体は Linux/Windows でも startup work を減らすので全 platform に適用する。

## Alternatives Considered

- **`watcher::start` を `spawn_blocking` して UI だけ先に出す**: 却下。blank screen は減っても 3.5 秒待つ本質は変わらず、watcher が armed するまでの遅さを隠すだけで症状対策に留まる。
- **`ignore` crate で `.gitignore` を解釈する**: 却下。ADR-0005 の「kizu 側に `.gitignore` filter を持たない」に反する。watch policy が repo-local ignore semantics に引きずられ、実装面積も増える。
- **top-level child ごとに `git check-ignore` を shell out する**: 却下。child 列挙のたびに git invocation を追加するのは遅く、watcher startup path に新しい外部プロセス依存を持ち込む。
- **lazy initial scan**: 却下。ADR-0012 で必要な `compare_contents=true` は既存 contents の baseline が前提であり、baseline を遅延させると same-size rewrite 検出の correctness を崩す。

## References

- 関連 ADR: [ADR-0005](0005-watcher-coalescing-no-ignore-filter.md)
- 関連 ADR: [ADR-0010](0010-macos-poll-fallback-and-targeted-git-watch-roots.md)
- 関連 ADR: [ADR-0012](0012-worktree-content-compare-on-macos-poll.md)
- 関連 ADR: [ADR-0013](0013-source-aware-watcher-health.md)
- 関連 ExecPlan: `plans/v0.2.md`（今回の freeze 発見は Surprises & Discoveries に記録候補）
