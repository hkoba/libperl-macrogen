# Plan: syn::Expr 完全移行 — 旧方式廃止

## 現状

### 完了済み

- Phase 1-2: `syn_codegen.rs` に `parenthesize()`, `expr_to_string()` 等を実装
- Phase 3: AST 変換ヘルパー (`wrap_as_bool`, `insert_cast`, `null_for_type` 等)
- Phase 4 (部分): `normalize_parens()` を codegen 出力ポイントに適用 (53→46 paren warnings)

### 問題点

`expr_to_rust_ctx()` (macro, 801行) と `expr_to_rust_inline_ctx()` (inline, 728行) が
ほぼ同じロジックを重複して実装している（45% 同一、36% 類似、16% 差異）。

同様に `stmt_to_rust()` (37行) と `stmt_to_rust_inline()` (293行)、
`infer_expr_type()` (166行) と `infer_expr_type_inline()` (189行) も重複。

**合計重複**: ~2200行（rust_codegen.rs 6439行の 34%）

### 廃止対象

| 対象 | 行数 | 置換先 |
|------|------|--------|
| `expr_to_rust_ctx()` | 801 | `build_syn_expr()` |
| `expr_to_rust_inline_ctx()` | 728 | `build_syn_expr()` (共通化) |
| `stmt_to_rust()` | 37 | `build_syn_stmt()` |
| `stmt_to_rust_inline()` | 293 | `build_syn_stmt()` (共通化) |
| `ExprContext` enum | 7 | 不要（parenthesize が担当） |
| `strip_outer_parens()` | 30 | 不要（normalize_parens → 不要） |
| `normalize_parens()` | — | 不要（直接 syn::Expr を構築するため） |

## 設計方針

### 型推論コンテキストの統一

macro と inline の主な差異は型推論のソース。これをトレイトで抽象化する。

```rust
/// 式のコード生成に必要な型情報コンテキスト
trait CodegenTypeContext {
    /// 式の型を推論
    fn infer_expr_type(&self, expr: &Expr) -> Option<UnifiedType>;
    /// 式がポインタ型か
    fn is_pointer_expr(&self, expr: &Expr) -> bool;
    /// 式が bool を返すか
    fn is_bool_expr(&self, expr: &Expr) -> bool;
    /// 呼び出し先の引数型を取得
    fn get_callee_param_type(&self, func_name: &str, idx: usize) -> Option<UnifiedType>;
    /// 呼び出し先の戻り値型を取得
    fn get_callee_return_type(&self, func_name: &str) -> Option<UnifiedType>;
    /// フィールドの型を取得
    fn get_field_type(&self, field: &str) -> Option<&UnifiedType>;
}
```

マクロ用とインライン用の実装:

```rust
/// マクロ用: MacroInferInfo + TypeEnv からの型推論
struct MacroTypeContext<'a> { ... }

/// インライン用: current_param_types からの型推論  
struct InlineTypeContext<'a> { ... }
```

### build_syn_expr の構造

```rust
impl RustCodegen<'_> {
    fn build_syn_expr(&mut self, expr: &Expr, ctx: &dyn CodegenTypeContext) -> syn::Expr {
        match &expr.kind {
            ExprKind::Ident(name) => self.build_ident(*name, ctx),
            ExprKind::Binary { op, lhs, rhs } => self.build_binary(*op, lhs, rhs, ctx),
            ExprKind::Cast { type_name, expr } => self.build_cast(type_name, expr, ctx),
            // ... 全 ExprKind を網羅
        }
    }
}
```

各 match arm はプライベートメソッドに分離し、テスト可能にする。

### 出力フロー

```
C AST (Expr)
  ↓  build_syn_expr(expr, ctx)    — 意味変換込みで syn::Expr 構築
syn::Expr
  ↓  parenthesize()               — 優先順位に基づく括弧挿入
syn::Expr (括弧付き)
  ↓  pretty_expr()                — prettyplease で整形
String (最終出力)
```

## 実施計画

### Step 0: CodegenTypeContext トレイト導入

**目的**: macro/inline の型推論差異を抽象化し、後続ステップの統一を可能にする。

**変更**:
1. `CodegenTypeContext` トレイトを `syn_codegen.rs` に定義
2. `MacroTypeContext` を実装（`infer_expr_type`, `infer_type_hint` のロジックを移動）
3. `InlineTypeContext` を実装（`infer_expr_type_inline`, `is_pointer_expr_inline` のロジックを移動）
4. 既存の `expr_to_rust_ctx` / `expr_to_rust_inline_ctx` 内部で
   トレイト経由の呼び出しに段階的に切り替え

**規模**: 中（~300行の移動+リファクタリング）
**リスク**: 低（既存出力に影響なし、内部リファクタリングのみ）
**検証**: `cargo test` + 統合テスト（出力変更なし）

### Step 1: build_syn_expr — リテラル・単純式

**対象 ExprKind** (14 variants, 同一/類似):
- `Ident`, `IntLit`, `UIntLit`, `FloatLit`, `CharLit`, `StringLit`
- `Deref`, `AddrOf`, `UnaryPlus`, `BitNot`
- `Member`, `PtrMember`
- `Comma`, `SizeofType`

**方法**:
1. `build_syn_expr()` メソッドを `RustCodegen` に追加
2. 上記 variants を syn::Expr として構築
3. 未対応 variants は `syn::parse_str(expr_to_rust_ctx(...))` でフォールバック
4. テスト: 個別の ExprKind に対する単体テスト

**規模**: 中（~200行の新規コード）
**リスク**: 低（フォールバック付き）

### Step 2: build_syn_expr — 意味変換付き式

**対象 ExprKind** (5 variants, 差異あり):
- `Cast` — void/bool/enum 分岐、ExprContext 削除
- `UnaryMinus` — unsigned 型の wrapping_neg
- `LogNot` — bool 変換統合
- `Sizeof` — literal_string_param 最適化
- `Index` — pointer offset 変換

**方法**:
1. 各 variant の意味変換ロジックを `CodegenTypeContext` 経由で統一
2. syn::Expr を構築（括弧は parenthesize に委譲）

**規模**: 中（~150行）
**リスク**: 中（意味変換の正確性を検証必要）
**検証**: 各 variant に対する比較テスト（旧出力 vs 新出力）

### Step 3: build_syn_expr — Binary 演算

**対象**: `ExprKind::Binary` (macro: 221行, inline: 202行)

Binary は最も複雑な arm で、以下の意味変換を含む:
1. sizeof(param) - 1 → param.len() (マクロのみ)
2. pointer == null → .is_null()
3. bool == 0/1 → bool 最適化
4. pointer ± integer → .offset()
5. pointer - pointer → .offset_from()
6. float vs int → リテラル変換
7. LogAnd/LogOr → bool 変換
8. 整数幅不一致 → as キャスト
9. bool → integer キャスト

**方法**:
1. 各意味変換を独立メソッドに分離:
   - `try_sizeof_optimization()` → Option<syn::Expr>
   - `try_null_comparison()` → Option<syn::Expr>
   - `try_bool_simplification()` → Option<syn::Expr>
   - `try_pointer_arithmetic()` → Option<syn::Expr>
   - `build_basic_binary()` → syn::Expr (キャスト挿入含む)
2. `build_binary()` はこれらを順に試行し、最初に成功した結果を返す

**規模**: 大（~250行、最も複雑）
**リスク**: 高（多数の意味変換の正確性）
**検証**: Binary の全パターンに対する比較テスト

### Step 4: build_syn_expr — Call・MacroCall

**対象**: `ExprKind::Call` (93行), `ExprKind::MacroCall` (22行), `ExprKind::BuiltinCall` (26行)

**意味変換**:
- `__builtin_expect` → 引数を透過
- `__builtin_unreachable` → `unreachable_unchecked()`
- `__builtin_ctz/clz` → `.trailing_zeros()` / `.leading_zeros()`
- `ASSERT_IS_LITERAL` 等 → 引数を透過
- THX injection (`my_perl` パラメータ追加)
- ジェネリック型パラメータ → turbofish 構文
- `offsetof` → `std::mem::offset_of!`
- 引数のキャスト（`expr_to_rust_arg` 相当）

**方法**:
1. builtin 関数ディスパッチを共通テーブルに
2. `build_call_args()` で引数の型キャスト・null 変換を統合
3. MacroCall は `should_emit_as_macro_call` で分岐

**規模**: 中（~150行）
**リスク**: 中（THX injection とジェネリクスの正確性）

### Step 5: build_syn_expr — 文 (Assign, Inc/Dec, StmtExpr, Assert)

**対象**:
- `Assign` (macro: 89行, inline: 54行)
- `PreInc/PreDec/PostInc/PostDec` (各 ~15行 × 4)
- `StmtExpr` (~40行)
- `Assert` (~20行)

**方法**:
1. syn::Block を使ってブロック式を構築
2. Assign の型キャスト挿入を `CodegenTypeContext` 経由で統一
3. Pre/PostInc の pointer 判定を統一
4. lvalue 展開 (`try_expand_call_as_lvalue`) を統一

**規模**: 中（~200行）
**リスク**: 中（ブロック式の整形）

### Step 6: build_syn_stmt — 文の統合

**対象**: `stmt_to_rust()` (37行) + `stmt_to_rust_inline()` (293行)

**方法**:
1. `build_syn_stmt()` を作成、CodegenTypeContext 経由で統一
2. `return` 文、`if` 文、`for` 文、`while` 文、`switch` 文を syn::Stmt で構築
3. `stmt_to_rust` / `stmt_to_rust_inline` を `build_syn_stmt` 経由に切り替え

**規模**: 大（~300行）
**リスク**: 中（inline 関数の文は複雑）

### Step 7: 切り替え・旧方式削除

**対象**: 全呼び出し元を `build_syn_expr` に切り替え

1. `generate_macro` の Expression パス:
   `build_syn_expr` → `parenthesize` → `pretty_expr` → writeln
2. `generate_inline_fn` の本体生成:
   `build_syn_stmt` → syn::Block → prettyplease
3. 旧コード削除:
   - `expr_to_rust_ctx` / `expr_to_rust_inline_ctx` (~1529行)
   - `stmt_to_rust` / `stmt_to_rust_inline` (~330行)
   - `infer_type_hint` / `is_pointer_expr_inline` (~200行)
   - `ExprContext` enum, `strip_outer_parens`, `normalize_parens`
   - `wrap_as_bool_condition_macro` / `wrap_as_bool_condition_inline`

**規模**: 中（削除主体）
**リスク**: 低（Step 1-6 で段階的に検証済み）

## 検証戦略

### 各 Step の検証

```bash
# 1. 単体テスト
cargo test

# 2. 統合テスト（出力比較）
cargo run -- samples/xs-wrapper.h --auto --gen-rust --bindings samples/bindings.rs > /tmp/new.rs
diff /tmp/baseline.rs /tmp/new.rs

# 3. ビルドテスト（エラー数・警告数）
~/blob/libperl-rs/12-macrogen-2-build.zsh
grep -c '^error' tmp/build-error.log
grep -c 'remove these parentheses' tmp/build-error.log
```

### 出力差分の許容範囲

syn::Expr 移行では以下の出力変更が期待される:
- 不要な括弧の除去（改善）
- `prettyplease` による整形差異（スペース、改行位置）

意味の変わる変更は不許容。差分は全て手動レビュー。

## 規模見積もり

| Step | 新規コード | 削除コード | 純変更 |
|------|-----------|-----------|--------|
| 0 | ~300行 | 0 | +300 |
| 1 | ~200行 | 0 | +200 |
| 2 | ~150行 | 0 | +150 |
| 3 | ~250行 | 0 | +250 |
| 4 | ~150行 | 0 | +150 |
| 5 | ~200行 | 0 | +200 |
| 6 | ~300行 | 0 | +300 |
| 7 | ~50行 | ~2100行 | -2050 |
| **合計** | **~1600行** | **~2100行** | **-500** |

最終的に rust_codegen.rs は ~6439 → ~4900行（-24%）に縮小。
重複コードが統一され、保守性が大幅に向上する。

## 期待効果

- 括弧 warning 46件 → 0件（完全解消）
- macro/inline のコード重複 ~2200行 → 0行
- `ExprContext`, `strip_outer_parens` 等の括弧制御コード完全廃止
- 型キャスト挿入が AST レベルで正確に
- 新しい ExprKind 追加時に 1 箇所のみ変更（現在 2 箇所）
