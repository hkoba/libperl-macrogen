# Enum バリアント処理の改善計画

## 概要

生成された Rust コードで、C の enum 定数に関する2種類のエラーが発生している。
本計画では、`parse_each` 時に enum 情報を収集し、コード生成時にそれを活用することで
これらの問題を解決する。

## 問題の詳細

### 問題1: E0425 - enum バリアントがスコープに見つからない

```rust
// 生成されたコード
let pv = Perl_SvPV_helper(my_perl, sv, &mut len, SV_GMAGIC, SvPVnormal_type_, ...);
```

```
error[E0425]: cannot find value `SvPVnormal_type_` in this scope
help: consider importing this unit variant
   4 + use crate::PL_SvPVtype::SvPVnormal_type_;
```

**原因**: `PL_SvPVtype` enum のバリアント `SvPVnormal_type_` が import されていない。

### 問題2: E0408 - パターンマッチで変数束縛として扱われる

```rust
// 生成されたコード
match r#type {
    SVt_PVHV | SVt_PVAV | SVt_PVOBJ => { ... }
    ...
}
```

```
error[E0408]: variable `SVt_PVAV` is not bound in all patterns
help: use the full path in the pattern
   1860 |   SVt_PVHV | crate::svtype::SVt_PVAV | SVt_PVOBJ => {
```

**原因**: Rust の match パターンでは、単純な識別子は新しい変数束縛として解釈される。
enum バリアントとして扱うには、フルパスまたは use 文が必要。

## 元の C ヘッダー定義

```c
typedef enum {
    SvPVutf8_type_,
    SvPVbyte_type_,
    SvPVnormal_type_,
    SvPVforce_type_,
    SvPVutf8_pure_type_,
    SvPVbyte_pure_type_
} PL_SvPVtype;

typedef enum {
    SVt_NULL,       /* 0 */
    SVt_IV,         /* 1 */
    SVt_NV,         /* 2 */
    SVt_PV,         /* 3 */
    ...
    SVt_LAST        /* keep last in enum. used to size arrays */
} svtype;
```

## 解決方針

### 方針1: target enum のバリアントを一括 import

C ヘッダーで定義された enum の定数名は、元のプログラムで衝突しないように
選ばれているはずなので、`use crate::EnumName::*;` で一括 import しても安全。

### 方針2: パターンマッチでは enum バリアントを prefix

match パターンに出現する識別子が enum バリアントである場合、
`crate::EnumName::VariantName` のようにフルパスで出力する。

## 設計

### 新規データ構造: EnumDict

```rust
// src/enum_dict.rs (新規ファイル)

use std::collections::{HashMap, HashSet};
use crate::intern::InternedStr;

/// Enum バリアント名 → Enum 名のマッピング
#[derive(Debug, Default)]
pub struct EnumDict {
    /// バリアント名 → enum 名
    /// 同じバリアント名が複数の enum で使われる可能性は低いが、念のため HashSet
    variant_to_enum: HashMap<InternedStr, HashSet<InternedStr>>,

    /// enum 名 → バリアント名リスト
    enum_to_variants: HashMap<InternedStr, Vec<InternedStr>>,

    /// target ディレクトリで定義された enum 名のセット
    target_enums: HashSet<InternedStr>,
}

impl EnumDict {
    pub fn new() -> Self {
        Self::default()
    }

    /// enum 定義を収集
    pub fn collect_enum(
        &mut self,
        enum_name: InternedStr,
        variants: &[InternedStr],
        is_target: bool,
    ) {
        for &variant in variants {
            self.variant_to_enum
                .entry(variant)
                .or_default()
                .insert(enum_name);
        }
        self.enum_to_variants.insert(enum_name, variants.to_vec());
        if is_target {
            self.target_enums.insert(enum_name);
        }
    }

    /// バリアント名から enum 名を取得（一意の場合のみ Some）
    pub fn get_enum_for_variant(&self, variant: InternedStr) -> Option<InternedStr> {
        self.variant_to_enum.get(&variant).and_then(|enums| {
            if enums.len() == 1 {
                enums.iter().next().copied()
            } else {
                None  // 複数の enum で同名バリアントがある場合は None
            }
        })
    }

    /// バリアント名かどうかをチェック
    pub fn is_enum_variant(&self, name: InternedStr) -> bool {
        self.variant_to_enum.contains_key(&name)
    }

    /// target ディレクトリで定義された enum のイテレータ
    pub fn target_enums(&self) -> impl Iterator<Item = InternedStr> + '_ {
        self.target_enums.iter().copied()
    }
}
```

### Step 1: parse_each 時に enum を収集

**ファイル:** `src/fields_dict.rs` または新規 `src/enum_dict.rs`

```rust
// ExternalDecl から enum 情報を収集
pub fn collect_from_external_decl(
    &mut self,
    decl: &ExternalDecl,
    is_target: bool,
    interner: &StringInterner,
) {
    if let ExternalDecl::Declaration(d) = decl {
        for type_spec in &d.specs.type_specs {
            if let TypeSpec::Enum(spec) = type_spec {
                self.collect_from_enum_spec(spec, is_target, interner);
            }
        }
    }
}

fn collect_from_enum_spec(
    &mut self,
    spec: &EnumSpec,
    is_target: bool,
    interner: &StringInterner,
) {
    // enum 名がある場合のみ収集
    if let Some(enum_name) = spec.name {
        if let Some(enumerators) = &spec.enumerators {
            let variants: Vec<InternedStr> = enumerators
                .iter()
                .map(|e| e.name)
                .collect();
            self.collect_enum(enum_name, &variants, is_target);
        }
    }
}
```

### Step 2: infer_api.rs で EnumDict を使用

**ファイル:** `src/infer_api.rs`

```rust
pub fn run_macro_inference(...) -> Result<(Vec<MacroInferInfo>, FieldsDict, EnumDict)> {
    let mut enum_dict = EnumDict::new();

    parser.parse_each_with_pp(|decl, _loc, _path, pp| {
        let interner = pp.interner();

        // 既存の fields_dict 収集
        fields_dict.collect_from_external_decl(decl, decl.is_target(), interner);

        // 新規: enum 情報を収集
        enum_dict.collect_from_external_decl(decl, decl.is_target(), interner);

        // ... 既存の処理 ...
    })?;

    Ok((results, fields_dict, enum_dict))
}
```

### Step 3: コード生成で enum import を出力

**ファイル:** `src/rust_codegen.rs`

```rust
impl<'a> RustCodegen<'a> {
    pub fn new(
        writer: Box<dyn Write + 'a>,
        interner: &'a StringInterner,
        enum_dict: &'a EnumDict,  // 追加
        // ...
    ) -> Self { ... }

    fn write_header(&mut self) -> io::Result<()> {
        // ... 既存の use 文 ...

        // target enum のバリアントを import
        for enum_name in self.enum_dict.target_enums() {
            let name = self.interner.get(enum_name);
            writeln!(self.writer, "use crate::{}::*;", name)?;
        }

        Ok(())
    }
}
```

### Step 4: パターンマッチで enum バリアントを prefix

**ファイル:** `src/rust_codegen.rs`

match パターン生成時に、識別子が enum バリアントかどうかをチェックし、
そうであれば `crate::EnumName::VariantName` 形式で出力する。

```rust
/// match パターン用の式を Rust に変換
/// enum バリアントの場合はフルパスで出力
fn expr_to_rust_pattern(&mut self, expr: &Expr) -> String {
    match &expr.kind {
        ExprKind::Ident(name) => {
            // enum バリアントかチェック
            if let Some(enum_name) = self.enum_dict.get_enum_for_variant(*name) {
                let enum_str = self.interner.get(enum_name);
                let variant_str = self.interner.get(*name);
                format!("crate::{}::{}", enum_str, variant_str)
            } else {
                self.interner.get(*name).to_string()
            }
        }
        // 他の式は通常の変換
        _ => self.expr_to_rust_inline(expr)
    }
}
```

switch 文から match への変換部分を修正:

```rust
// collect_switch_cases 内
let pattern_strs: Vec<String> = patterns.iter()
    .map(|e| self.expr_to_rust_pattern(e))  // expr_to_rust_inline → expr_to_rust_pattern
    .collect();
```

## 実装順序

1. **Step 1**: `EnumDict` 構造体を作成（新規ファイル `src/enum_dict.rs`）
2. **Step 2**: `parse_each` 時に enum 情報を収集
3. **Step 3**: コード生成で `use crate::EnumName::*;` を出力
4. **Step 4**: `expr_to_rust_pattern` を追加し、match パターンで enum prefix
5. **Step 5**: テストと検証

## 変更対象ファイル

| ファイル | 変更内容 |
|----------|----------|
| `src/enum_dict.rs` | 新規作成: EnumDict 構造体 |
| `src/lib.rs` | `pub mod enum_dict;` 追加 |
| `src/infer_api.rs` | EnumDict の収集と返却 |
| `src/rust_codegen.rs` | enum import 出力、パターンマッチ prefix |
| `src/main.rs` | EnumDict の受け渡し（必要に応じて） |

## テスト方法

結合テストには以下のスクリプトを使用する:

```bash
~/blob/libperl-rs/12-macrogen-2-build.zsh
```

テスト結果の確認:
- **エラーログ**: `tmp/build-error.log`
- **生成コード**: `tmp/macro_bindings.rs`

エラー数の確認:
```bash
grep -c "^error\[E" tmp/build-error.log
```

enum 関連エラーの確認:
```bash
# E0425 (not found in scope)
grep "^error\[E0425\]" tmp/build-error.log | wc -l

# E0408 (variable not bound in all patterns)
grep "^error\[E0408\]" tmp/build-error.log | wc -l
```

## 備考

- anonymous enum（名前のない enum）は収集しない
- 同じバリアント名が複数の enum で使われている場合は prefix を付けない
  （コンパイラのエラーメッセージで対応を促す）
- `typedef enum { ... } Name;` 形式の場合、typedef 名を enum 名として使用
- `enum Name { ... };` 形式の場合、enum タグ名を使用

## 想定される効果

- E0425 エラー（`SvPVnormal_type_` 等）: import により解消
- E0408 エラー（パターンマッチ）: フルパス化により解消
- enum 関連のエラーが大幅に減少することを期待
