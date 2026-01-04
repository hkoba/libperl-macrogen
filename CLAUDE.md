# Claude Code Project Guidelines

This file contains guidelines for Claude Code when working on this project.

## Project Configuration

- **Rust Edition**: 2024 (do not change this)

## TinyCC Reference Rule

**IMPORTANT**: When encountering problems or implementing new features:

1. **First check TinyCC's approach**: Before implementing a solution, investigate how TinyCC (especially `tccpp.c`) handles the same problem
2. **Document findings**: Explain how TinyCC solves the problem before proposing a solution
3. **Follow TinyCC patterns**: Prefer solutions that align with TinyCC's approach for consistency

The TinyCC source code is located in the `tinycc/` directory.

## Development Workflow

### Signature Approval Rule

**IMPORTANT**: Before implementing any new module or making significant changes:

1. **Present Signatures First**: Before creating a new `.rs` file, present the public API (structs, enums, function signatures) to the user for review
2. **Wait for Approval**: Do not start implementation until the user explicitly approves the signatures
3. **Apply to Changes**: When making major changes to existing modules, present signature-level changes first

### Phase-based Development

This project is developed in phases. Each phase should:
1. Start with signature approval
2. Implement the approved design
3. Test and verify before moving to the next phase

### Current Phase Structure

- **Phase 1**: Lexer foundation (completed)
- **Phase 2**: Preprocessor (completed)
- **Phase 3**: Parser + S-expression dump (completed)
  - `ast.rs` - Abstract Syntax Tree definitions
  - `parser.rs` - C language parser
  - `sexp.rs` - S-expression output
  - `main.rs` - CLI binary for S-expression dump

### Commit Guidelines

- Commit after each phase is complete
- Use descriptive commit messages explaining the changes

### Documentation Updates

When making changes to `src/macrogen.rs` (especially `generate()` function):
- Update `doc/macrogen-flow.md` to reflect the changes
- Keep the processing flow description in sync with the actual code

### Test Files Location

Temporary test files should be placed in `./tmp/` directory, not `/tmp`.

## CLI Usage

### Testing with samples/wrapper.h

The recommended way to test with `samples/wrapper.h` is using the `--auto` option:

```bash
# Parse and output S-expression (recommended)
cargo run -- --auto samples/wrapper.h

# Streaming mode with source context on errors
cargo run -- --auto --streaming samples/wrapper.h

# Preprocess only (like gcc -E)
cargo run -- --auto -E samples/wrapper.h

# GCC-compatible output format (for diff comparison)
cargo run -- --auto -E --gcc-format samples/wrapper.h
```

The `--auto` option automatically retrieves include paths and defines from Perl's `Config.pm`.

### Testing Rust Function Generation (--gen-rust-fns)

To test macro-to-Rust function generation with production data:

```bash
cargo run --bin libperl-macrogen -- samples/wrapper.h --auto --gen-rust-fns --bindings samples/bindings.rs --apidoc samples/embed.fnc
```

This command:
- Uses `samples/wrapper.h` as input
- Reads type information from `samples/bindings.rs`
- Reads API documentation from `samples/embed.fnc`
- Generates Rust functions from C macros

### Manual Options (alternative)

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
  samples/wrapper.h
```

## Implemented Features

### GCC Extensions Supported

- `__attribute__((...))` - on functions, parameters, struct members, declarations
- `__extension__` - ignored
- `__asm__` / `asm` / `__asm` - inline assembly (skipped in parsing)
- `__typeof__` / `typeof` - typeof operator
- `__alignof__` / `__alignof` - alignof operator
- `__signed__` - signed keyword variant
- `bool` (C23/GCC) - boolean type
- `_Bool` - C99 boolean type
- `_Complex` - complex number type
- `_Float16`, `_Float32`, `_Float64`, `_Float128`, `_Float32x`, `_Float64x` - extended float types
- `__int128` - 128-bit integer type
- `_Thread_local` / `__thread` - thread-local storage (ignored)
- `({ ... })` - statement expressions
- `_Pragma(...)` - pragma operator (defined as empty macro)

### Preprocessor Features

- Object and function-like macros
- `#if`, `#ifdef`, `#ifndef`, `#elif`, `#else`, `#endif`
- `#include` and `#include_next`
- `#define` and `#undef`
- `#pragma` (ignored)
- `#error` and `#warning`
- Token pasting (`##`) and stringification (`#`)
- Variadic macros (`__VA_ARGS__`, `##__VA_ARGS__`)
- Predefined macros (`__FILE__`, `__LINE__`, etc.)
- Macro argument prescanning (C standard compliant)

### Macro Expansion Location Tracking

When errors occur in macro-expanded code, the error location points to where the macro is **used**, not where it is **defined**. This makes debugging easier.

## Current Status

- **wrapper.h parsing**: Successfully parses 5529 declarations
- **All tests passing**: 52 tests pass
