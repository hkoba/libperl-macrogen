# Claude Code Project Guidelines

## Project Configuration

- **Rust Edition**: 2024 (do not change this)
- **対象 Perl Build Mode**: Threaded (`-Dusethreads`) と Non-threaded
  (`-Uusethreads`) の両方をサポート。実行時に `Config{usethreads}` から
  自動検出（`PerlBuildMode::detect_from_perl_config`）。`--perl-build-mode`
  CLI フラグで明示指定可能。詳細は
  [doc/plan/non-threaded-perl-support.md](doc/plan/non-threaded-perl-support.md)
  と [doc/verification-non-threaded-build.md](doc/verification-non-threaded-build.md)。

## Perl C Header Files

The target Perl C header files (e.g., `sv.h`, `inline.h`, `perl.h`, `handy.h`) are located at `/usr/lib64/perl5/CORE/`. When investigating how Perl C macros or inline functions work, check these headers directly.

検証用の非 threaded perl は `tmp/perls/v5.42.2/bin/perl` に用意されている。
ヘッダ・libperl も同梱。`PATH=$PWD/tmp/perls/v5.42.2/bin:$PATH ...` で対象
perl を切り替えてビルド検証できる。

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

### 型の検査・操作は構造ベースで (Structure-First Type Handling)

**IMPORTANT**: 型の検査・分解・比較・変換は **構造的な enum マッチを第一**
とし、文字列操作 (`starts_with` / `strip_prefix` / `contains` / `split` / 正規表現)
に頼ることは極力避けて下さい。

対象の型表現:
- `UnifiedType` (`src/unified_type.rs`)
- `TypeRepr` / `CTypeSpecs` / `CDerivedType` / `RustTypeRepr` (`src/type_repr.rs`)

**禁止する代表パターン**:
- `ty_str.contains("*mut")` でポインタ判定 → `ut.is_pointer()` / `matches!(t, TypeRepr::CType { derived, .. } if derived.iter().any(|d| matches!(d, CDerivedType::Pointer { .. })))`
- `ty_str.starts_with(":: std :: option :: Option<")` で Option 判定 → `matches!(rt, RustTypeRepr::Option(_))` / `ut.is_optional_fn_ptr()`
- `ty_str.contains("void") && ty_str.contains('*')` で void* 判定 → `ut.is_void_pointer()`
- `ty_str.replace("*mut ", "...").replace(" :: ", "::")` で Rust 形式正規化 → 構造化された値の段階で持ち回す。`syn::Type` が手元にあるなら `UnifiedType::from_syn_type` 等の構造化エントリを使う
- `to_rust_string()` してから再度 `from_rust_str()` する round-trip → ロスがあるので避ける。構造のまま変換器を書く

**やむを得ず文字列を扱う場合**:
1. **入口**: 外部入力 (apidoc の C 型文字列、ヘッダパース結果) からの **一度きりのパース** に限定する
2. **出口**: emit 時の最終 `to_rust_string` / `to_display_string` のみ
3. **escape hatch**: `UnifiedType::Verbatim(String)` など「構造化を諦める」専用 variant を経由し、保持文字列は **syn 正規形** (`proc_macro2::TokenStream` の to_string()) に限定する。手書き文字列は入れない
4. パッチを当てる際は **なぜ構造化できないのか** をコメントで残し、後で解消できるよう負債として可視化する

**根拠 / 過去の事例**:
- 50dad70 で `*mut HV` round-trip を救うため `rust_type_string_to_c` ヘルパを足したが、`Option<extern "C" fn(...)>` 系の型では再度破綻した。文字列 prefix 剥がしの累積は脆弱性が線形に増える
- `from_type_string` (type_repr.rs) の `Option<` 判定は `to_token_stream().to_string()` が出力する空白入りトークン (`:: std :: option :: Option < ...>`) にマッチせず、長期間 fallthrough していた
- bindings.rs は **syn でパース済み** なので、文字列に潰してから再パースする経路は **設計上の劣化**。`syn::Type` を出発点にして構造的に decompose することを第一の選択肢とする

**Signature Approval Rule との関係**: `UnifiedType` / `TypeRepr` への
新 variant 追加や `from_*` 系コンストラクタの新設は、本ルールに沿った
構造化を進める変更として推奨される。実装前にシグネチャを提示してレビューを得ること。

## skip_codegen 運用ポリシー

**IMPORTANT**: `apidoc/*.patches.json` の `skip_codegen` および skip-list ファイルは、
Perl 本体側に存在しない関数や、現状の codegen で扱えない既知の構文を **明示的に**
除外するためのものです。

**禁止事項**: CI のビルド失敗ログを見て、エラーが出た関数名を skip_codegen に
継ぎ足していく運用は避けて下さい。これは以下の理由で根本解決を遠ざけます:

1. **問題の握りつぶし**: 失敗の原因（型推論の不備、cascade 伝播の漏れ、
   未対応構文 etc.）を特定せず symptom だけを抑止することになる
2. **複雑性の増大**: skip_codegen リストが肥大化し、本来 codegen 可能な関数まで
   除外されたり、後の改善で不要になったエントリが残り続ける
3. **cascade の不整合露呈**: そもそも caller が `[CASCADE_UNAVAILABLE]` に
   降格されない（= cascade 伝播の漏れ）ことが原因で skip_codegen を継ぎ足す羽目に
   なっているケースがある。これは Phase 2 の `check_function_availability` /
   `propagate_unavailable_*` が `apidoc_patches.skip_codegen` を参照していない
   ことに起因する設計上のギャップであり、**継ぎ足しではなく Phase 2 での
   `calls_unavailable` 伝播経路を直すべき**

**正しい対応順序**:
1. CI 失敗時はまず原因を特定する（型推論の不備か、cascade 漏れか、未対応構文か）
2. `calls_unavailable` 伝播の漏れであれば Phase 2 の伝播ロジックを修正する
3. codegen 側のバグなら codegen を直す
4. Perl 本体に存在しない関数 / 構造的に対応不可能な構文のみ skip_codegen に
   登録する（理由を `reason` フィールドに明記）

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

> 履歴メモ: かつて `--use-syn-expr` オプションで syn::Expr 経路を切り替える
> 二重出力モードを持っていた（旧パス: `tmp/`、syn パス: `tmp/new/`）。
> syn::Expr 経路への完全移行に伴い codegen 側のフラグは廃止されたため、
> オプションを付けても出力先が `tmp/new/` に変わるだけでコード生成結果は
> 同一になる。

## Reference Documents

Details moved to separate files to keep CLAUDE.md focused:

- [CLI Usage](doc/reference-cli-usage.md) — command-line options and examples
- [Implemented Features](doc/reference-implemented-features.md) — supported GCC extensions, preprocessor features
- [Code Generation Rules](doc/reference-codegen-rules.md) — macro handling rules, consistency principle
