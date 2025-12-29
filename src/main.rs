//! TinyCC Macro Bindgen CLI
//!
//! CファイルをパースしてS-expression形式で出力する

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;

use std::ops::ControlFlow;

use clap::Parser as ClapParser;
use tinycc_macro_bindgen::{
    extract_called_functions, get_perl_config, CompileError, ExternalDecl, FieldsDict, FileId,
    FunctionSignature, InferenceContext, MacroAnalyzer, MacroCategory, PPConfig, Parser,
    PendingFunction, Preprocessor, RustCodeGen, RustDeclDict, SexpPrinter, SourceLocation,
    TokenKind, TypedSexpPrinter,
};

/// コマンドライン引数
#[derive(ClapParser)]
#[command(name = "tinycc-macro-bindgen")]
#[command(version, about = "C to Rust macro bindgen tool")]
struct Cli {
    /// 入力Cファイル（--parse-rust-bindings使用時は不要）
    input: Option<PathBuf>,

    /// インクルードパス (-I)
    #[arg(short = 'I', long = "include")]
    include: Vec<PathBuf>,

    /// マクロ定義 (-D)
    #[arg(short = 'D', long = "define")]
    define: Vec<String>,

    /// 出力ファイル（省略時は標準出力）
    #[arg(short = 'o', long = "output")]
    output: Option<PathBuf>,

    /// プリプロセッサ出力のみ (cc -E 相当)
    #[arg(short = 'E')]
    preprocess_only: bool,

    /// プリプロセッサデバッグ出力
    #[arg(long = "debug-pp")]
    debug_pp: bool,

    /// Perl Config.pm から設定を自動取得
    #[arg(long = "auto")]
    auto: bool,

    /// GCC互換の出力形式 (-E と併用)
    #[arg(long = "gcc-format")]
    gcc_format: bool,

    /// ストリーミングモード（逐次パース、エラー時にソースコード表示）
    #[arg(long = "streaming")]
    streaming: bool,

    /// 型注釈付きS-expression出力
    #[arg(long = "typed-sexp")]
    typed_sexp: bool,

    /// 構造体フィールド辞書をダンプ
    #[arg(long = "dump-fields-dict")]
    dump_fields_dict: bool,

    /// フィールド辞書収集対象ディレクトリ（複数指定可）
    #[arg(long = "fields-dir")]
    fields_dir: Vec<PathBuf>,

    /// Rustバインディングファイルから宣言を抽出
    #[arg(long = "parse-rust-bindings")]
    parse_rust_bindings: Option<PathBuf>,

    /// マクロ関数を解析（Def-Use chain、カテゴリ分類、型推論）
    #[arg(long = "analyze-macros")]
    analyze_macros: bool,

    /// マクロとinline関数からRust関数を生成
    #[arg(long = "gen-rust-fns")]
    gen_rust_fns: bool,

    /// Rustバインディングファイル（--gen-rust-fns用）
    #[arg(long = "bindings")]
    bindings: Option<PathBuf>,

    /// デバッグ: マクロ変換を即座に出力
    #[arg(long = "debug-macro-gen")]
    debug_macro_gen: bool,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    // --parse-rust-bindings: Rustファイルのみ処理（プリプロセッサ不要）
    if let Some(ref rust_file) = cli.parse_rust_bindings {
        return run_parse_rust_bindings(rust_file);
    }

    // 入力ファイルが必要
    let input = cli.input.ok_or("Input file is required")?;

    // プリプロセッサ設定
    let config = if cli.auto {
        // --auto: Perl Config.pm から設定を取得
        if !cli.include.is_empty() {
            return Err("--auto cannot be used with -I options".into());
        }
        let perl_cfg = get_perl_config()?;
        // 追加の -D オプションがあればマージ
        let mut defines = perl_cfg.defines;
        defines.extend(parse_defines(&cli.define));
        PPConfig {
            include_paths: perl_cfg.include_paths,
            predefined: defines,
            debug_pp: cli.debug_pp,
        }
    } else {
        // 従来通り CLI 引数から
        PPConfig {
            include_paths: cli.include,
            predefined: parse_defines(&cli.define),
            debug_pp: cli.debug_pp,
        }
    };

    // プリプロセッサを初期化してファイルを処理
    let mut pp = Preprocessor::new(config);
    if let Err(e) = pp.process_file(&input) {
        return Err(format_error(&e, &pp).into());
    }

    if cli.preprocess_only {
        // -E: プリプロセス結果のみ出力
        output_preprocessed(&mut pp, cli.output.as_ref(), cli.gcc_format)?;
    } else if cli.streaming {
        // --streaming: ストリーミングモード
        run_streaming(&mut pp)?;
    } else if cli.typed_sexp {
        // --typed-sexp: 型注釈付きS-expression出力
        run_typed_sexp(&mut pp, cli.output.as_ref())?;
    } else if cli.dump_fields_dict {
        // --dump-fields-dict: 構造体フィールド辞書をダンプ
        run_dump_fields_dict(&mut pp, &cli.fields_dir)?;
    } else if cli.analyze_macros {
        // --analyze-macros: マクロ関数を解析
        run_analyze_macros(&mut pp, &cli.fields_dir)?;
    } else if cli.gen_rust_fns {
        // --gen-rust-fns: マクロとinline関数からRust関数を生成
        let bindings = cli.bindings.ok_or("--bindings is required with --gen-rust-fns")?;
        run_gen_rust_fns(&mut pp, &bindings, cli.output.as_ref())?;
    } else if cli.debug_macro_gen {
        // --debug-macro-gen: デバッグ用即時出力モード
        run_debug_macro_gen(&mut pp)?;
    } else {
        // 通常: パースしてS-expression出力
        let mut parser = match Parser::new(&mut pp) {
            Ok(p) => p,
            Err(e) => return Err(format_error(&e, &pp).into()),
        };
        let tu = match parser.parse() {
            Ok(tu) => tu,
            Err(e) => return Err(format_error(&e, &pp).into()),
        };

        // 出力
        if let Some(output_path) = cli.output {
            let file = File::create(&output_path)?;
            let mut writer = BufWriter::new(file);
            let mut printer = SexpPrinter::new(&mut writer, pp.interner());
            printer.print_translation_unit(&tu)?;
            writer.flush()?;
        } else {
            let stdout = io::stdout();
            let mut handle = stdout.lock();
            let mut printer = SexpPrinter::new(&mut handle, pp.interner());
            printer.print_translation_unit(&tu)?;
            handle.flush()?;
        }
    }

    Ok(())
}

/// ストリーミングモードで実行
fn run_streaming(pp: &mut Preprocessor) -> Result<(), Box<dyn std::error::Error>> {
    let mut parser = match Parser::new(pp) {
        Ok(p) => p,
        Err(e) => return Err(format_error(&e, pp).into()),
    };

    // ストリーミング出力用
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    let mut count = 0usize;
    let mut last_error: Option<(CompileError, SourceLocation)> = None;

    // parse_each でパースし、即座に出力
    parser.parse_each(|result, loc, _path, interner| {
        match result {
            Ok(decl) => {
                let mut printer = SexpPrinter::new(&mut handle, interner);
                if let Err(e) = printer.print_external_decl(&decl) {
                    eprintln!("Output error: {}", e);
                    return ControlFlow::Break(());
                }
                if let Err(e) = printer.writeln() {
                    eprintln!("Output error: {}", e);
                    return ControlFlow::Break(());
                }
                count += 1;
                ControlFlow::Continue(())
            }
            Err(e) => {
                last_error = Some((e, loc.clone()));
                ControlFlow::Break(())
            }
        }
    });

    drop(handle);

    // エラーがあった場合、詳細を表示
    if let Some((error, _decl_start_loc)) = last_error {
        // エラー内の実際の位置を使う
        let error_loc = error.loc();

        eprintln!("\n=== Parse Error ===");
        eprintln!("Location: {}:{}:{}",
            pp.files().get_path(error_loc.file_id).display(),
            error_loc.line,
            error_loc.column);
        eprintln!("Error: {}", format_error(&error, pp));

        // ソースコードのコンテキストを表示
        show_source_context(pp, error_loc);

        return Err("Parse failed".into());
    }

    eprintln!("\nSuccessfully parsed {} declarations", count);
    Ok(())
}

/// 構造体フィールド辞書をダンプ
fn run_dump_fields_dict(pp: &mut Preprocessor, fields_dirs: &[PathBuf]) -> Result<(), Box<dyn std::error::Error>> {
    // フィールド辞書を作成
    let mut fields_dict = FieldsDict::new();

    // 収集対象ディレクトリを設定
    for dir in fields_dirs {
        fields_dict.add_target_dir(&dir.to_string_lossy());
    }

    // デフォルトで /usr/lib64/perl5/CORE を対象に
    if fields_dirs.is_empty() {
        fields_dict.add_target_dir("/usr/lib64/perl5/CORE");
    }

    let mut parser = match Parser::new(pp) {
        Ok(p) => p,
        Err(e) => return Err(format_error(&e, pp).into()),
    };

    // パースしながらフィールド情報を収集
    parser.parse_each(|result, _loc, path, _interner| {
        if let Ok(ref decl) = result {
            fields_dict.collect_from_external_decl(decl, path);
        }
        std::ops::ControlFlow::Continue(())
    });

    // 統計情報を表示
    let stats = fields_dict.stats();
    eprintln!("=== Fields Dictionary Stats ===");
    eprintln!("Total fields: {}", stats.total_fields);
    eprintln!("Unique fields (can infer struct): {}", stats.unique_fields);
    eprintln!("Ambiguous fields: {}", stats.ambiguous_fields);
    eprintln!();

    // 一意なフィールドをダンプ
    let interner = parser.interner();
    println!("{}", fields_dict.dump_unique(interner));

    Ok(())
}

/// マクロ関数を解析
fn run_analyze_macros(pp: &mut Preprocessor, fields_dirs: &[PathBuf]) -> Result<(), Box<dyn std::error::Error>> {
    // フィールド辞書を作成（パースしながら収集）
    let mut fields_dict = FieldsDict::new();

    // 収集対象ディレクトリを設定
    for dir in fields_dirs {
        fields_dict.add_target_dir(&dir.to_string_lossy());
    }
    if fields_dirs.is_empty() {
        fields_dict.add_target_dir("/usr/lib64/perl5/CORE");
    }

    // パースしてフィールド辞書を構築
    let mut parser = match Parser::new(pp) {
        Ok(p) => p,
        Err(e) => return Err(format_error(&e, pp).into()),
    };

    parser.parse_each(|result, _loc, path, _interner| {
        if let Ok(ref decl) = result {
            fields_dict.collect_from_external_decl(decl, path);
        }
        std::ops::ControlFlow::Continue(())
    });

    // パース中に収集したtypedef名を取得（マクロパース時のキャスト式判定用）
    let typedefs = parser.typedefs().clone();

    // sv_any, sv_refcnt, sv_flags を一意にsvとして登録
    // set_unique_field_type を使って既存の登録を上書き
    {
        let interner = pp.interner_mut();
        let sv = interner.intern("sv");
        let sv_any = interner.intern("sv_any");
        let sv_refcnt = interner.intern("sv_refcnt");
        let sv_flags = interner.intern("sv_flags");

        fields_dict.set_unique_field_type(sv_any, sv);
        fields_dict.set_unique_field_type(sv_refcnt, sv);
        fields_dict.set_unique_field_type(sv_flags, sv);
    }

    // マクロ解析
    let interner = pp.interner();
    let files = pp.files();
    let mut analyzer = MacroAnalyzer::new(interner, files, &fields_dict);
    analyzer.set_typedefs(typedefs);
    analyzer.analyze(pp.macros());

    // 統計情報を出力
    eprintln!("{}", analyzer.dump_stats());

    // Def-Use chain を出力
    println!("{}", analyzer.dump_def_use());

    Ok(())
}

/// マクロとinline関数からRust関数を生成（反復型推論版）
fn run_gen_rust_fns(
    pp: &mut Preprocessor,
    bindings_path: &PathBuf,
    output: Option<&PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::collections::HashMap;
    use tinycc_macro_bindgen::MacroKind;

    // 1. Rustバインディングをパースして型情報を取得
    let rust_decls = RustDeclDict::parse_file(bindings_path)?;
    eprintln!("Loaded {} functions, {} structs, {} types from bindings",
        rust_decls.fns.len(), rust_decls.structs.len(), rust_decls.types.len());

    // 2. フィールド辞書を作成
    let mut fields_dict = FieldsDict::new();
    fields_dict.add_target_dir("/usr/lib64/perl5/CORE");

    // 3. パースしてフィールド辞書を構築 & inline関数を収集
    let mut parser = match Parser::new(pp) {
        Ok(p) => p,
        Err(e) => return Err(format_error(&e, pp).into()),
    };

    let mut inline_functions: Vec<(String, ExternalDecl, String)> = Vec::new(); // (name, decl, path)

    parser.parse_each(|result, _loc, path, interner| {
        if let Ok(ref decl) = result {
            // フィールド辞書を収集
            fields_dict.collect_from_external_decl(decl, path);

            // inline関数を収集（対象ディレクトリ内のみ）
            let path_str = path.to_string_lossy();
            if path_str.contains("/usr/lib64/perl5/CORE") {
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
        std::ops::ControlFlow::Continue(())
    });

    // パース中に収集したtypedef名を取得（マクロパース時のキャスト式判定用）
    let typedefs = parser.typedefs().clone();

    // sv_any, sv_refcnt, sv_flags を一意にsvとして登録
    {
        let interner = pp.interner_mut();
        let sv = interner.intern("sv");
        let sv_any = interner.intern("sv_any");
        let sv_refcnt = interner.intern("sv_refcnt");
        let sv_flags = interner.intern("sv_flags");
        fields_dict.set_unique_field_type(sv_any, sv);
        fields_dict.set_unique_field_type(sv_refcnt, sv);
        fields_dict.set_unique_field_type(sv_flags, sv);
    }

    // 4. RustCodeGen を作成
    let interner = pp.interner();
    let codegen = RustCodeGen::new(interner, &fields_dict);

    // 5. 反復型推論コンテキストを作成
    let mut infer_ctx = InferenceContext::new(interner);

    // bindings.rsから確定済み関数を読み込む
    infer_ctx.load_bindings(&rust_decls);
    let initial_confirmed = infer_ctx.confirmed_count();
    eprintln!("Initial confirmed functions: {}", initial_confirmed);

    // 6. inline関数を確定済みとして追加（型が既知のため）
    // inline関数を名前順にソート
    let mut inline_functions = inline_functions;
    inline_functions.sort_by(|a, b| a.0.cmp(&b.0));

    for (_name, decl, _path) in &inline_functions {
        if let ExternalDecl::FunctionDef(func_def) = decl {
            // inline関数からFunctionSignatureを作成
            if let Some(sig) = extract_inline_fn_signature(func_def, interner, &codegen) {
                infer_ctx.add_confirmed(sig);
            }
        }
    }

    let after_inline = infer_ctx.confirmed_count();
    eprintln!("After inline functions: {} confirmed (+{} inline)",
        after_inline, after_inline - initial_confirmed);

    // 7. 出力先を準備
    let mut out: Box<dyn Write> = if let Some(path) = output {
        Box::new(BufWriter::new(File::create(path)?))
    } else {
        Box::new(io::stdout().lock())
    };

    // ヘッダーコメント（型推論の統計は後で追加）
    writeln!(out, "// Auto-generated Rust functions from C macros and inline functions")?;
    writeln!(out, "// Source: samples/wrapper.h with types from samples/bindings.rs")?;
    writeln!(out)?;
    writeln!(out, "#![allow(non_snake_case)]")?;
    writeln!(out, "#![allow(unused)]")?;
    writeln!(out)?;
    writeln!(out, "use std::ffi::{{c_char, c_int, c_uint, c_long, c_ulong, c_void}};")?;
    writeln!(out)?;

    // 統計用カウンタ
    let mut inline_success = 0usize;
    let mut inline_failure = 0usize;

    // ==================== inline関数の出力（マクロより先） ====================
    writeln!(out, "// ==================== Inline Functions ====================")?;
    writeln!(out)?;

    // inline関数を順次出力
    for (name, decl, _path) in &inline_functions {
        if let ExternalDecl::FunctionDef(func_def) = decl {
            match codegen.inline_fn_to_rust(func_def) {
                Ok(rust_code) => {
                    writeln!(out, "{}", rust_code)?;
                    inline_success += 1;
                }
                Err(e) => {
                    writeln!(out, "// FAILED: {} - {}", name, e)?;
                    inline_failure += 1;
                }
            }
        }
    }

    eprintln!("Inline functions: {} success, {} failures", inline_success, inline_failure);

    // ==================== マクロ関数の処理 ====================

    // 8. マクロ解析
    let files = pp.files();
    let mut analyzer = MacroAnalyzer::new(interner, files, &fields_dict);
    analyzer.set_typedefs(typedefs);
    analyzer.analyze(pp.macros());

    // 9. マクロ関数をpendingとして追加
    let macros = pp.macros();
    let mut macro_exprs: HashMap<String, tinycc_macro_bindgen::Expr> = HashMap::new();
    let mut macro_failures: Vec<(String, String)> = Vec::new();

    for (name, info) in analyzer.iter() {
        if !info.is_target {
            continue;
        }

        let name_str = interner.get(*name).to_string();

        let def = match macros.get(*name) {
            Some(d) => d,
            None => {
                macro_failures.push((name_str, "macro definition not found".to_string()));
                continue;
            }
        };

        // オブジェクトマクロ（引数なし）はスキップ
        if matches!(def.kind, MacroKind::Object) {
            continue;
        }

        // Expressionマクロのみ処理
        if info.category != MacroCategory::Expression {
            macro_failures.push((name_str, format!("not an expression macro (category: {:?})", info.category)));
            continue;
        }

        // マクロ本体をパース
        let (expanded, parse_result) = analyzer.parse_macro_body(def, macros);
        let expr = match parse_result {
            Ok(e) => e,
            Err(e) => {
                let expanded_str = expanded.iter()
                    .map(|t| t.kind.format(interner))
                    .collect::<Vec<_>>()
                    .join(" ");
                macro_failures.push((name_str, format!("parse error: {} | expanded: {}", e, expanded_str)));
                continue;
            }
        };

        // パラメータを取得
        if let MacroKind::Function { ref params, .. } = def.kind {
            // 呼び出す関数を抽出
            let called_fns = extract_called_functions(&expr, interner);

            // 既知の型情報を取得
            let known_types = info.param_types.clone();

            // bindingsから戻り値型を取得（あれば優先）
            let ret_ty = if let Some(rust_fn) = rust_decls.fns.get(&name_str) {
                rust_fn.ret_ty.clone()
            } else {
                info.return_type.clone()
            };

            // PendingFunctionを作成
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

    eprintln!("Pending functions: {}", infer_ctx.pending_count());

    // 10. 反復推論を実行
    let (resolved, iterations) = infer_ctx.run_inference();
    eprintln!("Iterative inference: {} resolved in {} iterations", resolved, iterations);
    eprintln!("Final: {} confirmed, {} still pending",
        infer_ctx.confirmed_count(), infer_ctx.pending_count());

    // 推論結果を取得
    let (confirmed_fns, _still_pending) = infer_ctx.into_results();

    // ==================== マクロ関数の出力 ====================
    writeln!(out)?;
    writeln!(out, "// ==================== Macro Functions ====================")?;
    writeln!(out, "// Type inference: {} iterations, {} functions resolved", iterations, resolved)?;
    writeln!(out)?;

    let mut macro_success = 0usize;
    let mut macro_failure = 0usize;

    // マクロ名を収集してソート
    let mut macro_names: Vec<_> = analyzer.iter()
        .filter(|(_, info)| info.is_target)
        .map(|(name, _)| *name)
        .collect();
    macro_names.sort_by_key(|name| interner.get(*name));

    // マクロを順次出力
    for name in macro_names {
        let info = match analyzer.get_info(name) {
            Some(i) => i,
            None => continue,
        };

        let name_str = interner.get(name).to_string();

        // マクロ定義を取得
        let def = match macros.get(name) {
            Some(d) => d,
            None => continue, // 既にfailuresに記録済み
        };

        // オブジェクトマクロ（引数なし）はスキップ
        if matches!(def.kind, MacroKind::Object) {
            continue;
        }

        // Expressionマクロのみ処理
        if info.category != MacroCategory::Expression {
            continue; // 既にfailuresに記録済み
        }

        // パース済みの式を取得
        let expr = match macro_exprs.get(&name_str) {
            Some(e) => e,
            None => continue, // 既にfailuresに記録済み
        };

        // 推論結果から型情報を取得
        let mut info_with_inferred = info.clone();

        // 確定関数から型情報を取得
        if let Some(sig) = confirmed_fns.get(&name_str) {
            // パラメータ型を更新
            if let MacroKind::Function { ref params, .. } = def.kind {
                for (i, param_id) in params.iter().enumerate() {
                    if i < sig.params.len() {
                        let (_, ty) = &sig.params[i];
                        if ty != "UnknownType" && !info_with_inferred.param_types.contains_key(param_id) {
                            info_with_inferred.param_types.insert(*param_id, ty.clone());
                        }
                    }
                }
            }
            // 戻り値型を更新
            if info_with_inferred.return_type.is_none() {
                info_with_inferred.return_type = sig.ret_ty.clone();
            }
        }

        // bindingsから戻り値型を取得（あれば優先）
        if let Some(rust_fn) = rust_decls.fns.get(&name_str) {
            if let Some(ref ret_ty) = rust_fn.ret_ty {
                info_with_inferred.return_type = Some(ret_ty.clone());
            }
        }

        // Rust関数を生成して出力
        let rust_code = codegen.macro_to_rust_fn(def, &info_with_inferred, expr);
        writeln!(out, "{}", rust_code)?;
        macro_success += 1;
    }

    // 失敗したマクロを出力
    for (name, reason) in &macro_failures {
        writeln!(out, "// FAILED: {} - {}", name, reason)?;
        macro_failure += 1;
    }

    // 統計を出力
    eprintln!("Macro functions: {} success, {} failures", macro_success, macro_failure);
    eprintln!("Total: {} success, {} failures",
        macro_success + inline_success,
        macro_failure + inline_failure);

    out.flush()?;
    Ok(())
}

/// デバッグ: マクロ変換を即座に出力
fn run_debug_macro_gen(pp: &mut Preprocessor) -> Result<(), Box<dyn std::error::Error>> {
    // フィールド辞書を作成
    let mut fields_dict = FieldsDict::new();
    fields_dict.add_target_dir("/usr/lib64/perl5/CORE");

    // パースしてフィールド辞書を構築
    let mut parser = match Parser::new(pp) {
        Ok(p) => p,
        Err(e) => return Err(format_error(&e, pp).into()),
    };

    let mut inline_count = 0usize;

    parser.parse_each(|result, _loc, path, interner| {
        if let Ok(ref decl) = result {
            fields_dict.collect_from_external_decl(decl, path);

            // inline関数を即座に出力
            let path_str = path.to_string_lossy();
            if path_str.contains("/usr/lib64/perl5/CORE") {
                if let ExternalDecl::FunctionDef(func_def) = decl {
                    if func_def.specs.is_inline {
                        if let Some(name) = func_def.declarator.name {
                            let name_str = interner.get(name);
                            println!("// INLINE: {} (from {})", name_str, path_str);
                            inline_count += 1;
                        }
                    }
                }
            }
        }
        std::ops::ControlFlow::Continue(())
    });

    // パース中に収集したtypedef名を取得（マクロパース時のキャスト式判定用）
    let typedefs = parser.typedefs().clone();

    eprintln!("Found {} inline functions", inline_count);

    // sv_any, sv_refcnt, sv_flags を登録
    {
        let interner = pp.interner_mut();
        let sv = interner.intern("sv");
        let sv_any = interner.intern("sv_any");
        let sv_refcnt = interner.intern("sv_refcnt");
        let sv_flags = interner.intern("sv_flags");
        fields_dict.set_unique_field_type(sv_any, sv);
        fields_dict.set_unique_field_type(sv_refcnt, sv);
        fields_dict.set_unique_field_type(sv_flags, sv);
    }

    // マクロ解析
    let interner = pp.interner();
    let files = pp.files();
    let mut analyzer = MacroAnalyzer::new(interner, files, &fields_dict);
    analyzer.set_typedefs(typedefs);
    analyzer.analyze(pp.macros());

    // RustCodeGen
    let codegen = RustCodeGen::new(interner, &fields_dict);

    // マクロを即座に出力（名前順ではない）
    // 引数のある関数マクロのみを対象とする
    use tinycc_macro_bindgen::MacroKind;

    let macros = pp.macros();
    let mut success_count = 0usize;
    let mut failure_count = 0usize;
    let mut skipped_object_macros = 0usize;

    for (name, info) in analyzer.iter() {
        if !info.is_target {
            continue;
        }

        let name_str = interner.get(*name);

        let def = match macros.get(*name) {
            Some(d) => d,
            None => {
                println!("// FAILED: {} - macro not found", name_str);
                failure_count += 1;
                continue;
            }
        };

        // オブジェクトマクロ（引数なし）はスキップ
        if matches!(def.kind, MacroKind::Object) {
            skipped_object_macros += 1;
            continue;
        }

        if info.category != MacroCategory::Expression {
            println!("// FAILED: {} - category: {:?}", name_str, info.category);
            failure_count += 1;
            continue;
        }

        // マクロ本体をパース（展開済みトークンも取得）
        let (expanded, parse_result) = analyzer.parse_macro_body(def, macros);
        let expr = match parse_result {
            Ok(e) => e,
            Err(e) => {
                let expanded_str = expanded.iter()
                    .map(|t| t.kind.format(interner))
                    .collect::<Vec<_>>()
                    .join(" ");
                println!("// FAILED: {} - parse error: {}", name_str, e);
                println!("//   expanded: {}", expanded_str);
                failure_count += 1;
                continue;
            }
        };

        let rust_code = codegen.macro_to_rust_fn(def, info, &expr);
        println!("{}", rust_code);
        success_count += 1;
    }

    eprintln!("Macros: {} success, {} failures (skipped {} object macros)",
        success_count, failure_count, skipped_object_macros);
    Ok(())
}

/// 型注釈付きS-expression出力モードで実行
fn run_typed_sexp(pp: &mut Preprocessor, output: Option<&PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    let mut parser = match Parser::new(pp) {
        Ok(p) => p,
        Err(e) => return Err(format_error(&e, pp).into()),
    };

    let tu = match parser.parse() {
        Ok(tu) => tu,
        Err(e) => return Err(format_error(&e, pp).into()),
    };

    // 出力
    if let Some(output_path) = output {
        let file = File::create(output_path)?;
        let mut writer = BufWriter::new(file);
        let mut printer = TypedSexpPrinter::new(&mut writer, pp.interner());
        for decl in &tu.decls {
            printer.print_external_decl(decl)?;
        }
        writer.flush()?;
    } else {
        let stdout = io::stdout();
        let mut handle = stdout.lock();
        let mut printer = TypedSexpPrinter::new(&mut handle, pp.interner());
        for decl in &tu.decls {
            printer.print_external_decl(decl)?;
        }
        handle.flush()?;
    }

    Ok(())
}

/// Rustバインディングファイルから宣言を抽出
fn run_parse_rust_bindings(rust_file: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let dict = RustDeclDict::parse_file(rust_file)?;

    // 統計情報を表示
    let stats = dict.stats();
    eprintln!("=== Rust Declarations Stats ===");
    eprintln!("Constants: {}", stats.const_count);
    eprintln!("Type aliases: {}", stats.type_count);
    eprintln!("Functions: {}", stats.fn_count);
    eprintln!("Structs: {}", stats.struct_count);
    eprintln!();

    // ダンプ
    println!("{}", dict.dump());

    Ok(())
}

/// エラー箇所のソースコードコンテキストを表示
fn show_source_context(pp: &Preprocessor, loc: &SourceLocation) {
    let path = pp.files().get_path(loc.file_id);

    // ファイルを読み込み
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Could not read source file: {}", e);
            return;
        }
    };

    let lines: Vec<&str> = content.lines().collect();
    let target_line = loc.line as usize;

    // エラー行の前後2行を表示
    let start = target_line.saturating_sub(3);
    let end = (target_line + 2).min(lines.len());

    eprintln!("\nSource context:");
    eprintln!("{}:{}:{}", path.display(), loc.line, loc.column);
    eprintln!("{}", "-".repeat(60));

    for i in start..end {
        let line_num = i + 1;
        let marker = if line_num == target_line as usize { ">>>" } else { "   " };
        if i < lines.len() {
            eprintln!("{} {:4} | {}", marker, line_num, lines[i]);

            // エラー行の場合、カラム位置を矢印で示す
            if line_num == target_line as usize && loc.column > 0 {
                let spaces = " ".repeat(loc.column as usize + 7);
                eprintln!("{}^", spaces);
            }
        }
    }
    eprintln!("{}", "-".repeat(60));
}

/// エラーをファイル名付きでフォーマット
fn format_error(e: &CompileError, pp: &Preprocessor) -> String {
    e.format_with_files(pp.files())
}

/// プリプロセス結果を出力
fn output_preprocessed(
    pp: &mut Preprocessor,
    output: Option<&PathBuf>,
    gcc_format: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut out: Box<dyn Write> = if let Some(path) = output {
        Box::new(BufWriter::new(File::create(path)?))
    } else {
        Box::new(io::stdout().lock())
    };

    if gcc_format {
        output_gcc_format(pp, &mut out)
    } else {
        output_debug_format(pp, &mut out)
    }
}

/// GCC互換形式で出力（diff比較用）
/// 行マーカーはファイル変更時と文の開始時のみ出力（文中のマクロ展開は無視）
fn output_gcc_format(
    pp: &mut Preprocessor,
    out: &mut dyn Write,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut last_file: Option<FileId> = None;
    let mut last_output_line = 0u32;
    let mut need_space = false;
    let mut at_statement_start = true;
    let mut brace_depth = 0i32;
    let mut pending_block_end = false;
    let mut file_stack: Vec<FileId> = Vec::new();

    loop {
        let token = match pp.next_token() {
            Ok(t) => t,
            Err(e) => return Err(format_error(&e, pp).into()),
        };

        if matches!(token.kind, TokenKind::Eof) {
            break;
        }

        let current_file = token.loc.file_id;
        let current_line = token.loc.line;

        // 文の開始時のみファイル/行の変更をチェック（ブレース更新前）
        if at_statement_start && brace_depth == 0 {
            if last_file != Some(current_file) {
                // ファイル変更
                if need_space {
                    writeln!(out)?;
                }

                // GCCフラグを決定
                let flag = if last_file.is_none() {
                    "" // 最初のファイル
                } else if file_stack.contains(&current_file) {
                    // 以前のファイルに戻る
                    while file_stack.last() != Some(&current_file) {
                        file_stack.pop();
                    }
                    " 2"
                } else {
                    // 新しいファイルに入る
                    if let Some(prev) = last_file {
                        file_stack.push(prev);
                    }
                    " 1"
                };

                let path = pp.files().get_path(current_file);
                writeln!(out, "# {} \"{}\"{}", current_line, path.display(), flag)?;
                last_file = Some(current_file);
                last_output_line = current_line;
                need_space = false;
            } else if current_line > last_output_line {
                // 同一ファイル内で行が進んだ
                let gap = current_line - last_output_line;
                if gap <= 8 {
                    // 小さいギャップは空行で埋める
                    if need_space {
                        writeln!(out)?;
                    }
                    for _ in 1..gap {
                        writeln!(out)?;
                    }
                    need_space = false;
                } else {
                    // 大きいギャップはディレクティブを使う
                    if need_space {
                        writeln!(out)?;
                    }
                    let path = pp.files().get_path(current_file);
                    writeln!(out, "# {} \"{}\"", current_line, path.display())?;
                    need_space = false;
                }
                last_output_line = current_line;
            }
            at_statement_start = false;
        }

        // 前のトークンがブロック終了で、次がセミコロンでない場合は改行
        // （関数定義の終わり）
        if pending_block_end && !matches!(token.kind, TokenKind::Semi) {
            writeln!(out)?;
            last_output_line += 1;
            need_space = false;
            at_statement_start = true;
        }
        pending_block_end = false;

        // ブレース深度を更新
        let was_in_block = brace_depth > 0;
        match token.kind {
            TokenKind::LBrace => brace_depth += 1,
            TokenKind::RBrace => brace_depth -= 1,
            _ => {}
        }

        // トークン間のスペース（セミコロン、カンマ、閉じ括弧の前は不要）
        if need_space && !matches!(token.kind, TokenKind::Semi | TokenKind::Comma | TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace) {
            write!(out, " ")?;
        }

        // 開き括弧の後はスペース不要
        let suppress_next_space = matches!(token.kind, TokenKind::LParen | TokenKind::LBracket);

        // トークンを出力
        write!(out, "{}", token.kind.format(pp.interner()))?;
        need_space = !suppress_next_space;

        // トップレベルのセミコロンで改行
        if brace_depth == 0 && matches!(token.kind, TokenKind::Semi) {
            writeln!(out)?;
            last_output_line += 1;
            need_space = false;
            at_statement_start = true;
        }
        // トップレベルに戻った閉じブレースをマーク
        if brace_depth == 0 && was_in_block && matches!(token.kind, TokenKind::RBrace) {
            pending_block_end = true;
        }
    }

    if need_space {
        writeln!(out)?;
    }
    Ok(())
}

/// デバッグ用詳細形式で出力（行追跡あり）
fn output_debug_format(
    pp: &mut Preprocessor,
    out: &mut dyn Write,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut last_line = 0u32;
    let mut last_file = None;
    let mut need_space = false;

    loop {
        let token = match pp.next_token() {
            Ok(t) => t,
            Err(e) => return Err(format_error(&e, pp).into()),
        };

        if matches!(token.kind, TokenKind::Eof) {
            break;
        }

        let current_file = Some(token.loc.file_id);
        let current_line = token.loc.line;

        // ファイルが変わった場合、または行が大きく変わった場合はディレクティブを出力
        if current_file != last_file {
            // ファイル変更
            if last_file.is_some() {
                writeln!(out)?;
            }
            let path = pp.files().get_path(token.loc.file_id);
            writeln!(out, "# {} \"{}\"", current_line, path.display())?;
            last_line = current_line;
            last_file = current_file;
            need_space = false;
        } else if current_line > last_line {
            // 行が進んだ
            let gap = current_line - last_line;
            if gap <= 8 {
                // 小さいギャップは空行で埋める
                for _ in 0..gap {
                    writeln!(out)?;
                }
            } else {
                // 大きいギャップはディレクティブを使う
                writeln!(out)?;
                let path = pp.files().get_path(token.loc.file_id);
                writeln!(out, "# {} \"{}\"", current_line, path.display())?;
            }
            last_line = current_line;
            need_space = false;
        }

        // トークン間のスペース
        if need_space {
            write!(out, " ")?;
        }

        // トークンを出力
        write!(out, "{}", token.kind.format(pp.interner()))?;
        need_space = true;
    }

    writeln!(out)?;
    Ok(())
}

/// -D オプションをパース（NAME または NAME=VALUE 形式）
fn parse_defines(defines: &[String]) -> Vec<(String, Option<String>)> {
    defines
        .iter()
        .map(|s| {
            if let Some(pos) = s.find('=') {
                let (name, value) = s.split_at(pos);
                (name.to_string(), Some(value[1..].to_string()))
            } else {
                (s.clone(), None)
            }
        })
        .collect()
}

/// inline関数からFunctionSignatureを抽出
fn extract_inline_fn_signature(
    func_def: &tinycc_macro_bindgen::FunctionDef,
    interner: &tinycc_macro_bindgen::StringInterner,
    codegen: &RustCodeGen,
) -> Option<FunctionSignature> {
    use tinycc_macro_bindgen::DerivedDecl;

    // 関数名を取得
    let name = func_def.declarator.name?;
    let name_str = interner.get(name).to_string();

    // パラメータを抽出
    let mut params = Vec::new();
    for derived in &func_def.declarator.derived {
        if let DerivedDecl::Function(param_list) = derived {
            for param in &param_list.params {
                if let Some(ref decl) = param.declarator {
                    if let Some(param_name) = decl.name {
                        let param_name_str = interner.get(param_name).to_string();
                        // 型を取得
                        let ty = codegen.param_decl_to_rust_type(param);
                        params.push((param_name_str, ty));
                    }
                }
            }
            break;
        }
    }

    // 戻り値型を抽出
    let ret_ty = codegen.extract_fn_return_type(&func_def.specs, &func_def.declarator.derived);

    Some(FunctionSignature {
        name: name_str,
        params,
        ret_ty,
    })
}
