# API 再構成計画

> **採用案**: 案C（フェーズ別 Config + Builder）
> 詳細な比較は `api-config-alternatives.md` を参照

## 現状の問題

### 1. Preprocessor の所有権問題

```
run_macro_inference(InferConfig)
    └── PPConfig を内部で構築
    └── Preprocessor を内部で作成
    └── run_inference_with_preprocessor() を呼び出し

run_inference_with_preprocessor(pp, apidoc_path, bindings_path, debug_opts)
    └── 既存の Preprocessor を受け取る
    └── main.rs から呼ばれる
```

- `run_macro_inference` は PPConfig/Preprocessor を内部で作成するため、CLI オプション（-I, -D など）を渡せない
- `main.rs` は `run_inference_with_preprocessor` を直接呼び出している
- README に書いた `run_macro_inference` は実際には main.rs で使われていない

### 2. フェーズの分離がない

現在は「プリプロセス → 推論 → コード生成」が一体化しており、個別に呼び出せない。

## 設計目標

1. **3フェーズ構成**: Preprocessor → Inference → Codegen
2. **個別/一括呼び出し**: 各フェーズを個別に呼べ、かつ一括実行もできる
3. **統一 Config**: 全フェーズで使う設定を一つの構造体にまとめる

## 提案: Pipeline アーキテクチャ（案C: フェーズ別 Config + Builder）

### フェーズ別 Config 構造

```rust
/// Preprocessor フェーズの設定
pub struct PreprocessConfig {
    /// 入力ファイル
    pub input_file: PathBuf,
    /// インクルードパス (-I)
    pub include_paths: Vec<PathBuf>,
    /// プリプロセッサ定義 (-D)
    pub defines: HashMap<String, Option<String>>,
    /// ターゲットディレクトリ（Perl CORE）
    pub target_dir: Option<PathBuf>,
    /// マクロ展開マーカーを出力
    pub emit_markers: bool,
    /// ラップ対象マクロ（inline関数内で特別扱いするマクロ）
    pub wrapped_macros: Vec<String>,
    /// デバッグ出力
    pub debug_pp: bool,
}

/// Inference フェーズの設定
pub struct InferConfig {
    /// Rust バインディングファイル
    pub bindings_path: Option<PathBuf>,
    /// apidoc ファイルパス（省略時は自動検索）
    pub apidoc_path: Option<PathBuf>,
    /// apidoc ディレクトリ
    pub apidoc_dir: Option<PathBuf>,
    /// apidoc マージ後にダンプして終了
    pub dump_apidoc_after_merge: Option<String>,
}

/// Codegen フェーズの設定
pub struct CodegenConfig {
    /// Rust edition for rustfmt
    pub rust_edition: String,
    /// rustfmt 失敗時にエラー
    pub strict_rustfmt: bool,
    /// マクロ定義位置コメント
    pub macro_comments: bool,
    /// inline 関数を出力
    pub emit_inline_fns: bool,
    /// マクロを出力
    pub emit_macros: bool,
}
```

### Builder 構造

```rust
/// Pipeline を構築するための Builder
pub struct PipelineBuilder {
    preprocess: PreprocessConfig,
    infer: InferConfig,
    codegen: CodegenConfig,
}

impl PipelineBuilder {
    /// 入力ファイルを指定して Builder を作成
    pub fn new(input_file: impl Into<PathBuf>) -> Self { ... }

    // === Preprocess 設定 ===

    /// Perl Config.pm から自動設定（--auto 相当）
    pub fn with_auto_perl_config(mut self) -> Result<Self, PerlConfigError> { ... }

    /// コード生成に推奨される設定を適用
    /// - wrapped_macros: ["assert", "assert_"]
    pub fn with_codegen_defaults(mut self) -> Self { ... }

    /// インクルードパスを追加 (-I)
    pub fn with_include(mut self, path: impl Into<PathBuf>) -> Self { ... }

    /// マクロ定義を追加 (-D)
    pub fn with_define(mut self, name: &str, value: Option<&str>) -> Self { ... }

    // === Infer 設定 ===

    /// Rust バインディングファイルを指定
    pub fn with_bindings(mut self, path: impl Into<PathBuf>) -> Self { ... }

    /// apidoc ファイルを指定
    pub fn with_apidoc(mut self, path: impl Into<PathBuf>) -> Self { ... }

    // === Codegen 設定 ===

    /// rustfmt 失敗時にエラー終了
    pub fn with_strict_rustfmt(mut self) -> Self { ... }

    /// Rust edition を指定
    pub fn with_rust_edition(mut self, edition: &str) -> Self { ... }

    // === Build ===

    /// Pipeline を構築
    pub fn build(self) -> Result<Pipeline, PipelineError> { ... }

    /// PreprocessConfig のみを取り出す（Preprocessor 単独使用時）
    pub fn preprocess_config(self) -> PreprocessConfig { ... }
}
```

### Pipeline 構造（型状態マシン）

```rust
/// 初期状態の Pipeline
pub struct Pipeline {
    preprocess_config: PreprocessConfig,
    infer_config: InferConfig,
    codegen_config: CodegenConfig,
}

impl Pipeline {
    /// Builder から構築
    pub fn builder(input_file: impl Into<PathBuf>) -> PipelineBuilder { ... }

    /// Phase 1: プリプロセスのみ実行
    pub fn preprocess(self) -> Result<PreprocessedPipeline, PipelineError> { ... }

    /// Phase 2: 推論まで実行（プリプロセスも含む）
    pub fn infer(self) -> Result<InferredPipeline, PipelineError> { ... }

    /// Phase 3: コード生成まで実行（全フェーズ）
    pub fn generate<W: Write>(self, writer: W) -> Result<GeneratedPipeline, PipelineError> { ... }
}

/// プリプロセス完了状態
pub struct PreprocessedPipeline {
    preprocessor: Preprocessor,
    infer_config: InferConfig,
    codegen_config: CodegenConfig,
}

impl PreprocessedPipeline {
    /// Preprocessor への参照を取得
    pub fn preprocessor(&self) -> &Preprocessor { ... }

    /// Preprocessor を消費して取得
    pub fn into_preprocessor(self) -> Preprocessor { ... }

    // === Infer 設定を追加で指定可能 ===

    pub fn with_bindings(mut self, path: impl Into<PathBuf>) -> Self { ... }
    pub fn with_apidoc(mut self, path: impl Into<PathBuf>) -> Self { ... }

    /// Phase 2: 推論を実行
    pub fn infer(self) -> Result<InferredPipeline, PipelineError> { ... }
}

/// 推論完了状態
pub struct InferredPipeline {
    result: InferResult,  // 既存の InferResult を使用
    codegen_config: CodegenConfig,
}

impl InferredPipeline {
    /// InferResult への参照を取得
    pub fn result(&self) -> &InferResult { ... }

    /// InferResult を消費して取得
    pub fn into_result(self) -> InferResult { ... }

    // === Codegen 設定を追加で指定可能 ===

    pub fn with_strict_rustfmt(mut self) -> Self { ... }
    pub fn with_rust_edition(mut self, edition: &str) -> Self { ... }

    /// Phase 3: コード生成
    pub fn generate<W: Write>(self, writer: W) -> Result<GeneratedPipeline, PipelineError> { ... }
}

/// コード生成完了状態
pub struct GeneratedPipeline {
    result: InferResult,
    stats: CodegenStats,
}

impl GeneratedPipeline {
    /// 統計情報を取得
    pub fn stats(&self) -> &CodegenStats { ... }

    /// InferResult を取得（後続処理用）
    pub fn into_result(self) -> InferResult { ... }
}
```

### 使用例

#### 一括実行（build.rs 向け）

```rust
use libperl_macrogen::Pipeline;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut output = File::create("macro_fns.rs")?;

    let generated = Pipeline::builder("wrapper.h")
        .with_auto_perl_config()?    // Perl Config.pm からインクルードパス等を取得
        .with_codegen_defaults()     // assert 等のラップ設定
        .with_bindings("bindings.rs")
        .build()?
        .generate(&mut output)?;

    let stats = generated.stats();
    println!("Generated {} functions", stats.total_success());
    Ok(())
}
```

#### フェーズ別実行（main.rs 向け）

```rust
// Builder で共通設定を構築
let pipeline = Pipeline::builder(&input)
    .with_auto_perl_config()?
    .with_codegen_defaults()
    .build()?;

// -E: プリプロセスのみ
if cli.preprocess_only {
    let preprocessed = pipeline.preprocess()?;
    output_tokens(preprocessed.preprocessor());
    return Ok(());
}

// --typed-sexp: 推論まで（bindings は推論段階で追加）
if cli.typed_sexp {
    let inferred = pipeline
        .preprocess()?
        .with_bindings(&bindings_path)  // Infer 設定を追加
        .infer()?;
    output_typed_sexp(inferred.result());
    return Ok(());
}

// --gen-rust: 全フェーズ
if cli.gen_rust {
    let generated = pipeline
        .preprocess()?
        .with_bindings(&bindings_path)
        .infer()?
        .with_strict_rustfmt()  // Codegen 設定を追加
        .generate(&mut output)?;
    return Ok(());
}
```

#### 段階的実行（設定を各フェーズで追加）

```rust
// Phase 1: Preprocess
let preprocessed = Pipeline::builder("wrapper.h")
    .with_auto_perl_config()?
    .with_codegen_defaults()
    .build()?
    .preprocess()?;

// Preprocessor を使って何か処理...
println!("Macros: {}", preprocessed.preprocessor().macros().len());

// Phase 2: Infer（bindings をここで追加）
let inferred = preprocessed
    .with_bindings("bindings.rs")
    .infer()?;

// InferResult を使って何か処理...
println!("Inline fns: {}", inferred.result().inline_fn_dict.len());

// Phase 3: Generate（rustfmt 設定をここで追加）
let generated = inferred
    .with_strict_rustfmt()
    .generate(&mut output)?;

println!("Stats: {:?}", generated.stats());
```

## 実装計画

### Step 1: フェーズ別 Config の作成

**ファイル**: `src/pipeline.rs`（新規）

- `PreprocessConfig`, `InferConfig`, `CodegenConfig` を定義
- 各 Config に Default 実装
- `PreprocessConfig::from_perl_config()` を実装

### Step 2: PipelineBuilder の実装

**ファイル**: `src/pipeline.rs`

- `PipelineBuilder` 構造体を定義
- Builder メソッド群を実装:
  - `with_auto_perl_config()`, `with_codegen_defaults()`
  - `with_include()`, `with_define()`
  - `with_bindings()`, `with_apidoc()`
  - `with_strict_rustfmt()`, `with_rust_edition()`
- `build()` メソッドで `Pipeline` を構築

### Step 3: Pipeline 型状態マシンの実装

**ファイル**: `src/pipeline.rs`

- `Pipeline`, `PreprocessedPipeline`, `InferredPipeline`, `GeneratedPipeline` を実装
- 各フェーズの遷移メソッドを実装:
  - `Pipeline::preprocess()` → `PreprocessedPipeline`
  - `PreprocessedPipeline::infer()` → `InferredPipeline`
  - `InferredPipeline::generate()` → `GeneratedPipeline`
- 各状態で追加設定可能なメソッドを実装
- 既存の `run_inference_with_preprocessor` のロジックを分解して再利用

### Step 4: 既存 API との互換レイヤー

**ファイル**: `src/infer_api.rs`

- `run_macro_inference` を Pipeline ベースに書き換え
- `run_inference_with_preprocessor` は非推奨マーク後、内部で Pipeline を使用

### Step 5: main.rs の書き換え

**ファイル**: `src/main.rs`

- `PipelineBuilder` を使用して設定を構築
- 各モード（-E, --sexp, --typed-sexp, --gen-rust 等）を Pipeline API で実装

### Step 6: lib.rs の re-export 整理

**ファイル**: `src/lib.rs`

- `Pipeline`, `PipelineBuilder`, フェーズ別 Config を公開
- 旧 API は互換性のため維持（非推奨マーク付き）

## 変更対象ファイル

| ファイル | 変更内容 |
|----------|----------|
| `src/pipeline.rs` | 新規: フェーズ別 Config, PipelineBuilder, Pipeline 型状態マシン |
| `src/infer_api.rs` | 既存 API を Pipeline ラッパーに変更、非推奨マーク |
| `src/main.rs` | PipelineBuilder を使用するよう書き換え |
| `src/lib.rs` | Pipeline, PipelineBuilder, フェーズ別 Config を re-export |
| `README.md` | 新 API の使用例に更新 |

## 段階的移行

1. **Phase 1**: `pipeline.rs` を追加し、新 API を実装（既存コードは変更しない）
2. **Phase 2**: main.rs を新 API に移行、動作確認
3. **Phase 3**: 旧 API を非推奨にマーク
4. **Phase 4**: README.md を更新

## 検討事項

### Q1: PreprocessResult に何を含めるか？

現在の `run_inference_with_preprocessor` は Preprocessor を消費している。
PreprocessResult は Preprocessor そのものを保持し、次のフェーズに渡す。

### Q2: InferResult の Preprocessor 所有権

現在 InferResult は Preprocessor を所有している（interner へのアクセス用）。
この設計は維持する。

### Q3: DebugOptions の扱い

デバッグダンプ（`--dump-apidoc-after-merge` など）は各フェーズの途中で
早期終了する機能。Pipeline でどう扱うか？

→ 各フェーズの結果型に `is_debug_exit: bool` フラグを持たせるか、
   または Result の Err バリアントとして `DebugExit` を追加する。
