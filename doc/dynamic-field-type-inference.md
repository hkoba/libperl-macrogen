# 動的フィールド型推論の設計

## 背景と問題

現在、`lookup_known_field_type`, `get_field_type_by_name`, `get_field_type` でフィールド名と型の対応をハードコードしている。

```rust
// 例: macro_analysis.rs
("_xivu", "xivu_iv") => Some("IV".to_string()),
("XPVAV", "xav_alloc") => Some("*mut *mut SV".to_string()),
```

これらは Perl の内部構造であり、バージョンによって変化する可能性がある。ハードコードでは異なる Perl バージョンに対応できない。

## 目標

1. **ヘッダーから動的に読み取る**: 構造体・共用体の定義をパース時に収集し、フィールドの型情報を自動的に取得
2. **設定による拡張**: 自動導出できない恣意的な対応関係のみ `MacrogenBuilder` の設定で外から渡す

## 設計

### 1. FieldsDict の拡張

現在の `FieldsDict` は `field_name -> HashSet<struct_name>` のマッピングのみ。これを拡張して型情報も保持する。

```rust
pub struct FieldsDict {
    /// フィールド名 -> 構造体名のセット
    field_to_structs: HashMap<InternedStr, HashSet<InternedStr>>,

    /// (構造体名, フィールド名) -> フィールド型（新規追加）
    field_types: HashMap<(InternedStr, InternedStr), FieldType>,

    /// 収集対象のディレクトリパス
    target_dir: Option<String>,
}

/// フィールドの型情報
pub struct FieldType {
    /// C言語での型表現
    pub c_type: String,
    /// Rust言語での型表現（変換済み）
    pub rust_type: String,
}
```

### 2. パース時の型情報収集

`collect_from_struct_spec` を拡張し、フィールドの型情報も収集する。

```rust
fn collect_from_struct_spec(&mut self, spec: &StructSpec, interner: &StringInterner) {
    let struct_name = match spec.name {
        Some(name) => name,
        None => return,
    };

    let members = match &spec.members {
        Some(m) => m,
        None => return,
    };

    for member in members {
        for decl in &member.declarators {
            if let Some(ref declarator) = decl.declarator {
                if let Some(field_name) = declarator.name {
                    // 既存: フィールド名 -> 構造体名
                    self.field_to_structs
                        .entry(field_name)
                        .or_default()
                        .insert(struct_name);

                    // 新規: 型情報の収集
                    let c_type = extract_type_from_decl_specs(&member.specs, declarator);
                    let rust_type = c_type_to_rust(&c_type);
                    self.field_types.insert(
                        (struct_name, field_name),
                        FieldType { c_type, rust_type },
                    );
                }
            }
        }
    }
}
```

### 3. 型情報の検索 API

```rust
impl FieldsDict {
    /// (構造体名, フィールド名) から型を検索
    pub fn get_field_type(&self, struct_name: InternedStr, field_name: InternedStr) -> Option<&FieldType> {
        self.field_types.get(&(struct_name, field_name))
    }

    /// フィールド名から一意に型を特定（構造体が1つしかない場合）
    pub fn get_unique_field_type(&self, field_name: InternedStr) -> Option<&FieldType> {
        let structs = self.field_to_structs.get(&field_name)?;
        if structs.len() != 1 {
            return None;
        }
        let struct_name = structs.iter().next()?;
        self.field_types.get(&(*struct_name, field_name))
    }
}
```

### 4. MacrogenBuilder での設定

自動導出できない対応関係を外部から設定可能にする。

```rust
impl MacrogenBuilder {
    /// フィールド型のオーバーライドを追加
    /// 自動収集できない場合や、特殊なマッピングが必要な場合に使用
    pub fn add_field_type_override(
        &mut self,
        struct_name: &str,
        field_name: &str,
        rust_type: &str,
    ) -> &mut Self {
        self.field_type_overrides.push(FieldTypeOverride {
            struct_name: struct_name.to_string(),
            field_name: field_name.to_string(),
            rust_type: rust_type.to_string(),
        });
        self
    }
}
```

使用例（build.rs など）:

```rust
MacrogenBuilder::new()
    .input("wrapper.h")
    .auto_config()
    // 自動導出できない特殊ケースのみオーバーライド
    .add_field_type_override("XPVCV", "xcv_xsub", "XSUBADDR_t")
    .generate()?;
```

### 5. 型情報収集の課題と対策

#### 課題 A: typedef 解決

```c
typedef struct sv SV;
struct xpvav {
    SV** xav_alloc;  // SV** を *mut *mut SV に変換する必要
};
```

**対策**: 既存の `c_type_to_rust` 関数を活用。typedef は別途追跡し、変換時に解決。

#### 課題 B: union のネスト

```c
union _xivu {
    IV xivu_iv;
    UV xivu_uv;
};
struct xpviv {
    union _xivu xiv_u;  // xiv_u の型は union _xivu
};
```

`xiv_u.xivu_iv` のようなアクセスでは、`xiv_u` と `xivu_iv` 両方の型情報が必要。

**対策**:
- `xiv_u` → `union _xivu` のマッピングを保持
- `(_xivu, xivu_iv)` → `IV` のマッピングを保持
- 式解析時に両方を組み合わせて最終型を決定

#### 課題 C: マクロによる型定義

```c
#define xiv_iv xiv_u.xivu_iv
```

このマクロは展開後に `xiv_u.xivu_iv` となるため、パース時のトークン列にはマクロ展開後の形式で出現する。

**対策**: 現在の実装で既に対応済み（展開後のトークンを解析）

## 実装ステップ

### Phase 1: FieldsDict の拡張

1. `FieldType` 構造体を追加
2. `field_types: HashMap<(InternedStr, InternedStr), FieldType>` を追加
3. `collect_from_struct_spec` でフィールド型を収集
4. `get_field_type`, `get_unique_field_type` を追加

### Phase 2: 型抽出ロジック

1. `DeclSpecs` + `Declarator` から C 型文字列を抽出する関数を実装
2. ポインタ、配列、関数ポインタなどの複雑な型に対応
3. `c_type_to_rust` との連携

### Phase 3: 推論ロジックの更新

1. `macro_analysis.rs` のハードコード部分を削除
2. `FieldsDict` を使った動的型検索に置き換え
3. `iterative_infer.rs` の `lookup_known_field_type` も同様に更新

### Phase 4: MacrogenBuilder 設定

1. `field_type_overrides` フィールドを追加
2. `add_field_type_override` メソッドを実装
3. オーバーライドを `FieldsDict` に適用するロジック

### Phase 5: テストとリファクタリング

1. 各種 Perl 構造体でのテスト
2. ハードコード削除の確認
3. ドキュメント更新

## 変更対象ファイル

- `src/fields_dict.rs` - 型情報の追加
- `src/macro_analysis.rs` - ハードコード削除、FieldsDict 利用
- `src/iterative_infer.rs` - ハードコード削除、FieldsDict 利用
- `src/macrogen.rs` - MacrogenBuilder 設定追加
- `src/lib.rs` - API 公開

## 期待される効果

1. **Perl バージョン非依存**: ヘッダーから動的に型情報を取得するため、Perl 5.36 でも 5.40 でも動作
2. **保守性向上**: ハードコードを削除し、自動収集に依存
3. **拡張性**: 必要に応じて外部から設定可能
