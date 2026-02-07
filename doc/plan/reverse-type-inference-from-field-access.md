# 計画: フィールドアクセスからのベース型逆推論

## 実装ステータス

| Phase | 内容 | 状況 |
|-------|------|------|
| Phase 1 | `lookup_unique` による一意フィールドからの推論 | ✅ 完了（効果限定的） |
| Phase 2 | `get_consistent_base_type` による SV ファミリー対応 | ✅ 完了 |
| Phase 3 | `Member` (直接アクセス) 対応 | ⏸️ 保留（効果限定的） |

**Phase 2 の成果**: `SvFLAGS`, `SvTYPE`, `SvREFCNT` 等の SV ファミリーマクロで
パラメータ型 `*mut SV` が正しく推論されるようになった。

## 背景

### 問題

apidoc がないマクロでは、パラメータの型が推論できない。

```c
#define SvFLAGS(sv)  (sv)->sv_flags
```

現状:
- `sv_flags` の型は `U32` と推論できる（`fields_dict` から取得）
- しかし `sv` の型は `/* unknown */` のまま

### 原因

`sv->sv_flags` を処理する際:
1. `sv_flags` フィールドの型を `fields_dict` から取得 → OK
2. `sv` の型を推論する処理がない → NG

### 期待する動作

`sv->sv_flags` というアクセスパターンから:
- `sv_flags` は `sv` 構造体のフィールド
- したがって `sv` は `*mut SV` 型

## 設計

### アプローチ

`collect_expr_constraints` で `PtrMember` (ptr->field) を処理する際に、
`fields_dict` を使ってベース型を逆推論し、パラメータに型制約を追加する。

### 処理フロー

```
sv->sv_flags を処理
    │
    ├─ 現在の処理: sv_flags の型を取得 (U32)
    │
    └─ 追加する処理:
        │
        ├─ fields_dict.lookup_unique(sv_flags) または
        │  fields_dict.get_consistent_base_type(sv_flags) を呼び出し
        │
        ├─ sv_flags を持つ構造体が特定できた場合 (例: sv)
        │
        └─ base (sv) に対して型制約を追加
           TypeConstraint { ty: *mut SV, context: "field sv_flags implies SV*" }
```

### 実装箇所

**ファイル**: `src/semantic.rs`

**対象メソッド**: `collect_expr_constraints` の `ExprKind::PtrMember` 分岐

```rust
ExprKind::PtrMember { base, member } => {
    self.collect_expr_constraints(base, type_env);

    // === 追加: ベース型の逆推論 ===
    if let Some(fields_dict) = self.fields_dict {
        // フィールドから構造体を特定
        if let Some(struct_name) = fields_dict.lookup_unique(*member) {
            // base に対する型制約を追加
            let base_type = TypeRepr::CType {
                specs: CTypeSpecs::TypedefName(struct_name),
                derived: vec![CDerivedType::Pointer { is_const: false }],
                source: CTypeSource::FieldInference,
            };
            type_env.add_constraint(TypeEnvConstraint::new(
                base.id,
                base_type,
                format!("field {} implies {}*", member_name, struct_name_str),
            ));
        }
    }
    // === 追加ここまで ===

    // 既存の処理...
}
```

### lookup_unique vs get_consistent_base_type

| 方法 | 説明 | 精度 | 適用ケース |
|------|------|------|-----------|
| `lookup_unique` | フィールドが1つの構造体にのみ存在 | 高 | `xav_array` (AV のみ) |
| 新規: `get_consistent_base_type` | 複数構造体で共通の親型 | 中 | `sv_flags` (SV ファミリー共通) |

### 新規メソッド: get_consistent_base_type

`sv_flags` のように複数の SV ファミリー構造体に存在するフィールドの場合、
共通の親型 (`SV`) を返す。

```rust
impl FieldsDict {
    /// フィールドを持つ構造体の共通親型を取得
    ///
    /// 複数の構造体がフィールドを持つ場合、それらの共通の親型を返す。
    /// 例: sv_flags は sv, av, hv, cv 等に存在 → 共通親型は SV
    pub fn get_consistent_base_type(&self, field_name: InternedStr) -> Option<InternedStr> {
        // SV ファミリーの場合は SV を返す
        // その他の場合は lookup_unique にフォールバック
    }
}
```

### CTypeSource の拡張

```rust
pub enum CTypeSource {
    Header,
    Apidoc { raw: String },
    InlineFn { func_name: InternedStr },
    Parser,
    FieldInference,  // 追加: フィールドアクセスからの推論
}
```

## 実装ステップ

### Phase 1: 基本実装

1. `CTypeSource::FieldInference` を追加
2. `collect_expr_constraints` の `PtrMember` 分岐で `lookup_unique` を使用
3. 一意に特定できるフィールドでベース型を推論

### Phase 2: SV ファミリー対応

1. `get_consistent_base_type` を実装
2. `sv_flags`, `sv_any`, `sv_u` など SV 共通フィールドで SV* を推論

### Phase 3: Member (直接アクセス) 対応

`ptr->field` だけでなく `struct.field` パターンにも対応。

#### Phase 3 の調査結果と結論

調査の結果、**Phase 3 を実装しても効果が限定的**であることが判明した。

**理由**:

1. **Perl API のほとんどはポインタパターン（`->`）を使用**
   ```c
   #define SvFLAGS(sv)  (sv)->sv_flags     // -> を使用
   #define HEK_KEY(hek) (hek)->hek_key     // -> を使用
   ```

2. **`.` パターンは中間アクセスに使用される**
   ```c
   #define AvARRAY(av)  ((av)->sv_u.svu_array)
   //                         ↑ -> の後の . は sv_u 特殊処理で対応済み
   ```

3. **値渡しパターン（`param.field`）は非常に少ない**
   ```c
   #define CX_CURPAD_SAVE(block)  (block).oldcomppad = PL_comppad
   ```
   これらには apidoc があり、型は既知。

4. **一部フィールドは収集されていない（別問題）**
   `xpadn_low`, `xpadn_high` などは `_PADNAME_BASE` マクロ内で定義されており、
   fields_dict に収集されていない。

**結論**: Phase 3 の実装優先度は低い。
他に優先すべき改善点:
- マクロ内で定義されたフィールドの収集
- キャストパターン (`((TYPE*)SvANY(x))->field`) からのパラメータ型推論

## 対象マクロと期待結果

| マクロ | フィールド | 推論されるベース型 |
|--------|-----------|-------------------|
| `SvFLAGS(sv)` | `sv_flags` | `*mut SV` |
| `CvROOT(sv)` | `sv_any` → `xcv_root_u` | `*mut CV` (キャストから) |
| `IoIFP(sv)` | `sv_u.svu_fp` | `*mut SV` (または `*mut IO`) |
| `HvKEYS(hv)` | (マクロ呼び出し) | 別アプローチが必要 |

### HvKEYS の特殊ケース

```c
#define HvKEYS(hv)  HvUSEDKEYS(hv)
```

これは別のマクロを呼び出すだけなので、フィールドアクセスがない。
→ マクロ呼び出しからの型伝播が必要（別課題）

## リスク

### 誤推論の可能性

同名フィールドが異なる型の構造体に存在する場合、誤った型を推論する可能性。

**対策**: `lookup_unique` で一意に特定できる場合のみ推論。
曖昧な場合は推論しない（現状維持）。

### パフォーマンス

フィールドアクセスごとに `fields_dict` をルックアップ。

**対策**: 既にキャッシュ機構 (`consistent_type_cache`) があるため、
同様のアプローチで最適化可能。

## 検証

### テストケース

```rust
#[test]
fn test_field_access_base_type_inference() {
    // SvFLAGS(sv) で sv: *mut SV が推論されることを確認
}
```

### 回帰テスト追加

- `SvFLAGS` を回帰テストに追加
- 期待: `sv: *mut SV` → `U32`

## 関連ファイル

| ファイル | 変更内容 |
|----------|----------|
| `src/type_repr.rs` | `CTypeSource::FieldInference` 追加 |
| `src/semantic.rs` | `PtrMember` でベース型逆推論 |
| `src/fields_dict.rs` | `get_consistent_base_type` 追加（Phase 2） |
