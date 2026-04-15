# ADR-0014: delta 風背景色ベースの diff 表示

- **Status**: Accepted
- **Date**: 2026-04-15
- **Deciders**: annenpolka, Claude

## Context

v0.1 MVP では diff 行を `+`/`-`/` ` の 1 文字 prefix 列 + `Color::Green` / `Color::Red` の前景色で表示していた（`render_diff_line` / `render_diff_line_wrapped`）。これは古典的な `git diff` の見た目で実装コストは小さいが、kizu の「ストリーミングする diff を横目で追う」という UX にとっては視線誘導が弱い:

- prefix 列は 1 文字幅しかなく、長い diff では行頭の `+`/`-` を拾いにくい
- 前景色だけで add/delete を表現するため、syntax highlight を導入すると追加行のコード色と衝突する (syntect を v0.1.1 candidate として先送りしていた根本原因)
- コンテキスト行・追加行・削除行の視覚的ブロック感が薄く、hunk の境界が視線でスキャンしにくい

delta (ripgrep-era の git pager) は同じ問題を**行全体を dim な緑/赤の背景で塗る**ことで解決しており、kizu の全画面巻物 UI とも相性がよい。v0.2 で scar 挿入を実装するにあたり、scar 対象の「この行」を精密に指差す UX を作るには、diff 行そのものの視認性を先に引き上げる必要があった。

合わせて、syntect による syntax highlight を v0.1.1 候補として棚上げしていたが、背景色ベースに切り替えれば highlight なしでも情報密度が十分に上がるため、syntect 導入自体を v0.3 以降へ再延期できる。

## Decision

v0.2 では diff 行のレンダリングを次のルールに切り替える。

1. **`+`/`-`/` ` の prefix 列を廃止する**。`render_diff_line` / `render_diff_line_wrapped` は content 文字列をそのまま bare に描画し、add/delete は行全体の背景色だけで表現する
2. **背景色は dim な固定値**を使う。`BG_ADDED = Color::Rgb(10, 50, 10)`, `BG_DELETED = Color::Rgb(60, 10, 10)`。どちらもコードテキスト（端末既定色）に対して十分なコントラストが出るよう抑えた値で、delta の `--dark` プロファイルを参考にした
3. **行の背景色は viewport 幅いっぱいまで padding する**。nowrap では `body_width = area.width - 5` (左バー分) まで space padding、wrap では `wrap_body_width = area.width - 6` (バー + `¶` marker 分) まで padding する。これをやらないと ratatui の `Paragraph` は content の末尾で style を切るため、背景色が行末で途切れる
4. **focus 判定は左バー + DIM コントラスト**で行う。
   - focused hunk (`is_selected == true`): add/delete 行は `bg(BG_ADDED|BG_DELETED)` フル輝度、context 行は `Style::default()` (端末既定色)
   - unfocused hunk (`is_selected == false`): add/delete 行は `bg(...) + Modifier::DIM`、context 行は `fg(DarkGray) + DIM`
   - cursor 行は左バーを `▶` (Yellow + Bold) に、selected hunk の他の行は `▎` (Yellow) に、それ以外は空白
5. **`Modifier::REVERSED` は使わない**。ExecPlan 起草時に想定していた REVERSED ベースの cursor 強調は、実装してみると左バーが既に focus を伝えるため冗長で、かつ REVERSED は fg/bg を反転するため dim な bg をコードテキストの fg に流入させて視認性を逆に落とすと判明した
6. **syntax highlight (`syntect`) は v0.3 以降へ再延期する**。背景色ベースで視線誘導と情報密度が十分に取れているため、v0.2 スコープには含めない

## Consequences

- **ポジティブ**:
  - diff 行の視認性が上がる。長い hunk でも add/delete のブロック感が一目でつかめる
  - `+`/`-` 列が消えたぶん、実コード content が 2 文字ぶん広く使えるようになり、wrap 境界も改善する
  - syntect 導入を先送りできる分、v0.2 の実装面積が減る
  - cursor 位置の focus コントラストが「左バー + DIM」の 2 経路になり、どちらか片方が読みづらい環境 (白背景端末、色弱配慮) でも focus を失わない
- **ネガティブ**:
  - 背景色を前提とした UI になるため、**背景色を描画しない端末 / 環境では add/delete の判別が `▎` バーと DIM だけに退化する**。たとえば `NO_COLOR=1` 相当の環境や一部の SSH 経由クライアント。v0.1 の `+`/`-` prefix 時代には起きなかった degradation
  - dim な Rgb 値を固定でハードコードしたため、明るい背景の端末 (light theme) では BG_ADDED / BG_DELETED のコントラストが弱くなる。v0.3 の設定ファイル (`~/.config/kizu/config.toml`) で theme を expose するまではこの制約が残る
  - wrap-mode の padding 計算のため、各 visual row の描画コストがわずかに上がる (body_width 分の `iter::repeat_n` allocation)。2000 行上限 (`SCROLL_ROW_LIMIT`) 以下では観測できるレベルではない
- **影響範囲**:
  - `src/ui.rs::render_diff_line` / `render_diff_line_wrapped` / `render_row` / `render` のトップレベル
  - `render_scroll_lines_use_background_color_for_added_and_deleted` / `render_scroll_lines_omit_plus_minus_prefix` / `nowrap_added_row_background_extends_to_viewport_edge` / `selected_hunk_is_bright_and_unselected_hunk_is_dim` の 4 本の Rust unit test (v0.1 の `render_scroll_lines_carry_added_and_deleted_colors` / `selected_hunk_diff_lines_render_at_full_color` はそれぞれリライト)
  - `tests/e2e/colors.test.ts` は「`+`/`-` prefix が body に付かない」という layout 契約に書き換え。tuistory の `foreground` / `background` フィルタは ratatui + crossterm が吐く `Rgb` bg を正確にマッチできないため、色そのものの pin 留めは Rust 単体テストに寄せる (ADR-0004 で既に採用した分担戦略を延長)

## Alternatives Considered

- **prefix 列を残したまま背景色だけ重ねる**: 却下。`+`/`-` が視覚的にノイズになり、delta-like のブロック感を阻害する。syntect と衝突する根本問題も残る
- **`Modifier::REVERSED` で cursor 行を反転させる**: 却下。bg と fg が swap されてコードテキストの可読性が落ちる。左バー (`▎` / `▶`) + DIM が既に focus を伝えており冗長
- **syntect を v0.2 で導入してから背景色化する**: 却下。実装面積が膨らみ、v0.2 の hook / scar スコープを圧迫する。背景色ベースだけで実用上十分に読めることが unit test / 手動確認で確認できた
- **add/delete を DIM な ANSI 色 (`Color::Green` / `Color::Red`) の background で塗る**: 却下。`Color::Green` は端末によって明るすぎる緑になり、コードテキストを埋没させる。`Color::Rgb(10, 50, 10)` レベルまで暗くして初めてコードが読めるコントラストになる
- **focused / unfocused で異なる bg 色を使う** (例: `BG_ADDED` / `BG_ADDED_DIMMER`): 却下。`Modifier::DIM` を使えば 1 色で済み、端末が DIM をサポートしない環境でもフル輝度で正しく表示される (worst-case で focus の識別力が下がるだけで、add/delete の判別は残る)

## References

- 関連 ADR: [ADR-0004](0004-tuistory-e2e.md) (e2e と unit の分担)
- 関連 ADR: [ADR-0006](0006-scroll-with-popup-picker.md) (巻物 UI モデル)
- 関連 ADR: [ADR-0009](0009-visual-cursor-position.md) (visual cursor の left-bar 表現)
- 関連 ExecPlan: `plans/v0.2.md` (M1: delta 風背景色への UI 刷新)
- 外部資料: delta (<https://github.com/dandavison/delta>) — 背景色ベースの diff 表示の参考実装
