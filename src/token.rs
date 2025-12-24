use crate::intern::{InternedStr, StringInterner};
use crate::source::SourceLocation;

/// コメント種別
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommentKind {
    /// 行コメント: // ...
    Line,
    /// ブロックコメント: /* ... */
    Block,
}

/// コメント
#[derive(Debug, Clone)]
pub struct Comment {
    pub kind: CommentKind,
    pub text: String,
    pub loc: SourceLocation,
}

impl Comment {
    /// 新しいコメントを作成
    pub fn new(kind: CommentKind, text: String, loc: SourceLocation) -> Self {
        Self { kind, text, loc }
    }

    /// ドキュメンテーションコメントかどうか（将来の拡張用、現在は常にfalse）
    pub fn is_doc(&self) -> bool {
        false
    }
}

/// トークン種別
#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // === リテラル ===
    /// 整数リテラル
    IntLit(i64),
    /// 符号なし整数リテラル
    UIntLit(u64),
    /// 浮動小数点リテラル
    FloatLit(f64),
    /// 文字リテラル
    CharLit(u8),
    /// ワイド文字リテラル
    WideCharLit(u32),
    /// 文字列リテラル
    StringLit(Vec<u8>),
    /// ワイド文字列リテラル
    WideStringLit(Vec<u32>),

    // === 識別子 ===
    Ident(InternedStr),

    // === キーワード ===
    // ストレージクラス
    KwAuto,
    KwExtern,
    KwRegister,
    KwStatic,
    KwTypedef,
    // 型指定子
    KwChar,
    KwDouble,
    KwFloat,
    KwInt,
    KwLong,
    KwShort,
    KwSigned,
    KwUnsigned,
    KwVoid,
    // 型修飾子
    KwConst,
    KwVolatile,
    KwRestrict,
    // 構造体・共用体・列挙
    KwStruct,
    KwUnion,
    KwEnum,
    // 制御フロー
    KwBreak,
    KwCase,
    KwContinue,
    KwDefault,
    KwDo,
    KwElse,
    KwFor,
    KwGoto,
    KwIf,
    KwReturn,
    KwSwitch,
    KwWhile,
    // その他
    KwInline,
    KwSizeof,
    // C99
    KwBool,
    KwComplex,
    KwImaginary,
    // C11
    KwAlignas,
    KwAlignof,
    KwAtomic,
    KwGeneric,
    KwNoreturn,
    KwStaticAssert,
    KwThreadLocal,

    // === 演算子 ===
    // 算術
    Plus,       // +
    Minus,      // -
    Star,       // *
    Slash,      // /
    Percent,    // %
    // ビット演算
    Amp,        // &
    Pipe,       // |
    Caret,      // ^
    Tilde,      // ~
    LtLt,       // <<
    GtGt,       // >>
    // 論理演算
    Bang,       // !
    AmpAmp,     // &&
    PipePipe,   // ||
    // 比較
    Lt,         // <
    Gt,         // >
    LtEq,       // <=
    GtEq,       // >=
    EqEq,       // ==
    BangEq,     // !=
    // 代入
    Eq,         // =
    PlusEq,     // +=
    MinusEq,    // -=
    StarEq,     // *=
    SlashEq,    // /=
    PercentEq,  // %=
    AmpEq,      // &=
    PipeEq,     // |=
    CaretEq,    // ^=
    LtLtEq,     // <<=
    GtGtEq,     // >>=
    // インクリメント・デクリメント
    PlusPlus,   // ++
    MinusMinus, // --
    // その他演算子
    Question,   // ?
    Colon,      // :
    Arrow,      // ->
    Dot,        // .
    Ellipsis,   // ...

    // === 区切り記号 ===
    Comma,      // ,
    Semi,       // ;
    LParen,     // (
    RParen,     // )
    LBracket,   // [
    RBracket,   // ]
    LBrace,     // {
    RBrace,     // }

    // === プリプロセッサ用 ===
    Hash,       // #
    HashHash,   // ##

    // === 特殊 ===
    /// ファイル終端
    Eof,
    /// 改行（プリプロセッサ用）
    Newline,
    /// 空白（スペース/タブ）- PARSE_FLAG_SPACES モード用
    Space,
}

impl TokenKind {
    /// キーワード文字列からTokenKindへの変換
    pub fn from_keyword(s: &str) -> Option<TokenKind> {
        match s {
            // ストレージクラス
            "auto" => Some(TokenKind::KwAuto),
            "extern" => Some(TokenKind::KwExtern),
            "register" => Some(TokenKind::KwRegister),
            "static" => Some(TokenKind::KwStatic),
            "typedef" => Some(TokenKind::KwTypedef),
            // 型指定子
            "char" => Some(TokenKind::KwChar),
            "double" => Some(TokenKind::KwDouble),
            "float" => Some(TokenKind::KwFloat),
            "int" => Some(TokenKind::KwInt),
            "long" => Some(TokenKind::KwLong),
            "short" => Some(TokenKind::KwShort),
            "signed" => Some(TokenKind::KwSigned),
            "unsigned" => Some(TokenKind::KwUnsigned),
            "void" => Some(TokenKind::KwVoid),
            // 型修飾子
            "const" => Some(TokenKind::KwConst),
            "volatile" => Some(TokenKind::KwVolatile),
            "restrict" => Some(TokenKind::KwRestrict),
            // 構造体・共用体・列挙
            "struct" => Some(TokenKind::KwStruct),
            "union" => Some(TokenKind::KwUnion),
            "enum" => Some(TokenKind::KwEnum),
            // 制御フロー
            "break" => Some(TokenKind::KwBreak),
            "case" => Some(TokenKind::KwCase),
            "continue" => Some(TokenKind::KwContinue),
            "default" => Some(TokenKind::KwDefault),
            "do" => Some(TokenKind::KwDo),
            "else" => Some(TokenKind::KwElse),
            "for" => Some(TokenKind::KwFor),
            "goto" => Some(TokenKind::KwGoto),
            "if" => Some(TokenKind::KwIf),
            "return" => Some(TokenKind::KwReturn),
            "switch" => Some(TokenKind::KwSwitch),
            "while" => Some(TokenKind::KwWhile),
            // その他
            "inline" => Some(TokenKind::KwInline),
            "__inline" => Some(TokenKind::KwInline),
            "__inline__" => Some(TokenKind::KwInline),
            "sizeof" => Some(TokenKind::KwSizeof),
            // C99
            "_Bool" => Some(TokenKind::KwBool),
            "_Complex" => Some(TokenKind::KwComplex),
            "_Imaginary" => Some(TokenKind::KwImaginary),
            // C11
            "_Alignas" => Some(TokenKind::KwAlignas),
            "_Alignof" => Some(TokenKind::KwAlignof),
            "_Atomic" => Some(TokenKind::KwAtomic),
            "_Generic" => Some(TokenKind::KwGeneric),
            "_Noreturn" => Some(TokenKind::KwNoreturn),
            "_Static_assert" => Some(TokenKind::KwStaticAssert),
            "_Thread_local" => Some(TokenKind::KwThreadLocal),
            _ => None,
        }
    }

    /// トークンを文字列に変換
    pub fn format(&self, interner: &StringInterner) -> String {
        match self {
            // リテラル
            TokenKind::Ident(id) => interner.get(*id).to_string(),
            TokenKind::IntLit(n) => n.to_string(),
            TokenKind::UIntLit(n) => format!("{}u", n),
            TokenKind::FloatLit(f) => f.to_string(),
            TokenKind::CharLit(c) => format!("'{}'", escape_char(*c)),
            TokenKind::WideCharLit(c) => format!("L'{}'", escape_wide_char(*c)),
            TokenKind::StringLit(s) => format!("\"{}\"", escape_string(s)),
            TokenKind::WideStringLit(s) => format!("L\"{}\"", escape_wide_string(s)),
            // キーワード
            TokenKind::KwAuto => "auto".to_string(),
            TokenKind::KwExtern => "extern".to_string(),
            TokenKind::KwRegister => "register".to_string(),
            TokenKind::KwStatic => "static".to_string(),
            TokenKind::KwTypedef => "typedef".to_string(),
            TokenKind::KwChar => "char".to_string(),
            TokenKind::KwDouble => "double".to_string(),
            TokenKind::KwFloat => "float".to_string(),
            TokenKind::KwInt => "int".to_string(),
            TokenKind::KwLong => "long".to_string(),
            TokenKind::KwShort => "short".to_string(),
            TokenKind::KwSigned => "signed".to_string(),
            TokenKind::KwUnsigned => "unsigned".to_string(),
            TokenKind::KwVoid => "void".to_string(),
            TokenKind::KwConst => "const".to_string(),
            TokenKind::KwVolatile => "volatile".to_string(),
            TokenKind::KwRestrict => "restrict".to_string(),
            TokenKind::KwStruct => "struct".to_string(),
            TokenKind::KwUnion => "union".to_string(),
            TokenKind::KwEnum => "enum".to_string(),
            TokenKind::KwBreak => "break".to_string(),
            TokenKind::KwCase => "case".to_string(),
            TokenKind::KwContinue => "continue".to_string(),
            TokenKind::KwDefault => "default".to_string(),
            TokenKind::KwDo => "do".to_string(),
            TokenKind::KwElse => "else".to_string(),
            TokenKind::KwFor => "for".to_string(),
            TokenKind::KwGoto => "goto".to_string(),
            TokenKind::KwIf => "if".to_string(),
            TokenKind::KwReturn => "return".to_string(),
            TokenKind::KwSwitch => "switch".to_string(),
            TokenKind::KwWhile => "while".to_string(),
            TokenKind::KwInline => "inline".to_string(),
            TokenKind::KwSizeof => "sizeof".to_string(),
            TokenKind::KwBool => "_Bool".to_string(),
            TokenKind::KwComplex => "_Complex".to_string(),
            TokenKind::KwImaginary => "_Imaginary".to_string(),
            TokenKind::KwAlignas => "_Alignas".to_string(),
            TokenKind::KwAlignof => "_Alignof".to_string(),
            TokenKind::KwAtomic => "_Atomic".to_string(),
            TokenKind::KwGeneric => "_Generic".to_string(),
            TokenKind::KwNoreturn => "_Noreturn".to_string(),
            TokenKind::KwStaticAssert => "_Static_assert".to_string(),
            TokenKind::KwThreadLocal => "_Thread_local".to_string(),
            // 演算子
            TokenKind::Plus => "+".to_string(),
            TokenKind::Minus => "-".to_string(),
            TokenKind::Star => "*".to_string(),
            TokenKind::Slash => "/".to_string(),
            TokenKind::Percent => "%".to_string(),
            TokenKind::Amp => "&".to_string(),
            TokenKind::Pipe => "|".to_string(),
            TokenKind::Caret => "^".to_string(),
            TokenKind::Tilde => "~".to_string(),
            TokenKind::LtLt => "<<".to_string(),
            TokenKind::GtGt => ">>".to_string(),
            TokenKind::Bang => "!".to_string(),
            TokenKind::AmpAmp => "&&".to_string(),
            TokenKind::PipePipe => "||".to_string(),
            TokenKind::Lt => "<".to_string(),
            TokenKind::Gt => ">".to_string(),
            TokenKind::LtEq => "<=".to_string(),
            TokenKind::GtEq => ">=".to_string(),
            TokenKind::EqEq => "==".to_string(),
            TokenKind::BangEq => "!=".to_string(),
            TokenKind::Eq => "=".to_string(),
            TokenKind::PlusEq => "+=".to_string(),
            TokenKind::MinusEq => "-=".to_string(),
            TokenKind::StarEq => "*=".to_string(),
            TokenKind::SlashEq => "/=".to_string(),
            TokenKind::PercentEq => "%=".to_string(),
            TokenKind::AmpEq => "&=".to_string(),
            TokenKind::PipeEq => "|=".to_string(),
            TokenKind::CaretEq => "^=".to_string(),
            TokenKind::LtLtEq => "<<=".to_string(),
            TokenKind::GtGtEq => ">>=".to_string(),
            TokenKind::PlusPlus => "++".to_string(),
            TokenKind::MinusMinus => "--".to_string(),
            TokenKind::Question => "?".to_string(),
            TokenKind::Colon => ":".to_string(),
            TokenKind::Arrow => "->".to_string(),
            TokenKind::Dot => ".".to_string(),
            TokenKind::Ellipsis => "...".to_string(),
            // 区切り記号
            TokenKind::Comma => ",".to_string(),
            TokenKind::Semi => ";".to_string(),
            TokenKind::LParen => "(".to_string(),
            TokenKind::RParen => ")".to_string(),
            TokenKind::LBracket => "[".to_string(),
            TokenKind::RBracket => "]".to_string(),
            TokenKind::LBrace => "{".to_string(),
            TokenKind::RBrace => "}".to_string(),
            // プリプロセッサ用
            TokenKind::Hash => "#".to_string(),
            TokenKind::HashHash => "##".to_string(),
            // 特殊
            TokenKind::Newline => "\n".to_string(),
            TokenKind::Eof => "".to_string(),
            TokenKind::Space => " ".to_string(),
        }
    }
}

/// 文字をエスケープ
fn escape_char(c: u8) -> String {
    match c {
        b'\n' => "\\n".to_string(),
        b'\r' => "\\r".to_string(),
        b'\t' => "\\t".to_string(),
        b'\\' => "\\\\".to_string(),
        b'\'' => "\\'".to_string(),
        c if c.is_ascii_graphic() || c == b' ' => (c as char).to_string(),
        c => format!("\\x{:02x}", c),
    }
}

/// ワイド文字をエスケープ
fn escape_wide_char(c: u32) -> String {
    if let Some(ch) = char::from_u32(c) {
        match ch {
            '\n' => "\\n".to_string(),
            '\r' => "\\r".to_string(),
            '\t' => "\\t".to_string(),
            '\\' => "\\\\".to_string(),
            '\'' => "\\'".to_string(),
            c if c.is_ascii_graphic() || c == ' ' => c.to_string(),
            c if c as u32 <= 0xFFFF => format!("\\u{:04x}", c as u32),
            c => format!("\\U{:08x}", c as u32),
        }
    } else {
        format!("\\U{:08x}", c)
    }
}

/// 文字列をエスケープ
fn escape_string(s: &[u8]) -> String {
    s.iter().map(|&c| escape_char(c)).collect()
}

/// ワイド文字列をエスケープ
fn escape_wide_string(s: &[u32]) -> String {
    s.iter().map(|&c| escape_wide_char(c)).collect()
}

/// 位置情報付きトークン
#[derive(Debug, Clone)]
pub struct Token {
    pub kind: TokenKind,
    pub loc: SourceLocation,
    /// このトークンの直前にあったコメント群
    pub leading_comments: Vec<Comment>,
}

impl Token {
    /// 新しいトークンを作成
    pub fn new(kind: TokenKind, loc: SourceLocation) -> Self {
        Self {
            kind,
            loc,
            leading_comments: Vec::new(),
        }
    }

    /// コメント付きでトークンを作成
    pub fn with_comments(kind: TokenKind, loc: SourceLocation, comments: Vec<Comment>) -> Self {
        Self {
            kind,
            loc,
            leading_comments: comments,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keyword_lookup() {
        assert_eq!(TokenKind::from_keyword("int"), Some(TokenKind::KwInt));
        assert_eq!(TokenKind::from_keyword("if"), Some(TokenKind::KwIf));
        assert_eq!(TokenKind::from_keyword("foo"), None);
    }

    #[test]
    fn test_inline_variants() {
        assert_eq!(TokenKind::from_keyword("inline"), Some(TokenKind::KwInline));
        assert_eq!(TokenKind::from_keyword("__inline"), Some(TokenKind::KwInline));
        assert_eq!(TokenKind::from_keyword("__inline__"), Some(TokenKind::KwInline));
    }
}
