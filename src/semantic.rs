//! 意味解析モジュール
//!
//! スコープ管理と型推論を行う。

use std::collections::{HashMap, HashSet};

use crate::apidoc::ApidocDict;
use crate::ast::*;
use crate::fields_dict::FieldsDict;
use crate::inline_fn::InlineFnDict;
use crate::intern::{InternedStr, StringInterner};
use crate::parser::parse_type_from_string;
use crate::rust_decl::RustDeclDict;
use crate::source::{FileRegistry, SourceLocation};
use crate::type_env::{TypeEnv, TypeConstraint as TypeEnvConstraint};
use crate::type_repr::{
    CTypeSource, CTypeSpecs, CDerivedType, InferredType,
    RustTypeRepr, RustTypeSource, TypeRepr,
};
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
    /// InlineFnDict への参照 (inline関数のAST情報)
    inline_fn_dict: Option<&'a InlineFnDict>,
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
    /// 確定済みマクロの戻り値型（マクロ名 -> 戻り値型）への参照
    macro_return_types: Option<&'a HashMap<String, String>>,
    /// 確定済みマクロのパラメータ型（マクロ名 -> [(パラメータ名, 型)])への参照
    /// ネストしたマクロ呼び出しからの型伝播に使用
    macro_param_types: Option<&'a HashMap<String, Vec<(String, String)>>>,
    /// ファイルレジストリ（型文字列パース用）
    files: Option<&'a FileRegistry>,
    /// typedef 名の集合（型文字列パース用）
    parser_typedefs: Option<&'a HashSet<InternedStr>>,
}

impl<'a> SemanticAnalyzer<'a> {
    /// 新しい意味解析器を作成
    pub fn new(
        interner: &'a StringInterner,
        apidoc: Option<&'a ApidocDict>,
        fields_dict: Option<&'a FieldsDict>,
    ) -> Self {
        Self::with_rust_decl_dict(interner, apidoc, fields_dict, None, None)
    }

    /// RustDeclDict と InlineFnDict を指定して意味解析器を作成
    pub fn with_rust_decl_dict(
        interner: &'a StringInterner,
        apidoc: Option<&'a ApidocDict>,
        fields_dict: Option<&'a FieldsDict>,
        rust_decl_dict: Option<&'a RustDeclDict>,
        inline_fn_dict: Option<&'a InlineFnDict>,
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
            inline_fn_dict,
            type_vars: HashMap::new(),
            next_type_var: 0,
            constraints: Vec::new(),
            constraint_mode: false,
            macro_params: HashSet::new(),
            macro_return_types: None,
            macro_param_types: None,
            files: None,
            parser_typedefs: None,
        }
    }

    /// 確定済みマクロの戻り値型キャッシュへの参照を設定
    pub fn set_macro_return_types(&mut self, cache: &'a HashMap<String, String>) {
        self.macro_return_types = Some(cache);
    }

    /// 確定済みマクロのパラメータ型キャッシュへの参照を設定
    pub fn set_macro_param_types(&mut self, cache: &'a HashMap<String, Vec<(String, String)>>) {
        self.macro_param_types = Some(cache);
    }

    /// マクロの戻り値型を取得
    pub fn get_macro_return_type(&self, macro_name: &str) -> Option<&str> {
        self.macro_return_types
            .and_then(|cache| cache.get(macro_name))
            .map(|s| s.as_str())
    }

    /// マクロのパラメータ型を取得
    pub fn get_macro_param_types(&self, macro_name: &str) -> Option<&Vec<(String, String)>> {
        self.macro_param_types
            .and_then(|cache| cache.get(macro_name))
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

    /// InlineFnDict から inline 関数の引数型を取得
    /// InlineFnDict から inline 関数のパラメータ型を TypeRepr として直接取得
    ///
    /// AST (DeclSpecs + Declarator) から TypeRepr を直接構築する。
    /// 文字列への変換・再パースを経由しないため、修飾子付きポインタ等も正確に処理できる。
    fn lookup_inline_fn_param_type_repr(
        &self,
        func_name: InternedStr,
        arg_index: usize,
    ) -> Option<TypeRepr> {
        let dict = self.inline_fn_dict?;
        let func_def = dict.get(func_name)?;

        let param_list = func_def.declarator.derived.iter()
            .find_map(|d| match d {
                DerivedDecl::Function(params) => Some(params),
                _ => None,
            })?;

        let param = param_list.params.get(arg_index)?;

        let specs = CTypeSpecs::from_decl_specs(&param.specs, self.interner);
        let derived = param.declarator.as_ref()
            .map(|d| {
                // Function 派生型より前の部分のみ（パラメータ自体の型）
                CDerivedType::from_derived_decls(&d.derived)
                    .into_iter()
                    .take_while(|d| !matches!(d, CDerivedType::Function { .. }))
                    .collect()
            })
            .unwrap_or_default();

        Some(TypeRepr::CType {
            specs,
            derived,
            source: CTypeSource::InlineFn { func_name },
        })
    }

    /// InlineFnDict から inline 関数の戻り値型を TypeRepr として直接取得
    fn lookup_inline_fn_return_type_repr(&self, func_name: InternedStr) -> Option<TypeRepr> {
        let dict = self.inline_fn_dict?;
        let func_def = dict.get(func_name)?;

        let specs = CTypeSpecs::from_decl_specs(&func_def.specs, self.interner);

        // Declarator の Function より前の derived 部分のみ（戻り値のポインタ等）
        let derived: Vec<_> = CDerivedType::from_derived_decls(&func_def.declarator.derived)
            .into_iter()
            .take_while(|d| !matches!(d, CDerivedType::Function { .. }))
            .collect();

        Some(TypeRepr::CType {
            specs,
            derived,
            source: CTypeSource::InlineFn { func_name },
        })
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
        files: &'a FileRegistry,
        typedefs: &'a HashSet<InternedStr>,
    ) {
        // files と typedefs を保存（後で型パース時に使用）
        self.files = Some(files);
        self.parser_typedefs = Some(typedefs);

        // macro_params に名前を登録（既存の動作を維持）
        self.macro_params.clear();
        for &param in params {
            self.macro_params.insert(param);
        }

        // apidoc からマクロ情報を取得
        let macro_name_str = self.interner.get(macro_name);
        if let Some(apidoc) = self.apidoc {
            if let Some(entry) = apidoc.get(macro_name_str) {
                // パラメータをシンボルとして登録
                for (i, &param_name) in params.iter().enumerate() {
                    if let Some(apidoc_arg) = entry.args.get(i) {
                        // parser で型文字列をパース
                        match parse_type_from_string(
                            &apidoc_arg.ty,
                            self.interner,
                            files,
                            typedefs,
                        ) {
                            Ok(type_name) => {
                                let ty = self.resolve_type_name(&type_name);
                                self.define_symbol(Symbol {
                                    name: param_name,
                                    ty,
                                    loc: SourceLocation::default(),
                                    kind: SymbolKind::Variable,
                                });
                            }
                            Err(_) => {}
                        }
                    }
                }
            }
        }
    }

    /// C 型文字列から TypeRepr を作成
    ///
    /// `files` と `parser_typedefs` が設定されている場合は完全な C パーサーを使用。
    /// 設定されていない場合は簡易パーサーにフォールバック。
    fn parse_type_string(&self, s: &str) -> TypeRepr {
        if let (Some(files), Some(typedefs)) = (self.files, self.parser_typedefs) {
            TypeRepr::from_c_type_string(s, self.interner, files, typedefs)
        } else {
            TypeRepr::from_apidoc_string(s, self.interner)
        }
    }

    /// 識別子がマクロパラメータかどうか
    fn is_macro_param(&self, name: InternedStr) -> bool {
        self.macro_params.contains(&name)
    }

    /// type_env から式の TypeRepr を直接取得
    fn get_expr_type_repr(&self, expr_id: ExprId, type_env: &TypeEnv) -> Option<TypeRepr> {
        type_env.expr_constraints.get(&expr_id)
            .and_then(|c| c.first())
            .map(|c| c.ty.clone())
    }

    /// type_env から式の型文字列を取得
    fn get_expr_type_str(&self, expr_id: ExprId, type_env: &TypeEnv) -> String {
        if let Some(constraints) = type_env.expr_constraints.get(&expr_id) {
            if let Some(c) = constraints.first() {
                return c.ty.to_display_string(self.interner);
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

    /// 文から式の型制約を収集（再帰的に走査）
    ///
    /// 文に含まれる式に対して `collect_expr_constraints` を呼び出す。
    pub fn collect_stmt_constraints(&mut self, stmt: &Stmt, type_env: &mut TypeEnv) {
        match stmt {
            Stmt::Compound(compound) => {
                for item in &compound.items {
                    match item {
                        BlockItem::Stmt(s) => self.collect_stmt_constraints(s, type_env),
                        BlockItem::Decl(_) => {} // 宣言は型制約収集の対象外
                    }
                }
            }
            Stmt::Expr(Some(expr), _) => {
                self.collect_expr_constraints(expr, type_env);
            }
            Stmt::If { cond, then_stmt, else_stmt, .. } => {
                self.collect_expr_constraints(cond, type_env);
                self.collect_stmt_constraints(then_stmt, type_env);
                if let Some(else_s) = else_stmt {
                    self.collect_stmt_constraints(else_s, type_env);
                }
            }
            Stmt::While { cond, body, .. } => {
                self.collect_expr_constraints(cond, type_env);
                self.collect_stmt_constraints(body, type_env);
            }
            Stmt::DoWhile { body, cond, .. } => {
                self.collect_stmt_constraints(body, type_env);
                self.collect_expr_constraints(cond, type_env);
            }
            Stmt::For { init, cond, step, body, .. } => {
                if let Some(ForInit::Expr(e)) = init {
                    self.collect_expr_constraints(e, type_env);
                }
                if let Some(c) = cond {
                    self.collect_expr_constraints(c, type_env);
                }
                if let Some(s) = step {
                    self.collect_expr_constraints(s, type_env);
                }
                self.collect_stmt_constraints(body, type_env);
            }
            Stmt::Return(Some(expr), _) => {
                self.collect_expr_constraints(expr, type_env);
            }
            Stmt::Switch { expr, body, .. } => {
                self.collect_expr_constraints(expr, type_env);
                self.collect_stmt_constraints(body, type_env);
            }
            Stmt::Case { expr, stmt, .. } => {
                self.collect_expr_constraints(expr, type_env);
                self.collect_stmt_constraints(stmt, type_env);
            }
            Stmt::Default { stmt, .. } | Stmt::Label { stmt, .. } => {
                self.collect_stmt_constraints(stmt, type_env);
            }
            _ => {} // Break, Continue, Goto, Asm, Expr(None), Return(None)
        }
    }

    /// 式全体から型制約を収集し、全式の型を計算（再帰的に走査）
    ///
    /// 子式を先に処理し、親式の型を後で計算する。
    pub fn collect_expr_constraints(&mut self, expr: &Expr, type_env: &mut TypeEnv) {
        match &expr.kind {
            // リテラル
            ExprKind::IntLit(_) => {
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id,
                    TypeRepr::Inferred(InferredType::IntLiteral),
                    "integer literal",
                ));
            }
            ExprKind::UIntLit(_) => {
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id,
                    TypeRepr::Inferred(InferredType::UIntLiteral),
                    "unsigned integer literal",
                ));
            }
            ExprKind::FloatLit(_) => {
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id,
                    TypeRepr::Inferred(InferredType::FloatLiteral),
                    "float literal",
                ));
            }
            ExprKind::CharLit(_) => {
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id,
                    TypeRepr::Inferred(InferredType::CharLiteral),
                    "char literal",
                ));
            }
            ExprKind::StringLit(_) => {
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id,
                    TypeRepr::Inferred(InferredType::StringLiteral),
                    "string literal",
                ));
            }

            // 識別子
            ExprKind::Ident(name) => {
                let name_str = self.interner.get(*name);

                // シンボルテーブルから型を取得
                if let Some(sym) = self.lookup_symbol(*name) {
                    let ty_str = sym.ty.display(self.interner);
                    // シンボル参照を示す TypeRepr を作成
                    // resolved_type は文字列からパースした C 型
                    let resolved = TypeRepr::from_apidoc_string(&ty_str, self.interner);
                    type_env.add_constraint(TypeEnvConstraint::new(
                        expr.id,
                        TypeRepr::Inferred(InferredType::SymbolLookup {
                            name: *name,
                            resolved_type: Box::new(resolved),
                        }),
                        "symbol lookup",
                    ));
                // RustDeclDict から定数の型を取得
                } else if let Some(rust_decl_dict) = self.rust_decl_dict {
                    if let Some(rust_const) = rust_decl_dict.lookup_const(name_str) {
                        type_env.add_constraint(TypeEnvConstraint::new(
                            expr.id,
                            TypeRepr::RustType {
                                repr: RustTypeRepr::from_type_string(&rust_const.ty),
                                source: RustTypeSource::Const {
                                    const_name: name_str.to_string(),
                                },
                            },
                            "bindings constant",
                        ));
                    } else if name_str == "my_perl" {
                        // THX 由来の my_perl はデフォルトで *mut PerlInterpreter
                        type_env.add_constraint(TypeEnvConstraint::new(
                            expr.id,
                            TypeRepr::Inferred(InferredType::ThxDefault),
                            "THX default type",
                        ));
                    }
                } else if name_str == "my_perl" {
                    // THX 由来の my_perl はデフォルトで *mut PerlInterpreter
                    type_env.add_constraint(TypeEnvConstraint::new(
                        expr.id,
                        TypeRepr::Inferred(InferredType::ThxDefault),
                        "THX default type",
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
                let result_ty_str = self.compute_binary_type_str(op, lhs.id, rhs.id, type_env);
                let result_type = TypeRepr::from_apidoc_string(&result_ty_str, self.interner);
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id,
                    TypeRepr::Inferred(InferredType::BinaryOp {
                        op: *op,
                        result_type: Box::new(result_type),
                    }),
                    "binary expression",
                ));
            }

            // 条件演算子
            ExprKind::Conditional { cond, then_expr, else_expr } => {
                self.collect_expr_constraints(cond, type_env);
                self.collect_expr_constraints(then_expr, type_env);
                self.collect_expr_constraints(else_expr, type_env);
                let then_ty_str = self.get_expr_type_str(then_expr.id, type_env);
                let else_ty_str = self.get_expr_type_str(else_expr.id, type_env);
                let result_ty_str = self.compute_conditional_type_str(then_expr.id, else_expr.id, type_env);
                let then_type = TypeRepr::from_apidoc_string(&then_ty_str, self.interner);
                let else_type = TypeRepr::from_apidoc_string(&else_ty_str, self.interner);
                let result_type = TypeRepr::from_apidoc_string(&result_ty_str, self.interner);
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id,
                    TypeRepr::Inferred(InferredType::Conditional {
                        then_type: Box::new(then_type),
                        else_type: Box::new(else_type),
                        result_type: Box::new(result_type),
                    }),
                    "conditional expression",
                ));
            }

            // キャスト
            ExprKind::Cast { type_name, expr: inner } => {
                self.collect_expr_constraints(inner, type_env);
                // AST → TypeRepr 直接変換（Type→String→TypeRepr roundtrip を排除）
                let specs = CTypeSpecs::from_decl_specs(&type_name.specs, self.interner);
                let derived: Vec<CDerivedType> = type_name.declarator.as_ref()
                    .map(|d| {
                        CDerivedType::from_derived_decls(&d.derived)
                            .into_iter()
                            .take_while(|d| !matches!(d, CDerivedType::Function { .. }))
                            .collect()
                    })
                    .unwrap_or_default();
                let target_type = TypeRepr::CType {
                    specs,
                    derived,
                    source: CTypeSource::Cast,
                };
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id,
                    TypeRepr::Inferred(InferredType::Cast {
                        target_type: Box::new(target_type),
                    }),
                    "cast expression",
                ));
            }

            // 配列添字
            ExprKind::Index { expr: base, index } => {
                self.collect_expr_constraints(base, type_env);
                self.collect_expr_constraints(index, type_env);
                // 配列/ポインタの要素型を推論
                let base_ty_str = self.get_expr_type_str(base.id, type_env);
                let elem_ty_str = if base_ty_str.ends_with('*') {
                    base_ty_str.trim_end_matches('*').trim().to_string()
                } else if base_ty_str.contains('[') {
                    base_ty_str.split('[').next().unwrap_or(&base_ty_str).trim().to_string()
                } else {
                    "<unknown>".to_string()
                };
                let base_type = TypeRepr::from_apidoc_string(&base_ty_str, self.interner);
                let element_type = TypeRepr::from_apidoc_string(&elem_ty_str, self.interner);
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id,
                    TypeRepr::Inferred(InferredType::ArraySubscript {
                        base_type: Box::new(base_type),
                        element_type: Box::new(element_type),
                    }),
                    "array subscript",
                ));
            }

            // メンバーアクセス
            ExprKind::Member { expr: base, member } => {
                self.collect_expr_constraints(base, type_env);

                let base_ty = self.get_expr_type_str(base.id, type_env);
                let member_name = self.interner.get(*member);

                // sv_u フィールドアクセスの特殊処理
                // base が ->sv_u パターンの場合、sv_u 辞書から型を解決
                // それ以外は FieldsDict から TypeRepr を直接取得
                let field_type = if self.is_sv_u_access(base) {
                    // sv_u フィールドは C 形式の型文字列で格納されている
                    self.lookup_sv_u_field_type(*member)
                        .map(|c_type| Box::new(TypeRepr::from_apidoc_string(&c_type, self.interner)))
                } else {
                    // TypeRepr ベースのフィールドルックアップ
                    let base_type_repr = self.get_expr_type_repr(base.id, type_env);
                    let struct_name = base_type_repr.as_ref().and_then(|t| t.type_name());
                    struct_name
                        .and_then(|n| self.fields_dict?.get_field_type(n, *member))
                        .map(|ft| Box::new(ft.type_repr.clone()))
                };

                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id,
                    TypeRepr::Inferred(InferredType::MemberAccess {
                        base_type: base_ty.clone(),
                        member: *member,
                        field_type,
                    }),
                    format!("{}.{}", base_ty, member_name),
                ));
            }

            // ポインタメンバーアクセス
            ExprKind::PtrMember { expr: base, member } => {
                self.collect_expr_constraints(base, type_env);

                // ベース型からメンバー型を推論
                let base_ty = self.get_expr_type_str(base.id, type_env);
                let member_name = self.interner.get(*member);

                // === ベース型の逆推論 ===
                // フィールド名から構造体を特定できる場合、ベース型を推論
                if let Some(fields_dict) = self.fields_dict {
                    // ベース型がまだ不明（unknown または Ident）の場合のみ逆推論を試みる
                    if base_ty == "/* unknown */" || self.is_ident_expr(base) {
                        // 1. まず一意なフィールドを試す (Phase 1)
                        // 2. 次に SV ファミリー共通フィールドを試す (Phase 2)
                        let inferred_struct = fields_dict.lookup_unique(*member)
                            .or_else(|| fields_dict.get_consistent_base_type(*member, self.interner));

                        if let Some(struct_name) = inferred_struct {
                            // typedef 名があれば使用（例: sv → SV）
                            let type_name = fields_dict.get_typedef_for_struct(struct_name)
                                .unwrap_or(struct_name);
                            let type_name_str = self.interner.get(type_name);
                            let base_type = TypeRepr::CType {
                                specs: CTypeSpecs::TypedefName(type_name),
                                derived: vec![CDerivedType::Pointer {
                                    is_const: false,
                                    is_volatile: false,
                                    is_restrict: false,
                                }],
                                source: CTypeSource::FieldInference { field_name: *member },
                            };
                            type_env.add_constraint(TypeEnvConstraint::new(
                                base.id,
                                base_type,
                                format!("field {} implies {}*", member_name, type_name_str),
                            ));
                        }
                    }
                }

                // TypeRepr ベースのフィールドルックアップ
                let base_type_repr = self.get_expr_type_repr(base.id, type_env);
                let pointee = base_type_repr.as_ref().and_then(|t| t.pointee_name());
                let (field_type, used_consistent_type) = if let Some(name) = pointee {
                    // ベース型が既知のポインタ型：構造体名で直接ルックアップ
                    let ty = self.fields_dict
                        .and_then(|fd| fd.get_field_type(name, *member))
                        .map(|ft| Box::new(ft.type_repr.clone()));
                    (ty, false)
                } else if let Some(fields_dict) = self.fields_dict {
                    // ベース型が不明な場合：一致型があればそれを使用（O(1)）
                    let ty = fields_dict.get_consistent_field_type(*member)
                        .cloned()
                        .map(Box::new);
                    (ty, true)
                } else {
                    (None, false)
                };

                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id,
                    TypeRepr::Inferred(InferredType::PtrMemberAccess {
                        base_type: base_ty.clone(),
                        member: *member,
                        field_type,
                        used_consistent_type,
                    }),
                    format!("{}->{}", base_ty, member_name),
                ));
            }

            // 代入演算子
            ExprKind::Assign { lhs, rhs, .. } => {
                self.collect_expr_constraints(lhs, type_env);
                self.collect_expr_constraints(rhs, type_env);
                // 代入式の型は左辺の型
                let lhs_ty_str = self.get_expr_type_str(lhs.id, type_env);
                let lhs_type = TypeRepr::from_apidoc_string(&lhs_ty_str, self.interner);
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id,
                    TypeRepr::Inferred(InferredType::Assignment {
                        lhs_type: Box::new(lhs_type),
                    }),
                    "assignment expression",
                ));
            }

            // コンマ演算子
            ExprKind::Comma { lhs, rhs } => {
                self.collect_expr_constraints(lhs, type_env);
                self.collect_expr_constraints(rhs, type_env);
                // コンマ式の型は右辺の型
                let rhs_ty_str = self.get_expr_type_str(rhs.id, type_env);
                let rhs_type = TypeRepr::from_apidoc_string(&rhs_ty_str, self.interner);
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id,
                    TypeRepr::Inferred(InferredType::Comma {
                        rhs_type: Box::new(rhs_type),
                    }),
                    "comma expression",
                ));
            }

            // 前置/後置インクリメント/デクリメント
            ExprKind::PreInc(inner) | ExprKind::PreDec(inner) |
            ExprKind::PostInc(inner) | ExprKind::PostDec(inner) => {
                self.collect_expr_constraints(inner, type_env);
                let inner_ty_str = self.get_expr_type_str(inner.id, type_env);
                let inner_type = TypeRepr::from_apidoc_string(&inner_ty_str, self.interner);
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id,
                    TypeRepr::Inferred(InferredType::IncDec {
                        inner_type: Box::new(inner_type),
                    }),
                    "increment/decrement",
                ));
            }

            // アドレス取得
            ExprKind::AddrOf(inner) => {
                self.collect_expr_constraints(inner, type_env);
                let inner_ty_str = self.get_expr_type_str(inner.id, type_env);
                let inner_type = TypeRepr::from_apidoc_string(&inner_ty_str, self.interner);
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id,
                    TypeRepr::Inferred(InferredType::AddressOf {
                        inner_type: Box::new(inner_type),
                    }),
                    "address-of",
                ));
            }

            // 間接参照
            ExprKind::Deref(inner) => {
                self.collect_expr_constraints(inner, type_env);
                let inner_ty_str = self.get_expr_type_str(inner.id, type_env);
                let pointer_type = TypeRepr::from_apidoc_string(&inner_ty_str, self.interner);
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id,
                    TypeRepr::Inferred(InferredType::Dereference {
                        pointer_type: Box::new(pointer_type),
                    }),
                    "dereference",
                ));
            }

            // 単項プラス/マイナス
            ExprKind::UnaryPlus(inner) | ExprKind::UnaryMinus(inner) => {
                self.collect_expr_constraints(inner, type_env);
                let inner_ty_str = self.get_expr_type_str(inner.id, type_env);
                let inner_type = TypeRepr::from_apidoc_string(&inner_ty_str, self.interner);
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id,
                    TypeRepr::Inferred(InferredType::UnaryArithmetic {
                        inner_type: Box::new(inner_type),
                    }),
                    "unary plus/minus",
                ));
            }

            // ビット反転
            ExprKind::BitNot(inner) => {
                self.collect_expr_constraints(inner, type_env);
                let inner_ty_str = self.get_expr_type_str(inner.id, type_env);
                let inner_type = TypeRepr::from_apidoc_string(&inner_ty_str, self.interner);
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id,
                    TypeRepr::Inferred(InferredType::UnaryArithmetic {
                        inner_type: Box::new(inner_type),
                    }),
                    "bitwise not",
                ));
            }

            // 論理否定
            ExprKind::LogNot(inner) => {
                self.collect_expr_constraints(inner, type_env);
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id,
                    TypeRepr::Inferred(InferredType::LogicalNot),
                    "logical not",
                ));
            }

            // sizeof（式）
            ExprKind::Sizeof(inner) => {
                self.collect_expr_constraints(inner, type_env);
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id,
                    TypeRepr::Inferred(InferredType::Sizeof),
                    "sizeof expression",
                ));
            }

            // sizeof（型）
            ExprKind::SizeofType(_) => {
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id,
                    TypeRepr::Inferred(InferredType::Sizeof),
                    "sizeof type",
                ));
            }

            // alignof
            ExprKind::Alignof(_) => {
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id,
                    TypeRepr::Inferred(InferredType::Alignof),
                    "alignof",
                ));
            }

            // 複合リテラル
            ExprKind::CompoundLit { type_name, .. } => {
                let ty = self.resolve_type_name(type_name);
                let ty_str = ty.display(self.interner);
                let type_name_repr = TypeRepr::from_apidoc_string(&ty_str, self.interner);
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id,
                    TypeRepr::Inferred(InferredType::CompoundLiteral {
                        type_name: Box::new(type_name_repr),
                    }),
                    "compound literal",
                ));
            }

            // Statement Expression (GCC拡張)
            ExprKind::StmtExpr(compound) => {
                self.collect_compound_constraints(compound, type_env);
                // 最後の式の型を取得
                if let Some(last_expr_id) = self.get_last_expr_id(compound) {
                    let last_ty_str = self.get_expr_type_str(last_expr_id, type_env);
                    let last_expr_type = TypeRepr::from_apidoc_string(&last_ty_str, self.interner);
                    type_env.add_constraint(TypeEnvConstraint::new(
                        expr.id,
                        TypeRepr::Inferred(InferredType::StmtExpr {
                            last_expr_type: Some(Box::new(last_expr_type)),
                        }),
                        "statement expression",
                    ));
                } else {
                    type_env.add_constraint(TypeEnvConstraint::new(
                        expr.id,
                        TypeRepr::Inferred(InferredType::StmtExpr {
                            last_expr_type: None,
                        }),
                        "statement expression (empty)",
                    ));
                }
            }

            // アサーション式
            ExprKind::Assert { condition, .. } => {
                self.collect_expr_constraints(condition, type_env);
                type_env.add_constraint(TypeEnvConstraint::new(
                    expr.id,
                    TypeRepr::Inferred(InferredType::Assert),
                    "assertion",
                ));
            }

            // マクロ呼び出し（展開結果の型を使用）
            ExprKind::MacroCall { name, args, expanded, .. } => {
                // 引数の型制約を収集
                for arg in args {
                    self.collect_expr_constraints(arg, type_env);
                }

                // 確定済みマクロのパラメータ型を参照（ネストしたマクロ呼び出しからの型伝播）
                let macro_name_str = self.interner.get(*name);
                if let Some(param_types) = self.get_macro_param_types(macro_name_str) {
                    for (i, arg) in args.iter().enumerate() {
                        if let Some((param_name, type_str)) = param_types.get(i) {
                            // キャッシュには Rust 形式の型文字列が保存されている
                            let constraint = TypeEnvConstraint::new(
                                arg.id,
                                TypeRepr::from_rust_string(type_str),
                                format!("arg {} ({}) of macro {}()", i, param_name, macro_name_str),
                            );
                            type_env.add_constraint(constraint);
                        }
                    }
                }

                // 展開結果の型制約を収集
                self.collect_expr_constraints(expanded, type_env);
                // MacroCall 式全体の型は expanded と同じ
                if let Some(constraints) = type_env.get_expr_constraints(expanded.id) {
                    if let Some(constraint) = constraints.first() {
                        type_env.add_constraint(TypeEnvConstraint::new(
                            expr.id,
                            constraint.ty.clone(),
                            "macro call (expanded)",
                        ));
                    }
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

    /// base が ->sv_u アクセスかどうかを判定
    ///
    /// `sv->sv_u.svu_pv` のような式で、`.svu_pv` の base が `sv->sv_u` かどうかを判定する。
    fn is_sv_u_access(&self, base: &Expr) -> bool {
        if let ExprKind::PtrMember { member, .. } = &base.kind {
            let sv_u_id = self.interner.lookup("sv_u");
            sv_u_id.map_or(false, |id| *member == id)
        } else {
            false
        }
    }

    /// 式が単純な識別子かどうかを判定
    ///
    /// マクロパラメータのように、まだ型が決まっていない識別子の場合に true を返す。
    fn is_ident_expr(&self, expr: &Expr) -> bool {
        matches!(expr.kind, ExprKind::Ident(_))
    }

    /// sv_u ユニオンフィールドの型を取得
    ///
    /// sv_u union のフィールド名から対応する C 型を返す。
    /// 例: svu_pv → "char*", svu_hash → "HE**"
    fn lookup_sv_u_field_type(&self, field: InternedStr) -> Option<String> {
        self.fields_dict?
            .get_sv_u_field_type(field)
            .map(|s| s.to_string())
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
                            TypeRepr::RustType {
                                repr: RustTypeRepr::from_type_string(&param.ty),
                                source: RustTypeSource::FnParam {
                                    func_name: func_name_str.to_string(),
                                    param_index: i,
                                },
                            },
                            format!("arg {} of {}()", i, func_name_str),
                        );
                        type_env.add_constraint(constraint);
                    }
                }

                // 戻り値型も制約として追加
                if let Some(ref ret_ty) = rust_fn.ret_ty {
                    let return_constraint = TypeEnvConstraint::new(
                        call_expr_id,
                        TypeRepr::RustType {
                            repr: RustTypeRepr::from_type_string(ret_ty),
                            source: RustTypeSource::FnReturn {
                                func_name: func_name_str.to_string(),
                            },
                        },
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
                            self.parse_type_string(&apidoc_arg.ty),
                            format!("arg {} ({}) of {}()", i, apidoc_arg.name, func_name_str),
                        );
                        type_env.add_constraint(constraint);
                    }
                }

                // 戻り値型
                if let Some(ref return_type) = entry.return_type {
                    let return_constraint = TypeEnvConstraint::new(
                        call_expr_id,
                        self.parse_type_string(return_type),
                        format!("return type of {}()", func_name_str),
                    );
                    type_env.add_constraint(return_constraint);
                }
            }
        }

        // InlineFnDict から型を取得（AST から TypeRepr を直接構築）
        if self.inline_fn_dict.is_some() {
            // 引数の型
            for (i, arg) in args.iter().enumerate() {
                if let Some(type_repr) = self.lookup_inline_fn_param_type_repr(func_name, i) {
                    let constraint = TypeEnvConstraint::new(
                        arg.id,
                        type_repr,
                        format!("arg {} of inline {}()", i, func_name_str),
                    );
                    type_env.add_constraint(constraint);
                }
            }

            // 戻り値型
            if let Some(type_repr) = self.lookup_inline_fn_return_type_repr(func_name) {
                let return_constraint = TypeEnvConstraint::new(
                    call_expr_id,
                    type_repr,
                    format!("return type of inline {}()", func_name_str),
                );
                type_env.add_constraint(return_constraint);
            }
        }

        // 確定済みマクロのパラメータ型を参照（ネストしたマクロ呼び出しからの型伝播）
        if let Some(param_types) = self.get_macro_param_types(func_name_str) {
            for (i, arg) in args.iter().enumerate() {
                if let Some((param_name, type_str)) = param_types.get(i) {
                    // キャッシュには Rust 形式の型文字列が保存されている
                    let constraint = TypeEnvConstraint::new(
                        arg.id,
                        TypeRepr::from_rust_string(type_str),
                        format!("arg {} ({}) of macro {}()", i, param_name, func_name_str),
                    );
                    type_env.add_constraint(constraint);
                }
            }
        }

        // 確定済みマクロの戻り値型を参照
        if let Some(return_type_str) = self.get_macro_return_type(func_name_str) {
            // キャッシュには Rust 形式の型文字列が保存されている
            let return_constraint = TypeEnvConstraint::new(
                call_expr_id,
                TypeRepr::from_rust_string(return_type_str),
                format!("return type of macro {}()", func_name_str),
            );
            type_env.add_constraint(return_constraint);
        }
    }

    /// 複合文から型制約を収集
    fn collect_compound_constraints(&mut self, compound: &CompoundStmt, type_env: &mut TypeEnv) {
        for item in &compound.items {
            match item {
                BlockItem::Decl(decl) => {
                    // 宣言の初期化子内の式を処理
                    self.collect_decl_initializer_constraints(decl, type_env);
                }
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

    /// 宣言の初期化子から型制約を収集
    fn collect_decl_initializer_constraints(&mut self, decl: &Declaration, type_env: &mut TypeEnv) {
        for init_decl in &decl.declarators {
            if let Some(ref init) = init_decl.init {
                self.collect_initializer_constraints(init, type_env);
            }
        }
    }

    /// 初期化子から型制約を収集（再帰）
    fn collect_initializer_constraints(&mut self, init: &Initializer, type_env: &mut TypeEnv) {
        match init {
            Initializer::Expr(expr) => {
                self.collect_expr_constraints(expr, type_env);
            }
            Initializer::List(items) => {
                for item in items {
                    self.collect_initializer_constraints(&item.init, type_env);
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
