# ADR-0017: per-project events dir でハッシュで worktree を分離する

- **Status**: Accepted
- **Date**: 2026-04-17
- **Deciders**: annenpolka, Claude

## Context

v0.3 のストリームモードでは、`hook-log-event` サブコマンドが AI エージェントの Write/Edit のたびに sanitized event を JSON ファイルとして書き出す。このファイルを TUI が notify で監視し、リアルタイムにストリーム view を更新する。

最初の実装では events dir を `<state_dir>/events/` にグローバル配置していたが、これは複数の worktree を同時に監視するユースケース (開発者が複数プロジェクトで並行して Claude Code を動かす) で以下の問題を起こす:

1. 異なる worktree の hook が同じディレクトリに書き込む → TUI A が別プロジェクトのイベントを誤って拾う
2. プロジェクト root 外のファイルパスを含むイベント (`~/.config/...` 等) は TUI 側でフィルタされるが、events dir はそれを知らずに書き続ける
3. `clean_stale_events` が worktree A の TUI 起動時にすべてのイベントを削除 → worktree B の TUI の live イベントも巻き添えで消える

### 選択肢

1. **events dir をグローバル `<state_dir>/events/` に置き、TUI 側で project root フィルタ**
   - メリット: 実装最小
   - デメリット: clean_stale_events の巻き添え問題、events dir 内に他プロジェクトのイベントが溜まる
2. **events dir を `<project_root>/.kizu/events/` に置く**
   - メリット: 分離が明示的
   - デメリット: 書き込み前に project root を知る必要があるが、hook-log-event は project root を stdin JSON の `cwd` で初めて知る。リポジトリに kizu のゴミを残すことになり `.gitignore` も必要
3. **events dir を `<state_dir>/events/<project_hash>/` で分離**
   - メリット: state_dir は XDG で標準化。ハッシュだけでディレクトリ名が決まるため shell-friendly、worktree 側に痕跡を残さない
   - デメリット: ハッシュ関数の選択が必要。`DefaultHasher` は Rust バージョン間で安定性を保証しない

## Decision

**選択肢 3** を採用する。

1. `paths::events_dir(root) = <state_dir>/events/<project_hash(root)>/`
2. `project_hash(root)` は `std::collections::hash_map::DefaultHasher` で path を hash し 16 進 16 文字で返す
3. hook-log-event は stdin JSON の `cwd` を `git::find_root` で repo root に解決してから `events_dir(root)` に書く (main.rs `run_hook_log_event`)
4. TUI 側は `events_dir(app.root)` を監視。他プロジェクトの events dir は別パスなので非干渉
5. TUI 起動時の `clean_stale_events` は**自プロジェクトの events dir のみ**を対象にする。他プロジェクトの live イベントは巻き添え削除しない

## Consequences

- **ポジティブ**:
  - プロジェクト A の TUI 起動が プロジェクト B の live stream を破壊しない
  - events dir の分離により、TUI のフィルタロジックが単純化される (`event.file_paths.starts_with(self.root)` のみでよい)
  - state_dir 直下が整理される (`~/Library/Application Support/kizu/events/<hash>/<timestamp>-<tool>.json`)
- **ネガティブ**:
  - **`DefaultHasher` の安定性は Rust ドキュメントで保証されていない**。rustdoc には "The internal algorithm is not specified, and so it and its hashes should not be relied upon over releases." と明記されている。現在は SipHash13 が固定的に使われているが、将来の Rust バージョンで変わる可能性がある
    - 実害: rebuild 後に project_hash が変わると、古い events dir が孤児化する
    - 緩和: `prune_event_log` が TTL 24h + 上限 1000 で自動 GC するため、孤児ディレクトリ内のファイルは 1 日以内に削除される。ディレクトリ自体は残るが数バイトなので放置
    - **同一 kizu バイナリ内では hash は決定的**なので、TUI と同時に動く hook-log-event は一貫した dir に書き込む。rebuild をまたぐケースのみが影響を受ける
  - ハッシュ衝突の可能性: 64-bit SipHash13 で 2 プロジェクト同時衝突の確率は実用上ゼロ (birthday bound は 2^32 プロジェクト)。懸念事項ではない
  - path 正規化を省略しているため、`/foo/bar` と `/foo/bar/` が別ハッシュになる。呼び出し側 (main.rs `run_hook_log_event`, `App::bootstrap`) は常に `git::find_root` の戻り値を使うため、この正規化がカノニカルになる
- **影響範囲**:
  - `src/paths.rs`: `events_dir`, `project_hash`
  - `src/hook.rs`: `write_event` (events_dir を per-project で引く)
  - `src/app.rs`: `clean_stale_events`, `handle_event_log` の project root フィルタ
  - `src/watcher.rs`: `spawn_events_dir_debouncer` (per-project events dir を watch)

## Alternatives Considered

- **SHA-256 や FNV-1a で決定論的ハッシュを採用**: 将来的な選択肢として残す。現状の実害は軽微 (TTL で GC される) なので追加依存を正当化しきれないが、v0.4+ で複数の kizu バイナリバージョンが並走するようになったら再検討する
- **ハッシュではなく path をディレクトリ名にエンコード**: 却下。`/` を `_` に置換するルールが衝突を生む (`/foo_bar` と `/foo/bar` が同じになる)。URL-encoding は読みにくく長さ制約もある
- **project-local な `.kizu/events/`**: 却下。worktree を汚す。`.gitignore` 管理の手間 (kizu init で何を触るかが増える)
- **events dir を使わず、unix socket / named pipe で TUI に直送**: 却下。実装コストが段違いに上がる。TUI 起動前のイベントをディスクに残したいという要件 (過去イベントの metadata 表示) にも合わない

## References

- 関連 ADR: [ADR-0016](0016-stream-mode-per-operation-diff.md) (stream モードの diff 算出戦略)
- 関連 ExecPlan: [`plans/v0.3.md`](../../plans/v0.3.md) (M1: `hook-log-event` 実装, M3: ストリームモード)
- 関連仕様: [`docs/SPEC.md`](../SPEC.md)
- 外部資料: [Rust DefaultHasher docs](https://doc.rust-lang.org/std/collections/hash_map/struct.DefaultHasher.html)
