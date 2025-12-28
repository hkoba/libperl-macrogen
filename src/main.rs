//! TinyCC Macro Bindgen CLI
//!
//! CファイルをパースしてS-expression形式で出力する

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;

use std::ops::ControlFlow;

use clap::Parser as ClapParser;
use tinycc_macro_bindgen::{
    get_perl_config, CompileError, ExternalDecl, FieldsDict, FileId, MacroAnalyzer,
    MacroCategory, PPConfig, Parser, Preprocessor, RustCodeGen, RustDeclDict, SexpPrinter,
    SourceLocation, TokenKind, TypedSexpPrinter,
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
    analyzer.analyze(pp.macros());

    // 統計情報を出力
    eprintln!("{}", analyzer.dump_stats());

    // Def-Use chain を出力
    println!("{}", analyzer.dump_def_use());

    Ok(())
}

/// マクロとinline関数からRust関数を生成
fn run_gen_rust_fns(
    pp: &mut Preprocessor,
    bindings_path: &PathBuf,
    output: Option<&PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
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

    // 4. マクロ解析
    let interner = pp.interner();
    let files = pp.files();
    let mut analyzer = MacroAnalyzer::new(interner, files, &fields_dict);
    analyzer.analyze(pp.macros());

    // 5. RustCodeGen を作成
    let codegen = RustCodeGen::new(interner, &fields_dict);

    // 6. 結果を収集（成功と失敗を分けて）
    #[derive(Clone)]
    enum GenResult {
        Success(String, String), // (name, rust_code)
        Failure(String, String), // (name, reason)
    }

    let mut results: Vec<GenResult> = Vec::new();

    // 6a. マクロから関数を生成
    // 引数のある関数マクロのみを対象とする
    use tinycc_macro_bindgen::MacroKind;

    let macros = pp.macros();
    for (name, info) in analyzer.iter() {
        // 対象ディレクトリ内かチェック
        if !info.is_target {
            continue;
        }

        let name_str = interner.get(*name).to_string();

        // マクロ定義を取得
        let def = match macros.get(*name) {
            Some(d) => d,
            None => {
                results.push(GenResult::Failure(name_str.clone(), "macro definition not found".to_string()));
                continue;
            }
        };

        // オブジェクトマクロ（引数なし）はスキップ
        if matches!(def.kind, MacroKind::Object) {
            continue;
        }

        // Expressionマクロのみ処理
        if info.category != MacroCategory::Expression {
            results.push(GenResult::Failure(
                name_str.clone(),
                format!("not an expression macro (category: {:?})", info.category),
            ));
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
                results.push(GenResult::Failure(
                    name_str.clone(),
                    format!("parse error: {} | expanded: {}", e, expanded_str),
                ));
                continue;
            }
        };

        // bindingsから戻り値型を取得（あれば優先）
        let mut info_with_bindings = info.clone();
        if let Some(rust_fn) = rust_decls.fns.get(&name_str) {
            if let Some(ref ret_ty) = rust_fn.ret_ty {
                info_with_bindings.return_type = Some(ret_ty.clone());
            }
        }

        // Rust関数を生成
        let rust_code = codegen.macro_to_rust_fn(def, &info_with_bindings, &expr);
        results.push(GenResult::Success(name_str, rust_code));
    }

    // 6b. inline関数からRust関数を生成（今は未実装でFailureとして記録）
    for (name, _decl, _path) in &inline_functions {
        results.push(GenResult::Failure(
            name.clone(),
            "inline function conversion not yet implemented".to_string(),
        ));
    }

    // 7. 結果を名前順にソート
    results.sort_by(|a, b| {
        let name_a = match a {
            GenResult::Success(n, _) => n,
            GenResult::Failure(n, _) => n,
        };
        let name_b = match b {
            GenResult::Success(n, _) => n,
            GenResult::Failure(n, _) => n,
        };
        name_a.cmp(name_b)
    });

    // 8. 出力
    let mut out: Box<dyn Write> = if let Some(path) = output {
        Box::new(BufWriter::new(File::create(path)?))
    } else {
        Box::new(io::stdout().lock())
    };

    // ヘッダーコメント
    writeln!(out, "// Auto-generated Rust functions from C macros and inline functions")?;
    writeln!(out, "// Source: samples/wrapper.h with types from samples/bindings.rs")?;
    writeln!(out)?;
    writeln!(out, "#![allow(non_snake_case)]")?;
    writeln!(out, "#![allow(unused)]")?;
    writeln!(out)?;
    writeln!(out, "use std::ffi::{{c_char, c_int, c_uint, c_long, c_ulong, c_void}};")?;
    writeln!(out)?;

    // 統計
    let success_count = results.iter().filter(|r| matches!(r, GenResult::Success(_, _))).count();
    let failure_count = results.iter().filter(|r| matches!(r, GenResult::Failure(_, _))).count();
    eprintln!("Generated {} functions, {} failures", success_count, failure_count);

    // 関数を出力
    for result in &results {
        match result {
            GenResult::Success(_name, rust_code) => {
                writeln!(out, "{}", rust_code)?;
            }
            GenResult::Failure(name, reason) => {
                writeln!(out, "// FAILED: {} - {}", name, reason)?;
            }
        }
    }

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
