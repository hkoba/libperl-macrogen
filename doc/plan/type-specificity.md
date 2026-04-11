# Plan: 型の具体性 (Specificity) による選択ルール

## 概念

型の選択において、Tier (情報源の確度) に加えて、
型の**内容の具体性** (specificity) を導入する。

| 概念 | 比較対象 | 例 |
|------|---------|-----|
| **Tier** (確度) | 情報の出所 | bindings (1) > apidoc (3) > 推論 (4) |
| **Specificity** (具体性) | 型の内容 | `*mut CV` > `*mut c_void` > `*mut _` |

同 Tier の制約が複数ある場合、より具体的な型を優先する。

## void ポインタの具体性

`c_void` は C の `void*` に対応し、任意のポインタ型を表す汎用型。
型情報を持たないため、具体的な型がある場合はそちらを優先すべき。

```
具体性: 高 ← *mut CV, *mut SV, *mut c_char
具体性: 低 ← *mut c_void
具体性: 無 ← ()  (void, 型なし)
```

## 適用箇所

### 1. Conditional 式の型推論 (codegen + semantic)

`cond ? NULL : gp_cv` → then=`*mut c_void`, else=`*mut CV` → `*mut CV` を選択。

**codegen 側** (`infer_expr_type` / `infer_expr_type_inline`):
```rust
ExprKind::Conditional { then_expr, else_expr, .. } => {
    let then_ty = self.infer_expr_type(then_expr, info);
    let else_ty = self.infer_expr_type(else_expr, info);
    select_more_specific(then_ty, else_ty)
}
```

**semantic 側** (`compute_conditional_type_str`):
void ポインタと具体的ポインタの統一で具体的な方を選ぶ。

### 2. 戻り値型の Tier ベース選択 (Phase 2 + codegen)

同 Tier の制約が複数ある場合に具体性で選択:
```rust
// 同 Tier なら具体性で選択
if tier == best_tier {
    if ty.specificity() > best_ty.specificity() {
        best = Some((ty, tier));
    }
}
```

### 3. null ポインタの型推論

代入文 `field = NULL` で field が具体的なポインタ型なら、
`std::ptr::null_mut()` で Rust に型推論を任せる（既に実装済み）。

## 実装

### Step 1: `UnifiedType` に具体性メソッドを追加

```rust
impl UnifiedType {
    /// void ポインタかどうか
    pub fn is_void_pointer(&self) -> bool {
        matches!(self, Self::Pointer { inner, .. }
            if matches!(**inner, Self::Named(ref n) if n == "c_void"))
    }

    /// ポインタだが void ではないか
    pub fn is_concrete_pointer(&self) -> bool {
        self.is_pointer() && !self.is_void_pointer()
    }
}
```

同様に `TypeRepr`:
```rust
impl TypeRepr {
    pub fn is_void_pointer(&self) -> bool {
        match self {
            TypeRepr::CType { specs, derived, .. } => {
                matches!(specs, CTypeSpecs::Void)
                    && derived.iter().any(|d| matches!(d, CDerivedType::Pointer { .. }))
            }
            ...
        }
    }
}
```

### Step 2: `infer_expr_type` の Conditional を修正

```rust
ExprKind::Conditional { then_expr, else_expr, .. } => {
    let then_ty = self.infer_expr_type(then_expr, info);
    let else_ty = self.infer_expr_type(else_expr, info);
    match (&then_ty, &else_ty) {
        // null/void なら相手側を使う
        (_, Some(et)) if is_null_literal(then_expr) => Some(et.clone()),
        (Some(tt), _) if is_null_literal(else_expr) => Some(tt.clone()),
        // void pointer vs concrete → concrete を優先
        (Some(tt), Some(et)) if tt.is_void_pointer() && et.is_concrete_pointer() => Some(et.clone()),
        (Some(tt), Some(et)) if et.is_void_pointer() && tt.is_concrete_pointer() => Some(tt.clone()),
        // デフォルト: then を優先
        (Some(_), _) => then_ty,
        (None, _) => else_ty,
    }
}
```

### Step 3: `compute_conditional_type_str` を修正

```rust
fn compute_conditional_type_str(&self, then_id: ExprId, else_id: ExprId, type_env: &TypeEnv) -> String {
    let then_ty = self.get_expr_type_str(then_id, type_env);
    let else_ty = self.get_expr_type_str(else_id, type_env);
    // void * vs 具体的ポインタ → 具体的な方
    if is_void_ptr_str(&then_ty) && is_concrete_ptr_str(&else_ty) {
        return else_ty;
    }
    if is_void_ptr_str(&else_ty) && is_concrete_ptr_str(&then_ty) {
        return then_ty;
    }
    self.usual_arithmetic_conversion_str(&then_ty, &else_ty)
}
```

### Step 4: `get_return_type` の同 Tier 選択に具体性を追加

`MacroInferInfo::get_return_type()` の Tier ベース選択で、
同 Tier の場合は void ポインタより具体的なポインタを優先:

```rust
let tier = c.ty.confidence_tier();
let is_more_specific = if tier == best_tier {
    // 同 Tier: 具体性で比較
    best_ty.is_void_pointer() && !c.ty.is_void_pointer()
} else {
    tier < best_tier
};
```

## 実装順序

1. `UnifiedType::is_void_pointer()` / `is_concrete_pointer()` 追加
2. `infer_expr_type` Conditional 修正 (codegen, 両パス)
3. `compute_conditional_type_str` 修正 (semantic)
4. `get_return_type` 同 Tier 具体性選択
5. アーキテクチャドキュメント更新

## 期待効果

- `GvCVu` の戻り値型が `*mut c_void` → `*mut CV` に
- 同様の `cond ? NULL : ptr` パターンで具体的な型が選ばれる
- `*mut c_void` 関連の E0308 エラーの一部が解消（推定 3-5 件）
