# Public API Usage Report

## 分析基準

`--gen-rust` 実行パスで使用されるかどうかを基準に判定。

```
main.rs (--gen-rust)
  → Pipeline::builder().build()
  → Pipeline::preprocess() → PreprocessedPipeline
  → PreprocessedPipeline::infer() → InferredPipeline
  → InferredPipeline::generate() → GeneratedPipeline
```

## 使用状況サマリー

| カテゴリ | ファイル数 |
|----------|-----------|
| --gen-rust で使用 | 25 |
| 他モードのみで使用 | 2 |
| 未使用/deprecated | 2 |

---

## ファイル別詳細

### Core Pipeline (--gen-rust で使用)

| ファイル | 主要公開要素 | 状況 |
|----------|-------------|------|
| `pipeline.rs` | `Pipeline`, `PipelineBuilder`, `PreprocessedPipeline`, `InferredPipeline`, `GeneratedPipeline`, `PipelineError` | **Active** |
| `infer_api.rs` | `InferResult`, `InferConfig`, `InferError`, `InferStats`, `run_inference_with_preprocessor` | **Active** |
| `rust_codegen.rs` | `CodegenDriver`, `RustCodegen`, `GeneratedCode`, `GenerateStatus`, `CodegenStats`, `CodegenConfig` | **Active** |
| `macro_infer.rs` | `MacroInferContext`, `MacroInferInfo`, `MacroParam`, `ParseResult`, `NoExpandSymbols` | **Active** |
| `preprocessor.rs` | `Preprocessor`, `PPConfig`, callback traits | **Active** |

### Data Structures (--gen-rust で使用)

| ファイル | 主要公開要素 | 状況 |
|----------|-------------|------|
| `ast.rs` | `Expr`, `Stmt`, `ExternalDecl`, etc. | **Active** |
| `apidoc.rs` | `ApidocDict`, `ApidocEntry`, `ApidocCollector` | **Active** |
| `enum_dict.rs` | `EnumDict` | **Active** |
| `fields_dict.rs` | `FieldsDict` | **Active** |
| `inline_fn.rs` | `InlineFnDict` | **Active** |
| `rust_decl.rs` | `RustDeclDict`, `RustFn`, `RustStruct` | **Active** |
| `type_env.rs` | `TypeEnv`, `TypeConstraint`, `ParamLink` | **Active** |
| `type_repr.rs` | `TypeRepr` | **Active** |

### Infrastructure (--gen-rust で使用)

| ファイル | 主要公開要素 | 状況 |
|----------|-------------|------|
| `intern.rs` | `StringInterner`, `InternedStr` | **Active** |
| `source.rs` | `FileRegistry`, `FileId`, `SourceLocation` | **Active** |
| `token.rs` | `Token`, `TokenKind`, `Comment` | **Active** |
| `token_source.rs` | `TokenSlice`, `TokenSource` | **Active** |
| `lexer.rs` | `Lexer`, traits | **Active** |
| `parser.rs` | `Parser`, `parse_expression_from_tokens` | **Active** |
| `error.rs` | `CompileError`, error types | **Active** |
| `macro_def.rs` | `MacroDef`, `MacroKind`, `MacroTable` | **Active** |
| `perl_config.rs` | `PerlConfig`, `get_perl_config` | **Active** |
| `pp_expr.rs` | preprocessor expression evaluation | **Active** (internal) |

### Analysis Support (--gen-rust で使用)

| ファイル | 主要公開要素 | 状況 |
|----------|-------------|------|
| `semantic.rs` | `SemanticAnalyzer`, `Type`, `Symbol` | **Active** (macro_infer.rs で使用) |
| `unified_type.rs` | `UnifiedType`, `SourcedType`, `IntSize` | **Active** (semantic.rs, type_registry.rs で使用) |
| `sexp.rs` | `SexpPrinter`, `TypedSexpPrinter` | **Active** (rust_codegen.rs, main.rs で使用) |

### 他モードのみで使用

| ファイル | 主要公開要素 | 状況 | 使用箇所 |
|----------|-------------|------|----------|
| `apidoc_data.rs` | `APIDOC_EMBEDDED_DATA` | Other modes | `--auto` 時の埋め込みデータ |

### 未使用/Deprecated

| ファイル | 主要公開要素 | 状況 | 備考 |
|----------|-------------|------|------|
| `thx_collector.rs` | `ThxCollector` | **Unused** | lib.rs でコメントアウト済み。HashSet 方式に置き換え |
| `type_registry.rs` | `TypeRegistry`, `TypeEquality` | **Unused** | どこからも参照されていない |

---

## Deprecated API 詳細

### 削除済み

以下のモジュール・関数は削除済み:

- `thx_collector.rs` - 削除済み（HashSet 方式に置き換え）
- `type_registry.rs` - 削除済み（未使用のため）
- `run_macro_inference()` - 削除済み（Pipeline API に統合）
- `detect_sv_any_patterns()`, `SvAnyPattern` - 削除済み
- `detect_sv_u_field_patterns()`, `SvUFieldPattern` - 削除済み
- `apply_sv_any_constraints()`, `apply_sv_u_field_constraints()` - 削除済み

---

## 今後の整理検討

1. **`sexp.rs`** - `--gen-rust` では `SexpPrinter` のみ使用（コメント出力用）。`TypedSexpPrinter` は他モード用
2. **`semantic.rs`** の `Type::to_unified()` - `unified_type.rs` との関係を整理
