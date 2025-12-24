//! TinyCC Macro Bindgen
//!
//! C言語のヘッダーファイルからマクロとinline static関数を抽出し、
//! Rustコードに変換するツール。

pub mod error;
pub mod intern;
pub mod lexer;
pub mod macro_def;
pub mod pp_expr;
pub mod preprocessor;
pub mod source;
pub mod token;

// 主要な型を再エクスポート
pub use error::{CompileError, LexError, PPError, ParseError, Result};
pub use intern::{InternedStr, StringInterner};
pub use lexer::Lexer;
pub use macro_def::{MacroDef, MacroKind, MacroTable};
pub use preprocessor::{PPConfig, Preprocessor};
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
        // tinycc方式: キーワードも識別子として返される
        assert!(matches!(tokens[0].kind, TokenKind::Ident(_)));
        assert!(matches!(tokens[1].kind, TokenKind::Ident(_)));
        assert!(matches!(tokens[2].kind, TokenKind::LParen));
        assert!(matches!(tokens[3].kind, TokenKind::Ident(_)));
        assert!(matches!(tokens[4].kind, TokenKind::RParen));
        assert!(matches!(tokens[5].kind, TokenKind::LBrace));
        assert!(matches!(tokens[6].kind, TokenKind::Ident(_)));
        assert!(matches!(tokens[7].kind, TokenKind::IntLit(0)));
        assert!(matches!(tokens[8].kind, TokenKind::Semi));
        assert!(matches!(tokens[9].kind, TokenKind::RBrace));

        // 識別子の内容を確認
        let get_ident = |t: &Token| -> Option<&str> {
            if let TokenKind::Ident(id) = t.kind {
                Some(interner.get(id))
            } else {
                None
            }
        };
        assert_eq!(get_ident(&tokens[0]), Some("int"));
        assert_eq!(get_ident(&tokens[1]), Some("main"));
        assert_eq!(get_ident(&tokens[3]), Some("void"));
        assert_eq!(get_ident(&tokens[6]), Some("return"));
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
        // tinycc方式: キーワードも識別子として返される
        assert!(matches!(token.kind, TokenKind::Ident(_)));
        if let TokenKind::Ident(id) = token.kind {
            assert_eq!(interner.get(id), "int");
        }
    }
}
