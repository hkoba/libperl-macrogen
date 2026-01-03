//! Rust関数生成ライブラリ
//!
//! CヘッダーファイルからマクロとインラインプトI関数を解析し、
//! Rustコードを生成する。build.rsからの利用を想定。

use std::collections::HashMap;
use std::io::Write;
use std::io;
use std::ops::ControlFlow;
use std::path::PathBuf;

use crate::{
    ApidocDict, CompileError, DerivedDecl, ExternalDecl, FieldsDict, FunctionSignature,
    InferenceContext, MacroAnalyzer2, MacroCategory2, MacroInfo2, MacroKind, MacroTable, PPConfig, Parser,
    PendingFunction, Preprocessor, RustCodeGen, RustDeclDict, StringInterner, TokenKind,
    extract_called_functions, get_default_target_dir, get_perl_config, PerlConfigError,
    TypedSexpPrinter,
};
use std::collections::HashSet;

/// Rust関数生成の設定
#[derive(Debug, Clone)]
pub struct MacrogenConfig {
    /// 入力Cファイル (wrapper.h等)
    pub input: PathBuf,

    /// Rustバインディングファイル (bindings.rs)
    pub bindings: PathBuf,

    /// Apidocファイル (embed.fnc または JSON)
    /// 省略時は自動検出を試みない
    pub apidoc: Option<PathBuf>,

    /// プリプロセッサ設定
    pub pp_config: PPConfig,

    /// ターゲットディレクトリ（単一）
    /// デフォルト: /usr/lib64/perl5/CORE
    pub target_dir: PathBuf,

    /// カスタムフィールド型マッピング (フィールド名 -> 構造体名)
    /// 例: ("sv_flags", "sv") で sv_flags -> sv型と推論
    pub field_type_overrides: Vec<(String, String)>,

    /// フィールドRust型オーバーライド (構造体名, フィールド名, Rust型)
    /// 自動導出できない場合や特殊なマッピングが必要な場合に使用
    /// 例: ("XPVCV", "xcv_xsub", "XSUBADDR_t")
    pub field_rust_type_overrides: Vec<(String, String, String)>,

    /// inline関数を出力に含めるか
    pub include_inline_functions: bool,

    /// マクロ関数を出力に含めるか
    pub include_macro_functions: bool,

    /// 冗長な進捗表示 (stderr)
    pub verbose: bool,

    /// 処理進行状況を表示 (stderr)
    pub progress: bool,
}

impl Default for MacrogenConfig {
    fn default() -> Self {
        Self {
            input: PathBuf::new(),
            bindings: PathBuf::new(),
            apidoc: None,
            pp_config: PPConfig::default(),
            target_dir: PathBuf::new(), // with_perl_auto_config() で設定される
            field_type_overrides: vec![],
            field_rust_type_overrides: vec![],
            include_inline_functions: true,
            include_macro_functions: true,
            verbose: false,
            progress: false,
        }
    }
}

/// MacrogenConfigのビルダー
pub struct MacrogenBuilder {
    config: MacrogenConfig,
}

impl MacrogenBuilder {
    /// 必須パラメータでビルダーを作成
    pub fn new(input: impl Into<PathBuf>, bindings: impl Into<PathBuf>) -> Self {
        Self {
            config: MacrogenConfig {
                input: input.into(),
                bindings: bindings.into(),
                ..Default::default()
            },
        }
    }

    /// Perl Config.pmから自動設定（--auto相当）
    pub fn with_perl_auto_config(mut self) -> Result<Self, PerlConfigError> {
        let perl_cfg = get_perl_config()?;
        self.config.pp_config.include_paths = perl_cfg.include_paths;
        self.config.pp_config.predefined = perl_cfg.defines;
        self.config.target_dir = get_default_target_dir()?;
        Ok(self)
    }

    /// 既存のPPConfigを使用
    pub fn pp_config(mut self, config: PPConfig) -> Self {
        self.config.pp_config = config;
        self
    }

    /// Apidocファイルを設定
    pub fn apidoc(mut self, path: impl Into<PathBuf>) -> Self {
        self.config.apidoc = Some(path.into());
        self
    }

    /// ターゲットディレクトリを設定
    pub fn target_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.config.target_dir = path.into();
        self
    }

    /// カスタムフィールド→構造体マッピングを追加
    pub fn add_field_type(mut self, field: impl Into<String>, struct_name: impl Into<String>) -> Self {
        self.config.field_type_overrides.push((field.into(), struct_name.into()));
        self
    }

    /// フィールドRust型をオーバーライド
    /// 自動導出できない場合や特殊なマッピングが必要な場合に使用
    pub fn add_field_type_override(
        mut self,
        struct_name: impl Into<String>,
        field_name: impl Into<String>,
        rust_type: impl Into<String>,
    ) -> Self {
        self.config.field_rust_type_overrides.push((
            struct_name.into(),
            field_name.into(),
            rust_type.into(),
        ));
        self
    }

    /// インクルードパスを追加
    pub fn add_include_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.config.pp_config.include_paths.push(path.into());
        self
    }

    /// マクロ定義を追加
    pub fn define(mut self, name: impl Into<String>, value: Option<String>) -> Self {
        self.config.pp_config.predefined.push((name.into(), value));
        self
    }

    /// inline関数を含めるかどうか
    pub fn include_inline_functions(mut self, include: bool) -> Self {
        self.config.include_inline_functions = include;
        self
    }

    /// マクロ関数を含めるかどうか
    pub fn include_macro_functions(mut self, include: bool) -> Self {
        self.config.include_macro_functions = include;
        self
    }

    /// 冗長出力
    pub fn verbose(mut self, verbose: bool) -> Self {
        self.config.verbose = verbose;
        self
    }

    /// 進行状況表示
    pub fn progress(mut self, progress: bool) -> Self {
        self.config.progress = progress;
        self
    }

    /// デバッグ出力を有効化
    pub fn debug_pp(mut self, debug: bool) -> Self {
        self.config.pp_config.debug_pp = debug;
        self
    }

    /// 設定を構築
    pub fn build(mut self) -> MacrogenConfig {
        // pp_config.target_dir を config.target_dir から同期
        self.config.pp_config.target_dir = Some(self.config.target_dir.clone());
        self.config
    }
}

/// 生成結果
pub struct MacrogenResult {
    /// 生成されたRustコード
    pub code: String,

    /// 統計情報
    pub stats: MacrogenStats,
}

/// 統計情報
#[derive(Debug, Clone, Default)]
pub struct MacrogenStats {
    /// 成功したinline関数数
    pub inline_success: usize,
    /// 失敗したinline関数数
    pub inline_failure: usize,
    /// 成功したマクロ関数数
    pub macro_success: usize,
    /// 失敗したマクロ関数数
    pub macro_failure: usize,
    /// bindings.rsから読み込んだ関数数
    pub bindings_loaded: usize,
    /// apidocから読み込んだ関数数
    pub apidoc_loaded: usize,
    /// 型推論で解決された関数数
    pub inference_resolved: usize,
    /// 型推論の反復回数
    pub inference_iterations: usize,

    // === apidoc 型一致評価 ===
    /// apidocにエントリがあるマクロ数
    pub apidoc_comparable: usize,
    /// 戻り値型が一致したマクロ数
    pub return_type_match: usize,
    /// 戻り値型が不一致のマクロ数（const/mut違いのみ）
    pub return_type_const_mut_only: usize,
    /// 戻り値型が不一致のマクロ数
    pub return_type_mismatch: usize,
    /// パラメータ型が全て一致したマクロ数
    pub params_all_match: usize,
    /// パラメータ型に不一致があるマクロ数
    pub params_has_mismatch: usize,
    /// 一致したパラメータ型の総数
    pub param_type_match: usize,
    /// パラメータ型の不一致（const/mut違いのみ）
    pub param_type_const_mut_only: usize,
    /// 不一致のパラメータ型の総数
    pub param_type_mismatch: usize,
}

/// 型一致の結果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeMatchResult {
    /// 完全一致
    Match,
    /// const/mut の違いのみ
    ConstMutOnly,
    /// 不一致
    Mismatch,
}

impl MacrogenStats {
    /// apidoc との戻り値型一致率を計算（const/mutのみの不一致も一致として扱う）
    pub fn return_type_match_rate(&self) -> f64 {
        let total = self.return_type_match + self.return_type_const_mut_only + self.return_type_mismatch;
        if total == 0 {
            0.0
        } else {
            (self.return_type_match + self.return_type_const_mut_only) as f64 / total as f64 * 100.0
        }
    }

    /// apidoc とのパラメータ型一致率を計算（const/mutのみの不一致も一致として扱う）
    pub fn param_type_match_rate(&self) -> f64 {
        let total = self.param_type_match + self.param_type_const_mut_only + self.param_type_mismatch;
        if total == 0 {
            0.0
        } else {
            (self.param_type_match + self.param_type_const_mut_only) as f64 / total as f64 * 100.0
        }
    }

    /// 統計情報のサマリを生成
    pub fn apidoc_summary(&self) -> String {
        format!(
            "=== Apidoc Type Comparison ===\n\
             Comparable macros (with apidoc entry): {}\n\
             Return type: {} match, {} const/mut only, {} mismatch ({:.1}%)\n\
             All params match: {} macros, has mismatch: {} macros\n\
             Parameter types: {} match, {} const/mut only, {} mismatch ({:.1}%)\n",
            self.apidoc_comparable,
            self.return_type_match,
            self.return_type_const_mut_only,
            self.return_type_mismatch,
            self.return_type_match_rate(),
            self.params_all_match,
            self.params_has_mismatch,
            self.param_type_match,
            self.param_type_const_mut_only,
            self.param_type_mismatch,
            self.param_type_match_rate(),
        )
    }
}

/// エラー型
#[derive(Debug)]
pub enum MacrogenError {
    /// I/Oエラー
    Io(std::io::Error),
    /// プリプロセスエラー
    Preprocess(String),
    /// パースエラー
    Parse(String),
    /// バインディング解析エラー
    Bindings(String),
    /// Apidoc解析エラー
    Apidoc(String),
}

impl std::fmt::Display for MacrogenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MacrogenError::Io(e) => write!(f, "I/O error: {}", e),
            MacrogenError::Preprocess(e) => write!(f, "Preprocess error: {}", e),
            MacrogenError::Parse(e) => write!(f, "Parse error: {}", e),
            MacrogenError::Bindings(e) => write!(f, "Bindings error: {}", e),
            MacrogenError::Apidoc(e) => write!(f, "Apidoc error: {}", e),
        }
    }
}

impl std::error::Error for MacrogenError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            MacrogenError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for MacrogenError {
    fn from(e: std::io::Error) -> Self {
        MacrogenError::Io(e)
    }
}

/// Rust関数を生成
pub fn generate(config: &MacrogenConfig) -> Result<MacrogenResult, MacrogenError> {
    let mut stats = MacrogenStats::default();
    let mut output = Vec::new();

    // 1. Rustバインディングをパースして型情報を取得
    let rust_decls = RustDeclDict::parse_file(&config.bindings)
        .map_err(|e| MacrogenError::Bindings(e.to_string()))?;

    stats.bindings_loaded = rust_decls.fns.len();

    // THX依存関数を取得
    let thx_functions = rust_decls.thx_functions();

    if config.verbose {
        eprintln!(
            "Loaded {} functions, {} structs, {} types from bindings",
            rust_decls.fns.len(),
            rust_decls.structs.len(),
            rust_decls.types.len()
        );
        eprintln!("THX-dependent functions: {}", thx_functions.len());
    }

    // 2. フィールド辞書を作成
    let mut fields_dict = FieldsDict::new();
    let target_dir_str = config.target_dir.to_string_lossy().to_string();

    // 3. プリプロセッサを初期化（第1パス：マクロ分析用）
    if config.progress {
        eprintln!("[progress] Starting 1st pass preprocessing...");
    }

    let mut pp = Preprocessor::new(config.pp_config.clone());
    pp.process_file(&config.input)
        .map_err(|e| MacrogenError::Preprocess(format_error(&e, &pp)))?;

    if config.progress {
        eprintln!("[progress] 1st pass preprocessing done. Starting parsing...");
    }

    // 4. パースしてフィールド辞書を構築 + inline関数を収集
    let parser_result = Parser::new(&mut pp);
    let mut parser = match parser_result {
        Ok(p) => p,
        Err(e) => return Err(MacrogenError::Parse(format_error(&e, &pp))),
    };

    let mut inline_functions: Vec<(String, ExternalDecl, String)> = Vec::new();

    parser.parse_each(|result, _loc, path, interner| {
        if let Ok(ref decl) = result {
            // フィールド情報を収集
            fields_dict.collect_from_external_decl(decl, decl.is_target(), interner);

            // inline関数を収集（対象ディレクトリ内のみ）
            if config.include_inline_functions && decl.is_target() {
                if let ExternalDecl::FunctionDef(func_def) = decl {
                    if func_def.specs.is_inline {
                        if let Some(name) = func_def.declarator.name {
                            let path_str = path.to_string_lossy();
                            inline_functions.push((
                                interner.get(name).to_string(),
                                decl.clone(),
                                path_str.to_string(),
                            ));
                        }
                    }
                }
            }
        }
        ControlFlow::Continue(())
    });

    let typedefs = parser.typedefs().clone();

    if config.progress {
        eprintln!("[progress] Parsing done.");
    }

    if config.verbose && config.include_inline_functions {
        eprintln!("Inline functions collected: {}", inline_functions.len());
    }

    // 5. カスタムフィールド型を登録
    {
        let interner = pp.interner_mut();
        for (field, struct_name) in &config.field_type_overrides {
            let field_id = interner.intern(field);
            let struct_id = interner.intern(struct_name);
            fields_dict.set_unique_field_type(field_id, struct_id);
        }

        // 5.1 フィールドRust型オーバーライドを登録
        for (struct_name, field_name, rust_type) in &config.field_rust_type_overrides {
            let struct_id = interner.intern(struct_name);
            let field_id = interner.intern(field_name);
            fields_dict.set_field_type_override(struct_id, field_id, rust_type.clone());
        }
    }

    // 6. RustCodeGen を作成
    let interner = pp.interner();
    let mut codegen = RustCodeGen::new(interner, &fields_dict);

    // 6.5. bindings.rs の定数名を収集
    let bindings_consts: std::collections::HashSet<String> = rust_decls.consts.keys().cloned().collect();

    // 7. 反復型推論コンテキストを作成
    let mut infer_ctx = InferenceContext::new(interner, &fields_dict);
    infer_ctx.load_bindings(&rust_decls);

    if config.verbose {
        eprintln!("After bindings.rs: {} confirmed", infer_ctx.confirmed_count());
    }

    // 8. Apidoc辞書を作成（MacroAnalyzer2で使用）
    let mut apidoc = ApidocDict::new();

    // 8.1 ファイルからApidocを読み込む（オプション）
    if let Some(ref apidoc_path) = config.apidoc {
        let file_apidoc = ApidocDict::load_auto(apidoc_path)
            .map_err(|e| MacrogenError::Apidoc(e.to_string()))?;

        if config.verbose {
            let apidoc_stats = file_apidoc.stats();
            eprintln!(
                "Loaded apidoc: {} entries ({} functions, {} macros, {} inline)",
                apidoc_stats.total,
                apidoc_stats.function_count,
                apidoc_stats.macro_count,
                apidoc_stats.inline_count
            );
        }

        let added = infer_ctx.load_apidoc(&file_apidoc);
        stats.apidoc_loaded = added;

        if config.verbose {
            eprintln!(
                "After apidoc: {} confirmed (+{} from apidoc)",
                infer_ctx.confirmed_count(),
                added
            );
        }

        apidoc.merge(file_apidoc);
    }

    // 9. 読み込まれたターゲットファイルを収集
    let files = pp.files();

    // FileRegistryからターゲットディレクトリ内のファイルを抽出
    let included_target_files: Vec<_> = files
        .iter()
        .filter(|(_, path)| path.to_string_lossy().starts_with(&target_dir_str))
        .map(|(id, path)| (id, path.to_path_buf()))
        .collect();

    if config.verbose {
        eprintln!("Included target files: {} files", included_target_files.len());
        for (_, path) in &included_target_files {
            eprintln!("  {}", path.display());
        }
    }

    // 9.5. ターゲットヘッダから =for apidoc コメントを抽出
    {
        let mut header_apidoc = ApidocDict::new();
        for (_, path) in &included_target_files {
            if let Ok(dict) = ApidocDict::parse_header_apidoc(path) {
                header_apidoc.merge(dict);
            }
        }

        if !header_apidoc.is_empty() {
            let header_stats = header_apidoc.stats();
            if config.verbose {
                eprintln!(
                    "Extracted apidoc from headers: {} entries ({} functions, {} macros, {} inline)",
                    header_stats.total,
                    header_stats.function_count,
                    header_stats.macro_count,
                    header_stats.inline_count
                );
            }

            let added = infer_ctx.load_apidoc(&header_apidoc);
            stats.apidoc_loaded += added;

            if config.verbose {
                eprintln!(
                    "After header apidoc: {} confirmed (+{} from headers)",
                    infer_ctx.confirmed_count(),
                    added
                );
            }

            apidoc.merge(header_apidoc);
        }
    }

    // 10. MacroAnalyzer2 を作成（SemanticAnalyzer ベースの新しい解析器）
    let mut analyzer = MacroAnalyzer2::new(interner, files, &apidoc, &fields_dict, &target_dir_str);
    analyzer.set_typedefs(typedefs.clone());
    analyzer.set_bindings_consts(bindings_consts.clone());
    analyzer.set_thx_functions(thx_functions.clone());
    analyzer.identify_constant_macros(pp.macros());
    analyzer.analyze(pp.macros());

    // THX依存マクロを識別
    analyzer.identify_thx_dependent_macros(pp.macros());

    // 定数マクロ情報をコード生成器に設定
    codegen.set_constant_macros(analyzer.constant_macros().clone());

    // THX依存情報をコード生成器に設定
    codegen.set_thx_macros(analyzer.thx_macros().clone());
    codegen.set_thx_functions(thx_functions.clone());

    if config.verbose {
        eprintln!("Constant macros identified: {}", analyzer.constant_macros().len());
        eprintln!("THX-dependent macros identified: {}", analyzer.thx_macros().len());
    }

    // 10. inline関数を確定済みとして追加
    inline_functions.sort_by(|a, b| a.0.cmp(&b.0));

    let before_inline = infer_ctx.confirmed_count();
    for (_name, decl, _path) in &inline_functions {
        if let ExternalDecl::FunctionDef(func_def) = decl {
            if let Some(sig) = extract_inline_fn_signature(func_def, interner, &codegen) {
                infer_ctx.add_confirmed(sig);
            }
        }
    }

    if config.verbose {
        let after_inline = infer_ctx.confirmed_count();
        eprintln!(
            "After inline functions: {} confirmed (+{} inline)",
            after_inline,
            after_inline - before_inline
        );
    }

    // 使用された定数マクロを収集
    let mut all_used_constants: std::collections::HashSet<crate::InternedStr> = std::collections::HashSet::new();

    // 関数出力を一時バッファに格納（const定義を先に出力するため）
    let mut functions_output: Vec<u8> = Vec::new();

    // 11. inline関数を出力
    if config.include_inline_functions {
        writeln!(functions_output, "// ==================== Inline Functions ====================")?;
        writeln!(functions_output)?;

        for (name, decl, _path) in &inline_functions {
            if let ExternalDecl::FunctionDef(func_def) = decl {
                let frag = codegen.inline_fn_to_rust(func_def);
                all_used_constants.extend(frag.used_constants.iter().cloned());
                if frag.has_issues() {
                    writeln!(functions_output, "// FAILED: {} - {}", name, frag.issues_summary())?;
                    for line in frag.code.lines() {
                        writeln!(functions_output, "// {}", line)?;
                    }
                    stats.inline_failure += 1;
                } else {
                    writeln!(functions_output, "{}", frag.code)?;
                    stats.inline_success += 1;
                }
            }
        }

        if config.verbose {
            eprintln!(
                "Inline functions: {} success, {} failures",
                stats.inline_success, stats.inline_failure
            );
        }
    }

    // 12. マクロ関数を処理
    if config.include_macro_functions {

        // マクロ関数をpendingとして追加
        let macros = pp.macros();

        // マクロから型エイリアスを抽出（例: #define Size_t size_t）
        let macro_type_aliases = extract_type_aliases_from_macros(macros, interner);
        if config.verbose {
            eprintln!("Extracted {} type aliases from macros", macro_type_aliases.len());
        }

        let mut macro_exprs: HashMap<String, crate::Expr> = HashMap::new();
        let mut macro_parse_failures: Vec<(String, String)> = Vec::new();

        for (name, info) in analyzer.iter() {
            if !info.is_target {
                continue;
            }

            let name_str = interner.get(*name).to_string();

            let def = match macros.get(*name) {
                Some(d) => d,
                None => {
                    macro_parse_failures.push((name_str, "macro definition not found".to_string()));
                    continue;
                }
            };

            if matches!(def.kind, MacroKind::Object) {
                continue;
            }

            if info.category != MacroCategory2::Expression {
                macro_parse_failures.push((
                    name_str,
                    format!("not an expression macro (category: {:?})", info.category),
                ));
                continue;
            }

            let (expanded, parse_result) = analyzer.parse_macro_body(def, macros);
            let expr = match parse_result {
                Ok(e) => e,
                Err(e) => {
                    let expanded_str = expanded
                        .iter()
                        .map(|t| t.kind.format(interner))
                        .collect::<Vec<_>>()
                        .join(" ");
                    macro_parse_failures.push((
                        name_str,
                        format!("parse error: {} | expanded: {}", e, expanded_str),
                    ));
                    continue;
                }
            };

            // println!("parsed expr: {:?}", expr);
            {
                eprint!("macro {}: ", name_str);
                let stderr = io::stderr();
                let mut handle = stderr.lock();
                let mut printer = TypedSexpPrinter::new(&mut handle, pp.interner(), None, None);
                let _ = printer.print_expr(& expr);
                handle.flush()?;
                eprint!("\n");
            }

            if let MacroKind::Function { ref params, .. } = def.kind {
                let called_fns = extract_called_functions(&expr, interner);
                let known_types = info.param_types.clone();

                let ret_ty = if let Some(rust_fn) = rust_decls.fns.get(&name_str) {
                    rust_fn.ret_ty.clone()
                } else {
                    info.return_type.clone()
                };

                let pending = PendingFunction {
                    name: name_str.clone(),
                    param_names: params.clone(),
                    known_types,
                    called_functions: called_fns,
                    ret_ty,
                    body_expr: Some(expr.clone()),
                    body_stmt: None,
                };

                infer_ctx.add_pending(pending);
                macro_exprs.insert(name_str, expr);
            }
        }

        if config.verbose {
            eprintln!("Pending functions: {}", infer_ctx.pending_count());
        }

        // 13. 反復推論を実行
        let (resolved, iterations) = infer_ctx.run_inference();
        stats.inference_resolved = resolved;
        stats.inference_iterations = iterations;

        if config.verbose {
            eprintln!(
                "Iterative inference: {} resolved in {} iterations",
                resolved, iterations
            );
            eprintln!(
                "Final: {} confirmed, {} still pending",
                infer_ctx.confirmed_count(),
                infer_ctx.pending_count()
            );
        }

        let (confirmed_fns, _still_pending) = infer_ctx.into_results();

        // 14. マクロ関数を出力
        writeln!(functions_output)?;
        writeln!(functions_output, "// ==================== Macro Functions ====================")?;
        writeln!(
            functions_output,
            "// Type inference: {} iterations, {} functions resolved",
            iterations, resolved
        )?;
        writeln!(functions_output)?;

        // マクロ名を収集してソート
        let mut macro_names: Vec<_> = analyzer
            .iter()
            .filter(|(_, info)| info.is_target)
            .map(|(name, _)| *name)
            .collect();
        macro_names.sort_by_key(|name| interner.get(*name));

        // パース失敗をHashMapに変換（順序保持用）
        let parse_failure_map: HashMap<String, String> =
            macro_parse_failures.into_iter().collect();

        // マクロを順次出力（成功・失敗をインライン）
        for name in macro_names {
            let name_str = interner.get(name).to_string();

            // パース段階で失敗した場合
            if let Some(reason) = parse_failure_map.get(&name_str) {
                writeln!(functions_output, "// FAILED: {} - {}", name_str, reason)?;
                stats.macro_failure += 1;
                continue;
            }

            let info = match analyzer.get_info(name) {
                Some(i) => i,
                None => continue,
            };

            let def = match macros.get(name) {
                Some(d) => d,
                None => continue,
            };

            if matches!(def.kind, MacroKind::Object) {
                continue;
            }

            if info.category != MacroCategory2::Expression {
                continue;
            }

            let expr = match macro_exprs.get(&name_str) {
                Some(e) => e,
                None => continue,
            };

            // 推論結果から型情報を取得
            let mut info_with_inferred = info.clone();

            if let Some(sig) = confirmed_fns.get(&name_str) {
                if let MacroKind::Function { ref params, .. } = def.kind {
                    for (i, param_id) in params.iter().enumerate() {
                        if i < sig.params.len() {
                            let (_, ty) = &sig.params[i];
                            if ty != "UnknownType"
                                && !info_with_inferred.param_types.contains_key(param_id)
                            {
                                info_with_inferred.param_types.insert(*param_id, ty.clone());
                            }
                        }
                    }
                }
                if info_with_inferred.return_type.is_none() {
                    info_with_inferred.return_type = sig.ret_ty.clone();
                }
            }

            if let Some(rust_fn) = rust_decls.fns.get(&name_str) {
                if let Some(ref ret_ty) = rust_fn.ret_ty {
                    info_with_inferred.return_type = Some(ret_ty.clone());
                }
            }

            // apidoc の型情報を適用（bindings.rs に存在する型のみ）
            if let MacroKind::Function { ref params, .. } = def.kind {
                apply_apidoc_types_to_params(
                    &mut info_with_inferred,
                    &name_str,
                    params,
                    &apidoc,
                    &rust_decls,
                    &macro_type_aliases,
                );
            }

            // apidoc との型比較
            if let MacroKind::Function { ref params, .. } = def.kind {
                if let Some(result) = compare_macro_signature(
                    &name_str,
                    &info_with_inferred,
                    params,
                    &apidoc,
                    interner,
                    &rust_decls,
                    &macro_type_aliases,
                ) {
                    stats.apidoc_comparable += 1;
                    match result.return_type_result {
                        TypeMatchResult::Match => stats.return_type_match += 1,
                        TypeMatchResult::ConstMutOnly => stats.return_type_const_mut_only += 1,
                        TypeMatchResult::Mismatch => stats.return_type_mismatch += 1,
                    }
                    stats.param_type_match += result.param_match;
                    stats.param_type_const_mut_only += result.param_const_mut_only;
                    stats.param_type_mismatch += result.param_mismatch;
                    if result.param_mismatch == 0 && result.param_const_mut_only == 0 && result.param_match > 0 {
                        stats.params_all_match += 1;
                    } else if result.param_mismatch > 0 || result.param_const_mut_only > 0 {
                        stats.params_has_mismatch += 1;
                    }
                }
            }

            let frag = codegen.macro_to_rust_fn(def, &info_with_inferred, expr);
            all_used_constants.extend(frag.used_constants.iter().cloned());
            if frag.has_issues() {
                writeln!(functions_output, "// FAILED: {} - {}", name_str, frag.issues_summary())?;
                for line in frag.code.lines() {
                    writeln!(functions_output, "// {}", line)?;
                }
                stats.macro_failure += 1;
            } else {
                writeln!(functions_output, "{}", frag.code)?;
                stats.macro_success += 1;
            }
        }

        if config.verbose {
            eprintln!(
                "Macro functions: {} success, {} failures",
                stats.macro_success, stats.macro_failure
            );
            eprintln!(
                "Total: {} success, {} failures",
                stats.macro_success + stats.inline_success,
                stats.macro_failure + stats.inline_failure
            );
            // apidoc 型比較サマリ
            eprint!("{}", stats.apidoc_summary());
        }
    }

    // 15. ヘッダーを出力
    writeln!(output, "use std::ffi::{{c_char, c_int, c_long, c_longlong, c_short, c_uchar, c_uint, c_ulong, c_ulonglong, c_ushort, c_void}};")?;
    writeln!(output, "use std::os::raw::{{c_double, c_float}};")?;
    writeln!(output, "#[allow(unused_imports)]")?;
    writeln!(output, "use crate::bindings::*;")?;
    writeln!(output)?;

    // 16. 定数定義を生成（bindings.rsにないもののみ）
    let macros = pp.macros();
    let mut const_names: Vec<_> = all_used_constants
        .iter()
        .filter(|id| {
            let name_str = interner.get(**id);
            !bindings_consts.contains(name_str)
        })
        .cloned()
        .collect();
    const_names.sort_by_key(|id| interner.get(*id));

    if !const_names.is_empty() {
        writeln!(output, "// ==================== Macro Constants ====================")?;

        for const_id in &const_names {
            let const_name = interner.get(*const_id);
            if let Some(def) = macros.get(*const_id) {
                // 定数マクロの本体を式としてパース
                let (expanded, parse_result) = analyzer.parse_macro_body(def, macros);
                match parse_result {
                    Ok(ref expr) => {
                        // 式をRustコードに変換
                        let frag = codegen.expr_to_rust(expr);
                        if frag.has_issues() {
                            writeln!(output, "// const {}: u32 = /* {} */;", const_name, frag.issues_summary())?;
                        } else {
                            writeln!(output, "pub const {}: u32 = {};", const_name, frag.code)?;
                        }
                    }
                    Err(e) => {
                        let expanded_str = expanded
                            .iter()
                            .map(|t| t.kind.format(interner))
                            .collect::<Vec<_>>()
                            .join(" ");
                        writeln!(output, "// const {}: u32 = /* parse error: {} | {} */;", const_name, e, expanded_str)?;
                    }
                }
            } else {
                writeln!(output, "// const {}: u32 = /* definition not found */;", const_name)?;
            }
        }
        writeln!(output)?;

        if config.verbose {
            eprintln!("Generated {} const definitions", const_names.len());
        }
    }

    // 17. 関数出力を追加
    output.extend(functions_output);

    let code = String::from_utf8(output).map_err(|e| MacrogenError::Io(
        std::io::Error::new(std::io::ErrorKind::InvalidData, e)
    ))?;

    Ok(MacrogenResult { code, stats })
}

/// inline関数からFunctionSignatureを抽出
fn extract_inline_fn_signature(
    func_def: &crate::FunctionDef,
    interner: &crate::StringInterner,
    codegen: &RustCodeGen,
) -> Option<FunctionSignature> {
    let name = func_def.declarator.name?;
    let name_str = interner.get(name).to_string();

    let mut params = Vec::new();
    for derived in &func_def.declarator.derived {
        if let DerivedDecl::Function(param_list) = derived {
            for param in &param_list.params {
                if let Some(ref decl) = param.declarator {
                    if let Some(param_name) = decl.name {
                        let param_name_str = interner.get(param_name).to_string();
                        let ty = codegen.param_decl_to_rust_type(param);
                        params.push((param_name_str, ty));
                    }
                }
            }
            break;
        }
    }

    let ret_ty = codegen.extract_fn_return_type(&func_def.specs, &func_def.declarator.derived);

    Some(FunctionSignature {
        name: name_str,
        params,
        ret_ty,
    })
}

/// エラーをファイル名付きでフォーマット
fn format_error(e: &CompileError, pp: &Preprocessor) -> String {
    e.format_with_files(pp.files())
}

// ==================== Apidoc 型比較 ====================

/// C型をRust型に正規化して比較可能にする
fn normalize_c_type_for_comparison(c_type: &str) -> String {
    crate::iterative_infer::c_type_to_rust(c_type)
}

/// Rust型を正規化（空白除去、ポインタ表記統一）
fn normalize_rust_type(rust_type: &str) -> String {
    let mut s = rust_type.trim().to_string();
    // 空白を除去
    s = s.replace(" ", "");
    // 先頭のコロン（::std::...）を除去
    s = s.trim_start_matches(':').to_string();
    s = s.replace("::", "::");
    // std::ffi:: プレフィックスを除去
    s = s.replace("std::ffi::", "");
    // std::os::raw:: プレフィックスを除去
    s = s.replace("std::os::raw::", "");
    s
}

/// 型エイリアスを解決する
/// bindings.rs の型エイリアス（例: STRLEN = usize）を使って展開する
fn resolve_type_alias(
    ty: &str,
    rust_decls: &RustDeclDict,
    macro_type_aliases: &HashMap<String, String>,
) -> String {
    let normalized = normalize_rust_type(ty);

    // ポインタ型の場合は中身を解決
    if normalized.starts_with("*mut") {
        let inner = normalized.strip_prefix("*mut").unwrap().trim();
        let resolved = resolve_base_type_for_comparison(inner, rust_decls, macro_type_aliases);
        return format!("*mut{}", resolved);
    }
    if normalized.starts_with("*const") {
        let inner = normalized.strip_prefix("*const").unwrap().trim();
        let resolved = resolve_base_type_for_comparison(inner, rust_decls, macro_type_aliases);
        return format!("*const{}", resolved);
    }

    resolve_base_type_for_comparison(&normalized, rust_decls, macro_type_aliases)
}

/// 基本型のエイリアスを解決する（比較用）
fn resolve_base_type_for_comparison(
    ty: &str,
    rust_decls: &RustDeclDict,
    macro_type_aliases: &HashMap<String, String>,
) -> String {
    // まず bindings.rs のエイリアスをチェック
    if let Some(alias) = rust_decls.types.get(ty) {
        // エイリアスを再帰的に解決
        return normalize_rust_type(&alias.ty);
    }

    // マクロ定義の型エイリアスを経由して解決
    // 例: Size_t -> size_t -> usize
    if let Some(base_type) = macro_type_aliases.get(ty) {
        return resolve_base_type_for_comparison(base_type, rust_decls, macro_type_aliases);
    }

    // 標準的な型変換（void → c_void など）
    // c_type_to_rust との整合性を保つ
    match ty {
        "void" | "()" => "c_void".to_string(),
        "size_t" => "usize".to_string(),
        "ssize_t" => "isize".to_string(),
        "off_t" | "off64_t" => "i64".to_string(),
        _ => ty.to_string(),
    }
}

/// const/mut を除いたポインタ型を取得（比較用）
fn strip_const_mut(ty: &str) -> String {
    ty.replace("*mut", "*").replace("*const", "*")
}

/// apidoc の C 型を bindings.rs に存在する Rust 型に変換
///
/// 例:
/// - "UV" -> Some("UV") (bindings.rs に `pub type UV = ...` があれば)
/// - "SV *" -> Some("*mut SV") (bindings.rs に SV 構造体があれば)
/// - "Size_t" -> Some("usize") (マクロで `#define Size_t size_t` があれば)
/// - "const char *" -> Some("*const c_char")
/// - "unknown_type" -> None
fn resolve_apidoc_type_to_rust(
    c_type: &str,
    rust_decls: &RustDeclDict,
    macro_type_aliases: &HashMap<String, String>,
) -> Option<String> {
    let trimmed = c_type.trim();

    // ポインタ型かチェック
    if trimmed.ends_with('*') {
        // "SV *" or "const SV *" のパターン
        let without_star = trimmed.trim_end_matches('*').trim();
        let (is_const, base_type) = if without_star.starts_with("const ") {
            (true, without_star.strip_prefix("const ").unwrap().trim())
        } else {
            (false, without_star)
        };

        // 基本型を解決
        if let Some(rust_base) = resolve_base_type_from_bindings(base_type, rust_decls, macro_type_aliases) {
            let ptr_kind = if is_const { "*const" } else { "*mut" };
            return Some(format!("{} {}", ptr_kind, rust_base));
        }
        return None;
    }

    // 非ポインタ型
    resolve_base_type_from_bindings(trimmed, rust_decls, macro_type_aliases)
}

/// マクロテーブルから型エイリアスを抽出
///
/// `#define Size_t size_t` のようなマクロを型エイリアスとして解釈する
/// 戻り値: 型名 → 基底型名 のマッピング
fn extract_type_aliases_from_macros(
    macros: &MacroTable,
    interner: &StringInterner,
) -> HashMap<String, String> {
    let mut aliases = HashMap::new();

    for (name_id, def) in macros.iter() {
        // オブジェクトマクロのみ対象
        if def.is_function() {
            continue;
        }

        // 本体が単一の識別子トークンの場合、型エイリアスと見なす
        if def.body.len() == 1 {
            if let TokenKind::Ident(base_id) = def.body[0].kind {
                let name = interner.get(*name_id);
                let base = interner.get(base_id);

                // 型名らしいもののみ（大文字で始まる or _t で終わる）
                if name.chars().next().map(|c| c.is_uppercase()).unwrap_or(false)
                    || name.ends_with("_t")
                {
                    aliases.insert(name.to_string(), base.to_string());
                }
            }
        }
    }

    aliases
}

/// apidoc の型情報をマクロのパラメータ型に適用
///
/// apidoc で指定された型が bindings.rs またはマクロ定義の型エイリアスに存在する場合、
/// その型を優先して使用する
fn apply_apidoc_types_to_params(
    info: &mut crate::MacroInfo2,
    macro_name: &str,
    params: &[crate::InternedStr],
    apidoc: &ApidocDict,
    rust_decls: &RustDeclDict,
    macro_type_aliases: &HashMap<String, String>,
) {
    let entry = match apidoc.get(macro_name) {
        Some(e) => e,
        None => return,
    };

    for (i, param_id) in params.iter().enumerate() {
        if let Some(apidoc_arg) = entry.args.get(i) {
            // apidoc の型を Rust 型に解決
            if let Some(rust_ty) = resolve_apidoc_type_to_rust(&apidoc_arg.ty, rust_decls, macro_type_aliases) {
                // apidoc の型で上書き（bindings.rs に存在する型のみ）
                info.param_types.insert(*param_id, rust_ty);
            }
        }
    }

    // 注: 戻り値型は apidoc から適用しない
    // apidoc は「期待される型」を示すが、実際のマクロ本体から推論された型を優先する
    // 例: isDIGIT_A(c) が () を返すなら、isDIGIT も () を返すべき
}

/// 基本型名を bindings.rs から解決
///
/// - 型エイリアス (pub type UV = ...) があればその名前を返す
/// - 構造体 (pub struct SV { ... }) があればその名前を返す
/// - マクロ定義の型エイリアス (#define Size_t size_t) を経由して解決
/// - char, int などの C 基本型は対応する Rust 型に変換
fn resolve_base_type_from_bindings(
    c_type: &str,
    rust_decls: &RustDeclDict,
    macro_type_aliases: &HashMap<String, String>,
) -> Option<String> {
    // bindings.rs の型エイリアスに存在するか
    if rust_decls.types.contains_key(c_type) {
        return Some(c_type.to_string());
    }

    // bindings.rs の構造体に存在するか
    if rust_decls.structs.contains_key(c_type) {
        return Some(c_type.to_string());
    }

    // マクロ定義の型エイリアスを経由して解決
    // 例: Size_t -> size_t -> usize
    if let Some(base_type) = macro_type_aliases.get(c_type) {
        // 再帰的に解決（Size_t -> size_t -> usize）
        if let Some(resolved) = resolve_base_type_from_bindings(base_type, rust_decls, macro_type_aliases) {
            return Some(resolved);
        }
    }

    // C の基本型を Rust 型に変換
    match c_type {
        "char" => Some("c_char".to_string()),
        "unsigned char" => Some("c_uchar".to_string()),
        "int" => Some("c_int".to_string()),
        "unsigned int" | "unsigned" => Some("c_uint".to_string()),
        "short" => Some("c_short".to_string()),
        "unsigned short" => Some("c_ushort".to_string()),
        "long" => Some("c_long".to_string()),
        "unsigned long" => Some("c_ulong".to_string()),
        "long long" => Some("c_longlong".to_string()),
        "unsigned long long" => Some("c_ulonglong".to_string()),
        "void" => Some("c_void".to_string()),
        "float" => Some("c_float".to_string()),
        "double" => Some("c_double".to_string()),
        "bool" | "_Bool" => Some("bool".to_string()),
        "size_t" => Some("usize".to_string()),
        "ssize_t" => Some("isize".to_string()),
        "off_t" | "off64_t" => Some("i64".to_string()),
        _ => None,
    }
}

/// 2つの型を比較して結果を返す
/// - c_type: apidoc の C型 (例: "SV *", "const char *")
/// - rust_type: 推論された Rust型 (例: "*mut SV", "*const c_char")
/// - rust_decls: bindings.rs から読み込んだ型エイリアス
/// - macro_type_aliases: マクロ定義から抽出した型エイリアス
fn types_match_with_aliases(
    c_type: &str,
    rust_type: &str,
    rust_decls: &RustDeclDict,
    macro_type_aliases: &HashMap<String, String>,
) -> TypeMatchResult {
    let normalized_c = normalize_rust_type(&normalize_c_type_for_comparison(c_type));
    let normalized_rust = normalize_rust_type(rust_type);

    // 型エイリアスを解決
    let resolved_c = resolve_type_alias(&normalized_c, rust_decls, macro_type_aliases);
    let resolved_rust = resolve_type_alias(&normalized_rust, rust_decls, macro_type_aliases);

    // 完全一致
    if resolved_c == resolved_rust {
        return TypeMatchResult::Match;
    }

    // const/mut の違いのみかチェック
    let stripped_c = strip_const_mut(&resolved_c);
    let stripped_rust = strip_const_mut(&resolved_rust);

    if stripped_c == stripped_rust {
        TypeMatchResult::ConstMutOnly
    } else {
        TypeMatchResult::Mismatch
    }
}

/// マクロの型シグネチャ比較結果
pub struct SignatureCompareResult {
    pub return_type_result: TypeMatchResult,
    pub param_match: usize,
    pub param_const_mut_only: usize,
    pub param_mismatch: usize,
}

/// マクロの型シグネチャを apidoc と比較
fn compare_macro_signature(
    macro_name: &str,
    info: &crate::MacroInfo2,
    param_names: &[crate::InternedStr],
    apidoc: &ApidocDict,
    interner: &crate::StringInterner,
    rust_decls: &RustDeclDict,
    macro_type_aliases: &HashMap<String, String>,
) -> Option<SignatureCompareResult> {
    let entry = apidoc.get(macro_name)?;

    // 戻り値型の比較
    let return_type_result = match (&entry.return_type, &info.return_type) {
        (Some(c_type), Some(rust_type)) => types_match_with_aliases(c_type, rust_type, rust_decls, macro_type_aliases),
        (None, None) => TypeMatchResult::Match,  // 両方なし = void
        (None, Some(rust_type)) => {
            if rust_type == "()" || rust_type == "void" {
                TypeMatchResult::Match
            } else {
                TypeMatchResult::Mismatch
            }
        }
        (Some(c_type), None) => {
            if c_type.trim() == "void" {
                TypeMatchResult::Match
            } else {
                TypeMatchResult::Mismatch
            }
        }
    };

    // パラメータ型の比較
    let mut param_match = 0;
    let mut param_const_mut_only = 0;
    let mut param_mismatch = 0;

    for (i, param_id) in param_names.iter().enumerate() {
        if let Some(apidoc_arg) = entry.args.get(i) {
            let param_name = interner.get(*param_id);
            if let Some(inferred_type) = info.param_types.get(param_id) {
                match types_match_with_aliases(&apidoc_arg.ty, inferred_type, rust_decls, macro_type_aliases) {
                    TypeMatchResult::Match => param_match += 1,
                    TypeMatchResult::ConstMutOnly => {
                        param_const_mut_only += 1;
                        eprintln!(
                            "  [const/mut] {}.{}: apidoc='{}' inferred='{}'",
                            macro_name, param_name, apidoc_arg.ty, inferred_type
                        );
                    }
                    TypeMatchResult::Mismatch => {
                        param_mismatch += 1;
                        eprintln!(
                            "  [mismatch] {}.{}: apidoc='{}' inferred='{}'",
                            macro_name, param_name, apidoc_arg.ty, inferred_type
                        );
                    }
                }
            } else {
                // 型が推論されていない場合は不一致
                param_mismatch += 1;
            }
        }
    }

    Some(SignatureCompareResult {
        return_type_result,
        param_match,
        param_const_mut_only,
        param_mismatch,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// テスト用ヘルパー: 型エイリアスなしで型を比較
    fn types_match(c_type: &str, rust_type: &str) -> bool {
        let empty_decls = RustDeclDict::new();
        let empty_macro_aliases = HashMap::new();
        matches!(
            types_match_with_aliases(c_type, rust_type, &empty_decls, &empty_macro_aliases),
            TypeMatchResult::Match
        )
    }

    /// テスト用ヘルパー: 型エイリアス付きで型を比較
    fn types_match_with_decls(c_type: &str, rust_type: &str, decls: &RustDeclDict) -> TypeMatchResult {
        let empty_macro_aliases = HashMap::new();
        types_match_with_aliases(c_type, rust_type, decls, &empty_macro_aliases)
    }

    #[test]
    fn test_builder_basic() {
        let config = MacrogenBuilder::new("input.c", "bindings.rs")
            .add_field_type("sv_any", "sv")
            .verbose(true)
            .build();

        assert_eq!(config.input, PathBuf::from("input.c"));
        assert_eq!(config.bindings, PathBuf::from("bindings.rs"));
        assert_eq!(config.field_type_overrides.len(), 1);
        assert!(config.verbose);
    }

    #[test]
    fn test_builder_with_options() {
        let config = MacrogenBuilder::new("wrapper.h", "out/bindings.rs")
            .apidoc("embed.json")
            .target_dir("/custom/target/dir")
            .add_include_path("/usr/include")
            .define("FOO", Some("1".to_string()))
            .define("BAR", None)
            .include_inline_functions(true)
            .include_macro_functions(false)
            .build();

        assert_eq!(config.apidoc, Some(PathBuf::from("embed.json")));
        assert_eq!(config.target_dir, PathBuf::from("/custom/target/dir"));
        assert_eq!(config.pp_config.include_paths.len(), 1);
        assert_eq!(config.pp_config.predefined.len(), 2);
        assert!(config.include_inline_functions);
        assert!(!config.include_macro_functions);
    }

    #[test]
    fn test_types_match_pointer() {
        // C "SV *" should match Rust "*mut SV"
        assert!(types_match("SV *", "*mut SV"));
        // C "const SV *" should match Rust "*const SV"
        assert!(types_match("const SV *", "*const SV"));
        // C "char *" should match Rust "*mut c_char"
        assert!(types_match("char *", "*mut c_char"));
        // C "const char *" should match Rust "*const c_char"
        assert!(types_match("const char *", "*const c_char"));
    }

    #[test]
    fn test_types_match_basic() {
        // C "int" should match Rust "c_int"
        assert!(types_match("int", "c_int"));
        // C "void" should match Rust "()"
        assert!(types_match("void", "()"));
        // C "unsigned int" should match Rust "c_uint"
        assert!(types_match("unsigned int", "c_uint"));
    }

    #[test]
    fn test_types_match_perl_types() {
        // Perl-specific types are preserved as-is in c_type_to_rust
        // (they're typedef names that exist in bindings.rs)
        assert!(types_match("SSize_t", "SSize_t"));
        assert!(types_match("Size_t", "Size_t"));
        assert!(types_match("STRLEN", "STRLEN"));
        assert!(types_match("IV", "IV"));
        assert!(types_match("UV", "UV"));
        assert!(types_match("SV *", "*mut SV"));
        assert!(types_match("AV *", "*mut AV"));
        assert!(types_match("HV *", "*mut HV"));
    }

    #[test]
    fn test_types_mismatch() {
        // Different types should not match
        assert!(!types_match("int", "c_long"));
        // mut vs const は ConstMutOnly として扱われる
        let empty_decls = RustDeclDict::new();
        let empty_macro_aliases = HashMap::new();
        assert_eq!(
            types_match_with_aliases("SV *", "*const SV", &empty_decls, &empty_macro_aliases),
            TypeMatchResult::ConstMutOnly
        );
        assert!(!types_match("char *", "*mut SV"));  // different base type
    }

    #[test]
    fn test_types_match_with_alias() {
        // bindings.rs のような型エイリアス付きでの比較
        let decls = RustDeclDict::parse("pub type STRLEN = usize;");

        // STRLEN と usize が一致するはず
        assert_eq!(
            types_match_with_decls("STRLEN", "usize", &decls),
            TypeMatchResult::Match
        );

        // 逆方向も一致
        assert_eq!(
            types_match_with_decls("usize", "STRLEN", &decls),
            TypeMatchResult::Match
        );
    }

    #[test]
    fn test_const_mut_only() {
        let empty_decls = RustDeclDict::new();
        let empty_macro_aliases = HashMap::new();

        // *mut vs *const は ConstMutOnly
        assert_eq!(
            types_match_with_aliases("SV *", "*const SV", &empty_decls, &empty_macro_aliases),
            TypeMatchResult::ConstMutOnly
        );
        assert_eq!(
            types_match_with_aliases("const SV *", "*mut SV", &empty_decls, &empty_macro_aliases),
            TypeMatchResult::ConstMutOnly
        );

        // 完全に異なる型は Mismatch
        assert_eq!(
            types_match_with_aliases("SV *", "*mut AV", &empty_decls, &empty_macro_aliases),
            TypeMatchResult::Mismatch
        );
    }

    #[test]
    fn test_stats_summary() {
        let mut stats = MacrogenStats::default();
        stats.apidoc_comparable = 10;
        stats.return_type_match = 8;
        stats.return_type_const_mut_only = 0;
        stats.return_type_mismatch = 2;
        stats.params_all_match = 6;
        stats.params_has_mismatch = 4;
        stats.param_type_match = 15;
        stats.param_type_const_mut_only = 0;
        stats.param_type_mismatch = 5;

        assert!((stats.return_type_match_rate() - 80.0).abs() < 0.01);
        assert!((stats.param_type_match_rate() - 75.0).abs() < 0.01);

        let summary = stats.apidoc_summary();
        assert!(summary.contains("80.0%"));
        assert!(summary.contains("75.0%"));
    }

    #[test]
    fn test_stats_with_const_mut() {
        let mut stats = MacrogenStats::default();
        stats.apidoc_comparable = 10;
        stats.return_type_match = 6;
        stats.return_type_const_mut_only = 2;
        stats.return_type_mismatch = 2;
        stats.param_type_match = 10;
        stats.param_type_const_mut_only = 5;
        stats.param_type_mismatch = 5;

        // const/mut only は match として扱われる
        assert!((stats.return_type_match_rate() - 80.0).abs() < 0.01);  // (6+2)/10 = 80%
        assert!((stats.param_type_match_rate() - 75.0).abs() < 0.01);  // (10+5)/20 = 75%
    }

    #[test]
    fn test_types_match_with_macro_aliases() {
        // マクロ定義の型エイリアスを使った比較
        // #define Size_t size_t のようなマクロを模擬
        let empty_decls = RustDeclDict::new();
        let mut macro_aliases = HashMap::new();
        macro_aliases.insert("Size_t".to_string(), "size_t".to_string());
        macro_aliases.insert("SSize_t".to_string(), "ssize_t".to_string());
        macro_aliases.insert("Off_t".to_string(), "off64_t".to_string());

        // Size_t と usize が一致するはず（Size_t -> size_t -> usize）
        assert_eq!(
            types_match_with_aliases("Size_t", "usize", &empty_decls, &macro_aliases),
            TypeMatchResult::Match
        );

        // SSize_t と isize が一致するはず（SSize_t -> ssize_t -> isize）
        assert_eq!(
            types_match_with_aliases("SSize_t", "isize", &empty_decls, &macro_aliases),
            TypeMatchResult::Match
        );

        // Off_t と i64 が一致するはず（Off_t -> off64_t -> i64）
        assert_eq!(
            types_match_with_aliases("Off_t", "i64", &empty_decls, &macro_aliases),
            TypeMatchResult::Match
        );
    }
}
