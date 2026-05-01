# performance benchmark suite と hot path 改善

この ExecPlan はリビングドキュメントです。`Progress`、`Surprises & Discoveries`、`Decision Log`、`Outcomes & Retrospective` は作業の進行に合わせて更新します。

## Purpose / Big Picture

kizu は AI エージェントの横で常時動く TUI なので、重い repo や長いセッションでも「起動する」「diff を読む」「キー操作に追従する」「hook が待たせない」ことが体験の中核になる。これまでの性能確認は一時 probe で効果を見た後に削除されていたため、同じ回帰を継続的に測る場所がない。

この変更では、主要操作を恒久的な `cargo bench` 対象として repo に残し、実測で見つかった hot path を改善する。完了後、開発者は `/Users/annenpolka/ghq/github.com/annenpolka/kizu` で `cargo bench --bench operations` を実行し、diff 解析、layout、navigation、search、render、highlight、hook、scar、stream の各操作が継続的に測れる。

## Progress

- [x] (2026-05-01 05:33:45Z) 仕様確認: `/Users/annenpolka/ghq/github.com/annenpolka/kizu/docs/SPEC.md` で v0.1 から v0.5 の操作面を確認した。
- [x] (2026-05-01 05:33:45Z) 既存 perf 履歴確認: `plans/app-ui-responsibility-refactor.md` と memory の記録で、過去の一時 probe が `current_hunk_range`、`viewport_top`、`viewport_placement`、`build_layout` だけを測っていたことを確認した。
- [x] (2026-05-01 05:33:45Z) 現状確認: `git status --short --branch` は `## main...origin/main` で未コミット差分なしだった。
- [x] (2026-05-01 05:36:48Z) Red: benchmark fixture の形を固定する `perf_fixture_many_hunk_app_has_requested_shape` と `perf_fixture_large_unified_diff_parses_to_requested_shape` を追加し、未実装関数で期待通り compile fail を確認した。
- [x] (2026-05-01 05:42:31Z) Green: `src/lib.rs` と `src/perf.rs` を追加し、`cargo test perf_fixture -- --nocapture` が 2 件成功した。
- [x] (2026-05-01 05:45:50Z) Baseline: `cargo bench --bench operations` を追加して初回計測を完了した。対象は diff parse、layout/viewport、navigation、search、render、highlight、hook、scar、stream。
- [x] (2026-05-01 05:50:44Z) Improvement 1: `Highlighter` に path+line cache を追加し、`highlight_line_reuses_cached_tokens_for_same_path_and_content` を Red/Green で通した。
- [x] (2026-05-01 05:54:31Z) Improvement 2: search の ASCII lowercase smart-case path から per-line `to_ascii_lowercase()` allocation を削除し、`ascii_case_insensitive_find_reports_original_byte_offsets` を Red/Green で通した。
- [x] (2026-05-01 06:06:22Z) Validation: current tree で `cargo bench --bench operations`、`just rust`、`cargo fmt --all -- --check`、`git diff --check`、ExecPlan validator が成功した。
- [x] (2026-05-01 06:15:10Z) Full gate: `just ci` が成功した。unit tests 471 件、release build、tuistory e2e 35 件が成功した。

## Surprises & Discoveries

- Observation: 現在の repo には `benches/` も Criterion 依存もない。
  Evidence: `find benches -maxdepth 2 -type f -print` は `No such file or directory`、`Cargo.toml` の dev-dependencies は `tempfile` のみだった。

- Observation: 旧 performance pass は恒久 benchmark ではなく一時 probe だった。
  Evidence: `plans/app-ui-responsibility-refactor.md` は `4,000 hunks / 4 lines / 80 cells` の ignored probe として記録しており、steady-state tree には probe が残っていない。

- Observation: kizu は現時点で binary-only crate なので、integration benchmark から module を import できない。
  Evidence: `/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/main.rs` が `mod app; mod git; ...` を直接持ち、`/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/lib.rs` は存在しない。

- Observation: 初回 benchmark では syntax highlight と render が可視 hot path だった。
  Evidence: 初回 `cargo bench --bench operations` は `highlight_300_rust_lines` が約 `5.89ms`、`render/full_frame_nowrap` が約 `960µs`、`render/full_frame_wrap` が約 `749µs` だった。

- Observation: lowercase smart-case search は ASCII の一般ケースでも毎 DiffLine を lowercase allocation していた。
  Evidence: `src/app/search.rs` は `line.content.to_ascii_lowercase()` を行ってから `find` していた。final bench では `search/find_matches_many_rows` が初回約 `1.77ms` から約 `1.10ms` になった。

- Observation: library facade 化により、これまで binary target では出ていなかった public API clippy lint が出た。
  Evidence: `just rust` 初回は `TerminalKind::from_str`、`AgentKind::from_str`、`Highlighter::new` に対して `should_implement_trait` / `new_without_default` を報告した。`FromStr` と `Default` 実装へ寄せて解消した。

- Observation: `paths` と `hook` の env-var tests は module-local mutex では直列化できていなかった。
  Evidence: `just ci` 初回で `paths::tests::events_dir_is_per_project` が `path_b.starts_with("/tmp/kizu-test-state/events/")` に失敗した。`KIZU_STATE_DIR` を hook tests が別 mutex で触っていたため、`src/test_support.rs` の crate-wide `ENV_LOCK` に統合した。

## Decision Log

- Decision: 一時 probe ではなく Criterion の `benches/operations.rs` を追加する。
  Rationale: ユーザーの要求は「徹底的にあらゆる操作をベンチマーク」なので、後続作業でも再実行できる測定面を repo に残す必要がある。Criterion は Rust で標準的な統計付き microbenchmark で、hot path の相対変化を追いやすい。
  Date/Author: 2026-05-01 05:33:45Z / Codex

- Decision: binary-only 構成を `src/lib.rs` + thin `src/main.rs` に分け、bench は library 経由で測る。
  Rationale: `benches/` は integration crate として build されるため、binary の private modules へ直接入れない。library facade によって通常 binary と benchmark が同じ production modules を使える。
  Date/Author: 2026-05-01 05:33:45Z / Codex

- Decision: benchmark 対象は「ユーザー操作」と「hook/stream の非 UI 操作」を分けて網羅する。
  Rationale: SPEC の操作面は TUI key 操作だけでなく、watcher-driven recompute、PostToolUse/Stop hook、stream event log を含む。ユーザーが体感する遅さはどちらからも来る。
  Date/Author: 2026-05-01 05:33:45Z / Codex

- Decision: syntax highlight は `Highlighter` 内の bounded cache で最適化する。
  Rationale: TUI は同じ可視行を frame ごとに繰り返し描くため、同じ `(path, line)` を毎回 syntect に通す必要はない。key を path+line content にすれば watcher recompute 後も content が変わった行だけ naturally miss し、古い entry は 8,192 件で上限を持てる。
  Date/Author: 2026-05-01 05:50:44Z / Codex

- Decision: ASCII lowercase search は allocation-free helper で byte offset を直接返す。
  Rationale: `find_matches` の byte offsets は renderer の search highlight にそのまま使うため、haystack を lowercase した別 String 上で探すより、元 content の byte slice を case-insensitive 比較する方が速くて offset 変換も不要である。
  Date/Author: 2026-05-01 05:54:31Z / Codex

## Outcomes & Retrospective

Criterion benchmark suite を `benches/operations.rs` として追加し、`cargo bench --bench operations` で主要操作を継続計測できるようにした。binary-only だった crate は `src/lib.rs` を持つ構成に変え、`src/main.rs` は CLI dispatch の thin binary になった。

性能改善は二つ入った。`src/highlight.rs` は `Highlighter` に path+line content cache を持たせ、同じ visible line を再描画するたびに syntect を走らせない。`src/app/search.rs` は lowercase ASCII smart-case 検索で per-line lowercase String を作らず、元 content の byte offset を直接探す。

初回 baseline から final current tree への主要値は、`highlight_300_rust_lines` が約 `5.89ms` から `52.4µs`、`render/full_frame_nowrap` が約 `960µs` から `264.8µs`、`render/full_frame_wrap` が約 `749µs` から `272.4µs`、`search/find_matches_many_rows` が約 `1.77ms` から `1.10ms` になった。`stream/build_stream_files_200_events` は約 `7.0ms` のまま残っており、次の大きな改善候補である。

## Context and Orientation

`/Users/annenpolka/ghq/github.com/annenpolka/kizu/docs/SPEC.md` は canonical specification である。v0.1 は filesystem と `git diff` から main diff view を作る。v0.2 は scar と hook を追加する。v0.3 は stream mode、event log、`--attach`、config を追加する。v0.4 は seen hunk 折りたたみ、v0.5 は line-number gutter と EOF newline marker を追加する。

主要 hot path は次の通り。

`/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/git/parse.rs` は `git diff --no-renames` の stdout を `FileDiff` に変換する。大きな diff ではここが recompute の入口になる。

`/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/app/layout.rs` は `ScrollLayout` と `VisualIndex` を作る。`ScrollLayout` は logical row の平坦化、hunk range、change run、line-number cache を持つ。`VisualIndex` は wrap mode で logical row と visual row を変換する。

`/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/app/navigation.rs` は `j/k/J/K`、hunk jump、viewport placement を扱う。大きな diff でキー入力に直結する。

`/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/app/search.rs` は `/` 検索と `n/N` jump の match list を作る。

`/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/ui/diff_view.rs` と `/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/ui/diff_line.rs` は render hot path である。ここは visible rows だけを描くが、syntax highlight、search highlight、wrap、line-number gutter が重なる。

`/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/highlight.rs` は syntect による line highlight を提供する。初期化と per-line highlight を分けて測る。

`/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/hook/scan.rs` と `/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/hook/input.rs` は hook latency に効く。PostToolUse/Stop hook が遅いとエージェントの操作全体が待たされる。

`/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/scar.rs` は `a/r/c/u` の実ファイル書き込み操作に効く。

`/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/stream.rs` と `/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/app/stream_events.rs` は stream mode の per-operation diff と event ingestion に効く。

## Plan of Work

最初に `src/lib.rs` を追加し、現在 `src/main.rs` にある production modules を library 側へ移す。`src/main.rs` は CLI parsing と command dispatch だけを残し、`kizu::app`、`kizu::git`、`kizu::hook` などを呼ぶ thin binary にする。これは benchmark から production code を import するための土台であり、外部動作は変えない。

次に `src/perf.rs` を追加する。この module は benchmark fixture 専用で、synthetic many-hunk app、large unified diff text、hook JSON payload、scar target file、render frame helper などを提供する。production の TUI 実行経路からは呼ばれないが、同じ `App`、`ScrollLayout`、`ui::render`、`git::parse`、`hook`、`scar` を通す。

その後 `Cargo.toml` に `criterion` と `[[bench]]` を追加し、`benches/operations.rs` を作る。bench group は `git_diff_parse`、`layout_and_viewport`、`navigation`、`search`、`render`、`highlight`、`hook`、`scar`、`stream` に分ける。

ベンチを一度走らせ、最も重い割に改善余地のある箇所を特定する。改善はテスト先行で行い、少なくとも一つの hot path で before/after を plan に記録する。最後に formatting、lint、unit tests、bench を確認する。

## Concrete Steps

作業ディレクトリは常に `/Users/annenpolka/ghq/github.com/annenpolka/kizu`。

1. Red テストを追加して fixture 仕様を固定する。

        cargo test perf_fixture -- --nocapture

   期待: 実装前は未定義関数または assertion で失敗し、fixture の必要条件が明確になる。

2. library facade と perf fixtures を実装し、Red を Green にする。

        cargo test perf_fixture -- --nocapture

   期待: `test result: ok`。

3. benchmark 依存と `benches/operations.rs` を追加し、benchmark 一覧を実行する。

        cargo bench --bench operations

   期待: Criterion が各 group の測定結果を出力し、panic しない。

4. 測定結果に基づく改善を実装する。

        cargo test --all-targets --all-features <new_or_targeted_test>
        cargo bench --bench operations

   期待: targeted test が通り、改善対象 bench の mean が下がる。

5. 最終検証を行う。

        just rust
        cargo bench --bench operations

   必要に応じて `just ci` も実行する。

## Validation and Acceptance

受け入れ条件は次のすべてを満たすこと。

`cargo bench --bench operations` が存在し、git diff parse、layout build、viewport placement、navigation、search、full frame render、syntax highlight、hook parse/scan、scar insert/remove、stream operation diff の測定結果を出す。

ベンチは production modules を通り、standalone の作り物ロジックだけを測らない。

少なくとも一つの実測 hot path でコード改善を行い、before/after をこの plan に記録する。

`cargo test --all-targets --all-features` と `cargo clippy --all-targets --all-features -- -D warnings` が通る。時間が許す場合は `just ci` まで通す。

## Idempotence and Recovery

`cargo bench --bench operations` は何度実行しても source tree を変えない。scar の filesystem benchmark は一時ディレクトリ内で完結させ、測定後に削除される。

`src/lib.rs` への分割で compile error が出た場合は、`src/main.rs` に残った module declaration と library module declaration の二重定義、または binary から呼ぶ module の visibility を確認する。

benchmark dependency の追加で lockfile が更新されるのは期待通りである。network 取得に失敗した場合は再実行し、sandbox 起因の失敗なら承認付きで再実行する。

## Artifacts and Notes

現時点の確認結果:

    git status --short --branch
    ## main...origin/main

    find benches -maxdepth 2 -type f -print
    find: benches: No such file or directory

    wc -l src/app.rs src/app/layout.rs src/app/navigation.rs src/ui/diff_line.rs src/ui/diff_view.rs src/git/parse.rs src/hook/scan.rs src/highlight.rs
        6128 src/app.rs
         524 src/app/layout.rs
         670 src/app/navigation.rs
         262 src/ui/diff_line.rs
         477 src/ui/diff_view.rs
         321 src/git/parse.rs
         109 src/hook/scan.rs
         146 src/highlight.rs

過去の一時 probe 記録:

    current_hunk_range x1000: 23.746458ms -> 2.292µs
    viewport_top nowrap x1000: 15.468542ms -> 2.958µs
    viewport_placement wrap x200: 251.945917ms -> 2.954625ms
    build_layout no seen x100: 42.202333ms -> 20.6235ms

Red 確認:

    cargo test perf_fixture -- --nocapture
    error[E0425]: cannot find function `many_hunk_app` in this scope
    error[E0425]: cannot find function `large_unified_diff` in this scope
    error[E0425]: cannot find function `parse_unified_diff_for_bench` in this scope

初回 benchmark baseline:

    git_diff_parse/parse_large_unified_diff: 214.16 µs
    layout_and_viewport/build_layout_4000_hunks: 344.20 µs
    layout_and_viewport/current_hunk_range: 1.2731 ns
    layout_and_viewport/viewport_placement_nowrap: 15.406 ns
    layout_and_viewport/viewport_placement_wrap_cached: 27.408 ns
    navigation/next_change_1000_steps: 250.46 µs
    navigation/prev_change_1000_steps: 275.78 µs
    search/find_matches_many_rows: 1.7694 ms
    render/full_frame_nowrap: 959.84 µs
    render/full_frame_wrap: 749.49 µs
    highlight/highlight_300_rust_lines: 5.8931 ms
    hook/parse_hook_payload_40_paths: 10.566 µs
    hook/sanitize_hook_event: 577.58 ns
    hook/scan_scars_80_files: 1.0341 ms
    scar/insert_and_remove_scar_200_line_file: 214.48 µs
    stream/compute_operation_diff: 54.515 µs
    stream/build_stream_files_200_events: 7.0739 ms

改善後の targeted benchmark:

    highlight/highlight_300_rust_lines: 50.807 µs, change -99.143%
    render/full_frame_nowrap: 267.44 µs, change -72.073%
    render/full_frame_wrap: 273.43 µs, change -63.562%
    search/find_matches_many_rows: 1.1269 ms, change -35.900%

current tree final benchmark:

    git_diff_parse/parse_large_unified_diff: 208.00 µs
    layout_and_viewport/build_layout_4000_hunks: 343.53 µs
    layout_and_viewport/current_hunk_range: 1.3250 ns
    layout_and_viewport/viewport_placement_nowrap: 15.379 ns
    layout_and_viewport/viewport_placement_wrap_cached: 27.989 ns
    navigation/next_change_1000_steps: 235.87 µs
    navigation/prev_change_1000_steps: 266.92 µs
    search/find_matches_many_rows: 1.1029 ms
    render/full_frame_nowrap: 264.80 µs
    render/full_frame_wrap: 272.40 µs
    highlight/highlight_300_rust_lines: 52.424 µs
    hook/parse_hook_payload_40_paths: 10.577 µs
    hook/sanitize_hook_event: 572.21 ns
    hook/scan_scars_80_files: 1.0224 ms
    scar/insert_and_remove_scar_200_line_file: 214.39 µs
    stream/compute_operation_diff: 55.351 µs
    stream/build_stream_files_200_events: 7.0432 ms

Validation:

    cargo test perf_fixture -- --nocapture
    2 passed

    cargo test highlight_line_reuses_cached_tokens_for_same_path_and_content -- --nocapture
    1 passed

    cargo test ascii_case_insensitive_find_reports_original_byte_offsets -- --nocapture
    1 passed

    cargo test find_matches_ -- --nocapture
    4 passed

    just rust
    clippy succeeded
    test result: ok. 471 passed; 0 failed

    just ci
    cargo fmt --all -- --check: success
    cargo clippy --all-targets --all-features -- -D warnings: success
    cargo test --all-targets --all-features: 471 passed
    cargo build --release --locked: success
    bun test: 35 pass / 0 fail

    cargo fmt --all -- --check
    success

    git diff --check
    success

    python3 /Users/annenpolka/.agents/skills/execplan-manager/scripts/validate_execplan.py plans/performance-benchmark-suite.md
    ExecPlan is valid

## Interfaces and Dependencies

`/Users/annenpolka/ghq/github.com/annenpolka/kizu/Cargo.toml` に次を追加する。

    [dev-dependencies]
    criterion = "0.5"

    [[bench]]
    name = "operations"
    harness = false

`/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/lib.rs` は production modules を公開する。`src/main.rs` は library を使う binary entrypoint になる。

`/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/perf.rs` は `#[doc(hidden)]` の benchmark support module とし、次の public helper を持つ。

    pub fn many_hunk_app(hunks: usize, lines_per_hunk: usize, line_width: usize) -> App
    pub fn rebuild_layout(app: &mut App)
    pub fn large_unified_diff(files: usize, hunks_per_file: usize, lines_per_hunk: usize, line_width: usize) -> String
    pub fn parse_unified_diff_for_bench(raw: &str) -> Vec<FileDiff>
    pub fn render_frame_for_bench(app: &App, width: u16, height: u16)
    pub fn operation_diff_for_bench(previous: &str, current: &str) -> String
    pub fn hook_payload_json(paths: usize) -> String

`/Users/annenpolka/ghq/github.com/annenpolka/kizu/benches/operations.rs` は Criterion を使い、各 operation を `black_box` で測る。
