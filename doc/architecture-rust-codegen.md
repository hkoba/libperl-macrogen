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
                              ▼
┌─────────────────────────────────────────────────────────────────────┐
│                     CodegenDriver                                    │
│  ・出力先 (Writer) の管理                                           │
│  ・セクション構造の制御                                              │
│  ・生成ステータスに応じた出力形式の決定                              │
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
└───────────────────────┘               └───────────────────────┘
          │
          ▼
┌─────────────────────────────────────────────────────────────────────┐
│                     GeneratedCode                                    │
│  ・code: String (生成されたコード)                                   │
│  ・incomplete_count: usize (不完全マーカー数)                        │
└─────────────────────────────────────────────────────────────────────┘
```

## 主要コンポーネント

### 1. CodegenDriver - 出力管理

**責務**: 生成全体の制御、出力先管理、統計収集

```rust
pub struct CodegenDriver<'a, W: Write> {
    writer: W,                              // 出力先
    interner: &'a StringInterner,
    enum_dict: &'a EnumDict,
    macro_ctx: &'a MacroInferContext,
    config: CodegenConfig,                  // 生成設定
    stats: CodegenStats,                    // 統計情報
}
```

#### 主要メソッド

| メソッド | 役割 |
|----------|------|
| `generate()` | 全体の生成エントリポイント |
| `generate_use_statements()` | use 文セクションを出力 |
| `generate_enum_imports()` | enum インポートを出力 |
| `generate_inline_fns()` | inline 関数セクションを生成 |
| `generate_macros()` | マクロセクションを生成 |
| `get_macro_status()` | マクロの生成ステータスを判定 |

#### 生成フロー

```
generate()
    │
    ├─ generate_use_statements()    // use std::ffi::...
    │
    ├─ generate_enum_imports()      // use crate::EnumName::*;
    │
    ├─ generate_inline_fns()
    │   └─ for each inline_fn:
    │       ├─ RustCodegen::generate_inline_fn()
    │       └─ 完全/不完全に応じて出力形式を決定
    │
    └─ generate_macros()
        └─ for each macro:
            ├─ get_macro_status() で分類
            ├─ Success → RustCodegen::generate_macro()
            ├─ ParseFailed → generate_macro_parse_failed()
            ├─ TypeIncomplete → generate_macro_type_incomplete()
            └─ CallsUnavailable → generate_macro_calls_unavailable()
```

### 2. RustCodegen - 単一関数生成

**責務**: 単一の関数/マクロを Rust コードに変換

```rust
pub struct RustCodegen<'a> {
    interner: &'a StringInterner,
    enum_dict: &'a EnumDict,
    macro_ctx: &'a MacroInferContext,
    buffer: String,                         // 生成結果バッファ
    incomplete_count: usize,                // 不完全マーカー数
}
```

#### 設計方針

- **使い捨てインスタンス**: 各関数の生成ごとに新規作成
- **不完全マーカー追跡**: 型が不明な箇所に `__UNKNOWN__` 等を出力し、カウント
- **純粋な変換**: IO を持たず、結果を `GeneratedCode` として返す

#### 主要メソッド

| メソッド | 役割 |
|----------|------|
| `generate_macro()` | マクロを Rust 関数に変換 |
| `generate_inline_fn()` | inline 関数を Rust 関数に変換 |
| `expr_to_rust()` | 式を Rust コードに変換 |
| `stmt_to_rust()` | 文を Rust コードに変換 |
| `type_repr_to_rust()` | TypeRepr を Rust 型文字列に変換 |

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

### 3. GeneratedCode - 生成結果

```rust
pub struct GeneratedCode {
    pub code: String,           // 生成されたコード
    pub incomplete_count: usize, // 不完全マーカー数
}

impl GeneratedCode {
    pub fn is_complete(&self) -> bool {
        self.incomplete_count == 0
    }
}
```

### 4. CodegenConfig - 生成設定

```rust
pub struct CodegenConfig {
    pub emit_inline_fns: bool,          // inline 関数を出力するか
    pub emit_macros: bool,              // マクロを出力するか
    pub include_source_location: bool,  // ソース位置をコメントに含めるか
    pub use_statements: Vec<String>,    // カスタム use 文
}
```

### 5. GenerateStatus - 生成ステータス

```rust
pub enum GenerateStatus {
    Success,            // 正常生成可能
    ParseFailed,        // パース失敗（トークン列のみ）
    TypeIncomplete,     // 型推論不完全
    CallsUnavailable,   // 利用不可関数を呼び出す
    Skip,               // 対象外（非関数形式マクロ等）
}
```

## CodegenDriver と RustCodegen の役割分担

| 観点 | CodegenDriver | RustCodegen |
|------|---------------|-------------|
| インスタンス寿命 | 生成全体で1つ | 関数ごとに使い捨て |
| IO | Writer を保持、直接出力 | バッファに蓄積、結果を返す |
| 状態管理 | 統計情報を累積 | 不完全マーカー数のみ |
| 出力形式決定 | 完全/不完全に応じて形式を選択 | 常に同じ形式で生成 |
| エラー処理 | フォールバック出力を担当 | 不完全マーカーを挿入 |

### 連携パターン

```rust
// CodegenDriver から RustCodegen を呼び出す
fn generate_inline_fns(&mut self, result: &InferResult) -> io::Result<()> {
    for (name, func_def) in fns {
        // 使い捨てインスタンスを作成
        let codegen = RustCodegen::new(self.interner, self.enum_dict, self.macro_ctx);
        let generated = codegen.generate_inline_fn(*name, func_def);

        if generated.is_complete() {
            // 完全な生成：そのまま出力
            write!(self.writer, "{}", generated.code)?;
            self.stats.inline_fns_success += 1;
        } else {
            // 不完全な生成：コメントアウトして出力
            writeln!(self.writer, "// [CODEGEN_INCOMPLETE] {}", name_str)?;
            for line in generated.code.lines() {
                writeln!(self.writer, "// {}", line)?;
            }
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

## 関連ファイル

| ファイル | 役割 |
|----------|------|
| `src/rust_codegen.rs` | コード生成モジュール本体 |
| `src/infer_api.rs` | InferResult の定義 |
| `src/macro_infer.rs` | MacroInferInfo の定義 |
| `src/type_repr.rs` | TypeRepr の定義 |
| `src/enum_dict.rs` | EnumDict の定義 |
