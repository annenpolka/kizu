# ADR-0007: Visual-row scroll model + session-scoped watcher observability

- **Status**: Accepted
- **Date**: 2026-04-15
- **Deciders**: annenpolka, Claude (Codex adversarial review loop)

## Context

二度目の Codex 敵対的レビュー (branch `feat/v0.1-mvp` 対 `main`, 31 files / +7484) で以下 3 件が指摘された。単体では fixable なバグに見えるが、根は **watcher レイヤの observability 不足** と **wrap モード導入時のスクロールモデルの置き去り** という二つの設計債。

1. **watcher backend 失敗の silent drop** (`src/watcher.rs:91-143`)
   両 debouncer callback が `DebounceEventResult::Err` を握り潰して即 return していた。FSEvents の drop、監視ディレクトリの move/delete、kqueue overflow 等が起きても `last_error` に届かず、UI は最後の成功時 diff をそのまま「live monitoring が生きている」かのように描画し続ける。
2. **`is_baseline_path` が `refs/**` を一律 baseline-affecting 扱い** (`src/watcher.rs:177-195`)
   `git fetch` の `refs/remotes/*` 更新、タグ書き込み、他 linked worktree の branch 移動など、**当セッションの baseline SHA とは無関係**な ref 活動で `WatchEvent::GitHead` が発火していた。ユーザは誤爆した `HEAD*` 警告を見て `R` で誤 re-baseline し、追跡したかった diff を見失う。ADR-0005 addendum で追加した common git dir 監視と組み合わさって誤爆面積が拡大していた。
3. **wrap モードの logical-row scroll と visual-row render の座標系不一致** (`src/ui.rs:102-124`)
   `w` 有効時 1 logical row が複数 visual line に展開されるが、`render_scroll` は logical row 単位で `row_idx` を進め viewport 満杯で停止していた。`viewport_top` / `scroll` / `chunk_size` も全部 logical row ベース。カーソル前の wrap 済み行が viewport を食い潰し、**選択行が画面から消える**。centering/top placement も壊れ、`J`/`K` chunking も長行でこそ破綻する。コミット `1f1e227` で wrap モードを導入した際に `ScrollLayout` を visual 化しなかった置き去り。

このうち #1 と #2 は「watcher が送り出す event の意味論が足りない」という共通の穴。#3 は単独だが、minimum correctness patch で済ませると「同じ指摘が次の review でまた出る」 — logical と visual の座標系を曖昧にしたまま拡張機能 (scroll animation, J/K chunking, sticky header) を積み上げた分だけ悪化する。

## Decision

以下 3 つを**同一 PR で**採用する。

1. **`WatchEvent::Error(String)` variant を追加**。両 debouncer callback の `Err` 枝は `format_notify_errors(layer, errors)` でメッセージを組み立ててチャネルに送出する。app の event loop は `Error` を受けると `last_error` に書き込み、**強制 recompute を発火する**。recompute 成功で `last_error` は上書きクリア、失敗なら新しいエラーで上書き — どちらも UI に可視化される。これにより「watcher が静かに壊れたまま古い diff を live 扱いで表示し続ける」状態を構造的に排除する。

2. **`is_baseline_path` 関数廃止 → `BaselineMatcher` struct に置き換え**。session 起動時に `git symbolic-ref HEAD` で当セッションの branch ref を resolve し、以下 3 パスだけを baseline-affecting として記録する (canonicalize 済み):
   - `<per-worktree git_dir>/HEAD`
   - `<common git_dir>/refs/heads/<current branch>` (detached HEAD の場合は `None`)
   - `<common git_dir>/packed-refs`

   分類方式を「path を pattern で分類する」から「session 起動時に captured な specific path と byte 比較する」に移す。detached HEAD では branch_ref は `None` で、HEAD と packed-refs のみが対象。linked worktree では per-worktree HEAD と common dir branch ref を同時に追跡する。`git fetch`/tag write/sibling branch 更新は全て除外される。

3. **`VisualIndex` 型導入 + wrap モードの scroll 数学を visual-row space に移行**。`ScrollLayout` に `visual_heights` を持たせる代わりに、render 時に `VisualIndex::build(layout, files, body_width)` で prefix sum を都度構築する (O(rows), 2000-row cap で十分安価)。以下を visual y で動かす:
   - `viewport_placement(height, body_width, now) -> (top_row, skip_visual)` — wrap モードでは `VisualIndex` で cursor/hunk を visual y space に写像して placement を計算、`skip_visual` で mid-row から描画を開始する
   - `ScrollAnim` の `from`/`target` は visual y (f32) として解釈。nowrap では visual_y == logical_row のため既存テストの値は numeric に不変
   - `scroll_by(delta)` は wrap 有効時 `delta` を visual row として解釈 (Ctrl-d/Ctrl-u が画面分の visible lines を動かす)
   - `next_change`/`prev_change` の hunk fit 判定 (long hunk branch) を visual 単位に切り替え、chunk サイズも visual row で前進

   `toggle_wrap_lines` は副作用として `self.anim = None` を行う — nowrap と wrap の間で座標系が変わるため。nowrap モードのスクロール挙動・既存 84 テストは一切変更されない。

## Consequences

- **ポジティブ**:
  - watcher observability が片道から双方向になる。backend 失敗は必ず UI に届き、自動 recover recompute で stale drift を塞ぐ。
  - `HEAD*` 誤爆ゼロ。`git fetch` / tag / sibling branch の更新で baseline が汚染されなくなり、multi-branch workflow で kizu を使い続けられる。
  - wrap モードでカーソル消失しない。J/K の chunk 前進も visual row ベースになったため長行でもクリックごとに「見える分量」進む。
  - nowrap モードは挙動不変 (VisualIndex が identity となり既存テストが全部パスする)。`visual_viewport_top` は nowrap fast path として保持。
  - ADR-0005 で watch 対象を拡張 (linked worktree の common dir 追加) したことと合わせて、watcher のイベント意味論は「session 固有の 3 パス + worktree」で完結する。
- **ネガティブ**:
  - `VisualIndex` の per-render rebuild は wrap モードで毎フレーム O(rows) かかる。2000 行 cap のため実害ゼロだが、スクロール量爆発系の仕様変更が来たらキャッシュを検討。
  - `BaselineMatcher` は session 起動時に branch ref を frozen で captured するため、**セッション中にユーザが checkout した新 branch への移動は HEAD の書き換えだけで検知する**。branch ref 自体は watch 対象から外れる — セッション中の checkout は baseline を再評価する運用前提に立っている。detached HEAD 時は `None` 固定で同様。
  - wrap モード toggle 時に scroll anim が切れる (座標系不一致のためのクリア)。操作直後のスムーズさが一瞬落ちる代わりに、混乱した tween が出ないことを優先した。
  - `WatchEvent` が `Copy` でなくなる (`Error(String)` を含むため)。`PartialEq, Eq` は保持 — 既存テスト ` assert_eq!(event, WatchEvent::Worktree)` は不変。
- **影響範囲**:
  - `src/watcher.rs`: event enum, start signature, BaselineMatcher, format_notify_errors, debouncer callback, 全テスト
  - `src/git.rs`: `current_branch_ref` 関数追加
  - `src/app.rs`: `VisualIndex`, `viewport_placement`, `placement_target_visual_y`, `last_body_width` field, `toggle_wrap_lines` anim clear, `scroll_by`/`next_change`/`prev_change` visual awareness, `current_branch_ref` field, bootstrap wiring, run_loop Error branch
  - `src/ui.rs`: `render_scroll` が `viewport_placement` を呼び `skip_visual` を先頭行で drop

## Alternatives Considered

- **#1 watcher error: 単純に `anyhow` で bubble up して即 quit**。却下: 一時的な backend hiccup (短時間の FSEvents drop 等) で tool 全体が落ちるのは過剰反応。UI に見せて自動 recover する方が user agent workflow と噛み合う。
- **#2 narrow matcher: 単に `refs/remotes/**` だけ除外**。却下: tag write、sibling branch、pack-refs 等の残余誤爆が残る。「session 起動時に captured な具体パスだけを追跡する」という方向に移した方が設計として明確。
- **#3 wrap mode: minimum correctness patch (viewport_top を wrap 幅で前進補正するだけ)**。却下: logical scroll model を残したまま visual render と噛み合わせる hack は、scroll animation / sticky header / J/K chunking と順に衝突して複雑さが積もる。次の review で wrap 関連の指摘が再発する可能性が高い。`VisualIndex` を入れて座標系を一本化するほうが後で楽になる。
- **#3 wrap mode: `ScrollLayout` に visual heights を焼き込む**。却下: layout は `recompute_diff` のタイミングで build、body_width は render 時に決まる。build と render の間で分離したほうが wrap toggle と terminal resize を自然に扱える。VisualIndex は per-frame ephemeral。

## References

- 関連 ADR: [ADR-0002](0002-notify-debouncer-full.md) (notify-debouncer-full 採用), [ADR-0005](0005-watcher-coalescing-no-ignore-filter.md) (common git dir 追加)
- Codex adversarial review round 2 (`braz38wzp`, 2026-04-15)
- 関連仕様: `docs/SPEC.md` — 監視対象, wrap mode
