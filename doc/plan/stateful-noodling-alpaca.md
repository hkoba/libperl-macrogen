# Plan: build_syn_expr 4つの問題の修正

## Context

`--use-syn-expr` で 128→202 (+74) エラーが発生する。
原因は 4 カテゴリに分類される。根本原因は「syn::Expr → 文字列化 →
文字列レベルの cast 挿入 → normalize_parens」の過程で括弧が崩壊すること。

## 問題 1 & 4（同根）: 返り値 cast の優先順位崩壊 (~50件)

### 原因

`generate_macro` の Expression パス:
```
build_syn_expr → expr_to_string (正しく括弧付き)
  → cast_return_expr_if_needed → "(expr as i32)" (文字列)
  → normalize_parens → "expr as i32" (括弧消失!)
```

`as` は `|`, `<<`, `^` より高優先のため、
`a | b << 8 as i32` = `a | b << (8 as i32)` になる。

### 修正方針

**cast を syn::Expr レベルで行う**。
`cast_return_expr_if_needed` が文字列を返す代わりに、
syn::Expr を返す版を作り、`build_syn_expr` の結果に直接 Cast を挿入する。
その後 `expr_to_string` (parenthesize 付き) で文字列化すれば括弧は正確になる。

### 具体的変更

**ファイル**: `src/rust_codegen.rs`

1. `cast_return_syn_expr_if_needed` メソッドを追加:
```rust
fn cast_return_syn_expr_if_needed(&self, expr: &Expr,
    info: Option<&MacroInferInfo>, syn_expr: syn::Expr) -> syn::Expr {
    if let Some(ret_ut) = &self.current_return_type {
        if let Some(expr_ut) = self.infer_expr_type_unified(expr, info) {
            let ret_s = ret_ut.to_rust_string();
            let expr_s = expr_ut.to_rust_string();
            if let (Some(nr), Some(ne)) = (...) {
                if !integer_types_compatible(nr, ne) {
                    return cast_syn_expr(syn_expr, nr); // syn::Expr::Cast を構築
                }
            }
        }
    }
    syn_expr // 変更なし
}
```

2. `generate_macro` Expression パスを変更:
```rust
// 旧: cast_return_expr_if_needed → normalize_parens (文字列)
// 新: cast_return_syn_expr_if_needed (syn::Expr) → expr_to_string
let syn_expr = self.build_syn_expr_with_type_hint(...);
let syn_expr = self.cast_return_syn_expr_if_needed(expr, Some(info), syn_expr);
let rust_expr = expr_to_string(&syn_expr);  // parenthesize 付き
self.writeln(&format!("{}{}", body_indent, normalize_parens(&rust_expr)));
```

**検証**: `packWARN2` が `(a | b << 8) as i32` を生成すること。

## 問題 2: Assign 内の MacroCall が `0` に展開される (~12件)

### 原因

`build_lvalue_string` の MacroCall 分岐 (L3769-3771):
```rust
ExprKind::MacroCall { expanded, .. } => {
    let syn_expr = self.build_syn_expr(expanded, info);
    // expanded はマクロの展開結果で、マクロ呼び出し形式ではない
}
```

`expanded` は型推論用の「完全展開済み式」で、内部にネストしたマクロ呼び出し
(HEK_KEY, HEK_LEN 等) が更に展開済み。`build_syn_expr` がこれを処理すると、
`should_emit_as_macro_call` で呼び出し形式にすべき子マクロが展開形式で
処理され、null pointer (IntLit(0)) に落ちる。

旧パスの `expr_to_rust(expanded, info)` は `expr_to_rust_ctx` を使い、
その中の MacroCall arm が `should_emit_as_macro_call` を正しく判定する。
`build_syn_expr` の MacroCall arm も同じ判定をしているが、`expanded` の
中のネストした MacroCall は ExprKind::MacroCall ではなく展開済みの式
(Call 等) として現れるため、`should_emit_as_macro_call` にヒットしない。

### 修正方針

`build_lvalue_string` はフォールバックパスではなく `build_syn_expr` を
使っている。Assign arm 全体をフォールバック (`expr_to_rust_ctx`) に
戻すのが最も安全。Assign はブロック式 `{ l = r; l }` を生成するため、
括弧の問題は発生しない（ブロックは自己完結）。

### 具体的変更

**ファイル**: `src/rust_codegen.rs`

`build_syn_expr` の Assign arm を削除し、フォールバック (`_` arm) に落とす。
同様に Pre/PostInc/Dec もフォールバックに戻す（同じ問題を抱える可能性）。

```rust
// build_syn_expr 内:
ExprKind::Assign { .. } | ExprKind::PreInc(_) | ExprKind::PreDec(_)
| ExprKind::PostInc(_) | ExprKind::PostDec(_) => {
    // ブロック式を生成するため、旧パスにフォールバック
    let fallback_str = match info {
        Some(info) => self.expr_to_rust_ctx(expr, info, ExprContext::Top),
        None => self.expr_to_rust_inline_ctx(expr, ExprContext::Top),
    };
    syn::parse_str(&fallback_str).unwrap_or_else(|_| int_lit(0))
}
```

**検証**: `HEK_UTF8` が `(*(HEK_KEY(hek) as ...).offset(...))` を生成すること。

## 問題 3: `true`/`false` が `r#true`/`r#false` になる (2件)

### 原因

`build_syn_expr` の Ident arm で `escape_rust_keyword("true")` → `"r#true"`。
旧パスでも同じだが、`expr_to_rust_arg` 内で bool パラメータなら `"true"` に
変換する。`build_arg_string_unified` にはこの処理があるが、
Ident arm 自体で `true`/`false` を bool リテラルに変換していない。

### 修正方針

`build_syn_expr` の Ident arm で、名前が `"true"` または `"false"` の場合は
syn::Expr::Lit(Bool) として返す。

### 具体的変更

**ファイル**: `src/rust_codegen.rs`

`build_syn_expr` の Ident arm、`escape_rust_keyword` 呼び出しの前に追加:

```rust
ExprKind::Ident(name) => {
    // ...既存のパラメータ置換、libc記録、未解決チェック...
    let name_str = self.interner.get(*name);
    // true/false は Rust の bool リテラルとして出力
    if name_str == "true" || name_str == "false" {
        return syn::parse_str(name_str).unwrap();
    }
    // ...残りの処理...
}
```

**検証**: `Perl_resume_compcv(..., true)` が `r#true` でなく `true` になること。

## 実施順序

| 順序 | 修正 | 影響件数 | 理由 |
|------|------|---------|------|
| 1 | 問題 3 (true/false) | 2件 | 最も単純、副作用なし |
| 2 | 問題 2 (Assign フォールバック) | ~12件 | Assign/Inc/Dec を旧パスに戻すだけ |
| 3 | 問題 1&4 (cast を syn レベルに) | ~50件 | 最大効果、設計変更あり |

各修正後に `cargo test` + 統合テスト (`--use-syn-expr`) で検証。

## 検証

```bash
cargo test

# 旧パス（ベースライン）
cargo run -- samples/xs-wrapper.h --auto --gen-rust --bindings samples/bindings.rs \
  2>/dev/null > /tmp/old.rs

# 新パス
cargo run -- samples/xs-wrapper.h --auto --gen-rust --bindings samples/bindings.rs \
  --use-syn-expr 2>/dev/null > /tmp/new.rs

# 差分確認
diff /tmp/old.rs /tmp/new.rs | grep '^[<>]' | grep -v '^[<>] //' | wc -l

# 統合ビルド
~/blob/libperl-rs/12-macrogen-2-build.zsh  # (--use-syn-expr はビルドスクリプトに要追加)
```
