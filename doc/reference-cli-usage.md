# CLI Usage Reference

## Testing with samples/xs-wrapper.h

The recommended way to test with `samples/xs-wrapper.h` is using the `--auto` option:

```bash
# Parse and output S-expression (recommended)
cargo run -- --auto samples/xs-wrapper.h

# Streaming mode with source context on errors
cargo run -- --auto --streaming samples/xs-wrapper.h

# Preprocess only (like gcc -E)
cargo run -- --auto -E samples/xs-wrapper.h

# GCC-compatible output format (for diff comparison)
cargo run -- --auto -E --gcc-format samples/xs-wrapper.h
```

The `--auto` option automatically retrieves include paths and defines from Perl's `Config.pm`.

## Testing Rust Function Generation (--gen-rust)

To test macro-to-Rust function generation with production data:

```bash
cargo run -- samples/xs-wrapper.h --auto --gen-rust --bindings samples/bindings.rs
```

This command:
- Uses `samples/xs-wrapper.h` as input
- Reads type information from `samples/bindings.rs`
- `--auto` automatically reads API documentation from `apidoc/embed.fnc`
- Generates Rust functions from C macros

## Testing Macro Type Inference (--infer-macro-types)

```bash
cargo run -- --auto --infer-macro-types samples/xs-wrapper.h --bindings samples/bindings.rs
```

This command:
- Uses `samples/xs-wrapper.h` as input
- Reads Rust type bindings from `samples/bindings.rs` (required for function signatures)
- `--auto` automatically reads API documentation (required for macro/function type hints)
- Performs type inference on all macros and outputs statistics

## Manual Options (alternative)

If `--auto` doesn't work, use explicit options:

```bash
cargo run -- -E \
  -I/usr/include \
  -I/usr/include/linux \
  -I/usr/lib/gcc/x86_64-redhat-linux/15/include \
  -D_REENTRANT \
  -D_GNU_SOURCE \
  -I/usr/local/include \
  -D_LARGEFILE_SOURCE \
  -D_FILE_OFFSET_BITS=64 \
  -I/usr/lib64/perl5/CORE \
  -D__linux \
  -D__linux__ \
  -D__unix \
  -D__unix__ \
  -D__x86_64 \
  -D__x86_64__ \
  -Dlinux \
  -Dunix \
  -D__gnu_linux__ \
  -D__STDC__ \
  -D__LP64__ \
  -D_LP64 \
  samples/xs-wrapper.h
```

## Macro Tracking CLI Options

- `--emit-macro-markers`: Output MacroBegin/MacroEnd marker tokens during preprocessing (for debugging)
- `--macro-comments`: Add definition location comments to generated Rust code (with `--gen-rust`)

Example with macro comments:
```bash
cargo run -- samples/xs-wrapper.h --auto --gen-rust --bindings samples/bindings.rs --macro-comments
```
