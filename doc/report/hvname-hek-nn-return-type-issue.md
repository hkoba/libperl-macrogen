# HvNAME_HEK_NN の戻り値の型が `()` と推論される問題

## 症状

`HvNAME_HEK_NN` の戻り値の型が `()` と推論され、不完全なコードが生成される。

```rust
pub unsafe fn HvNAME_HEK_NN(hv: *mut SV) -> () {
    unsafe {
        (if (((*HvAUX(hv)).xhv_name_count) != 0) {
            (*((*HvAUX(hv)).xhv_name_u).xhvnameu_names)
        } else {
            ((*HvAUX(hv)).xhv_name_u).xhvnameu_name
        })
    }
}
```

## 原因の連鎖

### 1. HvNAME_HEK_NN → HvAUX

`HvNAME_HEK_NN(hv)` の本体は条件式で、両方の分岐が `HvAUX(hv)` の戻り値のフィールドにアクセスする。`HvAUX(hv)` の戻り値型が `*mut ()` になっているため、フィールドアクセスが解決できず `()` になる。

### 2. HvAUX の本体

```c
#define HvAUX(hv) (&(((struct xpvhv_with_aux *)SvANY(hv))->xhv_aux))
```

型推論の処理:

| ステップ | 式 | 期待される型 | 実際の推論結果 |
|----------|-----|-------------|---------------|
| SvANY(hv) | 関数呼び出し | `*mut XPVHV` 等 | (正常) |
| `(struct xpvhv_with_aux *)` | キャスト | `*mut xpvhv_with_aux` | **`*mut c_int`** ← ★ |
| `->xhv_aux` | フィールドアクセス | `xpvhv_aux` | **`Void`** (int にフィールドは無い) |
| `&(...)` | アドレス取得 | `*mut xpvhv_aux` | **`*mut ()`** |

### 3. 根本原因: `resolve_decl_specs_readonly` が `TypeSpec::Struct` を無視する

`ExprKind::Cast` ハンドラ (`semantic.rs:1329`) の処理フロー:

```rust
ExprKind::Cast { type_name, expr: inner } => {
    let ty = self.resolve_type_name(type_name);        // ← (1)
    let ty_str = ty.display(self.interner);              // ← (2)
    let target_type = TypeRepr::from_apidoc_string(&ty_str, self.interner); // ← (3)
    ...
}
```

`resolve_type_name` は内部で `resolve_decl_specs_readonly` を呼ぶ (`semantic.rs:828`)。

`resolve_decl_specs_readonly` (`semantic.rs:790-824`) は **簡略版** で、主要なプリミティブ型のみを処理する:

```rust
fn resolve_decl_specs_readonly(&self, specs: &DeclSpecs) -> Type {
    // 簡略版: 主要な型のみ処理
    for spec in &specs.type_specs {
        match spec {
            TypeSpec::Void => ...,
            TypeSpec::Char => ...,
            TypeSpec::Int => ...,
            TypeSpec::Long => ...,
            TypeSpec::Float => ...,
            TypeSpec::Double => ...,
            TypeSpec::Unsigned => ...,
            TypeSpec::Bool => ...,
            TypeSpec::TypedefName(name) => ...,
            _ => {}  // ← ★ TypeSpec::Struct, Union, Enum が全て無視される
        }
    }
    ...
    base_type.unwrap_or(Type::Int)  // ← ★ None のまま → Int にフォールバック
}
```

`TypeSpec::Struct(StructSpec { name: Some("xpvhv_with_aux"), ... })` は `_ => {}` にマッチして無視され、`base_type` は `None` のまま。結果として `Type::Int` にフォールバックする。

### 4. Type→String→TypeRepr roundtrip の問題

仮に `resolve_decl_specs_readonly` が `TypeSpec::Struct` を正しく処理したとしても、後続の roundtrip にも問題がある:

1. `Type::Struct { name: Some("xpvhv_with_aux"), .. }` の `display()` → `"struct xpvhv_with_aux"`
2. ポインタを含めると `"struct xpvhv_with_aux *"`
3. `from_apidoc_string("struct xpvhv_with_aux *")` → `parse_c_base_type("struct xpvhv_with_aux")`
4. `interner.lookup("xpvhv_with_aux")` → 成功すれば `CTypeSpecs::Struct { name: Some(..) }` になる

この場合、文字列が `interner` に登録されていれば roundtrip は成功する。しかし、そもそも Type→String→TypeRepr の roundtrip 自体が不必要であり、直接 AST→TypeRepr 変換を行うべき。

## 影響範囲

`ExprKind::Cast` で構造体・共用体・列挙型へのキャストを含む全てのマクロで同じ問題が発生する。

代表例:
- `HvAUX(hv)` → `*mut ()` (should be `*mut xpvhv_aux`)
- `HvEITER(my_perl, hv)` → `()` (HvAUX 経由)
- `HvEITER_get(hv)` → `()` (HvAUX 経由)
- `HvNAME_HEK_NN(hv)` → `()` (HvAUX 経由)
- `HvNAME_get(hv)` → HvNAME_HEK_NN 経由で影響

## 修正方針

`doc/plan/eliminate-type-string-roundtrip.md` の Phase 2 (Cast 式の直接変換) がこの問題を解決する。

具体的には、`ExprKind::Cast` ハンドラで `resolve_type_name()` → `display()` → `from_apidoc_string()` の roundtrip を排除し、`TypeName` の AST ノードから直接 `TypeRepr` を構築する:

```rust
ExprKind::Cast { type_name, expr: inner } => {
    let specs = CTypeSpecs::from_decl_specs(&type_name.specs, self.interner);
    let derived = type_name.declarator.as_ref()
        .map(|d| CDerivedType::from_derived_decls(&d.derived)
            .into_iter()
            .take_while(|d| !matches!(d, CDerivedType::Function { .. }))
            .collect())
        .unwrap_or_default();
    let target_type = TypeRepr::CType { specs, derived, source: CTypeSource::Cast };
    ...
}
```

## 関連ファイル

- `src/semantic.rs:790-824` - `resolve_decl_specs_readonly()` (問題の関数)
- `src/semantic.rs:827-834` - `resolve_type_name()` (Cast ハンドラから呼ばれる)
- `src/semantic.rs:1329-1341` - `ExprKind::Cast` ハンドラ (roundtrip がある箇所)
- `src/type_repr.rs:797-835` - `parse_c_type_string()` (文字列パーサー)
- `doc/plan/eliminate-type-string-roundtrip.md` - 修正計画
