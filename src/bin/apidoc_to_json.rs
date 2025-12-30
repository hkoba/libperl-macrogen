//! Perl apidoc (embed.fnc) を JSON に変換するツール
//!
//! Usage:
//!   apidoc-to-json embed.fnc              # 標準出力へJSON出力
//!   apidoc-to-json embed.fnc -o out.json  # ファイルへ保存

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;

use clap::Parser;
use tinycc_macro_bindgen::ApidocDict;

#[derive(Parser)]
#[command(name = "apidoc-to-json")]
#[command(version, about = "Convert Perl embed.fnc (apidoc format) to JSON")]
struct Cli {
    /// 入力ファイル (embed.fnc)
    input: PathBuf,

    /// 出力ファイル（省略時は標準出力）
    #[arg(short = 'o', long = "output")]
    output: Option<PathBuf>,

    /// 統計情報を標準エラーに出力
    #[arg(short = 'v', long = "verbose")]
    verbose: bool,

    /// コンパクトなJSON出力（改行なし）
    #[arg(long = "compact")]
    compact: bool,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    // embed.fnc をパース
    let dict = ApidocDict::parse_embed_fnc(&cli.input)?;

    if cli.verbose {
        let stats = dict.stats();
        eprintln!("Loaded {} entries from {:?}", stats.total, cli.input);
        eprintln!("  Functions: {}", stats.function_count);
        eprintln!("  Macros: {}", stats.macro_count);
        eprintln!("  Inline: {}", stats.inline_count);
        eprintln!("  Public API: {}", stats.api_count);
    }

    // JSONにシリアライズ
    let json = if cli.compact {
        serde_json::to_string(&dict)?
    } else {
        serde_json::to_string_pretty(&dict)?
    };

    // 出力
    if let Some(output_path) = cli.output {
        let file = File::create(&output_path)?;
        let mut writer = BufWriter::new(file);
        writeln!(writer, "{}", json)?;
        writer.flush()?;

        if cli.verbose {
            eprintln!("Written to {:?}", output_path);
        }
    } else {
        println!("{}", json);
    }

    Ok(())
}
