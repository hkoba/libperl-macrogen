//! Rust関数生成ライブラリ
//!
//! CヘッダーファイルからマクロとインラインプトI関数を解析し、
//! Rustコードを生成する。build.rsからの利用を想定。

use std::collections::HashMap;
use std::io::Write;
use std::ops::ControlFlow;
use std::path::PathBuf;

use crate::{
    ApidocDict, CompileError, DerivedDecl, ExternalDecl, FieldsDict, FunctionSignature,
    InferenceContext, MacroAnalyzer, MacroCategory, MacroKind, PPConfig, Parser,
    PendingFunction, Preprocessor, RustCodeGen, RustDeclDict, extract_called_functions,
    get_perl_config, PerlConfigError,
};

/// デフォルトのターゲットディレクトリ
pub const DEFAULT_TARGET_DIR: &str = "/usr/lib64/perl5/CORE";

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
            target_dir: PathBuf::from(DEFAULT_TARGET_DIR),
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
    pub fn build(self) -> MacrogenConfig {
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
    fields_dict.set_target_dir(&target_dir_str);

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
            fields_dict.collect_from_external_decl(decl, path, interner);

            // inline関数を収集（対象ディレクトリ内のみ）
            if config.include_inline_functions {
                let path_str = path.to_string_lossy();
                let is_target = path_str.starts_with(&target_dir_str);

                if is_target {
                    if let ExternalDecl::FunctionDef(func_def) = decl {
                        if func_def.specs.is_inline {
                            if let Some(name) = func_def.declarator.name {
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

    // 8. Apidocから型情報を読み込む（オプション）
    if let Some(ref apidoc_path) = config.apidoc {
        let apidoc = ApidocDict::load_auto(apidoc_path)
            .map_err(|e| MacrogenError::Apidoc(e.to_string()))?;

        if config.verbose {
            let apidoc_stats = apidoc.stats();
            eprintln!(
                "Loaded apidoc: {} entries ({} functions, {} macros, {} inline)",
                apidoc_stats.total,
                apidoc_stats.function_count,
                apidoc_stats.macro_count,
                apidoc_stats.inline_count
            );
        }

        let added = infer_ctx.load_apidoc(&apidoc);
        stats.apidoc_loaded = added;

        if config.verbose {
            eprintln!(
                "After apidoc: {} confirmed (+{} from apidoc)",
                infer_ctx.confirmed_count(),
                added
            );
        }
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
        }
    }

    // 10. 定数マクロを識別（inline関数とマクロ関数の両方で使用）
    let mut analyzer = MacroAnalyzer::new(interner, files, &fields_dict);
    analyzer.set_target_dir(&target_dir_str);
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

            if info.category != MacroCategory::Expression {
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

            if info.category != MacroCategory::Expression {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
