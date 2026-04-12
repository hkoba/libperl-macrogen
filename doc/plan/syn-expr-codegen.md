# Plan: syn::Expr による Rust コード生成への移行

## 目的

コード生成を文字列ベースから `syn::Expr` AST ベースに移行し、
括弧制御とキャスト挿入の問題を根本解決する。

## 現状の問題（まとめ）

| 問題 | 原因 | 文字列ベースでの限界 |
|------|------|---------------------|
| 不要な括弧 | 生成時に括弧を付け、後で strip | strip しすぎ/不足 |
| 必要な括弧の欠落 | 後付け `!= 0`/`as T` で文脈が変わる | 文字列では検知不可 |
| キャスト挿入 | 文字列を `format!("({} as {})")` で加工 | 優先順位が壊れうる |

## 設計

### 全体フロー

```
C AST (crate::ast::Expr)
  ↓  build_syn_expr()        ← 新規: C AST → syn::Expr 変換
syn::Expr (Rust AST)
  ↓  transform passes         ← 新規: キャスト挿入、bool 変換等
syn::Expr (変換済み)
  ↓  parenthesize()           ← 新規: 優先順位に基づく括弧挿入
syn::Expr (括弧付き)
  ↓  prettyplease::unparse()  or  ToTokens → String
String (最終出力)
```

### 依存関係

```toml
# 既存
syn = { version = "2.0", features = ["full", "parsing"] }
quote = "1.0"

# 追加
proc-macro2 = "1.0"       # TokenStream 操作
prettyplease = "0.2"       # syn::File → 整形済み文字列
```

### Phase 1: 基盤モジュールの作成

#### 新ファイル: `src/syn_codegen.rs`

```rust
use syn::*;
use quote::quote;

/// C AST の式を syn::Expr に変換する
pub struct SynExprBuilder<'a> {
    interner: &'a StringInterner,
    // 型情報、マクロ情報等への参照
}

impl<'a> SynExprBuilder<'a> {
    /// C AST の Expr を syn::Expr に変換
    pub fn build_expr(&self, expr: &crate::ast::Expr) -> syn::Expr {
        match &expr.kind {
            ExprKind::Ident(name) => {
                let name_str = self.interner.get(*name);
                let ident = syn::Ident::new(
                    &escape_rust_keyword(name_str),
                    proc_macro2::Span::call_site(),
                );
                parse_quote!(#ident)
            }
            ExprKind::IntLit(n) => {
                let lit = syn::LitInt::new(&n.to_string(), proc_macro2::Span::call_site());
                parse_quote!(#lit)
            }
            ExprKind::Binary { op, lhs, rhs } => {
                let l = self.build_expr(lhs);
                let r = self.build_expr(rhs);
                let op = to_syn_binop(*op);
                parse_quote!(#l #op #r)
            }
            ExprKind::Cast { type_name, expr: inner } => {
                let e = self.build_expr(inner);
                let ty: syn::Type = syn::parse_str(&self.type_name_to_rust(type_name))
                    .unwrap_or_else(|_| parse_quote!(c_int));
                parse_quote!(#e as #ty)
            }
            ExprKind::Deref(inner) => {
                let e = self.build_expr(inner);
                parse_quote!(*#e)
            }
            ExprKind::Member { expr: base, member } => {
                let b = self.build_expr(base);
                let m = syn::Ident::new(
                    self.interner.get(*member),
                    proc_macro2::Span::call_site(),
                );
                parse_quote!(#b.#m)
            }
            // ... 他の ExprKind
        }
    }
}
```

### Phase 2: 括弧挿入パス

`syn::Expr` は括弧を自動挿入しない。
式木を走査して、優先順位に基づき `Expr::Paren` を挿入するパスを実装。

```rust
/// Rust 式に必要な括弧を挿入する
pub fn parenthesize(expr: syn::Expr) -> syn::Expr {
    // 子式を再帰的に処理し、親子の優先順位に基づき Paren を挿入
    match expr {
        Expr::Binary(mut binary) => {
            let parent_prec = binop_precedence(&binary.op);
            binary.left = Box::new(
                maybe_paren(parenthesize(*binary.left), parent_prec, Position::Left)
            );
            binary.right = Box::new(
                maybe_paren(parenthesize(*binary.right), parent_prec, Position::Right)
            );
            Expr::Binary(binary)
        }
        Expr::Cast(mut cast) => {
            // as は Binary より高優先: 子が Binary なら括弧必要
            cast.expr = Box::new(
                maybe_paren_for_cast(parenthesize(*cast.expr))
            );
            Expr::Cast(cast)
        }
        Expr::Unary(mut unary) => {
            // 単項は as より高優先
            unary.expr = Box::new(
                maybe_paren_for_unary(parenthesize(*unary.expr))
            );
            Expr::Unary(unary)
        }
        // If/MethodCall/Field 等: 子に Binary/Cast があれば括弧
        _ => expr,
    }
}

fn maybe_paren(expr: syn::Expr, parent_prec: u8, pos: Position) -> syn::Expr {
    let child_prec = expr_precedence(&expr);
    if child_prec < parent_prec {
        // 子の優先順位が低い → 括弧必要
        Expr::Paren(ExprParen {
            attrs: vec![],
            paren_token: syn::token::Paren::default(),
            expr: Box::new(expr),
        })
    } else {
        expr
    }
}
```

### Phase 3: AST レベルの変換パス

文字列後付け処理を AST 変換に置き換える。

#### bool 変換 (`!= 0`)

```rust
fn wrap_as_bool(expr: syn::Expr) -> syn::Expr {
    if is_bool_expr(&expr) {
        expr  // 既に bool → そのまま
    } else if is_pointer_expr(&expr) {
        parse_quote!(!#expr.is_null())  // ポインタ → is_null
    } else {
        parse_quote!(#expr != 0)  // 整数 → != 0（括弧は parenthesize で付く）
    }
}
```

#### キャスト挿入

```rust
fn insert_cast(expr: syn::Expr, target_ty: &syn::Type) -> syn::Expr {
    parse_quote!(#expr as #target_ty)
    // 括弧は parenthesize パスで自動挿入
}
```

#### null ポインタ変換

```rust
fn null_for_type(ty: &UnifiedType) -> syn::Expr {
    if ty.is_const_pointer() {
        parse_quote!(std::ptr::null())
    } else if ty.is_pointer() {
        parse_quote!(std::ptr::null_mut())
    } else {
        parse_quote!(0)
    }
}
```

### Phase 4: 出力

```rust
fn expr_to_string(expr: syn::Expr) -> String {
    let parenthesized = parenthesize(expr);
    let tokens = quote!(#parenthesized);
    tokens.to_string()
    // または prettyplease で整形
}
```

### Phase 5: 段階的移行

`expr_to_rust_ctx` を段階的に `build_syn_expr` に置き換え。
共存のため、`syn::Expr` → `String` の変換を随時行い、
既存の `String` ベースのコードと接続。

```rust
fn expr_to_rust_ctx(&mut self, expr: &Expr, info: &MacroInferInfo, ctx: ExprContext) -> String {
    // 新方式: syn::Expr を構築して文字列化
    if self.use_syn_codegen {
        let syn_expr = self.build_syn_expr(expr, info);
        return expr_to_string(syn_expr);
    }
    // 旧方式: 直接文字列生成（段階的に廃止）
    match &expr.kind {
        // ...
    }
}
```

## 移行スケジュール

| Phase | 内容 | 規模 | 影響 |
|-------|------|------|------|
| 1 | `syn_codegen.rs` 作成、基本 ExprKind 変換 | 中 | なし（新モジュール） |
| 2 | `parenthesize()` 実装 | 小 | なし |
| 3 | AST 変換パス（bool, cast, null） | 中 | なし |
| 4 | `generate_macro` を syn 方式に移行 | 大 | 出力変更 |
| 5 | `generate_inline_fn` を syn 方式に移行 | 大 | 出力変更 |
| 6 | 旧方式を廃止、`strip_outer_parens`/`ExprContext` 削除 | 中 | コード削減 |

Phase 1-3 は既存コードに影響なし。Phase 4-5 で段階的に出力が変わる。

## 期待効果

- 括弧の warning 41+ 件が完全解消
- `strip_outer_parens`, `ExprContext`, 文字列レベルの括弧制御が全て不要に
- キャスト挿入が AST レベルで正確に
- `!= 0` 後付けによるパースエラーが原理的に排除
- コード生成の保守性が大幅向上

## リスク

- `parse_quote!` のコンパイル時コストが増加する可能性
- `syn::Expr` 構築の冗長性（`quote!` で緩和可能）
- 段階的移行中の二重メンテナンス期間
- `prettyplease` の出力が `rustfmt` と微妙に異なる可能性
