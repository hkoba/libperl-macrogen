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

    // ── 共通フィールドマクロ系（perl5 `_XPV_HEAD` / `_XPVCV_COMMON` 等） ──

    /// 構造体名 → そこで展開された共通フィールドマクロ集合
    /// 例: xpvcv → [_XPV_HEAD, _XPVCV_COMMON]
    struct_to_common_macros: HashMap<InternedStr, Vec<InternedStr>>,

    /// 共通フィールドマクロ → それを使う構造体集合
    /// 例: _XPVCV_COMMON → [xpvcv, xpvfm]
    common_macro_to_structs: HashMap<InternedStr, Vec<InternedStr>>,

    /// 共通フィールドマクロ名 → そこで宣言されるフィールド情報
    /// 例: _XPVCV_COMMON → [{ name: xcv_xsub, is_fn_pointer: true, ... }, ...]
    common_macros: HashMap<InternedStr, CommonFieldMacro>,

    /// フィールド名 → そのフィールドを定義している共通マクロ名
    /// 例: xcv_xsub → _XPVCV_COMMON
    /// （無名 union 内のフィールドも含む）
    field_to_defining_macro: HashMap<InternedStr, InternedStr>,

    /// 共通フィールドマクロ宣言フィールド名 → 整合性のある Rust 型
    /// （bindings.rs 由来）。`build_common_field_rust_types` で構築。
    common_field_rust_types: HashMap<InternedStr, TypeRepr>,

    /// 共通フィールドマクロ → 一意な SV ファミリー typedef 名
    /// 例: `_XPVCV_COMMON` → `CV` （xpvcv ボディ → struct cv → typedef CV、
    /// xpvfm 側は対応 SV 構造体無しで除外、結果一意）
    /// `build_common_macro_sv_family` で構築。
    common_macro_to_sv_family: HashMap<InternedStr, InternedStr>,

    /// 構造体最終メンバーが flexible array member（`char foo[1]`、`[0]`、または
    /// C99 `[]` 構文）である場合の (struct_name, field_name) → 要素型
    /// （配列を剥がした TypeRepr）。C 慣用句で可変長バッファとして使われ、
    /// 配列名→ポインタ decay を Rust に翻訳するために必要。
    /// 例: `(struct hek, hek_key)` → `char` の TypeRepr
    flexible_array_fields: HashMap<(InternedStr, InternedStr), TypeRepr>,
}
```

#### Flexible Array Member の背景

C ヘッダーには `struct hek { ... char hek_key[1]; }` のように **構造体末尾に
サイズ 1 (or 0、C99 `[]`) の配列**を持ち、実際は **可変長バッファの先頭**として
使う慣用句が頻出する。`HEK_KEY(hek) = (hek)->hek_key` のような アクセス時、
C では配列名→ポインタ decay により `char *` として扱われる。

bindings.rs では `pub hek_key: [c_char; 1]` のまま生成されるため、Rust 側で
そのまま `(*hek).hek_key` を返すと **配列値**になり、`.offset()` 呼び出しや
`as *mut U` cast が型エラー (E0599 / E0605) になる。

`flexible_array_fields` は parse 時に「最終メンバーかつ size 1/0 配列、または
`[]`」を検出して登録し、`semantic.rs` / `rust_codegen.rs` でアクセスを
**ポインタ decay** に翻訳する基盤を提供する。`flexible_array_element` アクセサ
は typedef 解決付き（例: `HEK` で問い合わせても `hek` のエントリを返す）。

#### 共通フィールドマクロ系フィールドの背景

perl5 ヘッダーには `_XPV_HEAD` / `_XPVCV_COMMON` 等の **共通フィールド宣言マクロ** が
あり、複数の `xpv*` ボディ構造体で **同一フィールド集合** を共有している。
これらのマクロは perl.h で `#undef` されてしまい、通常のパース時には
構造として残らない。FieldsDict は `Preprocessor` に `MacroDefCallback`
（`infer_api.rs::CommonMacroBodyCollector`）を仕込んで `#define` 時点で
本体トークンを捕獲し、後段で `parse_struct_members_from_tokens_ref` で
パースしてフィールド集合を再構築する。

これにより以下が可能になる:

- **フィールド型の整合性解決**: 共通マクロが宣言するフィールドは
  全ボディで同じ型 → bindings.rs から canonical な Rust 型を一意に決定可能
  （`build_common_field_rust_types` で構築する `common_field_rust_types`）
- **SV ファミリー型の逆推論**: `_XPVCV_COMMON` 由来フィールドへのアクセス
  経路は `xpvcv` ボディに限定 → 対応する SV 構造体は `cv` のみ（typedef `CV`）
  と一意に推論できる（`build_common_macro_sv_family` の
  `common_macro_to_sv_family`）。`semantic.rs::try_infer_sv_family_from_member`
  がこれを使って `CvHASGV(cv)` のような未キャストマクロから
  `cv: *const CV` を逆推論する

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
    ├─ FieldsDict::new()                          // 1. 空の辞書を作成
    │
    ├─ pp.set_macro_def_callback(                 // 2a. 共通マクロ #define 観測
    │      CommonMacroBodyCollector::new(...))
    │
    ├─ parser.parse_each_with_pp(|decl| {         // 2b. パース時に収集
    │      fields_dict.collect_from_external_decl(decl, ...)
    │
    │      // _SV_HEAD マクロが呼ばれたかチェック
    │      if _SV_HEAD was called {
    │          fields_dict.add_sv_family_member_with_type(name, type)
    │      }
    │      // 共通マクロが struct 内で呼ばれたかチェック (B-1)
    │      if e.g. _XPVCV_COMMON was called {
    │          fields_dict.add_struct_uses_common_macro(struct, macro)
    │      }
    │  })
    │
    ├─ fields_dict.build_consistent_type_cache()  // 3. 型一致キャッシュ構築
    │
    ├─ fields_dict.build_common_macro_fields(     // 4. 共通マクロ本体パース (B-2)
    │      &macro_bodies, parse_struct_members)
    │
    ├─ fields_dict.build_common_field_rust_types( // 5. bindings.rs 連携
    │      rust_decl_dict, interner)
    │
    └─ fields_dict.build_common_macro_sv_family(  // 6. SV ファミリー逆引き
           interner)
```

`CommonMacroBodyCollector` (`src/infer_api.rs`) は `MacroDefCallback`
trait を実装しており、`Preprocessor` がプリプロセスドライバ中に `#define` を
観測した際に呼び出される。`#undef` で失われる前に共通マクロ本体トークンを
保持しておくのが目的。`take_macro_def_callback()` でドライバ終了後に取り出す。

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

#### Perl API における lookup_unique の限界

調査の結果、`lookup_unique` による**ベース型の逆推論**は Perl API マクロではあまり有効でない
可能性がある。理由:

1. **一意なフィールドはキャストパターンで使用される**

   ```c
   // xav_fill は xpvav に一意だが、キャスト経由でアクセス
   #define AvFILLp(av)  ((XPVAV*)SvANY(av))->xav_fill
   ```

   パラメータ `av` に直接 `->xav_fill` でアクセスしていないため、逆推論が適用されない。

2. **直接アクセスパターンは共通フィールドを使用**

   ```c
   // sv_flags は複数の SV ファミリー構造体に存在
   #define SvFLAGS(sv)  (sv)->sv_flags
   ```

   `sv_flags` は `sv`, `av`, `hv`, `cv` 等に存在するため、`lookup_unique` では特定できない。

3. **一意フィールドへの直接アクセスを持つマクロは apidoc がある**

   そもそも apidoc がある場合は、そちらから型情報を取得できるため `lookup_unique` は不要。

**結論**: `lookup_unique` はフィールド型の取得には有効だが、Perl API においては
パラメータ型の逆推論にはほとんど寄与しない。SV ファミリーの共通フィールドからベース型を
推論するには、`get_consistent_base_type` が必要。

### get_consistent_base_type

フィールドを持つ構造体群の共通親型を取得:

```rust
pub fn get_consistent_base_type(&self, field_name: InternedStr, interner: &StringInterner) -> Option<InternedStr> {
    let structs = self.field_to_structs.get(&field_name)?;
    if structs.is_empty() {
        return None;
    }
    // 全ての構造体が SV ファミリーメンバーかチェック
    let all_sv_family = structs.iter().all(|s| self.sv_family_members.contains(s));
    if all_sv_family {
        interner.lookup("sv")
    } else {
        None
    }
}
```

このメソッドは `lookup_unique` が失敗した場合のフォールバックとして使用される。
`sv_flags` のように複数の SV ファミリー構造体に存在するフィールドの場合、
共通の親型 `SV` を返す。

**使用例**:
- `SvFLAGS(sv)` → `sv->sv_flags` で `sv_flags` を検出 → `*mut SV` を推論

### get_typedef_for_struct

構造体名から typedef 名への逆引き:

```rust
pub fn get_typedef_for_struct(&self, struct_name: InternedStr) -> Option<InternedStr> {
    for (typedef_name, s_name) in &self.typedef_to_struct {
        if *s_name == struct_name {
            return Some(*typedef_name);
        }
    }
    None
}
```

フィールド推論で得られた構造体名（例: `sv`）を typedef 名（例: `SV`）に変換する。
これにより、生成される Rust コードが既存の型定義と一貫性を持つ。

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

### 共通フィールドマクロ系アクセサ

```rust
/// フィールド名 → そのフィールドを宣言した共通マクロ名
/// 例: xcv_xsub → _XPVCV_COMMON
pub fn defining_macro_of(&self, field_name: InternedStr) -> Option<InternedStr>;

/// 共通マクロ名 → そのマクロが宣言する CommonField 集合
pub fn common_macro_info(&self, macro_name: InternedStr) -> Option<&CommonFieldMacro>;

/// フィールド名 → そのフィールド本体（CommonField）
pub fn common_field_for_field(&self, field_name: InternedStr) -> Option<&CommonField>;

/// 構造体が直接展開している共通マクロ集合
pub fn common_macros_used_by_struct(&self, struct_name: InternedStr) -> &[InternedStr];

/// 共通マクロ宣言フィールドの canonical Rust 型（bindings.rs 由来、`build_common_field_rust_types` で構築）
/// 例: xcv_xsub → `Option<unsafe extern "C" fn(...) -> ()>`
pub fn common_field_rust_type(&self, field_name: InternedStr) -> Option<&TypeRepr>;

/// 共通マクロ → 一意な SV ファミリー typedef（`build_common_macro_sv_family` で構築）
/// 例: _XPVCV_COMMON → "CV"
pub fn sv_family_of_common_macro(&self, macro_name: InternedStr) -> Option<InternedStr>;
```

`sv_family_of_common_macro` は `semantic.rs::try_infer_sv_family_from_member`
から呼ばれ、`CvHASGV(cv)` のような未キャストマクロから `cv: *const CV` を
逆推論するための核となる。詳細は `architecture-semantic-type-inference.md`
の「共通フィールドマクロからの SV ファミリー逆推論」節を参照。

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
