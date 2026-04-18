# Plan: C AST → syn::Expr 直接変換 / `expr_to_rust_ctx` 廃止

## ⚠️ 重要: 統合ビルドテスト時の必須オプション

本計画の検証で `~/blob/libperl-rs/12-macrogen-2-build.zsh` を使う際、
**syn::Expr モードを試したい場合は必ず `--use-syn-expr` を付けて呼び出す**:

```bash
# 旧パス（デフォルト） — 出力: tmp/build-error.log, tmp/macro_bindings.rs
~/blob/libperl-rs/12-macrogen-2-build.zsh

# 新パス（syn::Expr） — 出力: tmp/new/build-error.log, tmp/new/macro_bindings.rs
~/blob/libperl-rs/12-macrogen-2-build.zsh --use-syn-expr
```

仕組み（既にユーザ側で構築済み）:

- スクリプトは `--use-syn-expr` を受け取ると環境変数 `MACROGEN_SYN=1` を
  export して `cargo build` を起動する
- `~/blob/libperl-rs/12-macrogen-2/libperl-sys/build.rs:132` が
  `env::var("MACROGEN_SYN").is_ok()` を見て `.with_use_syn_expr(...)` を設定
- 旧パスと新パスの出力は別ディレクトリ（`tmp/` vs `tmp/new/`）に分離されるため、
  両方を残したまま比較できる

**`--use-syn-expr` を付け忘れると常に旧パスを検証してしまい、移行作業の
意味が失われる**。本リポジトリの `CLAUDE.md` の「Integration Test Files」
節に上記コマンド例を追記する（Step 0）。

### Step 0: CLAUDE.md への注記追加

- `CLAUDE.md` の「Integration Test Files」節（`build-error.log` 周辺）に
  syn モード起動コマンドと出力先（`tmp/new/` 配下）を追記
- ビルドスクリプト本体・`build.rs` 側は対応済みのため変更不要

## Context

`src/rust_codegen.rs` には現在 2 系統の式生成パスが並走している:

1. **旧パス（文字列ベース）**: `expr_to_rust_ctx` / `expr_to_rust_inline_ctx` を中心に、
   `String` を組み立て、最後に `normalize_parens` で `syn` 経由の括弧再正規化を行う。
2. **新パス（syn::Expr ベース）**: `build_syn_expr` / `build_syn_expr_with_type_hint` で
   `syn::Expr` を組み立て、`expr_to_string` で文字列化する。`--use-syn-expr` フラグで切替。

設計目標 (`doc/architecture-rust-codegen.md` Phase 5) は、新パスへの完全移行と
旧パスの削除。これにより:

- `as` 等の優先順位崩壊バグ（先行する plan `stateful-noodling-alpaca.md` 参照）の根本解消
- `ExprContext::Top` / `Default` による文字列レベルの括弧分岐が不要に
- 出力経路が一本化され、保守コストが下がる

現状 `--use-syn-expr` 有効時、旧出力との diff は約 **648 行**。原因は次の 3 つ:

1. **未対応バリアントの fallback** — `Assign`, `Pre/PostInc`, `Pre/PostDec`,
   `StmtExpr`, `CompoundLit`, `Alignof` が `expr_to_rust_ctx` に落ちる
   （差分の質的な原因 = 本計画の主対象）。
2. **fallback 文字列の再フォーマット崩れ** — fallback で得た既整形済み文字列を
   `syn::parse_str` 経由で再構築すると、`(*a).field` が `(* a) . field` のように
   空白展開される（差分の量的な原因）。**本計画では移行中の許容範囲とし、
   これに起因する見た目上の regression は無視する**。fallback そのものが
   消えれば自動的に解消するため、見た目を直すための先行対処はしない。
3. **その他の経路が未統合** — Statement 経路（`stmt_to_rust` / `_inline`）と
   inline 関数経路（`generate_inline_fn`）はまだ文字列ベースのまま。

## ゴール

- `expr_to_rust`, `expr_to_rust_ctx`, `expr_to_rust_inline`, `expr_to_rust_inline_ctx`,
  `expr_to_rust_arg`, `cast_return_expr_if_needed*`, `cast_integer_arg_if_needed`
  （文字列キャスト挿入版）, `is_string_bool_expr`, `ExprContext` enum を削除する。
- 全ての式生成は `build_syn_expr` を経由し、最終的に `expr_to_string` 一回だけで
  文字列化する（`syn::parse_str` での文字列→syn::Expr 変換は禁止）。
- マクロ展開（Expression / Statement）と inline 関数の両経路で syn::Expr を使う。

## 方針（段階的移行）

旧パスを一気に消すと回帰検出が困難になる。`--use-syn-expr` フラグを足場に、
**機能的等価性**を維持しつつ段階的に置換する。

### 成否判定の基本原則

- **主基準: cargo test と統合ビルドの成功** — `cargo test` 全パス、
  `~/blob/libperl-rs/12-macrogen-2-build.zsh` でのコンパイルエラー件数が
  旧パス（128 件）と同等以下、を各 Step の合格条件とする。
- **diff 行数は副次指標** — 参考までに以下を計測するが、減らないことを
  もって失敗とは判定しない（fallback 由来の空白差分は許容）:

```bash
cargo run -- samples/xs-wrapper.h --auto --gen-rust --bindings samples/bindings.rs \
  2>/dev/null > /tmp/old.rs
cargo run -- samples/xs-wrapper.h --auto --gen-rust --bindings samples/bindings.rs \
  --use-syn-expr 2>/dev/null > /tmp/new.rs
diff /tmp/old.rs /tmp/new.rs | grep -c '^[<>]'
```

- **移行中の見た目崩れは無視** — `( * a ) . field` のような prettyplease 由来の
  空白展開、コメント内（`// 元 C コード`）の整形差、などは Step 5 で
  fallback そのものが消えれば自然消滅する。これらを直すための先行修正はしない。
- **Step 5 の発火条件** — `--use-syn-expr` 経由のコンパイル結果がエラー件数で
  旧パスを上回らないことを確認できた段階。バイナリ完全一致は条件にしない。

## Step 1: ネイティブ Assign / Inc / Dec 対応（差分の質的原因を除去）

**ファイル**: `src/rust_codegen.rs`

`build_syn_expr` の Assign / Pre/PostInc / Pre/PostDec arm を、
旧パスへの fallback ではなく **syn::Expr で直接生成** するよう書き換える。
旧パスの実装は `rust_codegen.rs:4220-4280` 周辺（`ExprKind::PreInc` 〜 `PostDec`）
および Assign arm（`build_assign_stmt` を含む）を参照しつつ移植する。

### 必要なヘルパ（`src/syn_codegen.rs` に追加）

| ヘルパ | 役割 | 既存類似 |
|--------|------|---------|
| `assign_op(lhs, op, rhs) -> syn::Expr` | `lhs op= rhs` 形式 | `wrap_as_bool` の隣に置く |
| `block_with_value(stmts: &[syn::Stmt], value: syn::Expr) -> syn::Expr` | `{ stmt; ...; value }` | `if_else` 同じ階層 |
| `let_stmt(name: &str, value: syn::Expr) -> syn::Stmt` | `let _t = expr;` | 〃 |
| `method_call(receiver, method, args) -> syn::Expr` | `recv.method(args)` | `field_access` 同じ階層 |

### 実装パターン

```rust
// PreInc: { lvalue += 1; lvalue }  または ptr 用 { lv = lv.wrapping_add(1); lv }
ExprKind::PreInc(inner) => {
    let lv = self.build_lvalue_syn_expr(inner, info);  // ← 後述
    let body = if self.infer_type_hint(inner, info) == TypeHint::Pointer {
        // lv = lv.wrapping_add(1); lv
        let call = method_call(lv.clone(), "wrapping_add", vec![int_lit(1)]);
        block_with_value(&[assign_stmt(lv.clone(), call)], lv)
    } else {
        block_with_value(&[assign_op_stmt(lv.clone(), "+=", int_lit(1))], lv)
    };
    body
}
```

PostInc/PostDec は `let _t = lv; lv += 1; _t` パターン。
Assign は `{ lv op= rhs; lv }` パターンを `build_assign_stmt` から移植。

### `build_lvalue_syn_expr` の追加

現行 `build_lvalue_string` (`rust_codegen.rs:3740`) は文字列を返す。
syn::Expr を返す版を追加し、内部で
`try_expand_call_as_lvalue` / `_inline` の文字列結果を `syn::parse_str` で
取り込む（**この段階では暫定的に許容**。Step 4 で完全に syn::Expr 化する）。

### 検証

- `HEK_UTF8` 等が `(*HEK_KEY(hek) as ...).offset(...)` を生成（先行 plan の問題 2 と同じ）
- `cargo test` 全パス、統合ビルドのエラー件数が旧パス以下
- diff 行数の減少は副次指標として観察（参考値: 648 → ~150 程度を期待するが、
  到達しなくても合格）

## Step 2: CompoundLit / Alignof / StmtExpr のネイティブ化

`build_syn_expr` の `_` arm に残る fallback を解消。

| バリアント | 移植元 | syn::Expr 構築方針 |
|-----------|-------|------------------|
| `Alignof` | `expr_to_rust_ctx` の Alignof arm | `syn::parse_str("std::mem::align_of::<T>()")` ではなく、`Path` + `Call` を直接構築 |
| `CompoundLit` | 同 CompoundLit arm | 構造体リテラル / 配列リテラルを `syn::Expr::Struct` / `Array` で構築 |
| `StmtExpr` | 同 StmtExpr arm | `Stmt` 列を `build_syn_stmt`（Step 3 で導入）で構築し `block_with_value` |

`StmtExpr` は Statement 経路に依存するため、Step 3 と相互依存。
先に `MUTABLE_PTR` パターン以外を `unimplemented!()` に倒し、
Step 3 で本実装する。

### 検証

`cargo test` + 統合ビルドで regression なし。コメント内 C 整形の差分は
許容（Step 5 で自然解消）。

## Step 3: Statement 経路の syn 化

**目的**: `ParseResult::Statement` パスと inline 関数本体の両方で `syn::Stmt`
ベースの生成に切り替え、`stmt_to_rust*` 内の `expr_to_rust*` 呼び出しを撲滅する。

### 設計

新規関数 `build_syn_stmt(stmt: &Stmt, info: Option<&MacroInferInfo>) -> syn::Stmt`
を `src/rust_codegen.rs` に追加。
`stmt_to_rust` (`rust_codegen.rs:4733`) と `stmt_to_rust_inline`
(`rust_codegen.rs:5260`) を統一し、内部で `build_syn_expr` を呼ぶ。

| Stmt バリアント | syn 対応 |
|----------------|---------|
| `Stmt::Expr(Some(e))` | `syn::Stmt::Expr(build_syn_expr(e), Some(Semi))` |
| `Stmt::Return(Some(e))` | `syn::Stmt::Expr(Expr::Return(...), Some(Semi))` |
| `Stmt::If` | `syn::Stmt::Expr(Expr::If(...))` |
| `Stmt::While` | `syn::Stmt::Expr(Expr::While(...))` |
| `Stmt::For` | `syn::Stmt::Expr(Expr::Block { ... })` （初期化＋while 等価変換） |
| `Stmt::Compound` | `syn::Stmt::Expr(Expr::Block(...))` |

ローカル変数宣言 (`BlockItem::Decl`) は現状 `decl_to_rust_let` が直接文字列を返している。
当面は `syn::Stmt::Item` への parse-back を許容し、Step 4 のクリーンアップで対処。

### `wrap_as_bool_condition` の syn 化

`stmt_to_rust*` 内の if/while 条件で使われている `wrap_as_bool_condition*`
（文字列を受ける版, `rust_codegen.rs` で複数定義）を、
`syn::Expr` を受けて `syn::Expr` を返す `wrap_as_bool_condition_syn` に統合。
`syn_codegen::wrap_as_bool` を活用。

### `generate_inline_fn` への `--use-syn-expr` 反映

現状 `rust_codegen.rs:4976` の `generate_inline_fn` は分岐を持たない。
Step 3 完了時点で `if self.use_syn_expr { build_syn_stmt 経由 } else { 旧 }`
を導入し、両系統 diff = 0 を確認する。

### 検証

inline 関数を含むサンプルで `cargo test` + 統合ビルドが旧パスと
同等のエラー件数で完走。空白整形差分は許容。

## Step 4: lvalue / 引数生成の syn 化（残存 String 経路の根絶）

### 4-1: `build_arg_string_unified` → `build_arg_syn_expr`

現行 (`rust_codegen.rs:3669`) は最終的に `String` を返す。
キャスト挿入は既に syn::Expr レベルで行っているので、戻り値を
`syn::Expr` に変えて呼び出し側 (`build_syn_expr` の `Call` arm) で
`punctuated::Punctuated` に直接積む。
`cast_integer_arg_if_needed` の文字列フォールバック分岐
(`rust_codegen.rs:3722-3733`) は SV subtype cast を syn::Expr で
表現しなおす（`cast_syn_expr` は既存）。

### 4-2: `build_lvalue_string` → `build_lvalue_syn_expr`

Step 1 で導入した暫定版を完全 syn::Expr 化。
`try_expand_call_as_lvalue{,_inline}` (`rust_codegen.rs:1421`, `1446`) の
戻り値型を `Option<syn::Expr>` に変更。
内部の `expr_to_rust*` 呼び出しを `build_syn_expr` に置換。

### 4-3: `cast_return_expr_if_needed*` の整理

新パスでは `generate_macro` 内で直接 `cast_syn_expr` を呼んでいる
(`rust_codegen.rs:2582-2595`)。これを `cast_return_syn_expr_if_needed`
ヘルパに括り出し、旧パスの 3 メソッドは Step 5 で削除する。

### 検証

`grep -nE "self\.(expr_to_rust|expr_to_rust_inline|expr_to_rust_ctx|expr_to_rust_inline_ctx|expr_to_rust_arg|build_arg_string_unified|build_lvalue_string)\("`
で `build_syn_expr` 内に残存呼び出しがゼロになる。

## Step 5: フラグ撤去 + 旧パス削除

**発火条件**: Step 1〜4 完了後、`--use-syn-expr` 経由のコンパイル結果が
旧パスと **機能的に等価**（統合ビルドのエラー件数 ≤ 128 件、`cargo test`
全パス）になった段階。バイナリ完全一致や diff = 0 は条件にしない。
fallback 経由で生じていた空白整形差分は、本 Step での旧パス削除によって
自動的に消滅する（fallback パス自体が無くなるため）。

1. **CLI フラグ撤去**:
   - `src/main.rs:154-155` の `--use-syn-expr` 引数を削除
   - `src/main.rs:263-264` の `with_use_syn_expr` 呼び出し削除
   - `src/pipeline.rs:203, 232` の `use_syn_expr` フィールド削除
   - `src/rust_codegen.rs:990, 1002, 1286-1287` の同フィールド・setter 削除
2. **`generate_macro` 分岐の解消**:
   - `rust_codegen.rs:2561-2621` の `if self.use_syn_expr { ... } else { ... }`
     から else 側を削除し、syn::Expr 経路のみを残す
3. **死んだ関数の削除**:
   - `expr_to_rust` / `expr_to_rust_ctx` (`3784`, `3788`)
   - `expr_to_rust_inline` / `expr_to_rust_inline_ctx` (`5589`, `5593`)
   - `expr_to_rust_arg` (`2282`)
   - `cast_return_expr_if_needed` / `_inline` / `_unified` (`2380`, `2394`, `2399`)
   - `cast_integer_arg_if_needed` (`2331`) — syn 版に置換
   - `is_string_bool_expr` 等の文字列判定ヘルパ
   - `ExprContext` enum (`rust_codegen.rs:12-17`)
   - `expr_with_type_hint` / `_inline` (`4589`, `4607`) — syn 版が代替
4. **`normalize_parens` の整理**:
   - `syn_codegen` モジュール内の `expr_to_string` が parenthesize を内蔵すれば
     `normalize_parens` の文字列入力経路は不要。最終出力点での呼び出しを
     `expr_to_string` に統一（`syn_codegen.rs` を確認しつつ整理）。

## アーキテクチャドキュメントの更新

最終段階で `doc/architecture-rust-codegen.md`:

- 「文字列ベースの式生成」節（L499-528 周辺）を削除または「歴史」節へ
- L621-629 の Phase 5 を「完了」マークに更新
- `build_syn_expr` を中心とした新フローの図を追加

## 検証戦略

各 Step 終了時に必ず:

1. **単体テスト（必須）**: `cargo test`（299 テスト）が全てパス
2. **統合ビルド（必須）**: 以下のコマンドで syn パス側を検証する:
   ```bash
   ~/blob/libperl-rs/12-macrogen-2-build.zsh --use-syn-expr
   ```
   `tmp/new/build-error.log` のコンパイルエラー件数が、旧パス
   （`tmp/build-error.log`、ベースライン 128 件）と同等以下であること。
   **⚠️ `--use-syn-expr` を付け忘れるとデフォルトで旧パスを叩くため、
   常に旧パスをテストする結果になる**。スクリプト/build.rs は既に
   `--use-syn-expr` → `MACROGEN_SYN=1` → `.with_use_syn_expr(true)` の
   伝搬経路が組まれている（冒頭の「重要」節参照）。
3. **特定マクロのスポットチェック（必須）**:
   - `packWARN2`: `(a | b << 8) as i32` （優先順位）
   - `HEK_UTF8`: ネスト lvalue マクロ
   - `Perl_resume_compcv(..., true)`: bool リテラル
   - inline 関数: `Perl_av_top_index` 等
4. **統合 diff（参考のみ）**: `diff /tmp/old.rs /tmp/new.rs` の行数を
   記録するが、空白整形由来の差分は許容。減らないことをもって失敗とは
   判定しない（Step 5 の旧パス削除で自動解消するため）。

## 実施順序まとめ

| Step | 内容 | 主な変更ファイル |
|------|------|-----------------|
| 0 | **CLAUDE.md に syn モード時のビルドコマンド注記**（スクリプトは対応済み） | `CLAUDE.md` |
| 1 | Assign / Inc / Dec のネイティブ化 | `rust_codegen.rs`, `syn_codegen.rs` |
| 2 | CompoundLit / Alignof / StmtExpr | `rust_codegen.rs` |
| 3 | Statement / inline 関数経路の syn 化 | `rust_codegen.rs` |
| 4 | lvalue / 引数 / cast の完全 syn 化 | `rust_codegen.rs` |
| 5 | フラグ撤去 + 旧パス削除 + ドキュメント | `main.rs`, `pipeline.rs`, `rust_codegen.rs`, `doc/` |

各 Step の合格条件は「`cargo test` 全パス + 統合ビルドのエラー件数 ≤ 128」。
diff 行数は参考値であり、減らなくても可（Step 5 で自然解消する空白差分のため）。

各 Step 完了後にコミット。Step 1〜3 は機能的に独立しているため
途中で一旦止めても production に害はない（フラグ default off）。
Step 5 は不可逆なので、十分な統合テスト後に実施する。

## 重要な注意点

- **`syn::parse_str` 経由の往復は新規追加禁止**: 新たに書くコードでは
  `syn::parse_str(&format!("..."))` パターンを増やさない。syn::Expr ノードを
  直接構築する。**ただし既存の fallback 経路（Step 1〜4 の途中段階で
  残る `expr_to_rust_ctx → syn::parse_str` の往復）は許容する**。
  prettyplease の空白挿入による「見た目の diff」は本計画では規制せず、
  Step 5 で fallback ごと消えれば自動解消する。
- **Phase 2/3 分離原則の遵守**: 型推論や bool 判定は `Phase 2 (Infer)` で
  行うのが原則。新ヘルパ追加時に Phase 3 (Generate) で型解析を増やさない
  （CLAUDE.md 参照）。
- **コミット粒度**: 各 Step 内も「ヘルパ追加」「対応バリアントの置換」
  「検証」を別コミットに分けると bisect 容易。
