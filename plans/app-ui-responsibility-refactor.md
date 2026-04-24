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
- [x] (2026-04-23 21:23:39Z) one-hunk fixture 削減 pass: `single_hunk_file` / `single_added_file` を追加し、app/ui tests の単一 hunk fixture 重複を削除
- [x] (2026-04-23 21:23:39Z) one-hunk fixture 削減後の targeted tests と full gate (`just ci`) を通す
- [x] (2026-04-23 21:32:22Z) app-under-10k fixture 削減 pass: single-file app fixture と hunk header row assertion を helper 化し、`src/app.rs` を 1 万行未満へ落とす
- [x] (2026-04-23 21:32:22Z) app-under-10k fixture 削減後の targeted tests、clippy、full gate (`just ci`) を通す
- [x] (2026-04-23 21:37:09Z) legacy fixture cleanup pass: 残った direct `FileDiff` 構築と `RowKind` 探索 closure を test helper に寄せる
- [x] (2026-04-23 21:37:09Z) legacy fixture cleanup 後の targeted tests、fmt/clippy、full gate (`just ci`) を通す
- [x] (2026-04-23 21:44:02Z) single-hunk app fixture pass: app/ui tests の `app_with_file(make_file(vec![hunk(...)]))` と単一 hunk `app_with_hunks` を `single_hunk_app` に寄せる
- [x] (2026-04-23 21:44:02Z) single-hunk app fixture 後の targeted tests、clippy、full gate (`just ci`) を通す
- [x] (2026-04-23 21:47:07Z) numbered added lines fixture pass: `line {i}` の長い added hunk fixture を `numbered_added_lines` に寄せる
- [x] (2026-04-23 21:47:07Z) numbered added lines fixture 後の targeted tests、clippy、full gate (`just ci`) を通す
- [x] (2026-04-23 21:51:16Z) added hunk fixture pass: Added 行だけの multi-hunk fixture を `added_hunk` に寄せる
- [x] (2026-04-23 21:51:16Z) added hunk fixture 後の targeted tests、clippy、full gate (`just ci`) を通す
- [x] (2026-04-23 21:55:07Z) added hunk app fixture pass: Added 行だけの single-hunk app fixture を `added_hunk_app` に寄せる
- [x] (2026-04-23 21:55:07Z) added hunk app fixture 後の targeted tests、clippy、full gate (`just ci`) を通す
- [x] (2026-04-23 21:59:34Z) remaining Added-only hunk literal cleanup pass: app/ui tests に残った raw `hunk(... diff_line(LineKind::Added, ...))` を `added_hunk` に寄せる
- [x] (2026-04-23 21:59:34Z) remaining Added-only hunk literal cleanup 後の targeted tests、clippy、full gate (`just ci`) を通す
- [x] (2026-04-23 22:04:11Z) numbered diff line fixture pass: prefix 付き / 同種 line vector 生成を `prefixed_diff_lines` と `diff_lines` に寄せる
- [x] (2026-04-23 22:04:11Z) numbered diff line fixture 後の targeted tests、clippy、full gate (`just ci`) を通す
- [x] (2026-04-23 22:07:29Z) UI buffer lookup pass: row/text run 探索と sticky header fixture を helper に寄せる
- [x] (2026-04-23 22:07:29Z) UI buffer lookup 後の `ui::tests`、clippy、full gate (`just ci`) を通す
- [x] (2026-04-23 22:10:48Z) scar real-fs fixture pass: 単一 added/context 行の tempdir-backed App 生成を helper に寄せる
- [x] (2026-04-23 22:10:48Z) scar real-fs fixture 後の scar/editor/undo targeted tests、clippy、full gate (`just ci`) を通す
- [x] (2026-04-23 22:13:40Z) deleted-row lookup pass: scar deleted-line tests の duplicated RowKind scan を helper に寄せる
- [x] (2026-04-23 22:13:40Z) deleted-row lookup 後の targeted tests、clippy、full gate (`just ci`) を通す
- [x] (2026-04-23 22:16:57Z) hunk file fixture pass: single-hunk `FileDiff` wrapper を `file_with_hunk` / `added_hunk_file` / `context_hunk_file` に寄せる
- [x] (2026-04-23 22:16:57Z) hunk file fixture 後の targeted tests、clippy、full gate (`just ci`) を通す
- [x] (2026-04-23 22:25:36Z) many-hunk performance investigation: 4,000 hunk probe で wrap viewport placement と hunk range lookup の hot path を特定
- [x] (2026-04-23 22:25:36Z) performance pass: `ScrollLayout` に hunk range / fingerprint cache を持たせ、wrap mode の `VisualIndex` を body width ごとに再利用
- [x] (2026-04-23 22:25:36Z) performance pass 後の targeted tests、clippy、full gate (`just ci`) を通す
- [x] (2026-04-23 22:30:54Z) navigation performance pass: hunk/run navigation の線形探索と nowrap `VisualIndex` 再構築を削る
- [x] (2026-04-23 22:30:54Z) navigation performance pass 後の targeted tests、clippy、full gate (`just ci`) を通す
- [x] (2026-04-23 22:34:18Z) search highlight performance pass: row ごとの search match projection を全 match scan から range lookup に変える
- [x] (2026-04-23 22:34:18Z) search highlight performance pass 後の targeted tests、clippy、full gate (`just ci`) を通す

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

- Observation: app/ui tests には `make_file(..., vec![hunk(1, vec![diff_line(LineKind::Added, ...)])], secs)` と `make_file(..., vec![hunk(1, lines)], secs)` が大量に残っていた。
  Evidence: `single_added_file` と `single_hunk_file` に置換した後、差分は 3 ファイルで 65 insertions / 241 deletions、純減 176 行になり、`ui::tests` 63 件、`picker` 6 件、`search` 19 件、`apply_reset` 6 件、`scroll_to` 5 件、`apply_computed_files` 3 件、`refresh_anchor` 3 件、`wrap` 31 件、full `just ci` が成功した。

- Observation: `src/app.rs` のテストには、単一ファイル app を作るためだけの `fake_app(vec![make_file(...)])` と、同じ hunk header row 探索 / assertion がまだ多く残っていた。
  Evidence: `app_with_file`、`app_with_hunks`、`single_added_hunk_file`、`hunk_header_row`、`assert_cursor_on_hunk_header` に寄せた後、`src/app.rs` は 10111 行から 9945 行へ減り、差分は 3 ファイルで 198 insertions / 350 deletions、純減 152 行になった。`seen` 12 件、`search` 19 件、`line_number` 27 件、`ui::tests` 63 件、`cargo clippy --all-targets --all-features -- -D warnings`、full `just ci` が成功した。

- Observation: app tests には、削除ファイルや reset 後の単純な追加ファイルを表すためだけに raw `FileDiff { ... }` を直書きする箇所と、file header / diff line row を探す closure が残っていた。
  Evidence: `single_deleted_file`、`file_header_row`、`diff_line_row` に寄せた後、`src/app.rs` は 9945 行から 9929 行へ減り、差分は 2 ファイルで 34 insertions / 44 deletions、純減 10 行になった。`scar_target` 5 件、`revert_confirm` 6 件、`populate_mtimes` 1 件、fmt、clippy、full `just ci` が成功した。

- Observation: app/ui tests には、単一 hunk だけを持つ `App` を作るために `app_with_file(make_file(...))` や `app_with_hunks(..., vec![hunk(...)])` を重ねる箇所が残っていた。
  Evidence: `single_hunk_app` を `src/test_support.rs` に追加し、該当 fixture を置換した後、`src/app.rs` は 9929 行から 9883 行へ、`src/ui.rs` は 3614 行から 3574 行へ減った。差分は 3 ファイルで 256 insertions / 333 deletions、純減 77 行になり、`ui::tests` 63 件、`search` 19 件、`scar_target` 5 件、`line_number` 27 件、clippy、full `just ci` が成功した。

- Observation: 長い hunk fixture の多くは、`(0..N).map(|i| diff_line(LineKind::Added, &format!("line {i}"))).collect()` をそのまま繰り返していた。
  Evidence: `numbered_added_lines` に集約した後、`src/app.rs` は 9883 行から 9866 行へ、`src/ui.rs` は 3574 行から 3566 行へ減った。差分は 3 ファイルで 21 insertions / 40 deletions、純減 19 行になり、`viewport_top` 6 件、`ui::tests` 63 件、clippy、full `just ci` が成功した。

- Observation: navigation / seen / scroll animation tests の multi-hunk fixture は、Added 行だけの hunk を作るために `hunk(..., vec![diff_line(LineKind::Added, ...)])` を何度も積んでいた。
  Evidence: `added_hunk` に寄せた後、`src/app.rs` は 9866 行から 9786 行へ減った。差分は 2 ファイルで 31 insertions / 101 deletions、純減 70 行になり、`handle_key_j` 1 件、`seen` 12 件、`scroll_to` 5 件、clippy、full `just ci` が成功した。

- Observation: app/ui tests の single-hunk app fixture にも、Added 行だけなのに `single_hunk_app(..., vec![diff_line(LineKind::Added, ...)])` を展開する箇所が大量に残っていた。
  Evidence: `added_hunk_app` に寄せた後、`src/app.rs` は 9786 行から 9640 行へ、`src/ui.rs` は 3566 行から 3542 行へ減った。差分は 3 ファイルで 35 insertions / 201 deletions、純減 166 行になり、`search` 19 件、`line_number` 27 件、`scar_target` 5 件、`ui::tests` 63 件、clippy、full `just ci` が成功した。

- Observation: app/ui tests には、Added-only だが `App` ではなく raw `Hunk` が必要な箇所に `hunk(... diff_line(LineKind::Added, ...))` が残っていた。
  Evidence: `added_hunk` へ追加で寄せた後、`src/app.rs` は 9640 行から 9619 行へ、`src/ui.rs` は 3542 行から 3535 行へ減った。差分は 2 ファイルで 15 insertions / 43 deletions、純減 28 行になり、`refresh_anchor` 3 件、`next_hunk` 4 件、`ui::tests` 63 件、fmt check、clippy、full `just ci` が成功した。

- Observation: app tests には、`a0..a7` / `b0..b7` / `ctx 0..ctx 29` のような番号付き DiffLine 生成と、同種 line vector の手組みが残っていた。
  Evidence: `prefixed_diff_lines` と `diff_lines` に寄せた後、`src/app.rs` は 9619 行から 9556 行へ、`src/test_support.rs` は 221 行から 219 行へ減った。差分は 2 ファイルで 25 insertions / 90 deletions、純減 65 行になり、`viewport_top` 6 件、`apply_computed_files` 3 件、fmt check、clippy、full `just ci` が成功した。

- Observation: UI tests の buffer 探索は、同じ text-run lookup と row scan を複数箇所で手書きしていた。
  Evidence: `first_text_run` / `row_containing` と既存 `prefixed_diff_lines` に寄せた後、`src/ui.rs` は 3535 行から 3516 行へ減った。差分は 1 ファイルで 20 insertions / 39 deletions、純減 19 行になり、`ui::tests` 63 件、fmt check、clippy、full `just ci` が成功した。

- Observation: scar/editor/undo tests は、tempdir に実ファイルを置いた上で単一 added/context 行 hunk を作る fixture を何度も展開していた。
  Evidence: `scar_app_with_added_line` / `scar_app_with_context_line` と simplified `open_scar_comment_app` に寄せた後、`src/app.rs` は 9556 行から 9506 行へ減った。差分は 1 ファイルで 76 insertions / 126 deletions、純減 50 行になり、`scar` 70 件、`scar_comment` 7 件、`scar_target` 5 件、`undo_scar` 3 件、`open_in_editor` 3 件、fmt check、clippy、full `just ci` が成功した。

- Observation: deleted-line scar e2e tests は、layout から最初の Deleted `DiffLine` row を探す同じ closure を二重に持っていた。
  Evidence: `first_diff_row_with_kind` に寄せた後、`src/app.rs` は 9506 行から 9480 行へ減った。差分は 1 ファイルで 26 insertions / 52 deletions、純減 26 行になり、`scar_on_` 3 件、fmt check、clippy、full `just ci` が成功した。

- Observation: app tests には、単一 hunk の `FileDiff` を作るためだけに `make_file(name, vec![hunk(...)], secs)` を展開する箇所がまだ残っていた。
  Evidence: `file_with_hunk` / `added_hunk_file` / `context_hunk_file` に寄せた後、`src/app.rs` は 9480 行から 9449 行へ、`src/test_support.rs` は 219 行から 240 行になった。差分は 2 ファイルで 43 insertions / 53 deletions、純減 10 行になり、`viewport_top` 6 件、`search_matches_rehydrate` 2 件、fmt check、clippy、full `just ci` が成功した。

- Observation: hunk 数が増えた時の重さは、通常表示の hunk 範囲探索と wrap 表示の visual index 再構築に集中していた。
  Evidence: 一時的な ignored probe で 4,000 hunks / 4 lines / 80 cells の app を作り、修正前は `current_hunk_range x1000: 23.746458ms`、`viewport_top nowrap x1000: 15.468542ms`、`viewport_placement wrap x200: 251.945917ms` だった。修正後は同じ probe で `current_hunk_range x1000: 2.292µs`、`viewport_top nowrap x1000: 2.958µs`、`viewport_placement wrap x200: 2.954625ms` になった。

- Observation: seen hunk がない通常状態でも、`build_layout` は各 hunk の full-line fingerprint を計算していた。
  Evidence: `build_layout no seen x100` は修正前 `42.202333ms`、修正後 `20.6235ms`。`seen_hunk_fingerprint` で該当 mark がある hunk だけ current fingerprint を計算するようにした。

- Observation: 描画 hot path を軽くした後も、`j` / `k` / `h` / `l` / follow mode の navigation は sorted な `hunk_starts` / `change_runs` を線形探索していた。
  Evidence: `next_hunk`、`prev_hunk`、`next_change`、`prev_change` は `.iter().find(...)` / `.iter().rev().find(...)` を持ち、`follow_target_row` は layout rows を前後に scan していた。`partition_point` helper と `hunk_ranges` / `file_first_hunk` 参照へ置換後、`handle_key_j`、`lowercase_j` 5 件、`lowercase_k` 3 件、`follow_target` 2 件、clippy が成功した。

- Observation: search highlight rendering は `SearchState.matches` が row 順であるにもかかわらず、可視 DiffLine ごとに全 match を scan していた。
  Evidence: `row_search_matches` は `state.matches.iter().enumerate().filter(|(_, m)| m.row == row_idx)` だった。`partition_point` で対象 row の contiguous range だけを見る形にした後、`search` 19 件、`ui::tests::search` 4 件、clippy が成功した。

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

- Decision: 単一 hunk / 単一 added 行の fixture は `src/test_support.rs` に寄せる。
  Rationale: これらは app/ui test の意味ではなく、`FileDiff` を作るための儀式だった。`single_hunk_file` と `single_added_file` に閉じることで、テスト本文を「どのファイルに何があるか」だけに近づけられる。
  Date/Author: 2026-04-23 21:23:39Z / Codex

- Decision: test-only fixture helper は、`src/app.rs` の行数削減に効く場合は app 構築単位まで上げる。
  Rationale: `FileDiff` 単体 helper だけでは `fake_app(vec![...])` の wrapper ノイズが残る。`app_with_file` と `app_with_hunks` にすると、テスト本文から `Vec` 構築の儀式を消しつつ production surface を増やさずに済む。
  Date/Author: 2026-04-23 21:32:22Z / Codex

- Decision: テストが raw field に依存していない `FileDiff` 構築は、低レベル struct literal ではなく名前付き helper に隠す。
  Rationale: 直書きの `FileDiff { content: DiffContent::Text(...), status: ... }` はテストの主張ではなく、状態を作るための配管だった。`single_deleted_file` のような helper 名にすると、テスト本文は「削除ファイルがある」という振る舞いだけを読める。
  Date/Author: 2026-04-23 21:37:09Z / Codex

- Decision: 単一 hunk の `App` fixture は `single_hunk_app` で直接作る。
  Rationale: `app_with_file(make_file(...))` と `app_with_hunks(..., vec![hunk(...)])` はテスト本文で一番頻出する構築儀式だった。単一 hunk がテストの前提なら helper 名に明示し、hunk vector の包装は test support に閉じる。
  Date/Author: 2026-04-23 21:44:02Z / Codex

- Decision: `line 0`, `line 1` のような番号付き added 行 fixture は `numbered_added_lines` で作る。
  Rationale: これらのテストは長い hunk や viewport の挙動を見たいだけで、`DiffLine` の生成式自体には意味がない。fixture 生成の loop を helper 名に置き換えると、テスト本文の焦点が hunk の長さに戻る。
  Date/Author: 2026-04-23 21:47:07Z / Codex

- Decision: Added 行だけの hunk fixture は `added_hunk` で作る。
  Rationale: `hunk(start, vec![diff_line(LineKind::Added, ...)])` はテストの主張ではなく、ほぼ全ての navigation fixture に付随する構築ノイズだった。`added_hunk(start, &[...])` に寄せることで、どの hunk にどの visible text があるかだけを読める。
  Date/Author: 2026-04-23 21:51:16Z / Codex

- Decision: Added 行だけの単一 hunk `App` fixture は `added_hunk_app` で作る。
  Rationale: `single_hunk_app` は Context / Deleted を混ぜる低レベル fixture として残し、Added-only の頻出ケースはより狭い helper に寄せる。これでテスト本文の fixture は、変更の種類ではなく表示文字列と hunk 開始行だけを示せる。
  Date/Author: 2026-04-23 21:55:07Z / Codex

- Decision: raw `Hunk` が必要な Added-only fixture も `added_hunk` で作り、`single_hunk_app` は mixed-line fixture 専用に残す。
  Rationale: `hunk(..., vec![diff_line(LineKind::Added, ...)])` は意味のある low-level 表現ではなく、テスト本文を水増しする構築手順だった。`added_hunk` を使うと hunk 開始行と表示文字列だけが残り、Context / Deleted を含む例だけが `single_hunk_app` の raw line vector を使う。
  Date/Author: 2026-04-23 21:59:34Z / Codex

- Decision: 番号付き DiffLine fixture と同種 line vector fixture は `prefixed_diff_lines` / `diff_lines` で作る。
  Rationale: これらのテストは viewport や anchor の挙動を見ており、`(0..N).map(...)` や `vec![diff_line(Context, ...)]` の展開自体は主張ではない。test support に寄せると、fixture の意味だけを残して生成手順を消せる。
  Date/Author: 2026-04-23 22:04:11Z / Codex

- Decision: UI test の rendered buffer 探索は、text-run 単位の helper で扱う。
  Rationale: `buffer_row_text(...).find(...)` のループは assertion の主張ではなく、描画済み terminal buffer から座標を拾う儀式だった。`first_text_run` / `row_containing` に寄せると、個々のテストは色や gutter の期待だけを読める。
  Date/Author: 2026-04-23 22:07:29Z / Codex

- Decision: tempdir-backed scar fixture は、単一行 hunk 用 helper を標準にする。
  Rationale: scar/editor/undo tests の主張は実ファイルへの挿入・undo・line mapping であり、`vec![diff_line(LineKind::Added, ...)]` の組み立てではない。added/context の単一行 helper に寄せると、実 FS setup の意味だけが残る。
  Date/Author: 2026-04-23 22:10:48Z / Codex

- Decision: RowKind から実際の `DiffLine.kind` を引く test helper は、kind 指定の row lookup として共有する。
  Rationale: scar deleted-line tests は、row の種類だけではなく `FileDiff` 側の line kind を見て対象行を決める必要がある。この lookup を closure の直書きにすると、テスト本文より探索手順の方が大きくなるため helper 化する。
  Date/Author: 2026-04-23 22:13:40Z / Codex

- Decision: 単一 hunk の `FileDiff` fixture は `file_with_hunk` 系 helper に寄せる。
  Rationale: `make_file(name, vec![hunk(...)], secs)` は、テスト本文にとって hunk vector の包装手順でしかない。helper 名で added/context/single hunk の意図を表すと、viewport や search rehydrate の主張に読み筋が戻る。
  Date/Author: 2026-04-23 22:16:57Z / Codex

- Decision: 多 hunk performance は「表示結果の互換性を保った cache」として扱い、per-frame の全 layout 走査をなくす。
  Rationale: `current_hunk_range` は `build_layout` 時に hunk span を確定でき、`VisualIndex` は layout と body width が変わらない限り同じ値である。render hot path で再計算し続ける理由がないため、`ScrollLayout` の hunk cache と `App` の width-keyed visual index cache に寄せる。
  Date/Author: 2026-04-23 22:25:36Z / Codex

- Decision: navigation は sorted index の binary search に寄せ、nowrap の visual height 判定では `VisualIndex` を作らない。
  Rationale: `hunk_starts` と `change_runs` は `build_layout` が昇順で作る index なので、次/前の候補探索は `partition_point` で十分である。nowrap では 1 logical row = 1 visual row のため、long-run 判定に prefix-sum index を作る必要がない。
  Date/Author: 2026-04-23 22:30:54Z / Codex

- Decision: search matches は sorted invariant を使って row-local range として読む。
  Rationale: `find_matches` は layout row order で `MatchLocation` を push するため、同じ `row` の match は contiguous である。row render ごとに全 match を filter すると、match 数が多い検索で viewport 描画コストが膨らむ。
  Date/Author: 2026-04-23 22:34:18Z / Codex

## Outcomes & Retrospective

Stream mode の差分構築を `src/stream.rs` へ、footer 描画を `src/ui/footer.rs` へ、help/picker overlay 描画を `src/ui/overlays.rs` へ切り出した。`src/app.rs` は 10820 行から 10690 行へ、`src/ui.rs` は 4989 行から 4195 行へ減った。v0.5 行番号まわりの既存未コミット差分は巻き戻さず、責務分割だけを重ねた。

2nd pass では、単なる file split ではなく重複削除に寄せた。Claude Code / Qwen Code / Codex の settings hook installer を `install_settings_hook_agent` に統合し、`src/ui.rs` の numbered renderer wrapper 4 本を `add_line_number_gutters` に置換し、`FileViewVisualIndex` を削除して `VisualIndex::build_lines` に統合した。差分は `src/app.rs`、`src/init.rs`、`src/ui.rs` の 3 ファイルで 256 insertions / 508 deletions、純減 252 行。完了時点の行数は `src/app.rs` 10650、`src/ui.rs` 4017、`src/init.rs` 2130。

自律削減 pass では、search/scar 入力編集、normal/file-view の共通キー、picker/search navigation、Cline hook 判定、app/ui test fixture をまとめた。`src/test_support.rs` は test-only の共有 fixture として追加した。2nd pass 完了時点からさらに `src/app.rs` は 10650 行から 10441 行へ、`src/ui.rs` は 4017 行から 3830 行へ、`src/init.rs` は 2130 行から 2116 行へ減った。`src/test_support.rs` 153 行と `src/main.rs` の test-only module 宣言 2 行を差し引いても、この pass だけで約 255 行の純減になった。

検証は `just ci` が成功した。Rust unit tests は 467 件成功、release build 成功、e2e は 35 件成功 / 0 件失敗。残る課題は、まだ `src/app.rs` が 1 万行を超えていること、`src/ui.rs` も diff 行描画と file view 描画を抱えていることである。次の削減候補は file view state の module 化、diff/file view renderer の境界整理、`init.rs` の Cursor / Cline install/teardown の schema 別 adapter 化である。

post-commit 削減 pass では、`src/app.rs` と `src/ui.rs` の test-only boilerplate だけを削った。`src/app.rs` は 10441 行から 10386 行へ、`src/ui.rs` は 3830 行から 3730 行へ減った。差分は 2 ファイルで 153 insertions / 308 deletions、純減 155 行。検証は `just ci` が成功し、Rust unit tests は 467 件成功、release build 成功、e2e は 35 件成功 / 0 件失敗だった。

共通 key dispatch / fixture 削減 pass では、normal/file-view の共通 action key を `handle_common_action_key` に寄せ、単一 added 行の fixture を `single_added_app` として共有化した。`src/app.rs` は 10386 行から 10281 行へ、`src/ui.rs` は 3730 行から 3661 行へ、`src/test_support.rs` は 153 行から 161 行になった。差分は 3 ファイルで 92 insertions / 258 deletions、純減 166 行。検証は `just ci` が成功し、Rust unit tests は 467 件成功、release build 成功、e2e は 35 件成功 / 0 件失敗だった。

thin wrapper cleanup pass では、picker/search navigation の direction wrapper と UI test module の local wrapper を削除した。`src/app.rs` は 10281 行から 10259 行へ、`src/ui.rs` は 3661 行から 3653 行へ減った。差分は 2 ファイルで 14 insertions / 44 deletions、純減 30 行。検証は `just ci` が成功し、Rust unit tests は 467 件成功、release build 成功、e2e は 35 件成功 / 0 件失敗だった。

one-hunk fixture 削減 pass では、app/ui tests の単一 hunk / 単一 added 行 fixture を `single_hunk_file` と `single_added_file` に集約した。`src/app.rs` は 10259 行から 10111 行へ、`src/ui.rs` は 3653 行から 3621 行へ、`src/test_support.rs` は 161 行から 165 行になった。差分は 3 ファイルで 65 insertions / 241 deletions、純減 176 行。検証は `just ci` が成功し、Rust unit tests は 467 件成功、release build 成功、e2e は 35 件成功 / 0 件失敗だった。

app-under-10k fixture 削減 pass では、単一ファイル app fixture と hunk header row assertion を共有 helper に寄せた。`src/app.rs` は 10111 行から 9945 行へ、`src/ui.rs` は 3621 行から 3614 行へ、`src/test_support.rs` は 165 行から 186 行になった。差分は 3 ファイルで 198 insertions / 350 deletions、純減 152 行。検証は `just ci` が成功し、Rust unit tests は 467 件成功、release build 成功、e2e は 35 件成功 / 0 件失敗だった。

legacy fixture cleanup pass では、削除ファイル fixture と file/diff row lookup helper を追加し、テスト本文に残った raw `FileDiff` 構築と `RowKind` closure を削った。`src/app.rs` は 9945 行から 9929 行へ、`src/test_support.rs` は 186 行から 192 行になった。差分は 2 ファイルで 34 insertions / 44 deletions、純減 10 行。検証は `just ci` が成功し、Rust unit tests は 467 件成功、release build 成功、e2e は 35 件成功 / 0 件失敗だった。

single-hunk app fixture pass では、app/ui tests の単一 hunk app 構築を `single_hunk_app` に集約した。`src/app.rs` は 9929 行から 9883 行へ、`src/ui.rs` は 3614 行から 3574 行へ、`src/test_support.rs` は 192 行から 201 行になった。差分は 3 ファイルで 256 insertions / 333 deletions、純減 77 行。検証は `just ci` が成功し、Rust unit tests は 467 件成功、release build 成功、e2e は 35 件成功 / 0 件失敗だった。

numbered added lines fixture pass では、長い added hunk fixture を `numbered_added_lines` に集約した。`src/app.rs` は 9883 行から 9866 行へ、`src/ui.rs` は 3574 行から 3566 行へ、`src/test_support.rs` は 201 行から 207 行になった。差分は 3 ファイルで 21 insertions / 40 deletions、純減 19 行。検証は `just ci` が成功し、Rust unit tests は 467 件成功、release build 成功、e2e は 35 件成功 / 0 件失敗だった。

added hunk fixture pass では、Added 行だけの hunk fixture を `added_hunk` に集約した。`src/app.rs` は 9866 行から 9786 行へ、`src/test_support.rs` は 207 行から 217 行になった。差分は 2 ファイルで 31 insertions / 101 deletions、純減 70 行。検証は `just ci` が成功し、Rust unit tests は 467 件成功、release build 成功、e2e は 35 件成功 / 0 件失敗だった。

added hunk app fixture pass では、Added 行だけの single-hunk app fixture を `added_hunk_app` に集約した。`src/app.rs` は 9786 行から 9640 行へ、`src/ui.rs` は 3566 行から 3542 行へ、`src/test_support.rs` は 217 行から 221 行になった。差分は 3 ファイルで 35 insertions / 201 deletions、純減 166 行。検証は `just ci` が成功し、Rust unit tests は 467 件成功、release build 成功、e2e は 35 件成功 / 0 件失敗だった。

remaining Added-only hunk literal cleanup pass では、App fixture helper では置き換えられなかった raw `Hunk` 構築を `added_hunk` に寄せた。`src/app.rs` は 9640 行から 9619 行へ、`src/ui.rs` は 3542 行から 3535 行へ、`src/test_support.rs` は 221 行のままだった。差分は 2 ファイルで 15 insertions / 43 deletions、純減 28 行。検証は `just ci` が成功し、Rust unit tests は 467 件成功、release build 成功、e2e は 35 件成功 / 0 件失敗だった。

numbered diff line fixture pass では、prefix 付き番号行と同種 line vector fixture を `prefixed_diff_lines` / `diff_lines` に集約した。`src/app.rs` は 9619 行から 9556 行へ、`src/test_support.rs` は 221 行から 219 行へ減った。差分は 2 ファイルで 25 insertions / 90 deletions、純減 65 行。検証は `just ci` が成功し、Rust unit tests は 467 件成功、release build 成功、e2e は 35 件成功 / 0 件失敗だった。

UI buffer lookup pass では、UI tests の rendered buffer text-run 探索を `first_text_run` / `row_containing` に寄せ、sticky header fixture の番号付き context 行も `prefixed_diff_lines` に寄せた。`src/ui.rs` は 3535 行から 3516 行へ減った。差分は 1 ファイルで 20 insertions / 39 deletions、純減 19 行。検証は `just ci` が成功し、Rust unit tests は 467 件成功、release build 成功、e2e は 35 件成功 / 0 件失敗だった。

scar real-fs fixture pass では、tempdir-backed scar/editor/undo tests の単一 added/context 行 fixture を helper に寄せた。`src/app.rs` は 9556 行から 9506 行へ減った。差分は 1 ファイルで 76 insertions / 126 deletions、純減 50 行。検証は `just ci` が成功し、Rust unit tests は 467 件成功、release build 成功、e2e は 35 件成功 / 0 件失敗だった。

deleted-row lookup pass では、scar deleted-line tests の重複 RowKind scan を `first_diff_row_with_kind` に集約した。`src/app.rs` は 9506 行から 9480 行へ減った。差分は 1 ファイルで 26 insertions / 52 deletions、純減 26 行。検証は `just ci` が成功し、Rust unit tests は 467 件成功、release build 成功、e2e は 35 件成功 / 0 件失敗だった。

hunk file fixture pass では、single-hunk `FileDiff` wrapper を `file_with_hunk` / `added_hunk_file` / `context_hunk_file` に寄せた。`src/app.rs` は 9480 行から 9449 行へ、`src/test_support.rs` は 219 行から 240 行になった。差分は 2 ファイルで 43 insertions / 53 deletions、純減 10 行。検証は `just ci` が成功し、Rust unit tests は 467 件成功、release build 成功、e2e は 35 件成功 / 0 件失敗だった。

performance pass では、`current_hunk_range` の render-time scan を `ScrollLayout::hunk_ranges` lookup に置換し、seen hunk fingerprint を該当 mark がある hunk だけ計算するようにした。さらに wrap mode の `viewport_placement` が毎回 `VisualIndex::build` する経路を、body width keyed cache にした。`src/app.rs` は 9449 行から 9482 行へ、`src/ui.rs` は 3516 行から 3530 行へ、`src/test_support.rs` は 240 行から 241 行になった。差分は 3 ファイルで 109 insertions / 61 deletions、純増 48 行だが、4,000 hunk probe では wrap placement が約 85 倍、hunk range lookup が約 10,000 倍軽くなった。検証は `just ci` が成功し、Rust unit tests は 467 件成功、release build 成功、e2e は 35 件成功 / 0 件失敗だった。

navigation performance pass では、hunk/run の前後候補探索を `partition_point` helper に寄せ、follow target を `hunk_ranges` / `file_first_hunk` から直接読むようにした。nowrap の long-run 判定では visual height を row span から直接計算し、wrap 時だけ cached `VisualIndex` を使う。`src/app.rs` は 9482 行から 9480 行へ減り、差分は 1 ファイルで 65 insertions / 67 deletions、純減 2 行になった。検証は `just ci` が成功し、Rust unit tests は 467 件成功、release build 成功、e2e は 35 件成功 / 0 件失敗だった。

search highlight performance pass では、`row_search_matches` を全 match scan から `partition_point` による row-local range lookup に変えた。`src/ui.rs` は 3530 行から 3533 行になり、差分は 1 ファイルで 7 insertions / 4 deletions、純増 3 行。match 数が多い検索でも、可視行あたりの projection が O(total matches) から O(log total matches + matches on row) になる。検証は `just ci` が成功し、Rust unit tests は 467 件成功、release build 成功、e2e は 35 件成功 / 0 件失敗だった。

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

one-hunk fixture 削減 pass 後の追加証拠は以下である。

    src/app.rs          10111 lines
    src/ui.rs            3621 lines
    src/test_support.rs   165 lines

    git diff --stat -- src/app.rs src/ui.rs src/test_support.rs
    src/app.rs          | 242 ++++++++++------------------------------------------
    src/test_support.rs |  14 +--
    src/ui.rs           |  50 ++---------
    3 files changed, 65 insertions(+), 241 deletions(-)

    cargo test --all-targets --all-features ui::tests -- --nocapture
    63 passed

    cargo test --all-targets --all-features picker -- --nocapture
    6 passed

    cargo test --all-targets --all-features search -- --nocapture
    19 passed

    cargo test --all-targets --all-features apply_reset -- --nocapture
    6 passed

    cargo test --all-targets --all-features scroll_to -- --nocapture
    5 passed

    cargo test --all-targets --all-features apply_computed_files -- --nocapture
    3 passed

    cargo test --all-targets --all-features refresh_anchor -- --nocapture
    3 passed

    cargo test --all-targets --all-features wrap -- --nocapture
    31 passed

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

app-under-10k fixture 削減 pass 後の追加証拠は以下である。

    src/app.rs           9945 lines
    src/ui.rs            3614 lines
    src/test_support.rs   186 lines

    git diff --stat
    src/app.rs          | 416 ++++++++++++++++------------------------------------
    src/test_support.rs |  23 ++-
    src/ui.rs           | 109 +++++++-------
    3 files changed, 198 insertions(+), 350 deletions(-)

    cargo test --all-targets --all-features seen -- --nocapture
    12 passed

    cargo test --all-targets --all-features search -- --nocapture
    19 passed

    cargo test --all-targets --all-features line_number -- --nocapture
    27 passed

    cargo test --all-targets --all-features ui::tests -- --nocapture
    63 passed

    cargo clippy --all-targets --all-features -- -D warnings
    Finished `dev` profile

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

legacy fixture cleanup pass 後の追加証拠は以下である。

    src/app.rs           9929 lines
    src/ui.rs            3614 lines
    src/test_support.rs   192 lines

    git diff --stat
    src/app.rs          | 72 +++++++++++++++++++++--------------------------------
    src/test_support.rs |  6 +++++
    2 files changed, 34 insertions(+), 44 deletions(-)

    cargo test --all-targets --all-features scar_target -- --nocapture
    5 passed

    cargo test --all-targets --all-features revert_confirm -- --nocapture
    6 passed

    cargo test --all-targets --all-features populate_mtimes -- --nocapture
    1 passed

    cargo fmt --all -- --check
    cargo clippy --all-targets --all-features -- -D warnings
    Finished `dev` profile

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

single-hunk app fixture pass 後の追加証拠は以下である。

    src/app.rs           9883 lines
    src/ui.rs            3574 lines
    src/test_support.rs   201 lines

    git diff --stat
    src/app.rs          | 360 +++++++++++++++++++++++-----------------------------
    src/test_support.rs |   9 ++
    src/ui.rs           | 220 +++++++++++++-------------------
    3 files changed, 256 insertions(+), 333 deletions(-)

    cargo test --all-targets --all-features ui::tests -- --nocapture
    63 passed

    cargo test --all-targets --all-features search -- --nocapture
    19 passed

    cargo test --all-targets --all-features scar_target -- --nocapture
    5 passed

    cargo test --all-targets --all-features line_number -- --nocapture
    27 passed

    cargo clippy --all-targets --all-features -- -D warnings
    Finished `dev` profile

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

numbered added lines fixture pass 後の追加証拠は以下である。

    src/app.rs           9866 lines
    src/ui.rs            3566 lines
    src/test_support.rs   207 lines

    git diff --stat
    src/app.rs          | 35 +++++++++--------------------------
    src/test_support.rs |  6 ++++++
    src/ui.rs           | 20 ++++++--------------
    3 files changed, 21 insertions(+), 40 deletions(-)

    cargo test --all-targets --all-features viewport_top -- --nocapture
    6 passed

    cargo test --all-targets --all-features ui::tests -- --nocapture
    63 passed

    cargo clippy --all-targets --all-features -- -D warnings
    Finished `dev` profile

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

added hunk fixture pass 後の追加証拠は以下である。

    src/app.rs           9786 lines
    src/ui.rs            3566 lines
    src/test_support.rs   217 lines

    git diff --stat
    src/app.rs          | 122 +++++++++-------------------------------------------
    src/test_support.rs |  10 +++++
    2 files changed, 31 insertions(+), 101 deletions(-)

    cargo test --all-targets --all-features handle_key_j -- --nocapture
    1 passed

    cargo test --all-targets --all-features seen -- --nocapture
    12 passed

    cargo test --all-targets --all-features scroll_to -- --nocapture
    5 passed

    cargo clippy --all-targets --all-features -- -D warnings
    Finished `dev` profile

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

added hunk app fixture pass 後の追加証拠は以下である。

    src/app.rs           9640 lines
    src/ui.rs            3542 lines
    src/test_support.rs   221 lines

    git diff --stat
    src/app.rs          | 194 +++++++---------------------------------------------
    src/test_support.rs |   4 ++
    src/ui.rs           |  38 ++--------
    3 files changed, 35 insertions(+), 201 deletions(-)

    cargo test --all-targets --all-features search -- --nocapture
    19 passed

    cargo test --all-targets --all-features line_number -- --nocapture
    27 passed

    cargo test --all-targets --all-features scar_target -- --nocapture
    5 passed

    cargo test --all-targets --all-features ui::tests -- --nocapture
    63 passed

    cargo clippy --all-targets --all-features -- -D warnings
    Finished `dev` profile

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

remaining Added-only hunk literal cleanup pass 後の追加証拠は以下である。

    src/app.rs           9619 lines
    src/ui.rs            3535 lines
    src/test_support.rs   221 lines

    git diff --stat
    src/app.rs | 41 ++++++++++-------------------------------
    src/ui.rs  | 17 +++++------------
    2 files changed, 15 insertions(+), 43 deletions(-)

    cargo fmt --all -- --check

    cargo test --all-targets --all-features refresh_anchor -- --nocapture
    3 passed

    cargo test --all-targets --all-features next_hunk -- --nocapture
    4 passed

    cargo test --all-targets --all-features ui::tests -- --nocapture
    63 passed

    cargo clippy --all-targets --all-features -- -D warnings
    Finished `dev` profile

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

numbered diff line fixture pass 後の追加証拠は以下である。

    src/app.rs           9556 lines
    src/ui.rs            3535 lines
    src/test_support.rs   219 lines

    git diff --stat
    src/app.rs          | 91 +++++++++--------------------------------------------
    src/test_support.rs | 24 +++++++-------
    2 files changed, 25 insertions(+), 90 deletions(-)

    cargo test --all-targets --all-features viewport_top -- --nocapture
    6 passed

    cargo test --all-targets --all-features apply_computed_files -- --nocapture
    3 passed

    cargo fmt --all -- --check

    cargo clippy --all-targets --all-features -- -D warnings
    Finished `dev` profile

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

UI buffer lookup pass 後の追加証拠は以下である。

    src/app.rs           9556 lines
    src/ui.rs            3516 lines
    src/test_support.rs   219 lines

    git diff --stat
    src/ui.rs | 59 ++++++++++++++++++++---------------------------------------
    1 file changed, 20 insertions(+), 39 deletions(-)

    cargo test --all-targets --all-features ui::tests -- --nocapture
    63 passed

    cargo fmt --all -- --check

    cargo clippy --all-targets --all-features -- -D warnings
    Finished `dev` profile

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

scar real-fs fixture pass 後の追加証拠は以下である。

    src/app.rs           9506 lines
    src/ui.rs            3516 lines
    src/test_support.rs   219 lines

    git diff --stat
    src/app.rs | 202 +++++++++++++++++++++++--------------------------------------
    1 file changed, 76 insertions(+), 126 deletions(-)

    cargo test --all-targets --all-features scar -- --nocapture
    70 passed

    cargo test --all-targets --all-features scar_comment -- --nocapture
    7 passed

    cargo test --all-targets --all-features scar_target -- --nocapture
    5 passed

    cargo test --all-targets --all-features undo_scar -- --nocapture
    3 passed

    cargo test --all-targets --all-features open_in_editor -- --nocapture
    3 passed

    cargo fmt --all -- --check

    cargo clippy --all-targets --all-features -- -D warnings
    Finished `dev` profile

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

deleted-row lookup pass 後の追加証拠は以下である。

    src/app.rs           9480 lines
    src/ui.rs            3516 lines
    src/test_support.rs   219 lines

    git diff --stat
    src/app.rs | 78 +++++++++++++++++++++-----------------------------------------
    1 file changed, 26 insertions(+), 52 deletions(-)

    cargo test --all-targets --all-features scar_on_ -- --nocapture
    3 passed

    cargo fmt --all -- --check

    cargo clippy --all-targets --all-features -- -D warnings
    Finished `dev` profile

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

hunk file fixture pass 後の追加証拠は以下である。

    src/app.rs           9449 lines
    src/ui.rs            3516 lines
    src/test_support.rs   240 lines

    git diff --stat
    src/app.rs          | 71 +++++++++++++++--------------------------------------
    src/test_support.rs | 25 +++++++++++++++++--
    2 files changed, 43 insertions(+), 53 deletions(-)

    cargo test --all-targets --all-features viewport_top -- --nocapture
    6 passed

    cargo test --all-targets --all-features search_matches_rehydrate -- --nocapture
    2 passed

    cargo fmt --all -- --check

    cargo clippy --all-targets --all-features -- -D warnings
    Finished `dev` profile

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

performance pass 後の追加証拠は以下である。

    src/app.rs           9482 lines
    src/ui.rs            3530 lines
    src/test_support.rs   241 lines

    git diff --stat
    src/app.rs          | 141 ++++++++++++++++++++++++++++++++--------------------
    src/test_support.rs |   3 +-
    src/ui.rs           |  26 +++++++---
    3 files changed, 109 insertions(+), 61 deletions(-)

    temporary ignored probe output (removed after measurement so normal test count stays unchanged)
    before:
    current_hunk_range x1000: 23.746458ms
    viewport_top nowrap x1000: 15.468542ms
    viewport_placement wrap x200: 251.945917ms
    build_layout no seen x100: 42.202333ms

    after:
    current_hunk_range x1000: 2.292µs
    viewport_top nowrap x1000: 2.958µs
    viewport_placement wrap x200: 2.954625ms
    build_layout no seen x100: 20.6235ms

    cargo test --all-targets --all-features seen -- --nocapture
    12 passed

    cargo test --all-targets --all-features viewport_top -- --nocapture
    6 passed

    cargo test --all-targets --all-features visual_viewport_top -- --nocapture
    2 passed

    cargo clippy --all-targets --all-features -- -D warnings
    Finished `dev` profile

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

navigation performance pass 後の追加証拠は以下である。

    src/app.rs           9480 lines
    src/ui.rs            3530 lines
    src/test_support.rs   241 lines

    git diff --stat
    src/app.rs | 132 ++++++++++++++++++++++++++++++-------------------------------
    1 file changed, 65 insertions(+), 67 deletions(-)

    cargo test --all-targets --all-features handle_key_j -- --nocapture
    1 passed

    cargo test --all-targets --all-features lowercase_j -- --nocapture
    5 passed

    cargo test --all-targets --all-features lowercase_k -- --nocapture
    3 passed

    cargo test --all-targets --all-features follow_target -- --nocapture
    2 passed

    cargo clippy --all-targets --all-features -- -D warnings
    Finished `dev` profile

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

search highlight performance pass 後の追加証拠は以下である。

    src/app.rs           9480 lines
    src/ui.rs            3533 lines
    src/test_support.rs   241 lines

    git diff --stat
    src/ui.rs | 11 +++++++----
    1 file changed, 7 insertions(+), 4 deletions(-)

    cargo test --all-targets --all-features search -- --nocapture
    19 passed

    cargo test --all-targets --all-features ui::tests::search -- --nocapture
    4 passed

    cargo clippy --all-targets --all-features -- -D warnings
    Finished `dev` profile

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

`/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/test_support.rs` は `#[cfg(test)]` の test-only module である。`src/app.rs` と `src/ui.rs` の test module からだけ使い、production code からは参照しない。主な helper は `diff_line`、`diff_lines`、`numbered_added_lines`、`prefixed_diff_lines`、`hunk`、`added_hunk`、`make_file`、`file_with_hunk`、`added_hunk_file`、`context_hunk_file`、`single_hunk_file`、`single_added_file`、`single_added_hunk_file`、`single_deleted_file`、`binary_file`、`app_with_file`、`app_with_hunks`、`single_hunk_app`、`added_hunk_app`、`app_with_files`、`file_view_state`、`install_search` で、同じ fixture 生成を複数 test module に置かないためのもの。

`/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/app.rs` の `ScrollLayout` は render hot path 用に `hunk_ranges` と `hunk_fingerprints` を持つ。`hunk_ranges[file_idx][hunk_idx]` は `(start, end_exclusive)` の row span、`hunk_fingerprints[file_idx][hunk_idx]` は seen mark のある hunk だけ `Some(current_fp)` になる。`App` は wrap mode の `VisualIndex` を `visual_index_cache` に body width keyed で保持し、`build_layout` で無効化する。

`next_sorted_after`、`prev_sorted_before`、`change_run_at`、`next_change_run_start_after`、`prev_change_run_start_before` は、`build_layout` が昇順に作る `hunk_starts` / `change_runs` を `partition_point` で読む navigation helper である。これらは `src/app.rs` 内部専用で、外部 interface は増やさない。

`row_search_matches` は `SearchState.matches` が row order で並ぶ invariant に依存し、`partition_point` で該当 row の match slice だけを返す。`SearchState` の public shape は変えない。
