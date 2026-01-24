# libperl-macrogen

Rust library and CLI tool for generating Rust FFI bindings from C macro functions and inline functions in Perl header files.

> **Pre-alpha Release**: This project is in early development. APIs may change without notice.

## Overview

`libperl-macrogen` parses C header files (particularly Perl's internal headers) and generates Rust wrapper functions for:

- **C macro functions** - Converted to Rust `unsafe fn` with type inference
- **Inline functions** - Extracted and converted to Rust equivalents

This tool is designed to complement [rust-bindgen](https://github.com/rust-lang/rust-bindgen), which cannot handle C macros.

## Installation

```bash
cargo install libperl-macrogen
```

Or add to your `Cargo.toml`:

```toml
[build-dependencies]
libperl-macrogen = "0.1"
```

## Usage

### CLI

```bash
# Generate Rust wrapper functions from Perl headers
libperl-macrogen --auto --gen-rust \
    --bindings path/to/bindings.rs \
    -o macro_fns.rs \
    wrapper.h

# Output to stdout (for inspection)
libperl-macrogen --auto --gen-rust \
    --bindings path/to/bindings.rs \
    wrapper.h

# With rustfmt validation
libperl-macrogen --auto --gen-rust \
    --bindings path/to/bindings.rs \
    --strict-rustfmt \
    wrapper.h
```

#### Options

| Option | Description |
|--------|-------------|
| `--auto` | Auto-detect Perl include paths and defines from `Config.pm` |
| `--gen-rust` | Generate Rust code for macros and inline functions |
| `--bindings <FILE>` | Path to bindgen-generated Rust bindings (for type inference) |
| `-o <FILE>` | Output file (stdout if omitted) |
| `--strict-rustfmt` | Fail if generated code doesn't pass rustfmt |
| `-I <DIR>` | Add include directory |
| `-D <MACRO>` | Define a macro |

### Library (in build.rs)

```rust
use std::path::PathBuf;
use libperl_macrogen::{InferConfig, run_macro_inference, CodegenConfig, CodegenDriver};

fn main() {
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let bindings_path = format!("{}/bindings.rs", out_dir);

    // Run type inference on macros and inline functions
    let config = InferConfig {
        input_file: PathBuf::from("wrapper.h"),
        bindings_path: Some(PathBuf::from(&bindings_path)),
        apidoc_path: None,  // Auto-detected from Perl installation
        apidoc_dir: None,
        debug: false,
    };

    let result = run_macro_inference(config).expect("Inference failed");

    // Generate Rust code
    let codegen_config = CodegenConfig::default();
    let mut output = std::fs::File::create(format!("{}/macro_fns.rs", out_dir)).unwrap();
    let mut driver = CodegenDriver::new(&mut output, result.preprocessor.interner(), codegen_config);
    driver.generate(&result).expect("Code generation failed");

    let stats = driver.stats();
    println!("cargo:warning=Generated {} macro + {} inline functions",
        stats.macro_success, stats.inline_success);
}
```

## Features

- C preprocessor with full macro expansion
- C parser for declarations, expressions, and inline function bodies
- Type inference from function call context and Perl's apidoc (`embed.fnc`)
- GCC extensions (`__attribute__`, `__typeof__`, statement expressions, etc.)
- Automatic Perl configuration detection via `Config.pm`

## Acknowledgments

This project was inspired by and references the implementation of [TinyCC](https://bellard.org/tcc/), originally created by Fabrice Bellard. The preprocessor and parser design draws from TinyCC's elegant approach to C compilation.

Special thanks to the [TinyCC community](https://github.com/TinyCC/tinycc) for continuing the development and maintenance of TinyCC. Their work served as a valuable reference throughout this project's development.

## License

This project is licensed under the [GNU Lesser General Public License v2.1](LICENSE) (LGPL-2.1), following TinyCC's licensing.

## Author

**hkoba** (CPAN ID: [HKOBA](https://metacpan.org/author/HKOBA))

## Related Projects

- [libperl-rs](https://github.com/hkoba/libperl-rs) - Rust bindings for Perl
