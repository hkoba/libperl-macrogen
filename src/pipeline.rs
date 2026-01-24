//! Pipeline API for libperl-macrogen
//!
//! 3フェーズ構成の Pipeline アーキテクチャを提供:
//! 1. Preprocess: C ヘッダーファイルのプリプロセス
//! 2. Infer: マクロと inline 関数の型推論
//! 3. Generate: Rust コード生成
//!
//! # 使用例
//!
//! ```ignore
//! use libperl_macrogen::Pipeline;
//!
//! // 一括実行
//! let mut output = File::create("macro_fns.rs")?;
//! Pipeline::builder("wrapper.h")
//!     .with_auto_perl_config()?
//!     .with_codegen_defaults()
//!     .with_bindings("bindings.rs")
//!     .build()?
//!     .generate(&mut output)?;
//!
//! // 段階的実行
//! let preprocessed = Pipeline::builder("wrapper.h")
//!     .with_auto_perl_config()?
//!     .build()?
//!     .preprocess()?;
//!
//! let inferred = preprocessed
//!     .with_bindings("bindings.rs")
//!     .infer()?;
//!
//! let generated = inferred
//!     .with_strict_rustfmt()
//!     .generate(&mut output)?;
//! ```

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;

use crate::perl_config::{get_perl_config, PerlConfigError, get_default_target_dir};
use crate::preprocessor::{PPConfig, Preprocessor};
use crate::rust_codegen::{CodegenConfig as RustCodegenConfig, CodegenDriver, CodegenStats};
use crate::infer_api::{InferResult, InferError};
use crate::error::CompileError;

// ============================================================================
// Error types
// ============================================================================

/// Pipeline 実行時のエラー
#[derive(Debug)]
pub enum PipelineError {
    /// Perl 設定取得エラー
    PerlConfig(PerlConfigError),
    /// プリプロセス/パースエラー
    Compile(CompileError),
    /// 推論エラー
    Infer(InferError),
    /// I/O エラー
    Io(std::io::Error),
}

impl std::fmt::Display for PipelineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PipelineError::PerlConfig(e) => write!(f, "Perl config error: {}", e),
            PipelineError::Compile(e) => write!(f, "Compile error: {}", e),
            PipelineError::Infer(e) => write!(f, "Inference error: {}", e),
            PipelineError::Io(e) => write!(f, "I/O error: {}", e),
        }
    }
}

impl std::error::Error for PipelineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PipelineError::PerlConfig(e) => Some(e),
            PipelineError::Compile(e) => Some(e),
            PipelineError::Infer(e) => Some(e),
            PipelineError::Io(e) => Some(e),
        }
    }
}

impl From<PerlConfigError> for PipelineError {
    fn from(e: PerlConfigError) -> Self {
        PipelineError::PerlConfig(e)
    }
}

impl From<CompileError> for PipelineError {
    fn from(e: CompileError) -> Self {
        PipelineError::Compile(e)
    }
}

impl From<InferError> for PipelineError {
    fn from(e: InferError) -> Self {
        PipelineError::Infer(e)
    }
}

impl From<std::io::Error> for PipelineError {
    fn from(e: std::io::Error) -> Self {
        PipelineError::Io(e)
    }
}

// ============================================================================
// Phase-specific Config structs
// ============================================================================

/// Preprocessor フェーズの設定
#[derive(Debug, Clone)]
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

impl PreprocessConfig {
    /// 入力ファイルのみを指定した最小構成
    pub fn new(input_file: impl Into<PathBuf>) -> Self {
        Self {
            input_file: input_file.into(),
            include_paths: Vec::new(),
            defines: HashMap::new(),
            target_dir: None,
            emit_markers: false,
            wrapped_macros: Vec::new(),
            debug_pp: false,
        }
    }

    /// PPConfig に変換
    pub(crate) fn to_pp_config(&self) -> PPConfig {
        PPConfig {
            include_paths: self.include_paths.clone(),
            predefined: self.defines.iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            debug_pp: self.debug_pp,
            target_dir: self.target_dir.clone(),
            emit_markers: self.emit_markers,
        }
    }
}

/// Inference フェーズの設定
#[derive(Debug, Clone, Default)]
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

impl InferConfig {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Codegen フェーズの設定
#[derive(Debug, Clone)]
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

impl Default for CodegenConfig {
    fn default() -> Self {
        Self {
            rust_edition: "2024".to_string(),
            strict_rustfmt: false,
            macro_comments: false,
            emit_inline_fns: true,
            emit_macros: true,
        }
    }
}

impl CodegenConfig {
    /// RustCodegenConfig に変換
    pub(crate) fn to_rust_codegen_config(&self) -> RustCodegenConfig {
        RustCodegenConfig {
            emit_inline_fns: self.emit_inline_fns,
            emit_macros: self.emit_macros,
            include_source_location: self.macro_comments,
        }
    }
}

// ============================================================================
// PipelineBuilder
// ============================================================================

/// Pipeline を構築するための Builder
#[derive(Debug)]
pub struct PipelineBuilder {
    preprocess: PreprocessConfig,
    infer: InferConfig,
    codegen: CodegenConfig,
}

impl PipelineBuilder {
    /// 入力ファイルを指定して Builder を作成
    pub fn new(input_file: impl Into<PathBuf>) -> Self {
        Self {
            preprocess: PreprocessConfig::new(input_file),
            infer: InferConfig::new(),
            codegen: CodegenConfig::default(),
        }
    }

    // === Preprocess 設定 ===

    /// Perl Config.pm から自動設定（--auto 相当）
    ///
    /// インクルードパス、プリプロセッサ定義、ターゲットディレクトリを
    /// Perl の Config.pm から取得して設定する。
    pub fn with_auto_perl_config(mut self) -> Result<Self, PipelineError> {
        let perl_cfg = get_perl_config()?;
        self.preprocess.include_paths = perl_cfg.include_paths;
        self.preprocess.defines = perl_cfg.defines.into_iter().collect();
        self.preprocess.target_dir = get_default_target_dir().ok();
        Ok(self)
    }

    /// コード生成に推奨される設定を適用
    ///
    /// 以下を設定:
    /// - wrapped_macros: ["assert", "assert_"] （inline関数内のassertを正しく変換）
    pub fn with_codegen_defaults(mut self) -> Self {
        self.preprocess.wrapped_macros = vec![
            "assert".to_string(),
            "assert_".to_string(),
        ];
        self
    }

    /// インクルードパスを追加 (-I)
    pub fn with_include(mut self, path: impl Into<PathBuf>) -> Self {
        self.preprocess.include_paths.push(path.into());
        self
    }

    /// マクロ定義を追加 (-D)
    pub fn with_define(mut self, name: impl Into<String>, value: Option<impl Into<String>>) -> Self {
        self.preprocess.defines.insert(name.into(), value.map(|v| v.into()));
        self
    }

    /// ターゲットディレクトリを設定
    pub fn with_target_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.preprocess.target_dir = Some(path.into());
        self
    }

    /// マクロ展開マーカーを出力
    pub fn with_emit_markers(mut self) -> Self {
        self.preprocess.emit_markers = true;
        self
    }

    /// プリプロセッサデバッグ出力を有効化
    pub fn with_debug_pp(mut self) -> Self {
        self.preprocess.debug_pp = true;
        self
    }

    // === Infer 設定 ===

    /// Rust バインディングファイルを指定
    pub fn with_bindings(mut self, path: impl Into<PathBuf>) -> Self {
        self.infer.bindings_path = Some(path.into());
        self
    }

    /// apidoc ファイルを指定
    pub fn with_apidoc(mut self, path: impl Into<PathBuf>) -> Self {
        self.infer.apidoc_path = Some(path.into());
        self
    }

    /// apidoc ディレクトリを指定
    pub fn with_apidoc_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.infer.apidoc_dir = Some(path.into());
        self
    }

    /// apidoc マージ後にダンプして終了（デバッグ用）
    pub fn with_dump_apidoc(mut self, filter: impl Into<String>) -> Self {
        self.infer.dump_apidoc_after_merge = Some(filter.into());
        self
    }

    // === Codegen 設定 ===

    /// rustfmt 失敗時にエラー終了
    pub fn with_strict_rustfmt(mut self) -> Self {
        self.codegen.strict_rustfmt = true;
        self
    }

    /// Rust edition を指定
    pub fn with_rust_edition(mut self, edition: impl Into<String>) -> Self {
        self.codegen.rust_edition = edition.into();
        self
    }

    /// マクロ定義位置コメントを有効化
    pub fn with_macro_comments(mut self) -> Self {
        self.codegen.macro_comments = true;
        self
    }

    // === Build ===

    /// Pipeline を構築
    pub fn build(self) -> Result<Pipeline, PipelineError> {
        Ok(Pipeline {
            preprocess_config: self.preprocess,
            infer_config: self.infer,
            codegen_config: self.codegen,
        })
    }

    /// PreprocessConfig のみを取り出す（Preprocessor 単独使用時）
    pub fn preprocess_config(self) -> PreprocessConfig {
        self.preprocess
    }

    /// InferConfig を取り出す
    pub fn infer_config(&self) -> &InferConfig {
        &self.infer
    }

    /// CodegenConfig を取り出す
    pub fn codegen_config(&self) -> &CodegenConfig {
        &self.codegen
    }
}

// ============================================================================
// Pipeline (Initial state)
// ============================================================================

/// 初期状態の Pipeline
pub struct Pipeline {
    preprocess_config: PreprocessConfig,
    infer_config: InferConfig,
    codegen_config: CodegenConfig,
}

impl Pipeline {
    /// Builder を作成
    pub fn builder(input_file: impl Into<PathBuf>) -> PipelineBuilder {
        PipelineBuilder::new(input_file)
    }

    /// PreprocessConfig への参照を取得
    pub fn preprocess_config(&self) -> &PreprocessConfig {
        &self.preprocess_config
    }

    /// InferConfig への参照を取得
    pub fn infer_config(&self) -> &InferConfig {
        &self.infer_config
    }

    /// CodegenConfig への参照を取得
    pub fn codegen_config(&self) -> &CodegenConfig {
        &self.codegen_config
    }

    /// Phase 1: プリプロセスのみ実行
    pub fn preprocess(self) -> Result<PreprocessedPipeline, PipelineError> {
        // PPConfig を構築
        let pp_config = self.preprocess_config.to_pp_config();

        // Preprocessor を初期化
        let mut pp = Preprocessor::new(pp_config);

        // wrapped_macros を登録
        for macro_name in &self.preprocess_config.wrapped_macros {
            pp.add_wrapped_macro(macro_name);
        }

        // ファイルを処理
        pp.process_file(&self.preprocess_config.input_file)?;

        Ok(PreprocessedPipeline {
            preprocessor: pp,
            infer_config: self.infer_config,
            codegen_config: self.codegen_config,
        })
    }

    /// Phase 2: 推論まで実行（プリプロセスも含む）
    pub fn infer(self) -> Result<InferredPipeline, PipelineError> {
        self.preprocess()?.infer()
    }

    /// Phase 3: コード生成まで実行（全フェーズ）
    pub fn generate<W: Write>(self, writer: W) -> Result<GeneratedPipeline, PipelineError> {
        self.infer()?.generate(writer)
    }
}

// ============================================================================
// PreprocessedPipeline
// ============================================================================

/// プリプロセス完了状態
pub struct PreprocessedPipeline {
    preprocessor: Preprocessor,
    infer_config: InferConfig,
    codegen_config: CodegenConfig,
}

impl PreprocessedPipeline {
    /// Preprocessor への参照を取得
    pub fn preprocessor(&self) -> &Preprocessor {
        &self.preprocessor
    }

    /// Preprocessor への可変参照を取得
    pub fn preprocessor_mut(&mut self) -> &mut Preprocessor {
        &mut self.preprocessor
    }

    /// Preprocessor を消費して取得
    pub fn into_preprocessor(self) -> Preprocessor {
        self.preprocessor
    }

    /// InferConfig への参照を取得
    pub fn infer_config(&self) -> &InferConfig {
        &self.infer_config
    }

    /// CodegenConfig への参照を取得
    pub fn codegen_config(&self) -> &CodegenConfig {
        &self.codegen_config
    }

    // === Infer 設定を追加で指定可能 ===

    /// Rust バインディングファイルを指定
    pub fn with_bindings(mut self, path: impl Into<PathBuf>) -> Self {
        self.infer_config.bindings_path = Some(path.into());
        self
    }

    /// apidoc ファイルを指定
    pub fn with_apidoc(mut self, path: impl Into<PathBuf>) -> Self {
        self.infer_config.apidoc_path = Some(path.into());
        self
    }

    /// apidoc ディレクトリを指定
    pub fn with_apidoc_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.infer_config.apidoc_dir = Some(path.into());
        self
    }

    /// Phase 2: 推論を実行
    pub fn infer(self) -> Result<InferredPipeline, PipelineError> {
        use crate::apidoc::resolve_apidoc_path;
        use crate::infer_api::{run_inference_with_preprocessor, DebugOptions};

        // apidoc パスを解決
        let apidoc_path = resolve_apidoc_path(
            self.infer_config.apidoc_path.as_deref(),
            true, // auto_mode
            self.infer_config.apidoc_dir.as_deref(),
        ).map_err(|e| PipelineError::Infer(InferError::ApidocResolve(e)))?;

        // デバッグオプションを構築
        let debug_opts = if self.infer_config.dump_apidoc_after_merge.is_some() {
            Some(DebugOptions {
                dump_apidoc_after_merge: self.infer_config.dump_apidoc_after_merge.clone(),
            })
        } else {
            None
        };

        // 推論を実行
        let result = run_inference_with_preprocessor(
            self.preprocessor,
            apidoc_path.as_deref(),
            self.infer_config.bindings_path.as_deref(),
            debug_opts.as_ref(),
        )?;

        match result {
            Some(infer_result) => Ok(InferredPipeline {
                result: infer_result,
                codegen_config: self.codegen_config,
            }),
            None => {
                // デバッグダンプで早期終了
                // 空の結果を返すか、専用のエラーを返すか検討が必要
                // ここでは Io エラーとして扱う（暫定）
                Err(PipelineError::Io(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "Debug dump caused early exit",
                )))
            }
        }
    }

    /// Phase 3: コード生成まで実行（推論も含む）
    pub fn generate<W: Write>(self, writer: W) -> Result<GeneratedPipeline, PipelineError> {
        self.infer()?.generate(writer)
    }
}

// ============================================================================
// InferredPipeline
// ============================================================================

/// 推論完了状態
pub struct InferredPipeline {
    result: InferResult,
    codegen_config: CodegenConfig,
}

impl InferredPipeline {
    /// InferResult への参照を取得
    pub fn result(&self) -> &InferResult {
        &self.result
    }

    /// InferResult を消費して取得
    pub fn into_result(self) -> InferResult {
        self.result
    }

    /// CodegenConfig への参照を取得
    pub fn codegen_config(&self) -> &CodegenConfig {
        &self.codegen_config
    }

    // === Codegen 設定を追加で指定可能 ===

    /// rustfmt 失敗時にエラー終了
    pub fn with_strict_rustfmt(mut self) -> Self {
        self.codegen_config.strict_rustfmt = true;
        self
    }

    /// Rust edition を指定
    pub fn with_rust_edition(mut self, edition: impl Into<String>) -> Self {
        self.codegen_config.rust_edition = edition.into();
        self
    }

    /// マクロ定義位置コメントを有効化
    pub fn with_macro_comments(mut self) -> Self {
        self.codegen_config.macro_comments = true;
        self
    }

    /// Phase 3: コード生成
    pub fn generate<W: Write>(self, mut writer: W) -> Result<GeneratedPipeline, PipelineError> {
        let rust_codegen_config = self.codegen_config.to_rust_codegen_config();

        let mut driver = CodegenDriver::new(
            &mut writer,
            self.result.preprocessor.interner(),
            rust_codegen_config,
        );

        driver.generate(&self.result)?;

        let stats = driver.stats().clone();

        // TODO: strict_rustfmt の処理
        // 現状は CodegenDriver が rustfmt を呼び出さないため、
        // ここで別途 rustfmt を実行する必要がある

        Ok(GeneratedPipeline {
            result: self.result,
            stats,
        })
    }
}

// ============================================================================
// GeneratedPipeline
// ============================================================================

/// コード生成完了状態
pub struct GeneratedPipeline {
    result: InferResult,
    /// コード生成の統計情報
    pub stats: CodegenStats,
}

impl GeneratedPipeline {
    /// 統計情報を取得
    pub fn stats(&self) -> &CodegenStats {
        &self.stats
    }

    /// InferResult への参照を取得
    pub fn result(&self) -> &InferResult {
        &self.result
    }

    /// InferResult を消費して取得
    pub fn into_result(self) -> InferResult {
        self.result
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pipeline_builder_basic() {
        let builder = PipelineBuilder::new("test.h")
            .with_include("/usr/include")
            .with_define("FOO", Some("1"))
            .with_bindings("bindings.rs");

        assert_eq!(builder.preprocess.input_file, PathBuf::from("test.h"));
        assert_eq!(builder.preprocess.include_paths.len(), 1);
        assert_eq!(builder.preprocess.defines.get("FOO"), Some(&Some("1".to_string())));
        assert_eq!(builder.infer.bindings_path, Some(PathBuf::from("bindings.rs")));
    }

    #[test]
    fn test_pipeline_builder_codegen_defaults() {
        let builder = PipelineBuilder::new("test.h")
            .with_codegen_defaults();

        assert_eq!(builder.preprocess.wrapped_macros, vec!["assert", "assert_"]);
    }

    #[test]
    fn test_preprocess_config_to_pp_config() {
        let mut config = PreprocessConfig::new("test.h");
        config.include_paths.push(PathBuf::from("/usr/include"));
        config.defines.insert("FOO".to_string(), Some("1".to_string()));
        config.debug_pp = true;

        let pp_config = config.to_pp_config();
        assert_eq!(pp_config.include_paths.len(), 1);
        assert_eq!(pp_config.predefined.len(), 1);
        assert!(pp_config.debug_pp);
    }

    #[test]
    fn test_codegen_config_default() {
        let config = CodegenConfig::default();
        assert_eq!(config.rust_edition, "2024");
        assert!(!config.strict_rustfmt);
        assert!(config.emit_inline_fns);
        assert!(config.emit_macros);
    }
}
