# ADR-0006: 縦 2 ペイン UI を廃止し、フルスクリーン縦巻物 + popup picker に転換する

- **Status**: Accepted
- **Date**: 2026-04-15
- **Deciders**: Initial designer (実機での折り返し問題を踏まえて改訂)

## Context

ExecPlan 当初版 (M4) は ratatui のレイアウトを `Layout::horizontal([Percentage(30), Percentage(70)])` で左右分割し、左にファイルリスト、右に diff を描画する 2 ペイン構成にしていた。これは `lazygit` や `tig` で見慣れた形だが、kizu の典型的な走らせ方とは噛み合わないことが M5 完了直後の実機確認で分かった。

具体的な摩擦:

- kizu は **AI コーディングエージェントの並走監視ツール** であり、ユーザーは tmux/zellij/Ghostty で画面を分割した状態で立ち上げる。kizu 用の pane はターミナル全幅を貰えず、60〜90 列幅で運用される。
- 90 列幅の 30/70 分割では右ペインが約 63 列。Rust の現実的な行 (100 列前後の `if let Some(x) = ... && let Some(y) = ...` のようなチェーン) が確実に折り返す。
- 折り返しが起きる行は ratatui の `Paragraph` のデフォルト wrap 設定でも壊れた表示になり、`+` `-` プレフィックスと連続行の対応が崩れる。
- 「監視されている側が書いた行をそのままの形で見たい」という kizu のコア体験が、レイアウトのせいで損なわれていた。

選択肢を 5 つ並列で出して検討した (会話ログ参照):

A. ファイルリストを廃止して **全 hunk を縦に連結した巻物** にする
B. ファイルリストを上端 1 行のタブバーに圧縮して diff 全幅
C. ファイルリストを上端 1 行の sparkline ミニマップに圧縮
D. 平常時は diff 全幅、`Space` で modal popup の file picker
E. kizu 自身が tmux/zellij の pane を切って 2 プロセスに分業

A と D の組み合わせが「巻物が観察の主舞台、picker がジャンプの裏方」という役割分担として最もきれいに収まり、ratatui 標準の widget だけで実装できる。

## Decision

UI を以下の形に作り直す:

- `Layout::vertical([Min(0), Length(1)])` の **2 段** だけにし、上段はメイン (100% 幅)、下段は 1 行フッタ
- メインは **全ファイルの全 hunk を 1 本の縦巻物に flatten** した `Paragraph`。各ファイルの境界には灰色の `path ── status ── +A/-D` ヘッダ行と空行スペーサーを挿入する
- 30/70 の左右分割と独立した `render_file_list` / `render_diff` 関数は廃止する
- ファイル選択は `Space` で起動する **modal popup picker**:
  - ratatui 標準の `Clear` widget で中央 60% × 60% のフローティングを抜き、`Block::bordered()` を被せる
  - 上 1 行は query 入力 (`> auth`)、下は substring match で絞り込んだファイルの `List`
  - `Enter` で選択ファイルの**最初の hunk の絶対 row** に jump、`Esc` / `Ctrl-C` でキャンセル
  - 並び順は mtime 降順を保持するので、`Space` → そのまま `Enter` で「いまエージェントが書いた最新ファイル」に飛べる
- `App` の選択状態を以下に組み替える:
  - `selected: usize` / `selected_path: Option<PathBuf>` / `diff_scroll: usize` の 3 つを廃止
  - 代わりに `scroll: usize` (巻物の上端 row index)、`anchor: Option<HunkAnchor>` (path + hunk_old_start で hunk を fingerprinting)、`picker: Option<PickerState>` を持つ
  - 派生構造として `layout: ScrollLayout { rows, hunk_starts, file_first_hunk, file_of_row }` を `recompute_diff` のたびに `build_layout` で再構築する
  - `current_file_idx()` / `current_file_path()` は `layout.file_of_row[scroll]` から導出する
- `handle_key` を **2 モード dispatch** に分ける:
  - 通常モード: `j`/`k`/`Ctrl-d`/`Ctrl-u`/`g`/`G` は scroll、`J`/`K` は次/前 hunk へ jump、`f` は follow 復帰、`Space` で picker 起動、`R` / `q` / `Ctrl-C` は従来通り
  - picker モード: 任意の char は query に追加、`Backspace` で削除、`↑`/`↓`/`Ctrl-n`/`Ctrl-p`/`Ctrl-j`/`Ctrl-k` でカーソル移動 (fzf 互換)、`Enter` で確定、`Esc`/`Ctrl-C` でキャンセル
- フォローモードのターゲット定義を「mtime 最新ファイルの**最後**の hunk」に変更する。エージェントの最新書き込みが画面下端に常に映る。
- **ファイルは mtime 昇順** (古い順) で並べる。「テキストは下に流れる」という前提と「最新が上にいる」が認知的に矛盾しているため、`tail -f` / chat log と同じ方向 (古い → 新しいが top → bottom) に揃える。`App.files[0]` が最古、`App.files.len()-1` が最新。`build_layout` はそのまま順方向に rows を生成し、巻物は古→新の自然な時系列になる。`follow_target_row` は `files.last()` の最後の hunk = **巻物の底**。
- **picker は逆方向 (mtime 降順)** で表示する。ファイルピッカーの慣例は「いま編集したファイルが最初」なので、`picker_results` は `(0..files.len()).rev()` を返す。`Space` → そのまま `Enter` で「いまエージェントが書いた最新ファイル」に飛ぶ。スクロールと picker で並び順は逆になるが、それぞれの UI 構造 (連続的な巻物 / 離散的なリスト) に合った convention を優先する。

### 視野階層 (M4v 改訂)

巻物全体を常に展開しつつ、注意の階層を **色の濃淡** で立てる。視線を動かせばすべての hunk の本文が読めるが、視野のコアは選択中の hunk と accent マーカーだけ。

- **selected hunk**: フル彩度。`+` Color::Green / `-` Color::Red / context white、`@@` ヘッダ Color::Cyan、左端に Color::Yellow の `▎` 1 文字
- **unselected hunk**: 同じ色 + `Modifier::DIM`。type info は読めるが視線が引きつけられない
- **file header の path 色 = ステータス**: Modified=Cyan / Added=Green / Deleted=Red / Untracked=Yellow。`M`/`A`/`D`/`??` ラベルは廃止
- **file header に mtime と +N -M を常時表示**。視線スキャンで「いつ・どれくらい・どこ」が把握できる
- **hunk header は xfuncname を優先**: git の `@@ -10,6 +10,9 @@ fn verify_token(...) {` 形式から trailing context を `Hunk.context` として保持し、`@@ fn verify_token(...) {` の形で表示する。context が無いときは従来の `@@ -10,6 +10,9 @@` に fallback
- **scar `◇` プレースホルダー**: file header 末尾に scar 列を確保しておくが、scar 機能本体 (v0.2) が land するまで実データは入らない
- 完全な静寂 (γ) より騒がしいが、視線スキャンで全状態が拾える「**情報を持った静寂**」を選ぶ。`Modifier::DIM` が効かないターミナルでも色だけは出るので最低限の type info は維持される

## Consequences

**ポジティブ**:

- 折り返しが消える。kizu pane が 60 列でも各 diff 行はその全幅を使える
- 1 つのファイルに長い hunk が連続していても、巻物として上下スクロールするだけで読める
- ファイルリストを暗記しなくても、`Space` → タイプ → `Enter` で 1 アクションで飛べる
- ファイル数が増えたとき (50 ファイルの大規模リファクタなど) でも左ペインのリストでスクロールに時間を取られない
- ratatui の `Tabs` `Sparkline` `Clear` `List` `ListState` はすべて 0.30 の標準 widget で揃うので外部依存を増やさない

**ネガティブ**:

- 「並んでいるファイル全部を一覧してから選ぶ」という慣れた workflow が消える。常に巻物に潜る形になり、初見ユーザーには `Space` を覚えるまでの学習コストがある (フッタに `<Space> picker` のヒントを常時表示して緩和)
- `App` の状態が再設計されたので M3 で書いた app::tests のうち 13 件のうち多くを書き直した
- M4 で書いた ui::tests も `render_file_list` / `render_diff` が消えたので全面的に作り直した

**影響範囲**:

- `src/app.rs` を 460 行から 850 行に再構築 (data model + 2 モード handle_key + scroll/picker helper + tests)
- `src/ui.rs` を 380 行から 550 行に再構築 (`render_scroll` / `render_picker` / `render_footer` / `render_empty`)
- `src/git.rs` / `src/watcher.rs` / `src/main.rs` には影響なし
- M3 / M4 の plan エントリ (`selected` / `diff_scroll` 系) は obsolete として記録し、新エントリで上書き

## Alternatives Considered

- **B (タブバー)**: 縦の縦割りはなくなるが、ファイル数が 8 を超えると水平スクロールが必要で、それなら popup と変わらない
- **C (sparkline)**: 視覚的にきれいだが、選択操作が「sparkline 上の `●` を h/l で動かす」になりキー操作の発見性が低い
- **E (kizu が pane を切る)**: Unix 哲学的に魅力だが、tmux/zellij/kitty の API 差異を v0.1 で吸収するのは過剰、v0.3 の `--attach` で本格的に取り組む方が筋が良い
- **30/70 を維持して `Wrap { trim: false }` を有効にする**: 折り返しは消えるが、`+` `-` のプレフィックスが折り返し後の行頭にも付かないので diff の意味が壊れる

## References

- 関連 ExecPlan: `plans/v0.1-mvp.md` Milestone 4 (UI re-design セクション)
- 関連 ADR: ADR-0002 (notify-debouncer-full), ADR-0003 (tokio)
- ratatui 0.30 の `Clear` / `ListState` / `Block::bordered`: <https://docs.rs/ratatui/0.30.0/ratatui/widgets/index.html>
- 会話ログ: emergent-engine セッションで A〜E の 5 候補を出した turn
