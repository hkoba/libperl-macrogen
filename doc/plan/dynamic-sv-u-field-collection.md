# sv_u フィールド型の動的収集

## 背景

sv_u union に関連する型推論は2種類ある：

### 1. フィールドアクセス結果の型推論（semantic.rs）

`sv->sv_u.svu_pv` の `.svu_pv` 部分の結果の C 型を推論。

現在は `main.rs` でハードコード登録：
```rust
let sv_u_fields = [
    ("svu_pv", "char*"),
    ("svu_hash", "HE**"),
    // ...
];
```

**→ 動的収集に変更**

### 2. SV ファミリー引数の型推論（macro_infer.rs）

`sv->sv_u.svu_hash` パターンから、`sv` が `HV*` であることを推論。

現在は `sv_u_field_to_type()` でハードコード：
```rust
fn sv_u_field_to_type(field: &str) -> Option<&'static str> {
    match field {
        "svu_pv" => Some("SV"),
        "svu_hash" => Some("HV"),
        // ...
    }
}
```

**→ ハードコード維持**（関数名を `sv_u_field_to_parameter_type` に改名）

理由: SV ファミリー引数の型推論は、フィールド名から「そのフィールドにアクセスするのに
適切な SV ファミリー型」を判断する意味的な対応関係であり、C 型とは異なる概念。
安全のため明示的に定義しておく方が良い。

## sv_u union の定義場所

sv.h で `_SV_HEAD_UNION` マクロとして定義：

```c
#define _SV_HEAD_UNION \
    union {                             \
        char*   svu_pv;                 \
        IV      svu_iv;                 \
        UV      svu_uv;                 \
        _NV_BODYLESS_UNION              \
        SV*     svu_rv;                 \
        SV**    svu_array;              \
        HE**    svu_hash;               \
        GP*     svu_gp;                 \
        PerlIO *svu_fp;                 \
    }   sv_u
```

このマクロは各 SV ファミリー構造体で展開される：

```c
struct sv { _SV_HEAD(void*); _SV_HEAD_UNION; };
struct av { _SV_HEAD(XPVAV*); _SV_HEAD_UNION; };
struct hv { _SV_HEAD(XPVHV*); _SV_HEAD_UNION; };
// ...
```

## 収集タイミング

`_SV_HEAD_UNION` マクロが展開されると、パーサーは以下の構造を認識する：
- 構造体メンバーとして `sv_u` という名前の union
- その union 内に svu_pv, svu_iv, ... などのフィールド

したがって、**SV ファミリー構造体のパース時**に sv_u union のフィールド情報を収集できる。

## 設計

### 動的収集の対象

フィールドアクセス結果の C 型のみを動的収集する：

| フィールド名 | C 型（動的収集） |
|------------|-----------------|
| svu_pv | char* |
| svu_iv | IV |
| svu_uv | UV |
| svu_rv | SV* |
| svu_array | SV** |
| svu_hash | HE** |
| svu_gp | GP* |
| svu_fp | PerlIO* |

### ハードコード維持の対象

SV ファミリー引数の型推論は `macro_infer.rs` でハードコード維持：

| フィールド名 | 引数の SV ファミリー型 |
|------------|----------------------|
| svu_pv, svu_iv, svu_uv, svu_rv, svu_rx | SV |
| svu_array | AV |
| svu_hash | HV |
| svu_gp | GV |
| svu_fp | IO |

### データ構造

`FieldsDict` の既存構造を活用（変更なし）：

```rust
// 既存のまま
sv_u_field_types: HashMap<InternedStr, String>,
```

### 収集ロジック

`fields_dict.rs` の `collect_from_struct_spec()` を拡張：

```rust
fn collect_from_struct_spec(&mut self, spec: &StructSpec, interner: &StringInterner) {
    // ... 既存の処理 ...

    // sv_u union メンバーを検出
    if let Some(members) = &spec.members {
        for member in members {
            if is_sv_u_union_member(member, interner) {
                // union の内部フィールドを収集
                self.collect_sv_u_union_fields(member, interner);
            }
        }
    }
}

fn collect_sv_u_union_fields(&mut self, member: &Declaration, interner: &StringInterner) {
    // member が union 型の場合、その内部フィールドを走査
    for type_spec in &member.specs.type_specs {
        if let TypeSpec::Union(union_spec) = type_spec {
            if let Some(union_members) = &union_spec.members {
                for union_member in union_members {
                    // フィールド名と C 型を抽出
                    if let Some((field_name, c_type)) = extract_field_info(union_member, interner) {
                        self.register_sv_u_field(field_name, c_type);
                    }
                }
            }
        }
    }
}
```

## 実装計画

### Phase 1: macro_infer.rs の関数名変更

`sv_u_field_to_type()` を `sv_u_field_to_parameter_type()` に改名：

```rust
/// sv_u ユニオンフィールドから SV ファミリー引数型へのマッピング
///
/// このマッピングは意味的な対応関係であり、C 型とは独立。
/// ハードコードで維持する。
fn sv_u_field_to_parameter_type(field: &str) -> Option<&'static str> {
    match field {
        "svu_pv" => Some("SV"),
        "svu_iv" => Some("SV"),
        // ... 既存のまま
    }
}
```

### Phase 2: fields_dict.rs に動的収集を追加

1. `collect_from_struct_spec()` を拡張して sv_u union を検出
2. `collect_sv_u_union_fields()` を追加
3. union 内部のフィールド名と C 型を `sv_u_field_types` に登録

### Phase 3: main.rs のハードコード削除

1. sv_u フィールド型のハードコード登録部分を削除
2. 動的に収集された情報を使用（既存の `get_sv_u_field_type()` 経由）

### Phase 4: テストと動作確認

1. `cargo test` で既存テストが通ることを確認
2. 実際の出力で型が正しく解決されることを確認
3. 動的収集されたフィールド数の統計を出力

## 期待される効果

1. **保守性向上**: sv_u の C 型定義が変更されても自動追従
2. **コードの一貫性**: C 型情報が FieldsDict に集約
3. **安全性維持**: SV ファミリー引数推論はハードコードで意図を明示

## 追加考慮事項

### 条件付きフィールド

`_NV_BODYLESS_UNION` は条件付きで `svu_nv` フィールドを追加する：

```c
#if NVSIZE <= IVSIZE
#  define _NV_BODYLESS_UNION NV svu_nv;
#else
#  define _NV_BODYLESS_UNION
#endif
```

動的収集では、実際に展開されたフィールドのみを収集するため、
この条件分岐は自動的に処理される。

### フォールバック

動的収集が失敗した場合（例: sv.h がパースされない環境）に備え、
警告を出力する。ただし、sv_u union が存在しない環境では
そもそも SV ファミリー関連のマクロも存在しないため、
フォールバック値は不要と考える。

### 2つの推論の関係

| 推論 | 目的 | 情報源 | 管理方針 |
|------|------|--------|----------|
| C 型 | フィールドアクセス結果の型 | AST から動的収集 | fields_dict |
| SV ファミリー | 引数の型推論 | 意味的対応関係 | ハードコード |

この分離により、C 型の変更は自動追従しつつ、
SV ファミリー推論の意図は明示的に保持される。
