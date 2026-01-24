# API 再構成計画

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

## 提案: Pipeline アーキテクチャ

### Config 構造

```rust
/// 全フェーズ共通の設定
pub struct PipelineConfig {
    /// 入力ファイル
    pub input_file: PathBuf,

    // === Preprocessor 設定 ===
    /// インクルードパス (-I)
    pub include_paths: Vec<PathBuf>,
    /// プリプロセッサ定義 (-D)
    pub defines: HashMap<String, Option<String>>,
    /// ターゲットディレクトリ（Perl CORE）
    pub target_dir: Option<PathBuf>,
    /// マクロ展開マーカーを出力
    pub emit_markers: bool,
    /// Perl Config.pm から自動設定（--auto 相当）
    pub auto_perl_config: bool,

    // === Inference 設定 ===
    /// Rust バインディングファイル
    pub bindings_path: Option<PathBuf>,
    /// apidoc ファイルパス（省略時は自動検索）
    pub apidoc_path: Option<PathBuf>,
    /// apidoc ディレクトリ
    pub apidoc_dir: Option<PathBuf>,

    // === Codegen 設定 ===
    /// Rust edition for rustfmt
    pub rust_edition: String,
    /// rustfmt 失敗時にエラー
    pub strict_rustfmt: bool,
    /// マクロ定義位置コメント
    pub macro_comments: bool,

    // === Debug 設定 ===
    pub debug_pp: bool,
    pub dump_apidoc_after_merge: Option<String>,

    // === 内部設定（with_codegen_defaults で設定される） ===
    /// ラップ対象マクロ（inline関数内で特別扱いするマクロ）
    /// デフォルト: ["assert", "assert_"]
    pub wrapped_macros: Vec<String>,
}

impl PipelineConfig {
    /// 最小構成
    pub fn new(input_file: PathBuf) -> Self { ... }

    /// Perl 自動設定を有効化（--auto 相当）
    pub fn with_auto_perl_config(mut self) -> Result<Self, PerlConfigError> { ... }

    /// コード生成に推奨される設定を一括適用
    ///
    /// 以下を設定:
    /// - wrapped_macros: ["assert", "assert_"] （inline関数内のassertを正しく変換）
    /// - その他、コード生成に必要なデフォルト設定
    pub fn with_codegen_defaults(mut self) -> Self { ... }

    /// Builder パターンメソッド群
    pub fn with_bindings(mut self, path: PathBuf) -> Self { ... }
    pub fn with_include(mut self, path: PathBuf) -> Self { ... }
    pub fn with_define(mut self, name: &str, value: Option<&str>) -> Self { ... }
    // ...
}
```

### Pipeline 構造

```rust
/// パイプライン状態マシン
pub struct Pipeline {
    config: PipelineConfig,
    state: PipelineState,
}

enum PipelineState {
    /// 初期状態
    Initial,
    /// プリプロセス完了
    Preprocessed(PreprocessResult),
    /// 推論完了
    Inferred(InferResult),
}

/// プリプロセス結果
pub struct PreprocessResult {
    pub preprocessor: Preprocessor,
}

/// 推論結果（現在の InferResult を継続使用）
pub struct InferResult {
    // ... 既存のフィールド
}

impl Pipeline {
    pub fn new(config: PipelineConfig) -> Result<Self, PipelineError> { ... }

    // === フェーズ別 API ===

    /// Phase 1: プリプロセスのみ実行
    pub fn preprocess(self) -> Result<PreprocessedPipeline, PipelineError> { ... }

    /// Phase 2: 推論まで実行（プリプロセスも含む）
    pub fn infer(self) -> Result<InferredPipeline, PipelineError> { ... }

    /// Phase 3: コード生成まで実行（全フェーズ）
    pub fn generate<W: Write>(self, writer: W) -> Result<GeneratedPipeline<W>, PipelineError> { ... }
}

/// プリプロセス完了状態
pub struct PreprocessedPipeline {
    config: PipelineConfig,
    result: PreprocessResult,
}

impl PreprocessedPipeline {
    /// Preprocessor への参照を取得
    pub fn preprocessor(&self) -> &Preprocessor { ... }

    /// Phase 2: 推論を実行
    pub fn infer(self) -> Result<InferredPipeline, PipelineError> { ... }
}

/// 推論完了状態
pub struct InferredPipeline {
    config: PipelineConfig,
    result: InferResult,
}

impl InferredPipeline {
    /// InferResult への参照を取得
    pub fn result(&self) -> &InferResult { ... }

    /// Phase 3: コード生成
    pub fn generate<W: Write>(self, writer: W) -> Result<GeneratedPipeline<W>, PipelineError> { ... }
}
```

### 使用例

#### 一括実行（build.rs 向け）

```rust
use libperl_macrogen::{Pipeline, PipelineConfig};

fn main() {
    let config = PipelineConfig::new("wrapper.h".into())
        .with_auto_perl_config()?    // Perl Config.pm からインクルードパス等を取得
        .with_codegen_defaults()     // assert 等のラップ設定
        .with_bindings("bindings.rs".into());

    let mut output = File::create("macro_fns.rs")?;
    let pipeline = Pipeline::new(config)?
        .generate(&mut output)?;

    let stats = pipeline.stats();
    println!("Generated {} functions", stats.total_success());
}
```

#### フェーズ別実行（main.rs 向け）

```rust
// -E: プリプロセスのみ
if cli.preprocess_only {
    let pipeline = Pipeline::new(config)?.preprocess()?;
    output_tokens(pipeline.preprocessor());
    return Ok(());
}

// --gen-rust: 全フェーズ
if cli.gen_rust {
    let pipeline = Pipeline::new(config)?
        .generate(&mut output)?;
    return Ok(());
}

// --typed-sexp: 推論まで
if cli.typed_sexp {
    let pipeline = Pipeline::new(config)?.infer()?;
    output_typed_sexp(pipeline.result());
    return Ok(());
}
```

## 実装計画

### Step 1: PipelineConfig の作成

**ファイル**: `src/pipeline.rs`（新規）

- `PipelineConfig` 構造体を定義
- 既存の `InferConfig` と `PPConfig` の内容を統合
- Builder パターンメソッドを実装

### Step 2: Pipeline 型状態マシンの実装

**ファイル**: `src/pipeline.rs`

- `Pipeline`, `PreprocessedPipeline`, `InferredPipeline`, `GeneratedPipeline` を実装
- 各フェーズの遷移メソッドを実装
- 既存の `run_inference_with_preprocessor` のロジックを分解して再利用

### Step 3: 既存 API との互換レイヤー

**ファイル**: `src/infer_api.rs`

- `run_macro_inference` を Pipeline ベースに書き換え
- `run_inference_with_preprocessor` は非推奨マーク後、Pipeline へのラッパーとして維持

### Step 4: main.rs の書き換え

**ファイル**: `src/main.rs`

- PPConfig/Preprocessor の直接構築を Pipeline 経由に変更
- 各モード（-E, --sexp, --typed-sexp, --gen-rust 等）を Pipeline API で実装

### Step 5: lib.rs の re-export 整理

**ファイル**: `src/lib.rs`

- `Pipeline`, `PipelineConfig` を公開
- 旧 API は互換性のため維持

## 変更対象ファイル

| ファイル | 変更内容 |
|----------|----------|
| `src/pipeline.rs` | 新規: Pipeline アーキテクチャ全体 |
| `src/infer_api.rs` | 既存 API を Pipeline ラッパーに変更 |
| `src/main.rs` | Pipeline API を使用するよう書き換え |
| `src/lib.rs` | Pipeline を re-export |
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
