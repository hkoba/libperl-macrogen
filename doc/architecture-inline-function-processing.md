# C Inline 関数の処理アーキテクチャ

## 概要

本ドキュメントは、C inline 関数が収集され、Rust 関数に変換されるまでの流れを説明する。
マクロ関数との違いに焦点を当て、assert などの処理がどのように異なるかを解説する。

**関連ドキュメント**: [マクロ展開制御アーキテクチャ](./architecture-macro-expansion-control.md)

---

## マクロと Inline 関数の処理経路比較

```
┌─────────────────────────────────────────────────────────────────────────┐
│                    C ヘッダファイル (wrapper.h)                          │
│                                                                         │
│  #define FOO(x) ...              PERL_STATIC_INLINE I32*               │
│  (マクロ定義)                     Perl_CvDEPTH(const CV * const sv)      │
│                                   { ... }  (inline 関数定義)             │
└────────────────────────┬──────────────────────────┬─────────────────────┘
                         │                          │
         ┌───────────────┴───────────────┐          │
         │       マクロの処理経路         │          │
         │                               │          │
         │  1. Preprocessor で定義収集    │          │
         │  2. Preprocessor で展開        │          │
         │     (explicit_expand で制御)   │          │
         │  3. パース → MacroInferInfo    │          │
         │  4. 型推論 (TypeEnv)           │          │
         │  5. expr_to_rust() で変換      │          │
         │                               │          │
         │  ★ wrapped_macros で          │          │
         │    assert 引数を保存          │          │
         │    → MacroBegin/End マーカー   │          │
         │    → Parser で Assert に変換  │          │
         │                               │          │
         │  ★ MacroCall AST ノードで     │          │
         │    元のマクロ呼び出しを保存    │          │
         └───────────────────────────────┘          │
                         │                          │
                         ▼                          ▼
         ┌───────────────────────────────┐  ┌─────────────────────────────┐
         │ MacroInferContext.macros     │  │ Inline 関数の処理経路        │
         │ (MacroInferInfo)             │  │                             │
         └───────────────────────────────┘  │ 1. Parser で関数定義パース   │
                                           │    (マクロ展開あり)          │
                                           │    ★ wrapped_macros で      │
                                           │      assert 引数を保存      │
                                           │ 2. InlineFnDict に収集       │
                                           │ 3. FunctionDef を保存        │
                                           │ 4. generate_inline_fn()     │
                                           │ 5. expr_to_rust_inline()    │
                                           │    で変換                    │
                                           │                             │
                                           │ ★ with_codegen_defaults()   │
                                           │   必須（省略時 { 0; }; に）  │
                                           └─────────────────────────────┘
                                                        │
                                                        ▼
                                           ┌─────────────────────────────┐
                                           │ InlineFnDict               │
                                           │ (FunctionDef AST)          │
                                           └─────────────────────────────┘
```

---

## Inline 関数の処理フロー詳細

### Stage 1: パース時の収集

**場所**: `src/infer_api.rs:237-253`

```rust
let mut inline_fn_dict = InlineFnDict::new();

parser.parse_each_with_pp(|decl, _loc, _path, pp| {
    let interner = pp.interner();

    // inline 関数を収集
    if decl.is_target() {
        if let ExternalDecl::FunctionDef(func_def) = decl {
            inline_fn_dict.collect_from_function_def(func_def, interner);
        }
    }
    ControlFlow::Continue(())
})?;
```

**この段階での状態**:
- C コードは Preprocessor でマクロ展開**済み**
- assert マクロも展開されている（`PERL_ARGS_ASSERT_*` → 空、`assert(...)` → 条件式のまま）
- 関数定義全体が `FunctionDef` AST として取得される

---

### Stage 2: InlineFnDict への収集

**場所**: `src/inline_fn.rs:51-69`

```rust
pub fn collect_from_function_def(&mut self, func_def: &FunctionDef, interner: &StringInterner) {
    // inline 関数のみを対象
    if !func_def.specs.is_inline {
        return;
    }

    let name = match func_def.declarator.name {
        Some(n) => n,
        None => return,
    };

    // ★ 重要: クローンして assert 呼び出しを変換
    let mut func_def = func_def.clone();
    convert_assert_calls_in_compound_stmt(&mut func_def.body, interner);

    self.insert(name, func_def);
}
```

**assert 変換の理由**:
- パース時点では `assert(cond)` は単なる関数呼び出し (`Call`) として AST に存在
- マクロの場合は `wrapped_macros` により Parser が `Assert` ノードを作成する
- inline 関数はマクロ展開後にパースされるため、`assert` が展開**されてしまっている**
- そのため、収集時に明示的に `Call("assert", ...)` → `Assert { kind, condition }` の変換が必要

---

### Stage 3: assert 変換処理

**場所**: `src/macro_infer.rs:1471-1636`

#### detect_assert_kind()
```rust
pub fn detect_assert_kind(name: &str) -> Option<AssertKind> {
    match name {
        "assert" => Some(AssertKind::Assert),
        "assert_" => Some(AssertKind::AssertUnderscore),
        _ => None,
    }
}
```

#### convert_assert_calls_in_compound_stmt()
```rust
pub fn convert_assert_calls_in_compound_stmt(
    compound: &mut crate::ast::CompoundStmt,
    interner: &StringInterner
) {
    use crate::ast::BlockItem;
    for item in &mut compound.items {
        if let BlockItem::Stmt(s) = item {
            convert_assert_calls_in_stmt(s, interner);
        }
    }
}
```

#### convert_assert_calls_in_stmt()

文のすべての種類を再帰的に処理:
- `Expr` 文 → `convert_assert_calls()` を呼び出し
- `If` 文 → 条件と両方の分岐を処理
- `While`, `For`, `DoWhile` → 条件と本体を処理
- `Compound` → 再帰的に処理
- など

#### convert_assert_calls()

式ツリーを走査し、`Call { func: Ident("assert" | "assert_"), args }` を
`Assert { kind, condition }` に変換:

```rust
// 変換前
Call {
    func: Ident("assert"),
    args: [condition_expr]
}

// 変換後
Assert {
    kind: AssertKind::Assert,
    condition: Box::new(condition_expr)
}
```

---

### Stage 4: Rust コード生成

**場所**: `src/rust_codegen.rs:997-1039`

```rust
pub fn generate_inline_fn(
    mut self,
    name: InternedStr,
    func_def: &FunctionDef
) -> GeneratedCode {
    let name_str = self.interner.get(name);

    // パラメータリストを生成
    let params_str = self.build_fn_param_list(&func_def.declarator.derived);

    // 戻り値の型を取得
    let return_type = self.decl_specs_to_rust(&func_def.specs);

    // ドキュメントコメント
    self.writeln(&format!("/// {} - inline function", name_str));
    self.writeln("#[inline]");

    // 関数シグネチャ
    self.writeln(&format!(
        "pub unsafe fn {}({}) -> {} {{",
        name_str, params_str, return_type
    ));

    // 関数本体（unsafe ブロックが必要かチェック）
    let needs_unsafe = func_def.function_call_count > 0
                    || func_def.deref_count > 0;
    if needs_unsafe {
        self.writeln("    unsafe {");
        let body_str = self.compound_stmt_to_string(&func_def.body, "        ");
        self.buffer.push_str(&body_str);
        self.writeln("    }");
    } else {
        let body_str = self.compound_stmt_to_string(&func_def.body, "    ");
        self.buffer.push_str(&body_str);
    }

    self.writeln("}");
    self.into_generated_code()
}
```

---

### Stage 5: 式の Rust 変換

**場所**: `src/rust_codegen.rs:1453-1652`

`expr_to_rust_inline()` はマクロ用の `expr_to_rust()` とは別のメソッド:

| 特徴 | expr_to_rust() (マクロ用) | expr_to_rust_inline() |
|------|---------------------------|----------------------|
| 型情報 | TypeEnv から取得 | なし（AST のみ） |
| 変数参照 | パラメータリンク解決 | 単純な識別子変換 |
| 複雑さ | 高（型推論統合） | 低（直接変換） |

#### Assert の変換（両方で同じ）

```rust
ExprKind::Assert { kind, condition } => {
    let cond = self.expr_to_rust_inline(condition);
    let assert_expr = if is_boolean_expr(condition) {
        format!("assert!({})", cond)
    } else {
        format!("assert!(({}) != 0)", cond)
    };
    match kind {
        AssertKind::Assert => assert_expr,
        AssertKind::AssertUnderscore => format!("{{ {}; }}", assert_expr),
    }
}
```

**Perl_CvDEPTH の例**:

```c
// 元の C コード
PERL_STATIC_INLINE I32 *
Perl_CvDEPTH(const CV * const sv)
{
    PERL_ARGS_ASSERT_CVDEPTH;  // → 空に展開
    assert(SvTYPE(sv) == SVt_PVCV || SvTYPE(sv) == SVt_PVFM);
    return &((XPVCV*)SvANY(sv))->xcv_depth;
}
```

```rust
// 生成される Rust コード（with_codegen_defaults() 使用時）
#[inline]
pub unsafe fn Perl_CvDEPTH(sv: *const CV) -> *mut I32 {
    unsafe {
        { 0; };  // PERL_ARGS_ASSERT_CVDEPTH（DEBUGGING 未定義で空）
        assert!((SvTYPE(sv) == SVt_PVCV || SvTYPE(sv) == SVt_PVFM) != 0);
        return (&mut (*((*sv).sv_any as *mut XPVCV)).xcv_depth);
    }
}
```

**重要**: 上記の適切な `assert!()` 出力を得るには、Pipeline API で `with_codegen_defaults()` を
呼び出す必要がある。詳細は後述の「wrapped_macros との関連」を参照。

**`with_codegen_defaults()` を使用しない場合**:
```rust
// with_codegen_defaults() 未使用時の出力
#[inline]
pub unsafe fn Perl_CvDEPTH(sv: *const CV) -> *mut I32 {
    unsafe {
        { 0; };  // PERL_ARGS_ASSERT_CVDEPTH
        { 0; };  // assert が空に展開されてしまう
        return (&mut (*((*sv).sv_any as *mut XPVCV)).xcv_depth);
    }
}
```

---

## Inline 関数の型情報活用

### マクロからの参照時

マクロが inline 関数を呼び出す場合、型推論で inline 関数のシグネチャが参照される:

**場所**: `src/semantic.rs:551-583`

```rust
fn lookup_inline_fn_param_type(
    &self,
    func_name: InternedStr,
    arg_index: usize
) -> Option<Type> {
    let dict = self.inline_fn_dict?;
    let func_def = dict.get(func_name)?;

    // ParamList を取得
    let param_list = func_def.declarator.derived.iter()
        .find_map(|d| match d {
            DerivedDecl::Function(params) => Some(params),
            _ => None,
        })?;

    let param = param_list.params.get(arg_index)?;

    // 型を構築
    let base_ty = self.resolve_decl_specs_readonly(&param.specs);
    // ...
}
```

**使用例**:
```c
// inline 関数
static inline I32* Perl_CvDEPTH(const CV* sv) { ... }

// マクロ
#define CvDEPTH(cv) Perl_CvDEPTH(cv)
```

マクロ `CvDEPTH` の型推論時:
1. `Perl_CvDEPTH(cv)` の呼び出しを検出
2. `lookup_inline_fn_param_type()` で `CV*` を取得
3. `cv` パラメータに `CV*` 型制約を追加
4. 戻り値型も `lookup_inline_fn_return_type()` で取得

---

## 制御点まとめ

### Inline 関数固有の制御点

| 制御点 | 場所 | 役割 |
|--------|------|------|
| **G: is_inline チェック** | `inline_fn.rs:52` | inline 関数のみ収集 |
| **H: assert 変換** | `inline_fn.rs:66` | 収集時に assert 呼び出しを変換 |
| **I: expr_to_rust_inline()** | `rust_codegen.rs:1453` | 型推論なしの式変換 |
| **J: generate_inline_fn()** | `rust_codegen.rs:997` | Rust 関数生成 |

### マクロ展開制御との関連

| マクロ制御点 | Inline 関数での状況 |
|-------------|-------------------|
| A: skip_expand_macros | **適用済み** - パース時にマクロ展開済み |
| B: NoExpandSymbols | **関係なし** - inline 関数は通常のパース時展開 |
| B': ExplicitExpandSymbols | **関係なし** - 型推論用の展開制御 |
| C: Preprocessor 展開制御 | **適用済み** - パース時に展開済み |
| C': MacroCall AST ノード | **関係なし** - inline 関数内では使用されない |
| D: is_function_available() | **inline 関数も可用** として認識 |
| D': wrapped_macros | **適用** - assert の引数保存に必須（下記参照） |
| E: MacroCall 出力判定 | **関係なし** - inline 関数は別経路 |
| F: escape_rust_keyword() | **共通使用** |

**注**: Inline 関数は Preprocessor で完全にマクロ展開されるため、
マクロ型推論用の制御点（MacroCall ノード等）は適用されない。
詳細は [マクロ展開制御アーキテクチャ](./architecture-macro-expansion-control.md) を参照。

---

## wrapped_macros との関連

### 背景: assert マクロの展開問題

Perl の `assert` マクロは `DEBUGGING` が定義されていない場合、`((void)0)` に展開される:

```c
// perl.h での定義（DEBUGGING 未定義時）
#define assert(x)  ((void)0)
```

inline 関数のパース時、Preprocessor がマクロを展開するため、
`assert(cond)` は `((void)0)` となり、**条件式が消失**してしまう。

### wrapped_macros による解決

`with_codegen_defaults()` を呼び出すと、`wrapped_macros` に `assert`, `assert_` が登録される:

```rust
// src/pipeline.rs
pub fn with_codegen_defaults(mut self) -> Self {
    self.preprocess.wrapped_macros = vec![
        "assert".to_string(),
        "assert_".to_string(),
    ];
    self
}
```

`wrapped_macros` に登録されたマクロは、展開結果を `MacroBegin`/`MacroEnd` マーカーで囲む:

```
assert(cond)
    │
    ▼ Preprocessor で展開
┌─────────────────────────────────────────────┐
│ MacroBegin { name: "assert", args: [cond] } │
│ ((void)0)  ← 展開結果                        │
│ MacroEnd                                    │
└─────────────────────────────────────────────┘
    │
    ▼ Parser で検出
┌─────────────────────────────────────────────┐
│ Assert {                                    │
│     kind: AssertKind::Assert,               │
│     condition: Box::new(cond)  ← 引数から復元│
│ }                                           │
└─────────────────────────────────────────────┘
```

### Pipeline API での必須設定

```rust
// ✓ 正しい使い方
Pipeline::builder("wrapper.h")
    .with_auto_perl_config()?
    .with_bindings(&bindings_path)
    .with_codegen_defaults()       // ← 必須: assert 保存機構を有効化
    .build()?
    .generate(&mut output)?;

// ✗ 誤った使い方（assert が { 0; }; になる）
Pipeline::builder("wrapper.h")
    .with_auto_perl_config()?
    .with_bindings(&bindings_path)
    // with_codegen_defaults() を忘れている
    .build()?
    .generate(&mut output)?;
```

詳細は [マクロ展開制御アーキテクチャ - 制御点 D'](./architecture-macro-expansion-control.md#制御点-d-wrapped_macros-assert-保存機構) を参照。

---

## ユースケース別ガイド

### ユースケース 1: Inline 関数内の assert を抑制したい

**現状**: assert は `{ 0; };` または `assert!(...)` として出力される

**完全に削除したい場合**:

```rust
// src/inline_fn.rs の collect_from_function_def() を修正
pub fn collect_from_function_def(&mut self, func_def: &FunctionDef, interner: &StringInterner) {
    // ...
    let mut func_def = func_def.clone();

    // assert 呼び出しを削除（変換ではなく）
    remove_assert_statements(&mut func_def.body, interner);

    self.insert(name, func_def);
}
```

**assert! を debug_assert! にしたい場合**:

```rust
// src/rust_codegen.rs の expr_to_rust_inline() を修正
ExprKind::Assert { kind, condition } => {
    let cond = self.expr_to_rust_inline(condition);
    // assert! → debug_assert! に変更
    format!("debug_assert!(({}) != 0)", cond)
}
```

---

### ユースケース 2: 特定の inline 関数を Rust 生成から除外したい

**場所**: `src/rust_codegen.rs` の `CodegenDriver::generate()`

```rust
// generate() メソッド内
for (name, func_def) in &result.inline_fn_dict.iter() {
    let name_str = interner.get(*name);

    // 除外リスト
    let skip_inline = ["Perl_DebugFunc", "Perl_InternalFunc"];
    if skip_inline.contains(&name_str) {
        continue;
    }

    // 生成処理...
}
```

---

### ユースケース 3: Inline 関数のシグネチャをカスタマイズしたい

**場所**: `src/rust_codegen.rs:997-1039`

```rust
pub fn generate_inline_fn(...) -> GeneratedCode {
    // パラメータ型のカスタマイズ
    let params_str = self.build_fn_param_list_with_overrides(
        &func_def.declarator.derived,
        &custom_param_types  // カスタム型マッピング
    );

    // 戻り値型のカスタマイズ
    let return_type = custom_return_types
        .get(name_str)
        .unwrap_or_else(|| self.decl_specs_to_rust(&func_def.specs));

    // ...
}
```

---

## マクロと Inline 関数の処理比較表

| 観点 | マクロ | Inline 関数 |
|------|--------|-------------|
| **保存形式** | `MacroInferInfo` (パース結果) | `FunctionDef` (完全な AST) |
| **assert 処理タイミング** | Preprocessor 展開後、Parser で | 収集時 (`collect_from_function_def`) |
| **assert 引数保存** | wrapped_macros で MacroBegin/End マーカー | 同左（`with_codegen_defaults()` 必須） |
| **型推論** | TypeEnv による制約ベース | なし（AST 直接参照） |
| **Rust 変換メソッド** | `expr_to_rust()` | `expr_to_rust_inline()` |
| **可用性判定** | bindings.rs, builtins 確認 | InlineFnDict に存在すれば可用 |
| **展開制御** | explicit_expand_macros, skip_expand | なし（パース時に展開済み） |
| **マクロ呼び出し保存** | MacroCall AST ノード | なし |

---

## ファイル別責務

| ファイル | Inline 関数関連の責務 |
|----------|----------------------|
| `inline_fn.rs` | InlineFnDict 定義、収集、assert 変換呼び出し |
| `macro_infer.rs` | `convert_assert_calls*` 関数群 |
| `semantic.rs` | inline 関数シグネチャの型参照 |
| `rust_codegen.rs` | `generate_inline_fn()`, `expr_to_rust_inline()` |
| `infer_api.rs` | パイプラインでの収集統合 |

---

## 今後の拡張ポイント

### Inline 関数のマクロ展開をより細かく制御したい場合

現状では、inline 関数本体は Preprocessor でマクロ展開済みの状態でパースされる。
特定のマクロだけ展開を抑制したい場合は:

1. **Preprocessor レベル**: `skip_expand_macros` に追加（全体に影響）
2. **Parser レベル**: マーカー付きパースで検出（現状は未実装）
3. **収集後レベル**: AST を走査して特定パターンを検出・変換

### Inline 関数に型推論を適用したい場合

現状の `expr_to_rust_inline()` は型推論なしで直接変換する。
型情報が必要な場合は:

1. `SemanticAnalyzer` で inline 関数本体を解析
2. `TypeEnv` を構築
3. `expr_to_rust()` と同様の型情報付き変換を実装
