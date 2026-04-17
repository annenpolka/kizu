# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

**kizu** は AI コーディングエージェント (Claude Code, Cursor, Codex, Qwen Code, Cline, Gemini 等) と並走させるリアルタイム diff 監視 + inline scar review TUI。Rust 製の単一バイナリ。

現状は **v0.3** (feat/v0.3 ブランチで PR レビュー中 → main へのマージ待ち)。v0.2 までの scar + hook 層に加えて、ストリームモード (Tab 切替の操作履歴ビュー、`hook-log-event` が書き出す JSON を `git diff` snapshot の差分で per-operation diff 化)、`--attach` ターミナル自動分割 (tmux / zellij / kitty / Ghostty)、`~/.config/kizu/config.toml` (キーバインド・色・デバウンス・エディタ・分割先)、scar undo stack (`u`)、適応的 `j`/`k` ナビゲーション、Claude Code プラグイン (`plugin/`) を実装済み。

## 実装前に必ず読むもの

`docs/SPEC.md` がこのプロジェクトの canonical specification。**実装に着手する前に、対象機能が属するフェーズ (v0.1 / v0.2 / v0.3) を SPEC.md で確認すること。** v0.1 と v0.2 ではデータソースもキー操作も異なる。

設計コンテキストは `docs/` 配下に集約されている:

- `docs/SPEC.md` — 全体仕様、フェーズ分割、TUI/hook 層スキーマ
- `docs/claude-code-hooks.md` — PostToolUse/Stop hook の入出力と落とし穴 (`stop_hook_active` 無限ループ等)
- `docs/inline-scar-pattern.md` — scar (`@kizu[ask|reject|free]:` インラインコメント) を非同期ファイル書き込みで実現する設計理由
- `docs/related-tools.md` — diffpane など類似ツールのサーベイ
- `docs/deep-research-ai-agent-hooks.md` — 10 AI コーディングエージェントの hook 機構調査 (v0.2 統合地図)
- `docs/adr/` — Architecture Decision Records。採用した設計判断の「なぜ」を記録する不可逆な履歴

## ドキュメントの役割分担

判断に迷ったら: **実装の how は ExecPlan、製品の what は SPEC、設計の why は ADR**。

- **`docs/SPEC.md`** — 「何を作るか」の正準仕様。機能要件、フェーズ分割、TUI/hook 層のスキーマ。
- **`docs/adr/`** — 「なぜこの設計を選んだか」の不可逆な判断を Michael Nygard 形式で記録。ライブラリ選定やレイヤ分割などの重い判断を残す。詳細は `docs/adr/README.md`。
- **`plans/`** — 「どう実装するか」の ExecPlan (PLANS.md 準拠のリビングドキュメント)。進捗・発見・判断を作業中に継続更新する。小さな判断は ExecPlan の Decision Log に残し、後から効く重い判断は ADR に昇格させる。

## ビルド・検証

ローカル作業の一括ランナーとして **`just`** を採用している。`justfile` 参照。

```bash
just                # default = `just ci` — 全 CI gate を順に実行
just ci             # fmt-check → clippy → cargo test → release → e2e
just rust           # 高速ループ: fmt + lint + cargo test (e2e をスキップ)
just test           # cargo test --all-targets のみ
just e2e            # release build + tuistory e2e (bun test)
just run            # cargo run --release で kizu を起動
just clean          # cargo clean + tests/e2e/node_modules 削除
```

`just --list` でレシピ一覧を確認できる。生 cargo コマンドを直接叩いてもよい (以下は等価):

```bash
cargo build                       # debug
cargo check                       # 型チェック
cargo clippy -- -D warnings       # lint (warning は error 扱い)
cargo build --release             # release (LTO thin, strip)
```

リリースプロファイルは `lto = "thin"`, `codegen-units = 1`, `strip = true` でバイナリサイズを最適化している。`edition = "2024"` を使用 (Rust 1.94+ 必須)。

## 開発スタイル: t-wada 流 TDD

新規ロジックは **t-wada 流の TDD (Red → Green → Refactor)** で進める。先にテストを書かずに本体コードを書き始めない。

- **Red**: まず失敗するテストを 1 つ書く。コンパイルエラーも Red とみなす。
- **Green**: テストを通す**最小限**のコードを書く。仮実装 (fake it)・ベタ書きで構わない。
- **Refactor**: テストが緑のまま重複と表現を整える。リファクタ中に新機能を足さない。
- **小さく刻む**: 1 サイクルは数分。テストは 1 つの振る舞いだけを検証する。
- **三角測量**: 1 例だけで一般化しない。値を変えた 2 例目で実装を一般化する。
- **明白な実装は OK**: 仮実装を経ずに書ける確信があれば直接書いてよい。ただし常にテスト先行。
- **テストの命名**: `何を_どう振る舞うか` を日本語/英語どちらでも明示する (例: `compute_diff_returns_empty_when_no_changes`)。
- **失敗の確認**: Red の段階で**期待通りのメッセージで落ちる**ことを必ず確認してから Green に進む。

`git.rs` の diff parsing のような純粋関数から TDD で起こす。watcher / TUI など I/O が絡む層はテスト境界を切り出してから着手する。

## ビルド検証フロー (CI と一致)

CI (`.github/workflows/ci.yml`) と同じ順序を `just ci` 一発でローカル再現できる:

```bash
just ci   # fmt-check → clippy → cargo test → release → e2e
```

展開すると以下と等価:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo build --release --locked

# tuistory + bun による e2e (v0.1 から required check)
cd tests/e2e && bun install --frozen-lockfile && KIZU_BIN=../../target/release/kizu bun test
```

`tests/e2e/` は TypeScript + bun:test で書かれた black-box e2e テスト群。kizu バイナリを実 pty で起動し、キー操作・`waitForText`・inline snapshot で検証する。代表シナリオは smoke / navigation / reactive / reset / colors / scar (a/c/x/e) / init (interactive + non-interactive + teardown)。ratatui の basic ANSI 色は tuistory の `foreground` フィルタでマッチしないので、色検証は Rust 単体テストに寄せ、e2e ではテキストレイアウトを pin する。詳細は ADR-0004。

## コミット / PR 規約

- コミット / PR を作る前に **`just ci` が通ること** (= CI と同順の fmt-check → clippy → cargo test → release → e2e)
- コミットメッセージは `prefix: subject` 形式 (例: `init: kizu v0.1 skeleton + design context`)
- フッターに `Co-Authored-By: Claude ...` を維持する (このリポジトリは Claude Code との共同作業を前提とした設計のため)

## 実装上のメモ

- **diff の取得は git CLI を shell out して行う** (ライブラリは使わない)。`git diff --no-renames <baseline_sha> --` を基本とし、untracked は `git status --porcelain` から合成する。`--no-renames` を含む高度なフラグを再実装せずに使えることが理由 → [ADR-0001](docs/adr/0001-git-cli-shell-out.md)
- watcher のデバウンスは worktree = 300ms / `<git_dir>/HEAD` = 100ms (SPEC.md 記載値)。`notify-debouncer-full` を採用し、macOS では `PollWatcher` fallback + common git-dir root 常設 watch で `HEAD` / `refs` / `packed-refs` の意味論を担保しつつ、worktree 監視でも `compare_contents` を有効にして same-size rewrite を取りこぼさない。health は `WatchSource` 単位で追跡し、partial recovery で warning を消さない → [ADR-0002](docs/adr/0002-notify-debouncer-full.md), [ADR-0010](docs/adr/0010-macos-poll-fallback-and-targeted-git-watch-roots.md), [ADR-0011](docs/adr/0011-common-git-root-watch-for-late-packed-refs.md), [ADR-0012](docs/adr/0012-worktree-content-compare-on-macos-poll.md), [ADR-0013](docs/adr/0013-source-aware-watcher-health.md)
- 非同期ランタイムは tokio の `current_thread` flavor。`crossterm::event::EventStream` と watcher の `tokio::sync::mpsc::UnboundedReceiver` を `tokio::select!` で統合 → [ADR-0003](docs/adr/0003-tokio-async-runtime.md)
- e2e テストは tuistory + bun (`tests/e2e/`)。Rust 単体テストは `cargo test --all-targets`、e2e は `bun test` → [ADR-0004](docs/adr/0004-tuistory-e2e.md)
- watcher → app 境界は **drain ベースの coalescing** で背圧を取る。kizu 側の `.gitignore` フィルタは持たず、`git diff` 自体の `.gitignore` 尊重と coalescing で吸収する → [ADR-0005](docs/adr/0005-watcher-coalescing-no-ignore-filter.md)
- git ディレクトリは `git rev-parse --absolute-git-dir` で解決する (`<root>/.git` をハードコードしない)。linked worktree 対応のため → [ADR-0005](docs/adr/0005-watcher-coalescing-no-ignore-filter.md)
- session baseline は起動時の HEAD SHA。`R` (Shift+r) でリセット可能 (v0.1 仕様)。初回コミットがないリポでは empty tree SHA (`4b825dc642cb6eb9a060e54bf8d69288fbee4904`) を baseline にフォールバックする
- `git diff` 失敗時は `App.last_error: Option<String>` に記録してフッタ右端に赤の `×` 表示。`files` は前回成功時の状態を保持し、次の watcher イベントで自動リトライする (panic させない)
- バイナリファイルは `DiffContent::Binary` バリアントとしてリストに `bin` 表示、右ペインは `[binary file - diff suppressed]` プレースホルダー

## v0.2 実装上のメモ

- **scar** (`@kizu[ask|reject|free]:` inline comment) は `src/scar.rs` で言語別コメント構文を判定し、`insert_scar(path, line, kind, body)` でファイルに直接書き込む。べき等 (同一 scar が前行にあれば no-op)
- **hook 層** (`src/hook.rs`): `kizu hook-post-tool` (PostToolUse 用) と `kizu hook-stop` (Stop 用) サブコマンド。stdin JSON を `NormalizedHookInput` に正規化し、scar grep → JSON 出力。`scan_scars` は行頭コメント構文 (`//`, `#`, `--`, `/*`, `<!--`) に続く `@kizu[` のみマッチし、テスト文字列内の false positive を排除
- **session baseline** (`src/session.rs`, `src/paths.rs`): kizu TUI 起動時に XDG state dir (`~/Library/Application Support/kizu/sessions/` on macOS) にセッションファイル (baseline SHA + PID) を書き出す。Stop hook はこれを読んで baseline 以降の変更ファイルだけをスキャン。TUI 終了時に削除
- **init/teardown** (`src/init.rs`): 6 エージェント対応 (Claude Code / Cursor / Codex / Qwen / Cline / Gemini)。scope は `project-local` (settings.local.json, gitignored) / `project-shared` / `user` の 3 種。Claude Code の hook は `matcher` + `hooks` 配列のネスト構造。`kizu init` は git pre-commit hook も設置 (scar が staged にあれば commit をブロック)
- **diff view の背景色** は delta 風 (`BG_ADDED = Rgb(10,50,10)`, `BG_DELETED = Rgb(60,10,10)`)。`+`/`-` prefix は廃止。ADR-0014
- **file view** (Enter): worktree ファイル全体を表示、hunk 内の Added 行に BG_ADDED を適用。j/k = chunk scroll, J/K = 1 行移動
- **検索** (`/`): smart case (全小文字 → case-insensitive)、`n`/`N` で match 間ジャンプ (wrap-around)
- **入力フィールド** (scar comment `c`, search `/`): フッター上の独立行に描画、折り返し対応、unicode-width でカーソル位置計算 (CJK 対応)、bracketed paste で IME 入力対応

## v0.3 実装上のメモ

- **hook-log-event** (`src/hook.rs`): `SanitizedEvent` に stdin JSON を sanitize (tool_input.content / tool_response.output を削除) して `<state_dir>/events/<timestamp>-<tool>.json` に atomic write。dir 0700 / file 0600。`prune_event_log` で TTL 24h + 上限 1000 エントリを自動削除。`KIZU_EVENT_TTL_SECS` で TTL 上書き可能
- **設定ファイル** (`src/config.rs`): `~/.config/kizu/config.toml` (TOML 形式) でキーバインドリマップ、diff 背景色、デバウンスタイミング、エディタコマンド、ターミナル分割設定を変更可能。`#[serde(default)]` で部分 TOML を既定値にマージ。`$KIZU_CONFIG` で設定ファイルパスを上書き可能
- **ストリームモード** (`src/app.rs` ViewMode::Stream): PostToolUse hook が書き出すイベントログを時系列で一覧表示する TUI ビュー。Tab キーでメイン diff ビューと切り替え。各イベントの per-operation diff は TUI がリアルタイムに `git diff` snapshot の差分計算で生成。TUI 起動前のイベントは metadata のみ表示。`WatchEvent::EventLog` で events dir を監視
- **`--attach`** (`src/attach.rs`): ターミナル自動検出 ($TMUX → $ZELLIJ → $KITTY_LISTEN_ON → $TERM_PROGRAM=ghostty) + 分割コマンド実行。config の `[attach].terminal` で強制指定可能。Ghostty は macOS のみ (AppleScript)
- **Claude Code プラグイン** (`plugin/`): `plugin.json` で PostToolUse (hook-post-tool + hook-log-event async) / Stop (hook-stop) hook を宣言、`/kizu` スラッシュコマンドでセッション状態確認

## ADR の運用

後から変えると痛い設計判断をした際は、**コード変更と同じ PR で** `docs/adr/NNNN-kebab-case-title.md` を追加する。テンプレートは `docs/adr/template.md`、運用ルールは `docs/adr/README.md`。

- 新規 ADR は `Proposed` → PR マージ時に `Accepted`
- 覆す場合は旧 ADR を `Superseded by ADR-NNNN` にして新 ADR を追加する。**旧 ADR の本文は書き換えない** (履歴として残す)
- 命名、小さなリファクタ、一時的な実装都合は ExecPlan の Decision Log で十分 — ADR に昇格させない
