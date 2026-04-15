# ADR-0001: git CLI を shell out して diff を計算する

- **Status**: Accepted
- **Date**: 2026-04-15
- **Deciders**: Initial designer

## Context

kizu は worktree の変更を unified diff として取得して TUI に描画する必要がある。Rust で git の diff を得る方法は大きく 2 つある:

1. **git2-rs (libgit2 バインディング)**: インプロセスで git オブジェクトを読める。高速で、外部プロセス起動のオーバーヘッドがない。
2. **`git` CLI を `std::process::Command` で呼ぶ**: git 本体の挙動をそのまま使える。`--no-renames`、`--find-renames`、`-M`、`-C`、`--diff-filter` などの高度なフラグが全て利用可能。

kizu は Claude Code 等の AI エージェントが生成した diff を観察するのが目的で、**表示される diff は `git diff` と完全に一致している必要がある**。libgit2 と git 本体は diff アルゴリズムや rename 検出に微妙な差異があり、ユーザーが `git diff` で確認したものと kizu で見るものがズレると「指差し」の精度が崩れる。

## Decision

diff 計算は `std::process::Command` で `git diff --no-renames <baseline_sha> --` を shell out して行う。untracked ファイルは `git status --porcelain` の `??` エントリから合成する。libgit2 / git2-rs は採用しない。

worktree の git ルート特定 (`git rev-parse --show-toplevel`) と HEAD SHA 取得 (`git rev-parse HEAD`) も同様に shell out する。

## Consequences

**ポジティブ**:

- `git diff` との出力一致が**定義上**保証される（同じバイナリが出力しているため）
- `--no-renames` を含む git のフラグ空間を再実装せずにそのまま使える
- libgit2 のバージョンアップに振り回されない
- バイナリサイズと依存ツリーが小さくなる（libgit2 は C FFI でビルド時間も増える）

**ネガティブ**:

- 変更が起きるたびに `git` プロセスを起動するオーバーヘッド（macOS で数 ms〜十数 ms）
- `git` が PATH にないと動かない（現実的には全環境にある前提で問題ない）
- stdout のパースが必要。unified diff パーサを自前で書く（`src/git.rs::parse_unified_diff`）

**影響範囲**:

- `src/git.rs` は `std::process::Command` に強く依存する
- 将来パフォーマンスがボトルネックになったら、ホットパス（`.gitignore` チェックなど）だけ in-process 実装に差し替える余地を残す

## Alternatives Considered

- **git2-rs**: rename 検出や diff のエッジケースで `git diff` と微妙にズレる可能性があり、「ユーザーの認知モデルと一致させる」という kizu の要件に合わない。却下。
- **gix (gitoxide)**: Pure Rust で魅力的だが、2026-04 時点で diff まわりの API が安定しておらず、kizu の要件（rename 検出の無効化など）を全てカバーできているか検証コストが高い。将来の再検討候補として保留。

## References

- 関連 ExecPlan: `plans/v0.1-mvp.md`
- 関連仕様: `docs/SPEC.md` の「技術スタック」「アーキテクチャ」節
- 参考実装: diffpane (Go/bubbletea) も同様に git CLI を shell out している
