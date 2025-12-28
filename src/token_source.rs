//! トークンソースの抽象化
//!
//! Parser がプリプロセッサ以外のソース（トークン列など）からも
//! トークンを読めるようにするための抽象化層。

use crate::error::Result;
use crate::intern::StringInterner;
use crate::source::{FileRegistry, SourceLocation};
use crate::token::{Token, TokenKind};

/// トークンを供給するソースの抽象化
pub trait TokenSource {
    /// 次のトークンを取得
    fn next_token(&mut self) -> Result<Token>;

    /// StringInterner への参照を取得
    fn interner(&self) -> &StringInterner;

    /// StringInterner への可変参照を取得
    fn interner_mut(&mut self) -> &mut StringInterner;

    /// FileRegistry への参照を取得
    fn files(&self) -> &FileRegistry;
}

/// トークン列からトークンを供給する実装
///
/// マクロ本体などの既存トークン列をパースする際に使用
pub struct TokenSlice {
    tokens: Vec<Token>,
    pos: usize,
    interner: StringInterner,
    files: FileRegistry,
    eof_loc: SourceLocation,
}

impl TokenSlice {
    /// 新しい TokenSlice を作成
    ///
    /// # Arguments
    /// * `tokens` - パースするトークン列
    /// * `interner` - 文字列インターナー（既存のものをクローン）
    /// * `files` - ファイルレジストリ（既存のものをクローン）
    pub fn new(tokens: Vec<Token>, interner: StringInterner, files: FileRegistry) -> Self {
        // EOF用の位置情報を設定
        let eof_loc = tokens.last()
            .map(|t| t.loc.clone())
            .unwrap_or_default();

        Self {
            tokens,
            pos: 0,
            interner,
            files,
            eof_loc,
        }
    }

    /// 現在位置を取得
    pub fn position(&self) -> usize {
        self.pos
    }

    /// 残りトークン数を取得
    pub fn remaining(&self) -> usize {
        self.tokens.len().saturating_sub(self.pos)
    }
}

impl TokenSource for TokenSlice {
    fn next_token(&mut self) -> Result<Token> {
        if self.pos < self.tokens.len() {
            let token = self.tokens[self.pos].clone();
            self.pos += 1;
            Ok(token)
        } else {
            // EOF トークンを返す
            Ok(Token::new(TokenKind::Eof, self.eof_loc.clone()))
        }
    }

    fn interner(&self) -> &StringInterner {
        &self.interner
    }

    fn interner_mut(&mut self) -> &mut StringInterner {
        &mut self.interner
    }

    fn files(&self) -> &FileRegistry {
        &self.files
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_token_slice_empty() {
        let interner = StringInterner::new();
        let mut files = FileRegistry::new();
        files.register(PathBuf::from("test.c"));

        let mut slice = TokenSlice::new(vec![], interner, files);

        let token = slice.next_token().unwrap();
        assert!(matches!(token.kind, TokenKind::Eof));
    }

    #[test]
    fn test_token_slice_tokens() {
        let mut interner = StringInterner::new();
        let mut files = FileRegistry::new();
        let file_id = files.register(PathBuf::from("test.c"));
        let loc = SourceLocation::new(file_id, 1, 1);

        let foo = interner.intern("foo");
        let tokens = vec![
            Token::new(TokenKind::Ident(foo), loc.clone()),
            Token::new(TokenKind::Plus, loc.clone()),
            Token::new(TokenKind::IntLit(42), loc.clone()),
        ];

        let mut slice = TokenSlice::new(tokens, interner, files);

        assert_eq!(slice.remaining(), 3);

        let t1 = slice.next_token().unwrap();
        assert!(matches!(t1.kind, TokenKind::Ident(_)));

        let t2 = slice.next_token().unwrap();
        assert!(matches!(t2.kind, TokenKind::Plus));

        let t3 = slice.next_token().unwrap();
        assert!(matches!(t3.kind, TokenKind::IntLit(42)));

        let t4 = slice.next_token().unwrap();
        assert!(matches!(t4.kind, TokenKind::Eof));

        assert_eq!(slice.remaining(), 0);
    }
}
