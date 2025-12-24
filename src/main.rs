//! TinyCC Macro Bindgen CLI
//!
//! CファイルをパースしてS-expression形式で出力する

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;

use clap::Parser as ClapParser;
use tinycc_macro_bindgen::{PPConfig, Parser, Preprocessor, SexpPrinter};

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
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    // プリプロセッサ設定
    let config = PPConfig {
        include_paths: cli.include,
        predefined: parse_defines(&cli.define),
    };

    // プリプロセッサを初期化してファイルを処理
    let mut pp = Preprocessor::new(config);
    pp.process_file(&cli.input)?;

    // パース
    let mut parser = Parser::new(&mut pp)?;
    let tu = parser.parse()?;

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
