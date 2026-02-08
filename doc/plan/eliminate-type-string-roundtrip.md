# 型の文字列ラウンドトリップ廃止計画

## 問題の本質

インライン関数のパラメータ型を制約に変換する際、フルスペックの C パーサーが
生成した AST があるにもかかわらず、**文字列経由の不完全なパーサー** で再パースしている。

```
現状: AST (ParamDecl) → Type → String → parse_c_type_string → TypeRepr
                                  ↑
                          ここで情報が欠落
                          (例: "SV * const" がパースできない)
```

`TypeRepr::from_decl` という **直接変換パス** が既に存在しており、
`CTypeSpecs::from_decl_specs` と `CDerivedType::from_derived_decls` は
AST から直接 `TypeRepr` を構築できる。

```
あるべき姿: AST (ParamDecl) → TypeRepr（直接変換）
```

## 現状のコードフロー

### collect_call_constraints の InlineFnDict セクション

```rust
// semantic.rs (現状)
if let Some(ty) = self.lookup_inline_fn_param_type(func_name, i) {
    let ty_str = ty.display(self.interner);                    // Type → String
    let (specs, derived) = self.parse_c_type_for_inline_fn(&ty_str);  // String → TypeRepr
    // ...
}
```

### lookup_inline_fn_param_type

```rust
// semantic.rs (現状)
fn lookup_inline_fn_param_type(&self, func_name: InternedStr, arg_index: usize) -> Option<Type> {
    // ParamDecl を取得
    let param = param_list.params.get(arg_index)?;
    // DeclSpecs → Type → apply_declarator → Type
    let base_ty = self.resolve_decl_specs_readonly(&param.specs);
    // ...
}
```

### 既存の直接変換パス

```rust
// type_repr.rs (既に存在)
pub fn from_decl(specs: &DeclSpecs, declarator: &Declarator, interner: &StringInterner) -> Self {
    let c_specs = CTypeSpecs::from_decl_specs(specs, interner);
    let derived = CDerivedType::from_derived_decls(&declarator.derived);
    TypeRepr::CType { specs: c_specs, derived, source: CTypeSource::Header }
}
```

## 実装計画

### Phase 1: lookup_inline_fn_param_type を TypeRepr 返却に変更

`Type` ではなく `TypeRepr` を直接返す関数を追加する。

```rust
fn lookup_inline_fn_param_type_repr(
    &self,
    func_name: InternedStr,
    arg_index: usize,
) -> Option<TypeRepr> {
    let dict = self.inline_fn_dict?;
    let func_def = dict.get(func_name)?;

    let param_list = func_def.declarator.derived.iter()
        .find_map(|d| match d {
            DerivedDecl::Function(params) => Some(params),
            _ => None,
        })?;

    let param = param_list.params.get(arg_index)?;

    // AST → TypeRepr 直接変換（文字列を経由しない）
    let specs = CTypeSpecs::from_decl_specs(&param.specs, self.interner);
    let derived = param.declarator.as_ref()
        .map(|d| CDerivedType::from_derived_decls(&d.derived))
        .unwrap_or_default();

    // Function 派生型は除外（パラメータ自体の型のみ必要）
    let derived = derived.into_iter()
        .take_while(|d| !matches!(d, CDerivedType::Function { .. }))
        .collect();

    Some(TypeRepr::CType {
        specs,
        derived,
        source: CTypeSource::InlineFn { func_name },
    })
}
```

### Phase 2: collect_call_constraints を直接変換に切り替え

```rust
// 引数の型
for (i, arg) in args.iter().enumerate() {
    if let Some(type_repr) = self.lookup_inline_fn_param_type_repr(func_name, i) {
        let constraint = TypeEnvConstraint::new(
            arg.id,
            type_repr,
            format!("arg {} of inline {}()", i, func_name_str),
        );
        type_env.add_constraint(constraint);
    }
}
```

### Phase 3: 戻り値型も同様に直接変換

`lookup_inline_fn_return_type` も同様に `TypeRepr` を直接返す版を追加。

```rust
fn lookup_inline_fn_return_type_repr(
    &self,
    func_name: InternedStr,
) -> Option<TypeRepr> {
    let dict = self.inline_fn_dict?;
    let func_def = dict.get(func_name)?;

    let specs = CTypeSpecs::from_decl_specs(&func_def.specs, self.interner);

    // Declarator の Function より前の derived 部分のみ
    let derived: Vec<_> = func_def.declarator.derived.iter()
        .take_while(|d| !matches!(d, DerivedDecl::Function(_)))
        .map(|d| match d {
            DerivedDecl::Pointer(quals) => CDerivedType::Pointer {
                is_const: quals.is_const,
                is_volatile: quals.is_volatile,
                is_restrict: quals.is_restrict,
            },
            _ => unreachable!(),
        })
        .collect();

    Some(TypeRepr::CType {
        specs,
        derived,
        source: CTypeSource::InlineFn { func_name },
    })
}
```

### Phase 4: 不要になったコードの整理

以下の関数・メソッドの使用箇所を確認し、インライン関数処理でのみ使われていた場合は削除を検討:

| 対象 | 用途 |
|------|------|
| `lookup_inline_fn_param_type` (Type 返却版) | 直接変換版で置き換え |
| `lookup_inline_fn_return_type` (Type 返却版) | 直接変換版で置き換え |
| `parse_c_type_for_inline_fn` | 不要に |
| `Type::display` for Pointer/TypedefName | 他で使用されていなければ不要 |
| `parse_c_type_string` の修飾子パース | apidoc 用途では残る |

## 影響範囲

| ファイル | 変更内容 |
|----------|----------|
| `src/semantic.rs` | `lookup_inline_fn_param_type_repr` 追加、`collect_call_constraints` 書き換え |
| `src/type_repr.rs` | `CDerivedType::from_derived_decls` を `pub` に（必要なら） |

## 期待される効果

1. **`SV * const` 問題が根本解決**: 文字列パースを経由しないため、修飾子の位置に起因するバグが発生しない
2. **関数ポインタパラメータも正しく処理**: `parse_c_type_string` が対応できない複雑な型も AST から直接変換
3. **情報の欠落がなくなる**: `Type::display()` → `parse_c_type_string` の往復で失われていた情報が保持される
4. **コードの簡潔化**: 変換ステップが減り、バグの入り込む余地が減る

## 注意点

- `parse_c_type_string` は apidoc 文字列のパース（外部入力）にも使われているため、完全に削除はできない
- `Type` 型自体は意味解析の他の部分で使われているため、`resolve_decl_specs_readonly` 等は残す
