# キャスト式の型名を正しく Rust に変換

## 問題

マクロ関数内のキャスト式で、プリミティブ型（`char`, `int` など）が
`/* type */` として出力される。

### 例: SvPVX_mutable, SvPVX_const

```c
// C の定義
#define SvPVX_mutable(sv)  ((char *)((sv)->sv_u.svu_pv))
#define SvPVX_const(sv)    ((const char*)((sv)->sv_u.svu_pv))
```

```rust
// 現在の出力（誤り）
pub unsafe fn SvPVX_mutable(sv: *mut SV) -> *mut c_char {
    (((*sv).sv_u).svu_pv as *mut /* type */)
}

pub unsafe fn SvPVX_const(sv: *mut SV) -> *const c_char {
    (((*sv).sv_u).svu_pv as *mut /* type */)  // const も失われている
}

// 期待する出力
pub unsafe fn SvPVX_mutable(sv: *mut SV) -> *mut c_char {
    (((*sv).sv_u).svu_pv as *mut c_char)
}

pub unsafe fn SvPVX_const(sv: *mut SV) -> *const c_char {
    (((*sv).sv_u).svu_pv as *const c_char)
}
```

## 原因

`type_name_to_rust` メソッドが `TypedefName` のみを処理し、
プリミティブ型を無視している。

### RustCodegen::type_name_to_rust (line 643-677)

```rust
fn type_name_to_rust(&mut self, type_name: &crate::ast::TypeName) -> String {
    // ベース型を取得（typedef 名があればそれを使用、なければ不完全マーカー）
    let mut base_type: Option<String> = None;
    for spec in &type_name.specs.type_specs {
        if let crate::ast::TypeSpec::TypedefName(name) = spec {
            base_type = Some(self.interner.get(*name).to_string());
            break;
        }
    }
    let mut base_type = base_type.unwrap_or_else(|| self.type_marker().to_string());
    // ↑ TypedefName でなければ /* type */ になる

    // 宣言子からポインタ/配列を適用
    if let Some(ref decl) = type_name.declarator {
        for derived in &decl.derived {
            // ... Pointer, Array 処理
        }
    }
    base_type
}
```

### 問題点

1. `TypeSpec::Char`, `TypeSpec::Int` などのプリミティブ型を処理していない
2. 既存の `decl_specs_to_rust` はプリミティブ型を正しく処理できる
3. 派生型の処理も `apply_derived_to_type` と重複している

## 解決策

`type_name_to_rust` を修正して、既存の `decl_specs_to_rust` と
`apply_derived_to_type` を再利用する。

### 修正後の実装

```rust
/// TypeName を Rust 型文字列に変換
fn type_name_to_rust(&mut self, type_name: &crate::ast::TypeName) -> String {
    // decl_specs_to_rust でベース型を取得（プリミティブ型も正しく変換）
    let base_type = self.decl_specs_to_rust(&type_name.specs);

    // 宣言子からポインタ/配列/関数を適用
    if let Some(ref decl) = type_name.declarator {
        self.apply_derived_to_type(&base_type, &decl.derived)
    } else {
        base_type
    }
}
```

## TypeName の構造

```rust
pub struct TypeName {
    pub specs: DeclSpecs,                    // 型指定子（char, int, typedef名など）
    pub declarator: Option<AbstractDeclarator>,  // 派生型（ポインタ、配列など）
}

pub struct AbstractDeclarator {
    pub derived: Vec<DerivedDecl>,  // Pointer, Array, Function
}
```

## 変更対象ファイル

| ファイル | 変更内容 |
|----------|----------|
| `src/rust_codegen.rs` | `RustCodegen::type_name_to_rust` を修正 |
| `src/rust_codegen.rs` | `CodegenDriver::type_name_to_rust` を修正 |
| `src/rust_codegen.rs` | `RustCodegen::apply_simple_derived` で void ポインタ対応 |
| `src/rust_codegen.rs` | `CodegenDriver::apply_simple_derived` で void ポインタ対応 |

## const 修飾子の対応

### 問題

C の `const char*` は `specs.qualifiers.is_const` に const が格納されるが、
`DerivedDecl::Pointer` の `quals.is_const` は false のまま。
そのため `*mut c_char` が生成されてしまう。

### 解決策

`type_name_to_rust` で結果を後処理し、`specs.qualifiers.is_const` が true の場合、
最も内側の `*mut ` を `*const ` に置換する。

```rust
// C の const 修飾子（例: const char*）を Rust の *const に反映
// 最も内側のポインタを *const にする
if type_name.specs.qualifiers.is_const {
    if let Some(pos) = result.rfind("*mut ") {
        result.replace_range(pos..pos + 5, "*const ");
    }
}
```

### 変換例

| C の型 | Rust の型 |
|--------|-----------|
| `char*` | `*mut c_char` |
| `const char*` | `*const c_char` |
| `const char**` | `*mut *const c_char` |

## 注意点

1. **関数ポインタ対応**: `apply_derived_to_type` は既に関数ポインタを処理できる
2. **const 修飾子**: `DerivedDecl::Pointer(quals)` の `quals.is_const` で処理される

## void ポインタの対応

### 問題

- `decl_specs_to_rust` は `void` → `()` を返す
- ポインタ適用後: `void *` → `*mut ()` （不正）

### 期待する動作

- `void` → `()` （戻り値型 `void func()` の場合はそのまま）
- `void *` → `*mut c_void` （ポインタ型の場合）
- `const void *` → `*const c_void`

### 実装

`apply_simple_derived` でポインタを適用する際に、ベース型が `()` なら `c_void` に置き換える:

```rust
fn apply_simple_derived(&self, base: &str, derived: &[DerivedDecl]) -> String {
    let mut result = base.to_string();
    for d in derived.iter().rev() {
        match d {
            DerivedDecl::Pointer(quals) => {
                // void ポインタの場合は c_void を使用
                if result == "()" {
                    result = "c_void".to_string();
                }
                if quals.is_const {
                    result = format!("*const {}", result);
                } else {
                    result = format!("*mut {}", result);
                }
            }
            DerivedDecl::Array(arr) => {
                // 配列も同様に void 対応
                if result == "()" {
                    result = "c_void".to_string();
                }
                if let Some(ref size_expr) = arr.size {
                    if let ExprKind::IntLit(n) = &size_expr.kind {
                        result = format!("[{}; {}]", result, n);
                    } else {
                        result = format!("*mut {}", result);
                    }
                } else {
                    result = format!("*mut {}", result);
                }
            }
            DerivedDecl::Function(_) => {
                // この関数では Function は処理しない
            }
        }
    }
    result
}
```

### 期待する出力例

```rust
// (void *)ptr
(ptr as *mut c_void)

// (const void *)ptr
(ptr as *const c_void)

// void **ptr (ポインタへのポインタ)
*mut *mut c_void
```

## テスト

1. `cargo build` でビルド確認
2. `cargo test` で既存テストが通ることを確認
3. `--gen-rust` で以下を確認:
   - `SvPVX_mutable` が `*mut c_char` でキャストされる
   - `SvPVX_const` が `*const c_char` でキャストされる
   - `void *` を含むキャストが `*mut c_void` になる
   - `const void *` を含むキャストが `*const c_void` になる
