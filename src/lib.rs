//! TinyCC Macro Bindgen
//!
//! C言語のヘッダーファイルからマクロとinline static関数を抽出し、
//! Rustコードに変換するツール。

pub mod ast;
pub mod error;
pub mod fields_dict;
pub mod intern;
pub mod lexer;
pub mod macro_def;
pub mod parser;
pub mod perl_config;
pub mod pp_expr;
pub mod preprocessor;
pub mod rust_decl;
pub mod semantic;
pub mod sexp;
pub mod source;
pub mod token;

// 主要な型を再エクスポート
pub use ast::*;
pub use error::{CompileError, DisplayLocation, LexError, PPError, ParseError, Result};
pub use fields_dict::FieldsDict;
pub use intern::{InternedStr, StringInterner};
pub use rust_decl::RustDeclDict;
pub use lexer::Lexer;
pub use macro_def::{MacroDef, MacroKind, MacroTable};
pub use parser::Parser;
pub use perl_config::{get_perl_config, PerlConfig, PerlConfigError};
pub use preprocessor::{PPConfig, Preprocessor};
pub use semantic::{SemanticAnalyzer, Symbol, SymbolKind, Type};
pub use sexp::{SexpPrinter, TypedSexpPrinter};
pub use source::{FileId, FileRegistry, SourceLocation};
pub use token::{Comment, CommentKind, Token, TokenKind};

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
