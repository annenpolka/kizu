# ADR-0003: tokio を非同期ランタイムとして採用する

- **Status**: Accepted
- **Date**: 2026-04-15
- **Deciders**: Initial designer

## Context

kizu の TUI イベントループは複数の非同期な情報源を統合する必要がある:

- crossterm からのキー入力 / リサイズイベント
- `notify-debouncer-full` からのファイルシステム変更通知（worktree / `.git/HEAD`）
- 将来 (v0.2): Claude Code hook サブコマンドの stdin 読み取り、JSON パース、ファイル書き込み
- 将来 (v0.3): `/tmp/kizu-events/` のストリーム監視、ターミナル分割サブプロセス起動

v0.1 単体では「`event::poll(100ms)` + `try_recv` のシングルスレッドポーリング」でも要件は満たせるが、v0.2/v0.3 で hook 統合とストリームモードを追加する際に書き換えコストが発生する。また、TUI の反応性を「100ms ポーリング遅延あり」ではなく「即時イベント駆動」で確保したい。

選択肢:

1. **`event::poll(100ms)` + `try_recv` (シングルスレッド sync)** — 最小コード量。100ms の遅延余地（実害なし）。v0.2/v0.3 で書き換えが必要になる可能性。
2. **input thread + 統合 mpsc (sync)** — 完全イベント駆動。スレッド間 panic 伝搬と input thread 停止の課題。v0.2/v0.3 でも sync で書き続けることになる。
3. **tokio + `crossterm::event::EventStream`** — 完全イベント駆動。v0.2/v0.3 の hook / stream mode を async で素直に書ける。ランタイム依存と async の伝染がコスト。

## Decision

tokio を v0.1 から非同期ランタイムとして採用する。`current_thread` flavor を使用し、**マルチスレッドランタイムは使わない**。

具体的な構造:

- `main` を `#[tokio::main(flavor = "current_thread")]` で起動する
- `app::run` は `async fn` とし、`tokio::select!` で `crossterm::event::EventStream` と watcher の `tokio::sync::mpsc::UnboundedReceiver<WatchEvent>` を統合する
- `notify-debouncer-full` のコールバック (sync) からは `UnboundedSender::send` で tokio チャネルに送る（sync 文脈から呼べる API のため bridge 不要）
- `git::compute_diff` 等の git CLI shell out は **`std::process::Command` のまま** とし、`tokio::process::Command` には移行しない（current_thread ランタイムで数十 ms の sync 呼び出しが走っても TUI 体感に影響しないため）
- `App::handle_key` 等のドメインロジックは sync の純粋関数として保ち、async の伝染を `app::run` の境界で止める
- `crossterm` の `event-stream` feature を有効にする
- `tokio` は `["rt", "macros", "sync", "time"]` の最小 feature セットで導入する

## Consequences

**ポジティブ**:

- TUI の反応性が「即時イベント駆動」になり、キー入力遅延が消える
- v0.2 の hook サブコマンド（stdin 読み取り、JSON パース、ファイル書き込み）を `tokio::io` で素直に書ける
- v0.3 のストリームモード（`/tmp/kizu-events/` 監視）を同じ `tokio::select!` パターンで拡張できる
- アイドル時の CPU 使用率がゼロになる（poll-based の数 % より低い）

**ネガティブ**:

- ビルド時間が増える（tokio + futures で初回 +20〜30 秒）
- 依存ツリーが膨らむ（pin-project-lite, mio, parking_lot 等が間接依存に入る）
- 読者が tokio の知識を要求される（`async fn`, `tokio::select!`, `Pin`）
- watcher コールバックの sync → async 境界で `UnboundedSender` のクローン管理が必要

**影響範囲**:

- `Cargo.toml` に tokio 依存追加、crossterm に `event-stream` feature 追加
- `src/main.rs` が `#[tokio::main]` 起動になる
- `src/app.rs` の `run()` が `async fn` になる
- `src/watcher.rs` の `WatchHandle.events` が `tokio::sync::mpsc::UnboundedReceiver<WatchEvent>` になる
- 純粋なドメインロジック (`git::*`, `App::handle_key`) は sync を維持する → テスト容易性は変わらない
- `current_thread` flavor の選択により `Send` 制約に縛られない（マルチスレッドランタイムに将来切り替える際は `Send` 要件が追加で発生する点に注意）

## Alternatives Considered

- **シングルスレッド sync (`poll(100ms)` + `try_recv`)**: コード量は最小。v0.1 だけ見れば十分機能するが、v0.2 で hook 統合を追加する際にイベントループを書き換える必要が出る可能性が高く、二度手間。却下。
- **input thread + 統合 mpsc (sync)**: tokio を避けつつイベント駆動にできるが、`event::read()` がブロッキングなので input thread の停止方法がクリーンでない（メインスレッド終了に依存する）。さらに v0.2 の hook で async I/O が欲しくなった際にこの構造のまま async に乗せ替えるのが面倒。却下。

## References

- 関連 ExecPlan: `plans/v0.1-mvp.md` Milestone 3
- 関連 ADR: ADR-0002 (notify-debouncer-full)
- 外部資料:
  - <https://docs.rs/tokio/latest/tokio/attr.main.html>
  - <https://docs.rs/crossterm/latest/crossterm/event/struct.EventStream.html>
  - <https://ratatui.rs/recipes/apps/handle-events/> (async pattern)
