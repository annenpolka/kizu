# kizu

[![Crates.io](https://img.shields.io/crates/v/kizu.svg)](https://crates.io/crates/kizu)
[![License: MIT](https://img.shields.io/crates/l/kizu.svg)](LICENSE)
[![CI](https://github.com/annenpolka/kizu/actions/workflows/ci.yml/badge.svg)](https://github.com/annenpolka/kizu/actions/workflows/ci.yml)

[English](README.md) | [日本語](README.ja.md)

> AIコーディングエージェント（Claude Code / Cursor / Codex / Qwen Code / Cline / Gemini）と並走するリアルタイム diff 監視 + インライン scar レビュー TUI。

![kizu demo](docs/media/demo.gif)

## 何をするツールか

ターミナル型の AI コーディングエージェント（Claude Code、Cursor など）がペインの片方でファイルを書き換えている間、kizu は反対のペインに常駐して「今何が変わったか」をリアルタイムに映し出します。おかしいと思ったらキー 1 打で、変更箇所のソースに `@kizu[ask|reject|free]:` というコメント（**scar**）が刻まれます。エージェントは次の `PostToolUse` でそれを読み、読み損ねても `Stop` フックがターンの終了をブロックするので、人間が長文で説明しなくても直しに戻ります。

kizu は、エージェントの出力を横目で眺めているときに発生する **3 つの摩擦**を出発点に設計されています:

1. **ディテールを見逃す。** ストリーミングは速く、「あれ？」と思った瞬間にはもう流れ去っている。
2. **違和感を言語化するのが面倒。** 何かがおかしいのは分かるのに、言葉にするのに時間がかかる。
3. **言葉にしてもズレて直される。** 曖昧な人間の文は、曖昧なエージェントの解釈になる。

kizu の答えは **「指差し」の精度**です。変更を漏れなく捕捉し、人間はキー 1 打で「これ」と指せればいい——そうすれば言語化の問題そのものが消えます。名前の _kizu_（傷）は日本語の「傷」「scar」そのもの。怪しい変更のひとつひとつがソースに小さな痕を残し、それが癒えるまでエージェントは先へ進めません。

## インストール

### crates.io から

```bash
cargo install kizu
```

### ソースから

```bash
git clone https://github.com/annenpolka/kizu
cd kizu
cargo install --path .
```

### 必要要件

- Rust 1.94+（edition 2024）
- `PATH` 上に `git` CLI
- macOS / Linux。Windows は未検証。`--attach` の Ghostty サポートは macOS のみ（AppleScript）、tmux / zellij / kitty の分割は macOS と Linux の両方で動作します。

## クイックスタート

まずはフックも scar も使わず、純粋な「working tree に追従する diff ペイン」として kizu を立ち上げるところから始めます。

```bash
cd path/to/your/repo
kizu
```

別ペインでエージェントにファイルを触らせると、変更のたびに kizu が描画し直します。`q` で終了。

これだけで「ストリームが速くて見逃す」という最初の摩擦が消えます。それが手に馴染んできたら、[AI エージェント連携](#ai-エージェント連携) を読んで、キー 1 打で**反応できる** scar ワークフローを有効化してください。

## 使い方

### 3 つのビュー

kizu は 1 つの TUI に 3 つのモードを持ちます。`Tab` でメイン diff ↔ ストリームを切り替え、`Enter` でカーソル位置のファイルにズーム。いつでも `?` でキー一覧のヘルプオーバーレイが開きます。以下で言う **セッションベースライン** は、kizu を起動した瞬間（または最後に `R` を押した瞬間）の `HEAD` SHA のことです。つまり「何が変わったか」は常に「レビューを始めてから何が変わったか」を指します。

| キー | ビュー | 表示されるもの |
|------|--------|----------------|
| _（既定）_ | **メイン diff** | `git diff <session-baseline>` に基づくファイル別の hunk。追加行は暗い緑、削除行は暗い赤の背景。hunk 境界は git が決めます。 |
| `Tab` | **ストリームモード** | エージェントが実際に行った操作を時系列で並べたビュー。1 エントリ = 1 回のファイル編集ツール呼び出し（Claude Code / Qwen は `Write` / `Edit` / `MultiEdit`、Cursor は `afterFileEdit`）。git ではなく `hook-log-event` が書き出す JSON ログが源泉。 |
| `Enter` | **ファイルビュー** | カーソル下のファイル全体。hunk 内の追加行はインラインでハイライト。hunk コンテキストだけでは足りないときに使います。`Enter` / `Esc` で閉じる。 |

### scar

**scar** は、対象ファイルの言語のコメント構文で kizu が直接書き込むインラインコメントです。種類タグ（`ask` / `reject` / `free`）と 1 行の本文を持ちます:

```rust
// @kizu[ask]: explain this change
```

```python
# @kizu[reject]: revert this change
```

```html
<!-- @kizu[free]: elaborate on the edge case here -->
```

JSX/TSX では、kizu が構文上安全な形を文脈で選びます。TypeScript / JavaScript の statement では従来通り `//`、JSX children や fragment では JSX block comment を使います:

```tsx
{/* @kizu[ask]: explain this change */}
<p>Count: {count}</p>
```

3 種類、それぞれがキー 1 打:

- **`ask`**（`a`）— 質問。定型文 `explain this change` を挿入。プロンプトも入力もなし。エージェントはインラインで答えてから作業を続けます。
- **`reject`**（`r`）— 拒否。定型文 `revert this change` を挿入。エージェントは編集を巻き戻します。
- **`free`**（`c`）— 自由記述。入力フィールドが開き、本文を自分でタイプします。何でも書ける。

`a` と `r` がタイピング不要なのは意図的です——指差しの要点は「文を組み立てなくていい」こと。ニュアンスが必要なら `c` を使ってください。scar は冪等です。同じ行で `a` を二度押しても no-op。`u` でセッション中の直前の scar を 1 つだけ取り消せます。

エージェントへのフィードバック経路は 2 本:

- **PostToolUse フック** は各編集の直後に発火します。編集されたファイルに scar があれば、フックが `additionalContext` としてエージェントに見せるため、次の 1 手で必ず scar を認識します。
- **Stop フック** はエージェントがターンを終えようとしたときに発火します。未解決の scar がリポジトリ内に残っていれば非ゼロで終了し、エージェントは作業を続けざるを得なくなります。

### そのほかの機能

- **`--attach`** — エージェントのペインから `kizu --attach` を実行すると、ターミナル（tmux / zellij / kitty / Ghostty）を自動分割して新しいペインで kizu を起動します。検知順は `$TMUX` → `$ZELLIJ` → `$KITTY_LISTEN_ON` → `$TERM_PROGRAM=ghostty`。設定の `[attach].terminal` で明示指定も可能。
- **既読化 / 折りたたみ（`Space`）** — カーソル上の hunk を「既読」とし、本体を折りたたんでヘッダーだけにします。後からその hunk の内容が変わると、fingerprint を比較して自動で展開し直すので、フォローアップの編集を見逃しません。ファイルには何も書かない、TUI 内部だけの状態。
- **検索（`/` `n` `N`）** — 現在のビューに対する smart-case 検索。本文中のマッチはハイライト表示され、`n` / `N` でラップアラウンドしながらジャンプ、ポジションインジケータも出ます。
- **行番号（`#`）** — メインビューとファイルビューで worktree 側の行番号ガターを表示するトグル。ストリームモードは操作単位の合成 diff なので実ファイルの行番号に対応せず、常に非表示。既定値とキーは設定で変更可能（`[line_numbers].enabled`、`[keys].line_numbers_toggle`）。
- **scar undo（`u`）** — セッションローカルなスタックで、直前の scar 書き込みだけを巻き戻します。テキストエディタの undo と同じ感覚で使えます。
- **ベースラインリセット（`R`）** — diff のベースラインを現在の `HEAD` に張り直します。セッション途中で commit したあと、レビュー済みの差分を kizu から忘れさせたいときに便利。
- **フォロー（`f`）** — 最新の変更に自動スクロールするか、カーソルをその場に留めるかのトグル。
- **ヘルプオーバーレイ（`?`）** — 2 カラムのキー一覧を開きます。`?` / `Esc` / `q` で閉じる。フッター自体はレスポンシブで、端末が狭いときはステータスだけに縮退するため、キーの正本はこのヘルプオーバーレイです。

## キーバインド

kizu 内で `?` を押すとライブのキー一覧が開きます。以下の表はその写しです。

### scar
| キー | 動作 |
|------|------|
| `a` | カーソル行の直上に `@kizu[ask]:` scar を挿入 |
| `r` | `@kizu[reject]:` scar を挿入 |
| `c` | コメント入力を開き、自由記述の `@kizu[free]:` scar を挿入 |
| `x` | 現在の hunk を revert（`git checkout -- <file>` 相当を hunk スコープで） |
| `e` | 現在の hunk を `$EDITOR` で開く |
| `Space` | 現在の hunk を既読化し本体を折りたたむ。内容が変われば自動で再展開 |
| `u` | 直前の scar 挿入を undo |

### ナビゲーション
| キー | 動作 |
|------|------|
| `j` / `↓` | 次の行（適応型：未変更行の連続はスキップ） |
| `k` / `↑` | 前の行 |
| `J` | 1 行ずつ下へ（きめ細かく） |
| `K` | 1 行ずつ上へ |
| `Ctrl-d` | 半ページ下 |
| `Ctrl-u` | 半ページ上 |
| `g` | diff の先頭 |
| `G` | diff の末尾 |
| `h` | 前のファイル |
| `l` | 次のファイル |
| `s` | ファイルピッカーを開く |

### 検索
| キー | 動作 |
|------|------|
| `/` | 検索入力（smart-case） |
| `n` | 次のマッチ（ラップ） |
| `N` | 前のマッチ |

### ビュー
| キー | 動作 |
|------|------|
| `Tab` | メイン diff ↔ ストリームモード |
| `Enter` | カーソル下のファイルビューを開く |
| `Esc` | ファイルビューを閉じる / 入力をキャンセル |
| `w` | 行折り返しトグル |
| `z` | カーソル位置スタイルの切替 |
| `f` | フォロー（自動スクロール）トグル |
| `#` | 行番号ガタートグル（メイン + ファイルビュー） |
| `?` | ヘルプオーバーレイを開く |

### セッション
| キー | 動作 |
|------|------|
| `R` | diff ベースラインを現在の `HEAD` にリセット |
| `q` / `Ctrl-C` | 終了 |

## 設定

kizu は `~/.config/kizu/config.toml` を読み込みます（`$KIZU_CONFIG` で上書き可能）。全フィールドがオプショナルで、部分的な TOML を書けば残りは既定値とマージされます。

```toml
# ~/.config/kizu/config.toml — すべて既定値

[keys]
ask                 = "a"
reject              = "r"
comment             = "c"
revert              = "x"
editor              = "e"
seen                = " "
follow              = "f"
search              = "/"
search_next         = "n"
search_prev         = "N"
picker              = "s"
reset_baseline      = "R"
cursor_placement    = "z"
wrap_toggle         = "w"
undo                = "u"
line_numbers_toggle = "#"

[colors]
bg_added   = [10, 50, 10]    # 暗い緑（delta スタイル）
bg_deleted = [60, 10, 10]    # 暗い赤

[timing]
debounce_worktree_ms = 300    # worktree のファイル変更
debounce_git_dir_ms  = 100    # HEAD / refs / packed-refs

[editor]
command = ""                  # 空文字列なら $EDITOR を使う

[attach]
terminal = ""                 # 空文字列なら自動検出。"tmux" | "zellij" | "kitty" | "ghostty"

[line_numbers]
enabled = false               # 起動時はガター非表示。`#` で実行時トグル
```

非文字キー（`Enter`、`Tab`、矢印、`Ctrl-*`）はリマップできません。

## AI エージェント連携

kizu はエージェントのフック機構への登録をコマンド 1 発で済ませます:

```bash
kizu init
```

対話フローがインストール済みエージェントを検知し、どのスコープ（`project-local` / `project-shared` / `user`）に入れるかを尋ね、正しい設定ファイルに書き込みます。非対話モードも用意しています:

```bash
kizu init --agent claude-code --scope project-local --non-interactive
```

`kizu teardown` は全エージェント・全スコープから kizu が入れたものをまとめて撤去します。

### 対応エージェント

エージェントのホストが提供するフック面積はばらばらなので、`kizu init` が実際に入れるフックセットも変わります。「Full」は PostToolUse 相当と Stop 相当の両方がある、つまり**ターン途中で scar が見え、かつ未解決 scar があるときに Stop がブロックできる**状態を意味します。

| エージェント識別子 | サポート水準 | `kizu init` が入れるもの |
|--------------------|--------------|----------------------------|
| `claude-code` | Full | PostToolUse（scar 通知 + 非同期イベントログ）+ Stop ゲート、`session_id` バインド付き |
| `cursor` | Full | `.cursor/hooks.json` の `afterFileEdit`（scar 通知 + イベントログ）+ `stop` ゲート、`session_id` バインド付き |
| `qwen` | Full | PostToolUse（scar 通知 + イベントログ）+ Stop ゲート |
| `codex` | Stop のみ | Stop ゲートのみ。Codex の PreTool / PostTool は Bash ツールにしか発火しないため、ファイル編集側にフックする余地がない |
| `cline` | PostToolUse のみ（best-effort） | ファイルベースの `.clinerules/hooks/PostToolUse`。Stop ゲートがないため、未解決 scar でタスク完了を**止められない** |
| `gemini` | 書き込み側のみ | ホスト側インストールなし。Gemini CLI が現状フック機構を公開していないため、diff ペインと scar 書き込みだけが使える。将来的にはパイプベースのストリーム統合を計画中 |

どのエージェントを選んでも、`kizu init` はリポジトリ全体の git `pre-commit` shim を 1 つだけ追加で入れます（下記参照）。

各エージェントの調査は [`docs/deep-research-ai-agent-hooks.md`](docs/deep-research-ai-agent-hooks.md)、Claude Code のフックスキーマと無限ループ対策は [`docs/claude-code-hooks.md`](docs/claude-code-hooks.md) を参照してください。

### フックの役割

`kizu init` は 3 つの関心事をまとめて設置します。最初の 2 つが実際に入るかはエージェントのフック面積に依存（上の表）。3 番目は git フックで、リポジトリごとに 1 度だけ入ります。

- **PostToolUse**（対応エージェントのみ）→ `kizu hook-post-tool`（ファイル単位の scar 通知）+ `kizu hook-log-event`（ストリームモードを駆動する非同期イベントログ）
- **Stop**（対応エージェントのみ）→ `kizu hook-stop` が tracked + untracked を走査し、未解決の `@kizu[...]` があればエージェントを終了させない。Stop フックのないエージェント（Cline、Gemini）では scar 解決は best-effort で、最後の安全網は git `pre-commit` だけになります。
- **git `pre-commit`**（リポジトリ全体、1 度だけ）→ `kizu hook-pre-commit` が staged ファイルに scar が残っているときに `git commit` をブロックし、未レビューのまま commit に逃げるのを防ぎます。shim は kizu 管理下にあり、既存の `.git/hooks/pre-commit` があれば `pre-commit.user` にリネームして新 shim からチェーンします。

## スタック

- Rust 2024 edition
- TUI は [ratatui](https://ratatui.rs/) + [crossterm](https://docs.rs/crossterm/)
- ファイル監視は [notify](https://docs.rs/notify/) + [notify-debouncer-full](https://docs.rs/notify-debouncer-full/)
- diff 計算は `git` CLI の shell out（`git diff --no-renames <baseline> --`）— [ADR-0001](docs/adr/0001-git-cli-shell-out.md) 参照
- シンタックスハイライトは一般言語を syntect、JS/TS/JSX/TSX を tree-sitter の文書単位 highlight で処理 — [ADR-0020](docs/adr/0020-tree-sitter-for-jsx-tsx.md) 参照

## 開発

ローカルでの作業は [`just`](https://github.com/casey/just) が一括ランナー。レシピは [`justfile`](justfile) を参照してください。

```bash
just            # 既定: CI ゲート全部（fmt-check → clippy → test → release → e2e）
just rust       # 高速ループ: fmt + clippy + cargo test（e2e をスキップ）
just e2e        # release build + tuistory e2e（bun test）
just run        # 現在の worktree に対して cargo run --release
```

生の cargo コマンドでも問題ありません:

```bash
cargo build --release
cargo test --all-targets
cargo clippy -- -D warnings
```

アーキテクチャ、設計判断、正準仕様:

- [`docs/SPEC.md`](docs/SPEC.md) — 全体仕様。アーキテクチャ、TUI / フック層スキーマ
- [`docs/adr/`](docs/adr/) — 不可逆な設計判断の Architecture Decision Record（git CLI shell-out、notify-debouncer-full、tuistory e2e、ストリームモード、…）
- [`docs/inline-scar-pattern.md`](docs/inline-scar-pattern.md) — ファイル書き込み + Stop フックによる非同期レビューパターン（kizu のコアメカニズム）
- [`docs/related-tools.md`](docs/related-tools.md) — diffpane / diffwatch / revdiff / watchexec+delta / hwatch / Claude Code Hooks パイプラインの比較調査

Issue や PR を歓迎します。

## ライセンス

MIT。[`LICENSE`](LICENSE) を参照してください。
