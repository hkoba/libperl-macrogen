# 型の文字列ラウンドトリップ廃止計画

## 問題の本質

`collect_expr_constraints` 内の多くのハンドラが、式の型を **文字列** で受け渡している。

```
現状: TypeRepr → to_display_string() → String → from_apidoc_string() → TypeRepr
                      ↑                              ↑
                  情報が欠落                    不完全なパーサーで再パース
```

`get_expr_type_str` → `from_apidoc_string` のパターンが **17箇所** あり、
以下の問題を引き起こしている:

1. `struct xpvhv_with_aux *` → `"struct xpvhv_with_aux *"` → 構造体名に `struct` タグが残り field lookup が失敗
2. `union _xhvnameu` → `"union _xhvnameu"` → 同上
3. `SV * const` → `parse_c_type_string` がパースできない（Phase 1 で解決済み）

**文字列操作でタグを除去するのは対症療法であり、根本的解決ではない。**

## あるべき姿

TypeRepr を直接受け渡し、構造体名は `InternedStr` で扱う。

```
あるべき姿: TypeRepr → (直接参照) → TypeRepr
            CTypeSpecs::Struct { name: InternedStr } → FieldsDict.field_types[(InternedStr, InternedStr)]
```

`FieldsDict` は既に `InternedStr` ベースの API を持っている:
- `field_types: HashMap<(InternedStr, InternedStr), FieldType>` (line 26)
- `resolve_typedef(typedef_name: InternedStr) -> Option<InternedStr>` (line 341)

## 完了済み

### Phase 1: インライン関数パラメータ・戻り値の直接変換 ✅

AST (ParamDecl/DeclSpecs) → TypeRepr の直接変換パスを構築。
`lookup_inline_fn_param_type_repr`, `lookup_inline_fn_return_type_repr` を追加し、
旧 `lookup_inline_fn_param_type`, `lookup_inline_fn_return_type` を削除。

### Cast 式の直接変換 ✅

`ExprKind::Cast` ハンドラで `resolve_type_name()` → `display()` → `from_apidoc_string()` を
`CTypeSpecs::from_decl_specs()` + `CDerivedType::from_derived_decls()` に置き換え。

## 実装計画

### Phase 2: TypeRepr 直接受け渡し基盤

`get_expr_type_str` に代わる、TypeRepr を直接返すメソッドを追加する。

#### Step 2a: `get_expr_type_repr` の追加

```rust
fn get_expr_type_repr(&self, expr_id: ExprId, type_env: &TypeEnv) -> Option<TypeRepr> {
    type_env.expr_constraints.get(&expr_id)
        .and_then(|c| c.first())
        .map(|c| c.ty.clone())
}
```

#### Step 2b: `TypeRepr` に型操作メソッドを追加

文字列操作の代わりに、TypeRepr から直接型情報を取得するメソッド群。

```rust
impl TypeRepr {
    /// ポインタ型の参照先の構造体/typedef 名を InternedStr で取得
    /// PtrMember (->), Deref (*) の base 型から構造体名を抽出するために使用
    fn pointee_name(&self) -> Option<InternedStr> {
        match self {
            TypeRepr::CType { specs, derived, .. } => {
                // ポインタが1段以上ある場合、最外ポインタを剥がした型名を返す
                if derived.iter().any(|d| matches!(d, CDerivedType::Pointer { .. })) {
                    specs.type_name()
                } else {
                    None
                }
            }
            TypeRepr::RustType { repr, .. } => repr.pointee_name(),
            TypeRepr::Inferred(inferred) => inferred.resolved_type()?.pointee_name(),
        }
    }

    /// 非ポインタ型の構造体/typedef 名を InternedStr で取得
    /// Member (.) の base 型から構造体名を抽出するために使用
    fn type_name(&self) -> Option<InternedStr> {
        match self {
            TypeRepr::CType { specs, .. } => specs.type_name(),
            TypeRepr::RustType { repr, .. } => repr.type_name(),
            TypeRepr::Inferred(inferred) => inferred.resolved_type()?.type_name(),
        }
    }
}

impl CTypeSpecs {
    /// 構造体/typedef 名を InternedStr で取得
    fn type_name(&self) -> Option<InternedStr> {
        match self {
            CTypeSpecs::Struct { name: Some(n), .. } => Some(*n),
            CTypeSpecs::TypedefName(n) => Some(*n),
            CTypeSpecs::Enum { name: Some(n) } => Some(*n),
            _ => None,
        }
    }
}

impl InferredType {
    /// Inferred ラッパーを解決して内側の TypeRepr を返す
    fn resolved_type(&self) -> Option<&TypeRepr> {
        match self {
            InferredType::Cast { target_type } => Some(target_type),
            InferredType::PtrMemberAccess { field_type: Some(ft), .. } => Some(ft),
            InferredType::MemberAccess { field_type: Some(ft), .. } => Some(ft),
            InferredType::ArraySubscript { element_type, .. } => Some(element_type),
            InferredType::AddressOf { inner_type } => Some(inner_type),
            InferredType::Dereference { pointer_type } => Some(pointer_type),
            InferredType::SymbolLookup { resolved_type, .. } => Some(resolved_type),
            InferredType::IncDec { inner_type } => Some(inner_type),
            InferredType::Assignment { lhs_type } => Some(lhs_type),
            InferredType::Comma { rhs_type } => Some(rhs_type),
            InferredType::Conditional { result_type, .. } => Some(result_type),
            InferredType::BinaryOp { result_type, .. } => Some(result_type),
            InferredType::UnaryArithmetic { inner_type } => Some(inner_type),
            InferredType::CompoundLiteral { type_name } => Some(type_name),
            InferredType::StmtExpr { last_expr_type } => last_expr_type.as_ref(),
            _ => None,
        }
    }
}

impl RustTypeRepr {
    fn pointee_name(&self) -> Option<InternedStr> { ... }
    fn type_name(&self) -> Option<InternedStr> { ... }
}
```

#### Step 2c: `FieldsDict` に `InternedStr` ベースのフィールド型取得を追加

既存の `field_types.get(&(struct_name, field_name))` (line 308) を公開 API にし、
typedef 解決も `InternedStr` ベースで行う。

```rust
impl FieldsDict {
    /// InternedStr で直接フィールド型を取得（typedef 解決付き）
    pub fn get_field_type(
        &self,
        struct_name: InternedStr,
        field_name: InternedStr,
    ) -> Option<&FieldType> {
        // 直接検索
        if let Some(ft) = self.field_types.get(&(struct_name, field_name)) {
            return Some(ft);
        }
        // typedef 解決して再検索
        let resolved = self.resolve_typedef(struct_name)?;
        self.field_types.get(&(resolved, field_name))
    }
}
```

### Phase 3: PtrMember / Member ハンドラの書き換え

`get_expr_type_str` + `extract_struct_name_from_pointer_type` + `lookup_field_type_repr`
の文字列チェーンを、TypeRepr ベースに置き換える。

#### PtrMember (->)

```rust
// 現状（文字列ベース）
let base_ty = self.get_expr_type_str(base.id, type_env);       // → "XPVHV *"
let struct_name = self.extract_struct_name_from_pointer_type(&base_ty); // → "XPVHV"
let field_type = self.lookup_field_type_repr(struct_name, *member);     // 文字列で検索

// 新（TypeRepr ベース）
let base_type = self.get_expr_type_repr(base.id, type_env);
let struct_name = base_type.as_ref().and_then(|t| t.pointee_name()); // → InternedStr
let field_type = struct_name.and_then(|n| {
    self.fields_dict?.get_field_type(n, *member)
}).map(|ft| ft.type_repr.clone());
```

#### Member (.)

```rust
// 新（TypeRepr ベース）
let base_type = self.get_expr_type_repr(base.id, type_env);
let struct_name = base_type.as_ref().and_then(|t| t.type_name()); // → InternedStr
let field_type = struct_name.and_then(|n| {
    self.fields_dict?.get_field_type(n, *member)
}).map(|ft| ft.type_repr.clone());
```

### Phase 4: 残りのハンドラの書き換え

`get_expr_type_str` → `from_apidoc_string` パターンを使う残りの箇所を
`get_expr_type_repr` に順次切り替え。

| ハンドラ | 現状 | 新 |
|----------|------|-----|
| AddrOf | `get_expr_type_str` → `from_apidoc_string` → AddressOf | `get_expr_type_repr` → AddressOf |
| Deref | 同上 → Dereference | `get_expr_type_repr` → Dereference |
| Index | 同上 → ArraySubscript | `get_expr_type_repr` → ArraySubscript |
| Assign | 同上 → Assignment | `get_expr_type_repr` → Assignment |
| Comma | 同上 → Comma | `get_expr_type_repr` → Comma |
| IncDec | 同上 → IncDec | `get_expr_type_repr` → IncDec |
| UnaryMinus/Plus | 同上 → UnaryArithmetic | `get_expr_type_repr` → UnaryArithmetic |
| BitwiseNot | 同上 → UnaryArithmetic | `get_expr_type_repr` → UnaryArithmetic |
| Conditional | 同上 → Conditional | `get_expr_type_repr` → Conditional |
| StmtExpr | 同上 → StmtExpr | `get_expr_type_repr` → StmtExpr |

### Phase 5: 不要コードの整理

| 対象 | 状態 |
|------|------|
| `get_expr_type_str` | 全箇所が `get_expr_type_repr` に移行後、削除 |
| `extract_struct_name_from_pointer_type` | TypeRepr::pointee_name() に置き換え後、削除 |
| `lookup_field_type_repr` (文字列ベース) | InternedStr ベースに置き換え後、削除 |
| `resolve_typedef_by_name` (文字列ベース) | `resolve_typedef` (InternedStr) のみ残す |
| `get_field_type_by_name` (文字列ベース) | `get_field_type` (InternedStr) のみ残す |

## RustTypeRepr の課題

`RustTypeRepr` は `InternedStr` を持たず `String` ベースで型名を格納している。
`pointee_name()` / `type_name()` の実装には `interner.lookup()` が必要になるが、
これは Phase 2 では対応せず、将来的に `RustTypeRepr` 自体を `InternedStr`
ベースに改修する際に対応する。当面は `None` を返してフォールバックさせる。

## 注意点

- `from_apidoc_string` は apidoc (embed.fnc) の外部入力パースに使われるため削除しない
- `get_expr_type_str` は S-expression 出力等の表示目的で残す可能性がある
- Phase 2-3 で HvNAME_HEK_NN の `()` 問題が解決する
