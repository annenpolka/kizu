# ADR-0016: ストリームモードの per-operation diff 表示はスナップショット差分で近似する

- **Status**: Accepted
- **Date**: 2026-04-17
- **Deciders**: annenpolka, Claude

## Context

v0.3 の Stream モード (Tab で切り替わる「Claude Code が何をしたか」の時系列ビュー) は、**操作単位の diff**、つまり「この Write/Edit 操作だけで何の行が増えたか」を右ペインに表示する必要がある。メインの diff ビューは `git diff <baseline> --` の結果なので、hunk 境界は git が決めてしまい、連続する複数の Write/Edit がマージされて見える。ストリームモードの価値は **この境界を操作単位で切り直すこと**にある。

ただし、v0.2 の hook 層 (ADR 未採番、ExecPlan `plans/v0.2.md` Decision Log) で**イベントログにコード本文を書き出さない**というセキュリティポリシーを採用済みである。`tool_input.content` / `tool_response.output` はディスクに残らず、`SanitizedEvent` には `file_paths` / `tool_name` / `timestamp_ms` などメタデータだけを保存する。このため、操作単位 diff を「イベントファイルに保存された before/after 本文から算出する」方針はとれない。

### 選択肢

1. **per-operation の full before/after を専用の private state dir に保存してから diff**
   - メリット: 完全に正確な操作 diff が得られる
   - デメリット: v0.2 セキュリティポリシーが崩れる。コード本文がディスクに残るため、機密リポジトリで漏洩リスク
2. **TUI が実時間で `git diff` スナップショットを memory 上に保持し、イベントごとに差分を計算**
   - メリット: ディスクに新規データを書かない。既存の watcher 経由で fresh に保てる
   - デメリット: TUI 起動前のイベントは metadata しか残っていないので diff 表示不可。スナップショット管理のトリッキーさ
3. **`similar` / `diff-rs` 等の LCS ベースで真の diff-of-diff を計算**
   - メリット: 正確
   - デメリット: 追加依存。スナップショット間の差分は「hunk 境界の再シャッフル」を含むため、意味ある出力にするにはセマンティックな解釈層が別途必要
4. **per-file `(baseline_sha → current)` の生テキストを multiset 差分**
   - メリット: 依存ゼロ。実装最小
   - デメリット: 近似なのでエッジケースで精度が落ちる (重複行・順序入れ替え)

## Decision

**選択肢 2 + 4 の合成**を採用する。

1. TUI 起動時に `App::seed_diff_snapshots` が全ファイルの `git diff <baseline> -- <path>` 出力を `HashMap<PathBuf, String>` に seed する。これが「ストリーム開始時点の状態」となる
2. `WatchEvent::EventLog` を受け取るたびに `handle_event_log` が当該ファイルの `git diff <baseline> -- <path>` を再取得し、**前回スナップショットとの multiset 差分**を操作 diff として保存する
3. スナップショットを取得する `git::diff_single_file` は非ゼロ終了で `Err` を返す。呼び出し側はエラー時にスナップショット更新を**スキップ**し、前回状態を保持する (transient git エラーが次イベントの op_diff を破壊しないように)
4. 差分関数 `compute_operation_diff` は **HashMap ベースの multiset カウント**を使う。`HashSet<&str>` だと重複行 (`+}`, `+` (空行) 等) が 1 個にマージされ、新規コピーが常にドロップされる。カウントを消費しながら比較することで重複の新規コピーを保持する
5. TUI 起動前のイベント (clean_stale_events が削除しない永続イベントはゼロなので、実質的に過去イベントは表示しない) は metadata だけの表示にとどめる — per-operation diff は算出不能

## Consequences

- **ポジティブ**:
  - v0.2 セキュリティポリシー ("sanitized metadata only on disk") が維持される
  - 追加クレート依存ゼロ。`HashMap` と `git diff` だけで実装可能
  - watcher 経由のリアルタイム更新が既存経路に乗るため、ストリームモードのコードパスが memory-only で副作用を起こさない (UI 側は既存の scroll infrastructure を再利用)
  - transient な git 失敗が次イベントを破壊しない (ADR で明示)
- **ネガティブ**:
  - **multiset 差分は完全な diff-of-diff ではない**。以下のエッジケースで精度が落ちる:
    - 同一行が prev にあって curr では順序が入れ替わっただけの場合、multiset には差分が出ないため op_diff 表示からは漏れる (ただし「行の並び替えだけ」は実用上まれ)
    - baseline → edit → revert(=baseline) の後、revert 操作の op_diff は inverse(prev) だが、multiset 差分では空を返す。revert は UI 上 "[diff not captured]" になる
    - hunk 境界の再チャンクで勝手にコンテキスト行が前後して見える場合、未知のコンテキスト行が op_diff に混入する可能性がある (見た目のノイズに留まり、機能影響はなし)
  - TUI 起動前に発生したイベントは diff 表示不可。ExecPlan のワークフロー想定 (`--attach` で TUI を常駐) では問題にならないが、TUI を落として再起動するユースケースでは体験が悪い
  - `git::diff_single_file` を毎イベントで shell-out する。現在は hook が debounce されて 100ms 以上離れるため UI ブロックはしないが、将来的に MultiEdit で 10+ ファイルに一度に書き込むようなケースでは IO コストが積み上がる可能性あり
  - スナップショットの HashMap はファイル削除後も残る。ファイル数が膨大なリポジトリで TTL なしに溜まるのは好ましくないが、現状は `stream_events` が 1000 件でプルーニングされるため実害は限定的
- **影響範囲**:
  - `src/app.rs`: `App::diff_snapshots`, `handle_event_log`, `seed_diff_snapshots`, `compute_operation_diff`, `build_stream_files`, `parse_stream_diff_to_hunk`
  - `src/git.rs`: `diff_single_file` (非ゼロ終了で `Err` を返すよう変更)
  - `src/watcher.rs`: `WatchEvent::EventLog`, `spawn_events_dir_debouncer`
  - `src/hook.rs`: `SanitizedEvent`, `write_event`, `prune_event_log`

## Alternatives Considered

- **per-operation の full before/after を残す**: 却下。v0.2 セキュリティポリシーを破る
- **`similar` crate で真の diff-of-diff を計算**: 却下。追加依存のコストと、hunk 境界の再シャッフルが意味ある出力にならないことを確認した。multiset 近似で実用上は十分読める
- **hook が diff そのものを書き出す**: 却下。コード本文がディスクに残るため、ポリシー違反
- **TUI 起動前イベントを擬似 diff で補完する** (例: ファイル mtime と最終 git log から推測): 却下。常に間違ったデータを出すより、素直に metadata だけ表示するほうが誤解を招かない

## References

- 関連 ADR: [ADR-0005](0005-watcher-coalescing-no-ignore-filter.md) (watcher イベントの coalescing パターン)
- 関連 ExecPlan: [`plans/v0.3.md`](../../plans/v0.3.md) (M3: ストリームモード TUI)
- 関連仕様: [`docs/SPEC.md`](../SPEC.md#データフロー--2系統のデータソース)
- 外部資料: [delta](https://github.com/dandavison/delta), [similar](https://docs.rs/similar/)
