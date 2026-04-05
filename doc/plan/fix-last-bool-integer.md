# Plan: 残り2件の `expected bool, found integer` エラーの修正

## ケース 1: `PL_valid_types_PVX[i] != 0` (1件)

### 現状

```rust
// Perl_sv_setpv_freshbuf 内の assert
assert!(((*PL_valid_types_PVX.as_ptr().offset(((SvTYPE(sv) as u32) & SVt_MASK) as isize))) != 0);
```

`PL_valid_types_PVX` は bindings.rs で `[bool; 17]` と宣言。
配列要素は `bool` 型だが、`!= 0` が付加されてエラー。

### 原因

1. `is_bool_expr_with_dict` が `Index` 式（配列要素アクセス）をカバーしていない
2. `infer_expr_type_inline` にも `Index` ケースがなく、配列要素型を推論できない

### 修正

**`is_bool_expr_with_dict`** に `Index` ケースを追加:
`Index { expr: Ident(name) }` で、`name` が static 配列で要素型が `bool` なら true。

bindings.rs の static 変数の型情報は `RustDeclDict` の `consts` に含まれている可能性。
または `static_arrays` にある可能性を確認する。

```rust
// is_bool_expr_with_dict に追加
ExprKind::Index { expr: base, .. } => {
    // 静的配列のインデックスアクセス: 要素型が bool なら bool
    if let ExprKind::Ident(name) = &base.kind {
        // rust_decl_dict の statics から配列要素型を確認
        // PL_valid_types_PVX: [bool; 17] → 要素型 bool
    }
}
```

ただし、`PL_valid_types_PVX` は Deref を経由して `as_ptr().offset(i)` に変換される。
assert ハンドラの条件式は `Deref(Call(offset, [Call(as_ptr, [Ident(PL_valid_types_PVX)]), ...]))`
のような形になる。これは `is_bool_expr_with_dict` で直接検出するには複雑。

**より簡単なアプローチ**: `infer_expr_type_inline` に `Deref(Call(offset, [receiver]))` パターンを追加。
receiver が `Call(as_ptr, [Ident(array_name)])` で、`array_name` の型が `[bool; N]` なら
Deref 結果は `bool`。

しかしこれは AST レベルではなく codegen レベルの変換後の形式。
元の C AST は `Index { Ident(PL_valid_types_PVX), BinaryExpr(BitAnd, ...) }`。

**最も簡単**: `infer_expr_type` / `infer_expr_type_inline` に `Index` ケースを追加:

```rust
ExprKind::Index { expr: base, .. } => {
    // 配列のインデックス → 配列要素型
    if let ExprKind::Ident(name) = &base.kind {
        if let Some(dict) = self.rust_decl_dict {
            let name_str = self.interner.get(*name);
            if let Some(c) = dict.consts.get(name_str) {
                // 配列の要素型を取得
                return c.uty.inner_type().cloned();
            }
        }
    }
    None
}
```

`UnifiedType::Array { inner }` の `inner_type()` は配列要素型を返す。

---

## ケース 2: `MAYBE_DEREF_GV_flags<T>` (1件)

### 現状

```rust
pub unsafe fn MAYBE_DEREF_GV_flags<T>(...) -> *mut GV {
    { { (((((&mut SV_GMAGIC) as T)) != 0) && ...) }; ... }
}
```

`(&mut SV_GMAGIC) as T` — ジェネリック型 `T` への `as` キャスト。
`T` は Rust では `as` キャストの対象にできないため E0605 エラーも出る。
`!= 0` も `T` に対して適用できないため E0308。

### 原因

C の `MAYBE_DEREF_GV_flags` は flags パラメータを型パラメータとして扱うマクロ。
C では `((U32)(flag) != 0)` のような暗黙のキャストが可能だが、
Rust のジェネリクスでは不可。

### 対処

このケースは **ジェネリクスの根本的な制限** (E0605 の23件と同じカテゴリ)。
`bool` の問題だけを修正しても他のエラーが残る。

**推奨**: この関数はジェネリクスのカテゴリとして扱い、
`bool` 固有の修正は不要。生成を抑制するか、ジェネリクス全体の対策で対応。

---

## 実装順序

1. **ケース 1**: `infer_expr_type` / `infer_expr_type_inline` に `Index` ケース追加 (1件解消)
2. **ケース 2**: ジェネリクスカテゴリとして後回し

## 期待効果

1件解消。残り1件はジェネリクスの根本問題。
