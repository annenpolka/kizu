# 10 の AI コーディングエージェントにフックを刺す ― kizu v0.2 のための統合地図

---

## 要約

**概要**: Claude Code 以外の 9 ツールを対象に PostToolUse/Stop 相当のフック機構を調査した結果、「刺せる」ツールは **Cursor / Codex CLI / Qwen Code / Cline / Windsurf** の 5 種類、「tail で拾える」ツールは **Gemini CLI** の 1 種類、「fsnotify まかせ」しかないツールは **Aider / Continue.dev / Roo Code / Zed** の 4 種類という 3 階層に整理できる。

**主要ポイント**:

- Cursor 1.7 以降が最も豊富な hook 面を持ち、`afterFileEdit` / `stop` / `preToolUse` / `postToolUse` / `beforeShellExecution` などが **production ステータス** で提供されている
- Codex CLI は Claude Code とほぼ同形の hooks.json を持つが、`PreToolUse` / `PostToolUse` の matcher が **現時点では Bash ツールのみ** に対応しており、file edit イベントには直接刺さらない
- Qwen Code と Cline はそれぞれ `.qwen/settings.json` と `.clinerules/hooks/` を持ち、Claude Code と互換に近い JSON-over-stdin プロトコルを採用している
- Gemini CLI は hook 機構を持たないが `--output-format stream-json` で tool_use/tool_result の JSONL イベントストリームを吐くため、外部 tail が現実的な唯一の統合点
- Aider / Continue.dev / Roo Code / Zed の 4 つは fsnotify + scar（ファイル書き込み）以外の統合点を持たない。いずれ hook を実装する可能性はあるが、2026 年 4 月時点ではスコープ外に置くべき

**確実性評価**: **高**（Codex / Cursor / Qwen Code / Cline / Windsurf はそれぞれ公式ドキュメント一次情報で確認済み。Aider / Continue.dev / Roo Code / Zed は「hook が無い」こと自体を確認するため、存在しないものに対する negative evidence として issue tracker と公式リファレンスを交差させた）

---

## 「PostToolUse を刺す」という目標 ― kizu の意図

### 中心目的

kizu は AI コーディングエージェントのリアルタイム diff を「指差し」する TUI であり、v0.2 で導入する scar（`@review:` インラインコメント）は **非同期でエージェントに戻せて初めて価値を持つ**。scar をファイルに書いただけでは、エージェントが次の Read をするまで拾われないからだ。

そのためには 2 系統のフィードバックパスが必要:

1. **即時通知系（PostToolUse 相当）**: エージェントが Write/Edit した直後、そのファイルに scar があれば `additionalContext` / `systemMessage` などの形で注入する
2. **最終チェック系（Stop 相当）**: エージェントがターンを閉じようとした瞬間、未対応 scar が残っていれば stop を阻止して「まだ残ってるよ」と戻す

このレポートは、上記 2 系統を 10 ツールそれぞれでどこまで実現できるかの実装地図を提供する。

---

## 中心アクター ― 10 ツールの hook 面プロフィール

### 各ツールの hook 面サマリ

| **エージェント** | **PreTool 相当** | **PostTool 相当** | **Stop 相当** | **event log** | **設定ファイル** | **ステータス** |
|:---|:---:|:---:|:---:|:---:|:---|:---:|
| **Claude Code** (基準) | ✅ | ✅ | ✅ | stdin JSON | `.claude/settings.json` | stable |
| **Cursor 1.7+** | ✅ | ✅ (+ `afterFileEdit`) | ✅ | stdin JSON + env | `.cursor/hooks.json` | **production** |
| **OpenAI Codex CLI** | ⚠️ Bash only | ⚠️ Bash only | ✅ | stdin JSON | `~/.codex/hooks.json` | **experimental** |
| **Qwen Code** | ✅ | ✅ | ✅ | stdin JSON | `.qwen/settings.json` | 正式 (default on) |
| **Cline v3.36+** | ✅ | ✅ | ⚠️ TaskStart のみ | スクリプトファイル | `.clinerules/hooks/` | 正式 (macOS/Linux) |
| **Windsurf Cascade** | ✅ | ✅ (on model response) | ✅ | stdin JSON | 3-tier JSON | stable |
| **Gemini CLI** | ❌ | ❌ | ❌ | JSONL ストリーム | `~/.gemini/settings.json` (MCP のみ) | stream-json 安定 |
| **Aider** | ❌ | ⚠️ lint-cmd/test-cmd のみ | ❌ | ❌ | `.aider.conf.yml` | 機能として stable |
| **Continue.dev** | ❌ | ❌ | ❌ | ❌ | `~/.continue/config.ts` | hook 未実装 |
| **Roo Code** | ❌ | ❌ | ❌ | ❌ | `.roo/` (MCP + rules) | **enhancement 申請中 #11504, #12025** |
| **Zed** | ❌ | ❌ | ❌ | ❌ | MCP + tasks.json のみ | hook 未実装 |

### 実装状況の核心部分

> **「現実的にフックを刺せるのは Claude Code を含めて 6 つ。残り 4 つは fsnotify + scar でしか攻めようがない」**

これが kizu v0.2 の設計を規定する最重要事実である。6 ツール向けに `kizu init --agent <name>` を分岐させ、残り 4 ツールは v0.1 のリアルタイム diff 監視の延長で押し切る。scar は「書き込み + エージェントの次 Read」というゆるい結合で機能するため、フックの無いツールでも完全に壊れはしない。

---

## 直接要因 ― 各ツールの hook 仕様の詳細

### Claude Code（リファレンス実装）

kizu v0.2 が基準とするモデル。`.claude/settings.json` に JSON-over-stdin、exit code 2 でブロック、`hookSpecificOutput.additionalContext` でエージェントに追加コンテキストを戻す方式。詳細は既存の `docs/claude-code-hooks.md` にまとまっている。

### Cursor 1.7+ ― 最も豊富な hook 面

- **`afterFileEdit`** を含む Agent hooks 17 種類 + Tab hooks 2 種類を提供
- 設定は `.cursor/hooks.json` に `{ "version": 1, "hooks": { "afterFileEdit": [...] } }` 形式
- **全て production ステータス**（2025 年 10 月 1.7 リリースで beta 脱却）
- stdin 入力 / stdout 出力 / exit code 2 で deny という Claude Code と同形のプロトコル
- 特異点: `postToolUse` の `additional_context` 出力が **MCP ツールの結果書き換え**を許可する（他ツールには無い機能）
- Codex 互換のため `CLAUDE_PROJECT_DIR` 環境変数も exposed される（クロスエージェント互換を意識している）

kizu v0.2 で `afterFileEdit` に結線する:

```json
{
  "version": 1,
  "hooks": {
    "afterFileEdit": [
      { "command": "kizu hook-post-tool", "timeout": 10 }
    ],
    "stop": [
      { "command": "kizu hook-stop", "timeout": 10 }
    ]
  }
}
```

### OpenAI Codex CLI ― 形は同じだが file edit に刺さらない

- `~/.codex/hooks.json` または `<repo>/.codex/hooks.json` に Claude Code と非常に似た構造で書く
- サポートイベントは **5 種類**: `SessionStart` / `PreToolUse` / `PostToolUse` / `UserPromptSubmit` / `Stop`
- 致命的制約: `PreToolUse` / `PostToolUse` の matcher が **現時点では Bash ツールのみ対応**。Write/Edit 系ツールには刺さらない
- ステータスは **experimental**、Windows サポートは一時無効化
- 回避策: `Stop` hook で diff を grep する Claude Code 流の最終チェック方式に寄せる。file edit の即時通知は Codex 側の matcher 拡張を待つか、fsmonitor で代替する

### Qwen Code ― Claude Code hook 仕様のクローン

- `.qwen/settings.json` に配置、構造は Claude Code にほぼそっくり
- `PreToolUse` / `PostToolUse` / `SessionStart` / `SessionEnd` / `UserPromptSubmit` / `Stop` / `StopFailure` / `PostCompact` を提供
- stdin JSON、exit code 2 でブロック、`disableAllHooks` で一括無効化可
- **hooks はデフォルト有効**（Codex と違って即使える）
- kizu v0.2 では Claude Code 向けの `kizu hook-post-tool` をそのまま流用可能

### Cline v3.36+ ― ファイル配置型のユニークな設計

- スクリプトを `.clinerules/hooks/<EventType>` ファイル名で直接配置する（設定ファイルではなくファイル名自体が event binding）
- 配置先: `~/Documents/Cline/Rules/Hooks/`（global）または `.clinerules/hooks/`（project）
- サポートイベント: `PreToolUse` / `PostToolUse` / `UserPromptSubmit` / `TaskStart`
- **Stop 相当はなく、`TaskStart` の反対方向のみ**。最終チェック系統は機能しない
- **macOS / Linux のみ**、Windows 非対応
- kizu v0.2 からは「`PostToolUse` 用スクリプトを 1 本書き出す installer」という形になる

### Windsurf Cascade ― 3-tier JSON 構成

- JSON 設定ファイルを user / workspace / project の 3 tier で merge
- pre-hook は exit code 2 でブロック可能
- `on model response` hook でモデル応答を tee でき、audit logging 用途で使われる
- ただし公式ドキュメントで file edit 特化の hook タイプ名が明示されていないため、実装時に Cascade Hooks の最新リファレンスで matcher 構文を再確認する必要がある

### Gemini CLI ― hook は無いが JSONL stream がある

- **hook 機構は存在しない**
- 代わりに `gemini --output-format stream-json` で JSONL イベントストリームを標準出力に吐く
- stream には `init` / `message` / `tool_use` / `tool_result` / `assistant` / `result` イベントが含まれる
- kizu v0.2 の戦略: `kizu consume-gemini-stream` のような pipe consumer を用意し、ユーザーが `gemini ... --output-format stream-json | kizu consume-gemini-stream` で起動する統合フロー
- この場合、scar は Gemini へ直接フィードバックできない（JSONL は読み出し専用）。次ターン以降の Read 時に拾ってもらう間接統合が限界
- 余談: Gemini CLI の設定ファイルは `~/.gemini/settings.json`（JSON）で、MCP 定義のみを載せる場所として存在する

### Aider ― lint-cmd / test-cmd という遠回りの hook

- 純粋な hook API は無いが、`--lint-cmd <cmd>` と `--test-cmd <cmd> --auto-test` で **毎 edit 後に任意コマンドを起動できる**
- kizu v0.2 の戦略: `--lint-cmd "kizu hook-post-tool"` としてねじ込む。Aider の lint-cmd は edit 後に自動実行されるため、PostToolUse 相当の穴になる
- ただし Aider は exit code で「エラー」と判断すると自動で再プロンプトするので、scar が見つかった時に exit 2 を返しても Claude Code のような `additionalContext` 注入ではなく、エラーメッセージとして扱われる
- コミット前の hook という扱いのため Stop 相当は無い
- 設定ファイルは `.aider.conf.yml`（YAML）

### Continue.dev ― 静的 config のみ

- `~/.continue/config.ts` の `modifyConfig` で静的カスタマイズはできるが **lifecycle event hook は存在しない**
- `slashCommands` は deprecated、新しい prompt files は invocation time のみで edit 時には発火しない
- kizu v0.2 ではフック統合対象から除外

### Roo Code ― enhancement 申請中

- **hook は未実装**。現行の issue tracker に `[ENHANCEMENT] Allow prompt based hook` #11504, `[ENHANCEMENT] Run hook command on events requiring prompts` #12025 として申請が並んでいる
- 現状の統合点は MCP と rules（`.roo/rules/*.md`）だけで、どちらも発火型ではなく参照型
- kizu v0.2 ではフック統合対象から除外、v0.3 以降で再評価

### Zed ― MCP と tasks.json のみ

- hook API は無し
- Agent Panel は built-in tools + MCP servers で拡張する設計
- `tasks.json` は手動タスクランナー（VS Code の tasks.json と同じ発想）で、エージェント lifecycle とは結線しない
- kizu v0.2 ではフック統合対象から除外

---

## 背景条件 ― なぜ 2026 年 4 月現在でこのバラつきがあるのか

### 「hook」概念の業界全体での認知の遅れ

hook という概念自体が AI コーディングエージェント界隈で一般化したのは **2024 年後半の Claude Code 先行実装がきっかけ**で、Cursor が 1.7 で追随したのは 2025 年 10 月、Codex CLI は現在も experimental。つまり業界の hook 標準化は **「Claude Code を模倣する」フェーズ**にある。

この文脈が示唆するのは:

- **kizu v0.2 の hook installer を Claude Code 形式で書けば、将来 Codex や Roo Code が追随したときに最小コストで拡張できる**
- 一方で「最大公約数 hook」を先に実装して仕様を凍結すると、後発ツールの固有機能（例: Cursor の `afterFileEdit` の edits 配列）を取り逃す

### 2 つの統合モデルの並立

業界は現状「ファイル系 hook」と「stream 系 event log」という 2 モデルが並立している:

**ファイル系 hook**:
- Claude Code, Cursor, Codex CLI, Qwen Code, Cline, Windsurf
- stdin JSON-over-command の形
- 同期的ブロックが可能（exit code 2）
- kizu が即時 feedback を返せる

**stream 系 event log**:
- Gemini CLI のみ（stream-json）
- JSONL を tail する consumer モデル
- ブロックは不可能、観察のみ
- kizu は次ターン Read での拾い上げに期待する間接フィードバック

kizu v0.2 は両モデルを並行サポートする必要がある。

---

## 構造的要因 ― kizu v0.2 で採用すべき 3 階層戦略

### Tier A: first-class hook 統合（5 ツール）

**対象**: Cursor / Codex CLI / Qwen Code / Cline / Windsurf

**実装方針**:

- `kizu init --agent <name>` でエージェント別の hook 設定を書き出す
- 共通の `kizu hook-post-tool` / `kizu hook-stop` サブコマンドを stdin JSON で動かす
- JSON スキーマは **Claude Code 形式を基準に正規化**し、各ツール固有のフィールド差分を shim 層で吸収する

**エージェント別のインストール先**:

| **エージェント** | **設定ファイル** | **event name** |
|:---|:---|:---:|
| Cursor | `.cursor/hooks.json` | `afterFileEdit` + `stop` |
| Codex CLI | `~/.codex/hooks.json` | `Stop`（PreTool/PostTool は Bash only 待ち） |
| Qwen Code | `.qwen/settings.json` | `PostToolUse` + `Stop` |
| Cline | `.clinerules/hooks/PostToolUse` | ファイル配置 |
| Windsurf | `.windsurf/hooks.json` | Cascade Hooks 最新リファレンス参照 |

### Tier B: stream tail 統合（1 ツール）

**対象**: Gemini CLI

**実装方針**:

- `kizu consume-gemini-stream` サブコマンドを提供
- ユーザーは `gemini --output-format stream-json -p "..." | kizu consume-gemini-stream` で起動
- 内部では JSONL をパースして `tool_use` イベントから file path を抽出、scar の拾い上げは kizu の通常 fsnotify と同じパスで処理
- 逆方向（kizu → Gemini）のフィードバックは不可能。scar 書き込みのみ

### Tier C: fsnotify + scar のみ（4 ツール）

**対象**: Aider / Continue.dev / Roo Code / Zed

**実装方針**:

- 専用 hook installer は提供しない
- ユーザーは v0.1 と同じく `kizu` を別ペインで起動して scar だけを書き込む
- エージェントが次 Read で拾うことを期待する完全非同期統合
- **Aider だけは例外**として、`--lint-cmd "kizu hook-post-tool"` 用のドキュメントを用意する（hook としては不完全だが近似可能）

### ユニバーサル fallback: git fsmonitor

どのツールからも漏れる場合の最終手段として **git fsmonitor hook** が存在する。`.git/hooks/fsmonitor-watchman` は git 操作に連動するため、AI エージェントが git status を呼んだ瞬間にだけ kizu に通知できる。ただし発火頻度が低く、即時性は期待できないため、kizu v0.2 のメインパスには据えず、v0.3 以降の bonus 機能として扱うのが妥当。

---

## 長期的文脈 ― hook 標準化はどこへ向かうか

### 2024 → 2026 の進化の軌跡

- **2024 Q4**: Claude Code が hooks.json を導入、業界標準の雛形を作る
- **2025 Q3**: Cursor 1.7 が `.cursor/hooks.json` を beta → production へ、Claude Code との互換性を意識して `CLAUDE_PROJECT_DIR` 環境変数まで exposed
- **2025 Q4**: Codex CLI が experimental hooks を発表、ただし Bash matcher 限定
- **2026 Q1**: Qwen Code と Cline が hooks を正式導入、JSON-over-stdin プロトコルが事実上の業界標準に
- **2026 Q2 予測**: Roo Code が hook を追加する可能性（issue #11504 が prioritized）、Continue.dev と Zed は MCP-first 戦略を維持

### システム的要因: MCP vs Hook の役割分担

Zed と Continue.dev が hook を持たない背景には **MCP 優先の設計思想**がある。MCP はエージェントに「新しい tool を足す」ための protocol であり、「既存 tool の前後に割り込む」hook とは異なる責務を持つ。

kizu の scar フィードバックは本質的に **割り込みセマンティクス**なので、MCP では不十分で hook が必要。この構造が変わらない限り、Zed と Continue.dev への完全統合は hook 実装待ちになる。

> **「kizu の価値は『既存ツールに割り込む』ことにあり、それを認めない哲学のエージェントとは本質的に相性が悪い」**

---

## システム分析 ― kizu の hook API 設計原理

### 正規化レイヤ（Claude Code 形式を中心に）

kizu v0.2 は内部で以下のデータ構造に正規化する:

```
NormalizedEvent {
    session_id: String,
    event_kind: EventKind,  // PreTool | PostTool | Stop | ...
    tool_name: Option<String>,
    file_paths: Vec<PathBuf>,  // tool_input から抽出
    cwd: PathBuf,
    agent: AgentKind,  // ClaudeCode | Cursor | Codex | Qwen | Cline | Windsurf | GeminiStream
}
```

各エージェント固有の入力 JSON は `From<ClaudeCodeHookInput> for NormalizedEvent` のような形で shim される。これにより `kizu hook-post-tool` 本体は 1 つのコードパスで全エージェントを扱える。

### 出力形式の吸収

逆方向（kizu → エージェント）は output 形式が微妙に違う:

| **エージェント** | **ブロック方法** | **追加コンテキスト方法** |
|:---|:---|:---|
| Claude Code | exit 2 + stderr | `hookSpecificOutput.additionalContext` JSON |
| Cursor | exit 2 or `permission: "deny"` | `additional_context` JSON |
| Codex CLI | exit 2 + stderr | `additionalContext` JSON |
| Qwen Code | exit 2 | Claude Code 形式と同じ |
| Cline | exit 2 | Claude Code 形式に準拠 |
| Windsurf | exit 2 | `systemMessage` JSON |

`kizu hook-post-tool` は `--agent <name>` フラグで出力形式を切り替える。agent 検出は設定ファイル経由で optional に。

### べき等性の要求

v0.1 の diff 監視は純粋な観察なのでべき等性はあまり問題にならないが、v0.2 の `kizu hook-post-tool` は `additionalContext` を返すたびに scar を含むファイルが再 Read される可能性がある。**同じ scar を複数回通知しない**ための hash-based dedupe が必要。設計は以下:

- `/tmp/kizu-scars/<session_id>.json` に通知済み scar ID を蓄積
- PostToolUse で scar を検出するたびにハッシュを計算し、未通知分だけ返す
- セッション終了時 (`Stop`) に該当ファイルを削除

---

## 結論と展望

kizu v0.2 の hook 層は **「Claude Code 互換の正規化 + 5 エージェント向け installer + 1 stream consumer」** という 3 層構造で実装するのが最も現実的。fsnotify + scar のバックボーンは既に v0.1 で完成しているため、hook 無しのツール（Aider/Continue.dev/Roo Code/Zed）でもツール価値は保たれる。

### 今後の見通し

- **短期（1-2 ヶ月）**: Cursor + Codex CLI + Qwen Code の 3 本を先行して `kizu init --agent` に実装し、ドッグフードする。Cline と Windsurf は Tier A 候補だが固有仕様調査コストが高いため次ラウンドに回す
- **中期（3-6 ヶ月）**: Gemini CLI の stream consumer を追加、Aider を `--lint-cmd` 統合で近似サポート
- **長期（6 ヶ月以上）**: Roo Code の hook 実装状況を監視、実装され次第 Tier A へ昇格。Continue.dev と Zed は MCP 側での統合可能性を別途調査（kizu MCP server 化）

### kizu v0.2 ExecPlan への反映事項

- **「v0.2 を Claude Code 専用と決めつけない」**。`kizu init --agent claude-code` を default にしつつ、内部データ構造は最初から正規化する
- Codex PreTool/PostTool が Bash 限定である事実を Surprises & Discoveries に予め記録する
- Cline の `.clinerules/hooks/` はファイル配置型なので installer ロジックが他と違うことを Decision Log に残す
- Windsurf Cascade Hooks の file edit 相当 event 名は実装直前に再確認する（ドキュメントで明示されていなかったため）

---

## 参考情報源

**OpenAI Codex CLI**:
- [Hooks – Codex | OpenAI Developers](https://developers.openai.com/codex/hooks)
- [Advanced Configuration – Codex | OpenAI Developers](https://developers.openai.com/codex/config-advanced)
- [Hook would be a great feature · openai/codex · Discussion #2150](https://github.com/openai/codex/discussions/2150)

**Cursor**:
- [Hooks | Cursor Docs](https://cursor.com/docs/hooks)
- [Cursor 1.7 Adds Hooks for Agent Lifecycle Control - InfoQ](https://www.infoq.com/news/2025/10/cursor-hooks/)
- [Automate Cursor Code Quality with afterFileEdit | egghead.io](https://egghead.io/automate-cursor-code-quality-with-after-file-edit~nczsu)

**Qwen Code**:
- [Qwen Code Hooks Documentation](https://qwenlm.github.io/qwen-code-docs/en/users/features/hooks/)
- [qwen-code/docs/users/features/hooks.md · GitHub](https://github.com/QwenLM/qwen-code/blob/main/docs/users/features/hooks.md)

**Cline**:
- [Cline v3.36: Hooks - Inject Custom Logic Into Cline's Workflow](https://cline.ghost.io/cline-v3-36-hooks/)
- [Hooks - Inject Custom Logic Into Cline's Workflow](https://cline.bot/blog/cline-v3-36-hooks)

**Windsurf**:
- [Cascade Hooks | Windsurf Docs](https://docs.windsurf.com/windsurf/cascade/hooks)
- [Windsurf Editor Changelog](https://windsurf.com/changelog)

**Gemini CLI**:
- [Headless mode reference | Gemini CLI](https://geminicli.com/docs/cli/headless/)
- [Add stream-json output format · Issue #8203](https://github.com/google-gemini/gemini-cli/issues/8203)
- [Gemini CLI configuration](https://geminicli.com/docs/reference/configuration/)

**Aider**:
- [Linting and testing | aider](https://aider.chat/docs/usage/lint-test.html)
- [YAML config file | aider](https://aider.chat/docs/config/aider_conf.html)

**Continue.dev**:
- [How to Configure Continue | Continue Docs](https://docs.continue.dev/customize/deep-dives/configuration)
- [config.json Reference - Continue Docs](https://docs.continue.dev/reference/config)

**Roo Code**:
- [RooCodeInc/Roo-Code · GitHub](https://github.com/RooCodeInc/Roo-Code)
- [[ENHANCEMENT] Allow prompt based hook · Issue #11504](https://github.com/RooCodeInc/Roo-Code/issues/11504)
- [[ENHANCEMENT] Run hook command on events requiring prompts · Issue #12025](https://github.com/RooCodeInc/Roo-Code/issues/12025)

**Zed**:
- [Agent Panel | AI Coding Agent - Zed](https://zed.dev/docs/ai/agent-panel)
- [Tools | AI Agent Tools - Zed](https://zed.dev/docs/ai/tools)
- [Tasks | Zed Code Editor Documentation](https://zed.dev/docs/tasks)
