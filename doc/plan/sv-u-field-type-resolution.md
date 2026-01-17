# sv_u ユニオンフィールドの型解決

## 背景

現在、`sv->sv_u.svu_pv` のような式の型は以下のように表示される：

```
SvPVXx: expression (5 constraints, 0 uses)
  (member
    (ptr-member
      (ident sv) :type SV * sv_u) :type SV *->sv_u svu_pv) :type SV *->sv_u.svu_pv
```

`sv_u.svu_pv` の型が `SV *->sv_u.svu_pv` という文字列表現になっており、
実際のフィールド型（`char*`）が表示されていない。

## sv_u union の定義（sv.h より）

```c
union {
    char*   svu_pv;         /* pointer to malloced string */
    IV      svu_iv;
    UV      svu_uv;
    _NV_BODYLESS_UNION
    SV*     svu_rv;         /* pointer to another SV */
    SV**    svu_array;
    HE**    svu_hash;
    GP*     svu_gp;
    PerlIO* svu_fp;
}   sv_u;
```

## 設計方針

### 前提知識の活用

1. `parse_each` で SV ファミリー構造体を網羅的に検出済み
2. SV ファミリーは全て同一の `sv_u` union を保持
3. `sv_u` は常に `->sv_u.field` パターンでアクセスされる

### アプローチ: AST パターンマッチング + sv_u 辞書

`Member` アクセスの `base` が `PtrMember` で member が `sv_u` の場合を特別扱いし、
sv_u 辞書から型を解決する。

## AST パターン

`sv->sv_u.svu_pv` の AST 構造：

```
(member                          <- 外側：.svu_pv アクセス
  (ptr-member                    <- 内側：->sv_u アクセス
    (ident sv)
    sv_u)
  svu_pv)
```

注目点：
- 外側は `ExprKind::Member { base, member: svu_field }`
- `base` が `ExprKind::PtrMember { member: sv_u, ... }` の場合に特殊処理

## 実装計画

### Phase 1: sv_u union フィールド辞書の作成

`fields_dict.rs` に sv_u union のフィールド情報を追加：

```rust
/// sv_u ユニオンフィールドの型マッピング
///
/// SV ファミリー構造体検出時に構築される。
/// key: フィールド名 (InternedStr)
/// value: フィールドの C 型文字列
sv_u_field_types: HashMap<InternedStr, String>,

/// sv_u ユニオンフィールドの型を登録
pub fn register_sv_u_field(&mut self, field_name: InternedStr, c_type: String) {
    self.sv_u_field_types.insert(field_name, c_type);
}

/// sv_u ユニオンフィールドの型を取得
pub fn get_sv_u_field_type(&self, field_name: InternedStr) -> Option<&str> {
    self.sv_u_field_types.get(&field_name).map(|s| s.as_str())
}
```

### Phase 2: parse_each での sv_u フィールド収集

`_SV_HEAD` マクロ検出時に sv_u union のフィールド情報も収集する。

方法1: `_SV_HEAD` の定義から sv_u union を抽出
方法2: ハードコードされたマッピングを登録

実装の簡便さから、方法2を採用：

```rust
// main.rs の parse_each 処理後、SV ファミリー検出時に呼び出し
fn register_sv_u_field_types(fields_dict: &mut FieldsDict, interner: &mut StringInterner) {
    // sv_u union のフィールド型を登録
    let mappings = [
        ("svu_pv", "char*"),
        ("svu_iv", "IV"),
        ("svu_uv", "UV"),
        ("svu_rv", "SV*"),
        ("svu_array", "SV**"),
        ("svu_hash", "HE**"),
        ("svu_gp", "GP*"),
        ("svu_fp", "PerlIO*"),
    ];

    for (field, c_type) in mappings {
        let field_id = interner.intern(field);
        fields_dict.register_sv_u_field(field_id, c_type.to_string());
    }
}
```

### Phase 3: semantic.rs での特殊パターン処理

`collect_expr_constraints` の `ExprKind::Member` ケースを拡張：

```rust
ExprKind::Member { expr: base, member } => {
    self.collect_expr_constraints(base, type_env);

    let base_ty = self.get_expr_type_str(base.id, type_env);
    let member_name = self.interner.get(*member);

    // sv_u フィールドアクセスの特殊処理
    // base が ->sv_u パターンの場合、sv_u 辞書から型を解決
    let member_ty_str = if self.is_sv_u_access(base) {
        self.lookup_sv_u_field_type(*member)
            .unwrap_or_else(|| "<unknown>".to_string())
    } else {
        self.lookup_field_type_by_name(&base_ty, *member)
            .unwrap_or_else(|| "<unknown>".to_string())
    };

    // ... 残りの処理
}

/// base が ->sv_u アクセスかどうかを判定
fn is_sv_u_access(&self, base: &Expr) -> bool {
    if let ExprKind::PtrMember { member, .. } = &base.kind {
        let sv_u_id = self.interner.lookup("sv_u");
        sv_u_id.map_or(false, |id| *member == id)
    } else {
        false
    }
}

/// sv_u フィールドの型を取得
fn lookup_sv_u_field_type(&self, field: InternedStr) -> Option<String> {
    self.fields_dict?
        .get_sv_u_field_type(field)
        .map(|s| s.to_string())
}
```

### Phase 4: テストと動作確認

1. `cargo test` で既存テストが通ることを確認
2. 実際の出力で型が正しく解決されることを確認：
   ```
   SvPVXx: expression (...)
     (member
       (ptr-member
         (ident sv) :type SV * sv_u) :type SV *->sv_u svu_pv) :type char*
   ```

## 期待される効果

| アクセスパターン | 現在の型 | 期待される型 |
|------------------|----------|--------------|
| `sv_u.svu_pv` | `*->sv_u.svu_pv` | `char*` |
| `sv_u.svu_iv` | `*->sv_u.svu_iv` | `IV` |
| `sv_u.svu_uv` | `*->sv_u.svu_uv` | `UV` |
| `sv_u.svu_rv` | `*->sv_u.svu_rv` | `SV*` |
| `sv_u.svu_array` | `*->sv_u.svu_array` | `SV**` |
| `sv_u.svu_hash` | `*->sv_u.svu_hash` | `HE**` |
| `sv_u.svu_gp` | `*->sv_u.svu_gp` | `GP*` |
| `sv_u.svu_fp` | `*->sv_u.svu_fp` | `PerlIO*` |

## 既存機能との関係

### macro_infer.rs の sv_u_field_to_type()

- 目的: 引数がどの SV ファミリー型か推論（svu_hash → HV）
- 対象: マクロパラメータの型推論

### 本機能の sv_u フィールド型解決

- 目的: フィールドアクセス結果の C 型を解決（svu_hash → HE**）
- 対象: 式の型推論

両者は補完的な役割を果たす。
