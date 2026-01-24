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

### Library API

The library provides a Pipeline API with three phases:

1. **Preprocess** - Parse C header files with macro expansion
2. **Infer** - Perform type inference on macros and inline functions
3. **Generate** - Generate Rust code

#### Simple Usage (build.rs)

```rust
use std::fs::File;
use libperl_macrogen::Pipeline;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = std::env::var("OUT_DIR")?;
    let bindings_path = format!("{}/bindings.rs", out_dir);
    let output_path = format!("{}/macro_fns.rs", out_dir);

    // One-shot execution with Pipeline builder
    let mut output = File::create(&output_path)?;
    let result = Pipeline::builder("wrapper.h")
        .with_auto_perl_config()?
        .with_bindings(&bindings_path)
        .build()?
        .generate(&mut output)?;

    println!("cargo:warning=Generated {} macro + {} inline functions",
        result.stats.macro_success, result.stats.inline_success);

    Ok(())
}
```

#### Step-by-Step Execution

For more control, you can execute each phase separately:

```rust
use std::fs::File;
use libperl_macrogen::Pipeline;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Phase 1: Preprocess
    let preprocessed = Pipeline::builder("wrapper.h")
        .with_auto_perl_config()?
        .build()?
        .preprocess()?;

    println!("Preprocessed {} macros", preprocessed.macro_count());

    // Phase 2: Infer types
    let inferred = preprocessed
        .with_bindings("bindings.rs")
        .infer()?;

    println!("Inferred {} macro functions", inferred.result().macro_infos.len());

    // Phase 3: Generate Rust code
    let mut output = File::create("macro_fns.rs")?;
    let generated = inferred
        .with_strict_rustfmt()
        .generate(&mut output)?;

    println!("Generated {} functions", generated.stats.total_success());

    Ok(())
}
```

#### Pipeline Builder Options

```rust
Pipeline::builder("wrapper.h")
    // Preprocessor options
    .with_auto_perl_config()?      // Auto-detect from Perl's Config.pm
    .add_include_path("/usr/include")
    .add_define("DEBUG", Some("1"))
    .with_target_dir("/usr/lib64/perl5/CORE")

    // Inference options
    .with_bindings("bindings.rs")  // bindgen output for type info
    .with_apidoc_path("embed.fnc") // Perl API documentation

    // Codegen options
    .with_strict_rustfmt()         // Fail if rustfmt fails
    .with_codegen_defaults()       // Apply default codegen settings

    .build()?
    .generate(&mut output)?;
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
