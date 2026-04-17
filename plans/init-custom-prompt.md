# kizu init — dialoguer から自前インタラクティブプロンプトへの移行

この ExecPlan はリビングドキュメントです。`Progress` / `Surprises & Discoveries` / `Decision Log` / `Outcomes & Retrospective` の各セクションは作業進行に合わせて継続的に更新します。

## Purpose / Big Picture

このマイルストーンを完了すると、`kizu init` のインタラクティブ選択画面で **色付きアイコンと詳細ラベルが再び表示される**ようになります。現状は dialoguer のバグ (後述) を回避するためにプレーンテキストで表示しており、サポートレベル (Full / StopOnly 等) の視認性と、バイナリ検出状態 (`✓ detected` / `~ bin only` / `✗ not found`) の一目分かりやすさが失われています。

動作確認の代表例:

    kizu init
    # 画面遷移:
    #
    #   傷  kizu init
    #   Hook installer for AI coding agent scar review
    #
    #   ? Select agents to install hooks for (space to toggle, enter to confirm)
    #   > [x] claude-code   ● Full                                ✓ detected
    #     [ ] cursor        ◐ PostToolOnlyBestEffort              ~ bin only
    #     [ ] codex         ◐ StopOnly                            ✗ not found
    #     [ ] qwen-code     ○ WriteSideOnly                       ✗ not found
    #     [ ] cline         ● Full                                ✗ not found
    #     [ ] gemini        ○ WriteSideOnly                       ✗ not found
    #
    # j/k または矢印で移動、space でトグル、Enter で確定、Esc/Ctrl-C でキャンセル。
    # キー操作ごとに画面が上にドリフトしないこと (現状 dialoguer 版は ANSI を
    # items に入れるとドリフトする)。

## Progress

作業を開始する前、完了直後、途中で停止する際に必ず更新してください。タイムスタンプは `(YYYY-MM-DD HH:MM:SSZ)` 形式。

### M1: `src/prompt.rs` 新設 + 純粋レンダリングの TDD — 完了

- [x] (2026-04-17) `SelectState` / `MultiSelectState` / `PromptKey` / `Outcome` 定義 (pure, I/O なし)
- [x] (2026-04-17) `render_select_frame` / `render_multi_frame` を `unicode-width` + ANSI strip で実装
- [x] (2026-04-17) `map_key` で crossterm `KeyEvent` → `PromptKey` 変換
- [x] (2026-04-17) `apply_select_key` / `apply_multi_key` で cursor / checkbox / confirm / cancel を pure に
- [x] (2026-04-17) truncation + ANSI 保存 (`truncate_to_width`) のテスト含む 23 本を追加

### M2: crossterm バインディングと TTY 入出力層 — 完了

- [x] (2026-04-17) `run_select_one` / `run_multi_select` 実装 (`Ok(None)` = キャンセル)
- [x] (2026-04-17) `RawModeGuard` の `Drop` で `disable_raw_mode` を確実に呼ぶ
- [x] (2026-04-17) 非 TTY 時は `ensure_tty` が早期 bail (呼び出し側は `--non-interactive` を要求するメッセージ)
- [x] (2026-04-17) `Event::Resize` を受信したら次ループで再描画
- [x] (2026-04-17) Ctrl-C / Ctrl-D / Esc / `q` をキャンセル扱いに統一

### M3: `init.rs` の 3 箇所を差し替え + dialoguer 除去 — 完了

- [x] (2026-04-17) `select_agents_interactive` を `run_multi_select` + `support_level_colored` + `detection_status_colored` + `pad_visible` (ANSI を可視幅で数える自前パディング) に置換
- [x] (2026-04-17) `select_scope_interactive` を `run_select_one` + `c_bold` + `c_dim` 注釈 付きラベルに置換
- [x] (2026-04-17) `ask_scope_fallback` を `run_select_one` に置換
- [x] (2026-04-17) `support_level_colored` / `detection_status_colored` を 8b0f9dd 以前の表現で復活
- [x] (2026-04-17) `Cargo.toml` から `dialoguer = "0.12"` を削除
- [x] (2026-04-17) `cargo tree -p kizu` で `dialoguer` / `console` / `shell-words` / `zeroize` の 4 crate が消えたことを確認
- [x] (2026-04-17) `cargo build --release --locked` 通過

### M4: ADR-0019 + e2e 追加 + CI 緑化 — 完了

- [x] (2026-04-17) `docs/adr/0019-custom-prompt-for-init.md` を Michael Nygard 形式で追加
- [x] (2026-04-17) 既存 `tests/e2e/init.test.ts` の "interactive init shows agent selection prompt" が新プロンプトで通ることを確認
- [x] (2026-04-17) `tests/e2e/init.test.ts` に新規 3 シナリオを追加:
  - `interactive init shows support-level and detection icons`: `[●◐○]` と `[✓~✗]` を正規表現で検出
  - `interactive init does not drift the banner on repeated keypresses`: `j×20 + k×20` のあとも `kizu init` バナー行が残存することを確認
  - `interactive scope prompt highlights project-local as default`: `/>\s+project-local/` で初期カーソル位置を検証
- [x] (2026-04-17) `just ci` 全 pass (fmt-check → clippy -D warnings → cargo test 346 passed → release build --locked → e2e 28 passed)

## Surprises & Discoveries

- Observation: dialoguer 0.12.0 `src/prompts/multi_select.rs:227` は `let size = &items.len();` で **バイト長**を `size_vec` に積み、`clear_preserve_prompt` がこれを `term.size().1` と比較して「折り返し行数」を推定する。ANSI エスケープを含む item だとバイト数が膨らみ、ターミナル幅を超えたと誤判定されて余分に行クリア → 表示が上にドリフトする。
  Evidence: `diff ~/.cargo/registry/src/.../dialoguer-0.11.0/src/prompts/multi_select.rs ~/.cargo/registry/src/.../dialoguer-0.12.0/src/prompts/multi_select.rs` — 0.11 と 0.12 で該当コード完全一致。`dialoguer-0.12.0/src/theme/render.rs` も 0.11 と差分 0。`console` 0.15→0.16 の diff にも関連する修正なし。
- Observation: ユーザー要望の本質は「init のアイコン表示を戻したい」だが、dialoguer のバグを前提に work around するには ANSI を諦めるか自前実装するかの二択。自前実装なら crossterm + unicode-width が既に依存に入っているので増依存ゼロ。
  Evidence: `grep -E '^(crossterm|unicode-width)' Cargo.toml` で両方存在。dialoguer を外すと `console` (0.16.3) / `shell-words` (1.1.1) / `zeroize` (1.8.2) の 3 crate が transitively 不要になる (`cargo tree -p kizu | grep dialoguer` の出力で確認済み)。

## Decision Log

- Decision: dialoguer を fork/patch するのではなく、自前で最小プロンプトを実装する
  Rationale: (1) 公開 issue や upstream 修正の見通しが不明で fork 維持コストが読めない、(2) kizu は crossterm + ratatui + unicode-width を既に採用しており、TTY 入出力のスタックが揃っている、(3) 置き換え対象は `init.rs` 3 箇所に限定され、多機能プロンプト (input / password / autocomplete) は不要なので実装表面積が小さい、(4) ANSI 可視幅計算は kizu の diff view で既に解いている問題で、ノウハウを持っている。
  Date/Author: 2026-04-17 / Initial implementer
- Decision: 代替ライブラリ (inquire 等) への乗り換えは採用しない
  Rationale: 依存の付け替えは同じリスク (将来別のバグを踏む) を残すうえ、kizu の UX 要件は「アイコン + サポートレベル表示 + 検出状態」で極めて限定的。自前実装の方が依存数も減る。
  Date/Author: 2026-04-17 / Initial implementer
- Decision: プロンプト層は `src/prompt.rs` 1 ファイルに閉じ、TUI ランタイム (tokio current_thread) とは独立の同期 API にする
  Rationale: `kizu init` は TUI 起動前に走る完全同期コマンドであり、非同期化のメリットがない。同期 API なら `main.rs` / `init.rs` からそのまま呼べる。raw mode の enter/leave も同期で完結する方が RAII ガードが書きやすい。
  Date/Author: 2026-04-17 / Initial implementer
- Decision: アイコン色は 8b0f9dd 削除以前の表現に合わせる (`●` = Full / `◐` = StopOnly 系 / `○` = WriteSideOnly / `✓` = detected / `~` = bin only / `✗` = not found)
  Rationale: 過去のリリースで使っていた表現で、ユーザーが「いつの間にか消えた」と認識しているのはこれ。新デザインを考案するより過去の体験を復元する方が低リスク。
  Date/Author: 2026-04-17 / Initial implementer

## Outcomes & Retrospective

**結果 (2026-04-17 完了)**:

- `src/prompt.rs` (約 520 行、うちテスト約 260 行) を新設し、`dialoguer` + transitive 3 crate (`console` / `shell-words` / `zeroize`) を依存から除去
- `kizu init` のアイコン付きインタラクティブプロンプトを復活 (Full/StopOnly/WriteSideOnly の `● / ◐ / ○`、検出状態の `✓ / ~ / ✗`)
- スクロールドリフト再発なし: e2e で j×20+k×20 押下後もバナー行 `kizu init` が残存することを確認
- `just ci` 全 pass: Rust 346 tests + e2e 28 tests

**うまく回ったこと**:

- 描画層を pure 関数 (`render_*_frame` / `apply_*_key`) に分離したことで、raw mode を立ち上げずに 23 本の単体テストでレイアウトと状態遷移を固定できた
- `dialoguer` のバグ原因が `items.len()` (バイト長) であることを 0.11 と 0.12 のソース diff で特定してから着手したので、自前実装では「行は折り返さず `…` 切り詰め + 可視幅は ANSI ストリップ後の `unicode-width`」という一貫方針で迷いがなかった
- 既存の e2e がそのまま通ったのは、キーコード (j/k/space/enter/esc) を dialoguer 互換に揃えたため

**予想外だったこと**:

- `dialoguer 0.11 → 0.12` で該当コードも `render.rs` も **完全同一**だった (0.12 アップグレードはバグ修正を含んでいなかった)。移行動機として十分だった
- `pad_visible` ヘルパーを `src/init.rs` 側に置く必要があった。prompt 層は「切り詰め」は面倒見るが「パディング」は呼び出し側責任、という分担

**ギャップ・残作業**:

- Ctrl-C や panic 時に `cursor::Show` が実行されないと端末がカーソル隠しのままになりうる。RAII で raw mode は戻るが、cursor visibility は戻らない。**受け入れる**: 実害は極めて軽微 (`echo -e '\e[?25h'` でリカバー可能)、panic が走るのは通常フローでないため
- マルチ select の検出状態 ` ` 背景色は付けていない (シンプルに `>` マーカーで表現)。将来 `config.colors.prompt_selected` を導入する余地はあるが、現状ニーズなし

## Context and Orientation

### 現状のファイル構成

- `src/init.rs` (1461 行) — `kizu init` / `kizu teardown` の実装。3 箇所で dialoguer を呼ぶ:
  - L335-367 `select_agents_interactive` — `dialoguer::MultiSelect` (エージェント複数選択)
  - L369-391 `select_scope_interactive` — `dialoguer::Select` (scope 3 択)
  - L537-576 `ask_scope_fallback` — `dialoguer::Select` (非互換 scope 時の代替選択)
- `src/main.rs` — サブコマンドディスパッチ。`init` コマンドから `init::run_init` を呼ぶ
- `src/app.rs` — TUI ランタイム (別系統)。raw mode 操作は `tui::enter` / `tui::leave` に集約されている。本 ExecPlan の自前プロンプトとは独立
- `Cargo.toml` — `dialoguer = "0.12"`, `crossterm = "0.29.0"`, `unicode-width = "0.2"`, `ratatui = "0.30.0"`

### 用語定義

- **dialoguer**: Rust の対話プロンプト crate。`MultiSelect` / `Select` / `Input` / `Password` などを提供。kizu では `MultiSelect` と `Select` のみ使用
- **raw mode**: ターミナルのライン編集バッファリングとエコーを無効化し、1 キーごとにプログラムが受け取れるモード。有効中はプロセスが落ちると端末が壊れるので RAII で必ず戻す
- **unicode-width**: 文字列を「ターミナル表示セル幅」単位で測るライブラリ。CJK は 2、ASCII は 1、ANSI エスケープは 0 を返せる (ANSI の扱いは自前でストリップが必要)
- **support level** (`src/init.rs::SupportLevel`): 各 AI エージェントがどこまで kizu の hook を活かせるかの区分。`Full` / `StopOnly` / `PostToolOnlyBestEffort` / `WriteSideOnly`
- **detection state**: そのエージェントのバイナリと設定ディレクトリがこのマシンに存在するか。`DetectedAgent::binary_found` / `config_dir_found` で表現
- **scope**: hook 設定を書き込む場所。`project-local` (gitignored) / `project-shared` (commit 対象) / `user` (`~/.claude/` 等)

### 既存依存関係の再利用方針

crossterm は `event-stream` feature で入っているが、同期 API (`event::read()` / `terminal::enable_raw_mode()` / `cursor::MoveTo` など) もそのまま使える。ratatui は使わない (本プロンプトは全画面 TUI ではなく、カーソル下に数行だけ描画するインライン UI)。

## Plan of Work

1. **`src/prompt.rs` 新規作成**
   - `pub struct PromptItem { pub label: String, pub visible_width: usize }` (ANSI ストリップ済み幅を事前計算)
   - `pub enum PromptKey { Up, Down, Home, End, Toggle, Confirm, Cancel, Other }` (crossterm `KeyEvent` → `PromptKey` の変換を `map_key` で)
   - `pub struct SelectState { prompt: String, items: Vec<PromptItem>, cursor: usize }`
   - `pub struct MultiSelectState { prompt: String, items: Vec<PromptItem>, cursor: usize, checked: Vec<bool> }`
   - `pub fn run_select_one(prompt: &str, items: &[&str], default: usize) -> Result<Option<usize>>`
   - `pub fn run_multi_select(prompt: &str, items: &[&str], defaults: &[bool]) -> Result<Option<Vec<usize>>>`
2. **純粋関数として `render_frame` を切り出し**
   - 入力: state + ターミナル幅、出力: 描画対象の `Vec<String>` (各行は **そのまま stdout に書けば端末セル幅に収まる** ことを保証)
   - 折り返しは行わない (スクロールドリフトの原因を自ら再導入しない)。ターミナル幅を超えるラベルは末尾を `…` で丸める (visible width 基準)
   - key loop と描画を分離できるので snapshot テスト (`cargo test`) が書ける
3. **描画プロトコル**
   - 初回描画: `println!` で N 行出力、カーソルを最下行の左端に置く
   - 2 回目以降: `MoveUp(N)` + `Clear(FromCursorDown)` で前回描画領域をクリアしてから再描画
   - `height = items.len() + (prompt の行数)` を状態に持ち、実測に基づく (items 数以外の動的要素はない)
   - ヘルプ行 (`j/k toggle/enter`) を最下行に常設
4. **raw mode ガード**
   - `struct RawModeGuard;` `impl Drop for RawModeGuard` で `disable_raw_mode` を呼ぶ
   - panic 時の恐怖を避けるため `catch_unwind` は使わず、Drop だけに任せる (Rust はパニック伝搬中も Drop を走らせる)
5. **init.rs の差し替え**
   - `select_agents_interactive`: `support_level_colored` + `detection_status_colored` + `c_bold` を復活、`run_multi_select` に ANSI 込み label を渡す
   - `select_scope_interactive` / `ask_scope_fallback`: `run_select_one` に差し替え、scope 説明を `c_dim` で併記
6. **Cargo.toml から dialoguer を削除**、`cargo update -w` で Cargo.lock 再生成
7. **ADR-0019 追加** — 「init の対話プロンプトを自前実装にした理由」を Michael Nygard 形式で

## Concrete Steps

作業ディレクトリは `/Users/annenpolka/ghq/github.com/annenpolka/kizu`。

M1 Red:

    cargo test --lib prompt::tests::render_single_highlights_cursor
    # expected: test が存在しないのでコンパイルエラー → まず `src/prompt.rs` を
    # `pub mod tests { ... }` まで含む骨組みで追加。最初の 1 本のテストを書く

M1 Green/Refactor:

    cargo test --lib prompt::
    # expected: 対象テスト群が pass。フレーム出力の期待値は各テストに
    # inline snapshot で記述 (insta crate は使わず、文字列比較)

M2 (crossterm バインディング):

    cargo test --lib prompt::
    # expected: pure render 層は変更なしで pass。run_select_one / run_multi_select
    # 自体は TTY を必要とするので unit test しない (手動確認)

M3 差し替え後:

    cargo run -- init
    # expected: エージェント選択に ● / ◐ / ○ アイコンと色が表示される。
    # j/k で移動、space でトグル、Enter で確定、Esc でキャンセル。
    # キー入力で画面が上にドリフトしないこと。

M3 依存除去確認:

    cargo tree -p kizu | grep -E 'dialoguer|console|shell-words|zeroize'
    # expected: 何も出力されない (全部消える)

M4 CI:

    just ci
    # expected: fmt-check → clippy → cargo test → release build → e2e bun test
    # がすべて pass。合計 Rust tests は現行 302+ から prompt モジュールの
    # 追加分だけ増える想定。

## Validation and Acceptance

### 機能的受け入れ基準

1. `kizu init` 実行時、以下がすべて観察できる:
   - バナー (`傷 kizu init`) の下に、エージェントごとに 1 行: `[x/ ] <agent名>  <● 色付きサポートレベル>  <✓/~/✗ 検出状態>`
   - `j` / `k` / 矢印で選択カーソルが移動する
   - `space` で現在行のチェック状態がトグルする
   - `Enter` で選択を確定し次のステップ (scope 選択) に進む
   - `Esc` または `Ctrl-C` でキャンセルし、エラー終了 (exit code 1) する
2. 連続キー入力で表示が上下にドリフトしないこと (目視、10 回キー操作して banner が画面上端に追いやられないこと)
3. ターミナル幅 40 桁程度で起動しても、ラベルが折り返されず末尾 `…` で丸められること
4. `LANG=C.UTF-8` 以外の環境 (例: `LC_ALL=ja_JP.UTF-8`) でも CJK 文字が含まれるラベルが崩れないこと (現状ラベルに CJK はないが、将来の拡張に備えて unicode-width 基準で設計)

### 自動テスト受け入れ基準

- `cargo test --lib prompt::` で新規ユニットテスト (render_* / apply_key_*) が最低 8 本 pass
- `just ci` が fmt-check → clippy (-D warnings) → cargo test --all-targets → release build --locked → bun test e2e を全 pass

### 非互換の検証

- `cargo tree -p kizu` で `dialoguer` / `console` / `shell-words` / `zeroize` が出力に含まれないこと
- `git grep dialoguer -- 'src/**/*.rs'` が 0 ヒットであること

## Idempotence and Recovery

- `src/prompt.rs` の追加とテストは何度でも再実行可能 (副作用なし)。raw mode は RAII ガードで必ず戻す
- `kizu init` 自体は元々べき等 (既存の hook が見つかればスキップ)。本 ExecPlan は UI 層のみ変更するため、副作用セマンティクスは変化しない
- 途中で中断した場合: `git stash` → `cargo test` で既存テストが緑なことを確認してから復帰
- raw mode が万一戻らない事故時のリカバリー: `stty sane` (ユーザー側操作) を README 的なコメントに残しておく必要はないが、Drop 実装で確実に `disable_raw_mode()` を呼ぶことで予防する

## Artifacts and Notes

### 現状の証拠: dialoguer 0.12 にバグが残っていること

    $ diff ~/.cargo/registry/src/index.crates.io-*/dialoguer-0.11.0/src/theme/render.rs \
           ~/.cargo/registry/src/index.crates.io-*/dialoguer-0.12.0/src/theme/render.rs
    (差分なし)

    $ grep -n 'items.len\|size_vec' \
        ~/.cargo/registry/src/index.crates.io-*/dialoguer-0.12.0/src/prompts/multi_select.rs
    215:        let mut paging = Paging::new(term, self.items.len(), self.max_length);
    219:        let mut size_vec = Vec::new();
    227:            let size = &items.len();   # ← バイト長。ANSI で膨張する
    228:            size_vec.push(*size);
    345:                render.clear_preserve_prompt(&size_vec)?;

### 削除される依存 (cargo tree で確認)

    $ cargo tree -p kizu --edges normal | grep -E 'dialoguer|console|shell-words|zeroize'
    ├── dialoguer v0.12.0
    │   ├── console v0.16.3
    │   ├── shell-words v1.1.1
    │   └── zeroize v1.8.2

## Interfaces and Dependencies

### 追加するモジュール: `src/prompt.rs`

公開 API (最終形):

    pub struct PromptItem<'a> {
        pub label: &'a str,   // ANSI 込みで OK
    }

    pub fn run_select_one(
        prompt: &str,
        items: &[&str],      // ANSI 込みでよい
        default: usize,
    ) -> anyhow::Result<Option<usize>>;
    // Ok(Some(idx)) = 選択確定 / Ok(None) = キャンセル (Esc/Ctrl-C)

    pub fn run_multi_select(
        prompt: &str,
        items: &[&str],
        defaults: &[bool],
    ) -> anyhow::Result<Option<Vec<usize>>>;
    // Ok(Some(indices)) = 選択確定 / Ok(None) = キャンセル

テスト可能な pure helper:

    pub(crate) fn visible_width(s: &str) -> usize;  // ANSI ストリップ + unicode-width
    pub(crate) fn truncate_to_width(s: &str, max: usize) -> String;
    pub(crate) fn render_select_frame(state: &SelectState, term_width: usize) -> Vec<String>;
    pub(crate) fn render_multi_frame(state: &MultiSelectState, term_width: usize) -> Vec<String>;

### 使用ライブラリ

- `crossterm = "0.29.0"` (既存) — `terminal::{enable_raw_mode, disable_raw_mode}`, `event::{read, Event, KeyEvent, KeyCode, KeyModifiers}`, `cursor::{MoveUp, MoveToColumn}`, `terminal::{Clear, ClearType}`, `execute!` マクロ
- `unicode-width = "0.2"` (既存) — 文字列のセル幅計算
- `anyhow = "*"` (既存) — エラー型
- (削除) `dialoguer = "0.12"` — Cargo.toml から除去

### 削除する関数 / 型

- `dialoguer::{MultiSelect, Select, theme::ColorfulTheme}` の use を `src/init.rs` 3 箇所から削除

### 復活する関数 (8b0f9dd で削除されたもの)

- `fn c_cyan(s: &str) -> String` (プロンプト先頭の `?` の色付け用)
- `fn support_level_colored(sl: SupportLevel) -> String`
- `fn detection_status_colored(d: &DetectedAgent) -> String`
- `DetectedAgent::support_level` フィールド (もしくは `support_level(kind)` 関数経由で都度計算する現行方式を継続)

### ADR ドラフト (0019)

タイトル: `Custom interactive prompt for kizu init instead of dialoguer`
Status: Proposed
Context: dialoguer 0.12 has a byte-length based overflow detection bug (multi_select.rs:227) that causes display drift when items contain ANSI escape sequences. Upstream render.rs is unchanged between 0.11 and 0.12. Icons and support-level colors in `kizu init` selection screens (originally added in 08661ab, removed as workaround in 8b0f9dd) cannot be restored while depending on dialoguer.
Decision: Replace dialoguer calls in `src/init.rs` (3 sites) with a bespoke prompt module `src/prompt.rs` using crossterm (already a core dependency) and unicode-width (already present). Remove `dialoguer` from `Cargo.toml`.
Consequences:
- (+) Full control over rendering; ANSI icons / colors / multibyte glyphs all measured via unicode-width
- (+) Removes 4 transitive crates (dialoguer, console, shell-words, zeroize)
- (+) Aligns with kizu's existing crossterm-based TTY stack
- (−) ~200 LoC of custom code to maintain
- (−) No built-in fuzzy-select / password / autocomplete (none currently needed)
- (−) Non-TTY fallback must be implemented manually
