# Claude Code Project Guidelines

## Project Configuration

- **Rust Edition**: 2024 (do not change this)

## Perl C Header Files

The target Perl C header files (e.g., `sv.h`, `inline.h`, `perl.h`, `handy.h`) are located at `/usr/lib64/perl5/CORE/`. When investigating how Perl C macros or inline functions work, check these headers directly.

## TinyCC Reference Rule

**IMPORTANT**: When encountering problems or implementing new features:

1. **First check TinyCC's approach**: Before implementing a solution, investigate how TinyCC (especially `tccpp.c`) handles the same problem
2. **Document findings**: Explain how TinyCC solves the problem before proposing a solution
3. **Follow TinyCC patterns**: Prefer solutions that align with TinyCC's approach for consistency

The TinyCC source code is located in the `tinycc/` directory.

## Pipeline Architecture (3-Pass Rule)

**IMPORTANT**: This project follows a strict 3-pass pipeline architecture:

```
Phase 1: Preprocess  (preprocessor.rs)  — C header preprocessing
Phase 2: Infer       (macro_infer.rs, semantic.rs) — Type inference & analysis
Phase 3: Generate    (rust_codegen.rs)  — Rust code generation
```

### Pass Separation Rule

**Type analysis and semantic decisions MUST be done in Phase 2 (Infer), NOT in Phase 3 (Generate).**

Specifically:
- **Parameter type inference** (including const/mut) → Phase 2
- **Return type inference** (including bool override) → Phase 2
- **Dependency-ordered analysis** (topological sort, propagation) → Phase 2
- **Code emission and formatting** → Phase 3

Phase 3 should only read analysis results from Phase 2 (`InferResult`) and emit code.
It should NOT perform new type analysis, dependency resolution, or semantic decisions.

**Current technical debt**: Some analysis is currently done in Phase 3 (`rust_codegen.rs`):
- `collect_must_mut_pointer_params()` — should be in Phase 2
- `is_boolean_expr_with_context()` bool analysis pass — should be in Phase 2
- `infer_expr_type()` / `infer_expr_type_inline()` — should be in Phase 2
- `get_return_type()` void fallback — should be in Phase 2

When touching these areas, prefer moving logic to Phase 2 over adding more analysis to Phase 3.

## Development Workflow

### Signature Approval Rule

**IMPORTANT**: Before implementing any new module or making significant changes:

1. **Present Signatures First**: Before creating a new `.rs` file, present the public API (structs, enums, function signatures) to the user for review
2. **Wait for Approval**: Do not start implementation until the user explicitly approves the signatures

### Commit Guidelines

- Commit after each phase is complete
- Use descriptive commit messages explaining the changes

### Documentation Updates

When making changes to `src/macrogen.rs` (especially `generate()` function):
- Update `doc/macrogen-flow.md` to reflect the changes

### Architecture Documentation Updates

**IMPORTANT**: After completing any significant implementation task:

1. **Check if architecture docs need updates**: Review `doc/architecture*.md` files
2. **Key architecture files**:
   - `doc/architecture-semantic-type-inference.md` - Type inference, SemanticAnalyzer, TypeRepr
   - `doc/architecture-rust-codegen.md` - Code generation, RustCodegen, CodegenDriver
   - `doc/architecture-type-inference-and-cast.md` - Type inference and cast generation details
   - `doc/architecture-macro-expansion-control.md` - Macro expansion rules
   - `doc/architecture-inline-function-processing.md` - Inline function handling
   - `doc/architecture-thx-dependency.md` - THX (my_perl) dependency detection

### Debugging Type Inference

To dump type inference details for a specific function during code generation:

```bash
cargo run -- samples/xs-wrapper.h --auto --gen-rust --bindings samples/bindings.rs \
  --dump-types-for CxLABEL 2>&1 | grep -A30 '=== Type dump'
```

This outputs to stderr:
- Parameter constraints with confidence tiers and sources
- Return type TypeRepr and its tier
- Root expression constraints (for Expression-type macros)
- `const_pointer_positions` and `is_bool_return` flags

### File Locations

- **Temporary test files**: `./tmp/` directory, not `/tmp`
- **Implementation plans**: `doc/plan/` directory

### Integration Test Files

- **bindings.rs**: `samples/bindings.rs` (use this for checking function availability)
- **Integration test script**: `~/blob/libperl-rs/12-macrogen-2-build.zsh`
- **Build error log**: `tmp/build-error.log`
- **Generated macro bindings**: `tmp/macro_bindings.rs`

## Reference Documents

Details moved to separate files to keep CLAUDE.md focused:

- [CLI Usage](doc/reference-cli-usage.md) — command-line options and examples
- [Implemented Features](doc/reference-implemented-features.md) — supported GCC extensions, preprocessor features
- [Code Generation Rules](doc/reference-codegen-rules.md) — macro handling rules, consistency principle
