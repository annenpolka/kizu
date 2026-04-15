# ADR-0002: ファイル監視は notify-debouncer-full を採用する

- **Status**: Accepted
- **Date**: 2026-04-15
- **Deciders**: Initial designer

## Context

kizu は worktree と `.git/HEAD` / `.git/refs` を監視し、変更をトリガに `git diff` を再計算する。OS のファイルシステム通知はバースト性が高く、1 回の保存で複数のイベントが届く（エディタの atomic-replace、一時ファイル、rename による書き換えなど）。そのままハンドラに流すと `git diff` が 1 秒間に何十回も走り、TUI が描画追従できなくなる。

SPEC.md は以下のデバウンス値を規定している:

- worktree ファイル変更: **300ms**
- `.git/HEAD` 変更: **100ms**

選択肢:

1. **生の `notify` crate + 自前デバウンス**: `HashMap<PathBuf, Instant>` で最終イベント時刻を持ち、タイマーで一括 flush する。
2. **`notify-debouncer-mini`**: notify 公式の軽量デバウンサ。単一のデバウンス間隔のみサポート。
3. **`notify-debouncer-full`**: notify 公式のフル機能版。rename の相関、同一パスのイベント集約、複数 watcher の管理をサポートする。

## Decision

`notify-debouncer-full 0.7` を採用する。worktree watcher と `.git` watcher を別インスタンスとして起動し、それぞれ 300ms / 100ms のデバウンス間隔を与える。生イベントは `WatchEvent::Worktree` / `WatchEvent::GitHead` の 2 種類に正規化して `mpsc::channel` で app 層に流す。

## Consequences

**ポジティブ**:

- atomic-replace 系エディタ（vim の `:w` が典型）の rename イベントが正しく「同一ファイルの書き換え」として集約される
- 自前デバウンス実装のエッジケース（タイマー漏れ、ロック競合）を回避できる
- notify 本体のアップデートに追従しやすい（公式ラッパなのでバージョン整合が取れている）

**ネガティブ**:

- デバウンサが内部スレッドを持つため、プロセスのスレッド数が増える（worktree + `.git` で最低 2 本）
- `notify-debouncer-full` の API は `notify` の低レベル API より抽象度が高く、細かい制御（例: 特定イベントの即時 flush）が難しい
- 依存が 1 つ増える（ただし notify crate のエコシステム内なので実質的な重みはわずか）

**影響範囲**:

- `src/watcher.rs` は `notify-debouncer-full` の `DebouncedEventKind` に依存する
- `WatchHandle` はデバウンサの所有権を保持する必要がある（Drop で監視が止まるため）

## Alternatives Considered

- **生 notify + 自前デバウンス**: 実装量が増えるわりにメリットがない。rename 相関を正しく実装するのが意外と難しく、バグの温床になる。却下。
- **notify-debouncer-mini**: 単一デバウンス間隔しか持てず、worktree と `.git/HEAD` で別レートにできない。SPEC の 300ms / 100ms 分離に合わない。却下。

## References

- 関連 ExecPlan: `plans/v0.1-mvp.md` Milestone 2
- 関連仕様: `docs/SPEC.md` の「TUI層」節（デバウンス値の記載）
- 外部資料: <https://docs.rs/notify-debouncer-full/>
