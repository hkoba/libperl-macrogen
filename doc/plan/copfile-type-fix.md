# CopFILE 型推論の修正計画

## 現象

```rust
/// CopFILE - macro function
#[inline]
pub unsafe fn CopFILE(c: *mut COP) -> c_void {
    (*c).cop_file
}
```

期待される出力（apidoc による）:
```rust
pub unsafe fn CopFILE(c: *const COP) -> *const c_char {
    (*c).cop_file
}
```

## 問題の分析

### 問題1: cop_file フィールドの型追跡

**現状**:
- `cop_file` は cop.h で `char * cop_file;` として定義されている
- `FieldsDict` に `cop_file -> cop` として記録されている
- しかし、式 `(*c).cop_file` の型推論で `c_void` が返されている

**調査が必要な点**:
- `FieldsDict` に `cop_file` の型 (`char *`) が保存されているか
- 型推論時に `COP` 構造体の `cop_file` フィールドの型が参照されているか
- `cop` と `COP` の typedef 関係が解決されているか

### 問題2: apidoc の優先度

**現状**:
- `macro_infer.rs` で apidoc の戻り値型が `type_env.add_return_constraint()` で追加される
- `rust_codegen.rs` の `get_return_type()` は `expr_constraints` のみを参照
- `return_constraints` は完全に無視されている

**原因**:
```rust
// rust_codegen.rs:320-332
fn get_return_type(&mut self, info: &MacroInferInfo) -> String {
    match &info.parse_result {
        ParseResult::Expression(expr) => {
            // expr_constraints のみ参照、return_constraints は無視
            if let Some(constraints) = info.type_env.expr_constraints.get(&expr.id) {
                if let Some(first) = constraints.first() {
                    return self.type_repr_to_rust(&first.ty);
                }
            }
            self.unknown_marker().to_string()
        }
        ...
    }
}
```

## 修正計画

### Phase 1: 問題1の調査と修正

#### 1.1 cop_file の型がFieldsDictに保存されているか確認

```bash
cargo run -- --auto samples/wrapper.h 2>&1 | grep -A5 "cop_file"
```

を実行して、`cop_file` の型情報を確認する。

#### 1.2 PtrMember 式の型推論を確認

`semantic.rs` の `infer_expr_type` で `PtrMember` ケースの処理を確認:
- base の型から構造体名を取得
- `FieldsDict::get_field_type_by_name` でフィールド型を取得
- typedef の解決（COP → cop）

#### 1.3 必要に応じて修正

- typedef 解決の問題がある場合、`FieldsDict` の typedef マッピングを修正
- 型推論パスの問題がある場合、`semantic.rs` を修正

### Phase 2: 問題2の修正 - apidoc 優先度

#### 2.1 `get_return_type` の修正

`rust_codegen.rs` の `get_return_type` を修正して、`return_constraints` を優先的に参照するようにする:

```rust
fn get_return_type(&mut self, info: &MacroInferInfo) -> String {
    // 1. まず return_constraints を確認（apidoc からの指定）
    if !info.type_env.return_constraints.is_empty() {
        // apidoc ソースの制約を優先
        for constraint in &info.type_env.return_constraints {
            if constraint.is_from_apidoc() {
                return self.type_repr_to_rust(&constraint.ty);
            }
        }
        // apidoc がなければ最初の return_constraint を使用
        if let Some(first) = info.type_env.return_constraints.first() {
            return self.type_repr_to_rust(&first.ty);
        }
    }

    // 2. 式から導出した型を使用
    match &info.parse_result {
        ParseResult::Expression(expr) => {
            if let Some(constraints) = info.type_env.expr_constraints.get(&expr.id) {
                if let Some(first) = constraints.first() {
                    return self.type_repr_to_rust(&first.ty);
                }
            }
            self.unknown_marker().to_string()
        }
        ParseResult::Statement(_) => "()".to_string(),
        ParseResult::Unparseable(_) => "()".to_string(),
    }
}
```

#### 2.2 パラメータ型の apidoc 優先

同様に、パラメータ型も apidoc の指定を優先するように修正が必要:

- `get_param_type` または同等の関数で、apidoc の引数型を参照する
- 現在の引数: `c: *mut COP` → 期待: `c: *const COP`

### Phase 3: TypeConstraint のソース判定

`TypeConstraint` に `is_from_apidoc()` メソッドがあるか確認し、なければ追加:

```rust
impl TypeConstraint {
    pub fn is_from_apidoc(&self) -> bool {
        // TypeRepr 内のソース情報を確認
        matches!(self.ty.source(), Some(CTypeSource::Apidoc))
    }
}
```

## 変更対象ファイル

| ファイル | 変更内容 |
|----------|----------|
| `src/rust_codegen.rs` | `get_return_type` で `return_constraints` を優先参照 |
| `src/rust_codegen.rs` | パラメータ型で apidoc を優先参照 |
| `src/type_env.rs` | 必要に応じて `TypeConstraint::is_from_apidoc()` 追加 |
| `src/semantic.rs` | PtrMember の型推論修正（調査後） |
| `src/fields_dict.rs` | typedef 解決の修正（調査後） |

## テスト計画

1. CopFILE のコード生成確認:
   ```bash
   cargo run -- --auto --gen-rust samples/wrapper.h 2>&1 | grep -A5 "fn CopFILE"
   ```
   期待: `c: *const COP`, `-> *const c_char`

2. 既存テストの通過確認

3. 他の apidoc 指定マクロも正しく生成されることを確認
