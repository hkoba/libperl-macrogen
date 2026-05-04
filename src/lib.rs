//! # libperl-macrogen
//!
//! Generate Rust FFI bindings for the things `bindgen` can't see:
//! C **macro functions** and **`static inline`** definitions in
//! Perl's header tree.
//!
//! `rust-bindgen` is the standard for translating C declarations to
//! Rust, but it deliberately skips macro-shaped function definitions
//! (because they have no fixed type signature) and produces no Rust
//! body for `static inline` functions (because their definitions live
//! in headers, not the linked library). For wrapping libperl that
//! gap is huge — much of the public-looking API (`SvIV`, `newRV_inc`,
//! `PL_stack_base`, hundreds more) is exposed as macros or
//! `static inline` only.
//!
//! `libperl-macrogen` complements `bindgen`: it lex / parse /
//! type-infers the relevant headers and emits Rust wrappers like
//!
//! ```text
//! pub unsafe fn SvIV(my_perl: *mut PerlInterpreter, sv: *mut SV) -> IV {
//!     unsafe { Perl_SvIV(my_perl, sv) }
//! }
//! ```
//!
//! plus declarative macros for `PERLVAR`-driven globals (so the same
//! `PL_stack_base!(my_perl)` source compiles against threaded and
//! non-threaded Perl).
//!
//! ## Library API
//!
//! The high-level entry point is the [`Pipeline`] builder, which
//! drives a header file through the preprocess → infer → codegen
//! stages and writes a Rust source file:
//!
//! ```no_run
//! use libperl_macrogen::Pipeline;
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let mut output = std::fs::File::create("macro_bindings.rs")?;
//! Pipeline::builder("xs-wrapper.h")
//!     .with_auto_perl_config()?
//!     .with_bindings("bindings.rs")     // bindgen output for type info
//!     .with_codegen_defaults()
//!     .build()?
//!     .generate(&mut output)?;
//! # Ok(())
//! # }
//! ```
//!
//! See [`PipelineBuilder`] for the full set of options
//! (skip-list, extra include paths, codegen knobs, ...).
//!
//! ## CLI
//!
//! Installing the crate also gives you a `libperl-macrogen` binary
//! for one-off / inspection use. Run with `--help` for the option
//! summary.
//!
//! ## Build-time data download
//!
//! At build time (the first `cargo build` after install / update),
//! the crate's own `build.rs` downloads `apidoc.tar.gz` from the
//! corresponding GitHub Release. The archive embeds an extracted
//! snapshot of perlapi documentation that the type inferencer
//! consults — pre-extracting it shaves significant time off every
//! consumer's build. Network access is therefore needed once per
//! version; subsequent builds reuse the cached `OUT_DIR` copy.
//! Set `LIBPERL_APIDOC_URL` to override the download URL (e.g. for
//! offline mirrors).
//!
//! ## Status
//!
//! Pre-1.0 — focused on the libperl-rs use case. Wider header-tree
//! coverage and stable APIs come after libperl-rs hits 1.0.

pub mod apidoc;
pub mod apidoc_data;
pub mod apidoc_patches;
pub mod ast;
pub mod c_fn_decl;
pub mod error;
pub mod enum_dict;
pub mod fields_dict;
pub mod global_const_dict;
pub mod infer_api;
pub mod inline_fn;
pub mod intern;
pub mod lexer;
pub mod macro_def;
pub mod macro_infer;
pub mod parser;
pub mod perl_config;
pub mod perlvar_dict;
pub mod perlvar_emitter;
pub mod pipeline;
pub mod pp_expr;
pub mod preprocessor;
pub mod rust_codegen;
pub mod rust_decl;
pub mod static_array_emitter;
pub mod struct_emitter;
pub mod semantic;
pub mod sexp;
pub mod syn_codegen;
pub mod source;
pub mod token;
pub mod token_source;
pub mod type_env;
pub mod type_repr;
pub mod unified_type;

// 主要な型を再エクスポート
pub use apidoc::{
    find_apidoc_dir_from, resolve_apidoc_path,
    ApidocArg, ApidocCollector, ApidocDict, ApidocEntry, ApidocFlags, ApidocResolveError, ApidocStats, Nullability,
};
pub use infer_api::{
    run_inference_with_preprocessor,
    DebugOptions, InferConfig, InferError, InferResult, InferStats, TypedefDict,
};
pub use ast::*;
pub use error::{CompileError, DisplayLocation, LexError, PPError, ParseError, Result};
pub use fields_dict::FieldsDict;
pub use inline_fn::InlineFnDict;
pub use intern::{InternedStr, StringInterner};
pub use rust_decl::RustDeclDict;
pub use lexer::{IdentResolver, Interning, Lexer, LookupOnly, MutableLexer, ReadOnlyLexer};
pub use macro_def::{MacroDef, MacroKind, MacroTable};
pub use macro_infer::{
    convert_assert_calls_in_compound_stmt, detect_assert_kind, InferStatus, MacroInferContext,
    MacroInferInfo, MacroInferStats, NoExpandSymbols, ParseResult,
};
pub use parser::{parse_expression_from_tokens, parse_expression_from_tokens_ref, parse_type_from_string, Parser};
pub use perl_config::{
    build_pp_config_for_perl, get_default_target_dir, get_perl_config, get_perl_version,
    PerlConfig, PerlConfigError,
};
pub use perlvar_dict::{
    ArrayLength, PerlvarCollector, PerlvarDict, PerlvarEntry, PerlvarKind,
};
pub use preprocessor::{
    CallbackPair, CommentCallback, MacroCalledCallback, MacroCallWatcher, MacroDefCallback,
    PPConfig, Preprocessor,
};
pub use semantic::{SemanticAnalyzer, Symbol, SymbolKind, Type};
pub use sexp::{SexpPrinter, TypedSexpPrinter};
pub use source::{FileId, FileRegistry, SourceLocation};
pub use token::{Comment, CommentKind, Token, TokenKind};
pub use token_source::{TokenSlice, TokenSliceRef, TokenSource};
pub use type_env::{ParamLink, TypeConstraint, TypeEnv};
pub use type_repr::{
    CDerivedType, CPrimitiveKind, CTypeSource, CTypeSpecs, InferredType,
    IntSize as TypeReprIntSize, RustPrimitiveKind, RustTypeRepr, RustTypeSource, TypeRepr,
};
pub use unified_type::{IntSize, SourcedType, TypeSource, UnifiedType};
pub use rust_codegen::{CodegenConfig, CodegenDriver, CodegenStats, GeneratedCode, GenerateStatus, RustCodegen};
pub use pipeline::{
    Pipeline, PipelineBuilder, PipelineError,
    PreprocessConfig, InferConfig as PipelineInferConfig, CodegenConfig as PipelineCodegenConfig,
    PreprocessedPipeline, InferredPipeline, GeneratedPipeline,
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_basic_lexer_integration() {
        let source = b"int main(void) { return 0; }";

        let mut files = FileRegistry::new();
        let file_id = files.register(PathBuf::from("test.c"));

        let mut interner = StringInterner::new();
        let mut lexer = Lexer::new(source, file_id, &mut interner);

        let mut tokens = Vec::new();
        loop {
            let token = lexer.next_token().unwrap();
            if matches!(token.kind, TokenKind::Eof) {
                break;
            }
            tokens.push(token);
        }

        // int main ( void ) { return 0 ; }
        assert_eq!(tokens.len(), 10);
        // キーワードはキーワードトークンとして返される
        assert!(matches!(tokens[0].kind, TokenKind::KwInt));
        assert!(matches!(tokens[1].kind, TokenKind::Ident(_)));  // main is identifier
        assert!(matches!(tokens[2].kind, TokenKind::LParen));
        assert!(matches!(tokens[3].kind, TokenKind::KwVoid));
        assert!(matches!(tokens[4].kind, TokenKind::RParen));
        assert!(matches!(tokens[5].kind, TokenKind::LBrace));
        assert!(matches!(tokens[6].kind, TokenKind::KwReturn));
        assert!(matches!(tokens[7].kind, TokenKind::IntLit(0)));
        assert!(matches!(tokens[8].kind, TokenKind::Semi));
        assert!(matches!(tokens[9].kind, TokenKind::RBrace));

        // 識別子の内容を確認
        if let TokenKind::Ident(id) = tokens[1].kind {
            assert_eq!(interner.get(id), "main");
        } else {
            panic!("Expected identifier for 'main'");
        }
    }

    #[test]
    fn test_comment_preservation() {
        let source = b"// doc comment\nint x;";

        let mut files = FileRegistry::new();
        let file_id = files.register(PathBuf::from("test.c"));

        let mut interner = StringInterner::new();
        let mut lexer = Lexer::new(source, file_id, &mut interner);

        // 最初に改行トークンが来る（コメントはその前）
        let newline = lexer.next_token().unwrap();
        assert!(matches!(newline.kind, TokenKind::Newline));
        assert_eq!(newline.leading_comments.len(), 1);
        assert!(newline.leading_comments[0].text.contains("doc comment"));

        let token = lexer.next_token().unwrap();
        // キーワードはキーワードトークンとして返される
        assert!(matches!(token.kind, TokenKind::KwInt));
    }
}
