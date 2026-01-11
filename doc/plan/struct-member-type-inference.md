# 構造体メンバーアクセスの型推論

## 目標

1. `PtrMember` / `Member` 式で、ベース型からメンバーの型を推論する
2. THX 由来の `my_perl` を `*mut PerlInterpreter` として扱い、そのメンバーアクセスの型を推論する

## 背景

### 現状の問題

```
SvCUR: expression (4 constraints, 1 uses)
  (ptr-member
    (cast (type-name
      (decl-specs (typedef-name XPV)) (abstract-declarator (pointer)))
      (call
        (ident SvANY) :type <unknown>
        (ident sv) :type SV*) :type <unknown>) :type XPV* xpv_cur) :type <unknown>
  expr#41710: <unknown> (pointer member access)
  expr#41709: XPV* (cast expression)
  expr#41707: SV* (symbol lookup)
```

- キャスト式の型 `XPV*` は正しく推論されている
- しかし `xpv_cur` メンバーの型が `<unknown>` のまま
- `FieldsDict` には `(XPV, xpv_cur)` → `STRLEN` の情報がある

### 利用可能なリソース

1. **FieldsDict**: `get_field_type(struct_name, field_name)` で `FieldType { rust_type: String }` を取得可能
2. **SemanticAnalyzer**: `fields_dict: Option<&'a FieldsDict>` フィールドを持つ
3. **type_env**: ベース式の型が既に格納されている（子式を先に処理するため）

## 実装計画

### Step 1: FieldsDict に文字列ベースのルックアップ追加

**src/fields_dict.rs:**

`StringInterner` は immutable 参照のため、`InternedStr` への変換ができない。
文字列ベースのルックアップメソッドを追加する。

```rust
/// 構造体名（文字列）とフィールド名からフィールド型を取得
pub fn get_field_type_by_name(
    &self,
    struct_name_str: &str,
    field_name: InternedStr,
    interner: &StringInterner,
) -> Option<&FieldType> {
    // field_types を走査して構造体名が一致するものを探す
    for ((s_name, f_name), field_type) in &self.field_types {
        if interner.get(*s_name) == struct_name_str && *f_name == field_name {
            return Some(field_type);
        }
    }
    None
}
```

### Step 2: ポインタ型から構造体名を抽出するヘルパー追加

**src/semantic.rs:**

```rust
/// ポインタ型文字列から構造体名を抽出（文字列として返す）
/// 例: "XPV*" → Some("XPV"), "* mut SV" → Some("SV")
fn extract_struct_name_from_pointer_type<'b>(&self, ty_str: &'b str) -> Option<&'b str> {
    let trimmed = ty_str.trim();

    // "*mut TYPE" または "* mut TYPE" 形式
    if let Some(rest) = trimmed.strip_prefix("*mut ").or_else(|| trimmed.strip_prefix("* mut ")) {
        return Some(rest.trim());
    }

    // "TYPE*" 形式
    if let Some(base) = trimmed.strip_suffix('*') {
        return Some(base.trim());
    }

    None
}
```

### Step 3: メンバーアクセスの型推論メソッド追加

**src/semantic.rs:**

```rust
/// 構造体メンバーの型を取得（FieldsDict から、構造体名は文字列）
fn lookup_field_type_by_name(&self, struct_name: &str, field_name: InternedStr) -> Option<String> {
    let fields_dict = self.fields_dict?;
    let field_type = fields_dict.get_field_type_by_name(struct_name, field_name, self.interner)?;
    Some(field_type.rust_type.clone())
}
```

### Step 4: PtrMember の型推論を実装

**src/semantic.rs の `collect_expr_constraints` 内:**

```rust
// ポインタメンバーアクセス
ExprKind::PtrMember { expr: base, member } => {
    self.collect_expr_constraints(base, type_env);

    // ベース型からメンバー型を推論
    let base_ty = self.get_expr_type_str(base.id, type_env);
    let member_ty = if let Some(struct_name) = self.extract_struct_name_from_pointer_type(&base_ty) {
        self.lookup_field_type_by_name(struct_name, *member)
            .unwrap_or_else(|| "<unknown>".to_string())
    } else {
        "<unknown>".to_string()
    };

    type_env.add_constraint(TypeEnvConstraint::new(
        expr.id, &member_ty, ConstraintSource::Inferred,
        format!("{}->{}",base_ty, self.interner.get(*member))
    ));
}
```

### Step 5: Member（直接メンバーアクセス）も同様に実装

```rust
// メンバーアクセス (x.field)
ExprKind::Member { expr: base, member } => {
    self.collect_expr_constraints(base, type_env);

    let base_ty = self.get_expr_type_str(base.id, type_env);
    // 直接アクセスの場合、base_ty は構造体名そのもの
    let member_ty = self.lookup_field_type_by_name(&base_ty, *member)
        .unwrap_or_else(|| "<unknown>".to_string());

    type_env.add_constraint(TypeEnvConstraint::new(
        expr.id, &member_ty, ConstraintSource::Inferred,
        format!("{}.{}", base_ty, self.interner.get(*member))
    ));
}
```

### Step 6: THX/my_perl のデフォルト型を設定

**src/semantic.rs の `collect_expr_constraints` 内、`ExprKind::Ident` の処理:**

```rust
ExprKind::Ident(name) => {
    let name_str = self.interner.get(*name);

    // シンボルテーブルから型を取得
    if let Some(sym) = self.lookup_symbol(*name) {
        let ty_str = sym.ty.display(self.interner);
        type_env.add_constraint(TypeEnvConstraint::new(
            expr.id, &ty_str, ConstraintSource::Inferred, "symbol lookup"
        ));
    } else if name_str == "my_perl" {
        // THX 由来の my_perl はデフォルトで *mut PerlInterpreter
        type_env.add_constraint(TypeEnvConstraint::new(
            expr.id, "*mut PerlInterpreter", ConstraintSource::Inferred,
            "THX default type"
        ));
    }

    // パラメータ参照の場合、ExprId とパラメータを紐付け
    if self.is_macro_param(*name) {
        type_env.link_expr_to_param(expr.id, *name, "parameter reference");
    }
}
```

## 修正対象ファイル

1. **src/fields_dict.rs**
   - `get_field_type_by_name` メソッド追加（文字列ベースのルックアップ）

2. **src/semantic.rs**
   - `extract_struct_name_from_pointer_type` ヘルパー追加
   - `lookup_field_type_by_name` ヘルパー追加
   - `ExprKind::PtrMember` の型推論を実装
   - `ExprKind::Member` の型推論を実装
   - `ExprKind::Ident` で `my_perl` のデフォルト型を設定

## 期待される結果

```
SvCUR: expression (5 constraints, 1 uses)
  (ptr-member
    (cast (type-name
      (decl-specs (typedef-name XPV)) (abstract-declarator (pointer)))
      (call
        (ident SvANY) :type <unknown>
        (ident sv) :type SV*) :type <unknown>) :type XPV* xpv_cur) :type STRLEN
  expr#41710: STRLEN (XPV*->xpv_cur)
  expr#41709: XPV* (cast expression)
  expr#41707: SV* (symbol lookup)
```

THX 依存マクロでの `my_perl->...` も同様に型推論される：
```
(ptr-member
  (ident my_perl) :type *mut PerlInterpreter
  field_name) :type <field_type>
```

## 注意点

1. `FieldsDict` は `InternedStr` を使うため、型文字列から構造体名を抽出後に `intern` する必要がある
2. `fields_dict` は `Option` なので、`None` の場合は `<unknown>` を返す
3. Rust形式の型 (`*mut TYPE`) と C形式の型 (`TYPE*`) の両方に対応する
