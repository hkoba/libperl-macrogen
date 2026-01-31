# C 関数宣言辞書の実装計画

## 目的

C ヘッダファイルから関数宣言を収集し、`RustDeclDict` と対になる `CFnDeclDict` を構築する。
収集された情報には THX 依存性も含まれ、マクロ解析や将来的な Rust bindings との比較に活用できる。

## 背景

### 現状の課題

1. マクロが外部関数を呼び出す場合、その関数の THX 依存性を検出できない
2. C 関数宣言の情報が体系的に収集されていない
3. `bindings.rs` の情報と C ヘッダの情報を比較する手段がない

### 解決策

`RustDeclDict` と同様の構造で C 関数宣言を収集する `CFnDeclDict` を実装する。

---

## データ構造設計

### `RustDeclDict` との対比

| RustDeclDict | CFnDeclDict | 備考 |
|--------------|-------------|------|
| `RustParam { name, ty }` | `CParam { name, ty }` | パラメータ情報 |
| `RustFn { name, params, ret_ty }` | `CFnDecl { name, params, ret_ty, is_thx, ... }` | 関数情報 + THX |
| - | `is_thx` | C 固有: pTHX_ の有無 |

### 提案する構造

```rust
// src/c_fn_decl.rs (新規作成)

use std::collections::HashMap;
use crate::intern::InternedStr;
use crate::ast::DeclSpecs;

/// C 関数パラメータ
#[derive(Debug, Clone)]
pub struct CParam {
    /// パラメータ名（匿名の場合は None）
    pub name: Option<InternedStr>,
    /// パラメータの型（DeclSpecs + Declarator から構築した文字列表現）
    pub ty: String,
}

/// C 関数宣言
#[derive(Debug, Clone)]
pub struct CFnDecl {
    /// 関数名
    pub name: InternedStr,
    /// パラメータリスト
    pub params: Vec<CParam>,
    /// 戻り値の型（文字列表現）
    pub ret_ty: String,
    /// THX 依存性（pTHX_ または pTHX がパラメータに含まれる）
    pub is_thx: bool,
    /// ターゲットディレクトリで宣言されたか
    pub is_target: bool,
    /// 宣言の場所（ファイルパス:行番号）
    pub location: Option<String>,
}

/// C 関数宣言辞書
#[derive(Debug, Default)]
pub struct CFnDeclDict {
    /// 関数名 → 関数宣言のマッピング
    pub fns: HashMap<InternedStr, CFnDecl>,
}

impl CFnDeclDict {
    pub fn new() -> Self {
        Self::default()
    }

    /// 関数宣言を追加
    pub fn insert(&mut self, decl: CFnDecl) {
        self.fns.insert(decl.name, decl);
    }

    /// 関数が存在するか
    pub fn contains(&self, name: InternedStr) -> bool {
        self.fns.contains_key(&name)
    }

    /// 関数宣言を取得
    pub fn get(&self, name: InternedStr) -> Option<&CFnDecl> {
        self.fns.get(&name)
    }

    /// 関数が THX 依存かどうか
    pub fn is_thx_dependent(&self, name: InternedStr) -> bool {
        self.fns.get(&name).map_or(false, |d| d.is_thx)
    }

    /// THX 依存関数の数
    pub fn thx_count(&self) -> usize {
        self.fns.values().filter(|d| d.is_thx).count()
    }

    /// 登録された関数数
    pub fn len(&self) -> usize {
        self.fns.len()
    }

    /// 空かどうか
    pub fn is_empty(&self) -> bool {
        self.fns.is_empty()
    }
}
```

---

## 収集メカニズム

### pTHX_ 検出（MacroCallWatcher 使用）

```rust
// src/infer_api.rs

// pTHX_ と pTHX のマクロ呼び出しを監視
let pthx_id = pp.interner_mut().intern("pTHX_");
let pthx_no_comma_id = pp.interner_mut().intern("pTHX");
pp.set_macro_called_callback(pthx_id, Box::new(MacroCallWatcher::new()));
pp.set_macro_called_callback(pthx_no_comma_id, Box::new(MacroCallWatcher::new()));

// C 関数宣言辞書を作成
let mut c_fn_decl_dict = CFnDeclDict::new();

parser.parse_each_with_pp(|decl, loc, path, pp| {
    // ... 既存の処理 ...

    // 関数宣言を収集
    if let ExternalDecl::Declaration(declaration) = decl {
        collect_function_declarations(
            declaration,
            &mut c_fn_decl_dict,
            pp,
            pthx_id,
            pthx_no_comma_id,
            loc,
            path,
        );
    }

    ControlFlow::Continue(())
})?;
```

### 関数宣言収集ロジック

```rust
/// 宣言から関数宣言を収集
fn collect_function_declarations(
    declaration: &Declaration,
    dict: &mut CFnDeclDict,
    pp: &mut Preprocessor,
    pthx_id: InternedStr,
    pthx_no_comma_id: InternedStr,
    loc: &SourceLocation,
    path: &Path,
) {
    // pTHX_ または pTHX が呼ばれたかチェック
    let is_thx = check_macro_called(pp, pthx_id) || check_macro_called(pp, pthx_no_comma_id);

    // 各宣言子を処理
    for init_decl in &declaration.declarators {
        let declarator = &init_decl.declarator;

        // 関数宣言かどうかをチェック
        if let Some(param_list) = find_function_params(&declarator.derived) {
            if let Some(name) = declarator.name {
                let c_fn_decl = CFnDecl {
                    name,
                    params: extract_params(param_list, pp.interner()),
                    ret_ty: decl_specs_to_string(&declaration.specs, pp.interner()),
                    is_thx,
                    is_target: declaration.is_target,
                    location: Some(format!("{}:{}", path.display(), loc.line)),
                };
                dict.insert(c_fn_decl);
            }
        }
    }

    // フラグをリセット（次の宣言のために）
    reset_macro_called(pp, pthx_id);
    reset_macro_called(pp, pthx_no_comma_id);
}

/// DerivedDecl から関数パラメータリストを探す
fn find_function_params(derived: &[DerivedDecl]) -> Option<&ParamList> {
    for d in derived {
        if let DerivedDecl::Function(params) = d {
            return Some(params);
        }
    }
    None
}

/// MacroCallWatcher の呼び出しフラグをチェック
fn check_macro_called(pp: &Preprocessor, macro_id: InternedStr) -> bool {
    pp.get_macro_called_callback(macro_id)
        .and_then(|cb| cb.as_any().downcast_ref::<MacroCallWatcher>())
        .map_or(false, |w| w.was_called())
}

/// MacroCallWatcher のフラグをリセット
fn reset_macro_called(pp: &mut Preprocessor, macro_id: InternedStr) {
    if let Some(cb) = pp.get_macro_called_callback(macro_id) {
        if let Some(w) = cb.as_any().downcast_ref::<MacroCallWatcher>() {
            w.take_called();  // フラグを消費してリセット
        }
    }
}
```

---

## 処理フロー

```
┌─────────────────────────────────────────────────────────────────────────┐
│                           入力                                           │
│  wrapper.h → proto.h を include → 関数宣言が展開される                   │
│                                                                         │
│  PERL_CALLCONV XOPRETANY                                                │
│  Perl_custom_op_get_field(pTHX_ const OP *o, const xop_flags_enum field)│
└─────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────┐
│ Stage 1: Preprocessor                                                    │
│                                                                         │
│  pTHX_ マクロが展開される際、MacroCallWatcher が呼び出しを記録           │
│  pTHX_ → pTHX, → tTHX my_perl, → ...                                    │
└─────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────┐
│ Stage 2: Parser                                                          │
│                                                                         │
│  ExternalDecl::Declaration として関数宣言をパース                         │
│  - DeclSpecs: PERL_CALLCONV XOPRETANY                                   │
│  - Declarator.name: Perl_custom_op_get_field                            │
│  - DerivedDecl::Function(params): パラメータリスト                       │
└─────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────┐
│ Stage 3: 関数宣言収集                                                    │
│                                                                         │
│  parse_each_with_pp コールバック内で:                                    │
│  1. ExternalDecl::Declaration を検出                                    │
│  2. DerivedDecl::Function があれば関数宣言と判定                          │
│  3. pTHX_/pTHX の呼び出しフラグをチェック → is_thx を設定                 │
│  4. CFnDecl を構築して CFnDeclDict に登録                                │
│                                                                         │
│  結果:                                                                   │
│  CFnDecl {                                                              │
│      name: "Perl_custom_op_get_field",                                  │
│      params: [...],                                                     │
│      ret_ty: "XOPRETANY",                                               │
│      is_thx: true,      ← pTHX_ があったので true                       │
│      is_target: true,                                                   │
│      location: Some("/usr/lib64/perl5/CORE/proto.h:678"),               │
│  }                                                                      │
└─────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────┐
│ Stage 4: マクロ解析での利用                                              │
│                                                                         │
│  build_macro_info() で:                                                 │
│  - called_functions に "Perl_custom_op_get_field" がある                │
│  - c_fn_decl_dict.is_thx_dependent("Perl_custom_op_get_field") → true  │
│  - → マクロも THX 依存と判定                                             │
└─────────────────────────────────────────────────────────────────────────┘
```

---

## InferResult への統合

```rust
// src/infer_api.rs

/// 型推論の結果
pub struct InferResult {
    // ... 既存フィールド ...

    /// C 関数宣言辞書（新規追加）
    pub c_fn_decl_dict: CFnDeclDict,
}
```

---

## 将来の活用例

### 1. THX 依存性の判定（本計画の主目的）

```rust
// マクロが呼び出す関数の THX 依存性をチェック
let has_thx_from_fn_calls = info.called_functions.iter().any(|fn_name| {
    c_fn_decl_dict.is_thx_dependent(*fn_name)
});
```

### 2. bindings.rs との比較

```rust
// C 宣言と Rust 宣言の突合
for (name, c_decl) in &c_fn_decl_dict.fns {
    let name_str = interner.get(*name);
    if let Some(rust_fn) = rust_decl_dict.fns.get(name_str) {
        // パラメータ数の比較
        let c_param_count = c_decl.params.len();
        let rust_param_count = rust_fn.params.len();

        // THX の場合、C は +1 パラメータがある
        let expected_diff = if c_decl.is_thx { 1 } else { 0 };
        if c_param_count != rust_param_count + expected_diff {
            eprintln!("Parameter count mismatch: {} (C: {}, Rust: {})",
                name_str, c_param_count, rust_param_count);
        }
    }
}
```

### 3. 関数シグネチャの検証

```rust
// マクロが呼び出す関数の存在確認を強化
fn is_function_available_enhanced(&self, fn_name: InternedStr) -> bool {
    // 既存: macros, bindings.rs, inline_fn_dict, builtins
    // 新規: c_fn_decl_dict も確認
    self.c_fn_decl_dict.contains(fn_name)
}
```

---

## 変更対象ファイル

| ファイル | 変更内容 |
|----------|----------|
| `src/c_fn_decl.rs` | **新規作成**: CParam, CFnDecl, CFnDeclDict |
| `src/lib.rs` | モジュール追加 |
| `src/infer_api.rs` | pTHX_ 監視、関数宣言収集、InferResult 拡張 |
| `src/macro_infer.rs` | analyze_all_macros に c_fn_decl_dict を渡す、THX 判定拡張 |

---

## 実装順序

### Phase 1: データ構造の実装

1. [ ] `src/c_fn_decl.rs` を新規作成
   - CParam 構造体
   - CFnDecl 構造体
   - CFnDeclDict 構造体とメソッド
2. [ ] `src/lib.rs` にモジュール追加
3. [ ] 単体テスト作成

### Phase 2: 関数宣言の収集

1. [ ] `infer_api.rs` に pTHX_/pTHX 監視コールバックを追加
2. [ ] `collect_function_declarations()` ヘルパー関数を実装
3. [ ] 型を文字列に変換するヘルパー関数を実装（または既存のものを再利用）
4. [ ] `parse_each_with_pp` 内で CFnDeclDict を構築
5. [ ] `InferResult` に `c_fn_decl_dict` フィールドを追加

### Phase 3: マクロ解析への統合

1. [ ] `analyze_all_macros()` のシグネチャに `c_fn_decl_dict` を追加
2. [ ] `build_macro_info()` で関数呼び出しの THX 依存性をチェック
3. [ ] THX 判定ロジックを拡張

### Phase 4: テストと検証

1. [ ] `cargo test` で回帰テストを実行
2. [ ] 収集された関数宣言数と THX 関数数を確認
3. [ ] `Perl_custom_op_xop` が THX 依存として検出されることを確認
4. [ ] 統計情報の出力を確認（例: `c_fn_decl: 1234 functions (456 THX)`）

---

## 統計情報の拡張

```rust
// src/infer_api.rs

#[derive(Debug, Clone, Default)]
pub struct InferStats {
    // ... 既存フィールド ...

    /// 収集された C 関数宣言数
    pub c_fn_decl_count: usize,
    /// THX 依存の C 関数数
    pub c_fn_thx_count: usize,
}
```

---

## 考慮事項

### 関数の前方宣言と定義

```c
// 前方宣言（proto.h）- ExternalDecl::Declaration
PERL_CALLCONV void Perl_foo(pTHX_ int x);

// 定義（.c ファイル）- ExternalDecl::FunctionDef
void Perl_foo(pTHX_ int x) { ... }
```

- 前方宣言は `Declaration` として収集される
- 関数定義は `FunctionDef` として別途処理される（InlineFnDict）
- 同じ関数が両方に現れる場合、重複を許容（上書き or 無視）

### InternedStr vs String

- `CFnDecl.name`: `InternedStr` を使用（既存の仕組みと一貫性）
- `CFnDecl.ret_ty`, `CParam.ty`: `String` を使用（複雑な型表現のため）

### ターゲット外ファイルの扱い

- `is_target: false` の関数宣言も収集する
- これにより、システムヘッダの関数（glibc 等）の情報も利用可能
- フィルタリングは利用側で行う

---

## 関連ドキュメント

- [THX 依存性検出アーキテクチャ](../architecture-thx-dependency.md)
- [マクロ展開制御アーキテクチャ](../architecture-macro-expansion-control.md)
- [C Inline 関数の処理アーキテクチャ](../architecture-inline-function-processing.md)
