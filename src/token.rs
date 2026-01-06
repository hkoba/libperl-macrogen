use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::intern::{InternedStr, StringInterner};
use crate::source::SourceLocation;

// ============================================================================
// TokenId - トークンの一意識別子
// ============================================================================

/// トークンID（一意の通し番号）
///
/// 各トークンに一意のIDを付与することで、マクロ展開の追跡や
/// 展開禁止情報の管理に使用する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct TokenId(pub u64);

/// TokenId 生成用のグローバルカウンター
static TOKEN_ID_COUNTER: AtomicU64 = AtomicU64::new(1); // 0 は無効値として予約

impl TokenId {
    /// 新しい一意のIDを生成
    pub fn next() -> Self {
        Self(TOKEN_ID_COUNTER.fetch_add(1, Ordering::Relaxed))
    }

    /// 無効なID（ペアリングや初期化用）
    pub const INVALID: Self = Self(0);

    /// IDが有効かどうか
    pub fn is_valid(&self) -> bool {
        self.0 != 0
    }
}

impl std::fmt::Display for TokenId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TokenId({})", self.0)
    }
}

// ============================================================================
// マクロ展開マーカー
// ============================================================================

/// マクロ呼び出しの種類
#[derive(Debug, Clone, PartialEq)]
pub enum MacroInvocationKind {
    /// オブジェクトマクロ（引数なし）
    Object,
    /// 関数マクロ（引数あり）
    Function {
        /// 引数のトークン列（展開前の生トークン）
        args: Vec<Vec<Token>>,
    },
}

/// マクロ展開開始マーカーの情報
///
/// マクロ展開の開始位置を示し、展開元の情報を保持する。
/// パーサーはこのマーカーを透過的に処理し、ASTにマクロ情報を付与する。
#[derive(Debug, Clone, PartialEq)]
pub struct MacroBeginInfo {
    /// このマーカーのID（MacroEnd との対応付け用）
    pub marker_id: TokenId,
    /// 展開を引き起こしたトークンのID
    pub trigger_token_id: TokenId,
    /// マクロ名
    pub macro_name: InternedStr,
    /// マクロの種類と引数
    pub kind: MacroInvocationKind,
    /// 展開が発生した位置（マクロ呼び出し位置）
    pub call_loc: SourceLocation,
}

/// マクロ展開終了マーカーの情報
///
/// 対応する MacroBegin との対を形成する。
#[derive(Debug, Clone, PartialEq)]
pub struct MacroEndInfo {
    /// 対応する MacroBegin のマーカーID
    pub begin_marker_id: TokenId,
}

// ============================================================================
// Comment
// ============================================================================

/// コメント種別
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommentKind {
    /// 行コメント: // ...
    Line,
    /// ブロックコメント: /* ... */
    Block,
}

/// コメント
#[derive(Debug, Clone, PartialEq)]
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
    // GCC拡張浮動小数点型
    KwFloat16,
    KwFloat32,
    KwFloat64,
    KwFloat128,
    KwFloat32x,
    KwFloat64x,
    // GCC拡張: 128ビット整数
    KwInt128,

    // === GCC拡張キーワード（別名） ===
    // inline variants
    KwInline2,      // __inline
    KwInline3,      // __inline__
    // signed variants
    KwSigned2,      // __signed__
    // const variants
    KwConst2,       // __const
    KwConst3,       // __const__
    // volatile variants
    KwVolatile2,    // __volatile
    KwVolatile3,    // __volatile__
    // restrict variants
    KwRestrict2,    // __restrict
    KwRestrict3,    // __restrict__
    // bool variant (C23)
    KwBool2,        // bool
    // alignof variants
    KwAlignof2,     // __alignof
    KwAlignof3,     // __alignof__
    // typeof
    KwTypeof,       // typeof (C23)
    KwTypeof2,      // __typeof
    KwTypeof3,      // __typeof__
    // attribute
    KwAttribute,    // __attribute
    KwAttribute2,   // __attribute__
    // asm
    KwAsm,          // asm
    KwAsm2,         // __asm
    KwAsm3,         // __asm__
    // __extension__
    KwExtension,
    // __thread (GCC style thread-local)
    KwThread,

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
    Backslash,  // \ (インラインアセンブリマクロなどで使用)

    // === 特殊 ===
    /// ファイル終端
    Eof,
    /// 改行（プリプロセッサ用）
    Newline,
    /// 空白（スペース/タブ）- PARSE_FLAG_SPACES モード用
    Space,

    // === マクロ展開マーカー ===
    /// マクロ展開開始マーカー
    ///
    /// プリプロセッサがマクロ展開時に挿入し、パーサーが透過的に処理する。
    /// AST構築時にマクロ展開情報を付与するために使用。
    MacroBegin(Box<MacroBeginInfo>),
    /// マクロ展開終了マーカー
    ///
    /// 対応する MacroBegin と対を形成する。
    MacroEnd(MacroEndInfo),
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
            "__inline" => Some(TokenKind::KwInline2),
            "__inline__" => Some(TokenKind::KwInline3),
            "sizeof" => Some(TokenKind::KwSizeof),
            // C99
            "_Bool" => Some(TokenKind::KwBool),
            "bool" => Some(TokenKind::KwBool2),
            "_Complex" => Some(TokenKind::KwComplex),
            "_Imaginary" => Some(TokenKind::KwImaginary),
            // C11
            "_Alignas" => Some(TokenKind::KwAlignas),
            "_Alignof" => Some(TokenKind::KwAlignof),
            "__alignof" => Some(TokenKind::KwAlignof2),
            "__alignof__" => Some(TokenKind::KwAlignof3),
            "_Atomic" => Some(TokenKind::KwAtomic),
            "_Generic" => Some(TokenKind::KwGeneric),
            "_Noreturn" => Some(TokenKind::KwNoreturn),
            "_Static_assert" => Some(TokenKind::KwStaticAssert),
            "_Thread_local" => Some(TokenKind::KwThreadLocal),
            "__thread" => Some(TokenKind::KwThread),
            // GCC拡張浮動小数点型
            "_Float16" => Some(TokenKind::KwFloat16),
            "_Float32" => Some(TokenKind::KwFloat32),
            "_Float64" => Some(TokenKind::KwFloat64),
            "_Float128" => Some(TokenKind::KwFloat128),
            "_Float32x" => Some(TokenKind::KwFloat32x),
            "_Float64x" => Some(TokenKind::KwFloat64x),
            // GCC拡張: 128ビット整数
            "__int128" => Some(TokenKind::KwInt128),
            // GCC拡張: signed
            "__signed__" => Some(TokenKind::KwSigned2),
            // GCC拡張: const
            "__const" => Some(TokenKind::KwConst2),
            "__const__" => Some(TokenKind::KwConst3),
            // GCC拡張: volatile
            "__volatile" => Some(TokenKind::KwVolatile2),
            "__volatile__" => Some(TokenKind::KwVolatile3),
            // GCC拡張: restrict
            "__restrict" => Some(TokenKind::KwRestrict2),
            "__restrict__" => Some(TokenKind::KwRestrict3),
            // GCC拡張: typeof
            "typeof" => Some(TokenKind::KwTypeof),
            "__typeof" => Some(TokenKind::KwTypeof2),
            "__typeof__" => Some(TokenKind::KwTypeof3),
            // GCC拡張: attribute
            "__attribute" => Some(TokenKind::KwAttribute),
            "__attribute__" => Some(TokenKind::KwAttribute2),
            // GCC拡張: asm
            "asm" => Some(TokenKind::KwAsm),
            "__asm" => Some(TokenKind::KwAsm2),
            "__asm__" => Some(TokenKind::KwAsm3),
            // GCC拡張: extension
            "__extension__" => Some(TokenKind::KwExtension),
            _ => None,
        }
    }

    /// キーワードトークンかどうかを判定
    pub fn is_keyword(&self) -> bool {
        matches!(
            self,
            TokenKind::KwAuto
                | TokenKind::KwBreak
                | TokenKind::KwCase
                | TokenKind::KwChar
                | TokenKind::KwConst
                | TokenKind::KwConst2
                | TokenKind::KwConst3
                | TokenKind::KwContinue
                | TokenKind::KwDefault
                | TokenKind::KwDo
                | TokenKind::KwDouble
                | TokenKind::KwElse
                | TokenKind::KwEnum
                | TokenKind::KwExtern
                | TokenKind::KwFloat
                | TokenKind::KwFor
                | TokenKind::KwGoto
                | TokenKind::KwIf
                | TokenKind::KwInline
                | TokenKind::KwInline2
                | TokenKind::KwInline3
                | TokenKind::KwInt
                | TokenKind::KwLong
                | TokenKind::KwRegister
                | TokenKind::KwRestrict
                | TokenKind::KwRestrict2
                | TokenKind::KwRestrict3
                | TokenKind::KwReturn
                | TokenKind::KwShort
                | TokenKind::KwSigned
                | TokenKind::KwSigned2
                | TokenKind::KwSizeof
                | TokenKind::KwStatic
                | TokenKind::KwStruct
                | TokenKind::KwSwitch
                | TokenKind::KwTypedef
                | TokenKind::KwUnion
                | TokenKind::KwUnsigned
                | TokenKind::KwVoid
                | TokenKind::KwVolatile
                | TokenKind::KwVolatile2
                | TokenKind::KwVolatile3
                | TokenKind::KwWhile
                | TokenKind::KwBool
                | TokenKind::KwBool2
                | TokenKind::KwComplex
                | TokenKind::KwImaginary
                | TokenKind::KwAlignas
                | TokenKind::KwAlignof
                | TokenKind::KwAlignof2
                | TokenKind::KwAlignof3
                | TokenKind::KwAtomic
                | TokenKind::KwGeneric
                | TokenKind::KwNoreturn
                | TokenKind::KwStaticAssert
                | TokenKind::KwThreadLocal
                | TokenKind::KwTypeof
                | TokenKind::KwTypeof2
                | TokenKind::KwTypeof3
                | TokenKind::KwAttribute
                | TokenKind::KwAttribute2
                | TokenKind::KwAsm
                | TokenKind::KwAsm2
                | TokenKind::KwAsm3
                | TokenKind::KwExtension
                | TokenKind::KwThread
                | TokenKind::KwInt128
        )
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
            TokenKind::KwFloat16 => "_Float16".to_string(),
            TokenKind::KwFloat32 => "_Float32".to_string(),
            TokenKind::KwFloat64 => "_Float64".to_string(),
            TokenKind::KwFloat128 => "_Float128".to_string(),
            TokenKind::KwFloat32x => "_Float32x".to_string(),
            TokenKind::KwFloat64x => "_Float64x".to_string(),
            // GCC拡張キーワード（別名）
            TokenKind::KwInline2 => "__inline".to_string(),
            TokenKind::KwInline3 => "__inline__".to_string(),
            TokenKind::KwSigned2 => "__signed__".to_string(),
            TokenKind::KwConst2 => "__const".to_string(),
            TokenKind::KwConst3 => "__const__".to_string(),
            TokenKind::KwVolatile2 => "__volatile".to_string(),
            TokenKind::KwVolatile3 => "__volatile__".to_string(),
            TokenKind::KwRestrict2 => "__restrict".to_string(),
            TokenKind::KwRestrict3 => "__restrict__".to_string(),
            TokenKind::KwBool2 => "bool".to_string(),
            TokenKind::KwAlignof2 => "__alignof".to_string(),
            TokenKind::KwAlignof3 => "__alignof__".to_string(),
            TokenKind::KwTypeof => "typeof".to_string(),
            TokenKind::KwTypeof2 => "__typeof".to_string(),
            TokenKind::KwTypeof3 => "__typeof__".to_string(),
            TokenKind::KwAttribute => "__attribute".to_string(),
            TokenKind::KwAttribute2 => "__attribute__".to_string(),
            TokenKind::KwAsm => "asm".to_string(),
            TokenKind::KwAsm2 => "__asm".to_string(),
            TokenKind::KwAsm3 => "__asm__".to_string(),
            TokenKind::KwExtension => "__extension__".to_string(),
            TokenKind::KwThread => "__thread".to_string(),
            TokenKind::KwInt128 => "__int128".to_string(),
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
            TokenKind::Backslash => "\\".to_string(),
            // 特殊
            TokenKind::Newline => "\n".to_string(),
            TokenKind::Eof => "".to_string(),
            TokenKind::Space => " ".to_string(),
            // マクロ展開マーカー
            TokenKind::MacroBegin(info) => {
                format!("/*<MACRO_BEGIN:{}>*/", interner.get(info.macro_name))
            }
            TokenKind::MacroEnd(info) => {
                format!("/*<MACRO_END:{}>*/", info.begin_marker_id)
            }
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
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    /// トークンの一意識別子
    pub id: TokenId,
    pub kind: TokenKind,
    pub loc: SourceLocation,
    /// このトークンの直前にあったコメント群
    pub leading_comments: Vec<Comment>,
    /// マクロ展開禁止リスト（自己参照マクロの無限再帰防止用）
    /// TODO: Phase 7 で NoExpandRegistry に移行後、削除予定
    pub no_expand: HashSet<InternedStr>,
}

impl Token {
    /// 新しいトークンを作成
    pub fn new(kind: TokenKind, loc: SourceLocation) -> Self {
        Self {
            id: TokenId::next(),
            kind,
            loc,
            leading_comments: Vec::new(),
            no_expand: HashSet::new(),
        }
    }

    /// コメント付きでトークンを作成
    pub fn with_comments(kind: TokenKind, loc: SourceLocation, comments: Vec<Comment>) -> Self {
        Self {
            id: TokenId::next(),
            kind,
            loc,
            leading_comments: comments,
            no_expand: HashSet::new(),
        }
    }

    /// マクロ展開禁止リストを継承した新しいトークンを作成
    pub fn with_no_expand(kind: TokenKind, loc: SourceLocation, no_expand: HashSet<InternedStr>) -> Self {
        Self {
            id: TokenId::next(),
            kind,
            loc,
            leading_comments: Vec::new(),
            no_expand,
        }
    }

    /// 同じ内容で新しいIDを持つトークンを複製
    ///
    /// マクロ展開時に、定義トークンから新しいインスタンスを作成する際に使用。
    /// 各展開インスタンスが独自のIDを持つことで、展開追跡が可能になる。
    pub fn clone_with_new_id(&self) -> Self {
        Self {
            id: TokenId::next(),
            kind: self.kind.clone(),
            loc: self.loc.clone(),
            leading_comments: self.leading_comments.clone(),
            no_expand: self.no_expand.clone(),
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
        assert_eq!(TokenKind::from_keyword("__inline"), Some(TokenKind::KwInline2));
        assert_eq!(TokenKind::from_keyword("__inline__"), Some(TokenKind::KwInline3));
    }

    #[test]
    fn test_gcc_extension_keywords() {
        // const variants
        assert_eq!(TokenKind::from_keyword("const"), Some(TokenKind::KwConst));
        assert_eq!(TokenKind::from_keyword("__const"), Some(TokenKind::KwConst2));
        assert_eq!(TokenKind::from_keyword("__const__"), Some(TokenKind::KwConst3));
        // volatile variants
        assert_eq!(TokenKind::from_keyword("volatile"), Some(TokenKind::KwVolatile));
        assert_eq!(TokenKind::from_keyword("__volatile"), Some(TokenKind::KwVolatile2));
        assert_eq!(TokenKind::from_keyword("__volatile__"), Some(TokenKind::KwVolatile3));
        // restrict variants
        assert_eq!(TokenKind::from_keyword("restrict"), Some(TokenKind::KwRestrict));
        assert_eq!(TokenKind::from_keyword("__restrict"), Some(TokenKind::KwRestrict2));
        assert_eq!(TokenKind::from_keyword("__restrict__"), Some(TokenKind::KwRestrict3));
        // typeof variants
        assert_eq!(TokenKind::from_keyword("typeof"), Some(TokenKind::KwTypeof));
        assert_eq!(TokenKind::from_keyword("__typeof"), Some(TokenKind::KwTypeof2));
        assert_eq!(TokenKind::from_keyword("__typeof__"), Some(TokenKind::KwTypeof3));
        // attribute variants
        assert_eq!(TokenKind::from_keyword("__attribute"), Some(TokenKind::KwAttribute));
        assert_eq!(TokenKind::from_keyword("__attribute__"), Some(TokenKind::KwAttribute2));
        // asm variants
        assert_eq!(TokenKind::from_keyword("asm"), Some(TokenKind::KwAsm));
        assert_eq!(TokenKind::from_keyword("__asm"), Some(TokenKind::KwAsm2));
        assert_eq!(TokenKind::from_keyword("__asm__"), Some(TokenKind::KwAsm3));
    }

    #[test]
    fn test_token_id_uniqueness() {
        let id1 = TokenId::next();
        let id2 = TokenId::next();
        let id3 = TokenId::next();

        assert_ne!(id1, id2);
        assert_ne!(id2, id3);
        assert_ne!(id1, id3);
    }

    #[test]
    fn test_token_id_invalid() {
        assert!(!TokenId::INVALID.is_valid());
        assert!(TokenId::next().is_valid());
    }

    #[test]
    fn test_token_has_unique_id() {
        let loc = SourceLocation::default();
        let t1 = Token::new(TokenKind::KwInt, loc.clone());
        let t2 = Token::new(TokenKind::KwInt, loc.clone());

        assert_ne!(t1.id, t2.id);
    }

    #[test]
    fn test_clone_with_new_id() {
        let loc = SourceLocation::default();
        let t1 = Token::new(TokenKind::KwInt, loc);
        let t2 = t1.clone_with_new_id();

        // 内容は同じだがIDは異なる
        assert_eq!(t1.kind, t2.kind);
        assert_ne!(t1.id, t2.id);
    }

    #[test]
    fn test_clone_preserves_id() {
        let loc = SourceLocation::default();
        let t1 = Token::new(TokenKind::KwInt, loc);
        let t2 = t1.clone();

        // 通常のcloneはIDも同じ
        assert_eq!(t1.id, t2.id);
    }
}
