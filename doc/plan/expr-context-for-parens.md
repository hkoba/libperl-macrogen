# Plan: 式生成にコンテキスト情報を渡して括弧制御を行う

## 背景

残り 140 件の `unnecessary parentheses` warning のうち、
主要なパターンは「関数引数内の `(expr as T)` に不要な外側括弧」。

```rust
// 現状:
SvTYPE((sv as *mut SV))    // ← (sv as *mut SV) の外側括弧が不要
isREGEXP((re as *mut SV))  // ← 同上
```

## 現状の問題

`expr_to_rust` / `expr_to_rust_inline` は常に `String` を返し、
呼び出し元のコンテキスト情報を受け取らない。
Cast 式は `format!("({} as {})", e, t)` で**常に括弧付き**を返す。

Rust の `as` は優先順位が低いため、多くの場面で括弧が必要:
- `*ptr as T` → `(*ptr) as T` ではなく `*(ptr as T)` と解釈される
- `a + b as T` → `a + (b as T)` と解釈される

しかし以下のコンテキストでは括弧不要:
- **関数引数**: `f(expr as T)` — `,` で区切られるため
- **let 初期化子**: `let x = expr as T;` — `;` で終端
- **return 値**: `return expr as T;` — `;` で終端
- **代入 RHS**: `x = expr as T;` — `;` で終端
- **if 条件内の比較**: `if expr as T == val` — `as` は `==` より優先

## 設計

### アプローチ: コンテキスト enum を `expr_to_rust` に渡す

```rust
/// 式が生成されるコンテキスト（括弧制御用）
#[derive(Clone, Copy, PartialEq, Eq)]
enum ExprContext {
    /// デフォルト: 括弧が必要な可能性がある場所
    /// Binary 式の一部、Deref の中身、etc.
    Default,
    /// 括弧不要のトップレベル位置
    /// 関数引数、let 初期化子、return 値、代入 RHS
    TopLevel,
}
```

### 変更するシグネチャ

```rust
// Before
fn expr_to_rust(&mut self, expr: &Expr, info: &MacroInferInfo) -> String
fn expr_to_rust_inline(&mut self, expr: &Expr) -> String

// After
fn expr_to_rust(&mut self, expr: &Expr, info: &MacroInferInfo, ctx: ExprContext) -> String
fn expr_to_rust_inline(&mut self, expr: &Expr, ctx: ExprContext) -> String
```

### Cast 式の変更

```rust
ExprKind::Cast { type_name, expr: inner } => {
    let e = self.expr_to_rust(inner, info, ExprContext::Default);
    let t = self.type_name_to_rust(type_name);
    if ctx == ExprContext::TopLevel {
        format!("{} as {}", e, t)
    } else {
        format!("({} as {})", e, t)
    }
}
```

### Binary 式の変更

```rust
ExprKind::Binary { op, lhs, rhs } => {
    let l = self.expr_to_rust(lhs, info, ExprContext::Default);
    let r = self.expr_to_rust(rhs, info, ExprContext::Default);
    if ctx == ExprContext::TopLevel {
        format!("{} {} {}", l, bin_op_to_rust(*op), r)
    } else {
        format!("({} {} {})", l, bin_op_to_rust(*op), r)
    }
}
```

### 呼び出し側の変更

```rust
// 関数引数: TopLevel
self.expr_to_rust(arg, info, ExprContext::TopLevel)

// Binary 式のオペランド: Default
self.expr_to_rust(lhs, info, ExprContext::Default)

// let 初期化子: TopLevel
self.expr_to_rust_inline(expr, ExprContext::TopLevel)

// return 値: TopLevel
self.expr_to_rust_inline(expr, ExprContext::TopLevel)
```

### コンテキストチェーンは不要

Lisp の cons 的な環境チェーン（car に直近の文脈、cdr に外側への参照）を
検討したが、括弧制御の判断に必要な情報は **直接の親の構文**のみ:

- 親が関数引数 → `as` に括弧不要
- 親が二項演算子 → `as` の優先順位次第で括弧要/不要
- 親が return / let RHS → 括弧不要

Rust の括弧の要否は構文的に局所的（直接の親演算子のみに依存）なので、
「親の親」を遡るケースはない。単純な enum で十分。

将来、優先順位ベースの制御が必要になった場合は:
```rust
enum ExprContext {
    Top,
    BinOp { parent_prec: u8 },
}
```
のように拡張可能。初期実装は `Top` / `Default` の 2 値。

## 実装の影響範囲

### 大きな変更

`expr_to_rust` と `expr_to_rust_inline` の全呼び出し箇所に
`ctx` 引数を追加する必要がある。呼び出し箇所は多い:

```
$ grep -c 'self.expr_to_rust(' src/rust_codegen.rs
→ 約 60 箇所
$ grep -c 'self.expr_to_rust_inline(' src/rust_codegen.rs
→ 約 80 箇所
```

### 安全な移行手順

1. `ExprContext` enum を定義
2. `expr_to_rust` / `expr_to_rust_inline` にデフォルト引数的なラッパーを追加:
   ```rust
   fn expr_to_rust(&mut self, expr: &Expr, info: &MacroInferInfo) -> String {
       self.expr_to_rust_ctx(expr, info, ExprContext::Default)
   }
   fn expr_to_rust_ctx(&mut self, expr: &Expr, info: &MacroInferInfo, ctx: ExprContext) -> String {
       // 本体
   }
   ```
3. 呼び出し側を段階的に `expr_to_rust_ctx(..., ExprContext::TopLevel)` に変更
4. `strip_outer_parens` の使用箇所を `ExprContext::TopLevel` に置き換え

### `strip_outer_parens` との共存

当面は `strip_outer_parens` と `ExprContext` を併用:
- `ExprContext::TopLevel` で Cast/Binary の外側括弧を抑制
- `strip_outer_parens` は既存の箇所に残す（段階的に除去）

将来的には `ExprContext` だけで括弧制御し、`strip_outer_parens` を廃止。

## 期待効果

残り 140 件の warning のうち、function argument (32件) + assigned value の一部 +
if/while condition の一部で括弧が除去される。50-80件の追加削減を見込む。

## 実装順序

1. `ExprContext` enum 定義
2. `expr_to_rust_ctx` / `expr_to_rust_inline_ctx` ラッパー追加
3. Cast / Binary 式で `ctx` による括弧制御
4. 関数引数の呼び出し側を `TopLevel` に変更
5. テスト・regression 確認
