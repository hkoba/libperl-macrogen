# Plan: 戻り値型の Tier ベース選択

## 問題

CxLABEL が `*mut c_char` を返すが、CopLABEL は `*const c_char` を返す。
CxLABEL は CopLABEL を呼ぶだけなので `*const c_char` であるべき。

## 原因

`MacroInferInfo::get_return_type()` はルート式の `expr_constraints` の
**最初の制約** (`constraints.first()`) を返す。

CxLABEL のルート式 `Call(CopLABEL, ...)` には複数の制約が付く:
1. `CType { Char, Pointer { is_const: false }, source: Parser }` → `*mut c_char`
2. `RustType { *const c_char, source: Parsed }` → `*const c_char` (return_types_cache 由来)

`first()` が (1) の `*mut c_char` を返すため、戻り値型が `*mut` になる。

## 修正方針

`get_param_type()` と同じく、**Tier ベース** で最高確度の制約を選択する。

### 変更箇所

**`MacroInferInfo::get_return_type()`** (macro_infer.rs):

```rust
// Before: constraints.first() で最初の制約を返す
// After: confidence_tier() で最高 Tier の制約を選ぶ

pub fn get_return_type(&self) -> Option<&TypeRepr> {
    if let Some(ty) = self.type_env.get_return_type() {
        return Some(ty);  // apidoc return_constraints は最優先
    }
    if let ParseResult::Expression(ref expr) = self.parse_result {
        if let Some(constraints) = self.type_env.get_expr_constraints(expr.id) {
            // Tier ベースで最高確度の制約を選択
            return constraints.iter()
                .filter(|c| !c.ty.is_void())
                .min_by_key(|c| c.ty.confidence_tier())
                .map(|c| &c.ty);
        }
    }
    None
}
```

## 期待効果

CxLABEL のような「別のマクロを呼ぶだけ」のラッパーマクロで、
呼び出し先の正確な戻り値型（Tier 3 Parsed、`return_types_cache` 由来）が
パーサー由来の推論型（Tier 4 Parser）より優先される。

同様のパターンは `CxLABEL_len`, `CxLABEL_len_flags` 等にも適用される。
