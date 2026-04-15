# Claude Code Hooks (reference)

_Mirrored from `Mechachang/wiki/Claude Codeフック.md` for in-repo access during implementation._


Claude Codeのフックシステムは、エージェントの行動の節目（PreToolUse / PostToolUse / Stop / SubagentStop / SessionStart 等）でユーザー定義のコマンドを実行できる仕組みである。`.claude/settings.json` に登録し、stdin経由でJSONを受け取り、stdout/stderrとexit codeでClaude Codeにフィードバックを返す。レビュー支援ツール（[[kizu]] 等）やリント自動化、コード品質ゲートを設計する上での骨格になる。

## PostToolUseフックの入力スキーマ

PostToolUseはツール実行の直後に発火する。stdinに以下のJSON構造が来る。

```json
{
  "session_id": "string",
  "transcript_path": "string",
  "cwd": "string",
  "hook_event_name": "PostToolUse",
  "tool_name": "Write",
  "tool_use_id": "string",
  "tool_input": {
    "file_path": "/absolute/path/to/file.rs",
    "content": "..."
  },
  "tool_response": "..."
}
```

`tool_name` は `Write` / `Edit` / `MultiEdit` / `Bash` / `Read` 等。Write/Editでは `tool_input.file_path` がフルパスで来るので、フック内で `jq -r '.tool_input.file_path'` する必要すらなく、環境変数ショートカット `$CLAUDE_TOOL_INPUT_FILE_PATH` で直接アクセスできる。同様に `$CLAUDE_TOOL_INPUT`（tool_input全体のJSON文字列）と `$CLAUDE_PROJECT_DIR`（プロジェクトルート）が用意されている。

## Claude Codeへのフィードバック経路

PostToolUseからClaude Codeに情報を返す経路は3つあり、レビューツール設計ではどれを使うかが決定的に重要になる。

**経路1: exit 0 + stdout — ユーザーにのみ表示。** stdoutの内容はCtrl+Oのtranscriptモードで人間には見えるが、Claude Codeには見えない。ログ用途に限られる。

**経路2: exit 2 + stderr — Claude Codeに直接フィードバック。** stderrの内容がClaude Codeの文脈に注入される。Claude Codeは次のアクションでこれを考慮する。ただしPostToolUseはツール実行後の発火なのでファイルは既に書き換えられており、取り消しはできない。フィードバックは「次の行動への指示」として機能する。

**経路3: JSON出力 + additionalContext — ツール結果に追記。** v2.1.9以降で利用可能。

```bash
cat <<EOF
{
  "hookSpecificOutput": {
    "hookEventName": "PostToolUse",
    "additionalContext": "レビュー注記: この変更でauth.rsのExpired判定が消えている。意図的か確認が必要。"
  }
}
EOF
exit 0
```

`additionalContext` の内容はツール結果に追記されてClaude Codeの文脈に入る。exit 2と違いエラーとしてではなく補足情報として注入される。リアルタイムフィードバックには経路3、強制的な再行動指示には経路2を使い分ける。

## Stopフックの動作と停止阻止

Claude Codeのターン終了時に発火するStopフックは、「ターン終了時にまとめてレビューを送る」「未対応タスクを検出して継続させる」というパターンに最適。入力JSONには `stop_hook_active` フラグがあり、前回のStopフックが既にexit 2を返したかどうかを示す。

```bash
#!/bin/bash
INPUT=$(cat)

if [ "$(echo "$INPUT" | jq -r '.stop_hook_active')" = "true" ]; then
  exit 0
fi

QUEUE="$CLAUDE_PROJECT_DIR/.claude/review-queue.md"
if [ -f "$QUEUE" ] && [ -s "$QUEUE" ]; then
  echo "未対応のレビューコメントがあります:" >&2
  cat "$QUEUE" >&2
  exit 2
fi

exit 0
```

`stop_hook_active` のチェックを忘れると、exit 2 → Claude Code継続 → 完了 → Stopフック → exit 2 → ……の無限ループに入る。これは設計上の最大の罠。

JSON decision制御という洗練された方法もあり、こちらは reason フィールドで停止阻止の理由を明示できる。

```bash
cat <<EOF
{
  "decision": "block",
  "reason": "未対応のレビューコメント: $(cat $QUEUE)"
}
EOF
```

`decision: "block"` はexit 2と同じ効果だが、reason がClaude Codeに渡る理由として明示される。

## 実行モデルと制約

**同期的に実行される。** async指定なしのPostToolUseフックは、完了するまでClaude Codeの次のツール実行をブロックする。重い処理（git status全走査、リポジトリ全体grep、HTTPコール等）はasync: trueで行うか、別プロセスに投げるべき。kizuの設計でPostToolUseが「単一ファイルgrep」に限定されたのはこの制約への回答。

**取り消しはできない。** PostToolUseはツール実行後の発火なので、フィードバックで「やめろ」と言ってもファイルは既に書き換わっている。取り消しが必要ならPreToolUseでexit 2を返すか、TUI側から直接ファイルをrevertする必要がある。kizuの `x` キー（hunk revert）はサンドボックス外からの介入で、フックを使わないこの経路に該当する。

**設定リロードはセッション開始時。** `.claude/settings.json` の変更はファイルウォッチャーで検出されるが、実行中のセッションに即座に反映されない場合がある。新しいフックを追加した場合はClaude Code再起動が推奨。

**matcherはツール名のみ。** PostToolUseの `matcher` フィールドはツール名でしかフィルタできない。ファイルパスでフィルタしたい場合はフック内部で `tool_input.file_path` を検査する。

```bash
FILE=$(jq -r '.tool_input.file_path' < /dev/stdin)
if echo "$FILE" | grep -qE '\.(test|spec)\.(ts|js)$'; then
  # テストファイルへの変更のみ処理
fi
```

## 設定例

```json
{
  "hooks": {
    "PostToolUse": [{
      "matcher": "Write|Edit|MultiEdit",
      "hooks": [{
        "type": "command",
        "command": "kizu hook-post-tool"
      }]
    }],
    "Stop": [{
      "hooks": [{
        "type": "command",
        "command": "kizu hook-stop"
      }]
    }]
  }
}
```

`hooks` 配列の各エントリは `matcher`（ツール名の正規表現）と `hooks` 配列を持ち、`hooks` 配列の各要素は `type: "command"` と `command` フィールドを持つ。複数のツールが同じmatcherを必要とする場合は配列に並べる。

## 設計パターン

[[inline scarパターン]] のように「TUI側がファイルに書き込み、Stopフックがgrepで拾って通知」とすれば、TUIとClaude Codeの間に専用IPCを用意せずファイルシステムだけでコミュニケーションが完結する。HTTPフックタイプを使えばWebUIと連携することもでき、claude-code-hooks-multi-agent-observabilityのようにVue + SQLite + WebSocketでイベントを可視化するパターンも存在する。

## 関連ページ

- [[kizu]] — フックを実装に組み込んだレビューツール
- [[inline scarパターン]] — ファイル直接書き込みによる非同期レビュー
- [[AIエージェント向けdiffビューア]] — フックを使うか否かでツールを分類
