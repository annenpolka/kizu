# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

**kizu** は AI コーディングエージェント (主に Claude Code) と並走させるリアルタイム diff 監視 TUI。Rust 製の単一バイナリ。

現状は **v0.1 skeleton**。`src/{app,git,watcher,ui}.rs` の各モジュールは型定義と TODO スタブのみで、`unimplemented!()` を含む。

## 実装前に必ず読むもの

`docs/SPEC.md` がこのプロジェクトの canonical specification。**実装に着手する前に、対象機能が属するフェーズ (v0.1 / v0.2 / v0.3) を SPEC.md で確認すること。** v0.1 と v0.2 ではデータソースもキー操作も異なる。

設計コンテキストは `docs/` 配下に集約されている:

- `docs/SPEC.md` — 全体仕様、フェーズ分割、TUI/hook 層スキーマ
- `docs/claude-code-hooks.md` — PostToolUse/Stop hook の入出力と落とし穴 (`stop_hook_active` 無限ループ等)
- `docs/inline-scar-pattern.md` — scar (`@review:` インラインコメント) を非同期ファイル書き込みで実現する設計理由
- `docs/related-tools.md` — diffpane など類似ツールのサーベイ

## ビルド・検証

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

CI (`.github/workflows/ci.yml`) は以下の順で実行される。ローカルでも同順で確認すること。

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo build --release
```

## コミット / PR 規約

- コミット / PR を作る前に **`cargo fmt --check` / `cargo clippy --all-targets -- -D warnings` / `cargo test --all-targets` が通ること** (CI と同順)
- コミットメッセージは `prefix: subject` 形式 (例: `init: kizu v0.1 skeleton + design context`)
- フッターに `Co-Authored-By: Claude ...` を維持する (このリポジトリは Claude Code との共同作業を前提とした設計のため)

## 実装上のメモ

- **diff の取得は git CLI を shell out して行う** (ライブラリは使わない)。`git diff --no-renames <baseline_sha> --` を基本とし、untracked は `git status --porcelain` から合成する。`--no-renames` を含む高度なフラグを再実装せずに使えることが理由。
- watcher のデバウンスは worktree = 300ms / `.git/HEAD` = 100ms (SPEC.md 記載値)
- session baseline は起動時の HEAD SHA。`r` 2 連打でリセット可能 (v0.1 仕様)
