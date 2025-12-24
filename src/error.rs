use crate::source::SourceLocation;
use crate::token::TokenKind;
use std::fmt;
use std::path::PathBuf;

/// レキサーエラー
#[derive(Debug)]
pub enum LexError {
    /// 閉じられていないブロックコメント
    UnterminatedComment,
    /// 閉じられていない文字列リテラル
    UnterminatedString,
    /// 閉じられていない文字リテラル
    UnterminatedChar,
    /// 空の文字リテラル
    EmptyCharLit,
    /// 不正な文字
    InvalidChar(char),
    /// 不正なエスケープシーケンス
    InvalidEscape(char),
    /// 不正な数値リテラル
    InvalidNumber(String),
    /// 不正なサフィックス
    InvalidSuffix(String),
}

impl fmt::Display for LexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LexError::UnterminatedComment => write!(f, "unterminated block comment"),
            LexError::UnterminatedString => write!(f, "unterminated string literal"),
            LexError::UnterminatedChar => write!(f, "unterminated character literal"),
            LexError::EmptyCharLit => write!(f, "empty character literal"),
            LexError::InvalidChar(c) => write!(f, "invalid character: {:?}", c),
            LexError::InvalidEscape(c) => write!(f, "invalid escape sequence: \\{}", c),
            LexError::InvalidNumber(s) => write!(f, "invalid number: {}", s),
            LexError::InvalidSuffix(s) => write!(f, "invalid suffix: {}", s),
        }
    }
}

/// プリプロセッサエラー（Phase 2 で拡張）
#[derive(Debug)]
pub enum PPError {
    /// 不正なディレクティブ
    InvalidDirective(String),
    /// マクロの再定義
    MacroRedefinition(String),
    /// インクルードファイルが見つからない
    IncludeNotFound(PathBuf),
    /// 対応する#ifがない#endif
    UnmatchedEndif,
    /// 対応する#endifがない
    MissingEndif,
    /// 不正なマクロ引数
    InvalidMacroArgs(String),
}

impl fmt::Display for PPError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PPError::InvalidDirective(s) => write!(f, "invalid directive: #{}", s),
            PPError::MacroRedefinition(s) => write!(f, "macro redefinition: {}", s),
            PPError::IncludeNotFound(p) => write!(f, "include file not found: {}", p.display()),
            PPError::UnmatchedEndif => write!(f, "#endif without matching #if"),
            PPError::MissingEndif => write!(f, "missing #endif"),
            PPError::InvalidMacroArgs(s) => write!(f, "invalid macro arguments: {}", s),
        }
    }
}

/// パースエラー（Phase 3 で拡張）
#[derive(Debug)]
pub enum ParseError {
    /// 予期しないトークン
    UnexpectedToken { expected: String, found: TokenKind },
    /// 予期しないファイル終端
    UnexpectedEof,
    /// 宣言エラー
    InvalidDeclaration(String),
    /// 型エラー
    InvalidType(String),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::UnexpectedToken { expected, found } => {
                write!(f, "expected {}, found {:?}", expected, found)
            }
            ParseError::UnexpectedEof => write!(f, "unexpected end of file"),
            ParseError::InvalidDeclaration(s) => write!(f, "invalid declaration: {}", s),
            ParseError::InvalidType(s) => write!(f, "invalid type: {}", s),
        }
    }
}

/// 統合エラー型
#[derive(Debug)]
pub enum CompileError {
    /// レキサーエラー
    Lex { loc: SourceLocation, kind: LexError },
    /// プリプロセッサエラー
    Preprocess { loc: SourceLocation, kind: PPError },
    /// パースエラー
    Parse { loc: SourceLocation, kind: ParseError },
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CompileError::Lex { loc, kind } => {
                write!(f, "{}:{}:{}: lexer error: {}", loc.file_id.as_u32(), loc.line, loc.column, kind)
            }
            CompileError::Preprocess { loc, kind } => {
                write!(f, "{}:{}:{}: preprocessor error: {}", loc.file_id.as_u32(), loc.line, loc.column, kind)
            }
            CompileError::Parse { loc, kind } => {
                write!(f, "{}:{}:{}: parse error: {}", loc.file_id.as_u32(), loc.line, loc.column, kind)
            }
        }
    }
}

impl std::error::Error for CompileError {}

/// Result型エイリアス
pub type Result<T> = std::result::Result<T, CompileError>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::FileId;

    #[test]
    fn test_lex_error_display() {
        let err = LexError::UnterminatedComment;
        assert_eq!(format!("{}", err), "unterminated block comment");
    }

    #[test]
    fn test_compile_error_display() {
        let loc = SourceLocation::new(FileId::default(), 10, 5);
        let err = CompileError::Lex {
            loc,
            kind: LexError::InvalidChar('$'),
        };
        assert!(format!("{}", err).contains("invalid character"));
    }
}
