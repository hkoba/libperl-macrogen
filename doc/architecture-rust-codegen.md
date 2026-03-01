# Rust コード生成アーキテクチャ

このドキュメントでは、libperl-macrogen における Rust コード生成の仕組みを解説する。

## 概要

コード生成システムは、型推論結果（`InferResult`）から Rust コードを生成する。
2層構造を採用し、責務を明確に分離している。

```
┌─────────────────────────────────────────────────────────────────────┐
│                        InferResult                                  │
│  ・MacroInferContext (マクロ推論結果)                               │
│  ・InlineFnDict (inline 関数辞書)                                   │
│  ・EnumDict (enum バリアント辞書)                                   │
│  ・RustDeclDict (bindings.rs 情報)                                  │
└─────────────────────────────────────────────────────────────────────┘
                              │
                    ┌─────────┴─────────┐
                    ▼                   ▼
          ┌──────────────────┐  ┌──────────────────┐
          │  BindingsInfo    │  │  KnownSymbols    │
          │  ・static_arrays │  │  ・既知シンボル集合│
          │  ・bitfield      │  │  ・未解決検出用    │
          │    _methods      │  │                  │
          └──────────────────┘  └──────────────────┘
                    │                   │
                    └─────────┬─────────┘
                              ▼
┌─────────────────────────────────────────────────────────────────────┐
│                     CodegenDriver                                    │
│  ・出力先 (Writer) の管理                                           │
│  ・セクション構造の制御                                              │
│  ・依存順でのコード生成（カスケード検出付き）                        │
│  ・動的 use libc 生成                                               │
│  ・統計情報の収集                                                    │
└─────────────────────────────────────────────────────────────────────┘
                              │
          ┌───────────────────┼───────────────────┐
          ▼                                       ▼
┌───────────────────────┐               ┌───────────────────────┐
│     RustCodegen       │               │   フォールバック出力   │
│  ・単一関数の生成      │               │  ・PARSE_FAILED       │
│  ・AST → Rust 変換    │               │  ・TYPE_INCOMPLETE    │
│  ・不完全マーカー管理  │               │  ・CALLS_UNAVAILABLE  │
│  ・未解決シンボル検出  │               │  ・CASCADE_UNAVAILABLE│
│  ・libc 使用追跡       │               │  ・UNRESOLVED_NAMES   │
└───────────────────────┘               └───────────────────────┘
          │
          ▼
┌─────────────────────────────────────────────────────────────────────┐
│                     GeneratedCode                                    │
│  ・code: String (生成されたコード)                                   │
│  ・incomplete_count: usize (不完全マーカー数)                        │
│  ・unresolved_names: Vec<String> (未解決シンボル名)                  │
│  ・used_libc_fns: HashSet<String> (使用した libc 関数名)            │
└─────────────────────────────────────────────────────────────────────┘
```

## 主要コンポーネント

### 1. BindingsInfo - bindings.rs 抽出情報

**責務**: RustDeclDict からコード生成に必要な情報を抽出

```rust
pub struct BindingsInfo {
    /// 配列型の extern static 変数名の集合
    pub static_arrays: HashSet<String>,
    /// ビットフィールドのメソッド名集合（構造体名 → メソッド名セット）
    pub bitfield_methods: HashMap<String, HashSet<String>>,
}
```

### 2. KnownSymbols - 既知シンボル集合

**責務**: コード生成時に未解決シンボルを検出するための参照集合

```rust
pub struct KnownSymbols {
    names: HashSet<String>,
}
```

以下のソースからシンボルを収集:
- bindings.rs の関数名、定数名、型名、構造体名、enum 名、extern static 変数名
- マクロ辞書のマクロ名
- inline 関数辞書の関数名
- ビルトイン関数名
- libc 関数名

### 3. CodegenDriver - 出力管理

**責務**: 生成全体の制御、出力先管理、依存順コード生成、統計収集

```rust
pub struct CodegenDriver<'a, W: Write> {
    writer: W,                              // 出力先
    interner: &'a StringInterner,
    enum_dict: &'a EnumDict,
    macro_ctx: &'a MacroInferContext,
    bindings_info: BindingsInfo,            // bindings.rs 抽出情報
    config: CodegenConfig,                  // 生成設定
    stats: CodegenStats,                    // 統計情報
    used_libc_fns: HashSet<String>,         // 使用された libc 関数名
    successfully_generated_inlines: HashSet<InternedStr>, // 正常生成 inline 関数
}
```

#### 主要メソッド

| メソッド | 役割 |
|----------|------|
| `generate()` | 全体の生成エントリポイント |
| `generate_use_statements()` | 動的 use libc 生成 |
| `generate_enum_imports()` | enum インポートを出力 |
| `generate_inline_fns()` | inline 関数セクション（依存順、カスケード検出） |
| `generate_macros()` | マクロセクション（クロスドメインカスケード検出） |
| `get_macro_status()` | マクロの生成ステータスを判定 |

#### 生成フロー（依存順コード生成）

```
generate()
    │
    ├─ Pass 1: inline 関数の生成（仮）
    │   └─ for each inline_fn:
    │       ├─ RustCodegen::generate_inline_fn()
    │       └─ 完全なら成功バッファに、不完全なら理由を記録
    │
    ├─ Pass 1.5: successfully_generated_inlines 集合を構築
    │
    ├─ Pass 2: カスケード依存の不動点ループ
    │   └─ loop:
    │       ├─ 成功 inline が利用不可 inline を呼ぶ場合 → 降格
    │       └─ 変更がなくなるまで繰り返し
    │
    ├─ Pass 3: inline 関数の出力（最終結果）
    │   └─ 成功/TYPE_INCOMPLETE/CASCADE_UNAVAILABLE/UNRESOLVED_NAMES を出力
    │
    ├─ generate_macros() — クロスドメインカスケード検出
    │   └─ for each macro:
    │       ├─ get_macro_status() で分類
    │       ├─ Success → RustCodegen::generate_macro()
    │       │   └─ 未解決シンボルあり → UNRESOLVED_NAMES としてコメントアウト
    │       │   └─ 依存先 inline が失敗 → CASCADE_UNAVAILABLE
    │       ├─ ParseFailed → generate_macro_parse_failed()
    │       ├─ TypeIncomplete → generate_macro_type_incomplete()
    │       └─ CallsUnavailable → generate_macro_calls_unavailable()
    │
    └─ generate_use_statements()  // 動的 use libc（実際に使用した関数のみ）
```

#### 動的 use libc 生成

`use libc::{...}` 文は、コード生成後に実際に使用された libc 関数のみを含む。
`LIBC_FUNCTIONS` リスト（`strcmp`, `strlen`, `memset` 等）と照合し、
使用されたもののみを動的に出力する。

#### カスケード依存検出

**Inline→Inline カスケード**: Pass 2 の不動点ループで検出。
inline 関数 A が inline 関数 B を呼び出し、B がコード生成失敗した場合、
A も CASCADE_UNAVAILABLE として出力する。

**Macro→Inline クロスドメインカスケード**: `generate_macros()` 内で検出。
マクロが `called_functions` で inline 関数を参照している場合、
その inline 関数が `successfully_generated_inlines` に含まれなければ、
マクロも CASCADE_UNAVAILABLE として出力する。

### 4. RustCodegen - 単一関数生成

**責務**: 単一の関数/マクロを Rust コードに変換

```rust
pub struct RustCodegen<'a> {
    interner: &'a StringInterner,
    enum_dict: &'a EnumDict,
    macro_ctx: &'a MacroInferContext,
    bindings_info: BindingsInfo,            // bindings.rs 抽出情報
    buffer: String,                         // 生成結果バッファ
    incomplete_count: usize,                // 不完全マーカー数
    current_type_param_map: HashMap<InternedStr, String>,   // ジェネリック型パラメータ
    current_literal_string_params: HashSet<InternedStr>,    // &str パラメータ
    current_return_type: Option<String>,    // 戻り値型文字列
    param_substitutions: HashMap<InternedStr, String>,      // lvalue Call 展開用
    current_param_types: HashMap<InternedStr, String>,      // パラメータ型情報
    known_symbols: &'a KnownSymbols,       // 既知シンボル集合
    current_local_names: HashSet<InternedStr>, // ローカルスコープ名
    unresolved_names: Vec<String>,         // 検出された未解決シンボル
    used_libc_fns: HashSet<String>,        // 使用した libc 関数名
}
```

#### 設計方針

- **使い捨てインスタンス**: 各関数の生成ごとに新規作成
- **不完全マーカー追跡**: 型が不明な箇所に `__UNKNOWN__` 等を出力し、カウント
- **未解決シンボル検出**: `KnownSymbols` と照合して未定義シンボルを検出
- **純粋な変換**: IO を持たず、結果を `GeneratedCode` として返す

#### 主要メソッド

| メソッド | 役割 |
|----------|------|
| `generate_macro()` | マクロを Rust 関数に変換 |
| `generate_inline_fn()` | inline 関数を Rust 関数に変換 |
| `expr_to_rust()` | 式を Rust コードに変換（マクロ用、型推論統合） |
| `expr_to_rust_inline()` | 式を Rust コードに変換（inline 関数用） |
| `stmt_to_rust()` | 文を Rust コードに変換 |
| `type_repr_to_rust()` | TypeRepr を Rust 型文字列に変換 |
| `get_param_type()` | パラメータの型を制約から取得 |
| `try_expand_call_as_lvalue()` | Call 式の lvalue 展開（マクロ用） |
| `try_expand_call_as_lvalue_inline()` | Call 式の lvalue 展開（inline 用） |

#### パラメータ型の解決

`get_param_type()` は `param_to_exprs` 逆引き辞書を使用して、パラメータを参照する
式の型制約から型を取得する。複数の制約がある場合、**void 型はスキップ**して
より具体的な型を優先する。

```rust
// 例: CopLABEL(c) の場合
// c に対する制約:
//   - void (symbol lookup)        ← スキップ
//   - *mut COP (arg 1 of func())  ← 採用
```

#### 不完全マーカー

```rust
fn unknown_marker(&mut self) -> &'static str {
    self.incomplete_count += 1;
    "__UNKNOWN__"
}

fn type_marker(&mut self) -> &'static str {
    self.incomplete_count += 1;
    "__TYPE__"
}
```

### 5. GeneratedCode - 生成結果

```rust
pub struct GeneratedCode {
    pub code: String,               // 生成されたコード
    pub incomplete_count: usize,    // 不完全マーカー数
    pub unresolved_names: Vec<String>, // 未解決シンボル名
    pub used_libc_fns: HashSet<String>, // 使用した libc 関数名
}

impl GeneratedCode {
    pub fn is_complete(&self) -> bool {
        self.incomplete_count == 0
    }
}
```

### 6. CodegenConfig - 生成設定

```rust
pub struct CodegenConfig {
    pub emit_inline_fns: bool,          // inline 関数を出力するか
    pub emit_macros: bool,              // マクロを出力するか
    pub include_source_location: bool,  // ソース位置をコメントに含めるか
    pub use_statements: Vec<String>,    // カスタム use 文
}
```

### 7. CodegenStats - 統計情報

```rust
pub struct CodegenStats {
    pub macros_success: usize,              // 正常生成されたマクロ数
    pub macros_parse_failed: usize,         // パース失敗マクロ数
    pub macros_type_incomplete: usize,      // 型推論失敗マクロ数
    pub macros_calls_unavailable: usize,    // 利用不可関数呼び出しマクロ数
    pub macros_cascade_unavailable: usize,  // カスケード依存でコメントアウトされたマクロ数
    pub macros_unresolved_names: usize,     // 未解決シンボルを含むマクロ数
    pub inline_fns_success: usize,              // 正常生成された inline 関数数
    pub inline_fns_type_incomplete: usize,      // 型推論失敗 inline 関数数
    pub inline_fns_unresolved_names: usize,     // 未解決シンボルを含む inline 関数数
    pub inline_fns_cascade_unavailable: usize,  // カスケード依存 inline 関数数
    pub inline_fns_contains_goto: usize,        // goto を含む inline 関数数
}
```

### 8. GenerateStatus - 生成ステータス

```rust
pub enum GenerateStatus {
    Success,            // 正常生成可能
    ParseFailed,        // パース失敗（トークン列のみ）
    TypeIncomplete,     // 型推論不完全
    CallsUnavailable,   // 利用不可関数を呼び出す
    Skip,               // 対象外（非関数形式マクロ等）
}
```

## C → Rust 変換パターン

### 型変換

| C パターン | Rust 変換 | 理由 |
|------------|-----------|------|
| `(enum_type)expr` | `std::mem::transmute::<_, EnumType>(expr)` | Rust では整数→enum の as キャスト不可 |
| `(bool)expr` | `((expr) != 0)` | Rust では整数→bool の as キャスト不可 |
| `NULL` / `(void*)0` | `null_mut()` / `null()` | 型ヒントから const/mut を判定 |
| `offsetof(T, field)` | `std::mem::offset_of!(T, field)` | BuiltinCall AST ノード経由 |

### ポインタ操作

| C パターン | Rust 変換 |
|------------|-----------|
| `arr[i]` (配列型) | `(*arr.as_ptr().offset(i as isize))` |
| `(T)-1` (ジェネリック) | `((-1) as T)` + turbofish 構文 |

### 文字列変換（&str パラメータ）

| C パターン | Rust 変換 |
|------------|-----------|
| `func(str_param)` | `func(str_param.as_ptr() as *const c_char)` |
| `strlen(str_param)` | `str_param.len()` |

### lvalue マクロ展開

lvalue コンテキスト（代入の左辺）でのマクロ/関数呼び出しは、
定義を展開してフィールドアクセスに変換する:

```c
// C: SvFLAGS(sv) |= flag
// パラメータ置換を使って展開 → (*sv).sv_flags |= flag
```

`param_substitutions` テーブルにマクロの仮引数→実引数のマッピングを保持し、
展開時に適用する。

### ジェネリック型パラメータと turbofish 構文

ジェネリック型パラメータを持つマクロは turbofish 構文で呼び出す:

```rust
// 呼び出し側
isPOWER_OF_2::<U32>(n)

// 定義側
pub unsafe fn isPOWER_OF_2<T>(n: T) -> c_int { ... }
```

### StmtExpr 内の let バインディング

Statement expression (`({ ... })`) 内のローカル変数宣言は
Rust の `let` バインディングに変換する:

```c
// C: ({ int __x = expr; __x; })
// Rust: { let __x: c_int = expr; __x }
```

## CodegenDriver と RustCodegen の役割分担

| 観点 | CodegenDriver | RustCodegen |
|------|---------------|-------------|
| インスタンス寿命 | 生成全体で1つ | 関数ごとに使い捨て |
| IO | Writer を保持、直接出力 | バッファに蓄積、結果を返す |
| 状態管理 | 統計情報を累積、libc 使用集約 | 不完全マーカー数、未解決シンボル |
| 出力形式決定 | 完全/不完全に応じて形式を選択 | 常に同じ形式で生成 |
| エラー処理 | フォールバック出力を担当 | 不完全マーカーを挿入 |
| 依存管理 | カスケード検出、依存順出力 | なし |

### 連携パターン

```rust
// CodegenDriver から RustCodegen を呼び出す
fn generate_inline_fns(&mut self, result: &InferResult) -> io::Result<()> {
    for (name, func_def) in fns {
        // 使い捨てインスタンスを作成
        let codegen = RustCodegen::new(
            self.interner, self.enum_dict, self.macro_ctx,
            self.bindings_info.clone(), &known_symbols,
        );
        let generated = codegen.generate_inline_fn(*name, func_def);

        if generated.is_complete() && generated.unresolved_names.is_empty() {
            // 完全な生成：そのまま出力
            write!(self.writer, "{}", generated.code)?;
            self.stats.inline_fns_success += 1;
            self.successfully_generated_inlines.insert(*name);
            self.used_libc_fns.extend(generated.used_libc_fns);
        } else if !generated.unresolved_names.is_empty() {
            // 未解決シンボルあり
            self.stats.inline_fns_unresolved_names += 1;
        } else {
            // 不完全な生成：コメントアウトして出力
            self.stats.inline_fns_type_incomplete += 1;
        }
    }
}
```

## 出力形式

### 成功時（is_complete() == true）

```rust
#[inline]
pub unsafe fn SvPVX(sv: *mut SV) -> *mut c_char {
    ((*SvANY(sv)).xpv_pv)
}
```

### 不完全時（is_complete() == false）

```rust
// [CODEGEN_INCOMPLETE] SomeFunction - inline function
// #[inline]
// pub unsafe fn SomeFunction(arg: __TYPE__) -> __UNKNOWN__ {
//     ...
// }
```

### パース失敗時

```rust
// [PARSE_FAILED] SOME_MACRO(x, y)
// Error: unexpected token
// (tokens not available in parsed form)
```

### 利用不可関数呼び出し時

```rust
// [CALLS_UNAVAILABLE] SOME_MACRO(x) [THX] - calls unavailable function(s)
// Unavailable: some_internal_func
```

### カスケード依存時

```rust
// [CASCADE_UNAVAILABLE] SOME_MACRO(x) - depends on unavailable: OtherMacro
// #[inline]
// pub unsafe fn SOME_MACRO(x: *mut SV) -> c_int { ... }
```

### 未解決シンボル時

```rust
// [UNRESOLVED_NAMES] SOME_MACRO(x) - unresolved: unknown_func
// #[inline]
// pub unsafe fn SOME_MACRO(x: *mut SV) -> c_int { ... }
```

## ヘルパー関数

モジュールレベルで定義される共通ヘルパー:

| 関数 | 役割 |
|------|------|
| `escape_rust_keyword()` | Rust 予約語のエスケープ（r# 付与） |
| `bin_op_to_rust()` | 二項演算子を Rust 形式に変換 |
| `assign_op_to_rust()` | 代入演算子を Rust 形式に変換 |
| `escape_char()` / `escape_string()` | 文字/文字列のエスケープ |
| `is_zero_constant()` | ゼロ定数判定（do-while(0) 検出用） |
| `is_boolean_expr()` | bool 式判定（条件式の変換用） |
| `wrap_as_bool_condition()` | 必要に応じて `!= 0` を追加 |
| `is_null_literal()` | NULL リテラル判定（`null_mut()` / `null()` 変換用） |

## 関連ファイル

| ファイル | 役割 |
|----------|------|
| `src/rust_codegen.rs` | コード生成モジュール本体 |
| `src/infer_api.rs` | InferResult の定義 |
| `src/macro_infer.rs` | MacroInferInfo の定義 |
| `src/type_repr.rs` | TypeRepr の定義 |
| `src/enum_dict.rs` | EnumDict の定義 |
| `src/rust_decl.rs` | RustDeclDict の定義（BindingsInfo のソース） |
