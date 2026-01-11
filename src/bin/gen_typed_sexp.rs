use std::env;
use std::path::Path;
use libperl_macrogen::{Parser, Preprocessor, PPConfig, TypedSexpPrinter};

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} <file.c>", args[0]);
        std::process::exit(1);
    }

    let path = Path::new(&args[1]);

    let mut pp = Preprocessor::new(PPConfig::default());
    if let Err(e) = pp.process_file(path) {
        eprintln!("Preprocess error: {:?}", e);
        std::process::exit(1);
    }

    let mut parser = match Parser::new(&mut pp) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Parser init error: {:?}", e);
            std::process::exit(1);
        }
    };

    let tu = match parser.parse() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("Parse error: {:?}", e);
            std::process::exit(1);
        }
    };

    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    let mut printer = TypedSexpPrinter::new(&mut handle, pp.interner());
    for decl in &tu.decls {
        printer.print_external_decl(decl).unwrap();
    }
}
