# 関数ポインタ型の Rust 変換

## 問題

inline 関数のパラメータで関数ポインタ型が正しく生成されない。

### 例: Perl_SvPV_helper

```c
// C の定義
char * (*non_trivial)(pTHX_ SV *, STRLEN * const, const U32)
```

```rust
// 現在の出力（誤り）
non_trivial: *mut *mut /* fn */ c_char

// 期待する出力
non_trivial: Option<unsafe extern "C" fn(*mut PerlInterpreter, *mut SV, *const STRLEN, U32) -> *mut c_char>
```

## 原因

`apply_derived_to_type` (line 794-796) で `DerivedDecl::Function` を処理する際、
単にコメント `/* fn */` を出力しているだけ:

```rust
DerivedDecl::Function(_) => {
    // 関数ポインタは複雑なので簡易実装
    result = format!("/* fn */ {}", result);
}
```

## C の関数ポインタ宣言の構造

`char * (*non_trivial)(pTHX_ SV *, STRLEN * const, const U32)` の場合:

### 現在の出力分析

現在の出力 `*mut *mut /* fn */ c_char` から逆算すると、
`apply_derived_to_type` の reverse iteration で:

1. base = `c_char`
2. Function → `/* fn */ c_char`
3. Pointer → `*mut /* fn */ c_char`
4. Pointer → `*mut *mut /* fn */ c_char`

つまり derived 配列は（forward 順）:
```
[Pointer(戻り値の*), Pointer(関数ポインタの*), Function(params)]
```

### 派生型の意味

- `derived[0]`: Pointer - 戻り値型 `char *` の `*`
- `derived[1]`: Pointer - 関数ポインタ `(*name)` の `*`
- `derived[2]`: Function - パラメータリスト `(params)`

### 正しい解釈

```
derived:  [Pointer, Pointer, Function(params)]
           ↑        ↑        ↑
           戻り値   fn ptr   パラメータ
```

- Function より**前**（forward で）: 戻り値型の派生 → `*mut c_char`
- Function より**後**（forward で）: なし（関数ポインタの Pointer は Function の前にある）

実際には：
- Pointer(戻り値) + base → 戻り値型 `*mut c_char`
- Pointer(fn ptr) + Function → 関数へのポインタ型

## 解決策

### Rust の関数ポインタ構文

```rust
// 基本形
fn(ParamTypes...) -> ReturnType

// C 関数との互換性
unsafe extern "C" fn(ParamTypes...) -> ReturnType

// NULL 許容（C では関数ポインタは NULL になりうる）
Option<unsafe extern "C" fn(ParamTypes...) -> ReturnType>
```

### 実装方針

`apply_derived_to_type` を拡張して、`DerivedDecl::Function` を適切に処理:

1. **関数ポインタパターンの検出**:
   - `derived` 配列内に `Pointer` + `Function` のパターンがある場合

2. **パラメータリストの生成**:
   - `Function(ParamList)` の各 `ParamDecl` を Rust 型に変換
   - pTHX_ は `*mut PerlInterpreter` として展開済み

3. **戻り値型の決定**:
   - `Function` より外側（先頭側）の派生型を戻り値型に適用
   - 基本型 `char` + 外側 `Pointer` → `*mut c_char`

### 新しい実装

`apply_derived_to_type` を拡張:

```rust
fn apply_derived_to_type(&mut self, base: &str, derived: &[DerivedDecl]) -> String {
    // Function を探す
    let fn_idx = derived.iter().position(|d| matches!(d, DerivedDecl::Function(_)));

    if let Some(idx) = fn_idx {
        if let DerivedDecl::Function(param_list) = &derived[idx] {
            // Function の直前が Pointer なら関数ポインタ
            let is_fn_pointer = idx > 0 && matches!(derived[idx - 1], DerivedDecl::Pointer(_));

            // 戻り値型の派生（Function と fn ptr Pointer を除く）
            let return_end = if is_fn_pointer { idx - 1 } else { idx };
            let return_derived = &derived[..return_end];
            let return_type = self.apply_simple_derived(base, return_derived);

            // パラメータリストを生成（型のみ、名前なし）
            let params: Vec<_> = param_list.params.iter()
                .map(|p| self.param_type_only(p))
                .collect();
            let params_str = params.join(", ");

            // 関数型を生成
            let fn_type = format!("unsafe extern \"C\" fn({}) -> {}", params_str, return_type);

            // 関数ポインタの場合は Option でラップ（NULL 許容）
            if is_fn_pointer {
                return format!("Option<{}>", fn_type);
            }
            return fn_type;
        }
    }

    // 通常の型変換（Function を含まない場合）
    self.apply_simple_derived(base, derived)
}

/// 単純な派生型の適用（Pointer と Array のみ）
fn apply_simple_derived(&mut self, base: &str, derived: &[DerivedDecl]) -> String {
    let mut result = base.to_string();
    for d in derived.iter().rev() {
        match d {
            DerivedDecl::Pointer(quals) => {
                if quals.is_const {
                    result = format!("*const {}", result);
                } else {
                    result = format!("*mut {}", result);
                }
            }
            DerivedDecl::Array(arr) => {
                // 配列処理（既存コードと同じ）
            }
            DerivedDecl::Function(_) => {
                // この関数では Function は処理しない
            }
        }
    }
    result
}

/// ParamDecl から型のみを取得（名前なし）
fn param_type_only(&mut self, param: &ParamDecl) -> String {
    let ty = self.decl_specs_to_rust(&param.specs);
    if let Some(ref declarator) = param.declarator {
        self.apply_derived_to_type(&ty, &declarator.derived)
    } else {
        ty
    }
}
```

## 変更対象ファイル

| ファイル | 変更内容 |
|----------|----------|
| `src/rust_codegen.rs` | `apply_derived_to_type` を拡張して関数ポインタ対応 |

## 注意点

1. **派生型の順序**: C の宣言構文では derived の順序が重要
2. **pTHX_ の展開**: マクロ展開済みなので `*mut PerlInterpreter` として含まれる
3. **NULL 許容**: C の関数ポインタは NULL になりうるので `Option` でラップ
4. **可変長引数**: `is_variadic` の場合の対応

## 期待する出力

```rust
// Perl_SvPV_helper の non_trivial パラメータ
non_trivial: Option<unsafe extern "C" fn(*mut PerlInterpreter, *mut SV, *const STRLEN, U32) -> *mut c_char>
```

## テスト

1. `cargo build` でビルド確認
2. `cargo test` で既存テストが通ることを確認
3. `Perl_SvPV_helper` の `non_trivial` パラメータが正しく生成されることを確認

## 段階的な実装

この変更は複雑なため、段階的に実装する:

1. **Phase 1**: `apply_simple_derived` を切り出し（既存の Pointer/Array 処理）
2. **Phase 2**: `param_type_only` を追加
3. **Phase 3**: `apply_derived_to_type` に Function 処理を追加
4. **Phase 4**: RustCodegen と CodegenDriver の両方に適用
