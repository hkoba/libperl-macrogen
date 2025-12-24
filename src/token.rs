use crate::intern::InternedStr;
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
