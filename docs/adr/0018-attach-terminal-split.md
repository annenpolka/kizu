# ADR-0018: --attach はターミナル別のネイティブ分割コマンドを shell out する

- **Status**: Accepted
- **Date**: 2026-04-17
- **Deciders**: annenpolka, Claude

## Context

v0.3 で追加した `kizu --attach` は「現在のターミナルを分割し、片側に kizu TUI、もう片側にシェル (Claude Code を起動する場所) を立ち上げる」UX を提供する。ユーザーが毎回手動でターミナルを分割して kizu を起動する手間を解消することが目的。

ターゲットとするターミナルは 4 種: tmux, zellij, kitty, Ghostty。それぞれが独自の分割メカニズムを持つため、統一された API は存在しない。

### 選択肢

1. **各ターミナルの CLI コマンドを `std::process::Command` で shell out**
   - メリット: 依存ゼロ、ターミナルの公式分割動作をそのまま使える
   - デメリット: ターミナルごとにフラグが異なる、Ghostty は CLI ではなく AppleScript
2. **portable な pty マルチプレクサ crate (e.g. `portable-pty`) で自前で分割 UI を作る**
   - メリット: 単一コードパス、依存側でテスト可能
   - デメリット: ターミナル emulator の分割機能を再実装することになる。実装面積が爆発する
3. **ターミナル側のプラグイン/拡張で連携**
   - メリット: ターミナル native UX
   - デメリット: 4 種のエコシステム全てにプラグインを作ることになる

## Decision

**選択肢 1** を採用する。`src/attach.rs` で各ターミナルの分割コマンドを shell out し、`TerminalKind` enum でディスパッチする。

1. **検出優先順: tmux → zellij → kitty → ghostty** (SPEC.md の規定)
   - 環境変数で判定: `$TMUX` / `$ZELLIJ` / `$KITTY_LISTEN_ON` / `$TERM_PROGRAM=ghostty`
   - 設定ファイル `[attach].terminal` で上書き可能
2. **分割コマンド一覧**:
   | ターミナル | コマンド |
   |:---|:---|
   | tmux | `tmux split-window -h <kizu_bin>` |
   | zellij | `zellij run --floating -- <kizu_bin>` |
   | kitty | `kitty @ launch --type=window <kizu_bin>` |
   | Ghostty (macOS) | `osascript -e 'tell application "Ghostty" to tell front window to split horizontally with command "<kizu_bin>"'` |
3. **Ghostty は macOS 限定**。AppleScript 依存のため `#[cfg(target_os = "macos")]` でガードし、それ以外の OS では明示的エラーを返す
4. **AppleScript に埋め込む `kizu_bin` パスは escape する**。`"` と `\` をバックスラッシュエスケープする `escape_applescript_string` を用意し、万一 `current_exe()` が特殊文字を含むパスを返しても AppleScript の文字列コンテナを抜け出せないようにする
5. `kizu_bin` は `std::env::current_exe()` で解決する。これにより shell PATH に `kizu` が無い環境でも `kizu --attach` を実行したバイナリが確実に新しいペインで起動する
6. 分割された新しいペインの `kizu` は `--attach` なしで起動される → 再帰分割を防ぐ
7. `kitty @ launch` は `--type=window` を使う。SPEC の初稿では `--type=overlay` だったが、overlay はカレントウィンドウを覆うフルスクリーンポップアップで分割ではない。ユーザーが期待するのは横並び分割なので `window` (現タブの新規 window、現在のレイアウトに従って split) を採用し SPEC もそれに合わせた

## Consequences

- **ポジティブ**:
  - 各ターミナルの native 分割 UX (tmux の split-window、kitty の layout system 等) をそのまま享受できる
  - 依存クレート追加なし、実装が 140 行程度に収まる
  - 設定ファイルで `terminal = "tmux"` 等の override が可能なので、複数ターミナルが並走する環境でもユーザー選択を尊重できる
  - AppleScript escape により `current_exe()` がパスに `"` を含んでも injection を起こさない
- **ネガティブ**:
  - **ターミナル側のインストール状態に依存する**。`tmux split-window` 相当のフラグが将来変わると壊れる。ただし 4 種とも長期安定フラグで、CI では e2e テストの対象外 (pty 上のマルチプレクサ依存なので黒箱テストが難しい)
  - Ghostty は macOS のみ。Linux Ghostty ユーザーには `kizu --attach` は使えない (手動分割へのガイド文言を error message に含める)
  - kitty は `allow_remote_control yes` + `listen_on ...` が必須。ユーザー設定の前提が効かないと黙って失敗する。error message でユーザーにリダイレクトする必要がある
  - zellij `--floating` は**分割ではなくフローティングペイン**。分割 UX としての違和感があるが、zellij の通常 `run` コマンドは run した瞬間閉じる挙動があり、`--floating` のほうが Claude Code との並走に向く。将来的に zellij のレイアウト機構が整備されたら `--floating` 外しを検討する
- **影響範囲**:
  - `src/attach.rs`: `TerminalKind`, `detect_terminal`, `split_and_launch`, `resolve_terminal`, `escape_applescript_string`
  - `src/main.rs`: `--attach` early return
  - `src/config.rs`: `AttachConfig.terminal`
  - `docs/SPEC.md`: 対応ターミナル表 (kitty `--type=window` に修正)

## Alternatives Considered

- **portable pty multiplexer で自前 UI**: 却下。ターミナル emulator の機能を再実装するのは kizu のスコープ外。ターミナル native UX を提供できない
- **Ghostty を Linux でも対応** (e.g. `ghostty +split` CLI があれば): 却下。現時点で Ghostty Linux に分割を自動化する CLI/API は存在しない
- **kitty で `--type=os-window` を使う** (新規 OS ウィンドウとして開く): 却下。ユーザーは既存ウィンドウ内での分割を期待している
- **zellij の `--floating` を外す**: 却下 (現状)。通常 `run` は短命プロセス想定で、長期稼働する kizu TUI に不向き。zellij 側の API 整理を待つ
- **分割方向をユーザー設定で変えられるようにする** (`-h` vs `-v`): 一旦却下。最小設定で v0.3 をリリースするため、分割方向は水平固定にする。要望があれば v0.4 で追加

## References

- 関連 ADR: [ADR-0016](0016-stream-mode-per-operation-diff.md), [ADR-0017](0017-per-project-events-dir.md) (v0.3 の姉妹 ADR)
- 関連 ExecPlan: [`plans/v0.3.md`](../../plans/v0.3.md) (M4: `--attach` ターミナル自動分割)
- 関連仕様: [`docs/SPEC.md`](../SPEC.md#対応ターミナルv03---attach)
- 外部資料:
  - [tmux split-window](https://man.openbsd.org/tmux.1)
  - [zellij run](https://zellij.dev/documentation/commands#run)
  - [kitty launch](https://sw.kovidgoyal.net/kitty/launch/)
  - [Ghostty AppleScript](https://ghostty.org/docs/features/applescript)
