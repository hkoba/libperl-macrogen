//! TinyCC Macro Bindgen CLI
//!
//! CファイルをパースしてS-expression形式で出力する

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;

use clap::Parser as ClapParser;
use tinycc_macro_bindgen::{
    CompileError, PPConfig, Parser, Preprocessor, SexpPrinter, TokenKind,
};

/// コマンドライン引数
#[derive(ClapParser)]
#[command(name = "tinycc-macro-bindgen")]
#[command(version, about = "C to Rust macro bindgen tool")]
struct Cli {
    /// 入力Cファイル
    input: PathBuf,

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
}

fn main() {
    if let Err(e) = run() {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    // プリプロセッサ設定
    let config = PPConfig {
        include_paths: cli.include,
        predefined: parse_defines(&cli.define),
        debug_pp: cli.debug_pp,
    };

    // プリプロセッサを初期化してファイルを処理
    let mut pp = Preprocessor::new(config);
    if let Err(e) = pp.process_file(&cli.input) {
        return Err(format_error(&e, &pp).into());
    }

    if cli.preprocess_only {
        // -E: プリプロセス結果のみ出力
        output_preprocessed(&mut pp, cli.output.as_ref())?;
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

/// エラーをファイル名付きでフォーマット
fn format_error(e: &CompileError, pp: &Preprocessor) -> String {
    e.format_with_files(pp.files())
}

/// プリプロセス結果を出力
fn output_preprocessed(
    pp: &mut Preprocessor,
    output: Option<&PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut out: Box<dyn Write> = if let Some(path) = output {
        Box::new(BufWriter::new(File::create(path)?))
    } else {
        Box::new(io::stdout().lock())
    };

    let mut last_line = 0u32;
    let mut last_file = None;

    loop {
        let token = match pp.next_token() {
            Ok(t) => t,
            Err(e) => return Err(format_error(&e, pp).into()),
        };

        if matches!(token.kind, TokenKind::Eof) {
            break;
        }

        // ファイルまたは行が変わったら # line ディレクティブを出力
        let current_file = Some(token.loc.file_id);
        if current_file != last_file || token.loc.line != last_line + 1 {
            if token.loc.line != last_line || current_file != last_file {
                let path = pp.files().get_path(token.loc.file_id);
                writeln!(out, "\n# {} \"{}\"", token.loc.line, path.display())?;
            }
        }
        last_line = token.loc.line;
        last_file = current_file;

        // トークンを出力
        write!(out, "{} ", token.kind.format(pp.interner()))?;
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
