# AI agent diff viewer survey (reference)

_Mirrored from `Mechachang/wiki/AIエージェント向けdiffビューア.md`._


AIコーディングエージェントがファイルを高速に書き換える時代に、その変更をリアルタイムにレビューするツールカテゴリが2025年後半から2026年にかけて急速に立ち上がっている。lazygit issue #5337 で「AIエージェントが高速に複数ファイルを変更する際、手動でj/kナビゲーションでは目的を果たせない」と指摘されたのが象徴的で、これは単なるUI便利機能ではなくエージェントへのオブザーバビリティの問題である。auto-acceptで動かす信頼を構築するには、エージェントが今何をしているかを把握できる経路が要る。

このページはClaude Code等の隣で動かせるリアルタイムdiffビューア群のサーベイをまとめる。実装方針の選択肢は4系統あり、それぞれ得意領域が異なる。kizuの設計判断（[[kizu]]）はここでの比較が前提になっている。

## 専用TUIツール

**diffpane（Astro-Han/diffpane）** はGo製でgitをsingle source of truthとする設計が明快。セッション開始時のHEAD SHAをベースラインとして記録し、`r`キーでリセット。フォローモードは最新の変更ファイルに自動ジャンプして最新の変更箇所にスクロールし、手動ナビゲーションすると一時停止、`f`で再開する。シンタックスハイライトは追加行・削除行を背景色で色分けし、ダーク/ライトターミナルにOSC 11クエリで自動対応。Bubble Tea + lipgloss + chromaのスタックで、テスト込み約2600行・本体1200-1500行。fsnotifyでworktreeと `.git/HEAD` / `.git/refs` を監視し、デバウンスはファイル変更300ms / HEAD変更100ms。`git check-ignore` をイベントごとにforkするのがTODOコメントにもあるパフォーマンスのボトルネック候補。kizuのv0.1はdiffpaneのアーキテクチャを参考にしながらRust + ratatuiで書き直す方針。

**diffwatch（deemkeen/diffwatch）** はファイルシステム監視ベースのリアルタイムdiffビューアで、git非依存。fsnotify → Debouncer（200ms） → State Manager → Diff Engine → TUI のパイプラインで、イベントコアレシング、ファイルフィルタリング（シェル履歴・ロックファイル・一時ファイルの自動無視）、再帰的ディレクトリ監視（`-r`フラグ）を備える。`brew install deemkeen/tap/diffwatch` で入る。git管理外のファイル変更も追えるのが特徴。

**diffwatch（sarfraznawaz2005/diffwatch）** は同名の別ツールで、こちらはgitリポジトリ特化。変更ファイルのステータス追跡（modified/new/deleted/renamed）とビジュアルdiffビューア。「AIエージェントが何をやっているか知りたい時に便利」と明記されている。

**revdiff** はオンデマンド型のレビューツール（Go製、umputun/revdiff）。常時監視ではなく、`brew install umputun/apps/revdiff` で入れて `/revdiff` で呼び出す。Ghostty 1.3+のAppleScript APIを使い、`tell application "Ghostty"` でsplitを作って `command` 付きsurface configを設定すると、コマンドが終了した時点でペインが自動的に閉じる仕組みで擬似オーバーレイを実現している。tmux環境では `display-popup`（本物のフローティングウィンドウ）が使える。kizuの `--attach` モードはこのパターンを直接踏襲する。

## ファイル監視 × git diff パイプライン

専用TUIを使わず既存Unixツールを組み合わせる方針。

**watchexec + delta** が最もカスタマイズ性が高い。watchexecはRust製で `.gitignore` を自動尊重、デバウンス50msデフォルト。deltaはgit diffのページャーでシンタックスハイライト・サイドバイサイド表示・ワードレベルdiffハイライトを提供する。

```bash
watchexec -e rs,rb,ts,js,py,java --clear -- git diff --stat && git diff | delta --side-by-side
```

deltaの設定は `~/.gitconfig` に `[delta] navigate = true / side-by-side = true / line-numbers = true / syntax-theme = Dracula` 等。

**hwatch** は `watch` コマンドの現代的代替。`hwatch -d word -n 2 "git diff --stat"` で2秒間隔実行＋前回との差分をワードレベルでハイライト。`D` キーでdiffモード切り替え、履歴も保持するので過去の変更を遡れる。

**entr** は最も軽量。`find src/ -name '*.rb' -o -name '*.ts' | entr -d -c sh -c 'git diff | delta'`。新規ファイル追加時に再起動が必要な制約あり。

## Claude Code Hooks経路

ファイル監視ではなくClaude Code自身のフックでdiff更新をトリガーする。詳細は [[Claude Codeフック]]。

```json
{
  "hooks": {
    "PostToolUse": [{
      "matcher": "Write|Edit|MultiEdit",
      "hooks": [{
        "type": "command",
        "command": "git diff --stat > /tmp/claude-diff-summary.txt && git diff > /tmp/claude-diff-full.txt",
        "async": true
      }]
    }]
  }
}
```

そして隣ペインで `watchexec -w /tmp/claude-diff-full.txt -- cat /tmp/claude-diff-full.txt | delta`。ポーリングではなくイベント駆動なので最も正確で遅延が少ない。HTTPフックタイプを使えばローカルWebサーバに送信してブラウザでリッチなdiffビューも可能で、claude-code-hooks-multi-agent-observability はこのパターンをVue + SQLite + WebSocketで実装している。

## ターミナル統合

**Ghosttyネイティブ分割。** `~/.config/ghostty/config` に `keybind = ctrl+t>shift+backslash=new_split:right` 等。Ghostty 1.3.0+では新しい分割ペインが現在のペインの作業ディレクトリを継承するので、同じリポジトリ内でそのままdiffコマンドを実行できる。

**ghostty-pane-splitter / ghostty-layout** で「左1ペイン（Claude Code用）+ 右2ペイン（上: diff、下: ターミナル）」のレイアウトを一発で構成できる。前者はRust製でmacOS/Linux対応、後者はSwift製でmacOS専用。

**tmux併用** の場合はGhosttyでShift+Enterの問題（tmux内でClaude Codeの改行入力が効かない問題）が報告されていて、Ghosttyのネイティブ分割を使う方が摩擦が少ない。ただしtmuxのスクリプタビリティ（`tmux split-window -h 'diffpane'` 等）は自動化に有利で、kizuの `--attach` モードは tmux/zellij/kitty/Ghostty を順に試す優先順位を持つ。

## 比較表

| アプローチ | セットアップ | リアルタイム性 | カスタマイズ性 | git依存 | 介入機能 |
|:---|:---:|:---:|:---:|:---:|:---:|
| diffpane | 低 | 高（自動フォロー） | 低 | あり | なし |
| diffwatch (deemkeen) | 低 | 高（fsnotify） | 中 | なし | なし |
| diffwatch (sarfraznawaz) | 低 | 高 | 低 | あり | なし |
| revdiff | 低 | オンデマンド | 中 | あり | なし |
| watchexec + delta | 中 | 中（ポーリング） | 高 | あり | なし |
| hwatch + git diff | 低 | 中（インターバル） | 中 | あり | なし |
| PostToolUseフック + delta | 中 | 最高（イベント駆動） | 高 | あり | フック経由 |
| Claude Code Desktop | 低 | 高（ネイティブUI） | 低 | あり | あり |
| [[kizu]]（設計中） | 低 | 高（fsnotify） | 中 | あり | inline scar |

## 構造的位置づけ

awesome-agent-orchestratorsリポジトリには parallel-code、vibecraft、agent-deck、amux など、エージェント監視・オーケストレーション用のツールが数十個リストされている。Claude Code自体もv2.0でUIを刷新し、Desktopではネイティブdiffビューア統合・チェックポイント機能（Esc×2で巻き戻し）を導入している。VS Code拡張ではインラインdiffがサイドバーパネルで表示可能。ただしターミナルで完結するワークフローを好むユーザー（Ghostty + nvim系）にとっては、これらのGUI統合は解決にならない。

短期的にはlazygitのauto-followモード追加が実装されればlazygit + Ghosttyペインが最有力候補になる可能性が高い。中期的にはClaude Code自体がターミナルUIにネイティブdiffパネルを統合する可能性。長期的にはエージェントオーケストレーション基盤（CodeAgentSwarm的なもの）の一部としてdiff監視が組み込まれていく流れになる。

## 関連リソース

- diffpane: https://github.com/Astro-Han/diffpane
- diffwatch (deemkeen): https://github.com/deemkeen/diffwatch
- diffwatch (sarfraznawaz2005): https://github.com/sarfraznawaz2005/diffwatch
- revdiff: https://github.com/umputun/revdiff
- delta: https://github.com/dandavison/delta
- watchexec: https://github.com/watchexec/watchexec
- hwatch: https://github.com/blacknon/hwatch
- claude-code-hooks-multi-agent-observability: https://github.com/disler/claude-code-hooks-multi-agent-observability
- ghostty-pane-splitter: https://github.com/rikeda71/ghostty-pane-splitter
- CodeAgentSwarm: https://www.codeagentswarm.com/en/guides/view-claude-code-changes-real-time
- lazygit auto-follow issue: https://github.com/jesseduffield/lazygit/issues/5337
- awesome-agent-orchestrators: https://github.com/andyrewlee/awesome-agent-orchestrators

## 関連ページ

- [[kizu]] — このカテゴリ内で「観察＋介入」を担うツールとして自作中
- [[Claude Codeフック]] — フック経路の入力スキーマと制約
- [[inline scarパターン]] — レビュー介入の非同期方式
