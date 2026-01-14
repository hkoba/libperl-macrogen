//! TinyCC Macro Bindgen CLI
//!
//! CファイルをパースしてS-expression形式で出力する

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;

use std::ops::ControlFlow;

use clap::Parser as ClapParser;
use libperl_macrogen::{
    get_default_target_dir, get_perl_config, ApidocCollector, ApidocDict,
    BlockItem, CompileError, ExternalDecl, FieldsDict, FileId, InlineFnDict, MacroInferContext,
    PPConfig, ParseResult, Parser, Preprocessor, RustDeclDict, SexpPrinter,
    SourceLocation, TokenKind, TypedSexpPrinter,
};

/// コマンドライン引数
#[derive(ClapParser)]
#[command(name = "libperl-macrogen")]
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

    /// ターゲットディレクトリ（デフォルト: /usr/lib64/perl5/CORE）
    #[arg(long = "target-dir")]
    target_dir: Option<PathBuf>,

    /// Rustバインディングファイルから宣言を抽出
    #[arg(long = "parse-rust-bindings")]
    parse_rust_bindings: Option<PathBuf>,

    // 廃止予定: --analyze-macros (MacroAnalyzer2 使用)
    // #[arg(long = "analyze-macros")]
    // analyze_macros: bool,

    // 廃止予定: --gen-rust-fns (macrogen 使用)
    // #[arg(long = "gen-rust-fns")]
    // gen_rust_fns: bool,

    /// Rustバインディングファイル（--infer-macro-types用）
    #[arg(long = "bindings")]
    bindings: Option<PathBuf>,

    /// Perl apidocファイル (embed.fnc)
    #[arg(long = "apidoc")]
    apidoc: Option<PathBuf>,

    // 廃止予定: --debug-macro-gen (MacroAnalyzer2, RustCodeGen 使用)
    // #[arg(long = "debug-macro-gen")]
    // debug_macro_gen: bool,

    /// 進行状況を表示
    #[arg(long = "progress")]
    progress: bool,

    /// マクロ展開マーカーを出力（デバッグ用）
    #[arg(long = "emit-macro-markers")]
    emit_macro_markers: bool,

    /// 生成コードにマクロ定義位置コメントを追加
    #[arg(long = "macro-comments")]
    macro_comments: bool,

    /// マクロ型推論（ExprId + TypeEnv ベース）
    #[arg(long = "infer-macro-types")]
    infer_macro_types: bool,
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
        // target_dir: CLI 指定があればそれを使用、なければデフォルト
        let target_dir = cli.target_dir.clone().or_else(|| get_default_target_dir().ok());
        PPConfig {
            include_paths: perl_cfg.include_paths,
            predefined: defines,
            debug_pp: cli.debug_pp,
            target_dir,
            emit_markers: cli.emit_macro_markers,
        }
    } else {
        // 従来通り CLI 引数から
        PPConfig {
            include_paths: cli.include.clone(),
            predefined: parse_defines(&cli.define),
            debug_pp: cli.debug_pp,
            target_dir: cli.target_dir.clone(),
            emit_markers: cli.emit_macro_markers,
        }
    };

    // 廃止予定: --gen-rust-fns
    // if cli.gen_rust_fns {
    //     let bindings = cli.bindings.ok_or("--bindings is required with --gen-rust-fns")?;
    //     return run_gen_rust_fns_lib(...);
    // }

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
        run_dump_fields_dict(&mut pp, cli.target_dir.as_ref())?;
    // 廃止予定: --analyze-macros
    // } else if cli.analyze_macros {
    //     run_analyze_macros(&mut pp, cli.target_dir.as_ref())?;
    } else if cli.infer_macro_types {
        // --infer-macro-types: マクロ型推論
        run_infer_macro_types(&mut pp, cli.apidoc.as_ref(), cli.bindings.as_ref())?;
    // 廃止予定: --debug-macro-gen
    // } else if cli.debug_macro_gen {
    //     run_debug_macro_gen(&mut pp)?;
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
fn run_dump_fields_dict(pp: &mut Preprocessor, _target_dir: Option<&PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    // フィールド辞書を作成
    let mut fields_dict = FieldsDict::new();

    let mut parser = match Parser::new(pp) {
        Ok(p) => p,
        Err(e) => return Err(format_error(&e, pp).into()),
    };

    // パースしながらフィールド情報を収集
    parser.parse_each(|result, _loc, _path, interner| {
        if let Ok(ref decl) = result {
            fields_dict.collect_from_external_decl(decl, decl.is_target(), interner);
        }
        std::ops::ControlFlow::Continue(())
    });

    // 統計情報を表示
    let stats = fields_dict.stats();
    let interner = parser.interner();
    eprintln!("=== Fields Dictionary Stats ===");
    eprintln!("Total fields: {}", stats.total_fields);
    eprintln!("Unique fields (can infer struct): {}", stats.unique_fields);
    eprintln!("Ambiguous fields: {}", stats.ambiguous_fields);
    eprintln!("Field types collected: {}", fields_dict.field_types_count());
    eprintln!();

    // 一意なフィールドをダンプ
    println!("{}", fields_dict.dump_unique(interner));

    Ok(())
}

// 廃止予定: run_analyze_macros (MacroAnalyzer2 使用)
// fn run_analyze_macros(pp: &mut Preprocessor, target_dir: Option<&PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
//     ...
// }

/// マクロ型推論（ExprId + TypeEnv ベース）
fn run_infer_macro_types(
    pp: &mut Preprocessor,
    apidoc_path: Option<&PathBuf>,
    bindings_path: Option<&PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    // フィールド辞書を作成（パースしながら収集）
    let mut fields_dict = FieldsDict::new();

    // ApidocCollector を Preprocessor に設定
    pp.set_macro_def_callback(Box::new(ApidocCollector::new()));

    // パーサー作成
    let mut parser = match Parser::new(pp) {
        Ok(p) => p,
        Err(e) => return Err(format_error(&e, pp).into()),
    };

    // inline 関数辞書を作成
    let mut inline_fn_dict = InlineFnDict::new();

    // parse_each でフィールド辞書と inline 関数を収集（同時にマクロ定義→コールバック呼び出し）
    parser.parse_each(|result, _loc, _path, interner| {
        if let Ok(ref decl) = result {
            fields_dict.collect_from_external_decl(decl, decl.is_target(), interner);

            // inline 関数を収集
            if decl.is_target() {
                if let ExternalDecl::FunctionDef(func_def) = decl {
                    inline_fn_dict.collect_from_function_def(func_def);
                }
            }
        }
        std::ops::ControlFlow::Continue(())
    });

    // パーサーから typedef 辞書を取得
    let typedefs = parser.typedefs().clone();

    // コールバックを取り出してダウンキャスト
    let callback = pp.take_macro_def_callback().expect("callback should exist");
    let apidoc_collector = callback
        .into_any()
        .downcast::<ApidocCollector>()
        .expect("callback type mismatch");

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

    // 一致型キャッシュを構築（全フィールドについて型の一貫性を事前計算）
    fields_dict.build_consistent_type_cache();

    // Apidoc をロード（ファイルから + コメントから）
    let mut apidoc = if let Some(path) = apidoc_path {
        ApidocDict::load_auto(path)?
    } else {
        ApidocDict::new()
    };
    let apidoc_from_comments = apidoc_collector.len();
    apidoc_collector.merge_into(&mut apidoc);

    // RustDeclDict をロード
    let rust_decl_dict = if let Some(path) = bindings_path {
        Some(RustDeclDict::parse_file(path)?)
    } else {
        None
    };

    // MacroInferContext を作成して解析
    let mut infer_ctx = MacroInferContext::new();

    // THX シンボルを事前に intern
    let sym_athx = pp.interner_mut().intern("aTHX");
    let sym_tthx = pp.interner_mut().intern("tTHX");
    let sym_my_perl = pp.interner_mut().intern("my_perl");
    let thx_symbols = (sym_athx, sym_tthx, sym_my_perl);

    // assert シンボルを事前に intern
    let sym_assert = pp.interner_mut().intern("assert");
    let sym_assert_ = pp.interner_mut().intern("assert_");
    let assert_symbols = (sym_assert, sym_assert_);

    let interner = pp.interner();
    let files = pp.files();

    infer_ctx.analyze_all_macros(
        pp.macros(),
        interner,
        files,
        Some(&apidoc),
        Some(&fields_dict),
        rust_decl_dict.as_ref(),
        Some(&inline_fn_dict),
        &typedefs,
        thx_symbols,
        assert_symbols,
    );

    // パース結果の統計を収集
    let mut expr_count = 0;
    let mut stmt_count = 0;
    let mut unparseable_count = 0;
    for info in infer_ctx.macros.values() {
        if !info.is_target {
            continue;
        }
        if info.is_expression() {
            expr_count += 1;
        } else if info.is_statement() {
            stmt_count += 1;
        } else {
            unparseable_count += 1;
        }
    }

    // 統計情報を出力
    let stats = infer_ctx.stats();
    eprintln!("=== Macro Type Inference Stats ===");
    eprintln!("Total macros analyzed: {}", stats.total);
    eprintln!("  - Expression macros: {}", expr_count);
    eprintln!("  - Statement macros: {}", stmt_count);
    eprintln!("  - Unparseable: {}", unparseable_count);
    eprintln!("Confirmed (type complete): {}", stats.confirmed);
    eprintln!("Unconfirmed (pending): {}", stats.unconfirmed);
    eprintln!("Args unknown: {}", stats.args_unknown);
    eprintln!("Return unknown: {}", stats.return_unknown);
    eprintln!();

    // コメントから収集した apidoc 数
    eprintln!("Apidoc from comments: {}", apidoc_from_comments);
    // THX 依存マクロ数（解析済みターゲットマクロのうち）
    let thx_count = infer_ctx.macros.values().filter(|info| info.is_target && info.is_thx_dependent).count();
    eprintln!("THX-dependent macros: {}", thx_count);
    eprintln!();

    // 各マクロの詳細を出力（辞書順）
    println!("=== Macro Analysis Results ===");
    let mut parseable_count = 0;
    let mut has_constraints_count = 0;

    // マクロ名でソートするためにベクターに収集
    // - 関数形式マクロ、または THX 依存のオブジェクトマクロを出力
    // - 空のトークン列を持つマクロ（条件コンパイルフラグ等）は除外
    let mut sorted_macros: Vec<_> = infer_ctx
        .macros
        .iter()
        .filter(|(_, info)| info.is_target && info.has_body && (info.is_function || info.is_thx_dependent))
        .collect();
    sorted_macros.sort_by_key(|(name, _)| interner.get(**name));

    for (name, info) in sorted_macros {
        let name_str = interner.get(*name);

        let parse_status = if info.is_expression() {
            parseable_count += 1;
            "expression"
        } else if info.is_statement() {
            parseable_count += 1;
            "statement"
        } else {
            "unparseable"
        };

        let thx_marker = if info.is_thx_dependent { " [THX]" } else { "" };
        let pasting_marker = if info.has_token_pasting { " [##]" } else { "" };
        let constraint_count = info.type_env.total_constraint_count();

        if constraint_count > 0 {
            has_constraints_count += 1;
        }

        println!(
            "{}: {} ({} constraints, {} uses){}{}",
            name_str,
            parse_status,
            constraint_count,
            info.uses.len(),
            thx_marker,
            pasting_marker
        );

        // 型付き S 式を追加出力（pretty print）
        match &info.parse_result {
            ParseResult::Expression(expr) => {
                let stdout = io::stdout();
                let mut handle = stdout.lock();
                let mut printer = TypedSexpPrinter::new(&mut handle, interner);
                printer.set_type_env(&info.type_env);
                printer.set_pretty(true);
                printer.set_indent(1);  // 行頭にスペース1文字分のインデント
                printer.set_skip_first_newline(true);  // 先頭の空行を抑制
                let _ = printer.print_expr(expr);
                let _ = writeln!(handle);
            }
            ParseResult::Statement(block_items) => {
                let stdout = io::stdout();
                let mut handle = stdout.lock();
                let _ = write!(handle, " ");  // 最初の行頭スペース
                let mut printer = TypedSexpPrinter::new(&mut handle, interner);
                printer.set_type_env(&info.type_env);
                printer.set_pretty(true);
                printer.set_indent(1);  // 行頭にスペース1文字分のインデント
                printer.set_skip_first_newline(true);  // 先頭の空行を抑制
                for item in block_items {
                    if let BlockItem::Stmt(stmt) = item {
                        let _ = printer.print_stmt(stmt);
                    }
                }
                let _ = writeln!(handle);
            }
            ParseResult::Unparseable(Some(err_msg)) => {
                println!("  error: {}", err_msg);
            }
            _ => {}
        }

        // 型制約の詳細
        if constraint_count > 0 {
            for (expr_id, constraints) in &info.type_env.expr_constraints {
                for c in constraints {
                    println!("  expr#{}: {} ({})", expr_id.0, c.ty.to_display_string(interner), c.context);
                }
            }
        }
    }

    eprintln!();
    eprintln!("Parseable macros: {}", parseable_count);
    eprintln!("Macros with type constraints: {}", has_constraints_count);

    Ok(())
}

// 廃止予定: run_gen_rust_fns_lib (macrogen 使用)
// fn run_gen_rust_fns_lib(...) -> Result<(), Box<dyn std::error::Error>> {
//     ...
// }

// 廃止予定: run_debug_macro_gen (MacroAnalyzer2, RustCodeGen 使用)
// fn run_debug_macro_gen(pp: &mut Preprocessor) -> Result<(), Box<dyn std::error::Error>> {
//     ...
// }

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

