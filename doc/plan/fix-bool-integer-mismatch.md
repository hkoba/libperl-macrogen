# Plan: `expected bool, found integer` エラーの修正

## 問題

39件の `expected bool, found integer` エラー。2つの異なるパターンがある。

### パターン 1: bool を返すマクロ呼び出しに `!= 0` を付加 (多数)

```rust
// isREGEXP は bool を返すのに != 0 が付いている
assert!((isREGEXP((re as *const SV))) != 0);

// CxEVALBLOCK は bool なのに != 0
pub unsafe fn CxTRYBLOCK(c: ...) -> bool {
    ((CxEVALBLOCK(c)) != 0)  // bool != 0 → エラー
}
```

**原因**: `wrap_as_bool_condition` / `is_bool_expr_with_dict` が、
codegen で `bool` に変更されたマクロ関数の戻り値を認識できていない。
`is_bool_expr_with_dict` は `macro_ctx.macros.get(name).get_return_type()` を
参照するが、これは macro_infer 側の型（`c_int` 等）で、codegen 側の
bool override 情報（`bool_return_macros`）が反映されていない。

### パターン 2: bool 型パラメータに `!= 0` を付加 (少数)

```rust
// b は bool 型パラメータ
pub unsafe fn boolSV(..., b: bool) -> ... {
    (if ((b) != 0) { ... } else { ... })  // bool != 0 → エラー
}

// cbool は bool 型パラメータ
pub unsafe fn cBOOL(cbool: bool) -> bool {
    ((cbool) != 0)  // bool != 0 → エラー
}

// x は bool 型パラメータ
pub unsafe fn ASSUME(x: bool) -> () {
    (if ((x) != 0) { ... })  // bool != 0 → エラー
}
```

**原因**: `wrap_as_bool_condition` がパラメータの型を確認せず、
パラメータを整数扱いして `!= 0` を付加する。

### パターン 3: bool 型フィールドに `!= 0` を付加

```rust
// Itainting, Itainted は bool フィールド
assert!(((((*my_perl).Itainting) != 0) || ...));
if (((*my_perl).Itainted) != 0) { ... }
```

**原因**: `wrap_as_bool_condition` がフィールドの型を確認せず、
フィールドアクセスを整数扱いして `!= 0` を付加する。

---

## 修正方針

### 修正 1: `is_bool_expr_with_dict` に `bool_return_macros` 集合を渡す

`RustCodegen` に `bool_return_macros: HashSet<InternedStr>` を保持させ、
`is_bool_expr_with_dict` でこの集合を参照する。

```rust
fn is_bool_expr_with_dict(&self, expr: &Expr) -> bool {
    // ... 既存チェック ...
    // 自家生成マクロ: codegen で bool と判定されたものを優先チェック
    if let ExprKind::Call { func, .. } = &expr.kind {
        if let ExprKind::Ident(name) = &func.kind {
            if self.bool_return_macros.contains(name) {
                return true;
            }
            // ... 既存の macro_ctx チェック ...
        }
    }
    // MacroCall も同様
    if let ExprKind::MacroCall { name, .. } = &expr.kind {
        if self.bool_return_macros.contains(name) {
            return true;
        }
    }
    false
}
```

### 修正 2: パラメータが bool 型なら `!= 0` を付加しない

`wrap_as_bool_condition_macro` / `wrap_as_bool_condition_inline` で、
式が `Ident(param_name)` かつ `current_param_types` でそのパラメータが
`bool` 型なら、そのまま返す。

```rust
fn wrap_as_bool_condition_macro(&self, expr: &Expr, expr_str: &str, info: &MacroInferInfo) -> String {
    // パラメータが bool 型なら != 0 不要
    if let ExprKind::Ident(name) = &expr.kind {
        if let Some(ut) = self.current_param_types.get(name) {
            if ut.is_bool() {
                return expr_str.to_string();
            }
        }
    }
    // ... 既存ロジック ...
}
```

### 修正 3: bool フィールドの `!= 0` を省略

`wrap_as_bool_condition` で、フィールドアクセスの型が `bool` なら
`!= 0` を付加しない。`field_type_map` を参照して判定。

```rust
// Member/PtrMember のフィールド型が bool なら != 0 不要
if let ExprKind::Member { member, .. } | ExprKind::PtrMember { member, .. } = &expr.kind {
    let member_str = self.interner.get(*member);
    if let Some(ut) = self.field_type_map.get(member_str) {
        if ut.is_bool() {
            return expr_str.to_string();
        }
    }
}
```

---

## 変更箇所

| ファイル | 関数 | 変更内容 |
|----------|------|----------|
| `rust_codegen.rs` | `RustCodegen` struct | `bool_return_macros: HashSet<InternedStr>` フィールド追加 |
| `rust_codegen.rs` | `with_bool_return()` | `bool_return_macros` も受け取るように拡張 |
| `rust_codegen.rs` | `generate_macros()` | `bool_return_macros` を `RustCodegen` に渡す |
| `rust_codegen.rs` | `is_bool_expr_with_dict()` | `bool_return_macros` をチェック |
| `rust_codegen.rs` | `wrap_as_bool_condition_macro()` | パラメータ bool 判定追加 |
| `rust_codegen.rs` | `wrap_as_bool_condition_inline()` | 同上 |
| `rust_codegen.rs` | 両 `wrap_as_bool_condition` | フィールド bool 判定追加 |

## 期待効果

39件の `expected bool, found integer` エラー解消
