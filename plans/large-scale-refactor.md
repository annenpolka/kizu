# kizu 大規模責務分割リファクタ

この ExecPlan はリビングドキュメントです。`Progress`、`Surprises & Discoveries`、`Decision Log`、`Outcomes & Retrospective` は作業の進行に合わせて更新します。

## Purpose / Big Picture

kizu は v0.5.1 時点で diff 監視、scar、Stream mode、file view、検索、line-number gutter、設定、複数エージェント hook install を備えている。動作は安定しているが、主要な責務が少数の巨大ファイルに寄り、次の機能追加やバグ修正で変更範囲を見誤りやすい。

このリファクタではユーザーから見える挙動を変えずに、`App` の状態機械、layout/navigation、modal 入力、scar/search/file-view、runtime loop、UI renderer、git diff backend、init installer を段階的に分割する。完了後も `kizu` は同じキー操作と表示を保ち、開発者は変更したい機能領域の小さな module だけを読んで修正できる。

## Progress

- [x] (2026-04-24 23:28:34Z) 現状調査を実施し、作業ツリーが `## main...origin/main` でクリーンなことを確認した。
- [x] (2026-04-24 23:28:34Z) `cargo test --all-targets --all-features` を実行し、467 件成功を確認した。
- [x] (2026-04-24 23:28:34Z) `cargo clippy --all-targets --all-features -- -D warnings` を実行し、warning なしを確認した。
- [x] (2026-04-24 23:28:34Z) 主要ファイルの行数と責務境界を調査し、この計画に反映した。
- [x] (2026-04-24 23:46:48Z) 作業ブランチ `refactor-large-scale-responsibility-split` を作成した。
- [x] (2026-04-24 23:46:48Z) Milestone 1: `src/app/layout.rs` を追加し、`ScrollLayout` / `RowKind` / `HunkAnchor` / `VisualIndex` / `CursorPlacement` / fingerprint helper / `build_scroll_layout` を分離した。
- [x] (2026-04-24 23:46:48Z) Milestone 1 検証: `build_layout` 8 件、`visual_index` 2 件、`line_number` 27 件、clippy が成功した。
- [x] (2026-04-25 00:23:49Z) Milestone 2: `src/app/navigation.rs` を追加し、scroll/viewport/navigation/anchor refresh を `src/app.rs` から分離した。
- [x] (2026-04-25 00:23:49Z) Milestone 3: `src/app/input.rs`、`src/app/picker.rs`、`src/app/search.rs`、`src/app/text_input.rs`、`src/app/file_view.rs`、`src/app/review.rs`、`src/app/stream_events.rs` を追加し、modal input、picker、search、text editing、file view、scar/revert、Stream event state を分離した。
- [x] (2026-04-25 00:23:49Z) Milestone 4: runtime loop を `src/app/runtime.rs` へ分離し、`crate::app::run` の外部 API を `pub use runtime::run` で維持した。
- [x] (2026-04-25 00:23:49Z) full gate `just ci` を実行し、fmt-check、clippy、467 unit tests、release build、e2e 35 件成功を確認した。
- [x] (2026-04-25 00:32:02Z) Milestone 5a: `src/ui/geometry.rs` を追加し、diff view と file view の line-number gutter / body width 算出を `RenderGeometry` に集約した。
- [x] (2026-04-25 00:32:02Z) Milestone 5b: `src/ui/line_numbers.rs` を追加し、line-number gutter span / insertion helper を `src/ui.rs` から分離した。
- [x] (2026-04-25 00:33:10Z) UI 第二波の full gate `just ci` を実行し、fmt-check、clippy、467 unit tests、release build、e2e 35 件成功を確認した。
- [ ] `src/ui/` 配下へ diff renderer、file-view renderer、wrap/search presentation を分割する。
- [ ] `src/git/` 配下へ types、repo command、parser、untracked synth、revert を分割する。
- [ ] `src/init/` 配下へ agent schema adapter と teardown/reporting を分割する。
- [ ] 必要に応じて UI/git/init の次波で e2e / perf 再計測を追加し、結果をこの計画に追記する。

## Surprises & Discoveries

- Observation: `src/app.rs` は 9480 行で、production code が 4432 行、test module が 5047 行を占めている。
  Evidence: `wc -l src/app.rs` は 9480 行、`#[cfg(test)] mod tests` は 4433 行目から始まる。

- Observation: `App` 構造体が repository identity、diff data、layout、modal state、file view、search、seen marks、health、config、Stream state、scar undo、viewport pin を一つに抱えている。
  Evidence: `/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/app.rs` の `pub struct App` は 172 行目から 371 行目まで続く。

- Observation: key dispatch は modal chain と normal/file-view/picker/search/scar/revert の入力処理が同じ `impl App` に密集している。
  Evidence: `handle_key` は 2063 行目から 2083 行目、normal key dispatch は 2088 行目から 2209 行目、picker helper は 2249 行目から 2361 行目にある。

- Observation: viewport/layout/navigation は既に performance cache を持つため、分割時に body width と `VisualIndex` cache の単一性を崩すと regress しやすい。
  Evidence: `with_visual_index` は 2521 行目から 2537 行目、`viewport_placement` は 2570 行目から 2590 行目、`build_layout` は 3822 行目から 3987 行目にある。

- Observation: `render_scroll` は描画だけでなく、body width 算出、line-number fallback、sticky header 判断、`App` の `Cell` 更新まで行っている。
  Evidence: `/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/ui.rs` の `render_scroll` は 183 行目から始まり、`app.last_body_height` と `app.last_body_width` を 320 行目から 321 行目で更新する。

- Observation: `render_row` は row rendering だけでなく seen hunk fingerprint 判定、search match projection、line-number gutter injection をまとめて行っている。
  Evidence: `render_row` は 589 行目から 715 行目にあり、`seen_hunk_fingerprint` 呼び出し、`row_search_matches`、`add_line_number_gutters` が同じ match arm に入っている。

- Observation: file view は state construction と navigation が `app.rs`、rendering が `ui.rs` に分かれているが、どちらも `VisualIndex::build_lines` と body width を直接扱っている。
  Evidence: `open_file_view` は `src/app.rs` 3172 行目から 3263 行目、`render_file_view` は `src/ui.rs` 1464 行目から 1575 行目にある。

- Observation: `src/git.rs` は diff model 型、git subprocess、untracked file synth、parser、repo metadata、hunk revert を単一ファイルに持つ。
  Evidence: model 型は 18 行目から 79 行目、`diff_single_file` と untracked synth は 124 行目から 257 行目、`compute_diff_with_snapshots` は 269 行目から 349 行目、`parse_unified_diff` は 913 行目から 1075 行目にある。

- Observation: `src/init.rs` は agent detection、interactive prompt、JSON hook merge、agent-specific installer、teardown/reporting を抱え、hook schema adapter の自然な分割境界がある。
  Evidence: `install_agent` は 665 行目から 697 行目、JSON hook merge は 827 行目から 987 行目、per-agent installers は 991 行目から 1191 行目にある。

- Observation: `refactor/large-scale-responsibility-split` という slash 付き branch 名は作れなかった。
  Evidence: `git switch -c refactor/large-scale-responsibility-split` は `cannot lock ref 'refs/heads/refactor/large-scale-responsibility-split': unable to create directory` で失敗したため、`refactor-large-scale-responsibility-split` を使った。

- Observation: layout 抽出は behavior change なしで成立し、`src/app.rs` を 505 行削れた。
  Evidence: `wc -l src/app.rs src/app/layout.rs` は `src/app.rs 8975`、`src/app/layout.rs 524` を表示した。`cargo test ... build_layout` は 8 件、`visual_index` は 2 件、`line_number` は 27 件成功し、clippy も成功した。

- Observation: app child module への第一波分割後、`src/app.rs` は 9480 行から 6128 行へ縮小し、移動先の `src/app/*.rs` は合計 3300 行になった。
  Evidence: `wc -l src/app.rs src/app/*.rs` は `6128 src/app.rs`、`3300 src/app/*.rs`、合計 `9428 total` を表示した。

- Observation: 第一波の分割は behavior change なしで full gate を通過した。
  Evidence: `just ci` は `cargo fmt --all -- --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`cargo test --all-targets --all-features`、`cargo build --release --locked`、`bun test` を順に実行し、unit tests は `467 passed`、e2e は `35 pass / 0 fail` だった。

- Observation: UI 第二波の最初は geometry と line-number gutter helpers の抽出だけで成立した。
  Evidence: `cargo test --all-targets --all-features line_number -- --nocapture` は 27 件成功し、`cargo test --all-targets --all-features ui::tests -- --nocapture` は 63 件成功した。`cargo clippy --all-targets --all-features -- -D warnings` も成功した。

- Observation: UI 第二波も full gate を通過した。
  Evidence: `just ci` は `cargo fmt --all -- --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`cargo test --all-targets --all-features`、`cargo build --release --locked`、`bun test` を順に実行し、unit tests は `467 passed`、e2e は `35 pass / 0 fail` だった。

## Decision Log

- Decision: 最初の実装単位は behavior change ではなく module extraction とする。
  Rationale: 現状の Rust unit tests と e2e が広く振る舞いを pin している。大規模リファクタの最初に新機能を混ぜると、回帰原因を切り分けられない。
  Date/Author: 2026-04-24 23:28:34Z / Codex

- Decision: `src/app.rs` をいきなり `src/app/mod.rs` に置き換えず、当面は `src/app.rs` を facade として残し、`src/app/*.rs` の子 module を追加する。
  Rationale: `src/main.rs` の `mod app;` を維持しながら、Rust の子 module と extension `impl App` で段階的に移動できる。大きなファイル rename を避けると diff と merge conflict が小さい。
  Date/Author: 2026-04-24 23:28:34Z / Codex

- Decision: UI split では body width / line-number gutter / wrap geometry の算出を一つの geometry helper に寄せてから renderer を分ける。
  Rationale: line-number 実装時の最大リスクは render と placement の width drift だった。renderer だけを先に分けると同じ罠を再導入しやすい。
  Date/Author: 2026-04-24 23:28:34Z / Codex

- Decision: git/init の分割は app/ui の後に行う。
  Rationale: app/ui は現在の変更頻度と blast radius が最も大きい。git/init は比較的独立しているため、app/ui の compile surface が安定してから実施する方が安全である。
  Date/Author: 2026-04-24 23:28:34Z / Codex

- Decision: runtime loop は最初の分割では top-level `src/runtime.rs` ではなく `src/app/runtime.rs` に置く。
  Rationale: `src/main.rs` の呼び出し面と `crate::app::run` の import を変えずに、terminal/tokio/crossterm side-effect を `App` state machine から切り離せる。top-level への昇格は app facade の縮小がさらに進んでからの方が diff が小さい。
  Date/Author: 2026-04-25 00:23:49Z / Codex

- Decision: scar insertion、free comment composer、undo、hunk revert confirmation は `src/app/review.rs` にまとめる。
  Rationale: これらはすべて「現在カーソルが指す変更に対してレビュー操作を発行する」責務であり、同じ target-line 計算、undo stack、last_error handling を共有する。`scar_actions.rs` より `review.rs` の方が revert も含む実際の境界を表す。
  Date/Author: 2026-04-25 00:23:49Z / Codex

## Outcomes & Retrospective

第一波として `src/app.rs` の責務を child module 群へ分割した。`src/app.rs` は facade と core state を保持し、layout、navigation、input dispatch、picker、search、text input editing、file view、review actions、Stream event ingestion、runtime loop は `src/app/` 配下の小さな module に移った。

ユーザーから見える挙動は変えていない。`just ci` が完走し、fmt-check、clippy、467 unit tests、release build、35 e2e tests がすべて成功した。次の波は UI renderer、git backend、init installer の分割であり、app/ui の width accounting と line-number gutter の単一性を崩さないことが引き続き最重要リスクである。

## Context and Orientation

作業対象は `/Users/annenpolka/ghq/github.com/annenpolka/kizu` の Rust TUI アプリケーションである。TUI は terminal user interface の略で、ratatui/crossterm を使って端末内に描画する UI を指す。

現在の主なファイル構成は次の通りである。`/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/app.rs` は facade、`App` core state、diff recompute、watch/reset、editor invocation、既存 unit tests を持つ。`src/app/layout.rs` は scroll layout と visual index、`src/app/navigation.rs` は viewport/navigation、`src/app/input.rs` は key dispatch、`src/app/picker.rs` は file picker、`src/app/search.rs` は search state、`src/app/text_input.rs` は char-safe text editing、`src/app/file_view.rs` は file view state/navigation、`src/app/review.rs` は scar/comment/undo/revert、`src/app/stream_events.rs` は Stream event ingestion、`src/app/runtime.rs` は terminal/tokio/crossterm runtime loop を持つ。`/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/ui.rs` は画面全体、diff rows、line-number gutter、wrap rendering、search highlight、file-view rendering を持ち、`src/ui/footer.rs` と `src/ui/overlays.rs` だけが既に分離されている。

`/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/git.rs` は git CLI を shell out して diff を取り、untracked file を合成し、unified diff を `FileDiff` / `Hunk` / `DiffLine` に parse する backend である。`/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/init.rs` は Claude Code、Cursor、Codex、Qwen Code、Cline、Gemini などの hook install と teardown を扱う。

`ScrollLayout` は diff を flat row list に変換した表示用 index である。`VisualIndex` は wrap mode で logical row と visual row を変換する prefix-sum index である。`scar` は `@kizu[...]` inline comment を実ファイルに挿入するレビュー指示である。Stream mode は hook event log から操作履歴を表示する view であり、git の現在状態を見る通常 diff view とは異なる。

## Plan of Work

最初に baseline を固定する。作業ブランチを作成し、`cargo test --all-targets --all-features`、`cargo clippy --all-targets --all-features -- -D warnings`、`just ci` を実行する。必要なら既存の一時 probe と同じ形で 4,000 hunk synthetic app の hot path を測る。ここでは通常 tree に benchmark code を残さない。

次に `src/app/` 配下の子 module を作り、`src/app.rs` は facade として残す。第一波では production behavior を変えず、`ScrollLayout`、`RowKind`、`HunkAnchor`、`VisualIndex`、layout construction、hunk fingerprint helper を `src/app/layout.rs` へ移す。`App::build_layout` は最終的に pure function `build_scroll_layout` を呼ぶだけに近づける。既存 tests は app module から見える public / pub(crate) surface を保って通す。

第二波では navigation と viewport を `src/app/navigation.rs` へ移す。対象は `scroll_by`、`scroll_to_visual`、`scroll_to`、`viewport_placement`、`next_hunk`、`prev_hunk`、`next_change`、`prev_change`、anchor refresh 系である。`VisualIndex` cache の invalidation は `build_layout` 後だけに維持し、nowrap の fast path と wrap の visual-row path を分けたままにする。

第三波では modal input と feature state を分ける。`src/app/input.rs` に `handle_key`、normal key dispatch、common action dispatch を移し、`src/app/picker.rs`、`src/app/search.rs`、`src/app/text_input.rs`、`src/app/review.rs`、`src/app/file_view.rs`、`src/app/stream_events.rs` に各 overlay / feature の state と `impl App` を移す。副作用境界は変えない。scar は引き続き `crate::scar::insert_scar` / `remove_scar` を呼ぶ。file view は `FileViewState` と visual-row navigation を一緒に置き、renderer から必要な read-only state を渡す。

第四波では runtime loop を `src/app/runtime.rs` に移す。対象は `run`、`run_loop`、`apply_key_effect`、`run_external_editor` である。`App` は pure-ish state machine、runtime は crossterm/tokio/ratatui event loop と watcher side-effect の holder、UI は renderer という三層に分ける。将来 `src/app.rs` facade がさらに薄くなった時点で、必要なら top-level `src/runtime.rs` へ昇格する。

第五波では UI を分割する。まず `src/ui/geometry.rs` に `RenderGeometry` を作り、diff view と file view の body width、line-number gutter、wrap width を同じ関数で算出する。次に `src/ui/line_numbers.rs`、`src/ui/search_highlight.rs`、`src/ui/diff_view.rs`、`src/ui/file_view.rs` へ移す。`src/ui.rs` は `render` と high-level layout だけを残す。sticky header overlay は通常 row renderer と別 path なので、line-number gutter alignment tests を必ず維持する。

第六波では `src/git.rs` を facade にし、`src/git/types.rs`、`src/git/repo.rs`、`src/git/diff.rs`、`src/git/untracked.rs`、`src/git/parse.rs`、`src/git/revert.rs` に分ける。外部 API は `git::compute_diff`、`git::compute_diff_with_snapshots`、`git::diff_single_file`、`git::head_sha`、`git::git_dir`、`git::git_common_dir`、`git::current_branch_ref`、`git::find_root`、`git::build_hunk_patch`、`git::revert_hunk` を維持する。

第七波では `src/init.rs` を facade にし、`src/init/detect.rs`、`src/init/scope.rs`、`src/init/report.rs`、`src/init/settings_json.rs`、`src/init/cursor.rs`、`src/init/cline.rs`、`src/init/teardown.rs` へ分ける。Cursor の flat hooks schema、Claude/Qwen/Codex の matcher-group schema、Cline の script-file schema を adapter として分離し、teardown が user hooks を消さない invariant を tests で守る。

各波の最後に targeted tests、`cargo fmt --all -- --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`cargo test --all-targets --all-features` を実行する。app/ui の波では `just ci` まで通す。大きな移動をした直後は test module も同じ責務 module へ移すが、test-only helper は production surface に出さない。

## Concrete Steps

作業ディレクトリは `/Users/annenpolka/ghq/github.com/annenpolka/kizu`。

開始前に current state を記録する。

    git status --short --branch
    wc -l src/app.rs src/ui.rs src/ui/footer.rs src/ui/overlays.rs src/stream.rs src/git.rs src/watcher.rs src/hook.rs src/config.rs src/init.rs src/main.rs src/scar.rs
    cargo test --all-targets --all-features
    cargo clippy --all-targets --all-features -- -D warnings

期待する結果は、`git status` が `## main...origin/main` を表示し、Rust unit tests が `467 passed`、clippy が warning なしで終わることである。

実装ブランチを作る。

    git switch -c refactor-large-scale-responsibility-split

Milestone 1 では `apply_patch` で `src/app/layout.rs` を追加し、`src/app.rs` 先頭に `mod layout;` と必要な `pub(crate) use` を追加する。移動後に次を実行する。

    cargo test --all-targets --all-features visual_index -- --nocapture
    cargo test --all-targets --all-features build_layout -- --nocapture
    cargo test --all-targets --all-features line_number -- --nocapture
    cargo clippy --all-targets --all-features -- -D warnings

Milestone 2 では `src/app/navigation.rs` を追加し、navigation / viewport / anchor 関連の `impl App` を移す。移動後に次を実行する。

    cargo test --all-targets --all-features viewport -- --nocapture
    cargo test --all-targets --all-features scroll -- --nocapture
    cargo test --all-targets --all-features next_hunk -- --nocapture
    cargo test --all-targets --all-features lower -- --nocapture
    cargo test --all-targets --all-features

Milestone 3 では `src/app/input.rs`、`src/app/picker.rs`、`src/app/search.rs`、`src/app/text_input.rs`、`src/app/review.rs`、`src/app/file_view.rs`、`src/app/stream_events.rs` を追加する。移動後に次を実行する。

    cargo test --all-targets --all-features handle_key -- --nocapture
    cargo test --all-targets --all-features search -- --nocapture
    cargo test --all-targets --all-features scar -- --nocapture
    cargo test --all-targets --all-features file_view -- --nocapture
    cargo test --all-targets --all-features undo -- --nocapture
    cargo clippy --all-targets --all-features -- -D warnings
    just ci

Milestone 4 では `src/app/runtime.rs` を追加し、`src/main.rs` の呼び出し先を変えずに `crate::app::run` の実体を runtime module に寄せる。移動後に次を実行する。

    cargo test --all-targets --all-features
    cargo build --release --locked

Milestone 5 では UI module を分ける。最初に `src/ui/geometry.rs` を作り、body width と gutter width を一箇所から返す。次に renderer を移す。移動後に次を実行する。

    cargo test --all-targets --all-features ui::tests -- --nocapture
    cargo test --all-targets --all-features file_view -- --nocapture
    cargo test --all-targets --all-features wrap -- --nocapture
    cargo test --all-targets --all-features line_numbers -- --nocapture
    cargo test --all-targets --all-features search -- --nocapture
    just ci

Milestone 6 では git module を分ける。移動後に次を実行する。

    cargo test --all-targets --all-features git::tests -- --nocapture
    cargo test --all-targets --all-features stream -- --nocapture
    cargo clippy --all-targets --all-features -- -D warnings

Milestone 7 では init module を分ける。移動後に次を実行する。

    cargo test --all-targets --all-features init::tests -- --nocapture
    cargo test --all-targets --all-features
    just ci

完了前に diff と行数を記録する。

    git diff --stat
    wc -l src/app.rs src/app/*.rs src/ui.rs src/ui/*.rs src/git.rs src/git/*.rs src/init.rs src/init/*.rs
    just ci

## Validation and Acceptance

受け入れ条件は、ユーザーから見える kizu の挙動が変わらず、責務境界が小さな module に分離されていることである。最低条件として `cargo test --all-targets --all-features` は 467 件以上成功し、`cargo clippy --all-targets --all-features -- -D warnings` は warning なしで通る。

`just ci` は app/ui/runtime の各大波後と最終完了時に通す。e2e は 35 件成功を期待する。特に navigation、file view wrap、line-number gutter、sticky header、search highlight、scar insert/undo、Stream mode event ingestion、init install/teardown の tests が成功していることを確認する。

手動確認は `just run` で kizu を起動し、通常 diff view、Tab の Stream mode 切替、`w` wrap toggle、`#` line-number toggle、`/` search、Enter file view、`a` / `r` / `c` scar、`u` undo、`s` picker、`e` editor invocation が従来と同じ感触で動くことを確認する。

性能面の受け入れ条件は、大量 hunk で過去に最適化した hot path を悪化させないことである。必要に応じて一時 probe を使い、`current_hunk_range`、`viewport_top nowrap`、`viewport_placement wrap`、`build_layout no seen` の before/after を計画に記録してから probe を削除する。

## Idempotence and Recovery

各 milestone は file move と import/visibility 調整を中心にし、ユーザーから見える behavior change を混ぜない。途中で compile error が出た場合は、直近 milestone の module visibility、`pub(crate) use`、test module path を直して再実行する。

`cargo fmt` は formatter による機械的変更として許可する。手作業のファイル編集は `apply_patch` を使う。`git reset --hard` や `git checkout --` は使わない。既存のユーザー差分が出てきた場合は巻き戻さず、影響がある時だけ読み込んで合わせる。

大規模 move で失敗した場合は、その milestone の patch を小さく割る。例えば `src/app/navigation.rs` が大きすぎる場合は、先に pure helper と `VisualIndex` だけを移し、次に `impl App` を移す。UI split で alignment test が落ちた場合は、renderer 移動を止めて `RenderGeometry` の入力と `app.last_body_width` 更新タイミングを先に固定する。

## Artifacts and Notes

現状調査の主要な証拠は以下である。

    git status --short --branch
    ## main...origin/main

    cargo test --all-targets --all-features
    test result: ok. 467 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.92s

    cargo clippy --all-targets --all-features -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.14s

    wc -l
    9480 src/app.rs
    3533 src/ui.rs
     591 src/ui/footer.rs
     228 src/ui/overlays.rs
     136 src/stream.rs
    2228 src/git.rs
    1428 src/watcher.rs
    1151 src/hook.rs
     368 src/config.rs
    2116 src/init.rs
     215 src/main.rs
     923 src/scar.rs

第一波完了後の主要な証拠は以下である。

    wc -l src/app.rs src/app/*.rs
        6128 src/app.rs
         351 src/app/file_view.rs
         193 src/app/input.rs
         524 src/app/layout.rs
         670 src/app/navigation.rs
         128 src/app/picker.rs
         486 src/app/review.rs
         308 src/app/runtime.rs
         197 src/app/search.rs
         350 src/app/stream_events.rs
          93 src/app/text_input.rs
        9428 total

    just ci
    cargo fmt --all -- --check
    cargo clippy --all-targets --all-features -- -D warnings
    cargo test --all-targets --all-features
    test result: ok. 467 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.90s
    cargo build --release --locked
    bun test
    35 pass
    0 fail
    Ran 35 tests across 8 files. [25.80s]

最近の履歴では、app/ui の test fixture 削減と performance pass が main に入っている。主な perf commits は `2744b3c perf: cache many-hunk viewport layout`、`3c60f30 perf: speed up hunk navigation lookup`、`a53d30b perf: narrow search match projection` である。これらの性能特性を壊さないよう、layout/navigation/UI geometry の移動は targeted tests と必要な probe で確認する。

## Interfaces and Dependencies

`src/app.rs` は当面 facade とし、外部から見える `crate::app::App`、`crate::app::ViewMode`、`crate::app::RowKind`、`crate::app::FileViewState`、`crate::app::SearchState`、`crate::app::find_matches`、`crate::app::VisualIndex`、`crate::app::run` の import を壊さない。内部 module は `mod layout; mod navigation; mod input; mod picker; mod search; mod text_input; mod review; mod file_view; mod stream_events; mod runtime;` を持ち、必要な型だけ `pub use` / private `use` する。

`src/app/layout.rs` は `ScrollLayout`、`RowKind`、`HunkAnchor`、`VisualIndex`、`hunk_fingerprint`、`seen_hunk_fingerprint`、`build_scroll_layout` を持つ。`build_scroll_layout(files, seen_hunks)` は `ScrollLayout` を返し、`App::build_layout` は `self.layout = build_scroll_layout(...)` と `self.visual_index_cache` invalidation を担当する。

`src/app/navigation.rs` は `impl App` として `scroll_by`、`scroll_to_visual`、`scroll_to`、`viewport_placement`、`next_hunk`、`prev_hunk`、`next_change`、`prev_change`、`follow_restore`、`refresh_anchor` を持つ。`VisualIndex` cache は `App` に残し、cache key は `body_width: Option<usize>` のままにする。

`src/app/input.rs` は `handle_key` と mode dispatch を持つ。戻り値の `KeyEffect` は `runtime` が解釈するため、`KeyEffect` の variants は `None`、`ReconfigureWatcher`、`OpenEditor(EditorInvocation)` を維持する。

`src/app/runtime.rs` は `pub async fn run(cwd: PathBuf, startup_trace: bool) -> Result<()>` の entry を持ち、terminal setup、session write/remove、watcher start、startup event replay、run loop、external editor suspend/resume を担当する。`App` は tokio/crossterm event stream を直接知らない状態に近づける。

`src/app/review.rs` は `ScarUndoEntry`、`ScarCommentState`、`RevertConfirmState` と scar/revert 操作を持つ。`src/app/stream_events.rs` は `DiffSnapshots`、`StreamEvent`、event log path validation、startup replay を持つ。`src/app/text_input.rs` は search と scar comment の共通 text editing primitive を持つ。

`src/ui/geometry.rs` は `RenderGeometry` を返す helper を持つ。diff view と file view の body width、line-number gutter width、effective line-number flag はここを通す。Stream mode は line numbers off の invariant をここで表現する。

`src/ui/line_numbers.rs` は `diff_ln_span`、`file_ln_span`、`add_line_number_gutters`、`insert_blank_gutter`、`insert_blank_gutter_at` を持つ。`LineNumberGutter` の width 決定は `geometry.rs` に置き、span insertion はこの module に閉じる。

`src/git.rs` は facade として public API を維持する。`FileDiff`、`DiffContent`、`FileStatus`、`Hunk`、`DiffLine`、`LineKind` は `src/git/types.rs` へ移し、`pub use types::*;` で既存 import を保つ。Parser は `parse_unified_diff` を `pub(crate)` のまま維持する。

`src/init.rs` は facade として `AgentKind`、`Scope`、`SupportLevel`、`DetectedAgent`、`InstallReport`、`detect_agents`、`run_init`、`run_teardown`、`shell_single_quote` を維持する。agent-specific schema は内部 adapter module に閉じ、teardown の user hook preservation invariant を tests で固定する。
