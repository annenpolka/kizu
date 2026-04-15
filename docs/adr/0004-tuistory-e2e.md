# ADR-0004: e2e テストに tuistory + bun を採用する

- **Status**: Accepted
- **Date**: 2026-04-15
- **Deciders**: Initial designer

## Context

kizu は TUI ツールであり、価値の中心が**画面に映る diff の見え方とリアクティブな更新挙動**にある。これらは `cargo test` の単体テストでは捕捉しきれない:

- ratatui のレイアウト崩れ（左右分割比率、フッタ位置）
- crossterm の raw mode が `q` 終了 / panic 時に正しく解除されるか
- 別プロセスのファイル書き換えから notify-debouncer-full の 300ms デバウンスを経て画面更新が観測されるまでのリアクティブ挙動
- syntect ハイライト（added=緑 / deleted=赤）が実際の端末色として出力されるか
- フォローモードと手動モードの遷移が key イベント経由で正しく動くか

これらを検証する選択肢:

1. **手動 E2E のみ**: ExecPlan の Validation セクションで定義した手順を毎リリース手で踏む
2. **Pure Rust の pty crate**: `expectrl` / `rexpect` を `cargo test` から呼ぶ
3. **`insta` + ratatui Buffer 直接スナップショット**: pty を介さず `App` 状態を `Buffer` に描画してスナップショット
4. **tuistory (TypeScript / bun)**: 「Playwright for terminal user interfaces」。実 pty + waitForText regex + 色フィルタ + inline snapshot

評価:

| 観点 | 手動 | expectrl | insta+Buffer | tuistory |
|---|---|---|---|---|
| 視覚忠実度 | 最高 | 中 | 高 | 最高 |
| 色情報の検証 | 目視 | 弱 | 強 | 強 |
| key イベント配送の検証 | 可 | 可 | 不可 | 可 |
| raw mode 復旧の検証 | 可 | 限定的 | 不可 | 可 |
| 自動化 | 不可 | 可 | 可 | 可 |
| 開発者体験 | 苦痛 | 中 | 高 (cargo test) | 高 (Playwright API) |
| Rust 単一トリ整合性 | 高 | 高 | 高 | 低 (Node toolchain) |

tuistory は v0.0.16 (2026-02-23 リリース) と pre-1.0 だが、active に開発されており kizu の検証ニーズに最もフィットする。

## Decision

v0.1 から **tuistory + bun** を e2e テストの主力として採用する。

具体的構造:

- `tests/e2e/` ディレクトリを作り、TypeScript で e2e テストを書く
- ランタイムは **bun**（tuistory 作者が主に使うランタイムで、インストールと起動が高速）
- テストランナーは **`bun:test`**（jest 互換 API、別途 vitest 不要）
- スナップショットは **`toMatchInlineSnapshot()`** を使い、テストファイル内に直接埋め込む（外部 .snap ファイルを管理しない）
- kizu バイナリの場所は環境変数 `KIZU_BIN` で渡す。ローカル開発時のデフォルトは `target/release/kizu`、CI では `${{ github.workspace }}/target/release/kizu`
- 一時 git リポは TypeScript ヘルパ `createTempRepo()` で `node:fs.mkdtempSync` + `child_process.execSync('git init ...')` を使って作る
- v0.1 でカバーする e2e は 5 本: (A) 起動→q クリーン終了 (B) ファイル書き換え→画面更新 (C) j/k ナビゲーションと [follow]/[manual] (D) R による baseline リセット (E) added=緑 / deleted=赤 の色検証
- CI は既存の `ci` ジョブの末尾に `oven-sh/setup-bun@v2` → `bun install --frozen-lockfile` → `bun test` を追加。required check として gate する

`insta` + Buffer 直接スナップショット方式は v0.1 では採用しない（tuistory が同じ範囲をカバーできるため）。将来 dev サイクルが遅すぎると感じたら hybrid を検討する。

## Consequences

**ポジティブ**:

- 「画面に映るもの」を実 pty で検証できるため、kizu の価値命題（visual diff observation）を直接守れる
- v0.1 Validation セクションの手動 E2E 5 項目のうち 5 つ全てを自動化可能
- inline snapshot により「実装が正しく描画している瞬間」をテストファイルで物理的に保存できる
- raw mode 復旧 / panic 時の端末状態など、unit test では絶対に捕まらない回帰を防げる
- tuistory の Playwright 風 API は学習コストが低く、テスト可読性が高い

**ネガティブ**:

- リポジトリに **Node toolchain (bun)** が必要になる。CI と dev 環境の両方
- `cargo test` だけでは v0.1 を完全検証できなくなる。`cargo test --all-targets && cd tests/e2e && bun install && bun test` が新しい完全検証コマンド
- tuistory が pre-1.0 (0.0.x) なので破壊的変更のリスク。`package.json` で正確な版を pin する
- bun daemon プロセスがバックグラウンドに残る可能性。CI の最後で `tuistory daemon-stop` を呼ぶか、`bun test` の後始末に任せる
- スナップショットは端末幅 / 高さ / 環境変数で揺らぐ。`tuistory.launchTerminal({ cols: 120, rows: 36, env: { ... } })` で固定する
- README に MIT 記載があるが LICENSE ファイルは未確認。導入前に確認する

**影響範囲**:

- `tests/e2e/` ディレクトリ新設（package.json, tsconfig.json, bun.lockb, helpers.ts, 5 個の `*.test.ts`）
- `.github/workflows/ci.yml` に bun セットアップ + e2e テストステップ追加
- `.gitignore` に `tests/e2e/node_modules/` を追加
- `README.md` / `CLAUDE.md` の検証フローに e2e ステップを追記
- `kizu` の本体コードには **影響しない**（black-box テストのため）

## Alternatives Considered

- **手動 E2E のみ**: リリースごとの目視チェック。回帰がすり抜けやすく、開発スピードが落ちる。却下。
- **`expectrl` (Pure Rust pty)**: Node を避けつつ `cargo test` から実 pty を扱える。ただし inline snapshot, 色フィルタ, regex ベースの `waitForText` といった tuistory の利点が弱い。kizu のように色とレイアウトが価値の中心にあるツールでは表現力不足。却下。
- **`insta` + ratatui Buffer 直接スナップショット**: 最速で決定的だが、pty を介さないので raw mode / key イベント配送 / リアクティブ更新の実時間挙動を測れない。tuistory が同じ視覚範囲をカバーできるため v0.1 では二重投資になる。将来 dev サイクル高速化が必要になった時点で再検討。

## References

- 関連 ExecPlan: `plans/v0.1-mvp.md` Milestone 6
- 関連 ADR: ADR-0003 (tokio)
- 外部資料:
  - <https://github.com/remorses/tuistory>
  - <https://bun.sh/docs/cli/test>
  - <https://github.com/oven-sh/setup-bun>
