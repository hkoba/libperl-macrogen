# Plan: 条件式の戻り値型で具体的な型を優先する

## 問題

`GvCVu(gv)` の戻り値型が `*mut c_void` になるが、`*mut CV` であるべき。

```c
#define GvCVu(gv) (GvGP(gv)->gp_cvgen ? NULL : GvGP(gv)->gp_cv)
```

C の `?:` 演算子で then が `NULL`、else が `gp_cv` (`*mut CV`)。
`NULL` は `(void*)0` で `*mut c_void` に変換される。
結果型は `*mut c_void` と `*mut CV` の統一で `*mut c_void` が選ばれる。

## 原因

2箇所で「具体的でない型」が優先される:

### 1. `infer_expr_type` の Conditional ハンドラ

```rust
ExprKind::Conditional { then_expr, .. } => {
    self.infer_expr_type(then_expr, info)  // then のみ参照
}
```

then 分岐のみの型を返す。then が `NULL` → `None` or `*mut c_void`。
else (`gp_cv: *mut CV`) を見ていない。

### 2. `compute_conditional_type_str` (semantic.rs)

then と else の型を `usual_arithmetic_conversion_str` で統一。
`void *` と `CV *` の統一で `void *` が選ばれる。

## 修正方針

### 原則: null リテラル分岐より具体的な分岐の型を優先

C の `NULL ? A : B` パターンでは、non-null 分岐が実際の型情報を持つ。
`NULL` は任意のポインタ型に変換可能な汎用リテラルなので、
相手側の具体的な型に合わせるべき。

### 修正 1: `infer_expr_type` の Conditional

```rust
ExprKind::Conditional { then_expr, else_expr, .. } => {
    // then が null リテラルなら else の型を使う
    if is_null_literal(then_expr) {
        return self.infer_expr_type(else_expr, info);
    }
    let then_ty = self.infer_expr_type(then_expr, info);
    // then が void ポインタなら else を試す
    if then_ty.as_ref().is_some_and(|t| t.is_void_pointer()) {
        if let Some(else_ty) = self.infer_expr_type(else_expr, info) {
            if !else_ty.is_void_pointer() {
                return Some(else_ty);
            }
        }
    }
    if then_ty.is_some() { return then_ty; }
    self.infer_expr_type(else_expr, info)
}
```

### 修正 2: `compute_conditional_type_str` (semantic.rs)

```rust
fn compute_conditional_type_str(...) -> String {
    let then_ty = self.get_expr_type_str(then_id, type_env);
    let else_ty = self.get_expr_type_str(else_id, type_env);
    // void * と具体的なポインタ型 → 具体的な方を採用
    if then_ty.contains("void") && !else_ty.contains("void") && else_ty.contains("*") {
        return else_ty;
    }
    if else_ty.contains("void") && !then_ty.contains("void") && then_ty.contains("*") {
        return then_ty;
    }
    self.usual_arithmetic_conversion_str(&then_ty, &else_ty)
}
```

## 影響範囲

同様のパターン (`cond ? NULL : ptr`) は Perl マクロに多数存在:
- `GvCVu`, `CopFILEGV`, `HeKEY_sv`, `GvIOp` 等

## 期待効果

`*mut c_void` → 具体的なポインタ型に改善されるケースが複数。
関連する E0308 エラーの一部が解消される。
