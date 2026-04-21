# kizu — AIコーディングエージェントのリアルタイムdiff監視 + inline scarレビューツール

## 概要

kizuは、Claude Codeなどのターミナル型AIコーディングエージェントが行うファイル変更をリアルタイムに監視し、問題のある変更に対して即座にインラインコメント（scar）を刻んでエージェントにフィードバックするTUIツール。

### 解決する問題

Claude Codeのストリーミング出力を横目で見ている時に生じる3つの摩擦:

1. **ストリーミングが速くて「あれ？」の詳細を見逃す** — 変更内容が流れ去って確認できない
2. **Escして「何が問題だったか」を言語化するのがだるい** — 違和感はあるが言葉にできない
3. **言語化が曖昧だから解釈がズレる** — Claude Codeが違う箇所を直す

### 設計思想

kizuが解決するのは**「指差し」の精度**。変更を見逃さず捕捉し、キー1打で「これ」と指せれば、言語化も解釈ズレも消える。

- **リアルタイム監視**: Claude Codeの作業中に別ペインで常時稼働
- **非同期レビュー**: scarはファイルに直接書き込み、Claude Codeが自分のペースで拾う
- **最小介入**: キー1打でscar挿入。言語化不要の定型操作（ask/reject）+ 自由入力

### 既存ツールとの差別化

- **revdiff**: オンデマンド型（呼び出して→見て→閉じる）。kizuは常時監視型
- **diffpane**: リアルタイム監視だが観察専用。kizuは観察+介入
- **Claude Code内蔵diff**: ストリーミング出力に埋もれる。kizuはdiffだけを切り出してノイズ除去

---

## フェーズ

### v0.1 (MVP) — 監視ツール

fsnotify + git diffでリアルタイムdiff表示。フォローモード。ファイルリスト+diffビュー。scarなし、フックなし。純粋な監視ツール。

**これだけで「ストリーミングが速くて見逃す」問題は解決する。**

### v0.2 — scar + フック + イベントログ

scar機能追加（a/r/c/xキーバインド、言語判定、ファイル書き込み）。`kizu init`でフック登録。PostToolUse + Stopフック。PostToolUseフックで操作イベントを`/tmp/kizu-events/`にJSON蓄積（ストリームモードのデータ源）。

### v0.3 — 統合

`--attach`でターミナル自動分割（Ghostty AppleScript / tmux split-window）。Claude Codeプラグイン（`/kizu`スラッシュコマンド）。ストリームモード（イベントログベース）。設定ファイル（`~/.config/kizu/config.toml`）。

### v0.4 — seen hunk の折りたたみ

Spaceで既読化したhunkはDiffLineが折りたたまれ、hunk headerだけが残る。内容が変わると自動展開（fingerprint方式）。

### v0.5 — 行数表示モード

`#`キーでdiff viewとfile viewの左ガターに行番号を表示する。どちらも**worktree側（new）の行番号を1列表示**。Deleted行は現在のファイルに存在しないので空白。wrap時は継続行の行番号を空白にする。**Stream modeでは常時無効**（合成されたold_start/new_startは実ファイル行番号ではないため）。設定ファイル（`[line_numbers].enabled`、`[keys].line_numbers_toggle`）でデフォルト状態とキーをリマップ可能。

---

## 技術スタック

- **言語**: Rust
- **TUIフレームワーク**: ratatui
- **ファイル監視**: notify crate（fsnotifyのRust版）
- **diff計算**: git CLIラッパー（`git diff --no-renames <baseline> --`）
- **シンタックスハイライト**: syntect（bat/deltaと同じエンジン）
- **配布**: cargo install / brew

---

## アーキテクチャ

### データフロー — 2系統のデータソース

メインdiffビューとストリームモードは異なるデータソースを持つ。

```
[データソース1: ファイルシステム → 状態ビュー]
    │
    notify(fsnotify) ─→ [TUI メインdiffビュー] ─→ git diff再計算 ─→ 画面更新
                             │
                             ├─ a/r/c キー ─→ @kizu[*]: コメント書き込み ─→ [ソースファイル]
                             └─ x キー ─→ git checkout（hunk revert）─→ [ソースファイル]

[データソース2: PostToolUseイベントログ → 操作履歴ビュー]
    │
    PostToolUse hook ─→ JSON書き込み ─→ /tmp/kizu-events/<timestamp>.json
                                            │
                             notify(fsnotify) ─→ [TUI ストリームモード]

[Claude Codeへのフィードバック]
    │
    ├─ PostToolUse hook ─→ 書いたファイルに @kizu[*]: があれば additionalContext で通知
    └─ Stop hook ─→ tracked + untracked を合成して grep '@kizu\[' → 未対応ならexit 2
```

**メインdiffビュー（状態）**: 「今どうなっているか」を見る。hunk境界はgitが決める。

**ストリームモード（操作履歴）**: 「Claude Codeが何をしたか」を時系列で見る。境界はClaude Codeの個別Write/Edit操作。gitのhunkマージに汚されない、操作単位の差分。

### TUI層

- **変更検知**: notify crate でworktree + .git/HEAD + .git/refs を監視
- **デバウンス**: ファイル変更300ms / HEAD変更100ms
- **diff計算**: `git diff --no-renames <baseline_sha> --` をshell exec
- **ベースライン**: セッション開始時のHEAD SHA。`R`キー（Shift+r）でリセット

### フック層（v0.2）

#### PostToolUseフック — 単ファイル即時通知 + イベントログ

```json
{
  "hooks": {
    "PostToolUse": [{
      "matcher": "Write|Edit|MultiEdit",
      "hooks": [
        {
          "type": "command",
          "command": "kizu hook-post-tool"
        },
        {
          "type": "command",
          "command": "kizu hook-log-event",
          "async": true
        }
      ]
    }]
  }
}
```

`kizu hook-post-tool`サブコマンドの動作:
1. `$CLAUDE_TOOL_INPUT_FILE_PATH`を読み取り
2. そのファイルだけ`grep '@kizu\['`
3. 見つかれば`additionalContext`でClaude Codeに通知
4. 見つからなければexit 0（何もしない）

`kizu hook-log-event`サブコマンドの動作（async、非ブロッキング）:
1. stdinからPostToolUseのJSON（tool_name, tool_input, tool_response）を読み取り
2. `/tmp/kizu-events/<timestamp>-<tool_name>.json`に書き込み
3. kizu TUI側は`/tmp/kizu-events/`をnotifyで監視し、ストリームモードに反映

#### Stopフック — 全体最終チェック

```json
{
  "hooks": {
    "Stop": [{
      "hooks": [{
        "type": "command",
        "command": "kizu hook-stop"
      }]
    }]
  }
}
```

`kizu hook-stop`サブコマンドの動作:
1. `stop_hook_active`チェック（無限ループ防止）
2. tracked + untracked の両方を列挙し `grep '@kizu\['` で scan（`git diff --name-only` 単独は untracked を取りこぼすので `git status --porcelain --untracked-files=all` と合成する）
3. 未対応があればstderrに内容を出力 + exit 2（Claude Code継続）
4. なければexit 0（停止許可）

---

## TUIビュー

### メインビュー（デフォルト）

```
┌─ファイルリスト──────┬─diffビュー──────────────────────┐
│ M src/auth.rs +12/-3│ @@ -10,6 +10,9 @@              │
│ M src/handler.rs +2 │  fn verify_token(claims) {      │
│ A src/auth_test.rs  │+   if claims.exp < Utc::now() { │
│                     │+     return Err(Expired);        │
│                     │+   }                             │
│                     │    Ok(true)                      │
│                     │  }                               │
├─────────────────────┴─────────────────────────────────┤
│ [follow] auth.rs +12/-3 | session: +82/-15 3 files    │
└───────────────────────────────────────────────────────┘
```

- 左ペイン: 変更ファイルリスト（ステータス + パス + 追加/削除行数）
- 右ペイン: 選択ファイルのunified diff（シンタックスハイライト付き）
- フッター: フォローモード状態、セッション全体の統計

### ストリームモード（v0.3、キー切り替え）

データソースはPostToolUseイベントログ（`/tmp/kizu-events/`）。gitのhunkではなく、Claude Codeの個別Write/Edit操作が1行1エントリ。

```
14:03:22 Write  src/auth.rs        +12/-3   verify_token関数にExpired判定追加
14:03:25 Edit   src/main.rs         +1/-0   use文追加
14:03:30 Write  tests/auth_test.rs +28/-0   新規テストファイル
14:03:33 Edit   src/handler.rs      +2/-0   呼び出し側を更新
```

- 各行がClaude Codeの1回のWrite/Edit操作に対応
- 選択すると**その操作のdiffだけ**が表示される（gitのhunkマージに汚されない）
- ターミナルのスクロールバックで過去を遡れる
- scar操作（a/r/c）は操作単位のdiffに対して行える——gitのhunk境界問題を回避

### フォローモード

- デフォルトON
- 最新の変更ファイルに自動ジャンプし、最新の変更行にスクロール
- 手動でj/k/left/rightナビゲーションするとフォロー一時停止
- `f`キーでフォロー再開

---

## scar機能（v0.2）

### キーバインド

```
a       ask — `explain this change` を ask scar として挿入
r       reject — `revert this change` を reject scar として挿入
c       comment — ミニ入力欄→任意のコメントを free scar として挿入
x       revert — hunkをgit checkoutで元に戻す（scarなし）
e       editor — $EDITOR +<line> <file> で外部エディタを起動
space   既読マーク（v0.4: hunk本体を折りたたみ。内容変化で自動展開。TUI内部のみ、ファイルに何も書かない）
```

全キーバインドは`~/.config/kizu/config.toml`でリマップ可能。

### `@kizu[...]:` コメントフォーマット

変更行の直上に、言語のコメント構文で挿入。`@kizu[<kind>]:` の `<kind>` は `ask` / `reject` / `free` の 3 種で、hook 層がカテゴリ別に未対応 scar を分類できる:

```rust
// @kizu[ask]: explain this change
if claims.exp < Utc::now() {
```

```ruby
# @kizu[reject]: revert this change
validate :email, presence: true
```

```html
<!-- @kizu[free]: このclass削除するとレイアウト崩れない？ -->
<div class="container">
```

拡張子→コメント構文マッピング:

| 拡張子 | 構文 |
|:---|:---|
| .rs, .ts, .js, .java, .go, .c, .cpp, .swift | `// @kizu[<kind>]: ...` |
| .rb, .py, .sh, .yaml, .yml, .toml | `# @kizu[<kind>]: ...` |
| .html, .xml, .svg | `<!-- @kizu[<kind>]: ... -->` |
| .css, .scss | `/* @kizu[<kind>]: ... */` |
| .sql | `-- @kizu[<kind>]: ...` |
| .lua, .hs | `-- @kizu[<kind>]: ...` |
| その他/不明 | `# @kizu[<kind>]: ...`（フォールバック） |

### revert操作（xキー）

選択中のhunkに対して`git checkout -p`相当の操作でファイルを元に戻す。scarは挿入しない。Claude Codeは次のReadで変更された状態を見る。

**hunk境界の注意**: メインdiffビューのhunk境界はgitが決めるため、Claude Codeの個別操作と一致しない場合がある（隣接する複数操作がgitにマージされる）。revertはgitのhunk単位で動作する。操作単位の精密なrevertが必要な場合は、ストリームモードから操作を選んでscar（`c`キーで「この変更だけ戻して」と記入）で対応する方が安全。

---

## CLI

```bash
# リアルタイム監視TUI起動
kizu

# ターミナル自動分割で起動（v0.3）
kizu --attach

# プロジェクト初期化（フック登録）（v0.2）
kizu init

# フック削除（v0.2）
kizu teardown

# PostToolUseフック用サブコマンド（v0.2、Claude Codeから呼ばれる）
kizu hook-post-tool

# PostToolUseイベントログ記録（v0.2、Claude Codeから非同期で呼ばれる）
kizu hook-log-event

# Stopフック用サブコマンド（v0.2、Claude Codeから呼ばれる）
kizu hook-stop
```

---

## 対応ターミナル（v0.3 --attach）

| ターミナル | 分割方法 | 検知 |
|:---|:---|:---|
| tmux | `split-window -h` | `$TMUX` |
| Ghostty | AppleScript split | `$TERM_PROGRAM=ghostty` |
| zellij | `zellij run --floating` | `$ZELLIJ` |
| kitty | `kitty @ launch --type=window` | `$KITTY_LISTEN_ON` |

優先順: tmux → zellij → kitty → ghostty

---

## 関連リソース

- diffpane（参考実装、Go/bubbletea）: https://github.com/Astro-Han/diffpane
- revdiff（既存のレビューツール、Go）: https://github.com/umputun/revdiff
- ratatui: https://ratatui.rs/
- notify crate: https://docs.rs/notify/
- syntect: https://docs.rs/syntect/
- Claude Code Hooks公式: https://code.claude.com/docs/en/hooks
- Ghostty AppleScript: https://ghostty.org/docs/features/applescript
