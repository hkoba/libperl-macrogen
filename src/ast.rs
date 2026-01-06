//! C言語の抽象構文木
//!
//! C11規格に基づくAST定義。

use crate::intern::InternedStr;
use crate::source::SourceLocation;
use crate::token::Comment;

// ============================================================================
// マクロ展開情報
// ============================================================================

/// 単一のマクロ呼び出し情報
#[derive(Debug, Clone, PartialEq)]
pub struct MacroInvocation {
    /// マクロ名
    pub name: InternedStr,
    /// 呼び出し位置
    pub call_loc: SourceLocation,
    /// 関数マクロの場合、引数のテキスト表現
    pub args: Option<Vec<String>>,
}

impl MacroInvocation {
    /// 新しいマクロ呼び出し情報を作成
    pub fn new(name: InternedStr, call_loc: SourceLocation) -> Self {
        Self {
            name,
            call_loc,
            args: None,
        }
    }

    /// 関数マクロの呼び出し情報を作成
    pub fn with_args(name: InternedStr, call_loc: SourceLocation, args: Vec<String>) -> Self {
        Self {
            name,
            call_loc,
            args: Some(args),
        }
    }
}

/// マクロ展開の履歴情報
///
/// ネストしたマクロ展開を追跡するためのチェーン構造を持つ。
/// 例: A が B を含み、B が C を含む場合: [A, B, C]
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MacroExpansionInfo {
    /// マクロ展開のチェーン（外側から内側へ）
    pub chain: Vec<MacroInvocation>,
}

impl MacroExpansionInfo {
    /// 新しい空のマクロ展開情報を作成
    pub fn new() -> Self {
        Self { chain: Vec::new() }
    }

    /// チェーンが空かどうか
    pub fn is_empty(&self) -> bool {
        self.chain.is_empty()
    }

    /// マクロ呼び出しを追加
    pub fn push(&mut self, invocation: MacroInvocation) {
        self.chain.push(invocation);
    }

    /// 最も内側のマクロ呼び出し
    pub fn innermost(&self) -> Option<&MacroInvocation> {
        self.chain.last()
    }

    /// 最も外側のマクロ呼び出し
    pub fn outermost(&self) -> Option<&MacroInvocation> {
        self.chain.first()
    }

    /// チェーンの長さ
    pub fn len(&self) -> usize {
        self.chain.len()
    }
}

/// ASTノードの共通メタデータ
///
/// ソース位置とオプションのマクロ展開情報を保持する。
/// Phase 5 で各ASTノードの `loc: SourceLocation` を `info: NodeInfo` に置き換える。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NodeInfo {
    /// ソース位置
    pub loc: SourceLocation,
    /// マクロ展開情報（マクロ展開由来の場合のみ Some）
    pub macro_expansion: Option<Box<MacroExpansionInfo>>,
}

impl NodeInfo {
    /// 新しいNodeInfoを作成（マクロ情報なし）
    pub fn new(loc: SourceLocation) -> Self {
        Self {
            loc,
            macro_expansion: None,
        }
    }

    /// マクロ情報付きのNodeInfoを作成
    ///
    /// マクロ情報が空の場合は None として保存する。
    pub fn with_macro_info(loc: SourceLocation, macro_info: MacroExpansionInfo) -> Self {
        Self {
            loc,
            macro_expansion: if macro_info.is_empty() {
                None
            } else {
                Some(Box::new(macro_info))
            },
        }
    }

    /// マクロ展開由来かどうか
    pub fn is_from_macro(&self) -> bool {
        self.macro_expansion.is_some()
    }

    /// マクロ展開情報への参照を取得
    pub fn macro_info(&self) -> Option<&MacroExpansionInfo> {
        self.macro_expansion.as_deref()
    }
}

// ============================================================================
// AST 定義
// ============================================================================

/// 翻訳単位（ファイル全体）
#[derive(Debug, Clone)]
pub struct TranslationUnit {
    pub decls: Vec<ExternalDecl>,
}

/// 外部宣言
#[derive(Debug, Clone)]
pub enum ExternalDecl {
    /// 関数定義
    FunctionDef(FunctionDef),
    /// 変数・型宣言
    Declaration(Declaration),
}

impl ExternalDecl {
    /// ターゲットディレクトリで定義されたかどうか
    pub fn is_target(&self) -> bool {
        match self {
            ExternalDecl::FunctionDef(f) => f.is_target,
            ExternalDecl::Declaration(d) => d.is_target,
        }
    }
}

/// 関数定義
#[derive(Debug, Clone)]
pub struct FunctionDef {
    pub specs: DeclSpecs,
    pub declarator: Declarator,
    pub body: CompoundStmt,
    pub loc: SourceLocation,
    pub comments: Vec<Comment>,
    /// ターゲットディレクトリで定義されたかどうか
    pub is_target: bool,
}

/// 宣言
#[derive(Debug, Clone)]
pub struct Declaration {
    pub specs: DeclSpecs,
    pub declarators: Vec<InitDeclarator>,
    pub loc: SourceLocation,
    pub comments: Vec<Comment>,
    /// ターゲットディレクトリで定義されたかどうか
    pub is_target: bool,
}

/// 宣言指定子
#[derive(Debug, Clone, Default)]
pub struct DeclSpecs {
    pub storage: Option<StorageClass>,
    pub type_specs: Vec<TypeSpec>,
    pub qualifiers: TypeQualifiers,
    pub is_inline: bool,
}

/// ストレージクラス
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageClass {
    Typedef,
    Extern,
    Static,
    Auto,
    Register,
}

/// 型指定子
#[derive(Debug, Clone)]
pub enum TypeSpec {
    Void,
    Char,
    Short,
    Int,
    Long,
    Float,
    Double,
    Signed,
    Unsigned,
    Bool,
    Complex,
    // GCC拡張浮動小数点型
    Float16,
    Float32,
    Float64,
    Float128,
    Float32x,
    Float64x,
    // GCC拡張: 128ビット整数
    Int128,
    // GCC拡張: __typeof__(expr)
    TypeofExpr(Box<Expr>),
    Struct(StructSpec),
    Union(StructSpec),
    Enum(EnumSpec),
    TypedefName(InternedStr),
}

/// 型修飾子
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TypeQualifiers {
    pub is_const: bool,
    pub is_volatile: bool,
    pub is_restrict: bool,
    pub is_atomic: bool,
}

impl TypeQualifiers {
    pub fn is_empty(&self) -> bool {
        !self.is_const && !self.is_volatile && !self.is_restrict && !self.is_atomic
    }
}

/// 構造体/共用体指定
#[derive(Debug, Clone)]
pub struct StructSpec {
    pub name: Option<InternedStr>,
    pub members: Option<Vec<StructMember>>,
    pub loc: SourceLocation,
}

/// 構造体メンバー
#[derive(Debug, Clone)]
pub struct StructMember {
    pub specs: DeclSpecs,
    pub declarators: Vec<StructDeclarator>,
}

/// 構造体メンバー宣言子
#[derive(Debug, Clone)]
pub struct StructDeclarator {
    pub declarator: Option<Declarator>,
    pub bitfield: Option<Box<Expr>>,
}

/// 列挙型指定
#[derive(Debug, Clone)]
pub struct EnumSpec {
    pub name: Option<InternedStr>,
    pub enumerators: Option<Vec<Enumerator>>,
    pub loc: SourceLocation,
}

/// 列挙子
#[derive(Debug, Clone)]
pub struct Enumerator {
    pub name: InternedStr,
    pub value: Option<Box<Expr>>,
    pub loc: SourceLocation,
}

/// パラメータ宣言
#[derive(Debug, Clone)]
pub struct ParamDecl {
    pub specs: DeclSpecs,
    pub declarator: Option<Declarator>,
    pub loc: SourceLocation,
}

/// パラメータリスト
#[derive(Debug, Clone)]
pub struct ParamList {
    pub params: Vec<ParamDecl>,
    pub is_variadic: bool,
}

/// 初期化子付き宣言子
#[derive(Debug, Clone)]
pub struct InitDeclarator {
    pub declarator: Declarator,
    pub init: Option<Initializer>,
}

/// 宣言子
#[derive(Debug, Clone)]
pub struct Declarator {
    pub name: Option<InternedStr>,
    pub derived: Vec<DerivedDecl>,
    pub loc: SourceLocation,
}

/// 派生宣言子（ポインタ、配列、関数）
#[derive(Debug, Clone)]
pub enum DerivedDecl {
    Pointer(TypeQualifiers),
    Array(ArrayDecl),
    Function(ParamList),
}

/// 配列宣言子
#[derive(Debug, Clone)]
pub struct ArrayDecl {
    pub size: Option<Box<Expr>>,
    pub qualifiers: TypeQualifiers,
    pub is_static: bool,
    pub is_vla: bool,
}

/// 初期化子
#[derive(Debug, Clone)]
pub enum Initializer {
    Expr(Box<Expr>),
    List(Vec<InitializerItem>),
}

/// 初期化子リスト項目
#[derive(Debug, Clone)]
pub struct InitializerItem {
    pub designation: Vec<Designator>,
    pub init: Initializer,
}

/// 指示子
#[derive(Debug, Clone)]
pub enum Designator {
    Index(Box<Expr>),
    Member(InternedStr),
}

/// 文
#[derive(Debug, Clone)]
pub enum Stmt {
    /// 複合文
    Compound(CompoundStmt),
    /// 式文
    Expr(Option<Box<Expr>>, SourceLocation),
    /// if文
    If {
        cond: Box<Expr>,
        then_stmt: Box<Stmt>,
        else_stmt: Option<Box<Stmt>>,
        loc: SourceLocation,
    },
    /// switch文
    Switch {
        expr: Box<Expr>,
        body: Box<Stmt>,
        loc: SourceLocation,
    },
    /// while文
    While {
        cond: Box<Expr>,
        body: Box<Stmt>,
        loc: SourceLocation,
    },
    /// do-while文
    DoWhile {
        body: Box<Stmt>,
        cond: Box<Expr>,
        loc: SourceLocation,
    },
    /// for文
    For {
        init: Option<ForInit>,
        cond: Option<Box<Expr>>,
        step: Option<Box<Expr>>,
        body: Box<Stmt>,
        loc: SourceLocation,
    },
    /// goto文
    Goto(InternedStr, SourceLocation),
    /// continue文
    Continue(SourceLocation),
    /// break文
    Break(SourceLocation),
    /// return文
    Return(Option<Box<Expr>>, SourceLocation),
    /// ラベル文
    Label {
        name: InternedStr,
        stmt: Box<Stmt>,
        loc: SourceLocation,
    },
    /// case文
    Case {
        expr: Box<Expr>,
        stmt: Box<Stmt>,
        loc: SourceLocation,
    },
    /// default文
    Default {
        stmt: Box<Stmt>,
        loc: SourceLocation,
    },
    /// asm文
    Asm {
        loc: SourceLocation,
    },
}

/// for文の初期化部
#[derive(Debug, Clone)]
pub enum ForInit {
    Expr(Box<Expr>),
    Decl(Declaration),
}

/// 複合文
#[derive(Debug, Clone)]
pub struct CompoundStmt {
    pub items: Vec<BlockItem>,
    pub loc: SourceLocation,
}

/// ブロック内項目
#[derive(Debug, Clone)]
pub enum BlockItem {
    Decl(Declaration),
    Stmt(Stmt),
}

/// 式
#[derive(Debug, Clone)]
pub enum Expr {
    // 一次式
    Ident(InternedStr, SourceLocation),
    IntLit(i64, SourceLocation),
    UIntLit(u64, SourceLocation),
    FloatLit(f64, SourceLocation),
    CharLit(u8, SourceLocation),
    StringLit(Vec<u8>, SourceLocation),

    // 後置式
    Index {
        expr: Box<Expr>,
        index: Box<Expr>,
        loc: SourceLocation,
    },
    Call {
        func: Box<Expr>,
        args: Vec<Expr>,
        loc: SourceLocation,
    },
    Member {
        expr: Box<Expr>,
        member: InternedStr,
        loc: SourceLocation,
    },
    PtrMember {
        expr: Box<Expr>,
        member: InternedStr,
        loc: SourceLocation,
    },
    PostInc(Box<Expr>, SourceLocation),
    PostDec(Box<Expr>, SourceLocation),
    CompoundLit {
        type_name: Box<TypeName>,
        init: Vec<InitializerItem>,
        loc: SourceLocation,
    },

    // 単項式
    PreInc(Box<Expr>, SourceLocation),
    PreDec(Box<Expr>, SourceLocation),
    AddrOf(Box<Expr>, SourceLocation),
    Deref(Box<Expr>, SourceLocation),
    UnaryPlus(Box<Expr>, SourceLocation),
    UnaryMinus(Box<Expr>, SourceLocation),
    BitNot(Box<Expr>, SourceLocation),
    LogNot(Box<Expr>, SourceLocation),
    Sizeof(Box<Expr>, SourceLocation),
    SizeofType(Box<TypeName>, SourceLocation),
    Alignof(Box<TypeName>, SourceLocation),
    Cast {
        type_name: Box<TypeName>,
        expr: Box<Expr>,
        loc: SourceLocation,
    },

    // 二項式
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
        loc: SourceLocation,
    },

    // 条件式
    Conditional {
        cond: Box<Expr>,
        then_expr: Box<Expr>,
        else_expr: Box<Expr>,
        loc: SourceLocation,
    },

    // 代入式
    Assign {
        op: AssignOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
        loc: SourceLocation,
    },

    // コンマ式
    Comma {
        lhs: Box<Expr>,
        rhs: Box<Expr>,
        loc: SourceLocation,
    },

    // GCC拡張: ステートメント式 ({ ... })
    StmtExpr(CompoundStmt, SourceLocation),
}

impl Expr {
    /// 式の位置情報を取得
    pub fn loc(&self) -> &SourceLocation {
        match self {
            Expr::Ident(_, loc) => loc,
            Expr::IntLit(_, loc) => loc,
            Expr::UIntLit(_, loc) => loc,
            Expr::FloatLit(_, loc) => loc,
            Expr::CharLit(_, loc) => loc,
            Expr::StringLit(_, loc) => loc,
            Expr::Index { loc, .. } => loc,
            Expr::Call { loc, .. } => loc,
            Expr::Member { loc, .. } => loc,
            Expr::PtrMember { loc, .. } => loc,
            Expr::PostInc(_, loc) => loc,
            Expr::PostDec(_, loc) => loc,
            Expr::CompoundLit { loc, .. } => loc,
            Expr::PreInc(_, loc) => loc,
            Expr::PreDec(_, loc) => loc,
            Expr::AddrOf(_, loc) => loc,
            Expr::Deref(_, loc) => loc,
            Expr::UnaryPlus(_, loc) => loc,
            Expr::UnaryMinus(_, loc) => loc,
            Expr::BitNot(_, loc) => loc,
            Expr::LogNot(_, loc) => loc,
            Expr::Sizeof(_, loc) => loc,
            Expr::SizeofType(_, loc) => loc,
            Expr::Alignof(_, loc) => loc,
            Expr::Cast { loc, .. } => loc,
            Expr::Binary { loc, .. } => loc,
            Expr::Conditional { loc, .. } => loc,
            Expr::Assign { loc, .. } => loc,
            Expr::Comma { loc, .. } => loc,
            Expr::StmtExpr(_, loc) => loc,
        }
    }
}

/// 型名（キャストやsizeofで使用）
#[derive(Debug, Clone)]
pub struct TypeName {
    pub specs: DeclSpecs,
    pub declarator: Option<AbstractDeclarator>,
}

/// 抽象宣言子（名前なし）
#[derive(Debug, Clone)]
pub struct AbstractDeclarator {
    pub derived: Vec<DerivedDecl>,
}

/// 二項演算子
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    // 乗除
    Mul,
    Div,
    Mod,
    // 加減
    Add,
    Sub,
    // シフト
    Shl,
    Shr,
    // 比較
    Lt,
    Gt,
    Le,
    Ge,
    Eq,
    Ne,
    // ビット演算
    BitAnd,
    BitXor,
    BitOr,
    // 論理演算
    LogAnd,
    LogOr,
}

/// 代入演算子
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignOp {
    Assign,
    MulAssign,
    DivAssign,
    ModAssign,
    AddAssign,
    SubAssign,
    ShlAssign,
    ShrAssign,
    AndAssign,
    XorAssign,
    OrAssign,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intern::StringInterner;

    #[test]
    fn test_type_qualifiers_is_empty() {
        let empty = TypeQualifiers::default();
        assert!(empty.is_empty());

        let with_const = TypeQualifiers {
            is_const: true,
            ..Default::default()
        };
        assert!(!with_const.is_empty());
    }

    // MacroInvocation tests

    #[test]
    fn test_macro_invocation_new() {
        let mut interner = StringInterner::new();
        let name = interner.intern("FOO");
        let loc = SourceLocation::default();

        let inv = MacroInvocation::new(name, loc.clone());

        assert_eq!(inv.name, name);
        assert_eq!(inv.call_loc, loc);
        assert!(inv.args.is_none());
    }

    #[test]
    fn test_macro_invocation_with_args() {
        let mut interner = StringInterner::new();
        let name = interner.intern("ADD");
        let loc = SourceLocation::default();
        let args = vec!["a".to_string(), "b".to_string()];

        let inv = MacroInvocation::with_args(name, loc.clone(), args.clone());

        assert_eq!(inv.name, name);
        assert_eq!(inv.call_loc, loc);
        assert_eq!(inv.args, Some(args));
    }

    // MacroExpansionInfo tests

    #[test]
    fn test_macro_expansion_info_new() {
        let info = MacroExpansionInfo::new();
        assert!(info.is_empty());
        assert_eq!(info.len(), 0);
        assert!(info.innermost().is_none());
        assert!(info.outermost().is_none());
    }

    #[test]
    fn test_macro_expansion_info_push() {
        let mut interner = StringInterner::new();
        let mut info = MacroExpansionInfo::new();

        let inv1 = MacroInvocation::new(interner.intern("FOO"), SourceLocation::default());
        let inv2 = MacroInvocation::new(interner.intern("BAR"), SourceLocation::default());

        info.push(inv1.clone());
        assert_eq!(info.len(), 1);
        assert!(!info.is_empty());

        info.push(inv2.clone());
        assert_eq!(info.len(), 2);

        // outermost は最初に追加したもの
        assert_eq!(info.outermost().unwrap().name, inv1.name);
        // innermost は最後に追加したもの
        assert_eq!(info.innermost().unwrap().name, inv2.name);
    }

    #[test]
    fn test_macro_expansion_info_chain() {
        let mut interner = StringInterner::new();
        let mut info = MacroExpansionInfo::new();

        // A → B → C のチェーン
        info.push(MacroInvocation::new(interner.intern("A"), SourceLocation::default()));
        info.push(MacroInvocation::new(interner.intern("B"), SourceLocation::default()));
        info.push(MacroInvocation::new(interner.intern("C"), SourceLocation::default()));

        assert_eq!(info.chain.len(), 3);
        assert_eq!(interner.get(info.chain[0].name), "A");
        assert_eq!(interner.get(info.chain[1].name), "B");
        assert_eq!(interner.get(info.chain[2].name), "C");
    }

    // NodeInfo tests

    #[test]
    fn test_node_info_new() {
        let loc = SourceLocation::new(crate::source::FileId::default(), 10, 5);
        let info = NodeInfo::new(loc.clone());

        assert_eq!(info.loc, loc);
        assert!(!info.is_from_macro());
        assert!(info.macro_info().is_none());
    }

    #[test]
    fn test_node_info_with_empty_macro_info() {
        let loc = SourceLocation::default();
        let macro_info = MacroExpansionInfo::new();

        let info = NodeInfo::with_macro_info(loc.clone(), macro_info);

        // 空のマクロ情報は None として保存される
        assert!(!info.is_from_macro());
        assert!(info.macro_info().is_none());
    }

    #[test]
    fn test_node_info_with_macro_info() {
        let mut interner = StringInterner::new();
        let loc = SourceLocation::default();

        let mut macro_info = MacroExpansionInfo::new();
        macro_info.push(MacroInvocation::new(interner.intern("FOO"), SourceLocation::default()));

        let info = NodeInfo::with_macro_info(loc.clone(), macro_info);

        assert!(info.is_from_macro());
        assert!(info.macro_info().is_some());
        assert_eq!(info.macro_info().unwrap().len(), 1);
    }

    #[test]
    fn test_node_info_default() {
        let info = NodeInfo::default();

        assert_eq!(info.loc, SourceLocation::default());
        assert!(!info.is_from_macro());
    }
}
