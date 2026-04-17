# kizu Claude Code Plugin

kizu の PostToolUse / Stop hook と `/kizu` スラッシュコマンドを Claude Code プラグインとして提供します。

## インストール

```bash
claude plugin add /path/to/kizu/plugin
```

**前提条件**: `kizu` バイナリが PATH 上にあること。

```bash
cargo install --path /path/to/kizu
# or
brew install kizu  # (将来対応予定)
```

## 提供する機能

### Hook (自動)

- **PostToolUse**: Write/Edit/MultiEdit 操作後に `kizu hook-post-tool` で scar スキャン + `kizu hook-log-event` でストリームモード用イベントログ記録
- **Stop**: ターン終了時に `kizu hook-stop` で未解決 scar チェック

### コマンド

- `/kizu` — セッション状態の確認（baseline SHA、pending scar 数、ストリームイベント数）

## `kizu init` との違い

- `kizu init` は対話的にエージェントを選んで settings.json に hook を書き込む
- このプラグインはインストールするだけで Claude Code の hook が自動有効化される
- 両方を使う場合、hook は 2 回呼ばれるが scar scan は冪等なので実害なし
