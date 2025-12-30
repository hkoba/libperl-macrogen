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
# Generate Rust functions from Perl headers
libperl-macrogen --auto --gen-rust-fns \
    --bindings bindings.rs \
    --apidoc embed.fnc \
    -o macro_fns.rs \
    wrapper.h
```

### Library (in build.rs)

```rust
use libperl_macrogen::{MacrogenBuilder, generate};

fn main() {
    let out_dir = std::env::var("OUT_DIR").unwrap();

    let config = MacrogenBuilder::new("wrapper.h", format!("{}/bindings.rs", out_dir))
        .with_perl_auto_config()
        .expect("Failed to get Perl config")
        .apidoc("apidoc/embed.json")
        .add_field_type("sv_any", "sv")
        .add_field_type("sv_refcnt", "sv")
        .add_field_type("sv_flags", "sv")
        .verbose(true)
        .build();

    let result = generate(&config).expect("Code generation failed");

    std::fs::write(format!("{}/macro_fns.rs", out_dir), &result.code)
        .expect("Failed to write output");

    println!("cargo:warning=Generated {} functions ({} inline, {} macro)",
        result.stats.inline_success + result.stats.macro_success,
        result.stats.inline_success,
        result.stats.macro_success);
}
```

## Features

- C preprocessor with macro expansion
- C parser for declarations and expressions
- Type inference from function call context
- Support for Perl's `embed.fnc` (apidoc) format
- GCC extensions (`__attribute__`, `__typeof__`, statement expressions, etc.)

## Acknowledgments

This project was inspired by and references the implementation of [TinyCC](https://bellard.org/tcc/), originally created by Fabrice Bellard. The preprocessor and parser design draws from TinyCC's elegant approach to C compilation.

Special thanks to the [TinyCC community](https://github.com/TinyCC/tinycc) for continuing the development and maintenance of TinyCC. Their work served as a valuable reference throughout this project's development.

## License

This project is licensed under the [GNU Lesser General Public License v2.1](LICENSE) (LGPL-2.1), following TinyCC's licensing.

## Author

**hkoba** (CPAN ID: [HKOBA](https://metacpan.org/author/HKOBA))

## Related Projects

- [libperl-rs](https://github.com/hkoba/libperl-rs) - Rust bindings for Perl
