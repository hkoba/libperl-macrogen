# 型情報を TypeRepr ベースに統一する計画

## 問題の根本原因

複数の箇所で型情報を文字列（`String`）で保持・受け渡ししており、
これが `TypeRepr::from_apidoc_string()` での C/Rust 形式不一致を引き起こしている。

## 型情報を文字列で扱っている箇所の一覧

### 優先度: 高（今回の問題の直接原因）

| ファイル | 箇所 | 内容 |
|----------|------|------|
| `fields_dict.rs:15` | `FieldType::rust_type: String` | フィールド型を文字列で保持 |
| `fields_dict.rs:29` | `consistent_type_cache: HashMap<..., Option<String>>` | 一致型キャッシュ |
| `fields_dict.rs:40` | `sv_u_field_types: HashMap<..., String>` | sv_u union フィールド型 |
| `type_repr.rs:257` | `InferredType::MemberAccess { base_type: String }` | ベース型を文字列で保持 |
| `type_repr.rs:264` | `InferredType::PtrMemberAccess { base_type: String }` | ベース型を文字列で保持 |

### 優先度: 中（semantic.rs の型計算）

| ファイル | 箇所 | 内容 |
|----------|------|------|
| `semantic.rs:1300` | `get_expr_type_str() -> String` | 式の型を文字列で返す |
| `semantic.rs:1310` | `compute_binary_type_str() -> String` | 二項演算の型を文字列で計算 |
| `semantic.rs:1352` | `compute_conditional_type_str() -> String` | 条件式の型を文字列で計算 |

### 優先度: 高（汚染の起点となりうる外部データ読み込み）

| ファイル | 箇所 | 内容 |
|----------|------|------|
| `apidoc.rs:40` | `ApidocArg::ty: String` | apidoc からの型文字列 → TypeRepr に変更 |
| `rust_decl.rs:18,25,40,54` | `ty: String` | Rust bindings からの型文字列 → TypeRepr に変更 |
| `macro_infer.rs:66` | `SvAnyPattern::cast_type: String` | SvANY パターンの型 → TypeRepr に変更 |

### from_apidoc_string 呼び出し箇所（semantic.rs 内）

```
line 1473: SymbolLookup
line 1536: Binary 演算
line 1555-1557: Conditional
line 1574: Cast
line 1597-1598: ArraySubscript
line 1628: MemberAccess
line 1682: PtrMemberAccess
line 1705: Assignment (lhs)
line 1721: Comma
line 1736: Sizeof
line 1750: Unary
line 1764: AddrOf
line 1778: Deref
line 1792: IncDec
line 1844: AlignOf
line 1860: StmtExpr
line 2020: apidoc arg
line 2031: apidoc return
line 2090: parse_rust_type_string
```

## 解決方針

`FieldsDict` の型情報を `TypeRepr` で保持するように変更し、
文字列変換を介さずに型情報を受け渡す。

## Phase 1: FieldType 構造体の変更

**ファイル**: `src/fields_dict.rs`

### 変更前
```rust
pub struct FieldType {
    pub rust_type: String,
}
```

### 変更後
```rust
pub struct FieldType {
    /// 型情報（構造化された表現）
    pub type_repr: TypeRepr,
}
```

## Phase 2: フィールド型収集の変更

`extract_field_type()` を変更して `TypeRepr` を直接生成する。

### 変更前
```rust
fn extract_field_type(&self, specs: &DeclSpecs, declarator: &Declarator, interner: &StringInterner) -> Option<String> {
    let base_type = self.extract_base_type(specs, interner)?;
    let full_type = self.apply_derived_decls(&base_type, &declarator.derived, &specs.qualifiers);
    Some(full_type)
}
```

### 変更後
```rust
fn extract_field_type(&self, specs: &DeclSpecs, declarator: &Declarator, interner: &StringInterner) -> Option<TypeRepr> {
    // DeclSpecs から CTypeSpecs を生成
    let c_specs = CTypeSpecs::from_decl_specs(specs, interner)?;

    // Declarator から CDerivedType のリストを生成
    let derived = CDerivedType::from_declarator(declarator);

    Some(TypeRepr::CType {
        specs: c_specs,
        derived,
        source: CTypeSource::Header,
    })
}
```

## Phase 3: CTypeSpecs::from_decl_specs の実装

**ファイル**: `src/type_repr.rs`

`DeclSpecs` から `CTypeSpecs` を生成するメソッドを追加：

```rust
impl CTypeSpecs {
    /// DeclSpecs から CTypeSpecs を生成
    pub fn from_decl_specs(specs: &DeclSpecs, interner: &StringInterner) -> Option<Self> {
        // 既存の extract_base_type のロジックを TypeRepr 版に移植
        // void, char, int, struct, typedef 等を処理
    }
}
```

## Phase 4: CDerivedType::from_declarator の実装

**ファイル**: `src/type_repr.rs`

`Declarator` から `CDerivedType` のリストを生成：

```rust
impl CDerivedType {
    /// Declarator の derived から CDerivedType リストを生成
    pub fn from_declarator(declarator: &Declarator) -> Vec<Self> {
        declarator.derived.iter().filter_map(|d| {
            match d {
                DerivedDecl::Pointer { qualifiers } => {
                    Some(CDerivedType::Pointer {
                        is_const: qualifiers.is_const,
                        is_volatile: qualifiers.is_volatile,
                        is_restrict: qualifiers.is_restrict,
                    })
                }
                DerivedDecl::Array { size } => {
                    Some(CDerivedType::Array { size: *size })
                }
                _ => None,
            }
        }).collect()
    }
}
```

## Phase 5: ルックアップ API の変更

**ファイル**: `src/fields_dict.rs`

### 変更前
```rust
pub fn get_field_type_by_name(...) -> Option<&FieldType>
// 呼び出し側: field_type.rust_type.clone()
```

### 変更後
```rust
pub fn get_field_type_by_name(...) -> Option<&FieldType>
// 呼び出し側: field_type.type_repr.clone()
```

## Phase 6: semantic.rs の変更

**ファイル**: `src/semantic.rs`

### 変更前
```rust
fn lookup_field_type_by_name(&self, struct_name: &str, field_name: InternedStr) -> Option<String> {
    let fields_dict = self.fields_dict?;
    let field_type = fields_dict.get_field_type_by_name(struct_name, field_name, self.interner)?;
    Some(field_type.rust_type.clone())
}

// 使用箇所
let member_ty_str = self.lookup_field_type_by_name(struct_name, *member)
    .unwrap_or_else(|| "<unknown>".to_string());
let field_type = if member_ty_str != "<unknown>" {
    Some(Box::new(TypeRepr::from_apidoc_string(&member_ty_str, self.interner)))
} else {
    None
};
```

### 変更後
```rust
fn lookup_field_type_repr(&self, struct_name: &str, field_name: InternedStr) -> Option<TypeRepr> {
    let fields_dict = self.fields_dict?;
    let field_type = fields_dict.get_field_type_by_name(struct_name, field_name, self.interner)?;
    Some(field_type.type_repr.clone())
}

// 使用箇所
let field_type = self.lookup_field_type_repr(struct_name, *member)
    .map(Box::new);
```

## Phase 7: 一致型キャッシュの変更

`consistent_type_cache` も `String` から `TypeRepr` に変更：

### 変更前
```rust
consistent_type_cache: HashMap<InternedStr, Option<String>>,
```

### 変更後
```rust
consistent_type_cache: HashMap<InternedStr, Option<TypeRepr>>,
```

## Phase 8: sv_u フィールド型の変更

`sv_u_field_types` も同様に変更：

### 変更前
```rust
sv_u_field_types: HashMap<InternedStr, String>,
```

### 変更後
```rust
sv_u_field_types: HashMap<InternedStr, TypeRepr>,
```

## Phase 9: apidoc.rs の TypeRepr 化

**ファイル**: `src/apidoc.rs`

`ApidocArg` の型を文字列から TypeRepr に変更：

### 変更前
```rust
pub struct ApidocArg {
    pub name: String,
    pub ty: String,  // 例: "const COP *"
    pub nullability: Nullability,
}
```

### 変更後
```rust
pub struct ApidocArg {
    pub name: String,
    pub type_repr: TypeRepr,  // パース済みの型情報
    pub nullability: Nullability,
}
```

パース時に `TypeRepr::from_apidoc_string()` を呼び出し、以降は TypeRepr として扱う。

## Phase 10: rust_decl.rs の TypeRepr 化

**ファイル**: `src/rust_decl.rs`

各構造体の `ty: String` を `type_repr: TypeRepr` に変更：

```rust
pub struct RustFnArg { pub ty: String, ... }  // → type_repr: TypeRepr
pub struct RustFn { pub ty: String, ... }     // → type_repr: TypeRepr
pub struct RustConst { pub ty: String, ... }  // → type_repr: TypeRepr
pub struct RustType { pub ty: String, ... }   // → type_repr: TypeRepr
```

パース時に `TypeRepr::from_rust_string()` を呼び出し（Rust 形式のパーサが必要）。

## Phase 11: macro_infer.rs の TypeRepr 化

**ファイル**: `src/macro_infer.rs`

`SvAnyPattern` の型を TypeRepr に変更：

### 変更前
```rust
pub struct SvAnyPattern {
    pub cast_type: String,  // 例: "XPVAV"
    // ...
}
```

### 変更後
```rust
pub struct SvAnyPattern {
    pub cast_type_repr: TypeRepr,
    // ...
}
```

## Phase 12: InferredType のベース型を TypeRepr に変更

**ファイル**: `src/type_repr.rs`

`MemberAccess` と `PtrMemberAccess` の `base_type` を `String` から `Box<TypeRepr>` に変更：

### 変更前
```rust
MemberAccess {
    base_type: String,
    member: InternedStr,
    field_type: Option<Box<TypeRepr>>,
},
PtrMemberAccess {
    base_type: String,
    member: InternedStr,
    field_type: Option<Box<TypeRepr>>,
    used_consistent_type: bool,
},
```

### 変更後
```rust
MemberAccess {
    base_type: Box<TypeRepr>,
    member: InternedStr,
    field_type: Option<Box<TypeRepr>>,
},
PtrMemberAccess {
    base_type: Box<TypeRepr>,
    member: InternedStr,
    field_type: Option<Box<TypeRepr>>,
    used_consistent_type: bool,
},
```

## Phase 10: semantic.rs の型計算を TypeRepr ベースに変更

**ファイル**: `src/semantic.rs`

### 変更前
```rust
fn get_expr_type_str(&self, expr_id: ExprId, type_env: &TypeEnv) -> String
fn compute_binary_type_str(&self, ...) -> String
fn compute_conditional_type_str(&self, ...) -> String
```

### 変更後
```rust
fn get_expr_type_repr(&self, expr_id: ExprId, type_env: &TypeEnv) -> TypeRepr
fn compute_binary_type_repr(&self, ...) -> TypeRepr
fn compute_conditional_type_repr(&self, ...) -> TypeRepr
```

これにより `from_apidoc_string` の呼び出しを削減。

## 変更対象ファイル一覧

| ファイル | 変更内容 |
|----------|----------|
| `src/type_repr.rs` | `CTypeSpecs::from_decl_specs`, `CDerivedType::from_declarator`, `from_rust_string` 追加 |
| `src/type_repr.rs` | `InferredType` の `base_type` を `Box<TypeRepr>` に変更 |
| `src/fields_dict.rs` | `FieldType` を TypeRepr ベースに変更、関連メソッド更新 |
| `src/semantic.rs` | 型計算を TypeRepr ベースに変更、`from_apidoc_string` 呼び出しを削減 |
| `src/apidoc.rs` | `ApidocArg::ty` を TypeRepr に変更 |
| `src/rust_decl.rs` | 各構造体の `ty` を TypeRepr に変更 |
| `src/macro_infer.rs` | `SvAnyPattern::cast_type` を TypeRepr に変更 |

## 実装順序

### Step 1: 基盤整備（type_repr.rs）
1. `CTypeSpecs::from_decl_specs` を実装
2. `CDerivedType::from_declarator` を実装
3. `TypeRepr::from_rust_string` を実装（Rust 形式 `*mut T` のパース）

### Step 2: 外部データ読み込みの TypeRepr 化（汚染の起点を封じる）
4. `apidoc.rs`: `ApidocArg::ty` → `ApidocArg::type_repr`
5. `rust_decl.rs`: 各構造体の `ty` → `type_repr`
6. `macro_infer.rs`: `SvAnyPattern::cast_type` → `cast_type_repr`

### Step 3: FieldsDict の TypeRepr 化
7. `FieldType::rust_type: String` → `FieldType::type_repr: TypeRepr`
8. `extract_field_type()` を TypeRepr を返すように変更
9. `consistent_type_cache` を `Option<TypeRepr>` に変更
10. `sv_u_field_types` を `TypeRepr` に変更

### Step 4: semantic.rs の TypeRepr 化
11. `lookup_field_type_by_name` → `lookup_field_type_repr`
12. `get_expr_type_str` → `get_expr_type_repr`
13. `compute_binary_type_str` → `compute_binary_type_repr`
14. `compute_conditional_type_str` → `compute_conditional_type_repr`
15. `collect_expr_constraints` 内の `from_apidoc_string` 呼び出しを削除

### Step 5: InferredType の TypeRepr 化
16. `MemberAccess`, `PtrMemberAccess` の `base_type` を `Box<TypeRepr>` に変更
17. 関連する `to_display_string`, `to_rust_string` を更新

### Step 6: クリーンアップ
18. デバッグコードを削除
19. テスト実行

## 期待される効果

- 型情報の文字列変換によるエラーが解消
- `CopFILE` の戻り値型が正しく `*mut c_char` になる
- 型情報の一貫性が向上
- 将来的な型推論の拡張が容易になる
- `from_apidoc_string` の呼び出しが大幅に削減（22箇所 → 数箇所）

## 注意事項

### 外部データ読み込み時の変換

外部ファイル（apidoc、Rust bindings）からの読み込み時点で即座に TypeRepr に変換する。
これにより、文字列形式の型情報がシステム内部に流入することを防ぐ。

- `apidoc.rs`: パース時に `TypeRepr::from_apidoc_string()` で変換
- `rust_decl.rs`: パース時に `TypeRepr::from_rust_string()` で変換

### 変更の影響範囲

この変更は以下のファイルに影響：

- `type_repr.rs`: 変換メソッド追加、InferredType 変更
- `fields_dict.rs`: FieldType を TypeRepr ベースに変更
- `semantic.rs`: 型計算を TypeRepr ベースに変更
- `apidoc.rs`: ApidocArg を TypeRepr ベースに変更
- `rust_decl.rs`: 各構造体を TypeRepr ベースに変更
- `macro_infer.rs`: SvAnyPattern を TypeRepr ベースに変更

外部 API（`InferResult`、`MacroInferInfo` 等）への影響は最小限。
