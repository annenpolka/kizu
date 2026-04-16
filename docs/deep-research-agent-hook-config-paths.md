# AI コーディングエージェント Hook 設定パスの真実 — project-local は誰がサポートしているのか

---

## 要約

**概要**: Claude Code のみが `settings.local.json`（gitignored な project-local 設定）を公式にサポートしている。Codex CLI・Cursor・Qwen Code・Cline はいずれも project-local と project-shared を区別する仕組みを持たない。

**主要ポイント**:
- Claude Code の `settings.local.json` は **業界で唯一の** gitignored project-local hook 設定機構
- Codex CLI は `.codex/hooks.json` を project と user の2階層で持つが、project 内に local/shared の区別はない
- Cursor は `.cursor/hooks.json`（project）と `~/.cursor/hooks.json`（user）の2階層。project 内に local 変種なし
- Qwen Code は `.qwen/settings.json` のみ。`settings.local.json` は**存在しない**
- Cline はファイルベースの hook（`.clinerules/hooks/`）で、local/shared の区別なし

**確実性評価**: 高 — 全エージェントの公式ドキュメントおよび GitHub リポジトリで確認済み

---

## 🎯 調査の目的 — kizu init の scope 設計を正しくするために

kizu の `init` コマンドは hook インストール時に `project-local`（gitignored）/ `project-shared`（committed）/ `user`（global）のスコープを選択させる。しかし Codex adversarial review で「Claude Code 以外のエージェントに project-local は意味がないのでは」という指摘が出た。

> **この調査は、各エージェントの hook 設定ファイルパスの正確な仕様を確認し、kizu init のスコープ設計を根拠ある形で修正するために行う。**

---

## 📊 エージェント別 Hook 設定パス一覧

| **エージェント** | **Project 設定** | **User 設定** | **Project-local (gitignored)** | **Hook の場所** |
|:---|:---|:---|:---:|:---|
| **Claude Code** | `.claude/settings.json` | `~/.claude/settings.json` | **✅ `.claude/settings.local.json`** | settings 内の `hooks` キー |
| **Codex CLI** | `.codex/config.toml` | `~/.codex/config.toml` | **❌** | `.codex/hooks.json` (別ファイル) |
| **Cursor** | `.cursor/hooks.json` | `~/.cursor/hooks.json` | **❌** | hooks.json 専用ファイル |
| **Qwen Code** | `.qwen/settings.json` | `~/.qwen/settings.json` | **❌** | settings 内の `hooks` キー |
| **Cline** | `.clinerules/hooks/<Event>` | `~/Documents/Cline/Rules/Hooks/` | **❌** | スクリプトファイル直接配置 |

---

## 🔍 各エージェントの詳細分析

### Claude Code — 唯一の project-local サポーター

Claude Code は **3 階層の設定ファイル** を持つ：

1. **`~/.claude/settings.json`** — ユーザーグローバル
2. **`.claude/settings.json`** — プロジェクト共有（committed）
3. **`.claude/settings.local.json`** — プロジェクトローカル（**自動的に gitignore される**）

> **Claude Code は `settings.local.json` 作成時に自動で git の ignore 設定を追加する。** これにより個人の hook 設定が誤ってコミットされることを防ぐ（[Claude Code Docs](https://code.claude.com/docs/en/settings)）。

優先順位: `settings.local.json` > `settings.json`（project）> `settings.json`（user）

**kizu への影響**: Claude Code に対してのみ `project-local` スコープが正当。`settings.local.json` に書いた hook は確実に gitignored される。

---

### Codex CLI — hooks.json は別ファイル、local 変種なし

Codex CLI の設定は **`config.toml`** ベースだが、hook は **`hooks.json`** という別ファイルで管理される（[OpenAI Developers](https://developers.openai.com/codex/hooks)）。

**Hook 設定の発見メカニズム**:
- `~/.codex/hooks.json` — ユーザーグローバル
- `<repo>/.codex/hooks.json` — プロジェクトスコープ

> **「Codex discovers `hooks.json` next to active config layers. If more than one `hooks.json` file exists, Codex loads all matching hooks. Higher-precedence config layers do not replace lower-precedence hooks.」**

**重要な点**:
- Hook は**累積的**に読み込まれる（上位が下位を上書きしない）
- **`hooks.json` に local 変種は存在しない**
- `.codex/hooks.json` はリポジトリ内に配置されるが、gitignored かどうかはユーザー次第
- 現在 hook 機能は **experimental** で `features.codex_hooks = true` で有効化が必要（[Config Reference](https://developers.openai.com/codex/config-reference)）
- **project-local と project-shared の区別がないため、`.codex/hooks.json` に書いた内容は commit 可能な状態**

**kizu への影響**: Codex の `project-local` インストールは `.codex/hooks.json` に書くが、これは gitignored ではない。ユーザーが自分で `.gitignore` に追加しない限り commit される。`project-shared` と実質的に同一。

---

### Cursor — hooks.json は project と user の 2 階層

Cursor は `hooks.json` 専用ファイルで hook を管理する（[Cursor Docs](https://cursor.com/docs/hooks)）。

**Hook 設定の場所**:
1. `~/.cursor/hooks.json` — ユーザーグローバル
2. `<project>/.cursor/hooks.json` — プロジェクトスコープ
3. Enterprise / Team レベル（組織向け）

> **「All matching hooks from every source run; conflicting responses are resolved by priority.」** — Enterprise → Team → Project → User の優先順位。

**重要な点**:
- **`.cursor/hooks.local.json` は存在しない**（[GitButler Deep Dive](https://blog.gitbutler.com/cursor-hooks-deep-dive) でも local 変種への言及なし）
- `.cursor/hooks.json` は「stored in version control alongside your code」と明記されている
- 個人用 hook は `~/.cursor/hooks.json`（user レベル）に書くのが正しいパス

**kizu への影響**: Cursor の `project-local` は `project-shared` と同一ファイルに書くことになり、区別が存在しない。個人用 hook は `user` スコープ（`~/.cursor/hooks.json`）に書くべきだが、Cursor は **`Scope::User` もサポートしない**（kizu 側で bail している）。

*⚠️ 注意: Cursor は `~/.cursor/hooks.json` で user-global hook をサポートしている。kizu の `Scope::User` 拒否は再検討の余地あり。*

---

### Qwen Code — Claude Code 互換を謳うが settings.local.json は非対応

Qwen Code は Claude Code と同形式の `settings.json` で hook を管理する（[QwenLM/qwen-code](https://github.com/QwenLM/qwen-code/blob/main/docs/users/configuration/settings.md)）。

**設定ファイルの場所**:
1. System defaults（OS 固有パス）
2. `~/.qwen/settings.json` — ユーザー設定
3. `.qwen/settings.json` — プロジェクト設定
4. System settings（管理者用上書き）

> **`settings.local.json` はドキュメントに一切言及がない。** Qwen Code は Claude Code の hooks プロトコル（JSON-over-stdin）を採用しているが、設定ファイルの階層は異なる。

**kizu への影響**: `.qwen/settings.local.json` に書いても **Qwen Code は読まない可能性が高い**。`project-local` スコープは Qwen Code では機能しない。

---

### Cline — ファイルベースの独自方式、local/shared の区別なし

Cline は JSON 設定ではなく、**スクリプトファイルを直接配置** する方式（[Cline Blog](https://cline.bot/blog/cline-v3-36-hooks)）。

**Hook の場所**:
- `<project>/.clinerules/hooks/<EventType>` — プロジェクトスコープ
- `~/Documents/Cline/Rules/Hooks/<EventType>` — グローバルスコープ

**重要な点**:
- ファイル名がイベント名（`PostToolUse`, `PreToolUse` 等）
- macOS / Linux のみサポート
- **local/shared の区別はない** — `.clinerules/hooks/` に置いたファイルは commit 可能

**kizu への影響**: Cline は `project-local` / `project-shared` の区別が構造的に不可能。スコープは無視して常にプロジェクトスコープに書く。

---

## 🏗️ kizu init への提言 — スコープ設計の修正案

### 現状の問題

現在の `scope_incompatible` は以下を拒否しているが、**拒否よりもスマートなフォールバック**が UX として望ましい：

| エージェント | `project-local` | `project-shared` | `user` |
|:---|:---:|:---:|:---:|
| Claude Code | ✅ | ✅ | ✅ |
| Codex CLI | ❌ (拒否) | ✅ | ✅ |
| Cursor | ❌ (拒否) | ✅ | ❌ (拒否) → **再検討** |
| Qwen Code | ❌ (拒否) | ✅ | ✅ |
| Cline | ❌ (拒否) | ✅ (常に project) | ❌ (拒否) |

### 提案: エラーではなくフォールバック + 警告

非互換なスコープでエラーにするのではなく、**自動フォールバック + 警告表示** に変更する：

```
  Claude Code   ✓ 1 entries added → .claude/settings.local.json
  Codex CLI     ⚠ project-local unavailable; falling back to user → ~/.codex/hooks.json
  Cursor        ⚠ project-local unavailable; falling back to user → ~/.cursor/hooks.json
```

具体的なフォールバック先：

| エージェント | `project-local` 指定時のフォールバック |
|:---|:---|
| Codex CLI | `user` (`~/.codex/hooks.json`) |
| Cursor | `user` (`~/.cursor/hooks.json`) |
| Qwen Code | `user` (`~/.qwen/settings.json`) |
| Cline | `project-shared` (スコープ概念なし) |

**理由**: `project-local` を選ぶユーザーの意図は「個人用、コミットしたくない」。その意図を尊重するなら、user-global へのフォールバックが最も近い。

### Cursor の `Scope::User` サポート再検討

Cursor は `~/.cursor/hooks.json` を公式にサポートしている。現在 kizu は `Scope::User` を拒否しているが、**これは誤り**。Cursor の user-global hook は正当なインストール先。

---

## 結論と展望

> **`settings.local.json`（gitignored project-local）は Claude Code の独自機能であり、業界標準ではない。** 他のエージェントが同等の機能を持たない以上、kizu の scope 設計は Claude Code 以外のエージェントに対してフォールバック戦略を持つべき。

**短期（即座）**:
- `scope_incompatible` のエラーをフォールバック + 警告に置き換え
- Cursor の `Scope::User` サポートを解禁

**中期（v0.3 以降）**:
- Codex CLI の hook 機能が experimental → stable になった際に設定パスの再調査
- Qwen Code が `settings.local.json` を採用する可能性を監視（Claude Code 互換を謳っている以上、追随の可能性あり）

**長期**:
- `settings.local.json` パターンが業界標準化するかどうかが、kizu のスコープ設計の根本的な方向を決める

---

## 参考情報源

- [Codex CLI Hooks Documentation](https://developers.openai.com/codex/hooks)
- [Codex Advanced Configuration](https://developers.openai.com/codex/config-advanced)
- [Codex Configuration Reference](https://developers.openai.com/codex/config-reference)
- [Cursor Hooks Documentation](https://cursor.com/docs/hooks)
- [Cursor Hooks Deep Dive — GitButler](https://blog.gitbutler.com/cursor-hooks-deep-dive)
- [Qwen Code Settings Documentation](https://github.com/QwenLM/qwen-code/blob/main/docs/users/configuration/settings.md)
- [Qwen Code Hooks Documentation](https://github.com/QwenLM/qwen-code/blob/main/docs/users/features/hooks.md)
- [Cline Hooks — v3.36 Release](https://cline.bot/blog/cline-v3-36-hooks)
- [Claude Code Settings Documentation](https://code.claude.com/docs/en/settings)
- [Cline Hooks System — DeepWiki](https://deepwiki.com/cline/cline/7.3-hooks-system)
