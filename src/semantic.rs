//! 意味解析モジュール
//!
//! スコープ管理と型推論を行う。

use std::collections::HashMap;

use crate::apidoc::ApidocDict;
use crate::ast::*;
use crate::fields_dict::FieldsDict;
use crate::intern::{InternedStr, StringInterner};
use crate::source::SourceLocation;

/// 解決済み型
#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    /// void
    Void,
    /// char
    Char,
    /// signed char
    SignedChar,
    /// unsigned char
    UnsignedChar,
    /// short
    Short,
    /// unsigned short
    UnsignedShort,
    /// int
    Int,
    /// unsigned int
    UnsignedInt,
    /// long
    Long,
    /// unsigned long
    UnsignedLong,
    /// long long
    LongLong,
    /// unsigned long long
    UnsignedLongLong,
    /// float
    Float,
    /// double
    Double,
    /// long double
    LongDouble,
    /// _Bool
    Bool,
    /// __int128
    Int128,
    /// unsigned __int128
    UnsignedInt128,
    /// ポインタ型
    Pointer(Box<Type>, TypeQualifiers),
    /// 配列型
    Array(Box<Type>, Option<usize>),
    /// 関数型
    Function {
        return_type: Box<Type>,
        params: Vec<Type>,
        variadic: bool,
    },
    /// 構造体型
    Struct {
        name: Option<InternedStr>,
        /// メンバー (名前, 型)
        members: Option<Vec<(InternedStr, Type)>>,
    },
    /// 共用体型
    Union {
        name: Option<InternedStr>,
        members: Option<Vec<(InternedStr, Type)>>,
    },
    /// 列挙型
    Enum {
        name: Option<InternedStr>,
    },
    /// typedef名（未解決）
    TypedefName(InternedStr),
    /// 不明な型（エラー時）
    Unknown,
}

impl Type {
    /// 型を人間が読める形式で表示
    pub fn display(&self, interner: &StringInterner) -> String {
        match self {
            Type::Void => "void".to_string(),
            Type::Char => "char".to_string(),
            Type::SignedChar => "signed char".to_string(),
            Type::UnsignedChar => "unsigned char".to_string(),
            Type::Short => "short".to_string(),
            Type::UnsignedShort => "unsigned short".to_string(),
            Type::Int => "int".to_string(),
            Type::UnsignedInt => "unsigned int".to_string(),
            Type::Long => "long".to_string(),
            Type::UnsignedLong => "unsigned long".to_string(),
            Type::LongLong => "long long".to_string(),
            Type::UnsignedLongLong => "unsigned long long".to_string(),
            Type::Float => "float".to_string(),
            Type::Double => "double".to_string(),
            Type::LongDouble => "long double".to_string(),
            Type::Bool => "_Bool".to_string(),
            Type::Int128 => "__int128".to_string(),
            Type::UnsignedInt128 => "unsigned __int128".to_string(),
            Type::Pointer(inner, quals) => {
                let mut s = inner.display(interner);
                s.push('*');
                if quals.is_const {
                    s.push_str(" const");
                }
                if quals.is_volatile {
                    s.push_str(" volatile");
                }
                if quals.is_restrict {
                    s.push_str(" restrict");
                }
                s
            }
            Type::Array(inner, size) => {
                let inner_s = inner.display(interner);
                match size {
                    Some(n) => format!("{}[{}]", inner_s, n),
                    None => format!("{}[]", inner_s),
                }
            }
            Type::Function { return_type, params, variadic } => {
                let params_s: Vec<_> = params.iter()
                    .map(|p| p.display(interner))
                    .collect();
                let mut s = format!("(function {} ({}))", return_type.display(interner), params_s.join(", "));
                if *variadic {
                    s = s.replace("))", ", ...))");
                }
                s
            }
            Type::Struct { name, .. } => {
                match name {
                    Some(n) => format!("struct {}", interner.get(*n)),
                    None => "struct <anonymous>".to_string(),
                }
            }
            Type::Union { name, .. } => {
                match name {
                    Some(n) => format!("union {}", interner.get(*n)),
                    None => "union <anonymous>".to_string(),
                }
            }
            Type::Enum { name } => {
                match name {
                    Some(n) => format!("enum {}", interner.get(*n)),
                    None => "enum <anonymous>".to_string(),
                }
            }
            Type::TypedefName(name) => interner.get(*name).to_string(),
            Type::Unknown => "<unknown>".to_string(),
        }
    }

    /// 整数型かどうか
    pub fn is_integer(&self) -> bool {
        matches!(
            self,
            Type::Char
                | Type::SignedChar
                | Type::UnsignedChar
                | Type::Short
                | Type::UnsignedShort
                | Type::Int
                | Type::UnsignedInt
                | Type::Long
                | Type::UnsignedLong
                | Type::LongLong
                | Type::UnsignedLongLong
                | Type::Bool
                | Type::Int128
                | Type::UnsignedInt128
                | Type::Enum { .. }
        )
    }

    /// 浮動小数点型かどうか
    pub fn is_floating(&self) -> bool {
        matches!(self, Type::Float | Type::Double | Type::LongDouble)
    }

    /// 算術型かどうか
    pub fn is_arithmetic(&self) -> bool {
        self.is_integer() || self.is_floating()
    }

    /// ポインタ型かどうか
    pub fn is_pointer(&self) -> bool {
        matches!(self, Type::Pointer(_, _))
    }
}

/// シンボル情報
#[derive(Debug, Clone)]
pub struct Symbol {
    /// 名前
    pub name: InternedStr,
    /// 型
    pub ty: Type,
    /// 定義位置
    pub loc: SourceLocation,
    /// シンボルの種類
    pub kind: SymbolKind,
}

/// シンボルの種類
#[derive(Debug, Clone, PartialEq)]
pub enum SymbolKind {
    /// 変数
    Variable,
    /// 関数
    Function,
    /// typedef
    Typedef,
    /// 列挙定数
    EnumConstant(i64),
}

/// スコープ
#[derive(Debug)]
pub struct Scope {
    /// シンボルテーブル (名前 -> シンボル)
    symbols: HashMap<InternedStr, Symbol>,
    /// 親スコープID (グローバルスコープはNone)
    parent: Option<ScopeId>,
}

impl Scope {
    fn new(parent: Option<ScopeId>) -> Self {
        Self {
            symbols: HashMap::new(),
            parent,
        }
    }
}

/// スコープID
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScopeId(usize);

/// 意味解析器
pub struct SemanticAnalyzer<'a> {
    /// 文字列インターナー
    interner: &'a StringInterner,
    /// スコープスタック
    scopes: Vec<Scope>,
    /// 現在のスコープID
    current_scope: ScopeId,
    /// 構造体定義 (名前 -> メンバーリスト)
    struct_defs: HashMap<InternedStr, Vec<(InternedStr, Type)>>,
    /// 共用体定義
    union_defs: HashMap<InternedStr, Vec<(InternedStr, Type)>>,
    /// typedef定義 (名前 -> 型)
    typedef_defs: HashMap<InternedStr, Type>,
    /// Apidoc辞書（関数/マクロのシグネチャ情報）
    apidoc: Option<&'a ApidocDict>,
    /// フィールド辞書（構造体フィールドの型情報）
    fields_dict: Option<&'a FieldsDict>,
}

impl<'a> SemanticAnalyzer<'a> {
    /// 新しい意味解析器を作成
    pub fn new(
        interner: &'a StringInterner,
        apidoc: Option<&'a ApidocDict>,
        fields_dict: Option<&'a FieldsDict>,
    ) -> Self {
        let global_scope = Scope::new(None);
        Self {
            interner,
            scopes: vec![global_scope],
            current_scope: ScopeId(0),
            struct_defs: HashMap::new(),
            union_defs: HashMap::new(),
            typedef_defs: HashMap::new(),
            apidoc,
            fields_dict,
        }
    }

    /// 新しいスコープを開始
    pub fn push_scope(&mut self) {
        let new_scope = Scope::new(Some(self.current_scope));
        let new_id = ScopeId(self.scopes.len());
        self.scopes.push(new_scope);
        self.current_scope = new_id;
    }

    /// 現在のスコープを終了
    pub fn pop_scope(&mut self) {
        if let Some(parent) = self.scopes[self.current_scope.0].parent {
            self.current_scope = parent;
        }
    }

    /// シンボルを現在のスコープに追加
    pub fn define_symbol(&mut self, symbol: Symbol) {
        let scope = &mut self.scopes[self.current_scope.0];
        scope.symbols.insert(symbol.name, symbol);
    }

    /// シンボルを検索（現在のスコープから親スコープへ）
    pub fn lookup_symbol(&self, name: InternedStr) -> Option<&Symbol> {
        let mut scope_id = Some(self.current_scope);
        while let Some(id) = scope_id {
            let scope = &self.scopes[id.0];
            if let Some(sym) = scope.symbols.get(&name) {
                return Some(sym);
            }
            scope_id = scope.parent;
        }
        None
    }

    /// DeclSpecs から Type を構築
    pub fn resolve_decl_specs(&mut self, specs: &DeclSpecs) -> Type {
        // 型指定子を集める
        let mut is_signed = false;
        let mut is_unsigned = false;
        let mut is_short = false;
        let mut is_long = 0u8; // longの個数
        let mut base_type: Option<Type> = None;

        for spec in &specs.type_specs {
            match spec {
                TypeSpec::Void => base_type = Some(Type::Void),
                TypeSpec::Char => base_type = Some(Type::Char),
                TypeSpec::Short => is_short = true,
                TypeSpec::Int => {
                    if base_type.is_none() {
                        base_type = Some(Type::Int);
                    }
                }
                TypeSpec::Long => is_long += 1,
                TypeSpec::Float => base_type = Some(Type::Float),
                TypeSpec::Double => base_type = Some(Type::Double),
                TypeSpec::Signed => is_signed = true,
                TypeSpec::Unsigned => is_unsigned = true,
                TypeSpec::Bool => base_type = Some(Type::Bool),
                TypeSpec::Int128 => base_type = Some(Type::Int128),
                TypeSpec::Struct(s) => {
                    let members = self.resolve_struct_members(s);
                    if let (Some(name), Some(m)) = (s.name, &members) {
                        self.struct_defs.insert(name, m.clone());
                    }
                    base_type = Some(Type::Struct {
                        name: s.name,
                        members,
                    });
                }
                TypeSpec::Union(s) => {
                    let members = self.resolve_struct_members(s);
                    if let (Some(name), Some(m)) = (s.name, &members) {
                        self.union_defs.insert(name, m.clone());
                    }
                    base_type = Some(Type::Union {
                        name: s.name,
                        members,
                    });
                }
                TypeSpec::Enum(e) => {
                    // 列挙定数を登録
                    self.process_enum(e);
                    base_type = Some(Type::Enum { name: e.name });
                }
                TypeSpec::TypedefName(name) => {
                    // typedefを解決
                    if let Some(ty) = self.typedef_defs.get(name) {
                        base_type = Some(ty.clone());
                    } else {
                        base_type = Some(Type::TypedefName(*name));
                    }
                }
                TypeSpec::TypeofExpr(_) => {
                    // TODO: typeof式の型を推論
                    base_type = Some(Type::Unknown);
                }
                _ => {}
            }
        }

        // 型修飾子を組み合わせる
        match (is_unsigned, is_signed, is_short, is_long, &base_type) {
            // unsigned指定
            (true, _, _, 0, None) | (true, _, _, 0, Some(Type::Int)) => Type::UnsignedInt,
            (true, _, _, 1, _) => Type::UnsignedLong,
            (true, _, _, 2, _) => Type::UnsignedLongLong,
            (true, _, true, _, _) => Type::UnsignedShort,
            (true, _, _, _, Some(Type::Char)) => Type::UnsignedChar,
            (true, _, _, _, Some(Type::Int128)) => Type::UnsignedInt128,
            // signed指定
            (_, true, _, _, Some(Type::Char)) => Type::SignedChar,
            // long指定
            (_, _, _, 1, None) | (_, _, _, 1, Some(Type::Int)) => Type::Long,
            (_, _, _, 2, _) => Type::LongLong,
            (_, _, _, 1, Some(Type::Double)) => Type::LongDouble,
            // short指定
            (_, _, true, _, _) => Type::Short,
            // デフォルト
            _ => base_type.unwrap_or(Type::Int),
        }
    }

    /// 構造体メンバーを解決
    fn resolve_struct_members(&mut self, spec: &StructSpec) -> Option<Vec<(InternedStr, Type)>> {
        spec.members.as_ref().map(|members| {
            let mut result = Vec::new();
            for member in members {
                let base_ty = self.resolve_decl_specs(&member.specs);
                for decl in &member.declarators {
                    if let Some(ref d) = decl.declarator {
                        if let Some(name) = d.name {
                            let ty = self.apply_declarator(&base_ty, d);
                            result.push((name, ty));
                        }
                    }
                }
            }
            result
        })
    }

    /// 列挙型を処理
    fn process_enum(&mut self, spec: &EnumSpec) {
        if let Some(ref enumerators) = spec.enumerators {
            let mut value = 0i64;
            for e in enumerators {
                if e.value.is_some() {
                    // TODO: 定数式を評価
                    value = 0; // 仮
                }
                self.define_symbol(Symbol {
                    name: e.name,
                    ty: Type::Int,
                    loc: spec.loc.clone(),
                    kind: SymbolKind::EnumConstant(value),
                });
                value += 1;
            }
        }
    }

    /// Declarator を適用して型を構築
    pub fn apply_declarator(&self, base_type: &Type, decl: &Declarator) -> Type {
        let mut ty = base_type.clone();

        for derived in &decl.derived {
            ty = match derived {
                DerivedDecl::Pointer(quals) => Type::Pointer(Box::new(ty), quals.clone()),
                DerivedDecl::Array(arr) => {
                    // TODO: サイズを評価
                    let _size = &arr.size;
                    Type::Array(Box::new(ty), None)
                }
                DerivedDecl::Function(params) => {
                    let param_types: Vec<_> = params.params
                        .iter()
                        .map(|p| {
                            let base = self.resolve_decl_specs_readonly(&p.specs);
                            if let Some(ref d) = p.declarator {
                                self.apply_declarator(&base, d)
                            } else {
                                base
                            }
                        })
                        .collect();
                    Type::Function {
                        return_type: Box::new(ty),
                        params: param_types,
                        variadic: params.is_variadic,
                    }
                }
            };
        }

        ty
    }

    /// DeclSpecs を読み取り専用で解決（再帰呼び出し用）
    fn resolve_decl_specs_readonly(&self, specs: &DeclSpecs) -> Type {
        // 簡略版: 主要な型のみ処理
        let mut is_unsigned = false;
        let mut is_long = 0u8;
        let mut base_type: Option<Type> = None;

        for spec in &specs.type_specs {
            match spec {
                TypeSpec::Void => base_type = Some(Type::Void),
                TypeSpec::Char => base_type = Some(Type::Char),
                TypeSpec::Int => base_type = Some(Type::Int),
                TypeSpec::Long => is_long += 1,
                TypeSpec::Float => base_type = Some(Type::Float),
                TypeSpec::Double => base_type = Some(Type::Double),
                TypeSpec::Unsigned => is_unsigned = true,
                TypeSpec::Bool => base_type = Some(Type::Bool),
                TypeSpec::TypedefName(name) => {
                    if let Some(ty) = self.typedef_defs.get(name) {
                        base_type = Some(ty.clone());
                    } else {
                        base_type = Some(Type::TypedefName(*name));
                    }
                }
                _ => {}
            }
        }

        match (is_unsigned, is_long, &base_type) {
            (true, 0, None) | (true, 0, Some(Type::Int)) => Type::UnsignedInt,
            (true, 1, _) => Type::UnsignedLong,
            (_, 1, None) | (_, 1, Some(Type::Int)) => Type::Long,
            (_, 2, _) => Type::LongLong,
            _ => base_type.unwrap_or(Type::Int),
        }
    }

    /// 式の型を推論
    pub fn infer_expr_type(&self, expr: &Expr) -> Type {
        match expr {
            // リテラル
            Expr::IntLit(_, _) => Type::Int,
            Expr::UIntLit(_, _) => Type::UnsignedInt,
            Expr::FloatLit(_, _) => Type::Double, // C のデフォルト
            Expr::CharLit(_, _) => Type::Int,     // char literalはint
            Expr::StringLit(_, _) => Type::Pointer(Box::new(Type::Char), TypeQualifiers::default()),

            // 識別子
            Expr::Ident(name, _) => {
                if let Some(sym) = self.lookup_symbol(*name) {
                    sym.ty.clone()
                } else {
                    Type::Unknown
                }
            }

            // 後置演算子
            Expr::PostInc(inner, _) | Expr::PostDec(inner, _) => self.infer_expr_type(inner),

            // 前置演算子
            Expr::PreInc(inner, _) | Expr::PreDec(inner, _) => self.infer_expr_type(inner),
            Expr::AddrOf(inner, _) => {
                let inner_ty = self.infer_expr_type(inner);
                Type::Pointer(Box::new(inner_ty), TypeQualifiers::default())
            }
            Expr::Deref(inner, _) => {
                let inner_ty = self.infer_expr_type(inner);
                if let Type::Pointer(elem, _) = inner_ty {
                    *elem
                } else {
                    Type::Unknown
                }
            }
            Expr::UnaryPlus(inner, _) | Expr::UnaryMinus(inner, _) => self.infer_expr_type(inner),
            Expr::BitNot(inner, _) => self.infer_expr_type(inner),
            Expr::LogNot(_, _) => Type::Int,
            Expr::Sizeof(_, _) | Expr::SizeofType(_, _) => Type::UnsignedLong,
            Expr::Alignof(_, _) => Type::UnsignedLong,

            // キャスト
            Expr::Cast { type_name, .. } => self.resolve_type_name(type_name),

            // 二項演算子
            Expr::Binary { op, lhs, rhs, .. } => {
                let lhs_ty = self.infer_expr_type(lhs);
                let rhs_ty = self.infer_expr_type(rhs);
                match op {
                    // 比較演算子は int を返す
                    BinOp::Lt
                    | BinOp::Gt
                    | BinOp::Le
                    | BinOp::Ge
                    | BinOp::Eq
                    | BinOp::Ne
                    | BinOp::LogAnd
                    | BinOp::LogOr => Type::Int,
                    // 算術演算子: 通常の型昇格
                    _ => self.usual_arithmetic_conversion(&lhs_ty, &rhs_ty),
                }
            }

            // 条件演算子
            Expr::Conditional { then_expr, else_expr, .. } => {
                let then_ty = self.infer_expr_type(then_expr);
                let else_ty = self.infer_expr_type(else_expr);
                self.usual_arithmetic_conversion(&then_ty, &else_ty)
            }

            // 関数呼び出し
            Expr::Call { func, .. } => {
                let func_ty = self.infer_expr_type(func);
                if let Type::Function { return_type, .. } = func_ty {
                    return *return_type;
                }
                if let Type::Pointer(inner, _) = func_ty {
                    if let Type::Function { return_type, .. } = *inner {
                        return *return_type;
                    }
                }
                // シンボルテーブルで見つからない場合、ApidocDict を検索
                if let Expr::Ident(func_name, _) = func.as_ref() {
                    if let Some(ret_ty) = self.lookup_apidoc_return_type(*func_name) {
                        return ret_ty;
                    }
                }
                Type::Unknown
            }

            // メンバーアクセス
            Expr::Member { expr: base, member, .. } => {
                let base_ty = self.infer_expr_type(base);
                self.lookup_member_type(&base_ty, *member)
            }

            // ポインタメンバーアクセス
            Expr::PtrMember { expr: base, member, .. } => {
                let base_ty = self.infer_expr_type(base);
                if let Type::Pointer(inner, _) = base_ty {
                    self.lookup_member_type(&inner, *member)
                } else {
                    Type::Unknown
                }
            }

            // 配列添字
            Expr::Index { expr: base, .. } => {
                let base_ty = self.infer_expr_type(base);
                match base_ty {
                    Type::Pointer(inner, _) => *inner,
                    Type::Array(inner, _) => *inner,
                    _ => Type::Unknown,
                }
            }

            // 代入演算子
            Expr::Assign { lhs, .. } => self.infer_expr_type(lhs),

            // コンマ演算子
            Expr::Comma { rhs, .. } => self.infer_expr_type(rhs),

            // 複合リテラル
            Expr::CompoundLit { type_name, .. } => self.resolve_type_name(type_name),

            // Statement Expression (GCC拡張)
            Expr::StmtExpr(compound, _) => {
                // 最後の式文の型を返す
                if let Some(last) = compound.items.last() {
                    if let BlockItem::Stmt(Stmt::Expr(Some(expr), _)) = last {
                        return self.infer_expr_type(expr);
                    }
                }
                Type::Void
            }
        }
    }

    /// TypeName から型を解決
    fn resolve_type_name(&self, type_name: &TypeName) -> Type {
        let base_ty = self.resolve_decl_specs_readonly(&type_name.specs);
        if let Some(ref abs_decl) = type_name.declarator {
            self.apply_abstract_declarator(&base_ty, abs_decl)
        } else {
            base_ty
        }
    }

    /// AbstractDeclarator を適用して型を構築
    fn apply_abstract_declarator(&self, base_type: &Type, decl: &AbstractDeclarator) -> Type {
        let mut ty = base_type.clone();

        for derived in &decl.derived {
            ty = match derived {
                DerivedDecl::Pointer(quals) => Type::Pointer(Box::new(ty), quals.clone()),
                DerivedDecl::Array(_) => {
                    Type::Array(Box::new(ty), None)
                }
                DerivedDecl::Function(params) => {
                    let param_types: Vec<_> = params.params
                        .iter()
                        .map(|p| {
                            let base = self.resolve_decl_specs_readonly(&p.specs);
                            if let Some(ref d) = p.declarator {
                                self.apply_declarator(&base, d)
                            } else {
                                base
                            }
                        })
                        .collect();
                    Type::Function {
                        return_type: Box::new(ty),
                        params: param_types,
                        variadic: params.is_variadic,
                    }
                }
            };
        }

        ty
    }

    /// メンバーの型を検索
    fn lookup_member_type(&self, base_ty: &Type, member: InternedStr) -> Type {
        match base_ty {
            Type::Struct { name, members } => {
                // まず直接のメンバーを探す
                if let Some(m) = members {
                    for (n, ty) in m {
                        if *n == member {
                            return ty.clone();
                        }
                    }
                }
                // 名前付き構造体なら定義を探す
                if let Some(name) = name {
                    if let Some(m) = self.struct_defs.get(name) {
                        for (n, ty) in m {
                            if *n == member {
                                return ty.clone();
                            }
                        }
                    }
                }
                // FieldsDictから検索
                self.lookup_field_type_from_dict(member)
            }
            Type::Union { name, members } => {
                if let Some(m) = members {
                    for (n, ty) in m {
                        if *n == member {
                            return ty.clone();
                        }
                    }
                }
                if let Some(name) = name {
                    if let Some(m) = self.union_defs.get(name) {
                        for (n, ty) in m {
                            if *n == member {
                                return ty.clone();
                            }
                        }
                    }
                }
                // FieldsDictから検索
                self.lookup_field_type_from_dict(member)
            }
            // 基底型が不明な場合もFieldsDictを試す
            Type::TypedefName(_) | Type::Unknown => {
                self.lookup_field_type_from_dict(member)
            }
            _ => Type::Unknown,
        }
    }

    /// FieldsDictからフィールド型を検索
    fn lookup_field_type_from_dict(&self, field: InternedStr) -> Type {
        if let Some(fields_dict) = self.fields_dict {
            if let Some(field_type) = fields_dict.get_unique_field_type(field) {
                return self.parse_rust_type_string(&field_type.rust_type);
            }
        }
        Type::Unknown
    }

    /// 通常の算術変換
    fn usual_arithmetic_conversion(&self, lhs: &Type, rhs: &Type) -> Type {
        // 簡略版: 大きい方の型に合わせる
        let rank = |ty: &Type| -> u8 {
            match ty {
                Type::LongDouble => 10,
                Type::Double => 9,
                Type::Float => 8,
                Type::UnsignedLongLong => 7,
                Type::LongLong => 6,
                Type::UnsignedLong => 5,
                Type::Long => 4,
                Type::UnsignedInt => 3,
                Type::Int => 2,
                Type::UnsignedShort => 1,
                Type::Short => 1,
                _ => 0,
            }
        };

        if rank(lhs) >= rank(rhs) {
            lhs.clone()
        } else {
            rhs.clone()
        }
    }

    /// 宣言を処理してシンボルを登録
    pub fn process_declaration(&mut self, decl: &Declaration) {
        let base_ty = self.resolve_decl_specs(&decl.specs);

        // typedefの場合
        if decl.specs.storage == Some(StorageClass::Typedef) {
            for init_decl in &decl.declarators {
                if let Some(name) = init_decl.declarator.name {
                    let ty = self.apply_declarator(&base_ty, &init_decl.declarator);
                    self.typedef_defs.insert(name, ty);
                }
            }
            return;
        }

        // 通常の変数宣言
        for init_decl in &decl.declarators {
            if let Some(name) = init_decl.declarator.name {
                let ty = self.apply_declarator(&base_ty, &init_decl.declarator);
                self.define_symbol(Symbol {
                    name,
                    ty,
                    loc: decl.loc.clone(),
                    kind: SymbolKind::Variable,
                });
            }
        }
    }

    /// 関数定義を処理
    pub fn process_function_def(&mut self, func: &FunctionDef) {
        let return_ty = self.resolve_decl_specs(&func.specs);
        let func_ty = self.apply_declarator(&return_ty, &func.declarator);

        // 関数をグローバルスコープに登録
        if let Some(name) = func.declarator.name {
            self.define_symbol(Symbol {
                name,
                ty: func_ty.clone(),
                loc: func.loc.clone(),
                kind: SymbolKind::Function,
            });
        }

        // 関数本体用のスコープを開始
        self.push_scope();

        // パラメータを登録
        if let Type::Function { params, .. } = &func_ty {
            for derived in &func.declarator.derived {
                if let DerivedDecl::Function(param_list) = derived {
                    for (param, param_ty) in param_list.params.iter().zip(params.iter()) {
                        if let Some(ref decl) = param.declarator {
                            if let Some(name) = decl.name {
                                self.define_symbol(Symbol {
                                    name,
                                    ty: param_ty.clone(),
                                    loc: func.loc.clone(),
                                    kind: SymbolKind::Variable,
                                });
                            }
                        }
                    }
                    break;
                }
            }
        }

        // 関数本体を処理
        self.process_compound_stmt(&func.body);

        self.pop_scope();
    }

    /// 複合文を処理
    pub fn process_compound_stmt(&mut self, stmt: &CompoundStmt) {
        self.push_scope();
        for item in &stmt.items {
            match item {
                BlockItem::Decl(decl) => self.process_declaration(decl),
                BlockItem::Stmt(stmt) => self.process_stmt(stmt),
            }
        }
        self.pop_scope();
    }

    /// 文を処理
    fn process_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Compound(compound) => self.process_compound_stmt(compound),
            Stmt::For { init, .. } => {
                self.push_scope();
                if let Some(ForInit::Decl(decl)) = init {
                    self.process_declaration(decl);
                }
                // TODO: 本体を処理
                self.pop_scope();
            }
            _ => {}
        }
    }

    /// ApidocDict から関数/マクロの戻り値型を検索
    fn lookup_apidoc_return_type(&self, func_name: InternedStr) -> Option<Type> {
        let apidoc = self.apidoc?;
        let func_name_str = self.interner.get(func_name);
        let entry = apidoc.get(func_name_str)?;
        let return_type_str = entry.return_type.as_ref()?;

        // C型文字列をType に変換
        Some(self.parse_c_type_string(return_type_str))
    }

    /// C型文字列を Type に変換
    fn parse_c_type_string(&self, type_str: &str) -> Type {
        let trimmed = type_str.trim();

        // ポインタ型
        if let Some(base) = trimmed.strip_suffix('*') {
            let base = base.trim();
            if let Some(inner) = base.strip_prefix("const ") {
                return Type::Pointer(
                    Box::new(self.parse_c_type_string(inner.trim())),
                    TypeQualifiers { is_const: true, ..Default::default() },
                );
            }
            return Type::Pointer(
                Box::new(self.parse_c_type_string(base)),
                TypeQualifiers::default(),
            );
        }

        // 基本型
        match trimmed {
            "void" => Type::Void,
            "char" => Type::Char,
            "int" => Type::Int,
            "unsigned" | "unsigned int" => Type::UnsignedInt,
            "long" => Type::Long,
            "unsigned long" => Type::UnsignedLong,
            "size_t" | "Size_t" | "STRLEN" => Type::UnsignedLong,
            "SSize_t" => Type::Long,
            "bool" | "_Bool" | "bool_t" => Type::Bool,
            "I32" => Type::Int,
            "U32" => Type::UnsignedInt,
            "IV" => Type::Long,
            "UV" => Type::UnsignedLong,
            "NV" => Type::Double,
            _ => {
                // typedef名として扱う (SV, AV, HV など)
                if let Some(interned) = self.interner.lookup(trimmed) {
                    Type::TypedefName(interned)
                } else {
                    Type::Unknown
                }
            }
        }
    }

    /// Rust型文字列を Type に変換
    fn parse_rust_type_string(&self, type_str: &str) -> Type {
        let trimmed = type_str.trim();

        // ポインタ型
        if let Some(rest) = trimmed.strip_prefix("*mut ") {
            return Type::Pointer(
                Box::new(self.parse_rust_type_string(rest)),
                TypeQualifiers::default(),
            );
        }
        if let Some(rest) = trimmed.strip_prefix("*const ") {
            return Type::Pointer(
                Box::new(self.parse_rust_type_string(rest)),
                TypeQualifiers { is_const: true, ..Default::default() },
            );
        }

        // 基本型
        match trimmed {
            "()" => Type::Void,
            "c_char" => Type::Char,
            "c_int" => Type::Int,
            "c_uint" => Type::UnsignedInt,
            "c_long" => Type::Long,
            "c_ulong" => Type::UnsignedLong,
            "bool" => Type::Bool,
            "usize" => Type::UnsignedLong,
            "isize" => Type::Long,
            _ => {
                // typedef名として扱う
                if let Some(interned) = self.interner.lookup(trimmed) {
                    Type::TypedefName(interned)
                } else {
                    Type::Unknown
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_type_display() {
        let interner = StringInterner::new();

        assert_eq!(Type::Int.display(&interner), "int");
        assert_eq!(Type::UnsignedLong.display(&interner), "unsigned long");
        assert_eq!(
            Type::Pointer(Box::new(Type::Char), TypeQualifiers::default()).display(&interner),
            "char*"
        );
    }

    #[test]
    fn test_scope_management() {
        let mut interner = StringInterner::new();
        let x = interner.intern("x");
        let mut analyzer = SemanticAnalyzer::new(&interner, None, None);

        // グローバルスコープでxを定義
        analyzer.define_symbol(Symbol {
            name: x,
            ty: Type::Int,
            loc: SourceLocation::default(),
            kind: SymbolKind::Variable,
        });

        assert!(analyzer.lookup_symbol(x).is_some());

        // 新しいスコープを開始
        analyzer.push_scope();

        // まだxが見える
        assert!(analyzer.lookup_symbol(x).is_some());

        // ローカルスコープでxをシャドウイング
        analyzer.define_symbol(Symbol {
            name: x,
            ty: Type::Float, // 異なる型
            loc: SourceLocation::default(),
            kind: SymbolKind::Variable,
        });

        // ローカルのxが見える
        let sym = analyzer.lookup_symbol(x).unwrap();
        assert_eq!(sym.ty, Type::Float);

        // スコープを終了
        analyzer.pop_scope();

        // グローバルのxが見える
        let sym = analyzer.lookup_symbol(x).unwrap();
        assert_eq!(sym.ty, Type::Int);
    }
}
