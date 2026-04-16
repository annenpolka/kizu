# Inline scar pattern (reference)

_Mirrored from `Mechachang/wiki/inline scarパターン.md`._


inline scarは、AIコーディングエージェントに対する非同期コードレビューのアーキテクチャパターンである。レビュアー側のツールがソースファイル内に直接コメントを書き込み、エージェントは次のRead/Editで自然にそれを認識する。専用IPCもAPIもfifoもWebSocketも要らず、プロトコルの全構成要素がプレーンテキストとファイルシステムだけで完結する。[[kizu]] の核心メカニズムであり、kizuに限らず他のレビュー支援ツールに転用できる。

## 基本のメカニズム

レビュアーがhunkを選び、キー一打でファイルの変更箇所の直上にコメントを挿入する。

```ruby
# @kizu[free]: このバリデーション消して大丈夫？元のPR #142で入れた理由があったはず
def verify_token(claims)
```

LLMはコード内コメントに極めて反応しやすい。チャットで「auth.rsの14行目のあの変更なんだけど」と説明するより、変更の真横にコメントが物理的に存在する方がコンテキスト解決のコストがゼロ。エージェントが対応したら `@kizu[...]:` 行ごと消す。消さなかったら次の[[Claude Codeフック|Stopフック]]でgrepして「未対応のレビューが残っている」と exit 2 で継続させる。

kizu 実装では `@kizu[<kind>]:` の `<kind>` に `ask` / `reject` / `free` のいずれかが入る (`a`/`r`/`c` キーに対応)。hook 層はブラケット部分で category 別に scar を分類できる。

## 設計選択の理由

inline scarが登場する前に検討された代替案は、Esc + send-keysでメッセージを直接打ち込む同期的中断だった。これは脆い。第一に状態依存——Claude Codeがストリーミング中なのか、プロンプト待ちなのか、承認ダイアログなのかで必要なキーシーケンスが変わる。第二にタイミング——Escの後プロンプトが出るまでのwaitが不定で、速すぎるとメッセージが途中で切れる。第三にバージョン依存——Claude Code側のUI変更で壊れる。

inline scarはこの脆さを根本から回避する。レビュアー側はマークするだけ、タイミングを気にしない。エージェントは自分のペースで拾う。作業の途中で壊れない。「パッと介入」の正体は「パッと傷をつける」ことだった、というのが設計上の発見。

## 言語ごとのコメント構文

scarは言語のコメント構文で挿入される。コンパイルが通る状態を保てる方が、エージェントがビルドエラーで混乱しない。拡張子マッピングは20行程度のテーブルで実装できる。

| 拡張子 | 構文 |
|:---|:---|
| .rs/.ts/.js/.java/.go/.c/.cpp/.swift | `// @kizu[<kind>]: ...` |
| .rb/.py/.sh/.yaml/.yml/.toml | `# @kizu[<kind>]: ...` |
| .html/.xml/.svg | `<!-- @kizu[<kind>]: ... -->` |
| .css/.scss | `/* @kizu[<kind>]: ... */` |
| .sql/.lua/.hs | `-- @kizu[<kind>]: ...` |
| その他/不明 | `# @kizu[<kind>]: ...`（フォールバック） |

## 拾い上げの2層構造

レビューコメントをいつエージェントに通知するかが設計の中で最後まで揺れた部分で、結論は2層構造になった。

**Stopフック（初回通知）。** ターン終了時に tracked + untracked 両方のファイルから `grep '@kizu\['` で scar を列挙し、未対応があれば exit 2 でClaude Codeを継続させる。stderrに内容を出し、Claude Codeはそれを次のターンの指示として受け取る。CLAUDE.mdに何も書かなくても、stderrメッセージ自体が指示として機能するので導入摩擦が最小。`git diff --name-only` 単独だと untracked ファイルに刻んだ scar を取りこぼすため、`git status --porcelain --untracked-files=all` と合成する必要がある。

**PostToolUseフック（見落とし防止）。** Claude Codeが既にscarされたファイルを再編集した時に、`$CLAUDE_TOOL_INPUT_FILE_PATH` を `grep '@kizu\['` してJSON出力で `additionalContext` として通知する。1ファイルgrepなので数ミリ秒。「scarを無視して上書きした時の検知」を担う。

PostToolUseだけでは不十分な理由は時系列にある。

```
Claude Code writes file A → PostToolUse fires (まだ @kizu[*]: なし)
    ↓
人間がdiffを見て @kizu[*]: を file A に書き込む
    ↓
Claude Code writes file B → PostToolUse fires for B (A の @kizu[*]: に気づかない)
```

scarは「書かれた後」に人間がつけるので、PostToolUseだけだと最初のscarを取りこぼす。Stopフックが diff 全体 + untracked を見ることで取りこぼしを補う。

## キーバインド設計

[[kizu]] が採用しているのは次の5つ。

```
a       ask      `explain this change` を @kizu[ask]: として挿入
r       reject   `revert this change` を @kizu[reject]: として挿入
c       comment  ミニ入力欄→任意のコメントを @kizu[free]: として挿入
x       revert   hunkをgit checkoutで元に戻す（scarなし）
e       editor   $EDITOR +<line> <file> で外部エディタを起動
space   見たマーク（TUI内部のみ、ファイルに何も書かない）
```

`a` と `r` は定型メッセージで言語化ゼロ、`c` は自由入力、`x` は「やめてほしい時にrevertまでやってしまう」直接介入、`space` は「見た、OK」のTUI内部状態のみ。記号は避けてアルファベットだけにすることで打鍵負荷を下げ、設定ファイルでリマップ可能にする。

## 派生バリエーション

**revert + scar（原子操作）。** `X` キーで「hunkをrevertしながら理由をコメントとして刻む」を一打で行う。Claude Codeは次のWriteで「戻された、理由も書いてある」を同時に知覚する。

**ghost prompt キュー。** scarをコード内ではなく `.claude/review-queue.md` に追記する。Stopフックがこのファイルを読んで未チェック項目があれば exit 2。コード本体に手を入れたくない場合の代替。

**tap（コメントすら書かない）。** hunkを選んで `space` だけ。「見た」マークがついていないものを未レビューとして扱う。`!` で「見た、NG」とすればファイル名と行番号だけがエージェントに渡り、理由は書かない。エージェントは元のコードと自分の変更を見比べてrejectされた理由を推論する。LLMはこの種の推論が得意。人間のキーストロークは1打。

## 関連ページ

- [[kizu]] — このパターンを実装する自作TUIツール
- [[Claude Codeフック]] — Stop/PostToolUseフックの動作詳細
- [[AIエージェント向けdiffビューア]] — kizuを含むツール群のサーベイ
