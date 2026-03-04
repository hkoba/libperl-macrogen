# Plan: E0308 Phase 2 — `__builtin_expect` bool 二重ラップ + `is_null_literal` Cast 対応

## Context

Phase 1 (完了) で E0308 を 687→676 に削減した。
残り 676 件の E0308 エラーのうち、最大カテゴリは **"expected bool, found integer" (91 件)** 。

生成コードを分析すると、二重ラップパターンが主原因:

```rust
// 例1: 比較結果に不要な != 0 が付く
if (((rc > 1)) != 0) {          // ← rc > 1 は既に bool

// 例2: ポインタ比較が is_null() に変換されず、さらに != 0
if (((sv != (0 as *mut c_void))) != 0) {  // ← 二重問題

// 例3: bool 変数に != 0
if ((sv_2bool_is_fallback) != 0) {        // ← bool なのに != 0
```

### 根本原因

**原因 A: `__builtin_expect` の AST/文字列不一致**

`expr_to_rust_inline()` の `__builtin_expect` ハンドラは
`args[0]` の codegen 文字列を返す（`Call` を剥がす）。
しかし `wrap_as_bool_condition_inline()` は **元の AST**（`Call { __builtin_expect, ... }`）
を見るため、`is_boolean_expr(Call{...})` が `false` を返し、`!= 0` を追加してしまう。

C コード: `if (PERL_LIKELY(rc > 1))`
→ 展開: `if (__builtin_expect((rc > 1), 1))`
→ AST: `Call { __builtin_expect, [Binary{Gt, rc, 1}, IntLit(1)] }`
→ `expr_to_rust_inline()`: `(rc > 1)` (内部式を返す)
→ `wrap_as_bool_condition_inline(Call{...}, "(rc > 1)")`:
  - `is_boolean_expr(Call{...})` → **false** ← ここが問題
  - → `(((rc > 1)) != 0)` ← 二重ラップ

**原因 B: `is_null_literal()` が Cast を処理しない**

`NULL` は `((void*)0)` に展開され、AST は `Cast { IntLit(0), *mut c_void }`。
`is_null_literal()` は `IntLit(0)` のみチェックするため Cast を見逃す。
→ `ptr != (0 as *mut c_void)` のまま出力（`!ptr.is_null()` に変換されない）

**原因 C: bool 型変数が検出されない**

`wrap_as_bool_condition_inline()` は `is_pointer_expr_inline()` でポインタを検出するが、
bool 型変数を検出するロジックがない。
Phase 1 で `current_param_types` にローカル変数の型を登録済みなので、
`Ident(name)` の型が `"bool"` ならスキップできる。

## 変更内容

### 変更 1: `is_null_literal()` に Cast 再帰を追加

`src/rust_codegen.rs` L357-359

```rust
// 変更前
fn is_null_literal(expr: &Expr) -> bool {
    matches!(expr.kind, ExprKind::IntLit(0))
}

// 変更後
fn is_null_literal(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::IntLit(0) => true,
        ExprKind::Cast { expr: inner, .. } => is_null_literal(inner),
        _ => false,
    }
}
```

**効果**: `ptr != ((void*)0)` → `!ptr.is_null()` がマクロ・inline 両方で効く。
Phase 1 で追加した Eq/Ne ハンドラ（inline L2600, macro L1318）が自動的に対応。

### 変更 2: `wrap_as_bool_condition_inline()` に `__builtin_expect` 再帰を追加

`src/rust_codegen.rs` L754-765

```rust
// 変更後
fn wrap_as_bool_condition_inline(&self, expr: &Expr, expr_str: &str) -> String {
    if is_boolean_expr(expr) {
        return expr_str.to_string();
    }
    // __builtin_expect(cond, val) → cond の型をチェック
    if let ExprKind::Call { func, args, .. } = &expr.kind {
        if let ExprKind::Ident(name) = &func.kind {
            if self.interner.get(*name) == "__builtin_expect" && !args.is_empty() {
                return self.wrap_as_bool_condition_inline(&args[0], expr_str);
            }
        }
    }
    // bool 型変数の検出
    if let ExprKind::Ident(name) = &expr.kind {
        if let Some(ty) = self.current_param_types.get(name) {
            if ty == "bool" {
                return expr_str.to_string();
            }
        }
    }
    if expr_str.ends_with(" as bool)") || expr_str.ends_with("!= 0)") || expr_str.ends_with(".is_null()") {
        return expr_str.to_string();
    }
    if self.is_pointer_expr_inline(expr) {
        return format!("!{}.is_null()", expr_str);
    }
    format!("(({}) != 0)", expr_str)
}
```

再帰により、`__builtin_expect` の内部式で `is_boolean_expr()` / `is_pointer_expr_inline()` が
正しく評価される。

### 変更 3: `wrap_as_bool_condition_macro()` にも同じ `__builtin_expect` 再帰を追加

`src/rust_codegen.rs` L740-751

```rust
// 変更後
fn wrap_as_bool_condition_macro(&self, expr: &Expr, expr_str: &str, info: &MacroInferInfo) -> String {
    if is_boolean_expr(expr) {
        return expr_str.to_string();
    }
    // __builtin_expect(cond, val) → cond の型をチェック
    if let ExprKind::Call { func, args, .. } = &expr.kind {
        if let ExprKind::Ident(name) = &func.kind {
            if self.interner.get(*name) == "__builtin_expect" && !args.is_empty() {
                return self.wrap_as_bool_condition_macro(&args[0], expr_str, info);
            }
        }
    }
    if expr_str.ends_with(" as bool)") || expr_str.ends_with("!= 0)") || expr_str.ends_with(".is_null()") {
        return expr_str.to_string();
    }
    if self.infer_type_hint(expr, info) == TypeHint::Pointer {
        return format!("!{}.is_null()", expr_str);
    }
    format!("(({}) != 0)", expr_str)
}
```

### 変更の相乗効果

| 生成コード (変更前) | 変更 1 のみ | 変更 2+3 のみ | 全変更 |
|---|---|---|---|
| `if (((sv != (0 as *mut c_void))) != 0)` | `if (!sv.is_null()) != 0)` ※string check で回避 | `if (sv != (0 as *mut c_void))` | `if !sv.is_null()` |
| `if (((rc > 1)) != 0)` | 変化なし | `if (rc > 1)` | `if (rc > 1)` |
| `if ((sv_2bool_is_fallback) != 0)` | 変化なし | `if sv_2bool_is_fallback` | `if sv_2bool_is_fallback` |

## 変更ファイル

| ファイル | 変更箇所 |
|----------|----------|
| `src/rust_codegen.rs` | `is_null_literal()` L357-359: Cast 再帰追加 |
| `src/rust_codegen.rs` | `wrap_as_bool_condition_inline()` L754-765: `__builtin_expect` 再帰 + bool 変数検出 |
| `src/rust_codegen.rs` | `wrap_as_bool_condition_macro()` L740-751: `__builtin_expect` 再帰 |

## 検証

```bash
# 1. 全テスト通過
cargo test

# 2. gen-rust stats が悪化しないこと
cargo run -- samples/xs-wrapper.h --auto --gen-rust --bindings samples/bindings.rs 2>&1 | tail -5

# 3. 統合ビルドテスト
~/blob/libperl-rs/12-macrogen-2-build.zsh
grep -c 'error\[E0308\]' tmp/build-error.log
# 期待: 676 から減少（91件の "expected bool" が大幅に減少）

# 4. 二重ラップパターンの確認
grep -c '!= 0)' tmp/macro_bindings.rs
# 期待: 261 から大幅に減少
```
