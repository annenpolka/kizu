# ADR-0019: `kizu init` のインタラクティブプロンプトは dialoguer をやめて自前実装する

- **Status**: Proposed
- **Date**: 2026-04-17
- **Deciders**: annenpolka, Claude

## Context

`kizu init` は 6 種類の AI コーディングエージェント (Claude Code / Cursor / Codex / Qwen Code / Cline / Gemini) に対して hook を設置するインタラクティブ CLI であり、ユーザーと kizu の初接触点である。

歴史的経緯:

- **08661ab (2026-04 初期)** で [dialoguer](https://crates.io/crates/dialoguer) `MultiSelect` / `Select` + `ColorfulTheme` を採用し、カラフルなサポートレベル表示 (`● Full` / `◐ StopOnly` / `○ WriteSideOnly`) + 検出状態 (`✓ detected` / `~ bin only` / `✗ not found`) を提供していた
- **8b0f9dd (2026-04-16)** で視覚的なスクロールドリフト (キー入力ごとに表示が上へずれる) を回避するため、items をプレーンテキストに退化させた。コミットメッセージ: _"dialoguer 0.11 uses byte length (not visible width) for line-clearing, so ANSI escapes caused it to over-count and drift the display upward on every keystroke"_
- **905e388 (同日)** で dialoguer を 0.11 → 0.12 へ上げたが、**バグは 0.12 でも未修正**である

### 0.12 で修正されていないことの検証

`~/.cargo/registry/src/index.crates.io-*/dialoguer-0.12.0/src/prompts/multi_select.rs:227` に以下のコードが残る:

    for items in self.items.iter().flat_map(|i| i.split('\n')).collect::<Vec<_>>() {
        let size = &items.len();         // ← str::len() = **バイト長**
        size_vec.push(*size);
    }
    // ...
    render.clear_preserve_prompt(&size_vec)?;  // サイズ > term_width なら行クリアを増やす

`clear_preserve_prompt` は `*size > self.term.size().1` で「折り返しによる追加行数」を推定する。ANSI エスケープが含まれると可視幅は 0 なのにバイト数は膨張するため、ターミナル幅を超えたと誤判定し、余分に行をクリアして表示が上にドリフトする。0.11 と 0.12 の `src/prompts/multi_select.rs` / `src/theme/render.rs` は完全同一 (`diff` 差分なし) で、`console` 0.15 → 0.16 の変更にも該当する修正は含まれない。

### 選択肢

1. **現状維持** (プレーンテキスト items で妥協)
   - メリット: 変更ゼロ
   - デメリット: 初接触 UX の視認性が低い。6 種のエージェント × 4 種のサポートレベル × 3 種の検出状態を、色もアイコンも無しで把握させるのは苦しい
2. **dialoguer を fork / patch**
   - メリット: ライブラリ側の他の機能も使い続けられる
   - デメリット: upstream に PR は出せるが、マージ保証なし。fork を抱える場合 kizu 側で transitively 維持することになる
3. **他の interactive crate に乗り換え** (e.g. `inquire`)
   - メリット: 機能豊富
   - デメリット: 同じ種類のバグを別のライブラリで踏み直すリスク。依存グラフが縮小する保証なし
4. **自前実装** (crossterm + unicode-width で `src/prompt.rs` を書く)
   - メリット: kizu は既に crossterm (`event-stream` feature) + unicode-width を持っており、TUI レイヤでも ANSI 可視幅計算の知見がある。置き換え対象は `src/init.rs` の 3 箇所のみで実装表面積が小さい
   - デメリット: ~200 LoC の自前コード + ユニットテスト維持

## Decision

**選択肢 4 を採用する。** `src/prompt.rs` を新設し、crossterm + unicode-width だけを使った最小同期プロンプトを実装する。`src/init.rs` の 3 箇所 (`select_agents_interactive` / `select_scope_interactive` / `ask_scope_fallback`) を差し替え、`Cargo.toml` から `dialoguer` を削除する。

方針詳細:

1. **スコープを `init` の UI だけに限定する**。TUI ランタイム (`src/app.rs`) には手を入れない
2. **可視幅は自前で計算する**。`unicode-width::UnicodeWidthStr` を `strip_ansi_escapes` 的な自前ストリッパを通してから適用する。行は**折り返さず**、ターミナル幅を超えるラベルは末尾 `…` で丸める (折り返しがドリフトの温床なので、そもそも作らない)
3. **raw mode は RAII ガードで確実に戻す**。`struct RawModeGuard; impl Drop for RawModeGuard { fn drop(&mut self) { let _ = disable_raw_mode(); } }` で、panic 時にも `disable_raw_mode()` を走らせる
4. **描画は相対カーソル + 部分クリア**。初回は普通に `print!`、2 回目以降は `cursor::MoveUp(prev_height)` + `terminal::Clear(ClearType::FromCursorDown)` で前回描画領域を消してから再描画する
5. **キャンセル経路を明示**。Esc と Ctrl-C を `Ok(None)` として返し、呼び出し側がユーザー都合のキャンセルと区別できるようにする
6. **非 TTY 時は即エラー**。パイプ経由での起動では `--non-interactive` フラグ (既存) を要求する
7. **アイコン表現は過去のデザインを復元する**: `●` (Full) / `◐` (StopOnly 系) / `○` (WriteSideOnly) / `✓ detected` / `~ bin only` / `✗ not found` + 既存の `c_green` / `c_yellow` / `c_dim` 色関数

## Consequences

- **ポジティブ**:
  - アイコンと色が戻る (ユーザーから「いつの間にか消えた」と指摘された表示が復活)
  - 依存から `dialoguer` / `console` / `shell-words` / `zeroize` の 4 crate が消える (`cargo tree -p kizu | grep -E 'dialoguer|console|shell-words|zeroize'` の出力が空になる)
  - ANSI 可視幅の計算責務を kizu 側に持つことで、将来 CJK ラベルや絵文字プレフィックスを追加しても崩れない設計になる
  - 描画ロジックがテスト可能 (`render_select_frame` / `render_multi_frame` を pure 関数に切る)
- **ネガティブ**:
  - 自前コード約 200 行 + ユニットテストの維持コストが発生する
  - `dialoguer` が提供する他機能 (`Input` / `Password` / `FuzzySelect`) を将来使いたくなった場合は、再度依存を戻すか自前実装を拡張する必要がある。ただし kizu は現状これらを必要としない
  - raw mode 復旧に失敗した場合、ユーザーが `stty sane` を手動実行する必要が出うる。Drop 実装で最大限予防するが、本質的にゼロにはできない
- **影響範囲**:
  - **追加**: `src/prompt.rs` (新規)、`docs/adr/0019-custom-prompt-for-init.md` (本 ADR)
  - **変更**: `src/init.rs` (3 箇所の呼び出し側)、`Cargo.toml` (dialoguer 削除)、`Cargo.lock` (再生成)
  - **テスト**: Rust unit tests (`cargo test --lib prompt::`)、e2e (`tests/e2e/init.test.ts` のキーコードは互換のはず、目視ドリフト不再発の expectation 追加)

## Alternatives Considered

- **現状維持 (プレーンテキスト)**: 却下。kizu とユーザーの最初の接触点であり、UX の質を落としたままで良い理由がない
- **dialoguer を fork / patch して upstream PR**: 却下。upstream の反応速度に kizu のリリースを縛られる。本 ADR の決定後に upstream PR を出すのは構わないが、kizu の依存戦略としては自前化しておく
- **inquire / termion-based prompt crate への乗り換え**: 却下。同じクラスのバグ (ANSI 扱い) を別ライブラリで踏むリスクがある。依存を増やして代替するよりも、スコープが小さい自前化が合理的
- **ratatui で init UI 全体を書く**: 却下。ratatui は全画面 alternate screen 前提で、`kizu init` のような「通常の shell 出力に数行の interactive prompt を挿入する」用途に向かない。alternate screen に切り替えると init 終了時にプロンプトの結果表示も消える

## References

- 関連 ExecPlan: [`plans/init-custom-prompt.md`](../../plans/init-custom-prompt.md)
- 関連 commit: `08661ab` (colorful UI 追加), `8b0f9dd` (plain-text 退化), `905e388` (dialoguer 0.11 → 0.12)
- 関連ファイル: `src/init.rs:335-391,537-576` (dialoguer 呼び出し 3 箇所)
- 外部資料:
  - [dialoguer 0.12.0 multi_select.rs](https://github.com/console-rs/dialoguer/blob/v0.12.0/src/prompts/multi_select.rs#L227) (該当バグ箇所)
  - [unicode-width](https://crates.io/crates/unicode-width)
  - [crossterm](https://crates.io/crates/crossterm)
