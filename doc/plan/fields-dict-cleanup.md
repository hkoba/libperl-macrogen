# FieldsDict クリーンアップ計画

## 背景

`fields_dict.rs` には以前のアプローチで使用されていた関数や、
将来の拡張用に残されたコードが含まれている。
`_SV_HEAD` マクロからの SV ファミリー検出機能が実装されたことで、
一部のコードは不要になった。

## 削除対象

### 1. 未使用の手動登録関数

```rust
// 使われていない
pub fn add_field(&mut self, field_name: InternedStr, struct_name: InternedStr)
pub fn add_sv_family_member(&mut self, struct_name: InternedStr)
pub fn set_field_type_override(&mut self, ...)
pub fn register_typedef(&mut self, typedef_name: InternedStr, struct_name: InternedStr)
```

### 2. 未使用の SV ファミリー判定関数

```rust
// SV ファミリー判定用だが、現在は使われていない
const SV_HEAD_FIELDS: &'static [&'static str]
pub fn is_polymorphic_field(&self, field_name: InternedStr) -> bool
pub fn get_structs_with_field(&self, field_name: InternedStr) -> Option<&HashSet<InternedStr>>
pub fn is_sv_family(&self, structs: &HashSet<InternedStr>, _interner: &StringInterner) -> bool
pub fn get_sv_family_base_type(&self, interner: &StringInterner) -> Option<InternedStr>
pub fn is_sv_family_field(&self, field_name: InternedStr, interner: &StringInterner) -> bool
```

### 3. 未使用の検索関数

```rust
// InternedStr ベースの汎用検索（より特化した関数が使われている）
pub fn lookup(&self, field_name: InternedStr) -> Option<&HashSet<InternedStr>>
pub fn get_field_type(&self, struct_name: InternedStr, field_name: InternedStr) -> Option<&FieldType>
```

### 4. main.rs の `set_unique_field_type` 呼び出し

```rust
// main.rs:397-414
// sv_any, sv_refcnt, sv_flags を一意にsvとして登録
{
    let interner = pp.interner_mut();
    let sv = interner.intern("sv");
    let sv_any = interner.intern("sv_any");
    let sv_refcnt = interner.intern("sv_refcnt");
    let sv_flags = interner.intern("sv_flags");

    fields_dict.set_unique_field_type(sv_any, sv);
    fields_dict.set_unique_field_type(sv_refcnt, sv);
    fields_dict.set_unique_field_type(sv_flags, sv);
}
```

**理由**: `_SV_HEAD` マクロからの SV ファミリー検出により、
これらのフィールドは自動的に正しい構造体に関連付けられる。
手動で上書きする必要はなくなった。

### 5. `set_unique_field_type` 関数自体

上記の呼び出しが削除されれば、この関数も不要。

```rust
pub fn set_unique_field_type(&mut self, field_name: InternedStr, struct_name: InternedStr)
```

## 削除後に残る関数

### データ収集（パース時）
- `new()` - コンストラクタ
- `collect_from_external_decl()` - 外部宣言からフィールド収集
- `collect_from_declaration()` - 内部
- `collect_typedef_aliases()` - typedef マッピング収集
- `collect_from_struct_spec()` - 構造体からフィールド収集
- `extract_field_type()` - 内部
- `extract_base_type()` - 内部
- `apply_derived_decls()` - 内部

### SvANY パターン型推論
- `add_sv_family_member_with_type()` - _SV_HEAD から呼ばれる
- `get_struct_for_sv_head_type()` - macro_infer で使用
- `sv_family_members_count()` - 統計
- `sv_head_type_mapping_count()` - 統計
- `sv_head_type_to_struct_iter()` - デバッグ

### SemanticAnalyzer での型推論
- `lookup_unique()` - フィールドから構造体特定
- `get_unique_field_type()` - フィールド型取得
- `get_field_type_by_name()` - 構造体名指定でフィールド型取得
- `resolve_typedef()` - typedef 解決
- `resolve_typedef_by_name()` - 内部

### 一致型キャッシュ
- `build_consistent_type_cache()` - 事前計算
- `compute_consistent_type()` - 内部
- `get_consistent_field_type()` - キャッシュ参照

### 統計・デバッグ
- `typedef_count()` - 統計
- `field_types_count()` - 統計
- `dump()` - デバッグ
- `dump_unique()` - `--dump-fields-dict` 用
- `dump_field_types()` - デバッグ
- `stats()` - 統計

## 削除後に不要になるフィールド

```rust
// sv_family_members は add_sv_family_member_with_type で使用されるので残す
// sv_head_type_to_struct も残す
```

すべてのフィールドは引き続き使用される。

## 実装手順

### Phase 1: main.rs の修正
1. `set_unique_field_type` の呼び出しブロックを削除

### Phase 2: fields_dict.rs の修正
1. 未使用関数を削除:
   - `add_field`
   - `add_sv_family_member`
   - `set_unique_field_type`
   - `set_field_type_override`
   - `register_typedef`
   - `lookup`
   - `get_field_type`
   - `is_polymorphic_field`
   - `get_structs_with_field`
   - `is_sv_family`
   - `get_sv_family_base_type`
   - `is_sv_family_field`
   - `SV_HEAD_FIELDS`

### Phase 3: テスト
1. `cargo build` で警告なくビルドできることを確認
2. `cargo test` で全テストが通ることを確認
3. `cargo run -- --auto samples/wrapper.h` でマクロ型推論が動作することを確認

## 期待される効果

- コードの簡素化（約100行削減）
- 未使用コードによる混乱の解消
- 実際に使用されている機能が明確になる
