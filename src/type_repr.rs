//! 型表現モジュール
//!
//! TypeConstraint で使用する構造化された型表現を提供する。
//! C 型、Rust 型、推論結果を統一的に表現し、文字列ベースの型比較を排除する。

use std::fmt;

use crate::ast::BinOp;
use crate::intern::InternedStr;

// ============================================================================
// TypeRepr: トップレベル型表現
// ============================================================================

/// 型表現（出所情報を含む）
#[derive(Debug, Clone)]
pub enum TypeRepr {
    /// C 言語の型（CHeader, Apidoc, InlineFn 共通）
    CType {
        /// 型指定子（int, char, struct X, など）
        specs: CTypeSpecs,
        /// 派生型（ポインタ、配列など）
        derived: Vec<CDerivedType>,
        /// 出所（デバッグ用）
        source: CTypeSource,
    },

    /// Rust バインディングからの型（syn::Type 由来）
    RustType {
        /// 型表現
        repr: RustTypeRepr,
        /// 出所（関数名など）
        source: RustTypeSource,
    },

    /// 推論で導出
    Inferred(InferredType),
}

// ============================================================================
// C 型の出所
// ============================================================================

/// C 型の出所
#[derive(Debug, Clone)]
pub enum CTypeSource {
    /// C ヘッダーのパース結果
    Header,
    /// apidoc（embed.fnc 等）- 元の文字列を保持
    Apidoc { raw: String },
    /// inline 関数の AST
    InlineFn { func_name: InternedStr },
}

// ============================================================================
// Rust 型の出所
// ============================================================================

/// Rust 型の出所
#[derive(Debug, Clone)]
pub enum RustTypeSource {
    /// bindings.rs の関数引数
    FnParam { func_name: String, param_index: usize },
    /// bindings.rs の関数戻り値
    FnReturn { func_name: String },
    /// bindings.rs の定数
    Const { const_name: String },
}

// ============================================================================
// C 型の構造化表現
// ============================================================================

/// C 型指定子（DeclSpecs から抽出）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CTypeSpecs {
    /// void
    Void,
    /// char (signed: None = plain char, Some(true) = signed, Some(false) = unsigned)
    Char { signed: Option<bool> },
    /// 整数型
    Int { signed: bool, size: IntSize },
    /// float
    Float,
    /// double (is_long: long double かどうか)
    Double { is_long: bool },
    /// _Bool
    Bool,
    /// 構造体/共用体
    Struct { name: Option<InternedStr>, is_union: bool },
    /// enum
    Enum { name: Option<InternedStr> },
    /// typedef 名
    TypedefName(InternedStr),
}

/// 整数サイズ
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntSize {
    /// short
    Short,
    /// int (default)
    Int,
    /// long
    Long,
    /// long long
    LongLong,
    /// __int128
    Int128,
}

/// C 派生型
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CDerivedType {
    /// ポインタ
    Pointer {
        is_const: bool,
        is_volatile: bool,
        is_restrict: bool,
    },
    /// 配列
    Array { size: Option<usize> },
    /// 関数
    Function {
        params: Vec<CTypeSpecs>,
        variadic: bool,
    },
}

// ============================================================================
// Rust 型の構造化表現
// ============================================================================

/// Rust 型表現（syn::Type から変換）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RustTypeRepr {
    /// C互換基本型 (c_int, c_char, etc.)
    CPrimitive(CPrimitiveKind),
    /// Rust基本型 (i32, u64, bool, etc.)
    RustPrimitive(RustPrimitiveKind),
    /// ポインタ (*mut T, *const T)
    Pointer {
        inner: Box<RustTypeRepr>,
        is_const: bool,
    },
    /// 参照 (&T, &mut T)
    Reference {
        inner: Box<RustTypeRepr>,
        is_mut: bool,
    },
    /// 名前付き型 (SV, AV, PerlInterpreter, etc.)
    Named(String),
    /// Option<T>
    Option(Box<RustTypeRepr>),
    /// 関数ポインタ
    FnPointer {
        params: Vec<RustTypeRepr>,
        ret: Option<Box<RustTypeRepr>>,
    },
    /// ユニット ()
    Unit,
    /// パース不能だった型（文字列で保持）
    Unknown(String),
}

/// C互換基本型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CPrimitiveKind {
    CChar,
    CSchar,
    CUchar,
    CShort,
    CUshort,
    CInt,
    CUint,
    CLong,
    CUlong,
    CLongLong,
    CUlongLong,
    CFloat,
    CDouble,
}

/// Rust基本型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RustPrimitiveKind {
    I8,
    I16,
    I32,
    I64,
    I128,
    Isize,
    U8,
    U16,
    U32,
    U64,
    U128,
    Usize,
    F32,
    F64,
    Bool,
}

// ============================================================================
// 推論の根拠 (InferredType)
// ============================================================================

/// 推論で導出された型
#[derive(Debug, Clone)]
pub enum InferredType {
    // ==================== リテラル ====================
    /// 整数リテラル (42, 0x1F, etc.)
    IntLiteral,
    /// 符号なし整数リテラル (42u, etc.)
    UIntLiteral,
    /// 浮動小数点リテラル (3.14, etc.)
    FloatLiteral,
    /// 文字リテラル ('a')
    CharLiteral,
    /// 文字列リテラル ("hello")
    StringLiteral,

    // ==================== 識別子参照 ====================
    /// シンボルテーブルからの参照
    SymbolLookup {
        name: InternedStr,
        /// 解決された型
        resolved_type: Box<TypeRepr>,
    },
    /// THX (my_perl) のデフォルト型
    ThxDefault,

    // ==================== 演算子 ====================
    /// 二項演算の結果
    BinaryOp {
        op: BinOp,
        /// 左右オペランドの型から計算された結果型
        result_type: Box<TypeRepr>,
    },
    /// 単項演算 (+, -, ~)
    UnaryArithmetic {
        /// 内部式の型をそのまま継承
        inner_type: Box<TypeRepr>,
    },
    /// 論理否定 (!) - 常に int
    LogicalNot,
    /// アドレス取得 (&x)
    AddressOf { inner_type: Box<TypeRepr> },
    /// 間接参照 (*p)
    Dereference { pointer_type: Box<TypeRepr> },
    /// インクリメント/デクリメント (++, --)
    IncDec { inner_type: Box<TypeRepr> },

    // ==================== メンバーアクセス ====================
    /// 直接メンバーアクセス (expr.member)
    MemberAccess {
        base_type: String,
        member: InternedStr,
        /// 解決されたフィールド型
        field_type: Option<Box<TypeRepr>>,
    },
    /// ポインタメンバーアクセス (expr->member)
    PtrMemberAccess {
        base_type: String,
        member: InternedStr,
        /// 解決されたフィールド型
        field_type: Option<Box<TypeRepr>>,
        /// 一致型を使用した場合（ベース型が不明時）
        used_consistent_type: bool,
    },

    // ==================== 配列/添字 ====================
    /// 配列添字 (arr[i])
    ArraySubscript {
        base_type: Box<TypeRepr>,
        /// 要素型
        element_type: Box<TypeRepr>,
    },

    // ==================== 条件・制御 ====================
    /// 条件演算子 (cond ? then : else)
    Conditional {
        then_type: Box<TypeRepr>,
        else_type: Box<TypeRepr>,
        /// 計算された共通型
        result_type: Box<TypeRepr>,
    },
    /// コンマ式 (a, b)
    Comma {
        /// 右辺の型
        rhs_type: Box<TypeRepr>,
    },
    /// 代入式 (a = b)
    Assignment {
        /// 左辺の型
        lhs_type: Box<TypeRepr>,
    },

    // ==================== 型操作 ====================
    /// キャスト式 ((type)expr)
    Cast { target_type: Box<TypeRepr> },
    /// sizeof 式/型 - 常に unsigned long
    Sizeof,
    /// alignof - 常に unsigned long
    Alignof,
    /// 複合リテラル ((type){...})
    CompoundLiteral { type_name: Box<TypeRepr> },

    // ==================== その他 ====================
    /// 文式 ({ ... })
    StmtExpr {
        /// 最後の式の型
        last_expr_type: Option<Box<TypeRepr>>,
    },
    /// アサーション - 常に void
    Assert,
    /// 関数呼び出しの戻り値（RustBindings/Apidoc から取得できなかった場合）
    FunctionReturn { func_name: InternedStr },
}

// ============================================================================
// 変換関数
// ============================================================================

impl CTypeSpecs {
    /// DeclSpecs から CTypeSpecs を抽出
    pub fn from_decl_specs(specs: &crate::ast::DeclSpecs, _interner: &crate::intern::StringInterner) -> Self {
        use crate::ast::TypeSpec;

        let mut has_signed = false;
        let mut has_unsigned = false;
        let mut has_short = false;
        let mut has_long: u8 = 0;
        let mut base_type: Option<CTypeSpecs> = None;

        for type_spec in &specs.type_specs {
            match type_spec {
                TypeSpec::Void => base_type = Some(CTypeSpecs::Void),
                TypeSpec::Char => {
                    // char の signed/unsigned は後で決定
                    if base_type.is_none() {
                        base_type = Some(CTypeSpecs::Char { signed: None });
                    }
                }
                TypeSpec::Short => has_short = true,
                TypeSpec::Int => {
                    if base_type.is_none() {
                        base_type = Some(CTypeSpecs::Int {
                            signed: true,
                            size: IntSize::Int,
                        });
                    }
                }
                TypeSpec::Long => has_long += 1,
                TypeSpec::Float => base_type = Some(CTypeSpecs::Float),
                TypeSpec::Double => base_type = Some(CTypeSpecs::Double { is_long: false }),
                TypeSpec::Signed => has_signed = true,
                TypeSpec::Unsigned => has_unsigned = true,
                TypeSpec::Bool => base_type = Some(CTypeSpecs::Bool),
                TypeSpec::Int128 => {
                    base_type = Some(CTypeSpecs::Int {
                        signed: !has_unsigned,
                        size: IntSize::Int128,
                    });
                }
                TypeSpec::Struct(s) => {
                    base_type = Some(CTypeSpecs::Struct {
                        name: s.name,
                        is_union: false,
                    });
                }
                TypeSpec::Union(s) => {
                    base_type = Some(CTypeSpecs::Struct {
                        name: s.name,
                        is_union: true,
                    });
                }
                TypeSpec::Enum(e) => {
                    base_type = Some(CTypeSpecs::Enum { name: e.name });
                }
                TypeSpec::TypedefName(name) => {
                    base_type = Some(CTypeSpecs::TypedefName(*name));
                }
                _ => {}
            }
        }

        // signed/unsigned と short/long の組み合わせを処理
        if has_short {
            return CTypeSpecs::Int {
                signed: !has_unsigned,
                size: IntSize::Short,
            };
        }

        if has_long >= 2 {
            return CTypeSpecs::Int {
                signed: !has_unsigned,
                size: IntSize::LongLong,
            };
        }

        if has_long == 1 {
            if let Some(CTypeSpecs::Double { .. }) = base_type {
                return CTypeSpecs::Double { is_long: true };
            }
            return CTypeSpecs::Int {
                signed: !has_unsigned,
                size: IntSize::Long,
            };
        }

        // char の signed/unsigned を確定
        if let Some(CTypeSpecs::Char { .. }) = base_type {
            if has_signed {
                return CTypeSpecs::Char { signed: Some(true) };
            } else if has_unsigned {
                return CTypeSpecs::Char { signed: Some(false) };
            }
            return CTypeSpecs::Char { signed: None };
        }

        // 単独の signed/unsigned
        if has_unsigned && base_type.is_none() {
            return CTypeSpecs::Int {
                signed: false,
                size: IntSize::Int,
            };
        }
        if has_signed && base_type.is_none() {
            return CTypeSpecs::Int {
                signed: true,
                size: IntSize::Int,
            };
        }

        // int の unsigned
        if has_unsigned {
            if let Some(CTypeSpecs::Int { size, .. }) = base_type {
                return CTypeSpecs::Int {
                    signed: false,
                    size,
                };
            }
        }

        base_type.unwrap_or(CTypeSpecs::Int {
            signed: true,
            size: IntSize::Int,
        })
    }
}

impl CDerivedType {
    /// DerivedDecl のリストから CDerivedType のリストを作成
    pub fn from_derived_decls(derived: &[crate::ast::DerivedDecl]) -> Vec<Self> {
        use crate::ast::ExprKind;

        derived
            .iter()
            .map(|d| match d {
                crate::ast::DerivedDecl::Pointer(quals) => CDerivedType::Pointer {
                    is_const: quals.is_const,
                    is_volatile: quals.is_volatile,
                    is_restrict: quals.is_restrict,
                },
                crate::ast::DerivedDecl::Array(array_decl) => {
                    // 配列サイズが定数リテラルの場合のみ抽出
                    let size = array_decl.size.as_ref().and_then(|expr| {
                        match &expr.kind {
                            ExprKind::IntLit(n) => Some(*n as usize),
                            ExprKind::UIntLit(n) => Some(*n as usize),
                            _ => None,
                        }
                    });
                    CDerivedType::Array { size }
                }
                crate::ast::DerivedDecl::Function(_params) => {
                    // 関数パラメータの詳細は簡略化
                    CDerivedType::Function {
                        params: vec![],
                        variadic: false,
                    }
                }
            })
            .collect()
    }
}

impl RustTypeRepr {
    /// 型文字列から RustTypeRepr をパース
    pub fn from_type_string(s: &str) -> Self {
        let s = s.trim();

        // ユニット型
        if s == "()" {
            return RustTypeRepr::Unit;
        }

        // ポインタ型
        if let Some(rest) = s.strip_prefix("*mut ") {
            return RustTypeRepr::Pointer {
                inner: Box::new(Self::from_type_string(rest)),
                is_const: false,
            };
        }
        if let Some(rest) = s.strip_prefix("* mut ") {
            return RustTypeRepr::Pointer {
                inner: Box::new(Self::from_type_string(rest)),
                is_const: false,
            };
        }
        if let Some(rest) = s.strip_prefix("*const ") {
            return RustTypeRepr::Pointer {
                inner: Box::new(Self::from_type_string(rest)),
                is_const: true,
            };
        }
        if let Some(rest) = s.strip_prefix("* const ") {
            return RustTypeRepr::Pointer {
                inner: Box::new(Self::from_type_string(rest)),
                is_const: true,
            };
        }

        // 参照型
        if let Some(rest) = s.strip_prefix("&mut ") {
            return RustTypeRepr::Reference {
                inner: Box::new(Self::from_type_string(rest)),
                is_mut: true,
            };
        }
        if let Some(rest) = s.strip_prefix("& mut ") {
            return RustTypeRepr::Reference {
                inner: Box::new(Self::from_type_string(rest)),
                is_mut: true,
            };
        }
        if let Some(rest) = s.strip_prefix('&') {
            return RustTypeRepr::Reference {
                inner: Box::new(Self::from_type_string(rest.trim())),
                is_mut: false,
            };
        }

        // C 互換基本型
        if let Some(kind) = Self::parse_c_primitive(s) {
            return RustTypeRepr::CPrimitive(kind);
        }

        // Rust 基本型
        if let Some(kind) = Self::parse_rust_primitive(s) {
            return RustTypeRepr::RustPrimitive(kind);
        }

        // Option<T>
        if s.starts_with("Option<") || s.starts_with(":: std :: option :: Option<") {
            if let Some(inner) = Self::extract_generic_param(s, "Option") {
                return RustTypeRepr::Option(Box::new(Self::from_type_string(&inner)));
            }
        }

        // 名前付き型（識別子）
        if s.chars().next().map(|c| c.is_alphabetic() || c == '_').unwrap_or(false) {
            // パスセパレータを含む場合は最後の部分を使用
            let name = s.split("::").last().unwrap_or(s).trim();
            return RustTypeRepr::Named(name.to_string());
        }

        // パース不能
        RustTypeRepr::Unknown(s.to_string())
    }

    /// C 互換基本型をパース
    fn parse_c_primitive(s: &str) -> Option<CPrimitiveKind> {
        // :: std :: os :: raw :: c_* 形式にも対応
        let s = s.trim();
        let name = if s.contains("::") {
            s.split("::").last()?.trim()
        } else {
            s
        };

        match name {
            "c_char" => Some(CPrimitiveKind::CChar),
            "c_schar" => Some(CPrimitiveKind::CSchar),
            "c_uchar" => Some(CPrimitiveKind::CUchar),
            "c_short" => Some(CPrimitiveKind::CShort),
            "c_ushort" => Some(CPrimitiveKind::CUshort),
            "c_int" => Some(CPrimitiveKind::CInt),
            "c_uint" => Some(CPrimitiveKind::CUint),
            "c_long" => Some(CPrimitiveKind::CLong),
            "c_ulong" => Some(CPrimitiveKind::CUlong),
            "c_longlong" => Some(CPrimitiveKind::CLongLong),
            "c_ulonglong" => Some(CPrimitiveKind::CUlongLong),
            "c_float" => Some(CPrimitiveKind::CFloat),
            "c_double" => Some(CPrimitiveKind::CDouble),
            _ => None,
        }
    }

    /// Rust 基本型をパース
    fn parse_rust_primitive(s: &str) -> Option<RustPrimitiveKind> {
        match s.trim() {
            "i8" => Some(RustPrimitiveKind::I8),
            "i16" => Some(RustPrimitiveKind::I16),
            "i32" => Some(RustPrimitiveKind::I32),
            "i64" => Some(RustPrimitiveKind::I64),
            "i128" => Some(RustPrimitiveKind::I128),
            "isize" => Some(RustPrimitiveKind::Isize),
            "u8" => Some(RustPrimitiveKind::U8),
            "u16" => Some(RustPrimitiveKind::U16),
            "u32" => Some(RustPrimitiveKind::U32),
            "u64" => Some(RustPrimitiveKind::U64),
            "u128" => Some(RustPrimitiveKind::U128),
            "usize" => Some(RustPrimitiveKind::Usize),
            "f32" => Some(RustPrimitiveKind::F32),
            "f64" => Some(RustPrimitiveKind::F64),
            "bool" => Some(RustPrimitiveKind::Bool),
            _ => None,
        }
    }

    /// ジェネリック型のパラメータを抽出
    fn extract_generic_param(s: &str, type_name: &str) -> Option<String> {
        // "Option<T>" または ":: std :: option :: Option<T>" から T を抽出
        let start = s.find(&format!("{}<", type_name))?;
        let after_open = start + type_name.len() + 1;
        let content = &s[after_open..];

        // 対応する > を探す（ネストを考慮）
        let mut depth = 1;
        let mut end = 0;
        for (i, c) in content.char_indices() {
            match c {
                '<' => depth += 1,
                '>' => {
                    depth -= 1;
                    if depth == 0 {
                        end = i;
                        break;
                    }
                }
                _ => {}
            }
        }

        if end > 0 {
            Some(content[..end].trim().to_string())
        } else {
            None
        }
    }
}

impl TypeRepr {
    /// Apidoc の型文字列から TypeRepr を作成
    pub fn from_apidoc_string(s: &str, interner: &crate::intern::StringInterner) -> Self {
        // C 型文字列をパース
        let (specs, derived) = Self::parse_c_type_string(s, interner);
        TypeRepr::CType {
            specs,
            derived,
            source: CTypeSource::Apidoc { raw: s.to_string() },
        }
    }

    /// C 型文字列をパース（簡易版）
    fn parse_c_type_string(s: &str, interner: &crate::intern::StringInterner) -> (CTypeSpecs, Vec<CDerivedType>) {
        let s = s.trim();

        // ポインタ数をカウント
        let mut ptr_count = 0;
        let mut is_const = false;
        let mut base = s;

        // 末尾の * をカウント
        while base.ends_with('*') {
            ptr_count += 1;
            base = base[..base.len() - 1].trim();
        }

        // "const" をチェック
        if base.starts_with("const ") {
            is_const = true;
            base = base[6..].trim();
        }
        if base.ends_with(" const") {
            is_const = true;
            base = base[..base.len() - 6].trim();
        }

        // 基本型をパース
        let specs = Self::parse_c_base_type(base, interner);

        // 派生型を構築
        let derived: Vec<CDerivedType> = (0..ptr_count)
            .map(|i| CDerivedType::Pointer {
                is_const: i == 0 && is_const,
                is_volatile: false,
                is_restrict: false,
            })
            .collect();

        (specs, derived)
    }

    /// C 基本型文字列をパース
    fn parse_c_base_type(s: &str, interner: &crate::intern::StringInterner) -> CTypeSpecs {
        match s {
            "void" => CTypeSpecs::Void,
            "char" => CTypeSpecs::Char { signed: None },
            "signed char" => CTypeSpecs::Char { signed: Some(true) },
            "unsigned char" => CTypeSpecs::Char { signed: Some(false) },
            "short" | "short int" | "signed short" | "signed short int" => {
                CTypeSpecs::Int { signed: true, size: IntSize::Short }
            }
            "unsigned short" | "unsigned short int" => {
                CTypeSpecs::Int { signed: false, size: IntSize::Short }
            }
            "int" | "signed" | "signed int" => {
                CTypeSpecs::Int { signed: true, size: IntSize::Int }
            }
            "unsigned" | "unsigned int" => {
                CTypeSpecs::Int { signed: false, size: IntSize::Int }
            }
            "long" | "long int" | "signed long" | "signed long int" => {
                CTypeSpecs::Int { signed: true, size: IntSize::Long }
            }
            "unsigned long" | "unsigned long int" => {
                CTypeSpecs::Int { signed: false, size: IntSize::Long }
            }
            "long long" | "long long int" | "signed long long" | "signed long long int" => {
                CTypeSpecs::Int { signed: true, size: IntSize::LongLong }
            }
            "unsigned long long" | "unsigned long long int" => {
                CTypeSpecs::Int { signed: false, size: IntSize::LongLong }
            }
            "float" => CTypeSpecs::Float,
            "double" => CTypeSpecs::Double { is_long: false },
            "long double" => CTypeSpecs::Double { is_long: true },
            "_Bool" | "bool" => CTypeSpecs::Bool,
            _ => {
                // 構造体/共用体/typedef 名として扱う
                if let Some(rest) = s.strip_prefix("struct ") {
                    if let Some(name) = interner.lookup(rest.trim()) {
                        return CTypeSpecs::Struct { name: Some(name), is_union: false };
                    }
                    return CTypeSpecs::Struct { name: None, is_union: false };
                }
                if let Some(rest) = s.strip_prefix("union ") {
                    if let Some(name) = interner.lookup(rest.trim()) {
                        return CTypeSpecs::Struct { name: Some(name), is_union: true };
                    }
                    return CTypeSpecs::Struct { name: None, is_union: true };
                }
                if let Some(rest) = s.strip_prefix("enum ") {
                    if let Some(name) = interner.lookup(rest.trim()) {
                        return CTypeSpecs::Enum { name: Some(name) };
                    }
                    return CTypeSpecs::Enum { name: None };
                }
                // typedef 名
                if let Some(name) = interner.lookup(s) {
                    CTypeSpecs::TypedefName(name)
                } else {
                    // 未知の型は typedef 名として扱う（文字列で保持できないので）
                    // この場合は interner に登録されていないため、後で解決する必要がある
                    CTypeSpecs::Void // フォールバック
                }
            }
        }
    }

    /// 後方互換: 文字列に変換（デバッグ用）
    pub fn to_display_string(&self, interner: &crate::intern::StringInterner) -> String {
        match self {
            TypeRepr::CType { specs, derived, .. } => {
                let base = specs.to_display_string(interner);
                let mut result = base;
                for d in derived {
                    match d {
                        CDerivedType::Pointer { is_const: true, .. } => result.push_str(" *const"),
                        CDerivedType::Pointer { .. } => result.push_str(" *"),
                        CDerivedType::Array { size: Some(n) } => {
                            result.push_str(&format!("[{}]", n));
                        }
                        CDerivedType::Array { size: None } => result.push_str("[]"),
                        CDerivedType::Function { .. } => result.push_str("()"),
                    }
                }
                result
            }
            TypeRepr::RustType { repr, .. } => repr.to_display_string(),
            TypeRepr::Inferred(inferred) => inferred.to_display_string(interner),
        }
    }
}

// ============================================================================
// Display 実装
// ============================================================================

impl CTypeSpecs {
    /// 表示用文字列に変換
    pub fn to_display_string(&self, interner: &crate::intern::StringInterner) -> String {
        match self {
            CTypeSpecs::Void => "void".to_string(),
            CTypeSpecs::Char { signed: None } => "char".to_string(),
            CTypeSpecs::Char { signed: Some(true) } => "signed char".to_string(),
            CTypeSpecs::Char { signed: Some(false) } => "unsigned char".to_string(),
            CTypeSpecs::Int { signed: true, size: IntSize::Short } => "short".to_string(),
            CTypeSpecs::Int { signed: false, size: IntSize::Short } => "unsigned short".to_string(),
            CTypeSpecs::Int { signed: true, size: IntSize::Int } => "int".to_string(),
            CTypeSpecs::Int { signed: false, size: IntSize::Int } => "unsigned int".to_string(),
            CTypeSpecs::Int { signed: true, size: IntSize::Long } => "long".to_string(),
            CTypeSpecs::Int { signed: false, size: IntSize::Long } => "unsigned long".to_string(),
            CTypeSpecs::Int { signed: true, size: IntSize::LongLong } => "long long".to_string(),
            CTypeSpecs::Int { signed: false, size: IntSize::LongLong } => "unsigned long long".to_string(),
            CTypeSpecs::Int { signed: true, size: IntSize::Int128 } => "__int128".to_string(),
            CTypeSpecs::Int { signed: false, size: IntSize::Int128 } => "unsigned __int128".to_string(),
            CTypeSpecs::Float => "float".to_string(),
            CTypeSpecs::Double { is_long: false } => "double".to_string(),
            CTypeSpecs::Double { is_long: true } => "long double".to_string(),
            CTypeSpecs::Bool => "_Bool".to_string(),
            CTypeSpecs::Struct { name: Some(n), is_union: false } => {
                format!("struct {}", interner.get(*n))
            }
            CTypeSpecs::Struct { name: None, is_union: false } => "struct".to_string(),
            CTypeSpecs::Struct { name: Some(n), is_union: true } => {
                format!("union {}", interner.get(*n))
            }
            CTypeSpecs::Struct { name: None, is_union: true } => "union".to_string(),
            CTypeSpecs::Enum { name: Some(n) } => format!("enum {}", interner.get(*n)),
            CTypeSpecs::Enum { name: None } => "enum".to_string(),
            CTypeSpecs::TypedefName(n) => interner.get(*n).to_string(),
        }
    }
}

impl RustTypeRepr {
    /// 表示用文字列に変換
    pub fn to_display_string(&self) -> String {
        match self {
            RustTypeRepr::CPrimitive(kind) => kind.to_string(),
            RustTypeRepr::RustPrimitive(kind) => kind.to_string(),
            RustTypeRepr::Pointer { inner, is_const: true } => {
                format!("*const {}", inner.to_display_string())
            }
            RustTypeRepr::Pointer { inner, is_const: false } => {
                format!("*mut {}", inner.to_display_string())
            }
            RustTypeRepr::Reference { inner, is_mut: true } => {
                format!("&mut {}", inner.to_display_string())
            }
            RustTypeRepr::Reference { inner, is_mut: false } => {
                format!("&{}", inner.to_display_string())
            }
            RustTypeRepr::Named(name) => name.clone(),
            RustTypeRepr::Option(inner) => format!("Option<{}>", inner.to_display_string()),
            RustTypeRepr::FnPointer { params, ret } => {
                let params_str: Vec<_> = params.iter().map(|p| p.to_display_string()).collect();
                let ret_str = ret
                    .as_ref()
                    .map(|r| format!(" -> {}", r.to_display_string()))
                    .unwrap_or_default();
                format!("fn({}){}", params_str.join(", "), ret_str)
            }
            RustTypeRepr::Unit => "()".to_string(),
            RustTypeRepr::Unknown(s) => s.clone(),
        }
    }
}

impl fmt::Display for CPrimitiveKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            CPrimitiveKind::CChar => "c_char",
            CPrimitiveKind::CSchar => "c_schar",
            CPrimitiveKind::CUchar => "c_uchar",
            CPrimitiveKind::CShort => "c_short",
            CPrimitiveKind::CUshort => "c_ushort",
            CPrimitiveKind::CInt => "c_int",
            CPrimitiveKind::CUint => "c_uint",
            CPrimitiveKind::CLong => "c_long",
            CPrimitiveKind::CUlong => "c_ulong",
            CPrimitiveKind::CLongLong => "c_longlong",
            CPrimitiveKind::CUlongLong => "c_ulonglong",
            CPrimitiveKind::CFloat => "c_float",
            CPrimitiveKind::CDouble => "c_double",
        };
        write!(f, "{}", s)
    }
}

impl fmt::Display for RustPrimitiveKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            RustPrimitiveKind::I8 => "i8",
            RustPrimitiveKind::I16 => "i16",
            RustPrimitiveKind::I32 => "i32",
            RustPrimitiveKind::I64 => "i64",
            RustPrimitiveKind::I128 => "i128",
            RustPrimitiveKind::Isize => "isize",
            RustPrimitiveKind::U8 => "u8",
            RustPrimitiveKind::U16 => "u16",
            RustPrimitiveKind::U32 => "u32",
            RustPrimitiveKind::U64 => "u64",
            RustPrimitiveKind::U128 => "u128",
            RustPrimitiveKind::Usize => "usize",
            RustPrimitiveKind::F32 => "f32",
            RustPrimitiveKind::F64 => "f64",
            RustPrimitiveKind::Bool => "bool",
        };
        write!(f, "{}", s)
    }
}

impl InferredType {
    /// 表示用文字列に変換
    pub fn to_display_string(&self, interner: &crate::intern::StringInterner) -> String {
        match self {
            InferredType::IntLiteral => "int".to_string(),
            InferredType::UIntLiteral => "unsigned int".to_string(),
            InferredType::FloatLiteral => "double".to_string(),
            InferredType::CharLiteral => "int".to_string(),
            InferredType::StringLiteral => "char *".to_string(),
            InferredType::SymbolLookup { resolved_type, .. } => {
                resolved_type.to_display_string(interner)
            }
            InferredType::ThxDefault => "*mut PerlInterpreter".to_string(),
            InferredType::BinaryOp { result_type, .. } => result_type.to_display_string(interner),
            InferredType::UnaryArithmetic { inner_type } => inner_type.to_display_string(interner),
            InferredType::LogicalNot => "int".to_string(),
            InferredType::AddressOf { inner_type } => {
                format!("{} *", inner_type.to_display_string(interner))
            }
            InferredType::Dereference { pointer_type } => {
                let s = pointer_type.to_display_string(interner);
                s.trim_end_matches(" *").to_string()
            }
            InferredType::IncDec { inner_type } => inner_type.to_display_string(interner),
            InferredType::MemberAccess { field_type: Some(ft), .. } => {
                ft.to_display_string(interner)
            }
            InferredType::MemberAccess { base_type, member, .. } => {
                format!("{}.{}", base_type, interner.get(*member))
            }
            InferredType::PtrMemberAccess { field_type: Some(ft), .. } => {
                ft.to_display_string(interner)
            }
            InferredType::PtrMemberAccess { base_type, member, .. } => {
                format!("{}->{}", base_type, interner.get(*member))
            }
            InferredType::ArraySubscript { element_type, .. } => {
                element_type.to_display_string(interner)
            }
            InferredType::Conditional { result_type, .. } => {
                result_type.to_display_string(interner)
            }
            InferredType::Comma { rhs_type } => rhs_type.to_display_string(interner),
            InferredType::Assignment { lhs_type } => lhs_type.to_display_string(interner),
            InferredType::Cast { target_type } => target_type.to_display_string(interner),
            InferredType::Sizeof | InferredType::Alignof => "unsigned long".to_string(),
            InferredType::CompoundLiteral { type_name } => type_name.to_display_string(interner),
            InferredType::StmtExpr { last_expr_type: Some(t) } => t.to_display_string(interner),
            InferredType::StmtExpr { last_expr_type: None } => "void".to_string(),
            InferredType::Assert => "void".to_string(),
            InferredType::FunctionReturn { func_name } => {
                format!("{}()", interner.get(*func_name))
            }
        }
    }
}

// ============================================================================
// テスト
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rust_type_repr_from_string() {
        assert!(matches!(
            RustTypeRepr::from_type_string("c_int"),
            RustTypeRepr::CPrimitive(CPrimitiveKind::CInt)
        ));

        assert!(matches!(
            RustTypeRepr::from_type_string("i32"),
            RustTypeRepr::RustPrimitive(RustPrimitiveKind::I32)
        ));

        assert!(matches!(
            RustTypeRepr::from_type_string("()"),
            RustTypeRepr::Unit
        ));

        if let RustTypeRepr::Pointer { inner, is_const: false } =
            RustTypeRepr::from_type_string("*mut SV")
        {
            assert!(matches!(*inner, RustTypeRepr::Named(ref n) if n == "SV"));
        } else {
            panic!("Expected *mut SV");
        }

        if let RustTypeRepr::Pointer { inner, is_const: true } =
            RustTypeRepr::from_type_string("*const c_char")
        {
            assert!(matches!(*inner, RustTypeRepr::CPrimitive(CPrimitiveKind::CChar)));
        } else {
            panic!("Expected *const c_char");
        }
    }

    #[test]
    fn test_rust_type_repr_from_string_with_spaces() {
        // syn の出力形式（スペースあり）
        if let RustTypeRepr::Pointer { inner, is_const: false } =
            RustTypeRepr::from_type_string("* mut SV")
        {
            assert!(matches!(*inner, RustTypeRepr::Named(ref n) if n == "SV"));
        } else {
            panic!("Expected * mut SV");
        }
    }

    #[test]
    fn test_c_primitive_display() {
        assert_eq!(CPrimitiveKind::CInt.to_string(), "c_int");
        assert_eq!(CPrimitiveKind::CUlong.to_string(), "c_ulong");
    }

    #[test]
    fn test_rust_primitive_display() {
        assert_eq!(RustPrimitiveKind::I32.to_string(), "i32");
        assert_eq!(RustPrimitiveKind::Usize.to_string(), "usize");
    }
}
