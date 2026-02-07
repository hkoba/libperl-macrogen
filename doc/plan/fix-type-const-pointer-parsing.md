# 計画: TYPE* const パターンのパース修正

## 背景

`CopLABEL` や `HvFILL` マクロの引数の型が `()` になる問題の根本原因は、
`type_repr.rs` の `parse_c_type_string` が `"COP* const"` のようなパターンを
正しくパースできないことにある。

詳細は `doc/report/macro-param-type-inference-issue.md` を参照。

## 問題の本質

`type_repr.rs` に不完全な C 型パーサー (`parse_c_type_string`) が実装されているが、
完全な C パーサーは既に `parser.rs` に存在する。重複実装を避け、`parser.rs` を
再利用すべき。

## 計画

### Phase 1: デバッグ出力機能の標準化

型推論過程を追跡するためのデバッグ出力機能を標準オプションとして追加する。

#### 1.1 CLI オプションの追加

`--debug-type-inference` オプションを追加:

```bash
cargo run -- --auto --gen-rust --debug-type-inference=CopLABEL,HvFILL samples/xs-wrapper.h
```

#### 1.2 出力内容

- マクロパラメータの apidoc 型情報
- シンボル登録時の型
- `lookup_symbol` の結果
- `TypeRepr::from_apidoc_string` の入出力
- `collect_call_constraints` での型制約
- `type_env` の最終状態

#### 1.3 実装箇所

| ファイル | 変更内容 |
|----------|----------|
| `src/main.rs` | CLI オプション追加 |
| `src/macro_infer.rs` | デバッグフラグの伝播 |
| `src/semantic.rs` | 条件付きデバッグ出力 |
| `src/type_repr.rs` | パース結果のデバッグ出力 |

### Phase 2: TypeRepr::from_type_name の実装

`parser.rs` の `parse_type_from_string` を活用するため、`TypeName` から
`TypeRepr` を作成するメソッドを追加する。

#### 2.1 TypeName の構造

```rust
pub struct TypeName {
    pub specs: DeclSpecs,
    pub declarator: Option<AbstractDeclarator>,
}

pub struct AbstractDeclarator {
    pub derived: Vec<DerivedDecl>,
}
```

#### 2.2 新しいメソッド

```rust
impl TypeRepr {
    /// TypeName (パーサー出力) から TypeRepr を作成
    pub fn from_type_name(
        type_name: &TypeName,
        interner: &StringInterner,
    ) -> Self {
        let specs = CTypeSpecs::from_decl_specs(&type_name.specs, interner);
        let derived = type_name.declarator
            .as_ref()
            .map(|d| CDerivedType::from_derived_decls(&d.derived))
            .unwrap_or_default();
        TypeRepr::CType {
            specs,
            derived,
            source: CTypeSource::Parser,
        }
    }
}
```

#### 2.3 CTypeSource の拡張

```rust
pub enum CTypeSource {
    Apidoc { raw: String },
    Header,
    Parser,  // 追加
}
```

### Phase 3: from_apidoc_string の改善

`parser.rs` を使った新しい実装に置き換える。

#### 3.1 問題: 依存関係

`parse_type_from_string` は以下を必要とする:
- `interner: &StringInterner`
- `files: &FileRegistry`
- `typedefs: &HashSet<InternedStr>`

現在の `from_apidoc_string` は `interner` のみを受け取る。

#### 3.2 解決策: 新しいメソッドを追加

```rust
impl TypeRepr {
    /// C 型文字列から TypeRepr を作成（パーサー版）
    ///
    /// parser.rs の parse_type_from_string を使用。
    /// typedefs が必要なため、semantic.rs など型情報が揃っている
    /// コンテキストでの使用を推奨。
    pub fn from_c_type_string(
        s: &str,
        interner: &StringInterner,
        files: &FileRegistry,
        typedefs: &HashSet<InternedStr>,
    ) -> Self {
        match parse_type_from_string(s, interner, files, typedefs) {
            Ok(type_name) => Self::from_type_name(&type_name, interner),
            Err(_) => {
                // フォールバック: 既存の簡易パーサーを使用
                let (specs, derived) = Self::parse_c_type_string(s, interner);
                TypeRepr::CType {
                    specs,
                    derived,
                    source: CTypeSource::Apidoc { raw: s.to_string() },
                }
            }
        }
    }
}
```

#### 3.3 呼び出し箇所の更新

`semantic.rs` の `SemanticAnalyzer` に `files` と `typedefs` を追加し、
`from_apidoc_string` の呼び出しを `from_c_type_string` に置き換える。

**対象**: `src/semantic.rs:1194` (SymbolLookup での型解決)

```rust
// Before
let resolved = TypeRepr::from_apidoc_string(&ty_str, self.interner);

// After
let resolved = TypeRepr::from_c_type_string(
    &ty_str,
    self.interner,
    self.files,
    self.typedefs,
);
```

#### 3.4 SemanticAnalyzer の拡張

```rust
pub struct SemanticAnalyzer<'a> {
    interner: &'a StringInterner,
    apidoc: Option<&'a ApidocDict>,
    fields_dict: Option<&'a FieldsDict>,
    rust_decl_dict: Option<&'a RustDeclDict>,
    inline_fn_dict: Option<&'a InlineFnDict>,
    files: &'a FileRegistry,       // 追加
    typedefs: &'a HashSet<InternedStr>,  // 追加
    // ...
}
```

### Phase 4: テストと検証

#### 4.1 ユニットテスト

```rust
#[test]
fn test_from_c_type_string_pointer_const() {
    // "COP* const" → CType { specs: TypedefName(COP), derived: [Pointer] }
    // "HV *const" → CType { specs: TypedefName(HV), derived: [Pointer] }
}
```

#### 4.2 回帰テスト

`CopLABEL` と `HvFILL` を回帰テストに追加:

```rust
const TARGET_FUNCTIONS: &[&str] = &[
    // ...
    "CopLABEL",
    "HvFILL",
];
```

#### 4.3 期待される出力

```rust
pub unsafe fn CopLABEL(my_perl: *mut PerlInterpreter, c: *mut COP) -> *const c_char {
    // ...
}

pub unsafe fn HvFILL(my_perl: *mut PerlInterpreter, hv: *mut HV) -> STRLEN {
    // ...
}
```

## 実装順序

1. **Phase 1**: デバッグ出力機能（開発・デバッグの補助として）
2. **Phase 2**: `from_type_name` の実装（基盤）
3. **Phase 3**: `from_c_type_string` と呼び出し箇所の更新（修正本体）
4. **Phase 4**: テストと検証

## リスク

### 循環依存

`type_repr.rs` から `parser.rs` の関数を呼ぶことで、モジュール間の依存関係が
複雑になる可能性がある。

**対策**: `from_c_type_string` は `semantic.rs` など上位モジュールからのみ
呼び出すようにし、`type_repr.rs` 内では使わない。

### パフォーマンス

`parse_type_from_string` は Lexer + Parser を使うため、簡易パーサーより遅い。

**対策**: 型推論は一度だけ実行されるため、パフォーマンス影響は限定的。
必要なら結果をキャッシュする。

## 関連ファイル

| ファイル | 役割 |
|----------|------|
| `src/parser.rs` | `parse_type_from_string` - 型文字列パーサー |
| `src/type_repr.rs` | `TypeRepr` - 型表現、`from_apidoc_string` |
| `src/semantic.rs` | `SemanticAnalyzer` - 意味解析、型制約収集 |
| `src/macro_infer.rs` | マクロ型推論 |
| `src/main.rs` | CLI オプション |

## 完了条件

1. `--debug-type-inference` オプションが動作する
2. `"COP* const"` が正しくパースされる
3. `CopLABEL` と `HvFILL` の引数に正しい型がつく
4. 既存のテストがすべてパスする
5. 新しい回帰テストが追加されている
