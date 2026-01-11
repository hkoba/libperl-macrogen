//! 意味解析モジュール
//!
//! スコープ管理と型推論を行う。

use std::collections::{HashMap, HashSet};

use crate::apidoc::ApidocDict;
use crate::ast::*;
use crate::fields_dict::FieldsDict;
use crate::intern::{InternedStr, StringInterner};
use crate::parser::parse_type_from_string;
use crate::rust_decl::RustDeclDict;
use crate::source::{FileRegistry, SourceLocation};
use crate::type_env::{ConstraintSource, TypeEnv, TypeConstraint as TypeEnvConstraint};
use crate::unified_type::{IntSize, UnifiedType};

/// 型変数 ID (制約ベース型推論用)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TypeVar(usize);

/// 型制約 (マクロ引数の型推論用)
#[derive(Debug, Clone)]
pub enum TypeConstraint {
    /// 関数呼び出しの引数として使用
    FunctionArg {
        var: TypeVar,
        func_name: InternedStr,
        arg_index: usize,
    },
    /// フィールドアクセスの基底として使用
    HasField {
        var: TypeVar,
        field: InternedStr,
    },
}

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

    /// UnifiedType に変換
    pub fn to_unified(&self, interner: &StringInterner) -> UnifiedType {
        match self {
            Type::Void => UnifiedType::Void,
            Type::Bool => UnifiedType::Bool,

            Type::Char => UnifiedType::Char { signed: None },
            Type::SignedChar => UnifiedType::Char { signed: Some(true) },
            Type::UnsignedChar => UnifiedType::Char { signed: Some(false) },

            Type::Short => UnifiedType::Int { signed: true, size: IntSize::Short },
            Type::UnsignedShort => UnifiedType::Int { signed: false, size: IntSize::Short },
            Type::Int => UnifiedType::Int { signed: true, size: IntSize::Int },
            Type::UnsignedInt => UnifiedType::Int { signed: false, size: IntSize::Int },
            Type::Long => UnifiedType::Int { signed: true, size: IntSize::Long },
            Type::UnsignedLong => UnifiedType::Int { signed: false, size: IntSize::Long },
            Type::LongLong => UnifiedType::Int { signed: true, size: IntSize::LongLong },
            Type::UnsignedLongLong => UnifiedType::Int { signed: false, size: IntSize::LongLong },
            Type::Int128 => UnifiedType::Int { signed: true, size: IntSize::Int128 },
            Type::UnsignedInt128 => UnifiedType::Int { signed: false, size: IntSize::Int128 },

            Type::Float => UnifiedType::Float,
            Type::Double => UnifiedType::Double,
            Type::LongDouble => UnifiedType::LongDouble,

            Type::Pointer(inner, quals) => UnifiedType::Pointer {
                inner: Box::new(inner.to_unified(interner)),
                is_const: quals.is_const,
            },

            Type::Array(inner, size) => UnifiedType::Array {
                inner: Box::new(inner.to_unified(interner)),
                size: *size,
            },

            Type::Struct { name: Some(n), .. } => {
                UnifiedType::Named(interner.get(*n).to_string())
            }
            Type::Struct { name: None, .. } => UnifiedType::Unknown,

            Type::Union { name: Some(n), .. } => {
                UnifiedType::Named(interner.get(*n).to_string())
            }
            Type::Union { name: None, .. } => UnifiedType::Unknown,

            Type::Enum { name: Some(n) } => {
                UnifiedType::Named(interner.get(*n).to_string())
            }
            Type::Enum { name: None } => UnifiedType::Int { signed: true, size: IntSize::Int },

            Type::TypedefName(name) => {
                UnifiedType::Named(interner.get(*name).to_string())
            }

            Type::Function { .. } => UnifiedType::Unknown, // 関数型は未サポート

            Type::Unknown => UnifiedType::Unknown,
        }
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
    /// RustDeclDict への参照 (bindings.rs の関数型情報)
    rust_decl_dict: Option<&'a RustDeclDict>,
    /// 型変数マップ (引数名 -> TypeVar)
    type_vars: HashMap<InternedStr, TypeVar>,
    /// 次の型変数ID
    next_type_var: usize,
    /// 収集された制約
    constraints: Vec<TypeConstraint>,
    /// 制約収集モードか
    constraint_mode: bool,
    /// マクロパラメータ名の集合（型制約収集用）
    macro_params: HashSet<InternedStr>,
}

impl<'a> SemanticAnalyzer<'a> {
    /// 新しい意味解析器を作成
    pub fn new(
        interner: &'a StringInterner,
        apidoc: Option<&'a ApidocDict>,
        fields_dict: Option<&'a FieldsDict>,
    ) -> Self {
        Self::with_rust_decl_dict(interner, apidoc, fields_dict, None)
    }

    /// RustDeclDict を指定して意味解析器を作成
    pub fn with_rust_decl_dict(
        interner: &'a StringInterner,
        apidoc: Option<&'a ApidocDict>,
        fields_dict: Option<&'a FieldsDict>,
        rust_decl_dict: Option<&'a RustDeclDict>,
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
            rust_decl_dict,
            type_vars: HashMap::new(),
            next_type_var: 0,
            constraints: Vec::new(),
            constraint_mode: false,
            macro_params: HashSet::new(),
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

    // ========================================
    // 制約ベース型推論 (マクロ引数用)
    // ========================================

    /// 制約収集モードを開始し、パラメータを型変数として登録
    pub fn begin_param_inference(&mut self, params: &[InternedStr]) {
        self.constraint_mode = true;
        self.type_vars.clear();
        self.constraints.clear();
        self.next_type_var = 0;

        for &param in params {
            let var = TypeVar(self.next_type_var);
            self.next_type_var += 1;
            self.type_vars.insert(param, var);
        }
    }

    /// 制約を解いて引数型を取得し、制約収集モードを終了
    pub fn end_param_inference(&mut self) -> HashMap<InternedStr, Type> {
        self.constraint_mode = false;
        let solutions = self.solve_constraints();

        // 型変数名から Type へのマップを構築
        let mut result = HashMap::new();
        for (&name, &var) in &self.type_vars {
            if let Some(ty) = solutions.get(&var) {
                result.insert(name, ty.clone());
            }
        }

        // クリーンアップ
        self.type_vars.clear();
        self.constraints.clear();
        self.next_type_var = 0;

        result
    }

    /// 制約を追加
    fn add_constraint(&mut self, constraint: TypeConstraint) {
        self.constraints.push(constraint);
    }

    /// 制約を解く
    fn solve_constraints(&self) -> HashMap<TypeVar, Type> {
        let mut solutions = HashMap::new();

        for constraint in &self.constraints {
            match constraint {
                TypeConstraint::FunctionArg { var, func_name, arg_index } => {
                    if solutions.contains_key(var) {
                        continue;
                    }

                    // RustDeclDict (bindings.rs) から関数シグネチャを取得
                    if let Some(ty) = self.lookup_rust_decl_param_type(*func_name, *arg_index) {
                        solutions.insert(*var, ty);
                    }
                }
                TypeConstraint::HasField { var, field } => {
                    if solutions.contains_key(var) {
                        continue;
                    }

                    // FieldsDict からフィールドを持つ構造体を特定
                    if let Some(fields_dict) = self.fields_dict {
                        if let Some(struct_name) = fields_dict.lookup_unique(*field) {
                            // struct_name は既に InternedStr なので直接使用
                            solutions.insert(
                                *var,
                                Type::Pointer(
                                    Box::new(Type::TypedefName(struct_name)),
                                    TypeQualifiers::default(),
                                ),
                            );
                        }
                    }
                }
            }
        }

        solutions
    }

    /// RustDeclDict から関数の引数型を取得
    fn lookup_rust_decl_param_type(&self, func_name: InternedStr, arg_index: usize) -> Option<Type> {
        let rust_decl_dict = self.rust_decl_dict?;
        let func_name_str = self.interner.get(func_name);
        let rust_fn = rust_decl_dict.fns.get(func_name_str)?;
        let param = rust_fn.params.get(arg_index)?;
        // Rust型文字列 (e.g., "*mut *mut SV") を Type に変換
        Some(self.parse_rust_type_string(&param.ty))
    }

    /// 式から引数に対する制約を収集
    fn collect_arg_constraint(&mut self, arg: &Expr, func_name: InternedStr, arg_index: usize) {
        // 引数が型変数として登録されている識別子かチェック
        if let ExprKind::Ident(name) = &arg.kind {
            if let Some(&var) = self.type_vars.get(name) {
                self.add_constraint(TypeConstraint::FunctionArg {
                    var,
                    func_name,
                    arg_index,
                });
            }
        }
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
    ///
    /// 注意: このメソッドは非推奨です。代わりに `collect_expr_constraints` を使用してください。
    /// `collect_expr_constraints` は全式の型を計算して type_env に追加します。
    #[deprecated(note = "use collect_expr_constraints instead")]
    pub fn infer_expr_type(&mut self, expr: &Expr) -> Type {
        match &expr.kind {
            // リテラル
            ExprKind::IntLit(_) => Type::Int,
            ExprKind::UIntLit(_) => Type::UnsignedInt,
            ExprKind::FloatLit(_) => Type::Double, // C のデフォルト
            ExprKind::CharLit(_) => Type::Int,     // char literalはint
            ExprKind::StringLit(_) => Type::Pointer(Box::new(Type::Char), TypeQualifiers::default()),

            // 識別子
            ExprKind::Ident(name) => {
                if let Some(sym) = self.lookup_symbol(*name) {
                    sym.ty.clone()
                } else {
                    Type::Unknown
                }
            }

            // 後置演算子
            ExprKind::PostInc(inner) | ExprKind::PostDec(inner) => self.infer_expr_type(inner),

            // 前置演算子
            ExprKind::PreInc(inner) | ExprKind::PreDec(inner) => self.infer_expr_type(inner),
            ExprKind::AddrOf(inner) => {
                let inner_ty = self.infer_expr_type(inner);
                Type::Pointer(Box::new(inner_ty), TypeQualifiers::default())
            }
            ExprKind::Deref(inner) => {
                let inner_ty = self.infer_expr_type(inner);
                if let Type::Pointer(elem, _) = inner_ty {
                    *elem
                } else {
                    Type::Unknown
                }
            }
            ExprKind::UnaryPlus(inner) | ExprKind::UnaryMinus(inner) => self.infer_expr_type(inner),
            ExprKind::BitNot(inner) => self.infer_expr_type(inner),
            ExprKind::LogNot(_) => Type::Int,
            ExprKind::Sizeof(_) | ExprKind::SizeofType(_) => Type::UnsignedLong,
            ExprKind::Alignof(_) => Type::UnsignedLong,

            // キャスト
            ExprKind::Cast { type_name, .. } => self.resolve_type_name(type_name),

            // 二項演算子
            ExprKind::Binary { op, lhs, rhs } => {
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
            ExprKind::Conditional { then_expr, else_expr, .. } => {
                let then_ty = self.infer_expr_type(then_expr);
                let else_ty = self.infer_expr_type(else_expr);
                self.usual_arithmetic_conversion(&then_ty, &else_ty)
            }

            // 関数呼び出し
            ExprKind::Call { func, args } => {
                // 制約収集モードの場合、引数の制約を収集
                if self.constraint_mode {
                    if let ExprKind::Ident(func_name) = &func.kind {
                        for (i, arg) in args.iter().enumerate() {
                            self.collect_arg_constraint(arg, *func_name, i);
                        }
                    }
                }

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
                if let ExprKind::Ident(func_name) = &func.kind {
                    if let Some(ret_ty) = self.lookup_apidoc_return_type(*func_name) {
                        return ret_ty;
                    }
                }
                Type::Unknown
            }

            // メンバーアクセス
            ExprKind::Member { expr: base, member } => {
                let base_ty = self.infer_expr_type(base);
                self.lookup_member_type(&base_ty, *member)
            }

            // ポインタメンバーアクセス
            ExprKind::PtrMember { expr: base, member } => {
                // 制約収集モードの場合、フィールドアクセス制約を収集
                if self.constraint_mode {
                    if let ExprKind::Ident(name) = &base.kind {
                        if let Some(&var) = self.type_vars.get(name) {
                            self.add_constraint(TypeConstraint::HasField {
                                var,
                                field: *member,
                            });
                        }
                    }
                }

                let base_ty = self.infer_expr_type(base);
                if let Type::Pointer(inner, _) = base_ty {
                    self.lookup_member_type(&inner, *member)
                } else {
                    Type::Unknown
                }
            }

            // 配列添字
            ExprKind::Index { expr: base, .. } => {
                let base_ty = self.infer_expr_type(base);
                match base_ty {
                    Type::Pointer(inner, _) => *inner,
                    Type::Array(inner, _) => *inner,
                    _ => Type::Unknown,
                }
            }

            // 代入演算子
            ExprKind::Assign { lhs, .. } => self.infer_expr_type(lhs),

            // コンマ演算子
            ExprKind::Comma { rhs, .. } => self.infer_expr_type(rhs),

            // 複合リテラル
            ExprKind::CompoundLit { type_name, .. } => self.resolve_type_name(type_name),

            // Statement Expression (GCC拡張)
            ExprKind::StmtExpr(compound) => {
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
    pub fn resolve_type_name(&self, type_name: &TypeName) -> Type {
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
                    loc: decl.loc().clone(),
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
                loc: func.loc().clone(),
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
                                    loc: func.loc().clone(),
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

    // ========================================
    // TypeEnv への型制約収集
    // ========================================

    /// マクロパラメータを設定
    pub fn set_macro_params(&mut self, params: &[InternedStr]) {
        self.macro_params.clear();
        for &param in params {
            self.macro_params.insert(param);
        }
    }

    /// マクロパラメータをクリア
    pub fn clear_macro_params(&mut self) {
        self.macro_params.clear();
    }

    /// マクロパラメータを apidoc 型情報付きでシンボルテーブルに登録
    ///
    /// # Arguments
    /// * `macro_name` - マクロ名
    /// * `params` - パラメータ名のリスト
    /// * `files` - ファイルレジストリ
    /// * `typedefs` - typedef 名セット
    pub fn register_macro_params_from_apidoc(
        &mut self,
        macro_name: InternedStr,
        params: &[InternedStr],
        files: &FileRegistry,
        typedefs: &HashSet<InternedStr>,
    ) {
        // macro_params に名前を登録（既存の動作を維持）
        self.macro_params.clear();
        for &param in params {
            self.macro_params.insert(param);
        }

        // apidoc からマクロ情報を取得
        if let Some(apidoc) = self.apidoc {
            let macro_name_str = self.interner.get(macro_name);
            if let Some(entry) = apidoc.get(macro_name_str) {
                // パラメータをシンボルとして登録
                for (i, &param_name) in params.iter().enumerate() {
                    if let Some(apidoc_arg) = entry.args.get(i) {
                        // parser で型文字列をパース
                        if let Ok(type_name) = parse_type_from_string(
                            &apidoc_arg.ty,
                            self.interner,
                            files,
                            typedefs,
                        ) {
                            let ty = self.resolve_type_name(&type_name);
                            self.define_symbol(Symbol {
                                name: param_name,
                                ty,
                                loc: SourceLocation::default(),
                                kind: SymbolKind::Variable,
                            });
                        }
                    }
                }
            }
        }
    }

    /// 識別子がマクロパラメータかどうか
    fn is_macro_param(&self, name: InternedStr) -> bool {
        self.macro_params.contains(&name)
    }

    /// type_env から式の型文字列を取得
    fn get_expr_type_str(&self, expr_id: ExprId, type_env: &TypeEnv) -> String {
        if let Some(constraints) = type_env.expr_constraints.get(&expr_id) {
            if let Some(c) = constraints.first() {
                return c.ty.clone();
            }
        }
        "<unknown>".to_string()
    }

    /// 二項演算の結果型を計算（文字列ベース）
    fn compute_binary_type_str(&self, op: &BinOp, lhs_id: ExprId, rhs_id: ExprId, type_env: &TypeEnv) -> String {
        match op {
            // 比較演算子・論理演算子は int を返す
            BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge |
            BinOp::Eq | BinOp::Ne | BinOp::LogAnd | BinOp::LogOr => "int".to_string(),
            // 算術演算子は通常の型昇格
            _ => {
                let lhs_ty = self.get_expr_type_str(lhs_id, type_env);
                let rhs_ty = self.get_expr_type_str(rhs_id, type_env);
                self.usual_arithmetic_conversion_str(&lhs_ty, &rhs_ty)
            }
        }
    }

    /// 通常の算術型変換（文字列ベース）
    fn usual_arithmetic_conversion_str(&self, lhs: &str, rhs: &str) -> String {
        // 簡易的な実装：ランク付けで大きい方を返す
        let rank = |ty: &str| -> u8 {
            match ty {
                "long double" => 10,
                "double" => 9,
                "float" => 8,
                "unsigned long long" => 7,
                "long long" => 6,
                "unsigned long" => 5,
                "long" => 4,
                "unsigned int" => 3,
                "int" => 2,
                "unsigned short" => 1,
                "short" => 1,
                _ => 0,
            }
        };

        if rank(lhs) >= rank(rhs) {
            lhs.to_string()
        } else {
            rhs.to_string()
        }
    }

    /// 条件演算の結果型を計算（文字列ベース）
    fn compute_conditional_type_str(&self, then_id: ExprId, else_id: ExprId, type_env: &TypeEnv) -> String {
        let then_ty = self.get_expr_type_str(then_id, type_env);
        let else_ty = self.get_expr_type_str(else_id, type_env);
        self.usual_arithmetic_conversion_str(&then_ty, &else_ty)
    }

    /// 式全体から型制約を収集し、全式の型を計算（再帰的に走査）
    ///
    /// 子式を先に処理し、親式の型を後で計算する。
    pub fn collect_expr_constraints(&mut self, expr: &Expr, type_env: &mut TypeEnv) {
        match &expr.kind {
            // リテラル
            ExprKind::IntLit(_) => {
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id, "int", ConstraintSource::Inferred, "integer literal"
                ));
            }
            ExprKind::UIntLit(_) => {
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id, "unsigned int", ConstraintSource::Inferred, "unsigned integer literal"
                ));
            }
            ExprKind::FloatLit(_) => {
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id, "double", ConstraintSource::Inferred, "float literal"
                ));
            }
            ExprKind::CharLit(_) => {
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id, "int", ConstraintSource::Inferred, "char literal"
                ));
            }
            ExprKind::StringLit(_) => {
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id, "char*", ConstraintSource::Inferred, "string literal"
                ));
            }

            // 識別子
            ExprKind::Ident(name) => {
                // シンボルテーブルから型を取得
                if let Some(sym) = self.lookup_symbol(*name) {
                    let ty_str = sym.ty.display(self.interner);
                    type_env.add_constraint(TypeEnvConstraint::new(
                        expr.id, &ty_str, ConstraintSource::Inferred, "symbol lookup"
                    ));
                }
                // パラメータ参照の場合、ExprId とパラメータを紐付け
                if self.is_macro_param(*name) {
                    type_env.link_expr_to_param(expr.id, *name, "parameter reference");
                }
            }

            // 関数呼び出し
            ExprKind::Call { func, args } => {
                // 子式を先に処理
                self.collect_expr_constraints(func, type_env);
                for arg in args {
                    self.collect_expr_constraints(arg, type_env);
                }
                // Call の型制約を追加（RustDeclDict / Apidoc から）
                self.collect_call_constraints(expr.id, func, args, type_env);
            }

            // 二項演算子
            ExprKind::Binary { op, lhs, rhs } => {
                // 子式を先に処理
                self.collect_expr_constraints(lhs, type_env);
                self.collect_expr_constraints(rhs, type_env);
                // 親式の型を計算
                let result_ty = self.compute_binary_type_str(op, lhs.id, rhs.id, type_env);
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id, &result_ty, ConstraintSource::Inferred, "binary expression"
                ));
            }

            // 条件演算子
            ExprKind::Conditional { cond, then_expr, else_expr } => {
                self.collect_expr_constraints(cond, type_env);
                self.collect_expr_constraints(then_expr, type_env);
                self.collect_expr_constraints(else_expr, type_env);
                let result_ty = self.compute_conditional_type_str(then_expr.id, else_expr.id, type_env);
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id, &result_ty, ConstraintSource::Inferred, "conditional expression"
                ));
            }

            // キャスト
            ExprKind::Cast { type_name, expr: inner } => {
                self.collect_expr_constraints(inner, type_env);
                let ty = self.resolve_type_name(type_name);
                let ty_str = ty.display(self.interner);
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id, &ty_str, ConstraintSource::Inferred, "cast expression"
                ));
            }

            // 配列添字
            ExprKind::Index { expr: base, index } => {
                self.collect_expr_constraints(base, type_env);
                self.collect_expr_constraints(index, type_env);
                // 配列/ポインタの要素型を推論
                let base_ty_str = self.get_expr_type_str(base.id, type_env);
                let elem_ty = if base_ty_str.ends_with('*') {
                    base_ty_str.trim_end_matches('*').trim().to_string()
                } else if base_ty_str.contains('[') {
                    base_ty_str.split('[').next().unwrap_or(&base_ty_str).trim().to_string()
                } else {
                    "<unknown>".to_string()
                };
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id, &elem_ty, ConstraintSource::Inferred, "array subscript"
                ));
            }

            // メンバーアクセス
            ExprKind::Member { expr: base, member } => {
                self.collect_expr_constraints(base, type_env);
                // TODO: 構造体メンバーの型を取得
                let _ = member; // unused warning 回避
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id, "<unknown>", ConstraintSource::Inferred, "member access"
                ));
            }

            // ポインタメンバーアクセス
            ExprKind::PtrMember { expr: base, member } => {
                self.collect_expr_constraints(base, type_env);
                // TODO: 構造体メンバーの型を取得
                let _ = member;
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id, "<unknown>", ConstraintSource::Inferred, "pointer member access"
                ));
            }

            // 代入演算子
            ExprKind::Assign { lhs, rhs, .. } => {
                self.collect_expr_constraints(lhs, type_env);
                self.collect_expr_constraints(rhs, type_env);
                // 代入式の型は左辺の型
                let lhs_ty = self.get_expr_type_str(lhs.id, type_env);
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id, &lhs_ty, ConstraintSource::Inferred, "assignment expression"
                ));
            }

            // コンマ演算子
            ExprKind::Comma { lhs, rhs } => {
                self.collect_expr_constraints(lhs, type_env);
                self.collect_expr_constraints(rhs, type_env);
                // コンマ式の型は右辺の型
                let rhs_ty = self.get_expr_type_str(rhs.id, type_env);
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id, &rhs_ty, ConstraintSource::Inferred, "comma expression"
                ));
            }

            // 前置/後置インクリメント/デクリメント
            ExprKind::PreInc(inner) | ExprKind::PreDec(inner) |
            ExprKind::PostInc(inner) | ExprKind::PostDec(inner) => {
                self.collect_expr_constraints(inner, type_env);
                let inner_ty = self.get_expr_type_str(inner.id, type_env);
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id, &inner_ty, ConstraintSource::Inferred, "increment/decrement"
                ));
            }

            // アドレス取得
            ExprKind::AddrOf(inner) => {
                self.collect_expr_constraints(inner, type_env);
                let inner_ty = self.get_expr_type_str(inner.id, type_env);
                let ptr_ty = format!("{}*", inner_ty);
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id, &ptr_ty, ConstraintSource::Inferred, "address-of"
                ));
            }

            // 間接参照
            ExprKind::Deref(inner) => {
                self.collect_expr_constraints(inner, type_env);
                let inner_ty = self.get_expr_type_str(inner.id, type_env);
                let deref_ty = if inner_ty.ends_with('*') {
                    inner_ty.trim_end_matches('*').trim().to_string()
                } else {
                    "<unknown>".to_string()
                };
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id, &deref_ty, ConstraintSource::Inferred, "dereference"
                ));
            }

            // 単項プラス/マイナス
            ExprKind::UnaryPlus(inner) | ExprKind::UnaryMinus(inner) => {
                self.collect_expr_constraints(inner, type_env);
                let inner_ty = self.get_expr_type_str(inner.id, type_env);
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id, &inner_ty, ConstraintSource::Inferred, "unary plus/minus"
                ));
            }

            // ビット反転
            ExprKind::BitNot(inner) => {
                self.collect_expr_constraints(inner, type_env);
                let inner_ty = self.get_expr_type_str(inner.id, type_env);
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id, &inner_ty, ConstraintSource::Inferred, "bitwise not"
                ));
            }

            // 論理否定
            ExprKind::LogNot(inner) => {
                self.collect_expr_constraints(inner, type_env);
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id, "int", ConstraintSource::Inferred, "logical not"
                ));
            }

            // sizeof（式）
            ExprKind::Sizeof(inner) => {
                self.collect_expr_constraints(inner, type_env);
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id, "unsigned long", ConstraintSource::Inferred, "sizeof expression"
                ));
            }

            // sizeof（型）
            ExprKind::SizeofType(_) => {
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id, "unsigned long", ConstraintSource::Inferred, "sizeof type"
                ));
            }

            // alignof
            ExprKind::Alignof(_) => {
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id, "unsigned long", ConstraintSource::Inferred, "alignof"
                ));
            }

            // 複合リテラル
            ExprKind::CompoundLit { type_name, .. } => {
                let ty = self.resolve_type_name(type_name);
                let ty_str = ty.display(self.interner);
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id, &ty_str, ConstraintSource::Inferred, "compound literal"
                ));
            }

            // Statement Expression (GCC拡張)
            ExprKind::StmtExpr(compound) => {
                self.collect_compound_constraints(compound, type_env);
                // 最後の式の型を取得
                if let Some(last_expr_id) = self.get_last_expr_id(compound) {
                    let last_ty = self.get_expr_type_str(last_expr_id, type_env);
                    type_env.add_constraint(TypeEnvConstraint::new(
                        expr.id, &last_ty, ConstraintSource::Inferred, "statement expression"
                    ));
                } else {
                    type_env.add_constraint(TypeEnvConstraint::new(
                        expr.id, "void", ConstraintSource::Inferred, "statement expression (empty)"
                    ));
                }
            }
        }
    }

    /// 複合文の最後の式の ExprId を取得
    fn get_last_expr_id(&self, compound: &CompoundStmt) -> Option<ExprId> {
        if let Some(BlockItem::Stmt(Stmt::Expr(Some(expr), _))) = compound.items.last() {
            Some(expr.id)
        } else {
            None
        }
    }

    /// 関数呼び出しから型制約を収集
    fn collect_call_constraints(
        &mut self,
        call_expr_id: ExprId,
        func: &Expr,
        args: &[Expr],
        type_env: &mut TypeEnv,
    ) {
        // 関数名を取得
        let func_name = match &func.kind {
            ExprKind::Ident(name) => *name,
            _ => return, // 間接呼び出しは未対応
        };

        let func_name_str = self.interner.get(func_name);

        // RustDeclDict から引数の型を取得
        if let Some(rust_decl_dict) = self.rust_decl_dict {
            if let Some(rust_fn) = rust_decl_dict.fns.get(func_name_str) {
                for (i, arg) in args.iter().enumerate() {
                    if let Some(param) = rust_fn.params.get(i) {
                        let constraint = TypeEnvConstraint::new(
                            arg.id,
                            &param.ty,
                            ConstraintSource::RustBindings,
                            format!("arg {} of {}()", i, func_name_str),
                        );
                        type_env.add_constraint(constraint);
                    }
                }

                // 戻り値型も制約として追加
                if let Some(ref ret_ty) = rust_fn.ret_ty {
                    let return_constraint = TypeEnvConstraint::new(
                        call_expr_id,
                        ret_ty,
                        ConstraintSource::RustBindings,
                        format!("return type of {}()", func_name_str),
                    );
                    type_env.add_constraint(return_constraint);
                }
            }
        }

        // Apidoc から型を取得
        if let Some(apidoc) = self.apidoc {
            if let Some(entry) = apidoc.get(func_name_str) {
                // 引数の型
                for (i, arg) in args.iter().enumerate() {
                    if let Some(apidoc_arg) = entry.args.get(i) {
                        let constraint = TypeEnvConstraint::new(
                            arg.id,
                            &apidoc_arg.ty,
                            ConstraintSource::Apidoc,
                            format!("arg {} ({}) of {}()", i, apidoc_arg.name, func_name_str),
                        );
                        type_env.add_constraint(constraint);
                    }
                }

                // 戻り値型
                if let Some(ref return_type) = entry.return_type {
                    let return_constraint = TypeEnvConstraint::new(
                        call_expr_id,
                        return_type,
                        ConstraintSource::Apidoc,
                        format!("return type of {}()", func_name_str),
                    );
                    type_env.add_constraint(return_constraint);
                }
            }
        }
    }

    /// 複合文から型制約を収集
    fn collect_compound_constraints(&mut self, compound: &CompoundStmt, type_env: &mut TypeEnv) {
        for item in &compound.items {
            match item {
                BlockItem::Stmt(Stmt::Expr(Some(expr), _)) => {
                    self.collect_expr_constraints(expr, type_env);
                }
                BlockItem::Stmt(Stmt::Return(Some(expr), _)) => {
                    self.collect_expr_constraints(expr, type_env);
                }
                BlockItem::Stmt(Stmt::Compound(inner)) => {
                    self.collect_compound_constraints(inner, type_env);
                }
                _ => {}
            }
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
        // synのto_token_stream().to_string()は "* mut" のようにスペースを入れるため正規化
        let normalized = type_str
            .replace("* mut", "*mut")
            .replace("* const", "*const");
        let trimmed = normalized.trim();

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
