use crate::source::{FileRegistry, SourceLocation};
use crate::token::TokenKind;
use std::fmt;
use std::path::PathBuf;

/// エラー表示用のロケーション（ファイル名解決付き）
pub struct DisplayLocation<'a> {
    pub loc: &'a SourceLocation,
    pub files: &'a FileRegistry,
}

impl<'a> fmt::Display for DisplayLocation<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let path = self.files.get_path(self.loc.file_id);
        write!(f, "{}:{}:{}", path.display(), self.loc.line, self.loc.column)
    }
}

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
    /// 未知の識別子（ReadOnly モードで intern 済みでない識別子を検出）
    UnknownIdentifier(String),
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
            LexError::UnknownIdentifier(s) => write!(f, "unknown identifier: {}", s),
        }
    }
}

/// プリプロセッサエラー
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
    /// ファイル読み込みエラー
    IoError(PathBuf, String),
    /// #if の条件式エラー
    InvalidCondition(String),
    /// 不正な#演算子の使用
    InvalidStringize,
    /// 不正な##演算子の使用
    InvalidTokenPaste,
    /// 再帰的マクロ展開の検出
    RecursiveMacro(String),
    /// 対応する#elseがない
    UnmatchedElse,
    /// #elifが#elseの後に出現
    ElifAfterElse,
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
            PPError::IoError(p, e) => write!(f, "I/O error reading {}: {}", p.display(), e),
            PPError::InvalidCondition(s) => write!(f, "invalid preprocessor condition: {}", s),
            PPError::InvalidStringize => write!(f, "'#' is not followed by a macro parameter"),
            PPError::InvalidTokenPaste => write!(f, "'##' cannot appear at boundary of macro expansion"),
            PPError::RecursiveMacro(s) => write!(f, "recursive macro expansion: {}", s),
            PPError::UnmatchedElse => write!(f, "#else without matching #if"),
            PPError::ElifAfterElse => write!(f, "#elif after #else"),
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
    /// assert マクロの引数数が不正
    InvalidAssertArgs { macro_name: String, arg_count: usize },
    /// assert マクロがオブジェクトマクロとして呼ばれた
    AssertNotFunctionMacro { macro_name: String },
    /// 入れ子の assert はサポートされない
    NestedAssertNotSupported,
    /// MacroEnd が見つからない
    MacroEndNotFound,
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
            ParseError::InvalidAssertArgs { macro_name, arg_count } => {
                write!(f, "assert macro '{}' expects 1 argument, got {}", macro_name, arg_count)
            }
            ParseError::AssertNotFunctionMacro { macro_name } => {
                write!(f, "assert macro '{}' must be called as function macro", macro_name)
            }
            ParseError::NestedAssertNotSupported => {
                write!(f, "nested assert macros are not supported")
            }
            ParseError::MacroEndNotFound => {
                write!(f, "matching MacroEnd not found")
            }
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

impl CompileError {
    /// エラーが発生した位置を取得
    pub fn loc(&self) -> &SourceLocation {
        match self {
            CompileError::Lex { loc, .. } => loc,
            CompileError::Preprocess { loc, .. } => loc,
            CompileError::Parse { loc, .. } => loc,
        }
    }

    /// ファイル名を解決してエラーメッセージをフォーマット
    pub fn format_with_files(&self, files: &FileRegistry) -> String {
        match self {
            CompileError::Lex { loc, kind } => {
                let disp = DisplayLocation { loc, files };
                format!("{}: lexer error: {}", disp, kind)
            }
            CompileError::Preprocess { loc, kind } => {
                let disp = DisplayLocation { loc, files };
                format!("{}: preprocessor error: {}", disp, kind)
            }
            CompileError::Parse { loc, kind } => {
                let disp = DisplayLocation { loc, files };
                format!("{}: parse error: {}", disp, kind)
            }
        }
    }
}

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
