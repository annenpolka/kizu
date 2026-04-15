# Architecture Decision Records (ADR)

このディレクトリは kizu の**アーキテクチャ上の判断**を記録する場所。実装計画 (`plans/`) や仕様 (`docs/SPEC.md`) とは役割が異なる:

- **ADR (`docs/adr/`)** — 「なぜこの設計を選んだか」の**不可逆な判断**と、その判断が後続の実装を縛る根拠。一度書いたら書き換えず、必要なら Superseded 状態で新しい ADR を追加する。
- **ExecPlan (`plans/`)** — 「どう実装するか」の手順・進捗・発見を記録するリビングドキュメント。作業中に継続更新する。
- **SPEC (`docs/SPEC.md`)** — 「何を作るか」の正準仕様。機能要件と TUI/hook 層のスキーマ。

判断に迷ったら: **実装の how は ExecPlan、製品の what は SPEC、設計の why は ADR**。

## フォーマット

Michael Nygard 形式を採用する。ファイル名は `NNNN-kebab-case-title.md`（例: `0001-git-cli-shell-out.md`）。番号は 0001 から連番。

各 ADR は以下のセクションで構成する:

- **Status** — `Proposed` / `Accepted` / `Deprecated` / `Superseded by ADR-NNNN`
- **Context** — この判断に至った背景、制約、選択肢
- **Decision** — 採用した結論（能動態の現在形で書く: 「〜を採用する」）
- **Consequences** — この判断による結果、トレードオフ、将来縛られること

## 運用ルール

- **書くタイミング**: 「この設計は後から変えると痛い」と感じた判断をしたとき。ライブラリ選定、レイヤ分割、プロトコル選択、大きな依存の追加など。
- **書かないもの**: 命名、変数の型、小さなリファクタ、一時的な実装都合。これらは ExecPlan の Decision Log で十分。
- **Status の遷移**: `Proposed` で PR を出し、マージ時に `Accepted` へ。後で覆す場合は元 ADR を `Superseded by ADR-NNNN` にして新 ADR を追加する。**本文は書き換えない**（履歴としての価値があるため）。
- **言語**: 日本語で記述する。技術用語とコード識別子は原語のまま。
- **参照**: ExecPlan の Decision Log から ADR へリンクしてよい。ADR は自己完結させる。

## テンプレート

新しい ADR は `template.md` をコピーして作成する:

    cp docs/adr/template.md docs/adr/NNNN-your-title.md

## 索引

- [0001 — git CLI を shell out して diff を計算する](0001-git-cli-shell-out.md)
- [0002 — ファイル監視は notify-debouncer-full を採用する](0002-notify-debouncer-full.md)
- [0003 — tokio を非同期ランタイムとして採用する](0003-tokio-async-runtime.md)
- [0004 — e2e テストに tuistory + bun を採用する](0004-tuistory-e2e.md)
- [0005 — watcher は coalescing で吸収し kizu 側に .gitignore フィルタを持たない](0005-watcher-coalescing-no-ignore-filter.md)
- [0006 — 縦 2 ペイン UI を廃止し、フルスクリーン縦巻物 + popup picker に転換する](0006-scroll-with-popup-picker.md)
