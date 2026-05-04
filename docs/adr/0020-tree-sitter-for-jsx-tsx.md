# ADR-0020: JSX/TSX に tree-sitter を採用する

- **Status**: Proposed
- **Date**: 2026-05-04
- **Deciders**: Codex

## Context

kizu の scar は対象行の直上にコメントを書き込む。従来は拡張子だけでコメント構文を選び、`.tsx` と `.jsx` も `//` として扱っていた。しかし JSX/TSX は JavaScript / TypeScript の statement と JSX children が同じファイルに混在する。JSX children の直上に `//` を挿すと表示テキストになったり、tag / props の途中に挿すと構文を壊したりする。

syntax highlight も従来は syntect の 1 行 fallback だった。複数行 JSX props、JSX children、Deleted 行の baseline 側のように、文書全体の構文状態が必要な場面では 1 行だけでは正しく色を決められない。

## Decision

JS/TS/JSX/TSX には tree-sitter を採用する。scar placement は tree-sitter の parse tree で対象行の JSX 文脈を判定し、JSX children / fragment / element では `{/* ... */}`、JavaScript / TypeScript statement では `// ...` を挿入する。複数行 props の途中は props 内へ書かず、nearest JSX element の直上へ移して `{/* ... */}` を挿入する。

JS/TS/JSX/TSX の syntax highlight は tree-sitter highlight query を文書単位で実行する。Main diff view は Added / Context を worktree 側 document、Deleted を baseline 側 document に対応させる。その他の言語は syntect fallback を維持する。

## Consequences

- ポジティブ: JSX/TSX の scar が構文を壊しにくくなり、hook / pre-commit は `{/* @kizu[...] */}` を未解決 scar として検出できる。
- ポジティブ: TSX の複数行 props や JSX tag / attribute が Main diff view と File view の両方で文書文脈に基づいて読める。
- ネガティブ: tree-sitter 系 crate が追加され、バイナリサイズと compile time は増える。
- ネガティブ: JS/TS/JSX/TSX の highlight palette は syntect theme と完全には一致しない。kizu 内で小さな固定 palette に対応づける。
- 影響範囲: `src/language/js_ts.rs`、`src/scar.rs`、`src/hook/scan.rs`、`src/highlight.rs`、`src/ui/*`、`src/git/repo.rs`。

## Alternatives Considered

- syntect fallback のまま拡張子 mapping だけを増やす: JSX children と TypeScript statement の安全な挿し分けができないため却下。
- Oxc parser を scar placement に使う: JavaScript / TypeScript parser として強力だが、highlight query は別途必要になり、今回の変更では parser と highlighter が二重化するため初期採用しない。
- JSX/TSX では常に `{/* ... */}` を使う: TypeScript statement の直上では JSX block comment が無効なので却下。

## References

- 関連 ExecPlan: `plans/jsx-tsx-complete-support.md`
- 関連仕様: `docs/SPEC.md`
- 関連 ADR: ADR-0001, ADR-0014
