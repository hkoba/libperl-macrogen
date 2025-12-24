//! TinyCC Macro Bindgen
//!
//! C言語のヘッダーファイルからマクロとinline static関数を抽出し、
//! Rustコードに変換するツール。

pub mod error;
pub mod intern;
pub mod lexer;
pub mod source;
pub mod token;

// 主要な型を再エクスポート
pub use error::{CompileError, LexError, PPError, ParseError, Result};
pub use intern::{InternedStr, StringInterner};
pub use lexer::Lexer;
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
        assert!(matches!(tokens[0].kind, TokenKind::KwInt));
        assert!(matches!(tokens[1].kind, TokenKind::Ident(_)));
        assert!(matches!(tokens[2].kind, TokenKind::LParen));
        assert!(matches!(tokens[3].kind, TokenKind::KwVoid));
        assert!(matches!(tokens[4].kind, TokenKind::RParen));
        assert!(matches!(tokens[5].kind, TokenKind::LBrace));
        assert!(matches!(tokens[6].kind, TokenKind::KwReturn));
        assert!(matches!(tokens[7].kind, TokenKind::IntLit(0)));
        assert!(matches!(tokens[8].kind, TokenKind::Semi));
        assert!(matches!(tokens[9].kind, TokenKind::RBrace));

        // 識別子 "main" が正しくインターンされていることを確認
        if let TokenKind::Ident(id) = tokens[1].kind {
            assert_eq!(interner.get(id), "main");
        } else {
            panic!("Expected identifier");
        }
    }

    #[test]
    fn test_comment_preservation() {
        let source = b"// doc comment\nint x;";

        let mut files = FileRegistry::new();
        let file_id = files.register(PathBuf::from("test.c"));

        let mut interner = StringInterner::new();
        let mut lexer = Lexer::new(source, file_id, &mut interner);

        let token = lexer.next_token().unwrap();
        assert!(matches!(token.kind, TokenKind::KwInt));
        assert_eq!(token.leading_comments.len(), 1);
        assert!(token.leading_comments[0].text.contains("doc comment"));
    }
}
