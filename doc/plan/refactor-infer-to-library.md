# 型推論機能のライブラリ化

## 背景

現在、マクロ型推論の主要ロジックは `main.rs` の `run_infer_macro_types()` に実装されている。
これを別の Rust ライブラリの `build.rs` から呼び出せるようにするため、
ライブラリレベルに移動する。

## 目標

1. 型推論に必要な機能をライブラリ API として提供
2. `MacroInferContext` を含む推論結果を戻り値として返す
3. `PPConfig` 作成機能と `resolve_apidoc_path` もライブラリ化
4. `build.rs` から簡単に呼び出せる高レベル API を提供

## 現在の依存関係

`run_infer_macro_types()` が依存する機能:

```
main.rs
├── PPConfig 作成 (--auto モード)
│   ├── get_perl_config()
│   ├── get_default_target_dir()
│   └── parse_defines()
├── resolve_apidoc_path()
│   ├── find_apidoc_dir()
│   ├── get_perl_version()
│   └── ApidocDict::find_json_for_version()
└── run_infer_macro_types()
    ├── Preprocessor
    ├── Parser
    ├── FieldsDict
    ├── InlineFnDict
    ├── ApidocDict / ApidocCollector
    ├── RustDeclDict
    ├── MacroInferContext
    └── NoExpandSymbols
```

## 設計

### 1. 新規モジュール: `src/infer_api.rs`

高レベル API を提供する新モジュール:

```rust
//! マクロ型推論の高レベル API
//!
//! build.rs や外部ツールから型推論を実行するための API を提供する。

use std::path::Path;

/// 型推論の設定
pub struct InferConfig {
    /// 入力ファイル（wrapper.h など）
    pub input_file: PathBuf,
    /// apidoc ファイルのパス（省略時は自動検索）
    pub apidoc_path: Option<PathBuf>,
    /// Rust バインディングファイルのパス
    pub bindings_path: Option<PathBuf>,
    /// apidoc ディレクトリの検索パス（省略時は自動検索）
    pub apidoc_dir: Option<PathBuf>,
    /// デバッグ出力
    pub debug: bool,
}

/// 型推論の結果
pub struct InferResult {
    /// マクロ推論コンテキスト（全マクロの解析結果）
    pub infer_ctx: MacroInferContext,
    /// フィールド辞書
    pub fields_dict: FieldsDict,
    /// インライン関数辞書
    pub inline_fn_dict: InlineFnDict,
    /// Apidoc 辞書
    pub apidoc: ApidocDict,
    /// Rust 宣言辞書
    pub rust_decl_dict: Option<RustDeclDict>,
    /// typedef 辞書
    pub typedefs: TypedefDict,
    /// プリプロセッサ（マクロテーブル等へのアクセス用）
    pub preprocessor: Preprocessor,
    /// 統計情報
    pub stats: InferStats,
}

// 注: StringInterner と FileRegistry は Preprocessor 経由でアクセス可能
// - preprocessor.interner() -> &StringInterner
// - preprocessor.files() -> &FileRegistry

/// 統計情報
pub struct InferStats {
    pub apidoc_from_comments: usize,
    pub sv_any_constraint_count: usize,
    pub sv_u_field_constraint_count: usize,
    pub thx_dependent_count: usize,
}

/// Perl 環境向けの型推論を実行
///
/// PPConfig を自動構築し、型推論を実行して結果を返す。
/// build.rs から呼び出すことを想定。
pub fn run_macro_inference(config: InferConfig) -> Result<InferResult, InferError> {
    // 1. PPConfig を構築
    // 2. Preprocessor を初期化してファイルを処理
    // 3. パースと型推論を実行
    // 4. InferResult を構築して返す
}

/// Perl 環境用の PPConfig を構築
pub fn build_perl_pp_config() -> Result<PPConfig, PerlConfigError> {
    let perl_cfg = get_perl_config()?;
    let target_dir = get_default_target_dir().ok();
    Ok(PPConfig {
        include_paths: perl_cfg.include_paths,
        predefined: perl_cfg.defines,
        debug_pp: false,
        target_dir,
        emit_markers: false,
    })
}
```

### 2. `perl_config.rs` の拡張

PPConfig 構築のヘルパー関数を追加:

```rust
/// Perl 環境用の PPConfig を構築
///
/// get_perl_config() と get_default_target_dir() を組み合わせて
/// プリプロセッサ設定を構築する。
pub fn build_pp_config_for_perl() -> Result<PPConfig, PerlConfigError> {
    let perl_cfg = get_perl_config()?;
    let target_dir = get_default_target_dir().ok();
    Ok(PPConfig {
        include_paths: perl_cfg.include_paths,
        predefined: perl_cfg.defines,
        debug_pp: false,
        target_dir,
        emit_markers: false,
    })
}
```

### 3. `apidoc.rs` の拡張

apidoc パス解決をライブラリ関数として追加:

```rust
/// apidoc ファイルのパスを解決
///
/// - explicit_path が Some なら: そのまま返す
/// - explicit_path が None で auto_mode なら: Perl バージョンから自動検索
/// - それ以外: None を返す
///
/// apidoc_dir: 検索対象ディレクトリ（None なら find_apidoc_dir() で検索）
pub fn resolve_apidoc_path(
    explicit_path: Option<&Path>,
    auto_mode: bool,
    apidoc_dir: Option<&Path>,
) -> Result<Option<PathBuf>, ApidocResolveError>

/// apidoc ディレクトリを検索
///
/// 検索順序:
/// 1. 指定されたベースディレクトリの apidoc/
/// 2. 実行ファイルからの相対パス
/// 3. カレントディレクトリの apidoc/
pub fn find_apidoc_dir_from(base_dir: Option<&Path>) -> Option<PathBuf>

/// apidoc 解決エラー
#[derive(Debug)]
pub enum ApidocResolveError {
    DevelopmentVersion { major: u32, minor: u32 },
    DirectoryNotFound,
    JsonNotFound { path: PathBuf, major: u32, minor: u32 },
    VersionError(PerlConfigError),
}
```

### 4. エラー型の整理

新しいエラー型を `src/infer_api.rs` に定義:

```rust
/// 型推論エラー
#[derive(Debug)]
pub enum InferError {
    /// Perl 設定取得エラー
    PerlConfig(PerlConfigError),
    /// apidoc 解決エラー
    ApidocResolve(ApidocResolveError),
    /// プリプロセッサエラー
    Preprocess(CompileError),
    /// パースエラー
    Parse(CompileError),
    /// ファイル I/O エラー
    Io(std::io::Error),
}
```

## 実装フェーズ

### Phase 1: エラー型の定義

1. `src/infer_api.rs` を作成
2. `InferError`, `ApidocResolveError` を定義
3. `lib.rs` にモジュール追加

### Phase 2: apidoc.rs の拡張

1. `find_apidoc_dir_from()` を追加（既存の main.rs から移動）
2. `resolve_apidoc_path()` を追加（既存の main.rs から移動）
3. `ApidocResolveError` を apidoc.rs に定義

### Phase 3: perl_config.rs の拡張

1. `build_pp_config_for_perl()` を追加

### Phase 4: infer_api.rs の実装

1. `InferConfig`, `InferResult`, `InferStats` を定義
2. `run_macro_inference()` を実装
   - main.rs の `run_infer_macro_types()` から推論ロジックを移植
   - 出力処理は含めない
3. `build_perl_pp_config()` を実装

### Phase 5: main.rs のリファクタリング

1. `run_infer_macro_types()` を `infer_api::run_macro_inference()` を使うように変更
2. 統計・詳細出力は main.rs に残す
3. 重複コードを削除

### Phase 6: lib.rs のエクスポート

1. 新しい API を公開エクスポート
2. ドキュメントコメントを整備

### Phase 7: テストと動作確認

1. `cargo test` で既存テストが通ることを確認
2. `cargo run -- --auto samples/wrapper.h` の動作確認
3. API の使用例をドキュメント化

## 期待される API 使用例

### build.rs から使う場合

```rust
use libperl_macrogen::{InferConfig, run_macro_inference};

fn main() {
    let config = InferConfig {
        input_file: "wrapper.h".into(),
        apidoc_path: None,  // 自動検索
        bindings_path: Some("bindings.rs".into()),
        apidoc_dir: Some("apidoc".into()),
        debug: false,
    };

    let result = run_macro_inference(config).expect("inference failed");

    // StringInterner と FileRegistry は Preprocessor 経由でアクセス
    let interner = result.preprocessor.interner();
    let files = result.preprocessor.files();

    // result.infer_ctx から必要な情報を取得してコード生成
    for (name, info) in &result.infer_ctx.macros {
        if info.is_target && info.is_expression() {
            let name_str = interner.get(*name);
            // コード生成...
        }
    }
}
```

### CLI から使う場合（従来通り）

```bash
cargo run -- --auto samples/wrapper.h
```

## 追加考慮事項

### Preprocessor の所有権

`run_macro_inference()` は内部で `Preprocessor` を作成し、
処理完了後に `InferResult` に含めて返す。
呼び出し元は `preprocessor.interner()` や `preprocessor.files()` を通じて
`StringInterner` や `FileRegistry` にアクセスできる。

### InternedStr の解決

`MacroInferContext` 内の `InternedStr` は `preprocessor.interner()` で解決:

```rust
let result = run_macro_inference(config)?;
let interner = result.preprocessor.interner();

for (name, info) in &result.infer_ctx.macros {
    let name_str = interner.get(*name);
    // ...
}
```

### ソース位置の解決

ソース位置情報は `preprocessor.files()` で解決:

```rust
let files = result.preprocessor.files();
let loc = files.location(file_id, offset);
```
