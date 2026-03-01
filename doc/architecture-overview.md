# アーキテクチャドキュメント概要

本ドキュメントは、libperl-macrogen の各アーキテクチャドキュメントの関係を
全体像として示し、目的別のガイドを提供する。

## ドキュメント一覧

| ドキュメント | 主題 | 主要ファイル |
|-------------|------|-------------|
| [意味解析と型推論](./architecture-semantic-type-inference.md) | マクロの型推論パイプライン | `macro_infer.rs`, `semantic.rs`, `type_env.rs`, `type_repr.rs` |
| [マクロ展開制御](./architecture-macro-expansion-control.md) | マクロ展開の制御点と判定フロー | `preprocessor.rs`, `macro_infer.rs`, `parser.rs` |
| [Rust コード生成](./architecture-rust-codegen.md) | CodegenDriver/RustCodegen の構造 | `rust_codegen.rs` |
| [Inline 関数処理](./architecture-inline-function-processing.md) | Inline 関数の収集・変換・カスケード検出 | `inline_fn.rs`, `rust_codegen.rs` |
| [THX 依存性検出](./architecture-thx-dependency.md) | THX（Thread Context）の検出と伝播 | `macro_infer.rs`, `rust_codegen.rs` |
| [FieldsDict](./architecture-fields-dict.md) | 構造体フィールド辞書と型推論での活用 | `fields_dict.rs`, `semantic.rs` |

## 全体パイプライン

```
┌─────────────────────────────────────────────────────────────────────────┐
│  入力: wrapper.h, bindings.rs, embed.fnc (apidoc)                      │
└────────────────────────────────┬────────────────────────────────────────┘
                                 │
                                 ▼
┌─────────────────────────────────────────────────────────────────────────┐
│  Stage 1: 前処理・パース                                                 │
│                                                                         │
│  Preprocessor → Parser → AST                                           │
│  ├─ InlineFnDict 収集 (inline_fn.rs)    → [Inline 関数処理]             │
│  ├─ FieldsDict 収集 (fields_dict.rs)    → [FieldsDict]                 │
│  ├─ EnumDict 収集                                                       │
│  └─ マクロ展開制御 (preprocessor.rs)    → [マクロ展開制御]               │
└────────────────────────────────┬────────────────────────────────────────┘
                                 │
                                 ▼
┌─────────────────────────────────────────────────────────────────────────┐
│  Stage 2: マクロ型推論 (analyze_all_macros)                               │
│                                                                         │
│  ├─ Step 1: build_macro_info()          → [マクロ展開制御]               │
│  ├─ Step 2: build_use_relations()       → [意味解析と型推論]             │
│  ├─ Step 3: THX/pasting 伝播            → [THX 依存性検出]               │
│  ├─ Step 4.5: マクロ可用性チェック       → [意味解析と型推論]             │
│  ├─ Step 4.6: inline 関数可用性チェック  → [Inline 関数処理]             │
│  ├─ Step 4.7: クロスドメイン推移閉包     → [Inline 関数処理]             │
│  └─ Step 5-6: 依存順型推論             → [意味解析と型推論]             │
└────────────────────────────────┬────────────────────────────────────────┘
                                 │
                                 ▼
┌─────────────────────────────────────────────────────────────────────────┐
│  Stage 3: Rust コード生成 (CodegenDriver)                                 │
│                                                                         │
│  ├─ precompute_macro_generability()     → [Rust コード生成]              │
│  ├─ generate_inline_fns()               → [Inline 関数処理]             │
│  │   └─ カスケード検出 (4方向)          → [Inline 関数処理]             │
│  ├─ generate_macros()                   → [Rust コード生成]              │
│  │   └─ クロスドメインカスケード        → [Rust コード生成]              │
│  └─ generate_use_statements()           → [Rust コード生成]              │
└────────────────────────────────┬────────────────────────────────────────┘
                                 │
                                 ▼
┌─────────────────────────────────────────────────────────────────────────┐
│  出力: macro_bindings.rs (Rust 関数定義)                                 │
└─────────────────────────────────────────────────────────────────────────┘
```

## ドキュメント間の関連

### 依存性追跡（横断的関心事）

マクロと inline 関数の利用可能性を追跡する依存性追跡は複数のドキュメントにまたがる:

```
                  推論段階                              codegen 段階
           ┌─────────────────────┐             ┌─────────────────────────┐
           │ [意味解析と型推論]    │             │ [Rust コード生成]        │
           │                     │             │                         │
           │ Step 4.5: マクロ     │             │ precompute_macro_       │
           │   可用性チェック     │             │   generability()        │
           │                     │             │                         │
           │ Step 4.6: inline    │             │ generate_inline_fns()   │
           │   可用性チェック     │             │   Pass 2: 不動点ループ  │
           │                     │             │                         │
           │ Step 4.7: クロス     │             │ generate_macros()       │
           │   ドメイン伝播      │             │   カスケード検出        │
           └─────────────────────┘             └─────────────────────────┘
                      │                                    │
                      └──────────┬─────────────────────────┘
                                 │
                      ┌──────────▼──────────┐
                      │ [Inline 関数処理]    │
                      │                     │
                      │ InlineFnDict:       │
                      │   called_functions  │
                      │   calls_unavailable │
                      └─────────────────────┘
```

### カスケード検出の 4 方向

| 方向 | 推論段階（Step 4.7） | codegen 段階 |
|------|---------------------|-------------|
| Macro→Macro | `propagate_unavailable_cross_domain` | `generate_macros()` |
| Inline→Inline | `propagate_unavailable_cross_domain` | `generate_inline_fns()` Pass 2 |
| Macro→Inline | `propagate_unavailable_cross_domain` | `generate_macros()` |
| Inline→Macro | `propagate_unavailable_cross_domain` | `generate_inline_fns()` Pass 2 + `generatable_macros` |

### assert 処理（横断的関心事）

assert の処理フローは複数のドキュメントにまたがる:

| 段階 | 内容 | ドキュメント |
|------|------|-------------|
| Preprocessor | `wrapped_macros` で引数を保存 | [マクロ展開制御](./architecture-macro-expansion-control.md) |
| Parser | `MacroBegin`/`MacroEnd` → `Assert` AST ノード | [マクロ展開制御](./architecture-macro-expansion-control.md) |
| InlineFnDict 収集 | `convert_assert_calls_in_compound_stmt()` | [Inline 関数処理](./architecture-inline-function-processing.md) |
| 依存性収集 | `ExprKind::Assert` の `collect_uses_from_expr`/`collect_function_calls_from_expr` | [意味解析と型推論](./architecture-semantic-type-inference.md) |
| codegen | `assert!(...)` / `debug_assert!(...)` 生成 | [Rust コード生成](./architecture-rust-codegen.md) |

### THX 依存性（横断的関心事）

| 段階 | 内容 | ドキュメント |
|------|------|-------------|
| 初期検出 | `aTHX`, `tTHX`, `my_perl` の検出 | [THX 依存性検出](./architecture-thx-dependency.md) |
| 伝播 | `used_by` グラフ経由の BFS | [THX 依存性検出](./architecture-thx-dependency.md) |
| codegen | `my_perl` パラメータ追加・注入 | [THX 依存性検出](./architecture-thx-dependency.md) |

## 目的別ガイド

### 新しいマクロの展開ルールを追加したい

→ [マクロ展開制御](./architecture-macro-expansion-control.md) の「ユースケース別ガイド」を参照

### 型推論の精度を改善したい

→ [意味解析と型推論](./architecture-semantic-type-inference.md) の Phase 2-3 と
  [FieldsDict](./architecture-fields-dict.md) の活用方法を参照

### コード生成のパターンを追加・変更したい

→ [Rust コード生成](./architecture-rust-codegen.md) の C → Rust 変換パターンを参照

### inline 関数の処理を変更したい

→ [Inline 関数処理](./architecture-inline-function-processing.md) のユースケース別ガイドを参照

### カスケード検出の仕組みを理解したい

→ [Inline 関数処理](./architecture-inline-function-processing.md) の「依存性追跡」セクションと
  [Rust コード生成](./architecture-rust-codegen.md) の「カスケード依存検出」セクションを参照

### ExprKind に新しいバリアントを追加したい

以下の全てを更新する必要がある:
1. `src/ast.rs` - バリアント定義
2. `src/parser.rs` - パース処理
3. `src/sexp.rs` - `SexpPrinter::print_expr` と `TypedSexpPrinter::print_expr`
4. `src/semantic.rs` - `collect_expr_constraints`
5. `src/macro_infer.rs` - `convert_assert_calls`, `collect_uses_from_expr`, `collect_function_calls_from_expr`
6. `src/rust_codegen.rs` - `infer_type_hint`, `expr_to_rust`, `expr_to_rust_inline`

## 主要データ構造の関連

```
MacroInferContext                    InlineFnDict
├─ macros: HashMap<MacroInferInfo>   ├─ fns: HashMap<FunctionDef>
│  ├─ uses / used_by                 ├─ called_functions: HashMap<HashSet>
│  ├─ called_functions               └─ calls_unavailable: HashSet
│  ├─ calls_unavailable
│  └─ type_env: TypeEnv              FieldsDict
│     └─ param_constraints           ├─ field_to_structs
│     └─ return_constraints          ├─ field_types
│                                    └─ consistent_type_cache
CodegenDriver
├─ bindings_info: BindingsInfo       KnownSymbols
├─ successfully_generated_inlines    └─ names: HashSet<String>
├─ generatable_macros
└─ stats: CodegenStats
```
