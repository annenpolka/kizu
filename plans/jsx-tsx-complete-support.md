# JSX/TSX 完全サポート

この ExecPlan はリビングドキュメントです。`Progress`、`Surprises & Discoveries`、`Decision Log`、`Outcomes & Retrospective` は作業の進行に合わせて更新します。

## Purpose / Big Picture

kizu は AI コーディングエージェントの変更に対して、ユーザーがキー 1 打で scar を刻み、エージェントへ非同期に戻す TUI である。React や Next.js のような JSX/TSX プロジェクトでは、見た目が HTML に近い領域と TypeScript の式領域が 1 つのファイルに混ざるため、現在の「`.tsx` は常に `//` コメント」という扱いでは、scar が JSX の表示テキストになったり、JSX の開始タグを壊したりする可能性がある。

この変更では、kizu の責務の範囲で JSX/TSX を完全に扱う。ここでいう「完全」とは、TypeScript の型検査やフォーマットを実装することではない。`a` / `r` / `c` で挿入する scar が JSX/TSX の構文を壊さず、hook と pre-commit が JSX 形式の scar を見落とさず、Main diff と File view が JSX/TSX を文脈付きで読みやすく表示できる、という意味である。

完了後、ユーザーは `.tsx` の JSX children、JSX fragment、複数行 props、TypeScript 式、通常の `.ts` / `.js` / `.jsx` の各変更行で `a` / `r` / `c` を押せる。JSX の children では `{/* @kizu[ask]: explain this change */}` が挿入され、TypeScript の式領域では `// @kizu[ask]: explain this change` が挿入される。`kizu hook-stop` と pre-commit guard はどちらの形も unresolved scar として検出する。

## Progress

- [x] (2026-05-04 11:54:25Z) 現状確認: `git status --short --branch` が `## main...origin/main` で、作業ツリーが clean であることを確認した。
- [x] (2026-05-04 11:54:25Z) 仕様確認: `/Users/annenpolka/ghq/github.com/annenpolka/kizu/docs/SPEC.md` の scar、hook、Stream mode、syntax highlight の現行仕様を確認した。
- [x] (2026-05-04 11:54:25Z) コード確認: `/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/scar.rs`、`src/hook/scan.rs`、`src/highlight.rs`、`src/ui/diff_line.rs`、`src/ui/file_view.rs`、`src/app/layout.rs` の現行責務を確認した。
- [x] (2026-05-04 11:54:25Z) 依存候補確認: crates.io 上で `tree-sitter-highlight 0.26.8`、`tree-sitter-typescript 0.23.2`、`tree-sitter-javascript 0.25.0`、`oxc_parser 0.128.0` を確認した。
- [x] (2026-05-04 11:54:25Z) ExecPlan 初版を起稿した。
- [x] (2026-05-04 12:03:59Z) 実装前 baseline: `cargo test --all-targets --all-features highlight::tests -- --nocapture`、`cargo test --all-targets --all-features scar::tests -- --nocapture`、`cargo test --all-targets --all-features hook::tests -- --nocapture` が成功した。
- [x] (2026-05-04 12:06:12Z) Milestone 1: tree-sitter の TSX parse と TSX highlight 設定を `tsx_parser_accepts_react_component_fixture`、`tsx_highlight_configuration_builds` で確認した。Red は偽実装の assertion failure、Green は tree-sitter 実装で成功。
- [x] (2026-05-04 12:08:15Z) Milestone 2: parser-aware scar placement を実装した。`jsx_tsx_scar_placement` の Red は JSX children / props / fragment / broken TSX で失敗し、Green 後は `cargo test scar::tests -- --nocapture` が 41 passed で成功した。
- [x] (2026-05-04 12:09:21Z) Milestone 3: hook scan と pre-commit scan 用 staged scan が JSX block scar を検出できるようにした。`cargo test hook::tests -- --nocapture` が 32 passed で成功した。
- [x] (2026-05-04 12:15:06Z) Milestone 4: JSX/TSX の document-aware highlighting を導入した。File view、Main diff の worktree Added/Context、baseline Deleted を tree-sitter document tokens に接続し、`cargo test highlight::tests -- --nocapture` と TSX UI targeted tests が成功した。
- [x] (2026-05-04 12:30:52Z) Milestone 5: README、SPEC、ADR、e2e、benchmark を更新し、`just ci` で完走した。途中の Criterion で render regression を検出し、非 JS/TS ファイルでは diff view が whole-document highlight を構築しない guard を追加して解消した。

## Surprises & Discoveries

- Observation: 現在の scar 挿入は拡張子だけでコメント構文を選ぶため、`.tsx` と `.jsx` は常に `//` コメントになる。
  Evidence: `/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/scar.rs` の `detect_comment_syntax` は `"ts" | "tsx" | "js" | "jsx"` を `SLASH_SLASH` に対応させている。

- Observation: 現在の hook scanner は JSX block scar の `{/* ... */}` を検出できない。
  Evidence: `/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/hook/scan.rs` の `SCAR_RE` は `//`、`#`、`--`、`/*`、`<!--` のみを先頭コメントとして扱う。

- Observation: 現在の syntax highlight は 1 行だけを syntect に渡すため、複数行 JSX、複数行 comment、template literal の状態を次行へ引き継げない。
  Evidence: `/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/highlight.rs` の `highlight_line_uncached` は毎回 `HighlightLines::new(syntax, theme)` を作り、その 1 行だけを `highlight_line` に渡している。

- Observation: Main diff renderer には diff row ごとの old/new line number が既に存在する。
  Evidence: `/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/app/layout.rs` の `ScrollLayout::diff_line_numbers` は DiffLine row ごとに `(old_line_number, new_line_number)` を保持している。これは document-aware highlighting の行対応に使える。

- Observation: crates.io 上の依存候補は Rust 1.94+ の project requirement と矛盾しない。
  Evidence: `cargo info tree-sitter-highlight` は `rust-version: 1.84`、`cargo info oxc_parser` は `rust-version: 1.93.0` を報告した。kizu の README は Rust 1.94+ を要求している。

- Observation: 初版の baseline test command は Cargo の test filter 仕様に合わなかった。
  Evidence: `cargo test --all-targets --all-features highlight::tests scar::tests hook::tests -- --nocapture` は `unexpected argument 'scar::tests'` で失敗した。Cargo は positional test filter を 1 つだけ受け取るため、同等の確認は `highlight::tests`、`scar::tests`、`hook::tests` に分けて実行した。

- Observation: 初回の全 operations bench で render が約 4 倍に退行した。
  Evidence: `cargo bench --bench operations -- --sample-size 10` の初回結果で `render/full_frame_nowrap` は約 `1.07ms`、`render/full_frame_wrap` は約 `1.09ms` になり、どちらも +290% 前後の regression と報告された。原因は diff view が非 JS/TS ファイルにも worktree / baseline の whole-document highlight を毎フレーム試していたことだった。

## Decision Log

- Decision: この ExecPlan での「完全サポート」は、kizu の feature surface に限定する。
  Rationale: kizu は TypeScript compiler、formatter、language server ではない。ユーザーが必要としているのは、JSX/TSX repo で scar review がコードを壊さず、未対応 scar が確実にエージェントへ戻り、TUI 上で変更が読めることである。
  Date/Author: 2026-05-04 11:54:25Z / Codex

- Decision: JSX/TSX の主要 backend は tree-sitter にする。
  Rationale: tree-sitter は構文木と highlight query の両方を提供し、同じ source parse から scar placement と syntax highlight を作れる。Oxc parser は JavaScript/TypeScript/JSX/TSX parser として強力だが、kizu のこの変更で必要な highlight query を別途用意する必要があり、最初の実装では依存を二重化しない方が小さく進められる。
  Date/Author: 2026-05-04 11:54:25Z / Codex

- Decision: `syntect` は削除せず、non-JS languages の fallback colorizer として残す。
  Rationale: kizu は Rust、Python、Ruby、HTML、CSS なども扱う。今回の問題は JSX/TSX の混合構文であり、すべての言語の highlight backend を同時に置き換える必要はない。
  Date/Author: 2026-05-04 11:54:25Z / Codex

- Decision: 実装順は「safe scar placement」、「scanner」、「highlight」、「docs/e2e」とする。
  Rationale: highlight の不正確さは読みやすさの問題だが、scar placement の不正確さは worktree を壊す問題である。hook scanner が JSX scar を拾えないと Stop hook と pre-commit guard の安全性も欠けるため、UI 表示より先に書き込みと検出を固める。
  Date/Author: 2026-05-04 11:54:25Z / Codex

- Decision: この変更は ADR を追加する。
  Rationale: SPEC は現時点で syntax highlight を syntect と説明している。JSX/TSX に限って tree-sitter を構文 backend として採用するのは、後から覆すとコード構造と依存に効く設計判断である。`docs/adr/0020-tree-sitter-for-jsx-tsx.md` を追加し、採用範囲と non-goals を記録する。
  Date/Author: 2026-05-04 11:54:25Z / Codex

- Decision: Main diff view の whole-document highlight は JS/TS/JSX/TSX だけに限定する。
  Rationale: Rust などの non-JS ファイルは既存の per-line syntect fallback で十分に読める。全ファイルへ document highlight を適用すると render hot path が file read と baseline read を毎フレーム試し、TSX 完全サポートの範囲外で大きく退行する。
  Date/Author: 2026-05-04 12:30:52Z / Codex

## Outcomes & Retrospective

JSX/TSX の scar insertion、hook scan、document-aware highlight、docs、ADR、e2e、benchmark を実装した。TSX children と fragment では JSX block scar、TypeScript expression では line comment scar、JSX opening tag の attribute 行では element 直前への relocation を行う。hook scanner と staged scan は JSX block scar を検出する。Main diff と File view は JS/TS/JSX/TSX に tree-sitter document highlight を使い、non-JS は syntect fallback を維持する。

初回 benchmark で render regression を検出したため、非 JS/TS ファイルでは diff view が whole-document highlight を作らない performance guard を追加した。最終的に `just ci`、全 e2e、全 operations benchmark が成功した。

## Context and Orientation

この repository は `/Users/annenpolka/ghq/github.com/annenpolka/kizu` にある Rust 製 TUI である。TUI とは terminal user interface の略で、端末の中に画面を描くアプリを意味する。kizu は AI コーディングエージェントが作った差分を監視し、ユーザーが問題のある行へ scar を挿入する。scar とは `@kizu[ask]: ...` のような marker を持つ inline comment である。

JSX は JavaScript の中に XML/HTML 風の tag を書ける構文である。TSX は TypeScript と JSX を同じファイルに書ける構文である。たとえば `return <div>{count}</div>` は TypeScript の式と JSX の tag と JSX children が混ざっている。通常の `//` コメントは TypeScript の式領域では安全だが、JSX children の中では JavaScript comment ではなく表示テキストとして扱われる可能性がある。JSX children の中で安全な comment は `{/* comment */}` である。

現在の scar 挿入の入口は `/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/scar.rs` である。`detect_comment_syntax(path: &Path) -> CommentSyntax` が拡張子だけで comment syntax を返し、`insert_scar(path, line_number, kind, body)` が対象行の直上へ 1 行挿入する。undo は `remove_scar(path, line_1indexed, rendered)` で、挿入時に保存した exact rendered line を使う。

現在の TUI から scar を呼ぶ入口は `/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/app/review.rs` である。`insert_canned_scar` と `commit_scar_comment` が `scar_target_line()` で file path と 1-indexed line number を取り、`insert_scar` を呼ぶ。

現在の scanner は `/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/hook/scan.rs` である。`scan_scars(paths)` は worktree file を読み、`scan_scars_from_index(root, paths)` は git index の staged blob を読み、同じ regex で unresolved scars を検出する。Stop hook と pre-commit guard はこの scanner に依存する。

現在の syntax highlight は `/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/highlight.rs` である。`Highlighter::highlight_line(line, path)` が 1 行の token list を返し、`src/ui/diff_line.rs` と `src/ui/file_view.rs` が token の foreground color を ratatui の `Span` に変換する。ratatui は Rust の TUI framework で、`Span` は色や太字などの style を持つ文字列片である。

Main diff view の layout は `/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/app/layout.rs` が作る。`RowKind::DiffLine { file_idx, hunk_idx, line_idx }` は「どの file のどの hunk のどの diff line か」を指す。`ScrollLayout::diff_line_numbers` は row index と同じ長さの配列で、DiffLine row には old side と new side の行番号が入る。Added と Context は new side の行番号を持ち、Deleted は old side の行番号を持つ。

File view は `/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/app/file_view.rs` と `/Users/annenpolka/ghq/github.com/annenpolka/kizu/src/ui/file_view.rs` に分かれている。File view は worktree の現在ファイル全体を表示するので、TSX file 全体を tree-sitter で parse して行ごとに highlight するのに向いている。

この ExecPlan で導入する parser-aware scar placement とは、拡張子だけで comment syntax を決めず、対象 line が JSX children、JSX opening tag、TypeScript expression のどこにあるかを parser で調べてから挿入形を決める仕組みである。parser とは source code を構文木に変換する部品で、tree-sitter は Rust から使える incremental parser library である。

## Plan of Work

最初に依存と API の prototype を作る。`Cargo.toml` に `tree-sitter = "0.26.8"`、`tree-sitter-highlight = "0.26.8"`、`tree-sitter-typescript = "0.23.2"`、`tree-sitter-javascript = "0.25.0"` を追加する。小さな unit test で `tree_sitter_typescript::LANGUAGE_TSX` を parser に設定し、`const App = () => <div>{count}</div>;` を parse できることを確認する。さらに `tree_sitter_highlight::HighlightConfiguration` を TSX / TypeScript / JavaScript 用に組み立てられることを確認する。ここで API 互換性に問題があれば、この ExecPlan の Decision Log に記録して依存方針を更新する。

次に parser-aware scar placement を作る。`src/language.rs` と `src/language/js_ts.rs` を追加し、JS/TS/JSX/TSX だけを扱う小さな language layer を置く。`src/scar.rs` は引き続き public API の入口にするが、`insert_scar` は file content を読んだ後に language layer へ問い合わせる。language layer は path extension と source text と target line を受け取り、通常の `//` scar、JSX block scar、または安全な行への relocation を返す。relocation とは、ユーザーが cursor を置いた行そのものが JSX opening tag の props の途中など安全に 1 行コメントを挿せない場所だったとき、同じ JSX element の直前など安全な行へ scar を移すことである。

TSX の placement rule はこの plan 内で固定する。`.ts` は常に TypeScript として扱い、`.tsx` は TSX として扱う。`.js` は JavaScript として扱い、`.jsx` は JSX として扱う。対象行が TypeScript / JavaScript の statement、expression、import、type alias、function body の中なら `// @kizu[...]` を対象行の直上に挿入する。対象行が JSX children text、JSX child element、JSX fragment の children の中なら `{/* @kizu[...] */}` を対象行の直上に同じ indentation で挿入する。対象行が JSX opening tag の attribute list の中なら、nearest JSX element または fragment の開始行の直上へ `{/* @kizu[...] */}` を挿入する。対象行が parser error を含む領域で安全判定できない場合は、従来の `//` で強行せず、`insert_scar` が `Err` を返し、TUI は `last_error` に `scar:` prefix 付きで表示する。

次に scanner を JSX block scar に対応させる。`src/hook/scan.rs` の regex と message trimming を更新し、`{/* @kizu[ask]: explain this change */}` を `kind = "ask"`、`message = "explain this change"` として返す。既存の `//`、`#`、`--`、`/* */`、`<!-- -->` は壊さない。Markdown の fenced code block skip は現状通り維持する。`scan_scars_from_index` も同じ helper を通るため、worktree scan と staged scan の両方で JSX scar を拾う。

次に document-aware highlighting を導入する。document-aware highlighting とは、1 行だけではなく file 全体を parse し、その結果から各行の token colors を返す方式である。`src/highlight.rs` は既存の `highlight_line` を fallback として残しつつ、`highlight_document(path, content)` と `highlight_document_line(path, content, line_number)` 相当の API を持つ。JS/TS/JSX/TSX では tree-sitter highlight を使い、それ以外では syntect の現行 path を使う。File view は worktree file 全体を持っているので、document highlight を直接使う。Main diff view は `ScrollLayout::diff_line_numbers` の old/new line number を使って、Added と Context には worktree 側 document highlight、Deleted には baseline 側 document highlight を対応させる。baseline 側とは、セッション開始時の HEAD にある file content のことである。

Main diff view の baseline 側 highlight のために、`src/git/diff.rs` または `src/git/repo.rs` に `read_file_at_revision(root, revision, path) -> Result<Option<String>>` を追加する。この関数は `git show <revision>:<path>` を shell out し、UTF-8 として読めたときだけ `Some(String)` を返す。file が baseline に存在しない new file、binary file、UTF-8 ではない file は `Ok(None)` とし、renderer は現行の line-local fallback に戻る。

最後に docs と ADR を更新する。`docs/SPEC.md` の comment mapping に `.jsx` と `.tsx` の文脈依存 rule を追加し、README の Scars section に JSX example を追加する。`docs/adr/0020-tree-sitter-for-jsx-tsx.md` を追加し、tree-sitter を JSX/TSX の language engine として採用し、syntect は fallback として残す判断を記録する。

## Concrete Steps

作業ディレクトリは常に `/Users/annenpolka/ghq/github.com/annenpolka/kizu` にする。

1. 現状の確認を行う。

        git status --short --branch
        cargo test --all-targets --all-features highlight::tests scar::tests hook::tests -- --nocapture

   期待する出力は、branch が `main...origin/main` を指し、既存 targeted tests が成功することである。

2. 依存を追加し、最初の prototype test を Red として書く。`Cargo.toml` に tree-sitter 系依存を追加し、`src/language.rs` と `src/language/js_ts.rs` を作る。最初の test 名は `tsx_parser_accepts_react_component_fixture` にする。

        cargo test tsx_parser_accepts_react_component_fixture -- --nocapture

   期待する Red は、未実装関数または未接続 module による compile error である。ここで runtime panic ではなく compile fail になってよい。

3. Prototype を Green にする。TSX parser を作り、fixture が parse error なしで通ること、TSX highlight configuration が作れることを確認する。

        cargo test tsx_parser_accepts_react_component_fixture -- --nocapture
        cargo test tsx_highlight_configuration_builds -- --nocapture

   期待する出力は `test result: ok` である。

4. scar placement の Red tests を追加する。最低限、次の test 名を追加する。

        cargo test jsx_tsx_scar_placement -- --nocapture

   test は `insert_scar_uses_jsx_block_comment_for_jsx_children`、`insert_scar_uses_slash_slash_for_ts_expression`、`insert_scar_relocates_from_jsx_opening_tag_attribute`、`insert_scar_handles_jsx_fragment_child`、`insert_scar_returns_error_when_tsx_parse_is_unrecoverable` を含める。期待する Red は、現行実装が JSX children に `//` を入れてしまう assertion failure である。

5. scar placement を Green にする。`src/scar.rs` の `insert_scar` は file content を読んだ後、JS/TS/JSX/TSX の path なら language layer へ placement を問い合わせる。既存 non-JS languages は従来の `detect_comment_syntax` path を通す。

        cargo test jsx_tsx_scar_placement -- --nocapture
        cargo test scar::tests -- --nocapture

   期待する出力は、JSX/TSX の新規 test と既存 scar tests がすべて成功することである。

6. hook scanner の Red tests を追加する。

        cargo test scan_scars_finds_jsx_block_comment -- --nocapture
        cargo test scan_scars_from_index_finds_jsx_block_comment -- --nocapture

   期待する Red は、`{/* @kizu[ask]: explain this change */}` が現行 regex では検出されない assertion failure である。

7. hook scanner を Green にする。`src/hook/scan.rs` の regex または parse helper を更新し、message から trailing `*/}` を取り除く。既存 HTML / CSS block comment の trailing marker trimming もこの機会に helper 化する。

        cargo test hook::tests -- --nocapture
        cargo test scan_scars_from_index -- --nocapture

   期待する出力は、hook tests がすべて成功することである。

8. document-aware highlighting の Red tests を追加する。

        cargo test tsx_document_highlight -- --nocapture

   test は `tsx_document_highlight_distinguishes_jsx_tag_and_ts_keyword`、`tsx_document_highlight_keeps_multiline_jsx_context`、`highlight_line_keeps_syntect_fallback_for_rust` を含める。期待する Red は、現行 `highlight_line` が TSX 全体文脈を持てず、token 色が単調になる assertion failure である。

9. document-aware highlighting を Green にする。`src/highlight.rs` を必要なら `src/highlight/` module に分割し、JS/TS/JSX/TSX の document path は tree-sitter、その他は syntect に通す。既存 `Highlighter::highlight_line` は public API として残し、renderer 移行中の fallback にする。

        cargo test tsx_document_highlight -- --nocapture
        cargo test highlight::tests -- --nocapture

   期待する出力は、TSX document highlight tests と既存 highlight tests がすべて成功することである。

10. Main diff view と File view を document highlight に接続する。File view は current file content を直接使う。Main diff view は Added / Context で worktree document、Deleted で baseline document を使う。ここで `git::read_file_at_revision` を追加する。

        cargo test diff_view_uses_document_highlight_for_tsx -- --nocapture
        cargo test file_view_uses_document_highlight_for_tsx -- --nocapture

   期待する出力は、TSX の JSX tag と TypeScript token が fallback の単色ではなく複数 token に分かれることである。

11. e2e test を追加する。`tests/e2e/jsx-tsx.test.ts` を作り、temporary git repo に TSX component を置いて kizu を起動し、JSX children 行で `a` を押す。file content に `{/* @kizu[ask]: explain this change */}` が入ることと、`u` で元に戻ることを確認する。別 scenario で `kizu hook-stop` が JSX scar を検出することも確認する。

        cargo build --release --locked
        cd tests/e2e && bun test jsx-tsx.test.ts

   期待する出力は、new e2e test が pass することである。

12. docs と ADR を更新する。`docs/SPEC.md`、`README.md`、`README.ja.md`、`docs/adr/0020-tree-sitter-for-jsx-tsx.md` を編集する。

        cargo fmt --all -- --check
        cargo clippy --all-targets --all-features -- -D warnings
        cargo test --all-targets --all-features
        cargo build --release --locked
        cd tests/e2e && bun test

   期待する出力は、Rust unit tests、release build、e2e がすべて成功することである。

13. 最後に full gate を実行する。

        just ci

   期待する出力は、fmt-check、clippy、cargo test、release build、tuistory e2e がすべて成功することである。

## Validation and Acceptance

この ExecPlan の受け入れ条件は、コードが compile することだけではない。実際に JSX/TSX の source file に scar を挿入し、hook がそれを検出し、TUI がその file を読みやすく表示することを確認する。

第一の受け入れ条件は、JSX children 行への scar 挿入である。fixture の `.tsx` file に次のような component を置く。

        export function Counter({ count }: { count: number }) {
          return (
            <section>
              <p>Count: {count}</p>
            </section>
          );
        }

`<p>Count: {count}</p>` の行で `a` を押すと、file は次のようになる。

        export function Counter({ count }: { count: number }) {
          return (
            <section>
              {/* <at>kizu[ask]: explain this change */}
              <p>Count: {count}</p>
            </section>
          );
        }

第二の受け入れ条件は、TypeScript expression 行への scar 挿入である。`const label: string = String(count);` の直上では、従来通り `// @kizu[ask]: explain this change` が挿入される。

第三の受け入れ条件は、JSX opening tag attribute の安全 relocation である。次のような複数行 props の途中に cursor がある場合、props の途中へ scar を挿さず、nearest JSX element の直上へ `{/* ... */}` を挿す。

        <Button
          kind="primary"
          onClick={() => save()}
        >
          Save
        </Button>

第四の受け入れ条件は、hook detection である。`kizu hook-stop` は `// @kizu[...]` と `{/* @kizu[...] */}` の両方を unresolved scar として stderr に出し、exit code 2 を返す。pre-commit guard も staged JSX scar を検出して commit を block する。

第五の受け入れ条件は、highlight である。File view で `.tsx` file を開くと、JSX tag、attribute、TypeScript keyword、string、comment が単一色ではなく token ごとの foreground color に分かれる。Main diff view でも Added / Context / Deleted の各行が document line number に基づいて色付けされる。Deleted 行の baseline file が読めないときだけ fallback してよい。

最終検証は `just ci` で行う。成功時は fmt-check、clippy、Rust unit tests、release build、e2e がすべて成功する。追加された targeted tests は Red で失敗してから Green で成功したことを、この plan の `Artifacts and Notes` に短く記録する。

## Idempotence and Recovery

依存追加は `Cargo.toml` と `Cargo.lock` の変更として管理する。途中で tree-sitter 系 crate の API 互換性に問題が出た場合は、prototype milestone の時点で止め、`Decision Log` に原因を記録する。Oxc parser へ切り替える場合も、まず scar placement だけを Oxc で実装し、highlight は tree-sitter または syntect fallback のままにする。

scar insertion tests と e2e tests は temporary directory の中で file を作る。何度実行しても repository の real source files を変更しない。e2e が途中で落ちた場合は `tests/e2e` の helper が作った temporary repo を削除して再実行する。

`insert_scar` の undo stack は exact rendered line に依存しているため、JSX block scar でも `ScarInsert.rendered` に newline を含めない 1 行の exact text を保存する。`remove_scar` は既存の exact match semantics を維持し、ユーザーが scar 行を編集した場合は mismatch を error として扱う。

document highlight の cache は source content と path によって invalidation できるようにする。watcher recompute 後に source content が変われば cache miss になる設計にし、古い parse tree による表示ずれを避ける。performance が悪化した場合は `cargo bench --bench operations` の `highlight` と `render` group を実行し、before/after を比較する。

## Artifacts and Notes

初版起稿時の repository 状態:

        git status --short --branch
        ## main...origin/main

依存候補の crates.io 確認:

        cargo search tree-sitter-highlight --limit 3
        tree-sitter-highlight = "0.26.8"

        cargo search tree-sitter-typescript --limit 3
        tree-sitter-typescript = "0.23.2"

        cargo search tree-sitter-javascript --limit 3
        tree-sitter-javascript = "0.25.0"

        cargo search oxc_parser --limit 3
        oxc_parser = "0.128.0"

現行実装の重要箇所:

        src/scar.rs
        detect_comment_syntax currently maps .tsx/.jsx to //.

        src/hook/scan.rs
        SCAR_RE currently matches //, #, --, /*, <!-- only.

        src/highlight.rs
        Highlighter currently highlights one line at a time with syntect.

        src/app/layout.rs
        ScrollLayout already stores diff_line_numbers for old/new line mapping.

このセクションには、実装中に Red の失敗出力、Green の成功出力、benchmark の before/after、e2e transcript を追記する。

実装前 baseline:

        cargo test --all-targets --all-features highlight::tests -- --nocapture
        result: 6 passed; 0 failed

        cargo test --all-targets --all-features scar::tests -- --nocapture
        result: 36 passed; 0 failed

        cargo test --all-targets --all-features hook::tests -- --nocapture
        result: 30 passed; 0 failed

Milestone 1 Red / Green:

        cargo test tsx_parser_accepts_react_component_fixture -- --nocapture
        Red: assertion failed: tsx_source_parses_without_errors(source)

        cargo test tsx_parser_accepts_react_component_fixture -- --nocapture
        Green: 1 passed; 0 failed

        cargo test tsx_highlight_configuration_builds -- --nocapture
        Green: 1 passed; 0 failed

Milestone 2 Red / Green:

        cargo test jsx_tsx_scar_placement -- --nocapture
        Red: JSX children、JSX opening tag attribute、fragment child は従来の `//` scar になり、broken TSX は error ではなく scar を挿入して失敗した。

        cargo test jsx_tsx_scar_placement -- --nocapture
        Green: 5 passed; 0 failed

        cargo test scar::tests -- --nocapture
        Green: 41 passed; 0 failed

Milestone 3 Red / Green:

        cargo test scan_scars_finds_jsx_block_comment -- --nocapture
        Red: JSX block scar が 0 hits になった。

        cargo test scan_scars_from_index_finds_jsx_block_comment -- --nocapture
        Red: staged JSX block scar が 0 hits になった。

        cargo test scan_scars_finds_jsx_block_comment -- --nocapture
        Green: 1 passed; 0 failed

        cargo test scan_scars_from_index_finds_jsx_block_comment -- --nocapture
        Green: 1 passed; 0 failed

        cargo test hook::tests -- --nocapture
        Green: 32 passed; 0 failed

Milestone 4 Red / Green:

        cargo test tsx_document_highlight -- --nocapture
        Red: `Highlighter::highlight_document` が未実装で compile error になった。

        cargo test tsx_document_highlight -- --nocapture
        Green: 3 passed; 0 failed

        cargo test diff_view_uses_document_highlight_for_tsx -- --nocapture
        Red: attribute `kind` の foreground が syntect fallback の `Rgb(211, 208, 200)` になり、tree-sitter document highlight の `Cyan` ではなかった。

        cargo test file_view_uses_document_highlight_for_tsx -- --nocapture
        Red: attribute `kind` の foreground が syntect fallback の `Rgb(211, 208, 200)` になり、tree-sitter document highlight の `Cyan` ではなかった。

        cargo test diff_view_uses_document_highlight_for_tsx -- --nocapture
        Green: 1 passed; 0 failed

        cargo test file_view_uses_document_highlight_for_tsx -- --nocapture
        Green: 1 passed; 0 failed

        cargo test diff_view_uses_baseline_document_highlight_for_deleted_tsx -- --nocapture
        Green: 1 passed; 0 failed

        cargo test highlight::tests -- --nocapture
        Green: 9 passed; 0 failed

Milestone 5 e2e / benchmark / gate:

        cargo test diff_view_does_not_document_highlight_non_js_files -- --nocapture
        Red: Rust file の diff render 後に document highlight cache が 1 件作られ、non-JS ファイルでも whole-document highlight が走っていることを確認した。

        cargo test diff_view_ -- --nocapture
        Green: 3 passed; 0 failed

        cargo clippy --all-targets --all-features -- -D warnings
        Green: finished successfully

        cargo test --all-targets --all-features
        Green: 489 passed; 0 failed

        cargo bench --bench operations -- --sample-size 10
        Green: all operations benches completed. 新規 TSX 系は `highlight_80_tsx_components_document` が約 `71.1µs`、`scan_jsx_block_scars_80_files` が約 `1.03ms`、`insert_and_remove_scar_30_component_tsx_file` が約 `443.6µs`。render は `full_frame_nowrap` 約 `262.1µs`、`full_frame_wrap` 約 `268.5µs` で regression なし。

        cargo build --release --locked
        Green: release build succeeded

        KIZU_BIN=../../target/release/kizu bun test jsx-tsx.test.ts
        Green: 2 pass; 0 fail

        KIZU_BIN=../../target/release/kizu bun test
        Green: 37 pass; 0 fail

        just ci
        Green: fmt-check、clippy、cargo test、release build、bun install、e2e が成功。e2e は 37 pass; 0 fail。

## Interfaces and Dependencies

`Cargo.toml` には次の依存を追加する。

        tree-sitter = "0.26.8"
        tree-sitter-highlight = "0.26.8"
        tree-sitter-typescript = "0.23.2"
        tree-sitter-javascript = "0.25.0"

`oxc_parser = "0.128.0"` は初期実装では追加しない。tree-sitter で safe scar placement が実装できない具体的な parse gap が見つかった場合だけ、Decision Log に理由を書いて追加する。

`src/language.rs` は新しい facade module になる。`src/lib.rs` から `pub mod language;` として公開するか、crate 内だけで十分なら `pub(crate) mod language;` とする。少なくとも `src/scar.rs` と `src/highlight.rs` から使える visibility にする。

`src/language/js_ts.rs` は JS/TS/JSX/TSX 専用の parser helper を持つ。最終的に次のような contract を満たす。

        pub(crate) enum JsTsDialect {
            JavaScript,
            Jsx,
            TypeScript,
            Tsx,
        }

        pub(crate) enum JsTsScarStyle {
            LineComment,
            JsxBlockComment,
        }

        pub(crate) struct JsTsScarPlacement {
            pub insert_before_line_1indexed: usize,
            pub style: JsTsScarStyle,
        }

        pub(crate) fn dialect_for_path(path: &std::path::Path) -> Option<JsTsDialect>;

        pub(crate) fn scar_placement_for_line(
            dialect: JsTsDialect,
            source: &str,
            target_line_1indexed: usize,
        ) -> anyhow::Result<JsTsScarPlacement>;

`src/scar.rs` は既存 public API を維持する。`insert_scar(path, line_number, kind, body) -> Result<Option<ScarInsert>>` と `remove_scar` の signature は変えない。内部では、JS/TS/JSX/TSX file の場合だけ `JsTsScarPlacement` を使って rendered line と insertion line を決める。JSX block scar の rendering は次の形に固定する。

        {/* <at>kizu[ask]: explain this change */}

`src/hook/scan.rs` は regex 単体にすべてを押し込まず、line prefix detection と trailing marker trimming を helper 化してよい。最終的な scanner contract は、次の input をすべて同じ `ScarHit` として返すことである。以下はこの Markdown 自体が現行 hook に unresolved scar として検出されるのを避けるため、literal `@` を `<at>` と表記している。実際の unit test fixture では `<at>` を `@` に置き換える。

        // <at>kizu[ask]: explain this change
        # <at>kizu[ask]: explain this change
        /* <at>kizu[ask]: explain this change */
        <!-- <at>kizu[ask]: explain this change -->
        {/* <at>kizu[ask]: explain this change */}

`src/highlight.rs` は既存 `HlToken` を維持する。新しく document-level API を追加する場合、最小 contract は次の通りにする。

        pub struct HighlightedDocument {
            pub lines: Vec<Vec<HlToken>>,
        }

        impl Highlighter {
            pub fn highlight_document(
                &self,
                content: &str,
                path: &std::path::Path,
            ) -> HighlightedDocument;

            pub fn highlight_line(
                &self,
                line: &str,
                path: &std::path::Path,
            ) -> Vec<HlToken>;
        }

`highlight_line` は既存 renderer と tests のために残す。JSX/TSX の完全な表示は `highlight_document` 経由で行い、line-local fallback は unknown file、binary、baseline read failure、parser failure の時だけ使う。

`src/git/repo.rs` または `src/git/diff.rs` には baseline content reader を追加する。signature は次の形を目安にする。

        pub fn read_file_at_revision(
            root: &std::path::Path,
            revision: &str,
            path: &std::path::Path,
        ) -> anyhow::Result<Option<String>>;

この関数は `git show <revision>:<path>` を使う。file が存在しない場合、binary または non-UTF-8 の場合は `Ok(None)` とし、UI は fallback highlight に戻る。

`docs/adr/0020-tree-sitter-for-jsx-tsx.md` は Michael Nygard 形式で追加する。Status は PR 中なら `Proposed`、merge 時に `Accepted` へ更新する。内容は、tree-sitter を JSX/TSX の構文 engine として採用し、syntect を fallback として残す判断、Oxc を初期採用しない理由、performance と binary size の懸念、今後の rollback path を含める。
