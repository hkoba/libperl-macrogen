# inline 関数の戻り値型にポインタが反映されない問題

## 問題

inline 関数で `HEK *` のようなポインタを返す関数が、Rust コードでは `HEK` として生成される。

### 例

```c
// C の定義
PERL_STATIC_INLINE HEK *
CvNAME_HEK(CV *sv)
{
    ...
}
```

```rust
// 現在の出力（誤り）
pub unsafe fn CvNAME_HEK(sv: *mut CV) -> HEK { ... }

// 期待する出力
pub unsafe fn CvNAME_HEK(sv: *mut CV) -> *mut HEK { ... }
```

## 原因

`RustCodegen::generate_inline_fn` (line 811) で戻り値型を取得する際:

```rust
let return_type = self.decl_specs_to_rust(&func_def.specs);
```

これは `DeclSpecs`（基本型 `HEK`）のみを使用し、`func_def.declarator.derived` に含まれるポインタ派生型（`*`）を適用していない。

### C の AST 構造

`HEK *CvNAME_HEK(CV *sv)` の場合:
- `func_def.specs` = `DeclSpecs { type_specs: [HEK] }` （基本型）
- `func_def.declarator.derived` = `[Pointer(*), Function((CV *sv))]`

派生型の順序:
1. `Pointer` - 戻り値をポインタにする `*`
2. `Function` - 関数パラメータ `(CV *sv)`

パラメータでは既に `apply_derived_to_type` を使っているが、戻り値型には適用されていない。

## 解決策

`generate_inline_fn` で戻り値型にも `apply_derived_to_type` を適用する。
ただし、`Function` 派生型は除外する必要がある。

### 修正コード

```rust
pub fn generate_inline_fn(mut self, name: crate::InternedStr, func_def: &FunctionDef) -> GeneratedCode {
    let name_str = self.interner.get(name);

    // パラメータリストを取得
    let params_str = self.build_fn_param_list(&func_def.declarator.derived);

    // 戻り値の型を取得（基本型）
    let return_type = self.decl_specs_to_rust(&func_def.specs);

    // declarator の派生型（ポインタなど）を適用（Function を除く）
    let return_derived: Vec<_> = func_def.declarator.derived.iter()
        .filter(|d| !matches!(d, DerivedDecl::Function(_)))
        .cloned()
        .collect();
    let return_type = self.apply_derived_to_type(&return_type, &return_derived);

    // ... 以下同じ
}
```

## 変更対象ファイル

| ファイル | 変更内容 |
|----------|----------|
| `src/rust_codegen.rs` | `generate_inline_fn` で戻り値型に派生型を適用 |

## 影響を受ける関数

- `CvNAME_HEK` - `HEK *` を返す
- `Perl_CvGV` - `GV *` を返す（※現在 `GV` になっている）
- `Perl_padname_refcnt_inc` - `PADNAME *` を返す
- その他ポインタを返す inline 関数全般

## テスト

1. `cargo build` でビルド確認
2. `cargo test` で既存テストが通ることを確認
3. `--gen-rust` で上記関数が正しい戻り値型で生成されることを確認
