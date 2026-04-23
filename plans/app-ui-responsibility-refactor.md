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
- [x] (2026-04-23 20:21:24Z) 敵対的削減 pass: settings 系 agent installer を統合し、line-number 付き renderer wrapper と `FileViewVisualIndex` の重複実装を削除
- [x] (2026-04-23 20:21:24Z) 対象テスト確認: `init::tests` 18 件、`ui::tests` 63 件、`visual_index` 2 件、`file_view` 16 件が成功
- [x] (2026-04-23 20:21:24Z) 削減後の full gate (`just ci`) を通す
- [x] (2026-04-23 20:36:04Z) 自律削減 pass: search/scar 入力編集、quit/page/picker/search navigation、Cline hook 判定、app/ui test fixture の重複を集約
- [x] (2026-04-23 20:36:04Z) 追加対象テスト確認: `search_input` 5 件、`scar_comment` 7 件、`picker` 6 件、`file_view` 16 件などが成功
- [x] (2026-04-23 20:36:04Z) 追加削減後の full gate (`just ci`) を通す
- [x] (2026-04-23 20:46:07Z) 仕上げ削減 pass: file-view scroll helper、`file_view_state` / `install_search` test fixture helper を追加し、行数を増やした候補 helper は撤回
- [x] (2026-04-23 20:46:07Z) 仕上げ後の full gate (`just ci`) を通す
- [x] (2026-04-23 20:57:14Z) post-commit 削減 pass: UI test render buffer / row text / cell search helper、scar comment test helper、RowKind row lookup helper を追加して test fixture 重複を削除
- [x] (2026-04-23 20:57:14Z) post-commit 削減後の full gate (`just ci`) を通す
- [x] (2026-04-23 21:03:51Z) 共通 key dispatch / fixture 削減 pass: normal/file-view 共通 action key handler と `single_added_app` test fixture を追加
- [x] (2026-04-23 21:03:51Z) 共通 key dispatch / fixture 削減後の full gate (`just ci`) を通す
- [x] (2026-04-23 21:07:25Z) thin wrapper cleanup pass: picker/search navigation の薄い wrapper と UI test の local `fake_app` / `binary_file` wrapper を削除
- [x] (2026-04-23 21:07:25Z) thin wrapper cleanup 後の full gate (`just ci`) を通す

## Surprises & Discoveries

- Observation: 作業開始時点で `src/app.rs`、`src/git.rs`、`src/ui.rs` に未コミット差分がある。
  Evidence: `git status --short --branch` は `## main...origin/main` と 3 つの modified file を表示した。差分は行番号 gutter、footer 密度選択、file view reflow など v0.5 系の整理であり、本リファクタはこれを巻き戻さない。

- Observation: 既存の Rust テストはリファクタ前に全緑だった。
  Evidence: `cargo test --all-targets --all-features` は `test result: ok. 467 passed; 0 failed` で終了した。

- Observation: `src/ui.rs` から footer を切り出すと、`key_label` だけは help overlay にも使われているため、footer module へ移すべきではなかった。
  Evidence: `rg -n "key_label\\(" src/ui.rs src/stream.rs src/app.rs` は help overlay の `help_row(key_label(...))` 呼び出しを示した。最終的に `key_label` は `src/ui.rs` に残し、`src/ui/footer.rs` には footer 専用 helper だけを置いた。

- Observation: overlay 抽出後に `src/ui.rs` の widget imports が余り、通常の `cargo test` では warning としてだけ出た。
  Evidence: `cargo test --all-targets --all-features` が `unused imports: Block, Borders, Clear, ListItem, ListState, List, and Wrap` を警告した。`src/ui.rs` の import を `widgets::Paragraph` のみに縮め、`cargo clippy --all-targets --all-features -- -D warnings` で warning がないことを確認した。

- Observation: `src/init.rs` の Claude Code / Qwen Code / Codex installer は同じ `merge_hooks_into_settings` 呼び出しを agent 名と PostToolUse の有無だけ変えて重複していた。
  Evidence: `install_claude_code` と `install_qwen` は `hook-post-tool`、`hook-log-event`、`hook-stop` の同一構成で、`install_codex` は Stop のみだった。`install_settings_hook_agent` に統合後、`init::tests` 18 件が成功した。

- Observation: `src/ui.rs` の `render_*_numbered` 関数群は本体描画を呼んで gutter を差し込むだけの wrapper だった。
  Evidence: `render_diff_line_numbered`、`render_diff_line_wrapped_numbered`、`render_file_view_line_numbered`、`render_file_view_line_wrapped_numbered` を `add_line_number_gutters` へ置換しても `ui::tests` 63 件が成功した。

- Observation: `FileViewVisualIndex` は `VisualIndex` と同じ prefix sum、`visual_y`、`visual_height`、`total_visual`、`logical_at` を再実装していた。
  Evidence: `FileViewVisualIndex` を削除し、`VisualIndex::build_lines` へ統合した後、`visual_index` 2 件と `file_view` 16 件が成功した。

- Observation: search 入力と scar comment 入力は、文字追加、Backspace、Esc、Enter の小さな editor state machine をそれぞれ手書きしていた。
  Evidence: `TextInputKeyEffect` と `handle_text_input_edit` に集約した後、`search_input` 5 件、`scar_comment` 7 件、`handle_key_c` 2 件が成功した。

- Observation: normal mode と file view mode の `q` / Ctrl-c、Ctrl-d / Ctrl-u、picker の上下移動、search の next / prev は、同じ条件分岐を別々の match arm で再実装していた。
  Evidence: `is_quit_key`、`control_page_delta`、`move_picker_cursor`、`search_jump_by` へ寄せた後、`handle_key_q`、`file_view_j_k`、`shift_j`、`scroll_by_in_wrap_mode`、`search_jump`、`picker` の対象テストが成功した。

- Observation: `src/app.rs` と `src/ui.rs` のテストは、`DiffLine`、`Hunk`、`FileDiff`、`App` fixture 生成を別々に持っていた。
  Evidence: `src/test_support.rs` に共有 fixture を追加して両 test module から使った後、`cargo test --all-targets --all-features` は 467 件成功した。

- Observation: Cline の install / teardown と JSON hook removal は、kizu hook command の文字列判定を複数箇所に持っていた。
  Evidence: `contains_kizu_hook_command` と `remove_json_hooks_with_report` に寄せた後、`init::tests` を含む full gate が成功した。

- Observation: `src/ui.rs` の file-view renderer に `render_file_view_line_block` helper を入れる案は、見通しは少し良くなるが行数を増やした。
  Evidence: helper 導入後の `wc -l` で `src/ui.rs` は 3890 行から 3912 行に増えたため、変更を撤回した。

- Observation: `src/init.rs` の teardown removal mark helper は意味的には素直だが、今回の削減軸では純増だった。
  Evidence: helper 導入後の `src/init.rs` は 2116 行から 2122 行に増えたため、変更を撤回した。

- Observation: test-only fixture helper は、追加行数を差し引いても app/ui の重複を削れた。
  Evidence: `file_view_state` と `install_search` を `src/test_support.rs` に追加した後、`src/app.rs` は 10486 行から 10441 行、`src/ui.rs` は 3890 行から 3830 行になり、`file_view` 16 件、`search` 19 件、full `just ci` が成功した。

- Observation: UI test の terminal rendering と buffer 走査は、各 assertion が低レベルの `TestBackend` 構築や nested loop を持つことで、実際に検証したい UI 条件を読みにくくしていた。
  Evidence: `render_buffer`、`buffer_row_text`、`first_cell_matching`、`buffer_has_cell` へ寄せた後、`src/ui.rs` は 3830 行から 3730 行になり、`ui::tests` 63 件と full `just ci` が成功した。

- Observation: scar comment と header-row 系の app tests は、同じ一時ファイル読み取り、scar overlay 起動、`RowKind` 探索を繰り返していた。
  Evidence: `read_temp_file`、`open_scar_comment_app`、既存 `find_first_row_matching` 利用へ寄せた後、`src/app.rs` は 10441 行から 10386 行になり、`scar_comment` 7 件、`file_header` 10 件、`hunk_header` 25 件、full `just ci` が成功した。

- Observation: normal mode と file-view mode は、help / ask / reject / comment / wrap / line-number / undo の同じ action key dispatch を別々に持っていた。
  Evidence: `handle_common_action_key` へ集約した後、`handle_key_` 13 件、`file_view` 16 件、`scar_comment` 7 件、`pound_key` 2 件、`w_key` 1 件、`undo` 8 件、full `just ci` が成功した。

- Observation: app/ui のテストは、単一ファイル・単一 added 行の fixture を何十行も繰り返していた。
  Evidence: `single_added_app` を `src/test_support.rs` に追加し、app と ui の該当 fixture を置換した後、`src/app.rs` は 10386 行から 10281 行、`src/ui.rs` は 3730 行から 3661 行になり、`build_layout` 8 件、`search` 19 件、`ui::tests` 63 件、full `just ci` が成功した。

- Observation: `picker_cursor_down` / `picker_cursor_up` と `search_jump_next` / `search_jump_prev` は、内部 helper に固定値を渡すだけの薄い wrapper だった。
  Evidence: 呼び出し元を `move_picker_cursor(±1)` と `search_jump_by(±1)` に置換して wrapper を削除した後、`picker` 6 件、`search_jump` 4 件、full `just ci` が成功した。

- Observation: UI test module の local `fake_app()` と `binary_file()` は、`test_support` の helper を薄く包むだけだった。
  Evidence: `app_with_files(Vec::new())` と `timed_binary_file(..., 0)` を直接使う形にして wrapper を削除した後、`ui::tests` 63 件と full `just ci` が成功した。

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

- Decision: 2nd pass は module extraction よりも、重複 wrapper と二重実装の削除を優先する。
  Rationale: 単に別ファイルへ移すだけではコード量は減らない。ユーザー要望は「もっと敵対的に大胆にレビューして、コード量の削減」なので、挙動を既存テストで固定した上で不要な抽象境界を消す。
  Date/Author: 2026-04-23 20:21:24Z / Codex

- Decision: settings.json 系 installer だけを `install_settings_hook_agent` に統合し、Cursor / Cline / Gemini は agent 固有実装のまま残す。
  Rationale: Cursor は `hooks.json` の flat schema、Cline は hook script file、Gemini は hook mechanism なしで、無理に統合すると条件分岐が増えて逆に読みにくくなる。
  Date/Author: 2026-04-23 20:21:24Z / Codex

- Decision: search/scar 入力編集は汎用 helper に寄せるが、Enter 時の commit 処理は各 overlay に残す。
  Rationale: 文字編集は同じ状態機械だが、検索の commit は match index 更新、scar comment の commit は file write と undo stack 更新で副作用が異なる。共通化の境界を編集操作だけにすると、削減しつつ責務の混線を避けられる。
  Date/Author: 2026-04-23 20:36:04Z / Codex

- Decision: app/ui の test fixture 共有は `#[cfg(test)] mod test_support` に限定し、production crate surface には出さない。
  Rationale: fixture 重複は削れるが、本番コードから見える helper にすると依存関係が濁る。`src/main.rs` の test-only module 宣言に閉じることで、テストの読みやすさだけを改善できる。
  Date/Author: 2026-04-23 20:36:04Z / Codex

- Decision: Cursor user-scope teardown helper は production path から外れたため `#[cfg(test)]` にする。
  Rationale: `run_teardown` は path を表示する `remove_json_hooks_with_report` を直接使うようになり、旧 helper は fake home を注入するテスト境界としてだけ必要になった。test-only にすることで dead code warning を消しつつ既存テストの意図を保つ。
  Date/Author: 2026-04-23 20:36:04Z / Codex

- Decision: 行数が増えるだけの抽象化は、この pass では採用しない。
  Rationale: 今回の指示は「敵対的に」「コード量の削減」であり、読み味だけの helper は優先度が低い。`render_file_view_line_block` と teardown mark helper は実装後に行数が増えたため撤回し、削減に効いた helper だけ残す。
  Date/Author: 2026-04-23 20:46:07Z / Codex

- Decision: `file_view_state` と `install_search` は `src/test_support.rs` に置く。
  Rationale: app/ui の test module が同じ `FileViewState` / `SearchState` fixture を繰り返していた。test-only helper に閉じると production code の依存を増やさず、テストの儀式だけを削れる。
  Date/Author: 2026-04-23 20:46:07Z / Codex

- Decision: post-commit pass は production 挙動に触れず、test-only boilerplate の削減に限定する。
  Rationale: 直前のコミットで production の大きな重複削除は full gate 済みだった。次の安全な削減余地は、UI buffer rendering、scar temp file setup、row lookup といったテスト儀式であり、ここを先に畳むと後続の production refactor をレビューしやすくできる。
  Date/Author: 2026-04-23 20:57:14Z / Codex

- Decision: common action key helper は normal mode と file-view mode の共通集合だけを扱い、follow / picker / revert / search / reset / editor は normal mode に残す。
  Rationale: 共通集合は同じ副作用を呼ぶだけだが、normal mode 専用 action は mode 境界を越えると挙動変更になる。削減のために mode semantics を混ぜない。
  Date/Author: 2026-04-23 21:03:51Z / Codex

- Decision: `single_added_app` は `src/test_support.rs` に置き、app/ui 両方の test module から使う。
  Rationale: fixture は app/ui の両方に同じ形で散っている。共有 test-only helper にすると production code を汚さず、各テストの本文を検証したい状態だけに寄せられる。
  Date/Author: 2026-04-23 21:03:51Z / Codex

- Decision: ただの direction wrapper は削除し、呼び出し側で `±1` を明示する。
  Rationale: `move_picker_cursor(1)` / `search_jump_by(-1)` は十分に読める。中間 wrapper があると分岐の実体を追うためにジャンプが増え、コード量も増える。
  Date/Author: 2026-04-23 21:07:25Z / Codex

## Outcomes & Retrospective

Stream mode の差分構築を `src/stream.rs` へ、footer 描画を `src/ui/footer.rs` へ、help/picker overlay 描画を `src/ui/overlays.rs` へ切り出した。`src/app.rs` は 10820 行から 10690 行へ、`src/ui.rs` は 4989 行から 4195 行へ減った。v0.5 行番号まわりの既存未コミット差分は巻き戻さず、責務分割だけを重ねた。

2nd pass では、単なる file split ではなく重複削除に寄せた。Claude Code / Qwen Code / Codex の settings hook installer を `install_settings_hook_agent` に統合し、`src/ui.rs` の numbered renderer wrapper 4 本を `add_line_number_gutters` に置換し、`FileViewVisualIndex` を削除して `VisualIndex::build_lines` に統合した。差分は `src/app.rs`、`src/init.rs`、`src/ui.rs` の 3 ファイルで 256 insertions / 508 deletions、純減 252 行。完了時点の行数は `src/app.rs` 10650、`src/ui.rs` 4017、`src/init.rs` 2130。

自律削減 pass では、search/scar 入力編集、normal/file-view の共通キー、picker/search navigation、Cline hook 判定、app/ui test fixture をまとめた。`src/test_support.rs` は test-only の共有 fixture として追加した。2nd pass 完了時点からさらに `src/app.rs` は 10650 行から 10441 行へ、`src/ui.rs` は 4017 行から 3830 行へ、`src/init.rs` は 2130 行から 2116 行へ減った。`src/test_support.rs` 153 行と `src/main.rs` の test-only module 宣言 2 行を差し引いても、この pass だけで約 255 行の純減になった。

検証は `just ci` が成功した。Rust unit tests は 467 件成功、release build 成功、e2e は 35 件成功 / 0 件失敗。残る課題は、まだ `src/app.rs` が 1 万行を超えていること、`src/ui.rs` も diff 行描画と file view 描画を抱えていることである。次の削減候補は file view state の module 化、diff/file view renderer の境界整理、`init.rs` の Cursor / Cline install/teardown の schema 別 adapter 化である。

post-commit 削減 pass では、`src/app.rs` と `src/ui.rs` の test-only boilerplate だけを削った。`src/app.rs` は 10441 行から 10386 行へ、`src/ui.rs` は 3830 行から 3730 行へ減った。差分は 2 ファイルで 153 insertions / 308 deletions、純減 155 行。検証は `just ci` が成功し、Rust unit tests は 467 件成功、release build 成功、e2e は 35 件成功 / 0 件失敗だった。

共通 key dispatch / fixture 削減 pass では、normal/file-view の共通 action key を `handle_common_action_key` に寄せ、単一 added 行の fixture を `single_added_app` として共有化した。`src/app.rs` は 10386 行から 10281 行へ、`src/ui.rs` は 3730 行から 3661 行へ、`src/test_support.rs` は 153 行から 161 行になった。差分は 3 ファイルで 92 insertions / 258 deletions、純減 166 行。検証は `just ci` が成功し、Rust unit tests は 467 件成功、release build 成功、e2e は 35 件成功 / 0 件失敗だった。

thin wrapper cleanup pass では、picker/search navigation の direction wrapper と UI test module の local wrapper を削除した。`src/app.rs` は 10281 行から 10259 行へ、`src/ui.rs` は 3661 行から 3653 行へ減った。差分は 2 ファイルで 14 insertions / 44 deletions、純減 30 行。検証は `just ci` が成功し、Rust unit tests は 467 件成功、release build 成功、e2e は 35 件成功 / 0 件失敗だった。

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

2nd pass 後の追加証拠は以下である。

    src/app.rs        10650 lines
    src/ui.rs          4017 lines
    src/init.rs        2130 lines

    git diff --stat
    src/app.rs  | 150 ++++++++-------------
    src/init.rs | 188 +++++++++++----------------
    src/ui.rs   | 426 ++++++++++++++++++------------------------------------------
    3 files changed, 256 insertions(+), 508 deletions(-)

    cargo test --all-targets --all-features init::tests -- --nocapture
    18 passed

    cargo test --all-targets --all-features ui::tests -- --nocapture
    63 passed

    cargo test --all-targets --all-features visual_index -- --nocapture
    2 passed

    cargo test --all-targets --all-features file_view -- --nocapture
    16 passed

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

自律削減 pass 後の追加証拠は以下である。

    src/app.rs          10486 lines
    src/ui.rs            3890 lines
    src/init.rs          2116 lines
    src/test_support.rs   118 lines
    src/main.rs           215 lines

    git diff --stat
    plans/app-ui-responsibility-refactor.md |  59 +++-
    src/app.rs                              | 536 +++++++++--------------------
    src/init.rs                             | 282 +++++++--------
    src/main.rs                             |   2 +
    src/ui.rs                               | 589 ++++++++------------------------
    5 files changed, 485 insertions(+), 983 deletions(-)

    src/test_support.rs
    118 lines added as a new test-only fixture module

    cargo fmt --all -- --check
    cargo clippy --all-targets --all-features -- -D warnings
    cargo test --all-targets --all-features
    test result: ok. 467 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

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

仕上げ削減 pass 後の追加証拠は以下である。

    src/app.rs          10441 lines
    src/ui.rs            3830 lines
    src/init.rs          2116 lines
    src/test_support.rs   153 lines
    src/main.rs           215 lines

    git diff --numstat -- src/app.rs src/init.rs src/main.rs src/ui.rs
    218  467  src/app.rs
    117  165  src/init.rs
      2    0  src/main.rs
    173  538  src/ui.rs

    src/test_support.rs
    153 lines added as a new test-only fixture module

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

post-commit 削減 pass 後の追加証拠は以下である。

    src/app.rs          10386 lines
    src/ui.rs            3730 lines
    src/test_support.rs   153 lines

    git diff --stat
    src/app.rs | 161 +++++++++++----------------------
    src/ui.rs  | 300 +++++++++++++++++++++----------------------------------------
    2 files changed, 153 insertions(+), 308 deletions(-)

    cargo test --all-targets --all-features scar_comment -- --nocapture
    7 passed

    cargo test --all-targets --all-features file_header -- --nocapture
    10 passed

    cargo test --all-targets --all-features hunk_header -- --nocapture
    25 passed

    cargo test --all-targets --all-features ui::tests -- --nocapture
    63 passed

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

共通 key dispatch / fixture 削減 pass 後の追加証拠は以下である。

    src/app.rs          10281 lines
    src/ui.rs            3661 lines
    src/test_support.rs   161 lines

    git diff --stat
    src/app.rs          | 231 ++++++++++++++--------------------------------------
    src/test_support.rs |   8 ++
    src/ui.rs           | 111 +++++--------------------
    3 files changed, 92 insertions(+), 258 deletions(-)

    cargo test --all-targets --all-features handle_key_ -- --nocapture
    13 passed

    cargo test --all-targets --all-features build_layout -- --nocapture
    8 passed

    cargo test --all-targets --all-features search -- --nocapture
    19 passed

    cargo test --all-targets --all-features ui::tests -- --nocapture
    63 passed

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

thin wrapper cleanup pass 後の追加証拠は以下である。

    src/app.rs 10259 lines
    src/ui.rs   3653 lines

    git diff --stat
    src/app.rs | 36 +++++++-----------------------------
    src/ui.rs  | 22 +++++++---------------
    2 files changed, 14 insertions(+), 44 deletions(-)

    cargo test --all-targets --all-features picker -- --nocapture
    6 passed

    cargo test --all-targets --all-features search_jump -- --nocapture
    4 passed

    cargo test --all-targets --all-features ui::tests -- --nocapture
    63 passed

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

`/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/test_support.rs` は `#[cfg(test)]` の test-only module である。`src/app.rs` と `src/ui.rs` の test module からだけ使い、production code からは参照しない。主な helper は `diff_line`、`hunk`、`make_file`、`binary_file`、`app_with_files`、`file_view_state`、`install_search` で、同じ fixture 生成を複数 test module に置かないためのもの。
