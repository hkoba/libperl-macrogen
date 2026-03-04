# Plan: E0308 Phase 3 — bool 冗長比較の除去 + 関数引数の bool 変換

## Context

Phase 2 (完了) で E0308 を 676→652 に削減（total: 969→944）。
残り 652 件のうち「expected bool, found integer」が依然 ~93 件で最大カテゴリ。

Phase 2 は `wrap_as_bool_condition_*()` の `__builtin_expect` 再帰と
`is_null_literal()` の Cast 対応を修正したが、以下の 2 パターンが残存:

### パターン A: AST 内の冗長 `!= 0` 比較 (~79 件)

C では比較演算子は `int` を返すため `(rc > 1) != 0` は合法。
しかし Rust では比較演算子は `bool` を返すため `bool != 0` は型エラー。

```rust
// 例1: 比較結果に != 0
if (((rc > 1)) != 0) {          // ← (rc > 1) は Rust では bool

// 例2: is_null() 結果に != 0
if ((!sv.is_null()) != 0) {     // ← !sv.is_null() は bool

// 例3: LogNot 結果に != 0
if (!(((flags & mask)) != 0))   // ← 内側の != 0 は OK だが、全体で二重
```

**根本原因**: `expr_to_rust()` / `expr_to_rust_inline()` の Binary Eq/Ne ハンドラは
ポインタ null チェック（L1349-1366, L2627-2644）の後、一般的に
`format!("({} {} {})", l, op, r)` を出力する。LHS が既に bool を返す式でも
`!= 0` をそのまま出力してしまう。

### パターン B: 関数引数の整数→bool 不一致 (~14 件)

```rust
// Perl_SvTRUE_common の第3引数は bool だが、整数リテラル 1 を渡す
return Perl_SvTRUE_common(my_perl, sv, 1);  // ← expected bool, found integer
```

**根本原因**: `expr_to_rust_inline()` の Call ハンドラ (L2738) は引数を
型情報なしで変換する。`rust_decl_dict` に関数シグネチャ（`RustFn.params`）が
あるが、codegen から参照されていない。

## 変更内容

### 変更 1: `is_boolean_expr_recursive()` ヘルパー追加

`src/rust_codegen.rs` L314 の `is_boolean_expr()` の後に追加。
`__builtin_expect(cond, val)` を透過して内部式の bool 判定を行う。

```rust
/// is_boolean_expr の再帰版: __builtin_expect(cond, val) を透過する
fn is_boolean_expr_recursive(expr: &Expr, interner: &StringInterner) -> bool {
    if is_boolean_expr(expr) {
        return true;
    }
    if let ExprKind::Call { func, args } = &expr.kind {
        if let ExprKind::Ident(name) = &func.kind {
            if interner.get(*name) == "__builtin_expect" && !args.is_empty() {
                return is_boolean_expr_recursive(&args[0], interner);
            }
        }
    }
    false
}
```

### 変更 2: Binary Eq/Ne ハンドラに bool 冗長比較の除去を追加

`expr_to_rust()` (L1366 の後) と `expr_to_rust_inline()` (L2644 の後) の
両方で、ポインタ null チェックの**後**に以下を挿入:

```rust
// bool_expr != 0 → bool_expr, bool_expr == 0 → !bool_expr
if is_boolean_expr_recursive(lhs, self.interner) {
    match (&rhs.kind, op) {
        (ExprKind::IntLit(0), BinOp::Ne) => {
            return self.expr_to_rust(lhs, info); // そのまま
        }
        (ExprKind::IntLit(0), BinOp::Eq) => {
            let l = self.expr_to_rust(lhs, info);
            return format!("!{}", l); // 否定
        }
        (ExprKind::IntLit(1), BinOp::Eq) => {
            return self.expr_to_rust(lhs, info);
        }
        (ExprKind::IntLit(1), BinOp::Ne) => {
            let l = self.expr_to_rust(lhs, info);
            return format!("!{}", l);
        }
        _ => {}
    }
}
// 逆順 (0 != bool_expr) も同様
if is_boolean_expr_recursive(rhs, self.interner) {
    match (&lhs.kind, op) {
        (ExprKind::IntLit(0), BinOp::Ne) => {
            return self.expr_to_rust(rhs, info);
        }
        (ExprKind::IntLit(0), BinOp::Eq) => {
            let r = self.expr_to_rust(rhs, info);
            return format!("!{}", r);
        }
        (ExprKind::IntLit(1), BinOp::Eq) => {
            return self.expr_to_rust(rhs, info);
        }
        (ExprKind::IntLit(1), BinOp::Ne) => {
            let r = self.expr_to_rust(rhs, info);
            return format!("!{}", r);
        }
        _ => {}
    }
}
```

inline 版は `self.expr_to_rust_inline(lhs)` を使う。

**安全性**: ポインタ null チェックが先に来るため `ptr == 0` は `.is_null()` に
変換され、ここには到達しない。`(x & mask) != 0` は BitAnd が
`is_boolean_expr` に含まれないため、正しく `!= 0` のまま残る。

### 変更 3: `RustCodegen` に `rust_decl_dict` 参照を追加

`RustCodegen` 構造体 (L536-569) に新フィールド追加:

```rust
/// Rust 宣言辞書への参照（関数パラメータ型参照用）
rust_decl_dict: Option<&'a RustDeclDict>,
```

`RustCodegen::new()` (L596) に引数追加。
3 箇所の呼び出し元 (L3154, L3210, L3414) を更新:
`result.rust_decl_dict.as_ref()` を渡す。

### 変更 4: `get_callee_param_type()` メソッド + 引数 bool 変換

```rust
fn get_callee_param_type(&self, func_name: &str, arg_index: usize) -> Option<&str> {
    self.rust_decl_dict?.fns.get(func_name).and_then(|f| {
        f.params.get(arg_index).map(|p| p.ty.as_str())
    })
}
```

`expr_to_rust_arg()` (L940-952) に bool 変換を追加:

```rust
fn expr_to_rust_arg(&mut self, expr: &Expr, info: &MacroInferInfo,
                     callee: Option<InternedStr>, arg_index: usize) -> String {
    // 既存: literal_string チェック
    if let Some(name) = self.find_literal_string_ident(expr) { ... }

    // 追加: bool パラメータへの整数リテラル変換
    if let Some(callee_name) = callee {
        let func_name = self.interner.get(callee_name);
        if let Some(param_ty) = self.get_callee_param_type(func_name, arg_index) {
            if param_ty == "bool" {
                match &expr.kind {
                    ExprKind::IntLit(0) => return "false".to_string(),
                    ExprKind::IntLit(1) => return "true".to_string(),
                    _ => {}
                }
            }
        }
    }

    self.expr_to_rust(expr, info)
}
```

inline 版 (L2738) にも同様のロジックを追加:

```rust
// 変更前
a.extend(args.iter().map(|arg| self.expr_to_rust_inline(arg)));

// 変更後
a.extend(args.iter().enumerate().map(|(i, arg)| {
    let param_idx = i + arg_offset;
    if let Some(param_ty) = self.get_callee_param_type(&f, param_idx) {
        if param_ty == "bool" {
            match &arg.kind {
                ExprKind::IntLit(0) => return "false".to_string(),
                ExprKind::IntLit(1) => return "true".to_string(),
                _ => {}
            }
        }
    }
    self.expr_to_rust_inline(arg)
}));
```

## 変更ファイル

| ファイル | 変更箇所 |
|----------|----------|
| `src/rust_codegen.rs` | `is_boolean_expr_recursive()` 追加 (L314 後) |
| `src/rust_codegen.rs` | `expr_to_rust()` Binary Eq/Ne (L1366 後): bool 冗長比較除去 |
| `src/rust_codegen.rs` | `expr_to_rust_inline()` Binary Eq/Ne (L2644 後): 同上 |
| `src/rust_codegen.rs` | `RustCodegen` 構造体 + `new()`: `rust_decl_dict` フィールド追加 |
| `src/rust_codegen.rs` | `get_callee_param_type()` メソッド追加 |
| `src/rust_codegen.rs` | `expr_to_rust_arg()`: bool 変換追加 |
| `src/rust_codegen.rs` | `expr_to_rust_inline()` Call: bool 変換追加 |

## 検証

```bash
# 1. 全テスト通過
cargo test

# 2. gen-rust stats が悪化しないこと
cargo run -- samples/xs-wrapper.h --auto --gen-rust --bindings samples/bindings.rs 2>&1 | tail -5

# 3. 統合ビルドテスト
~/blob/libperl-rs/12-macrogen-2-build.zsh
grep -c 'error\[E0308\]' tmp/build-error.log
# 期待: 652 → ~559 (約 93 件減少)

# 4. bool 冗長パターンの確認
grep 'expected.*bool.*found.*integer' tmp/build-error.log | wc -l
# 期待: 大幅減少
```
