# FieldsDict アーキテクチャ

このドキュメントでは、libperl-macrogen における `FieldsDict`（構造体フィールド辞書）の
構造、構築タイミング、および型推論での活用方法を解説する。

## 概要

`FieldsDict` は、C ヘッダーから抽出した構造体フィールド情報を管理する辞書である。
マクロの型推論において、`ptr->field` のようなメンバーアクセスから
ベース型（ptr の型）を逆引きするために使用される。

```
┌─────────────────────────────────────────────────────────────────────┐
│                        C ヘッダー                                    │
│  struct xpv { char* xpv_pv; STRLEN xpv_cur; ... };                  │
│  typedef struct xpv XPV;                                             │
└─────────────────────────────────────────────────────────────────────┘
                              │
                              ▼ パース時に収集
┌─────────────────────────────────────────────────────────────────────┐
│                        FieldsDict                                    │
├─────────────────────────────────────────────────────────────────────┤
│  field_to_structs:    xpv_pv → {xpv}, xpv_cur → {xpv, xpvav, ...}  │
│  field_types:         (xpv, xpv_pv) → char*                          │
│  typedef_to_struct:   XPV → xpv                                      │
│  consistent_type_cache: xpv_pv → char* (全構造体で一致する場合)       │
│  sv_family_members:   {sv, av, hv, cv, ...}                          │
│  sv_head_type_to_struct: XPVAV → av, XPVCV → cv                     │
│  sv_u_field_types:    svu_pv → "char*", svu_hash → "HE**"           │
└─────────────────────────────────────────────────────────────────────┘
                              │
                              ▼ 型推論時に参照
┌─────────────────────────────────────────────────────────────────────┐
│                     SemanticAnalyzer                                 │
│  ・ptr->field から ptr の型を推論                                    │
│  ・フィールドの型を取得                                              │
└─────────────────────────────────────────────────────────────────────┘
```

## FieldsDict の構造

### 主要フィールド

```rust
pub struct FieldsDict {
    /// フィールド名 → 構造体名のセット
    /// 同じフィールド名が複数の構造体で使われる可能性があるため HashSet
    field_to_structs: HashMap<InternedStr, HashSet<InternedStr>>,

    /// (構造体名, フィールド名) → フィールド型
    field_types: HashMap<(InternedStr, InternedStr), FieldType>,

    /// typedef 名 → 構造体名
    /// 例: XPV → xpv, XPVAV → xpvav
    typedef_to_struct: HashMap<InternedStr, InternedStr>,

    /// 一致型キャッシュ: フィールド名 → 型（全構造体で型が同じ場合のみ Some）
    consistent_type_cache: HashMap<InternedStr, Option<TypeRepr>>,

    /// SV ファミリーメンバー（_SV_HEAD マクロを使用する構造体）
    sv_family_members: HashSet<InternedStr>,

    /// _SV_HEAD(typeName) の typeName → 構造体名
    /// SvANY キャストパターンの型推論に使用
    sv_head_type_to_struct: HashMap<String, InternedStr>,

    /// sv_u ユニオンフィールドの型
    /// 例: svu_pv → "char*", svu_hash → "HE**"
    sv_u_field_types: HashMap<InternedStr, String>,
}
```

### FieldType

```rust
pub struct FieldType {
    /// 型情報（構造化された表現）
    pub type_repr: TypeRepr,
}
```

## 構築タイミング

`FieldsDict` は `infer_api.rs` の `run_pipeline` 関数内で構築される。

### 構築フロー

```
run_pipeline()
    │
    ├─ FieldsDict::new()                    // 1. 空の辞書を作成
    │
    ├─ parser.parse_each_with_pp(|decl| {   // 2. パース時に収集
    │      fields_dict.collect_from_external_decl(decl, ...)
    │
    │      // _SV_HEAD マクロが呼ばれたかチェック
    │      if _SV_HEAD was called {
    │          fields_dict.add_sv_family_member_with_type(name, type)
    │      }
    │  })
    │
    └─ fields_dict.build_consistent_type_cache()  // 3. キャッシュ構築
```

### 詳細な収集プロセス

#### 1. 宣言からの収集 (`collect_from_external_decl`)

```rust
// ターゲットディレクトリ内の宣言のみ収集
if !is_target { return; }

// Declaration を処理
if let ExternalDecl::Declaration(d) = decl {
    self.collect_from_declaration(d, interner);
}
```

#### 2. 構造体メンバーの収集 (`collect_from_struct_spec`)

```rust
// 名前付き構造体のみ対象
let struct_name = spec.name?;

for member in members {
    // フィールド名 → 構造体名マッピング
    self.field_to_structs
        .entry(field_name)
        .or_default()
        .insert(struct_name);

    // フィールド型の収集
    self.field_types.insert(
        (struct_name, field_name),
        FieldType { type_repr },
    );
}
```

#### 3. typedef エイリアスの収集 (`collect_typedef_aliases`)

```c
// typedef struct xpv XPV; の場合
// XPV → xpv のマッピングを登録
```

#### 4. SV ファミリーの検出

`_SV_HEAD(typeName)` マクロ呼び出しを監視し、動的に SV ファミリーを構築:

```rust
// infer_api.rs での監視設定
let sv_head_id = pp.interner_mut().intern("_SV_HEAD");
pp.set_macro_called_callback(sv_head_id, Box::new(MacroCallWatcher::new()));

// パース時のコールバック
if watcher.take_called() {
    let type_name = watcher.last_args()...;
    fields_dict.add_sv_family_member_with_type(name, &type_name);
}
```

#### 5. 一致型キャッシュの構築

パース完了後、全フィールドについて型の一貫性を事前計算:

```rust
fields_dict.build_consistent_type_cache(interner);

// 内部処理: 各フィールドについて全構造体で型が一致するかチェック
// 一致する場合のみ Some(TypeRepr) をキャッシュ
```

## SemanticAnalyzer での活用

### 1. 型制約の解決 (`solve_type_constraints`)

`HasField` 制約からベース型を推論:

```rust
TypeConstraint::HasField { var, field } => {
    if let Some(fields_dict) = self.fields_dict {
        // フィールドを一意に持つ構造体を検索
        if let Some(struct_name) = fields_dict.lookup_unique(*field) {
            solutions.insert(*var, Type::Pointer(
                Box::new(Type::TypedefName(struct_name)),
                TypeQualifiers::default(),
            ));
        }
    }
}
```

### 2. メンバーアクセスの型推論 (`collect_expr_constraints`)

`ptr->field` 式の型を推論:

```rust
ExprKind::PtrMember { base, member } => {
    let (field_type, used_consistent_type) =
        if let Some(struct_name) = self.extract_struct_name_from_pointer_type(&base_ty) {
            // ベース型が既知: 構造体名でルックアップ
            (self.lookup_field_type_repr(struct_name, *member), false)
        } else if let Some(fields_dict) = self.fields_dict {
            // ベース型が不明: 一致型キャッシュを使用（O(1)）
            (fields_dict.get_consistent_field_type(*member).cloned(), true)
        } else {
            (None, false)
        };
}
```

### 3. フィールド型の取得 (`lookup_field_type_repr`)

構造体名とフィールド名から型を取得:

```rust
fn lookup_field_type_repr(&self, struct_name: &str, field_name: InternedStr) -> Option<TypeRepr> {
    let fields_dict = self.fields_dict?;
    let field_type = fields_dict.get_field_type_by_name(
        struct_name, field_name, self.interner
    )?;
    Some(field_type.type_repr.clone())
}
```

### 4. sv_u ユニオンフィールドの型取得

SV 構造体の `sv_u` ユニオンフィールドの型を取得:

```rust
fn lookup_sv_u_field_type(&self, field: InternedStr) -> Option<String> {
    self.fields_dict?
        .get_sv_u_field_type(field)
        .map(|s| s.to_string())
}
```

## ルックアップ方法

### lookup_unique

フィールド名から一意に構造体を特定:

```rust
pub fn lookup_unique(&self, field_name: InternedStr) -> Option<InternedStr> {
    self.field_to_structs.get(&field_name).and_then(|structs| {
        if structs.len() == 1 {
            structs.iter().next().copied()
        } else {
            None  // 複数の構造体で使われている → 一意に特定不可
        }
    })
}
```

### get_consistent_field_type

フィールドが全構造体で同じ型を持つ場合、その型を返す（O(1)）:

```rust
pub fn get_consistent_field_type(&self, field_name: InternedStr) -> Option<&TypeRepr> {
    self.consistent_type_cache
        .get(&field_name)
        .and_then(|opt| opt.as_ref())
}
```

### get_field_type_by_name

構造体名（文字列）とフィールド名から型を取得。typedef 名でも検索可能:

```rust
pub fn get_field_type_by_name(
    &self,
    struct_name_str: &str,  // "XPV" でも "xpv" でも可
    field_name: InternedStr,
    interner: &StringInterner,
) -> Option<&FieldType>
```

## 統計情報

```rust
pub struct FieldsDictStats {
    pub total_fields: usize,      // 全フィールド数
    pub unique_fields: usize,     // 一意なフィールド数
    pub ambiguous_fields: usize,  // 曖昧なフィールド数
}
```

CLI での確認:
```bash
cargo run -- --auto --dump-fields-dict samples/xs-wrapper.h
```

## 関連ファイル

| ファイル | 役割 |
|----------|------|
| `src/fields_dict.rs` | FieldsDict 本体 |
| `src/infer_api.rs` | FieldsDict の構築（run_pipeline） |
| `src/semantic.rs` | FieldsDict の活用（型推論） |
| `src/type_repr.rs` | FieldType で使用される TypeRepr |

## 今後の拡張ポイント

1. **ベース型推論の強化**: 複数候補がある場合のヒューリスティクス
2. **継承関係の考慮**: SV ファミリーの継承構造を活用した型推論
3. **フィールドアクセスパターンの学習**: 使用パターンから型を推論
