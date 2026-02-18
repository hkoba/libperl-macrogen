# Plan: SV ファミリーキャストからのパラメータ型推論

## 問題

`CvDEPTH(sv)` のように、パラメータの型が apidoc にも bindings.rs にも存在しない場合、
パラメータ型は `/* unknown */` となる:

```rust
// 現在の出力（コメントアウト）
// pub unsafe fn CvDEPTH(sv: /* unknown */) -> I32 {
//     unsafe {
//         (*Perl_CvDEPTH((sv as *const CV)))
//     }
// }
```

しかし、マクロ本体に `(CV*)sv` というキャストが存在し、`CV` は SV ファミリーの
typedef 名であるため、`sv` は `*mut SV` 型であると推論できる。

## 背景

### SV ファミリーとは

Perl の内部構造体で `_SV_HEAD` マクロを使用する構造体群:
- `sv`, `av`, `hv`, `cv`, `gv`, `io` 等（全9メンバー）
- 対応する typedef: `SV`, `AV`, `HV`, `CV`, `GV`, `IO` 等

SV ファミリーの構造体はすべて `SV` へのアップキャストが安全（共通ヘッダを持つ）。

### 現在の型制約収集フロー

```
ExprKind::Cast { type_name: CV*, expr: Ident("sv") }

1. inner の Ident("sv") を処理:
   - SymbolLookup 制約を sv の ExprId に追加
   - link_expr_to_param(sv_expr_id, "sv") で紐付け

2. Cast 式自体を処理:
   - InferredType::Cast { target_type: CV* } を cast の ExprId に追加

問題: sv のパラメータには型制約が付かない
→ get_param_type() が param_to_exprs で sv_expr_id を探しても、
   sv_expr_id にあるのは SymbolLookup（未解決）のみ
```

### 判定に使えるデータ

- `FieldsDict::sv_family_members: HashSet<InternedStr>` — 構造体名（小文字: `cv`, `av`...）
- `FieldsDict::typedef_to_struct: HashMap<InternedStr, InternedStr>` — typedef → 構造体名（`CV` → `cv`）
- `FieldsDict::resolve_typedef()` — typedef 名から構造体名を解決

## 設計

### アプローチ

`collect_expr_constraints()` の `ExprKind::Cast` ハンドラで、以下の条件を満たす場合に
**内側の式に `*mut SV` 型制約を追加**する:

1. キャスト先がポインタ型である（`derived` に `Pointer` がある）
2. ポインタのベース型が SV ファミリーメンバーである
3. 内側の式がマクロパラメータの参照である

### SV ファミリー判定ロジック

キャスト先の `CTypeSpecs` から型名を取得し、SV ファミリーかどうかを判定:

```
CTypeSpecs::TypedefName(name) → resolve_typedef(name) → sv_family_members.contains()
CTypeSpecs::Struct { name, .. } → sv_family_members.contains(name)
```

### 追加する制約

内側の式の ExprId に `TypeRepr::CType { specs: TypedefName(SV), derived: [Pointer] }` を追加。
これにより `get_param_type()` が `param_to_exprs` 経由で `*mut SV` を発見できる。

## 実装

### ファイル: `src/fields_dict.rs`

#### ヘルパーメソッド追加

```rust
/// 型名（typedef 名または構造体名）が SV ファミリーかどうかを判定
pub fn is_sv_family_type(&self, type_name: InternedStr) -> bool {
    // 構造体名で直接チェック
    if self.sv_family_members.contains(&type_name) {
        return true;
    }
    // typedef 名 → 構造体名に解決してチェック
    if let Some(struct_name) = self.typedef_to_struct.get(&type_name) {
        return self.sv_family_members.contains(struct_name);
    }
    false
}
```

### ファイル: `src/semantic.rs`

#### `ExprKind::Cast` ハンドラの拡張

既存の Cast 処理の後に、SV ファミリーキャスト検出ロジックを追加:

```rust
ExprKind::Cast { type_name, expr: inner } => {
    self.collect_expr_constraints(inner, type_env);

    // 既存の Cast 制約追加（変更なし）
    let specs = CTypeSpecs::from_decl_specs(&type_name.specs, self.interner);
    let derived: Vec<CDerivedType> = type_name.declarator.as_ref()
        .map(|d| { ... })
        .unwrap_or_default();
    let target_type = TypeRepr::CType { specs: specs.clone(), derived: derived.clone(), ... };
    type_env.add_constraint(TypeEnvConstraint::new(
        expr.id,
        TypeRepr::Inferred(InferredType::Cast { target_type: Box::new(target_type) }),
        "cast expression",
    ));

    // 【新規】SV ファミリーキャストからのパラメータ型推論
    // (SV_FAMILY_TYPE *)param → param に *mut SV 制約を追加
    if let Some(fields_dict) = self.fields_dict {
        let is_ptr_cast = derived.iter().any(|d| matches!(d, CDerivedType::Pointer { .. }));
        if is_ptr_cast {
            if let Some(type_name_id) = specs.type_name() {
                if fields_dict.is_sv_family_type(type_name_id) {
                    // 内側の式がマクロパラメータの場合のみ
                    if let ExprKind::Ident(param_name) = &inner.kind {
                        if self.is_macro_param(*param_name) {
                            // *mut SV 型制約を追加
                            let sv_type = self.make_sv_ptr_type();
                            type_env.add_constraint(TypeEnvConstraint::new(
                                inner.id,
                                sv_type,
                                "SV family cast",
                            ));
                        }
                    }
                }
            }
        }
    }
}
```

#### `make_sv_ptr_type()` ヘルパー

```rust
/// *mut SV を表す TypeRepr を作成
fn make_sv_ptr_type(&self) -> TypeRepr {
    let sv_name = self.interner.lookup("SV")
        .expect("SV should be interned");
    TypeRepr::CType {
        specs: CTypeSpecs::TypedefName(sv_name),
        derived: vec![CDerivedType::Pointer { is_const: false }],
        source: CTypeSource::Inferred,
    }
}
```

注: `CTypeSource::Inferred` バリアントが存在しない場合は `CTypeSource::Cast` を流用するか、
新規追加する。

## 変更ファイル一覧

| ファイル | 変更内容 |
|----------|----------|
| `src/fields_dict.rs` | `is_sv_family_type()` メソッド追加 |
| `src/semantic.rs` | Cast ハンドラに SV ファミリー検出追加、`make_sv_ptr_type()` 追加 |
| `src/type_repr.rs` | `CTypeSource::Inferred` バリアント追加（必要な場合） |

## 検証

1. `cargo build` / `cargo test`

2. 出力確認:
   ```bash
   cargo run -- --auto --gen-rust samples/xs-wrapper.h --bindings samples/bindings.rs 2>/dev/null \
     | grep -B 1 -A 5 'fn CvDEPTH\b'
   ```
   - `sv: *mut SV` と推論されること
   - 関数がコメントアウトではなく生成されること

3. 他の SV ファミリーキャストマクロも確認:
   ```bash
   cargo run -- --auto --gen-rust samples/xs-wrapper.h --bindings samples/bindings.rs 2>/dev/null \
     | grep -c 'unknown'
   ```
   - `/* unknown */` の数が減少すること

4. 回帰テスト: `cargo test rust_codegen_regression`

## エッジケース

1. **const キャスト**: `(const CV*)sv` → `derived` に `Pointer { is_const: true }` があるが、
   パラメータは `*mut SV` として推論（SV へのポインタは通常 mutable で受け取る）。

2. **二重ポインタ**: `(CV**)sv` → `derived` にポインタが2つ → ポインタが1つのケースのみ対象とする。
   二重ポインタキャストは `SV*` パラメータとは異なるセマンティクス。

3. **既に型が判明しているパラメータ**: apidoc や bindings.rs で型が既にある場合、
   `get_param_type()` の優先順位により先に解決されるため、Cast 制約は無視される。
   問題なし。

4. **複数のキャスト**: 同じパラメータが `(CV*)sv` と `(AV*)sv` の両方でキャストされる場合、
   両方とも `*mut SV` 制約なので矛盾しない。

5. **SV* キャスト**: `(SV*)sv` → SV 自体も SV ファミリーなので正しく `*mut SV` を推論。

6. **非 SV ファミリーへのキャスト**: `(int*)sv` → `int` は SV ファミリーでないため無視。
