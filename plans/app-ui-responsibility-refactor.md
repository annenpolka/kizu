# app/ui 責務分割リファクタ

この ExecPlan はリビングドキュメントです。`Progress`、`Surprises & Discoveries`、`Decision Log`、`Outcomes & Retrospective` は作業の進行に合わせて更新します。

## Purpose / Big Picture

kizu の主要な振る舞いは既に動いているが、`/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/app.rs` が約 1.1 万行、`/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/ui.rs` が約 5 千行まで膨らんでいる。AI エージェントが継ぎ足した状態機械と描画ロジックが同じファイルに密集しており、次の機能追加やレビューで「どこを壊したか」を把握しにくい。

この変更では、ユーザーから見える動作を変えずに、Stream mode の差分構築と footer 描画を独立モジュールに切り出す。完了後は `cargo test --all-targets --all-features` が従来通り通り、開発者は `src/stream.rs` と `src/ui/footer.rs` を読むだけで、それぞれの責務を追えるようになる。

## Progress

- [x] (2026-04-23 19:55:51Z) ベースライン確認: `cargo test --all-targets --all-features` が 467 件成功
- [x] (2026-04-23 19:55:51Z) 現状調査: `src/app.rs` は 10820 行、`src/ui.rs` は 4989 行で、未コミット差分は v0.5 行番号まわりの既存リファクタ
- [x] (2026-04-23 20:05:17Z) `feat/v0.5.1-refactor` ブランチを作成し、v0.5.1 用の作業ブランチへ移動
- [x] (2026-04-23 20:05:17Z) Stream mode の差分構築を `src/stream.rs` に切り出し、既存テストを通す
- [x] (2026-04-23 20:05:17Z) footer 描画を `src/ui/footer.rs` に切り出し、既存 UI テストを通す
- [x] (2026-04-23 20:08:48Z) help overlay と picker overlay を `src/ui/overlays.rs` に切り出し、既存 UI テストを通す
- [x] (2026-04-23 20:08:48Z) `just ci` を通す
- [x] (2026-04-23 20:08:48Z) 実装結果と検証ログをこの ExecPlan に反映する

## Surprises & Discoveries

- Observation: 作業開始時点で `src/app.rs`、`src/git.rs`、`src/ui.rs` に未コミット差分がある。
  Evidence: `git status --short --branch` は `## main...origin/main` と 3 つの modified file を表示した。差分は行番号 gutter、footer 密度選択、file view reflow など v0.5 系の整理であり、本リファクタはこれを巻き戻さない。

- Observation: 既存の Rust テストはリファクタ前に全緑だった。
  Evidence: `cargo test --all-targets --all-features` は `test result: ok. 467 passed; 0 failed` で終了した。

- Observation: `src/ui.rs` から footer を切り出すと、`key_label` だけは help overlay にも使われているため、footer module へ移すべきではなかった。
  Evidence: `rg -n "key_label\\(" src/ui.rs src/stream.rs src/app.rs` は help overlay の `help_row(key_label(...))` 呼び出しを示した。最終的に `key_label` は `src/ui.rs` に残し、`src/ui/footer.rs` には footer 専用 helper だけを置いた。

- Observation: overlay 抽出後に `src/ui.rs` の widget imports が余り、通常の `cargo test` では warning としてだけ出た。
  Evidence: `cargo test --all-targets --all-features` が `unused imports: Block, Borders, Clear, ListItem, ListState, List, and Wrap` を警告した。`src/ui.rs` の import を `widgets::Paragraph` のみに縮め、`cargo clippy --all-targets --all-features -- -D warnings` で warning がないことを確認した。

## Decision Log

- Decision: 最初の大規模リファクタは全ファイル一括分割ではなく、`src/app.rs` の Stream mode 差分構築と `src/ui.rs` の footer 描画に限定する。
  Rationale: どちらも現在の巨大ファイルから明確に切り出せる責務で、既存テストが十分に振る舞いを pin している。状態機械全体を一度に解体すると、未コミットの v0.5 差分を巻き込みやすく、レビュー可能性も落ちる。
  Date/Author: 2026-04-23 19:55:51Z / Codex

- Decision: 新しい振る舞いは足さず、既存テストを characterization test として使う。
  Rationale: 今回は機能追加ではなくモジュール境界の整理であり、Red を作るべき新仕様がない。TDD の安全網として、リファクタ前に全テスト成功を確認し、切り出し後に同じテストが通ることを受け入れ基準にする。
  Date/Author: 2026-04-23 19:55:51Z / Codex

- Decision: `format_local_time` は `src/ui/footer.rs` に移し、`src/ui.rs` から `pub(crate) use footer::format_local_time;` で再公開する。
  Rationale: 時刻表示の実体は Stream header の footer 近傍 UI 表示に属するが、`src/stream.rs` から `crate::ui::format_local_time` として使う既存 interface を保つと、呼び出し側の変更を最小にできる。
  Date/Author: 2026-04-23 20:05:17Z / Codex

- Decision: help overlay と picker overlay は `src/ui/overlays.rs` にまとめ、`centered_rect` と picker 専用 truncation helper も同じ module に置く。
  Rationale: どちらも modal overlay 描画で、`src/ui.rs` の diff/file view 描画から独立している。`centered_line` は empty state で使われるため親 module に残すが、`centered_rect` は overlay 専用なので移動する。
  Date/Author: 2026-04-23 20:08:48Z / Codex

## Outcomes & Retrospective

Stream mode の差分構築を `src/stream.rs` へ、footer 描画を `src/ui/footer.rs` へ、help/picker overlay 描画を `src/ui/overlays.rs` へ切り出した。`src/app.rs` は 10820 行から 10690 行へ、`src/ui.rs` は 4989 行から 4195 行へ減った。v0.5 行番号まわりの既存未コミット差分は巻き戻さず、責務分割だけを重ねた。

検証は `just ci` が成功した。残る課題は、まだ `src/app.rs` が 1 万行を超えていること、`src/ui.rs` も diff 行描画と file view 描画を抱えていることである。次の分割候補は search/scar 入力状態、file view state、diff/file view renderer である。

## Context and Orientation

このリポジトリは Rust 製の TUI アプリケーションである。TUI は terminal user interface の略で、ブラウザではなく端末画面に描画するアプリを指す。kizu は AI コーディングエージェントが変更したファイルの diff をリアルタイムに表示し、必要に応じて scar と呼ばれる `@kizu[...]` コメントをファイルに挿入する。

`/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/app.rs` はアプリ状態、キー入力、diff 再計算、Stream mode、file view、検索、scar 操作をまとめて持つ。Stream mode は PostToolUse イベントログを操作履歴として表示するモードで、`StreamEvent` の diff snapshot から仮想的な `FileDiff` を組み立てる。現在その変換は `build_stream_files`、`parse_stream_diff_to_hunk`、`compute_operation_diff` として `app.rs` 末尾近くにある。

`/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/ui.rs` は ratatui による描画層である。footer は画面最下行の状態表示で、follow/manual、file view、picker、search、diagnostics、wrap/line-number 状態を横幅に応じて Full、Compact、Minimal の 3 密度から選ぶ。現在は `format_local_time` から `render_footer` までの footer 関連関数が diff 行描画や picker 描画と同じファイルにある。

`/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/main.rs` は crate の module 宣言を持つ。新しく `stream` module を追加する場合はここへ `mod stream;` を足す。`ui/footer.rs` は `ui.rs` の子 module にするため、`ui.rs` の先頭に `mod footer;` を足し、`footer::render_footer` を呼ぶ。

## Plan of Work

最初に `src/stream.rs` を追加し、`StreamEvent` を `app.rs` に残したまま、Stream mode の純粋な変換関数だけを移す。`build_stream_files` は `pub(crate)` とし、`app.rs` では `use crate::stream::{build_stream_files, compute_operation_diff};` で既存の呼び出しを維持する。`parse_stream_diff_to_hunk` は `stream.rs` 内の private helper にする。テスト module は `app.rs` に残し、`super::*` で従来通り関数名が見える状態を保つ。

次に `src/ui/footer.rs` を追加し、footer の型と helper を移す。`format_local_time` は Stream mode の file header prefix に必要なので `pub(crate)` で公開する。`render_footer` は `pub(super)` とし、親 module の `ui.rs` からのみ呼ぶ。`truncate_display` は footer 外にも同名用途があるため、最初の切り出しでは footer module 内に閉じ込め、既存の外部 helper と混ぜない。

最後に `ui.rs` から移動済み関数を削除し、imports を調整する。すべての編集は `apply_patch` で行う。formatter で生成される変更以外、shell redirection や Python write は使わない。

## Concrete Steps

作業ディレクトリは `/Users/annenpolka/ghq/github.com/annenpolka/kizu`。

まず現状確認を行う。

    git status --short --branch
    cargo test --all-targets --all-features

期待する結果は、既存の未コミット差分が表示され、テストが `467 passed` で終わることである。

次に `apply_patch` で `src/stream.rs` を追加し、`src/main.rs` と `src/app.rs` を更新する。その後、次を実行する。

    cargo test --all-targets --all-features

期待する結果は、Stream mode 関連テストを含む全 Rust テストが成功することである。

次に `apply_patch` で `src/ui/footer.rs` を追加し、`src/ui.rs` を更新する。その後、次を実行する。

    cargo fmt --all -- --check
    cargo test --all-targets --all-features

期待する結果は、format check が無出力で成功し、Rust テストが全件成功することである。

## Validation and Acceptance

受け入れ条件は、ユーザーから見える挙動が変わらず、責務境界だけが明確になることである。`cargo test --all-targets --all-features` を実行し、少なくとも 467 件の既存テストが成功することを確認する。特に `build_stream_files_converts_events_to_file_diffs`、`compute_operation_diff_preserves_duplicate_added_lines`、`line_numbers_hint_appears_in_footer_and_marks_state`、`responsive_footer_keeps_back_hint_when_file_view_path_is_long`、`render_footer_shows_last_error_in_red_when_set` が成功することが重要である。

目視での受け入れ条件は、`src/app.rs` から Stream diff 変換のまとまりが消え、`src/stream.rs` に集約されていること、`src/ui.rs` から footer の密度選択と diagnostics 表示のまとまりが消え、`src/ui/footer.rs` に集約されていることである。

## Idempotence and Recovery

この作業は既存ファイルを削除せず、関数の移動と module 宣言の追加だけで進める。途中で compile error が出た場合は、まず import と visibility を修正して再実行する。`apply_patch` が同じ編集で 3 回失敗した場合は作業を止める。既存の未コミット差分はユーザー由来として扱い、`git checkout --` や `git reset --hard` で戻さない。

`cargo fmt` は formatter による副作用書き込みとして許可される。formatter 以外のファイル作成、削除、編集はすべて `apply_patch` で行う。

## Artifacts and Notes

開始時点の主要な証拠は以下である。

    ## main...origin/main
     M src/app.rs
     M src/git.rs
     M src/ui.rs

    src/app.rs 10820 lines
    src/ui.rs   4989 lines

    test result: ok. 467 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

追加の検証結果は実装後に追記する。

完了時点の主要な証拠は以下である。

    ## feat/v0.5.1-refactor
     M src/app.rs
     M src/git.rs
     M src/main.rs
     M src/ui.rs
    ?? plans/app-ui-responsibility-refactor.md
    ?? src/stream.rs
    ?? src/ui/

    src/app.rs        10690 lines
    src/ui.rs          4195 lines
    src/stream.rs       136 lines
    src/ui/footer.rs    591 lines
    src/ui/overlays.rs  228 lines

    just ci
    cargo fmt --all -- --check
    cargo clippy --all-targets --all-features -- -D warnings
    cargo test --all-targets --all-features
    test result: ok. 467 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
    cargo build --release --locked
    cd tests/e2e && bun install --frozen-lockfile
    cd tests/e2e && KIZU_BIN="$(pwd)/../../target/release/kizu" bun test
    35 pass
    0 fail

## Interfaces and Dependencies

`/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/stream.rs` は次の interface を提供する。

    pub(crate) fn build_stream_files(events: &[crate::app::StreamEvent]) -> Vec<crate::git::FileDiff>
    pub(crate) fn compute_operation_diff(previous: &str, current: &str) -> String

`compute_operation_diff` は `app.rs` の `handle_event_log` から使われ、既存テストからも `super::compute_operation_diff` として見えるように `app.rs` 側で private import する。

`/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/ui/footer.rs` は次の interface を提供する。

    pub(crate) fn format_local_time(timestamp_ms: u64) -> String
    pub(super) fn render_footer(frame: &mut ratatui::Frame<'_>, area: ratatui::layout::Rect, app: &crate::app::App)

`format_local_time` は `src/stream.rs` の Stream header prefix 生成から使う。`render_footer` は `src/ui.rs` の `render` からのみ使う。追加 dependency は不要で、既存の `ratatui`、`unicode-width`、`libc` だけを使う。

`/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/ui/overlays.rs` は次の interface を提供する。

    pub(super) fn render_help_overlay(frame: &mut ratatui::Frame<'_>, area: ratatui::layout::Rect, app: &crate::app::App)
    pub(super) fn render_picker(frame: &mut ratatui::Frame<'_>, area: ratatui::layout::Rect, app: &crate::app::App)

help overlay は `app.config.keys` を読んでキー表示を組み立てる。picker overlay は `app.picker_results()`、`app.files`、`format_mtime` を使い、表示だけを担当する。状態更新やキー入力処理は引き続き `src/app.rs` に残す。
