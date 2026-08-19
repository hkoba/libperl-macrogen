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
`-I` は `--auto` と併用でき、追加のインクルードパスとして auto 設定の後にマージされる。
ビルド済み perl (actions-setup-perl / 古い perl-build 製) の `$Config{incpth}` が
実行環境の gcc と食い違って stddef.h 等が見つからない場合は
`-I "$(cc -print-file-name=include)"` を補うこと
(`scripts/multi-perl-smoke.pl` はこれを自動で行う)。

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

## Codegen List Options

- `--skip-codegen-list <FILE>`: codegen を抑制する関数名リスト (1 行 1 名、`#` コメント可、複数指定可)。
  運用ポリシーは CLAUDE.md の「skip_codegen 運用ポリシー」を参照
- `--require-codegen-list <FILE>`: **必ず生成されるべき**関数名リスト (書式は同上、複数指定可)。
  生成されなかった名前があれば、名前ごとの理由 (CASCADE_UNAVAILABLE の依存先、
  apidoc 宣言なし、CODEGEN_SUPPRESSED の patch reason 等) を stderr に表示して
  exit 1 する。**出力自体は最後まで書き切られる** (診断に使えるように)。
  同名が skip リストにも載っている設定矛盾は起動時に即エラー。
  partial eval 必須 API セットは `samples/require-partial-eval.txt`

```bash
cargo run -- samples/xs-wrapper.h --auto --gen-rust --bindings samples/bindings.rs \
  --require-codegen-list samples/require-partial-eval.txt
```

Pipeline API からは `PipelineBuilder::with_require_codegen_list()` /
`GeneratedPipeline::report` (CodegenReport) が対応する
(違反は `PipelineError::RequireCodegen`)。

## Macro Tracking CLI Options

- `--emit-macro-markers`: Output MacroBegin/MacroEnd marker tokens during preprocessing (for debugging)
- `--macro-comments`: Add definition location comments to generated Rust code (with `--gen-rust`)

Example with macro comments:
```bash
cargo run -- samples/xs-wrapper.h --auto --gen-rust --bindings samples/bindings.rs --macro-comments
```
