# ADR-0005: watcher は coalescing で吸収し、kizu 側に `.gitignore` フィルタを持たない

- **Status**: Accepted
- **Date**: 2026-04-15
- **Deciders**: Initial designer (Codex adversarial review を反映)

## Context

ExecPlan の当初版 (M2) は以下を前提にしていた:

1. worktree watcher のイベントを `ignore` crate (`GitignoreBuilder::new(root)` + `matched_path_or_any_parents`) でインプロセスフィルタする
2. `<root>/.git/` を文字列プレフィックスでハードコード除外する
3. watcher → app を `tokio::sync::mpsc::unbounded_channel` で繋ぎ、1 イベント = 1 `compute_diff` で処理する

Codex の adversarial review でこの設計に 3 つの correctness ホールが指摘された:

**ホール A (ignore crate API の誤用)** — `ignore 0.4.25` の `GitignoreBuilder::new(root)` はビルダの root path を設定するだけで、`.gitignore` パターンを一切ロードしない (`globs: vec![]`)。パターンを読むには `add(path)` を明示呼び出しする必要がある。さらに `Gitignore` 単体ではネストした `.gitignore` (例: `subdir/.gitignore`) を扱えず、ネスト対応の `dir::Ignore` API は全て `pub(crate)` のため外部公開されていない (`WalkBuilder` 経由でしか触れない)。

ソース確認: `/Users/annenpolka/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/ignore-0.4.25/src/gitignore.rs:317-333` (`new()` の実装), `392-419` (`add()` の実装), `dir.rs:155-830` (全関数が `pub(crate)`).

つまり当初プランの実装は黙って **何もフィルタしない** matcher を作るだけで、`target/` などへの書き込みが watcher を素通りして `compute_diff` を連発する。ビルド中やエディタ temp 書き込み中に kizu が無反応になる。

**ホール B (linked worktree 非対応)** — `<root>/.git/` を literal directory として watch / 除外する設計だが、`git worktree add` で作られた linked worktree では `.git` が**ファイル** (gitdir ポインタ) になり、実体は `~/.../main_repo/.git/worktrees/<name>/` 等にある。当初設計だと linked worktree で HEAD 変更を見逃し、git 内部 churn の除外も効かない。

**ホール C (背圧 / coalescing 不在)** — `unbounded_channel` + 1 イベント 1 `compute_diff` だと、バーストイベント時に消費が追いつかずキューが伸び続ける。エージェントや build が多ファイルを書く瞬間にこそキー応答が遅延し画面がステイルになり、メモリも青天井で成長する。

## Decision

これら 3 ホールを以下のように設計を組み直して塞ぐ:

### 1. `ignore` crate を v0.1 から完全に切る

`Cargo.toml` から `ignore` 依存を削除する。kizu 側に `.gitignore` フィルタを一切持たない。

理由: `git diff` 自体が `.gitignore` を尊重するので、無視対象ファイルへの書き込みで watcher イベントが発火しても、結果的な diff には何も現れない。下記の coalescing と組み合わせれば、`target/` などへの大量書き込みも「バースト → drain → 1 回の `compute_diff` 呼び出し → 結果ゼロ → 画面更新ゼロ」で吸収される。

トレードオフ: notify が `target/` 配下のイベントを debouncer に流す処理コスト (path string 処理 + timer reset) は残る。50000 ファイルの `cargo clean && cargo build` 級のバーストでは debouncer の内部バッファに 50000 path 分のメモリが 300ms 滞留する (推定数十 MB)。これは v0.1 では許容範囲とし、Surprises に「リアルなビルドバーストでパフォーマンスが落ちたら v0.1.1 で `target/` `node_modules/` のハードコード除外を追加する」と記録する。

### 2. git ディレクトリは `git rev-parse --absolute-git-dir` で解決する

`src/git.rs` に新しい関数 `git_dir(root: &Path) -> Result<PathBuf>` を追加し、`std::process::Command::new("git").args(["rev-parse", "--absolute-git-dir"]).current_dir(root)` で実体パスを取得する。返り値は通常リポでは `<root>/.git`、linked worktree では `<main_repo>/.git/worktrees/<name>` などの絶対パスになる。

`App.git_dir: PathBuf` として保持し、watcher は:

- worktree watcher のイベントから、絶対パスが `git_dir` のプレフィックスに一致するものを除外
- HEAD watcher は `<git_dir>/HEAD` と `<git_dir>/refs/` を直接 watch する

これで通常リポと linked worktree の両方で正しく動く。

### 3. watcher → app の境界に drain ベースの coalescing を入れる

チャネルは `tokio::sync::mpsc::unbounded_channel::<WatchEvent>` のままにする (sync コールバックから送る都合)。代わりに consumer 側で **1 イベント受信ごとに残りを `try_recv` で drain して 1 回だけ `compute_diff` を呼ぶ** パターンを徹底する:

```rust
loop {
    tokio::select! {
        Some(Ok(crossterm_ev)) = events.next() => { ... }
        Some(first) = watch_rx.recv() => {
            let mut worktree = matches!(first, WatchEvent::Worktree);
            let mut head     = matches!(first, WatchEvent::GitHead);
            // 同じターン内に積まれている残りを全部吸収
            while let Ok(more) = watch_rx.try_recv() {
                match more {
                    WatchEvent::Worktree => worktree = true,
                    WatchEvent::GitHead  => head     = true,
                }
            }
            if worktree { app.recompute_diff(); }
            if head     { app.mark_head_dirty(); }
        }
    }
    terminal.draw(|f| ui::render(f, &app))?;
    if app.should_quit { break; }
}
```

これにより、たとえば debouncer 1 サイクルで 100 件の `Worktree` イベントと 5 件の `GitHead` イベントが同時に積まれても、`compute_diff` は **1 回だけ** 呼ばれる。キューが伸び続けることはなく、ビルドバースト中もキー応答が遅延しない。

`tokio::sync::Notify` や `watch::channel::<u64>` への切り替えも検討したが、`Worktree` / `GitHead` の種別をシンプルに区別するためには `mpsc + drain` が最も読みやすい。

### 4. CI ガード

`.github/workflows/ci.yml` の bun + e2e ステップは `tests/e2e/package.json` の存在を `steps.e2e_scaffold.outputs.present == 'true'` でガードする。M6 で scaffold が land するまでステップ自体がスキップされ、`required check` を維持したまま M1〜M5 を incremental に PR で land できる。

## Consequences

**ポジティブ**:

- ignore crate API の誤用リスクが消える (依存自体を持たない)
- linked worktree が動く (kizu の対象ユーザー層に linked worktree ユーザーが少なくないはず)
- バーストイベント時もキー応答が遅延しない / メモリ膨張しない
- ci.yml が `tests/e2e/` 不在でも green になり、M1 から段階的に PR を land できる

**ネガティブ**:

- `target/` 等への大量書き込みで notify → debouncer の処理コストが残る (推定数十 MB の一時メモリ滞留)
- v0.1.1 で性能問題が見えたら `target/` `node_modules/` `.git/worktrees/` などのハードコード除外を追加する必要がある
- ネストした `.gitignore` を kizu 側で完全に尊重したい場合は `git check-ignore --stdin` を batch で呼ぶ実装に切り替える必要がある (こちらも v0.1.1 候補)

**影響範囲**:

- `Cargo.toml` から `ignore = "0.4.25"` を削除
- `src/git.rs` に `pub fn git_dir(root: &Path) -> Result<PathBuf>` を追加
- `src/watcher.rs` の `start` シグネチャを `pub fn start(root: &Path, git_dir: &Path) -> Result<WatchHandle>` に変更
- `src/app.rs` の `App` に `git_dir: PathBuf` を追加し、`bootstrap` で `git::git_dir` を呼んで保持
- `src/app.rs` の `run` ループに drain ベースの coalescing を組み込む
- `.github/workflows/ci.yml` の bun ステップを `hashFiles` ガード化

## Alternatives Considered

- **`Gitignore::new(root.join(".gitignore"))` に修正してネスト未対応のまま使う**: ルートの `.gitignore` だけは効くようになるが、`subdir/.gitignore` (テストプロジェクトでは普通) が無視され、ユーザーの期待とズレる。パッチワーク。却下。
- **`WalkBuilder::new(root).build()` で全ファイル列挙して HashSet キャッシュ**: 起動時に 1 回歩いて非無視ファイル集合を作り、watcher イベントごとにセットに照合。ただし新規ファイル追加 / 既存ファイル削除に追従できず staleness が出る。却下。
- **`git check-ignore --stdin` を debounce ごとに batch shell out**: 1 spawn / debounce-cycle なので頻度は許容範囲だが、shell out 実装と stdin パイプ管理が増える。v0.1 では coalescing だけで十分なので延期。v0.1.1 候補として残す。
- **`tokio::sync::Notify` + 共有可変ステート**: 「dirty フラグ」を `AtomicBool` で持ち、`Notify::notified()` で起こす設計。種別 (Worktree/GitHead) を分けたいので 2 つの Notify を用意する必要があり、`mpsc + drain` より行数が増えるためそちらを優先。

## References

- 関連 ExecPlan: `plans/v0.1-mvp.md` Milestone 2, Decision Log
- 関連 ADR: ADR-0002 (notify-debouncer-full), ADR-0003 (tokio)
- Codex adversarial review: 2026-04-15 turn (Findings 1-4)
- ignore crate ソース: `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/ignore-0.4.25/src/{gitignore.rs,dir.rs}`
- git linked worktree: <https://git-scm.com/docs/git-worktree>
- `git rev-parse --absolute-git-dir`: <https://git-scm.com/docs/git-rev-parse>
