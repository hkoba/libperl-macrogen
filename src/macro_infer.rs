//! マクロ型推論エンジン
//!
//! マクロ定義から型情報を推論するためのモジュール。
//! ExprId を活用し、複数ソースからの型制約を収集・管理する。

use std::collections::{HashMap, HashSet};

use crate::apidoc::ApidocDict;
use crate::ast::{AssertKind, BlockItem, DerivedDecl, Expr, ExprKind, Stmt, TypeName, TypeSpec};
use crate::fields_dict::FieldsDict;
use crate::inline_fn::InlineFnDict;
use crate::intern::{InternedStr, StringInterner};
use crate::macro_def::{MacroDef, MacroKind, MacroTable};
use crate::parser::{parse_expression_from_tokens_ref, parse_statement_from_tokens_ref};
use crate::rust_decl::RustDeclDict;
use crate::semantic::SemanticAnalyzer;
use crate::source::FileRegistry;
use crate::token::TokenKind;
use crate::token_expander::TokenExpander;
#[allow(deprecated)]
use crate::type_env::{ConstraintSource, TypeConstraint, TypeEnv};

// use std::io;
// use crate::SexpPrinter;

/// 展開を抑制するマクロシンボル
///
/// これらのマクロは展開せずに AST に関数呼び出しとして残す。
/// パターン検出（SvANY）や特殊処理（assert）に使用。
#[derive(Debug, Clone, Copy)]
pub struct NoExpandSymbols {
    /// assert マクロ
    pub assert: InternedStr,
    /// assert_ マクロ（Perl 独自）
    pub assert_: InternedStr,
    /// SvANY マクロ（SV ファミリー型推論用）
    pub sv_any: InternedStr,
}

impl NoExpandSymbols {
    /// 新しい NoExpandSymbols を作成
    pub fn new(interner: &mut StringInterner) -> Self {
        Self {
            assert: interner.intern("assert"),
            assert_: interner.intern("assert_"),
            sv_any: interner.intern("SvANY"),
        }
    }

    /// 全シンボルをイテレート
    pub fn iter(&self) -> impl Iterator<Item = InternedStr> {
        [self.assert, self.assert_, self.sv_any].into_iter()
    }
}

// ============================================================================
// SvANY パターン検出
// ============================================================================

/// SvANY パターンの検出結果
///
/// `((XPVAV*) SvANY(av))` のようなパターンから抽出した情報
#[derive(Debug, Clone)]
pub struct SvAnyPattern {
    /// キャスト先の型名（例: "XPVAV"）
    pub cast_type: String,
    /// SvANY の引数の識別子
    pub arg_ident: InternedStr,
}

/// 式から SvANY パターンを再帰的に検出
pub fn detect_sv_any_patterns(
    expr: &Expr,
    sv_any_id: InternedStr,
    interner: &StringInterner,
) -> Vec<SvAnyPattern> {
    let mut patterns = Vec::new();
    detect_sv_any_patterns_recursive(expr, sv_any_id, interner, &mut patterns);
    patterns
}

/// 式から SvANY パターンを再帰的に検出（内部関数）
fn detect_sv_any_patterns_recursive(
    expr: &Expr,
    sv_any_id: InternedStr,
    interner: &StringInterner,
    patterns: &mut Vec<SvAnyPattern>,
) {
    match &expr.kind {
        ExprKind::Cast { type_name, expr: inner } => {
            // キャスト先がポインタ型か確認
            if let Some(cast_type) = extract_pointer_base_type(type_name, interner) {
                // 内部が SvANY 呼び出しか確認
                if let Some(arg) = extract_sv_any_arg(inner, sv_any_id) {
                    patterns.push(SvAnyPattern {
                        cast_type,
                        arg_ident: arg,
                    });
                }
                // MUTABLE_PTR 経由のパターンも検出
                else if let Some(arg) = extract_sv_any_through_mutable_ptr(inner, sv_any_id, interner) {
                    patterns.push(SvAnyPattern {
                        cast_type,
                        arg_ident: arg,
                    });
                }
            }
            // 内部も再帰的に検索
            detect_sv_any_patterns_recursive(inner, sv_any_id, interner, patterns);
        }
        ExprKind::Binary { lhs, rhs, .. } => {
            detect_sv_any_patterns_recursive(lhs, sv_any_id, interner, patterns);
            detect_sv_any_patterns_recursive(rhs, sv_any_id, interner, patterns);
        }
        ExprKind::Call { func, args } => {
            detect_sv_any_patterns_recursive(func, sv_any_id, interner, patterns);
            for arg in args {
                detect_sv_any_patterns_recursive(arg, sv_any_id, interner, patterns);
            }
        }
        ExprKind::Index { expr: base, index } => {
            detect_sv_any_patterns_recursive(base, sv_any_id, interner, patterns);
            detect_sv_any_patterns_recursive(index, sv_any_id, interner, patterns);
        }
        ExprKind::Member { expr: base, .. } | ExprKind::PtrMember { expr: base, .. } => {
            detect_sv_any_patterns_recursive(base, sv_any_id, interner, patterns);
        }
        ExprKind::Conditional { cond, then_expr, else_expr } => {
            detect_sv_any_patterns_recursive(cond, sv_any_id, interner, patterns);
            detect_sv_any_patterns_recursive(then_expr, sv_any_id, interner, patterns);
            detect_sv_any_patterns_recursive(else_expr, sv_any_id, interner, patterns);
        }
        ExprKind::Assign { lhs, rhs, .. } | ExprKind::Comma { lhs, rhs } => {
            detect_sv_any_patterns_recursive(lhs, sv_any_id, interner, patterns);
            detect_sv_any_patterns_recursive(rhs, sv_any_id, interner, patterns);
        }
        ExprKind::PreInc(inner) | ExprKind::PreDec(inner)
        | ExprKind::PostInc(inner) | ExprKind::PostDec(inner)
        | ExprKind::AddrOf(inner) | ExprKind::Deref(inner)
        | ExprKind::UnaryPlus(inner) | ExprKind::UnaryMinus(inner)
        | ExprKind::BitNot(inner) | ExprKind::LogNot(inner)
        | ExprKind::Sizeof(inner) => {
            detect_sv_any_patterns_recursive(inner, sv_any_id, interner, patterns);
        }
        ExprKind::StmtExpr(compound) => {
            for item in &compound.items {
                collect_sv_any_patterns_from_block_item(item, sv_any_id, interner, patterns);
            }
        }
        ExprKind::Assert { condition, .. } => {
            detect_sv_any_patterns_recursive(condition, sv_any_id, interner, patterns);
        }
        ExprKind::CompoundLit { init, .. } => {
            for item in init {
                if let crate::ast::Initializer::Expr(e) = &item.init {
                    detect_sv_any_patterns_recursive(e, sv_any_id, interner, patterns);
                }
            }
        }
        // リテラル・識別子など再帰不要
        ExprKind::Ident(_) | ExprKind::IntLit(_) | ExprKind::UIntLit(_)
        | ExprKind::FloatLit(_) | ExprKind::CharLit(_) | ExprKind::StringLit(_)
        | ExprKind::SizeofType(_) | ExprKind::Alignof(_) => {}
    }
}

/// SvANY(arg) の arg を抽出（arg が識別子の場合のみ）
fn extract_sv_any_arg(expr: &Expr, sv_any_id: InternedStr) -> Option<InternedStr> {
    if let ExprKind::Call { func, args } = &expr.kind {
        if let ExprKind::Ident(id) = &func.kind {
            if *id == sv_any_id && args.len() == 1 {
                if let ExprKind::Ident(arg_id) = &args[0].kind {
                    return Some(*arg_id);
                }
            }
        }
    }
    None
}

/// MUTABLE_PTR(SvANY(arg)) パターンから arg を抽出
fn extract_sv_any_through_mutable_ptr(
    expr: &Expr,
    sv_any_id: InternedStr,
    interner: &StringInterner,
) -> Option<InternedStr> {
    if let ExprKind::Call { func, args } = &expr.kind {
        if let ExprKind::Ident(id) = &func.kind {
            let name = interner.get(*id);
            // MUTABLE_PTR, MUTABLE_HV, MUTABLE_AV, MUTABLE_CV, MUTABLE_SV など
            if name.starts_with("MUTABLE_") && args.len() == 1 {
                return extract_sv_any_arg(&args[0], sv_any_id);
            }
        }
    }
    None
}

/// ポインタ型からベース型名を抽出
///
/// `XPVAV*` から "XPVAV" を返す
fn extract_pointer_base_type(type_name: &TypeName, interner: &StringInterner) -> Option<String> {
    // 最初にポインタかどうか確認
    let is_pointer = type_name.declarator.as_ref()
        .map(|d| d.derived.iter().any(|dd| matches!(dd, DerivedDecl::Pointer(_))))
        .unwrap_or(false);

    if !is_pointer {
        return None;
    }

    // 型指定子から型名を取得
    for spec in &type_name.specs.type_specs {
        match spec {
            TypeSpec::Struct(struct_spec) => {
                if let Some(name) = struct_spec.name {
                    return Some(interner.get(name).to_string());
                }
            }
            TypeSpec::TypedefName(name) => {
                return Some(interner.get(*name).to_string());
            }
            _ => {}
        }
    }

    None
}

/// BlockItem から SvANY パターンを収集
fn collect_sv_any_patterns_from_block_item(
    item: &BlockItem,
    sv_any_id: InternedStr,
    interner: &StringInterner,
    patterns: &mut Vec<SvAnyPattern>,
) {
    match item {
        BlockItem::Stmt(stmt) => {
            collect_sv_any_patterns_from_stmt(stmt, sv_any_id, interner, patterns);
        }
        BlockItem::Decl(decl) => {
            // 宣言内の初期化式からも検出
            for init_decl in &decl.declarators {
                if let Some(crate::ast::Initializer::Expr(expr)) = &init_decl.init {
                    detect_sv_any_patterns_recursive(expr, sv_any_id, interner, patterns);
                }
            }
        }
    }
}

/// 文から SvANY パターンを収集
fn collect_sv_any_patterns_from_stmt(
    stmt: &Stmt,
    sv_any_id: InternedStr,
    interner: &StringInterner,
    patterns: &mut Vec<SvAnyPattern>,
) {
    match stmt {
        Stmt::Expr(Some(expr), _) | Stmt::Return(Some(expr), _) => {
            let found = detect_sv_any_patterns(expr, sv_any_id, interner);
            patterns.extend(found);
        }
        Stmt::If { cond, then_stmt, else_stmt, .. } => {
            let found = detect_sv_any_patterns(cond, sv_any_id, interner);
            patterns.extend(found);
            collect_sv_any_patterns_from_stmt(then_stmt, sv_any_id, interner, patterns);
            if let Some(else_s) = else_stmt {
                collect_sv_any_patterns_from_stmt(else_s, sv_any_id, interner, patterns);
            }
        }
        Stmt::While { cond, body, .. } => {
            let found = detect_sv_any_patterns(cond, sv_any_id, interner);
            patterns.extend(found);
            collect_sv_any_patterns_from_stmt(body, sv_any_id, interner, patterns);
        }
        Stmt::DoWhile { body, cond, .. } => {
            collect_sv_any_patterns_from_stmt(body, sv_any_id, interner, patterns);
            let found = detect_sv_any_patterns(cond, sv_any_id, interner);
            patterns.extend(found);
        }
        Stmt::For { init, cond, step, body, .. } => {
            if let Some(crate::ast::ForInit::Expr(e)) = init {
                let found = detect_sv_any_patterns(e, sv_any_id, interner);
                patterns.extend(found);
            }
            if let Some(c) = cond {
                let found = detect_sv_any_patterns(c, sv_any_id, interner);
                patterns.extend(found);
            }
            if let Some(s) = step {
                let found = detect_sv_any_patterns(s, sv_any_id, interner);
                patterns.extend(found);
            }
            collect_sv_any_patterns_from_stmt(body, sv_any_id, interner, patterns);
        }
        Stmt::Switch { expr, body, .. } => {
            let found = detect_sv_any_patterns(expr, sv_any_id, interner);
            patterns.extend(found);
            collect_sv_any_patterns_from_stmt(body, sv_any_id, interner, patterns);
        }
        Stmt::Compound(compound) => {
            for item in &compound.items {
                collect_sv_any_patterns_from_block_item(item, sv_any_id, interner, patterns);
            }
        }
        Stmt::Label { stmt: s, .. }
        | Stmt::Case { stmt: s, .. }
        | Stmt::Default { stmt: s, .. } => {
            collect_sv_any_patterns_from_stmt(s, sv_any_id, interner, patterns);
        }
        Stmt::Expr(None, _) | Stmt::Return(None, _)
        | Stmt::Goto(_, _) | Stmt::Continue(_) | Stmt::Break(_) | Stmt::Asm { .. } => {}
    }
}

// ============================================================================
// sv_u フィールドパターン検出
// ============================================================================

/// sv_u フィールドアクセスパターンの検出結果
///
/// `arg->sv_u.svu_XXX` のようなパターンから抽出した情報
#[derive(Debug, Clone)]
pub struct SvUFieldPattern {
    /// アクセスされた sv_u のフィールド名 (例: "svu_hash")
    pub sv_u_field: InternedStr,
    /// 引数の識別子名 (例: "hv")
    pub arg_ident: InternedStr,
    /// 推論される SV ファミリー型 (例: "HV")
    pub inferred_type: &'static str,
}

/// sv_u ユニオンフィールドから SV ファミリー引数型へのマッピング
///
/// このマッピングは意味的な対応関係であり、C 型とは独立。
/// `sv->sv_u.svu_XXX` パターンから `sv` の SV ファミリー型を推論するために使用。
fn sv_u_field_to_parameter_type(field: &str) -> Option<&'static str> {
    match field {
        "svu_pv" => Some("SV"),    // char* - PV系SV
        "svu_iv" => Some("SV"),    // IV - 整数SV
        "svu_uv" => Some("SV"),    // UV - 符号なし整数SV
        "svu_rv" => Some("SV"),    // SV* - リファレンス
        "svu_rx" => Some("SV"),    // REGEXP* - 正規表現SV
        "svu_array" => Some("AV"), // SV** - 配列
        "svu_hash" => Some("HV"),  // HE** - ハッシュ
        "svu_gp" => Some("GV"),    // GP* - グロブ
        "svu_fp" => Some("IO"),    // PerlIO* - IO
        _ => None,
    }
}

/// 式から sv_u フィールドアクセスパターンを検出
pub fn detect_sv_u_field_patterns(
    expr: &Expr,
    interner: &StringInterner,
) -> Vec<SvUFieldPattern> {
    let mut patterns = Vec::new();
    let sv_u_id = match interner.lookup("sv_u") {
        Some(id) => id,
        None => return patterns, // sv_u が interner に登録されていなければ空を返す
    };
    detect_sv_u_field_patterns_recursive(expr, sv_u_id, interner, &mut patterns);
    patterns
}

/// 式から sv_u フィールドアクセスパターンを再帰的に検出（内部関数）
fn detect_sv_u_field_patterns_recursive(
    expr: &Expr,
    sv_u_id: InternedStr,
    interner: &StringInterner,
    patterns: &mut Vec<SvUFieldPattern>,
) {
    match &expr.kind {
        // arg->sv_u.svu_XXX パターンを検出
        ExprKind::Member { expr: base, member: svu_field } => {
            // base が ptr-member で sv_u にアクセスしているか確認
            if let Some(pattern) = extract_sv_u_pattern(base, *svu_field, sv_u_id, interner) {
                patterns.push(pattern);
            }
            // base も再帰的に検索
            detect_sv_u_field_patterns_recursive(base, sv_u_id, interner, patterns);
        }
        ExprKind::PtrMember { expr: base, .. } => {
            detect_sv_u_field_patterns_recursive(base, sv_u_id, interner, patterns);
        }
        ExprKind::Binary { lhs, rhs, .. } => {
            detect_sv_u_field_patterns_recursive(lhs, sv_u_id, interner, patterns);
            detect_sv_u_field_patterns_recursive(rhs, sv_u_id, interner, patterns);
        }
        ExprKind::Call { func, args } => {
            detect_sv_u_field_patterns_recursive(func, sv_u_id, interner, patterns);
            for arg in args {
                detect_sv_u_field_patterns_recursive(arg, sv_u_id, interner, patterns);
            }
        }
        ExprKind::Cast { expr: inner, .. } => {
            detect_sv_u_field_patterns_recursive(inner, sv_u_id, interner, patterns);
        }
        ExprKind::Index { expr: base, index } => {
            detect_sv_u_field_patterns_recursive(base, sv_u_id, interner, patterns);
            detect_sv_u_field_patterns_recursive(index, sv_u_id, interner, patterns);
        }
        ExprKind::Conditional { cond, then_expr, else_expr } => {
            detect_sv_u_field_patterns_recursive(cond, sv_u_id, interner, patterns);
            detect_sv_u_field_patterns_recursive(then_expr, sv_u_id, interner, patterns);
            detect_sv_u_field_patterns_recursive(else_expr, sv_u_id, interner, patterns);
        }
        ExprKind::Assign { lhs, rhs, .. } | ExprKind::Comma { lhs, rhs } => {
            detect_sv_u_field_patterns_recursive(lhs, sv_u_id, interner, patterns);
            detect_sv_u_field_patterns_recursive(rhs, sv_u_id, interner, patterns);
        }
        ExprKind::PreInc(inner) | ExprKind::PreDec(inner)
        | ExprKind::PostInc(inner) | ExprKind::PostDec(inner)
        | ExprKind::AddrOf(inner) | ExprKind::Deref(inner)
        | ExprKind::UnaryPlus(inner) | ExprKind::UnaryMinus(inner)
        | ExprKind::BitNot(inner) | ExprKind::LogNot(inner)
        | ExprKind::Sizeof(inner) => {
            detect_sv_u_field_patterns_recursive(inner, sv_u_id, interner, patterns);
        }
        ExprKind::StmtExpr(compound) => {
            for item in &compound.items {
                collect_sv_u_patterns_from_block_item(item, sv_u_id, interner, patterns);
            }
        }
        ExprKind::Assert { condition, .. } => {
            detect_sv_u_field_patterns_recursive(condition, sv_u_id, interner, patterns);
        }
        ExprKind::CompoundLit { init, .. } => {
            for item in init {
                if let crate::ast::Initializer::Expr(e) = &item.init {
                    detect_sv_u_field_patterns_recursive(e, sv_u_id, interner, patterns);
                }
            }
        }
        // リテラル・識別子など再帰不要
        ExprKind::Ident(_) | ExprKind::IntLit(_) | ExprKind::UIntLit(_)
        | ExprKind::FloatLit(_) | ExprKind::CharLit(_) | ExprKind::StringLit(_)
        | ExprKind::SizeofType(_) | ExprKind::Alignof(_) => {}
    }
}

/// arg->sv_u.svu_XXX パターンを抽出
///
/// `base` が `(ptr-member (ident ARG) sv_u)` の形式で、
/// `svu_field` が有効な sv_u フィールド名の場合にパターンを返す
fn extract_sv_u_pattern(
    base: &Expr,
    svu_field: InternedStr,
    sv_u_id: InternedStr,
    interner: &StringInterner,
) -> Option<SvUFieldPattern> {
    // base が ptr-member で sv_u にアクセスしているか確認
    if let ExprKind::PtrMember { expr: ptr_base, member } = &base.kind {
        if *member == sv_u_id {
            // ptr_base が識別子か確認
            if let ExprKind::Ident(arg_ident) = &ptr_base.kind {
                // svu_field が有効なフィールド名か確認
                let field_str = interner.get(svu_field);
                if let Some(inferred_type) = sv_u_field_to_parameter_type(field_str) {
                    return Some(SvUFieldPattern {
                        sv_u_field: svu_field,
                        arg_ident: *arg_ident,
                        inferred_type,
                    });
                }
            }
        }
    }
    None
}

/// BlockItem から sv_u フィールドパターンを収集
fn collect_sv_u_patterns_from_block_item(
    item: &BlockItem,
    sv_u_id: InternedStr,
    interner: &StringInterner,
    patterns: &mut Vec<SvUFieldPattern>,
) {
    match item {
        BlockItem::Stmt(stmt) => {
            collect_sv_u_patterns_from_stmt(stmt, sv_u_id, interner, patterns);
        }
        BlockItem::Decl(decl) => {
            for init_decl in &decl.declarators {
                if let Some(crate::ast::Initializer::Expr(expr)) = &init_decl.init {
                    detect_sv_u_field_patterns_recursive(expr, sv_u_id, interner, patterns);
                }
            }
        }
    }
}

/// 文から sv_u フィールドパターンを収集
fn collect_sv_u_patterns_from_stmt(
    stmt: &Stmt,
    sv_u_id: InternedStr,
    interner: &StringInterner,
    patterns: &mut Vec<SvUFieldPattern>,
) {
    match stmt {
        Stmt::Expr(Some(expr), _) | Stmt::Return(Some(expr), _) => {
            detect_sv_u_field_patterns_recursive(expr, sv_u_id, interner, patterns);
        }
        Stmt::If { cond, then_stmt, else_stmt, .. } => {
            detect_sv_u_field_patterns_recursive(cond, sv_u_id, interner, patterns);
            collect_sv_u_patterns_from_stmt(then_stmt, sv_u_id, interner, patterns);
            if let Some(else_s) = else_stmt {
                collect_sv_u_patterns_from_stmt(else_s, sv_u_id, interner, patterns);
            }
        }
        Stmt::While { cond, body, .. } => {
            detect_sv_u_field_patterns_recursive(cond, sv_u_id, interner, patterns);
            collect_sv_u_patterns_from_stmt(body, sv_u_id, interner, patterns);
        }
        Stmt::DoWhile { body, cond, .. } => {
            collect_sv_u_patterns_from_stmt(body, sv_u_id, interner, patterns);
            detect_sv_u_field_patterns_recursive(cond, sv_u_id, interner, patterns);
        }
        Stmt::For { init, cond, step, body, .. } => {
            if let Some(crate::ast::ForInit::Expr(e)) = init {
                detect_sv_u_field_patterns_recursive(e, sv_u_id, interner, patterns);
            }
            if let Some(c) = cond {
                detect_sv_u_field_patterns_recursive(c, sv_u_id, interner, patterns);
            }
            if let Some(s) = step {
                detect_sv_u_field_patterns_recursive(s, sv_u_id, interner, patterns);
            }
            collect_sv_u_patterns_from_stmt(body, sv_u_id, interner, patterns);
        }
        Stmt::Switch { expr, body, .. } => {
            detect_sv_u_field_patterns_recursive(expr, sv_u_id, interner, patterns);
            collect_sv_u_patterns_from_stmt(body, sv_u_id, interner, patterns);
        }
        Stmt::Compound(compound) => {
            for item in &compound.items {
                collect_sv_u_patterns_from_block_item(item, sv_u_id, interner, patterns);
            }
        }
        Stmt::Label { stmt: s, .. }
        | Stmt::Case { stmt: s, .. }
        | Stmt::Default { stmt: s, .. } => {
            collect_sv_u_patterns_from_stmt(s, sv_u_id, interner, patterns);
        }
        Stmt::Expr(None, _) | Stmt::Return(None, _)
        | Stmt::Goto(_, _) | Stmt::Continue(_) | Stmt::Break(_) | Stmt::Asm { .. } => {}
    }
}

/// マクロのパース結果
#[derive(Debug, Clone)]
pub enum ParseResult {
    /// 式としてパース成功
    Expression(Box<Expr>),
    /// 文としてパース成功
    Statement(Vec<BlockItem>),
    /// パース不能（エラーメッセージ付き）
    Unparseable(Option<String>),
}

// ============================================================================
// MacroAst: マクロの AST 表現（パラメータ情報付き）
// ============================================================================

/// マクロパラメータの AST 表現
///
/// 各パラメータは `Expr` として表現され、固有の `ExprId` を持つ。
/// これにより、パラメータの型制約も `expr_constraints` に統一的に格納できる。
#[derive(Debug, Clone)]
pub struct MacroParam {
    /// パラメータ名
    pub name: InternedStr,
    /// パラメータを表す Expr（ExprKind::Ident を持つ）
    pub expr: Expr,
}

impl MacroParam {
    /// 新しい MacroParam を作成
    pub fn new(name: InternedStr, loc: crate::source::SourceLocation) -> Self {
        Self {
            name,
            expr: Expr::new(ExprKind::Ident(name), loc),
        }
    }

    /// パラメータの ExprId を取得
    pub fn expr_id(&self) -> crate::ast::ExprId {
        self.expr.id
    }
}

/// 推論状態
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InferStatus {
    /// 未処理
    Pending,
    /// 全ての型が確定
    TypeComplete,
    /// 一部の型が未確定
    TypeIncomplete,
    /// 型推論不能
    TypeUnknown,
}

impl Default for InferStatus {
    fn default() -> Self {
        Self::Pending
    }
}

/// マクロの型推論情報
#[derive(Debug, Clone)]
pub struct MacroInferInfo {
    /// マクロ名
    pub name: InternedStr,
    /// ターゲットマクロかどうか
    pub is_target: bool,
    /// マクロ本体にトークンがあるかどうか
    pub has_body: bool,
    /// 関数形式マクロかどうか
    pub is_function: bool,

    /// このマクロが使用する他のマクロ（def-use 関係）
    pub uses: HashSet<InternedStr>,
    /// このマクロを使用するマクロ（use-def 関係）
    pub used_by: HashSet<InternedStr>,

    /// THX 依存（aTHX, tTHX, my_perl を含む）
    pub is_thx_dependent: bool,

    /// トークン連結 (##) を含む（推移的）
    pub has_token_pasting: bool,

    /// パラメータリスト（各パラメータは ExprId を持つ）
    pub params: Vec<MacroParam>,

    /// パース結果
    pub parse_result: ParseResult,

    /// 型環境（収集された型制約）
    pub type_env: TypeEnv,

    /// 引数の型推論状態
    pub args_infer_status: InferStatus,

    /// 戻り値の型推論状態
    pub return_infer_status: InferStatus,
}

impl MacroInferInfo {
    /// 新しい MacroInferInfo を作成
    pub fn new(name: InternedStr) -> Self {
        Self {
            name,
            is_target: false,
            has_body: false,
            is_function: false,
            uses: HashSet::new(),
            used_by: HashSet::new(),
            is_thx_dependent: false,
            has_token_pasting: false,
            params: Vec::new(),
            parse_result: ParseResult::Unparseable(None),
            type_env: TypeEnv::new(),
            args_infer_status: InferStatus::Pending,
            return_infer_status: InferStatus::Pending,
        }
    }

    /// パラメータ名から対応する ExprId を検索
    pub fn find_param_expr_id(&self, name: InternedStr) -> Option<crate::ast::ExprId> {
        self.params.iter()
            .find(|p| p.name == name)
            .map(|p| p.expr_id())
    }

    /// 引数と戻り値の両方が確定しているか
    pub fn is_fully_confirmed(&self) -> bool {
        self.args_infer_status == InferStatus::TypeComplete
            && self.return_infer_status == InferStatus::TypeComplete
    }

    /// 使用するマクロを追加
    pub fn add_use(&mut self, used_macro: InternedStr) {
        self.uses.insert(used_macro);
    }

    /// 使用されるマクロを追加
    pub fn add_used_by(&mut self, user_macro: InternedStr) {
        self.used_by.insert(user_macro);
    }

    /// パース結果が式かどうか
    pub fn is_expression(&self) -> bool {
        matches!(self.parse_result, ParseResult::Expression(_))
    }

    /// パース結果が文かどうか
    pub fn is_statement(&self) -> bool {
        matches!(self.parse_result, ParseResult::Statement(_))
    }

    /// パース可能かどうか
    pub fn is_parseable(&self) -> bool {
        !matches!(self.parse_result, ParseResult::Unparseable(_))
    }

    /// マクロの戻り値型を取得
    ///
    /// 1. return_constraints があればそれを使用
    /// 2. 式マクロの場合、ルート式の型制約を使用
    pub fn get_return_type(&self) -> Option<&crate::type_repr::TypeRepr> {
        // まず return_constraints を確認
        if let Some(ty) = self.type_env.get_return_type() {
            return Some(ty);
        }

        // 式マクロの場合、ルート式の型を取得
        if let ParseResult::Expression(ref expr) = self.parse_result {
            if let Some(constraints) = self.type_env.get_expr_constraints(expr.id) {
                // 最初の制約の型を返す
                if let Some(constraint) = constraints.first() {
                    return Some(&constraint.ty);
                }
            }
        }

        None
    }
}

/// マクロ型推論コンテキスト
///
/// 全マクロの型推論を管理する。
pub struct MacroInferContext {
    /// マクロ名 → 推論情報
    pub macros: HashMap<InternedStr, MacroInferInfo>,

    /// 型確定済みマクロ
    pub confirmed: HashSet<InternedStr>,

    /// 型未確定マクロ
    pub unconfirmed: HashSet<InternedStr>,

    /// 型推論不能マクロ
    pub unknown: HashSet<InternedStr>,
}

impl MacroInferContext {
    /// 新しいコンテキストを作成
    pub fn new() -> Self {
        Self {
            macros: HashMap::new(),
            confirmed: HashSet::new(),
            unconfirmed: HashSet::new(),
            unknown: HashSet::new(),
        }
    }

    /// マクロ情報を登録
    pub fn register(&mut self, info: MacroInferInfo) {
        let name = info.name;
        self.macros.insert(name, info);
    }

    /// マクロ情報を取得
    pub fn get(&self, name: InternedStr) -> Option<&MacroInferInfo> {
        self.macros.get(&name)
    }

    /// マクロ情報を可変で取得
    pub fn get_mut(&mut self, name: InternedStr) -> Option<&mut MacroInferInfo> {
        self.macros.get_mut(&name)
    }

    /// def-use 関係を構築
    ///
    /// 各マクロの uses 情報から used_by を逆引きで構築する。
    pub fn build_use_relations(&mut self) {
        // まず uses 情報を収集
        let use_pairs: Vec<(InternedStr, InternedStr)> = self
            .macros
            .iter()
            .flat_map(|(user, info)| {
                info.uses
                    .iter()
                    .map(move |used| (*user, *used))
            })
            .collect();

        // used_by を設定
        for (user, used) in use_pairs {
            if let Some(used_info) = self.macros.get_mut(&used) {
                used_info.add_used_by(user);
            }
        }
    }

    /// 初期分類を行う
    ///
    /// 各マクロの状態に基づいて confirmed/unconfirmed/unknown に分類する。
    pub fn classify_initial(&mut self) {
        for (name, info) in &self.macros {
            if info.is_fully_confirmed() {
                self.confirmed.insert(*name);
            } else if info.args_infer_status == InferStatus::TypeUnknown
                || info.return_infer_status == InferStatus::TypeUnknown
            {
                self.unknown.insert(*name);
            } else {
                self.unconfirmed.insert(*name);
            }
        }
    }

    /// 推論候補を取得
    ///
    /// 未確定マクロのうち、使用するマクロが全て確定済みのものを返す。
    /// 使用マクロ数の少ない順にソート。
    pub fn get_inference_candidates(&self) -> Vec<InternedStr> {
        let mut candidates: Vec<_> = self
            .unconfirmed
            .iter()
            .filter(|name| {
                if let Some(info) = self.macros.get(name) {
                    // 使用するマクロが全て confirmed に含まれているか
                    info.uses.iter().all(|used| {
                        self.confirmed.contains(used) || !self.macros.contains_key(used)
                    })
                } else {
                    false
                }
            })
            .copied()
            .collect();

        // 使用マクロ数でソート
        candidates.sort_by_key(|name| {
            self.macros
                .get(name)
                .map(|info| info.uses.len())
                .unwrap_or(0)
        });

        candidates
    }

    /// マクロを確定済みに移動
    pub fn mark_confirmed(&mut self, name: InternedStr) {
        self.unconfirmed.remove(&name);
        self.confirmed.insert(name);
        if let Some(info) = self.macros.get_mut(&name) {
            info.args_infer_status = InferStatus::TypeComplete;
            info.return_infer_status = InferStatus::TypeComplete;
        }
    }

    /// マクロを未知に移動（引数側）
    pub fn mark_args_unknown(&mut self, name: InternedStr) {
        if let Some(info) = self.macros.get_mut(&name) {
            info.args_infer_status = InferStatus::TypeUnknown;
        }
    }

    /// マクロを未知に移動（戻り値側）
    pub fn mark_return_unknown(&mut self, name: InternedStr) {
        if let Some(info) = self.macros.get_mut(&name) {
            info.return_infer_status = InferStatus::TypeUnknown;
        }
    }

    /// マクロを unknown 集合に移動
    pub fn move_to_unknown(&mut self, name: InternedStr) {
        self.unconfirmed.remove(&name);
        self.unknown.insert(name);
    }

    /// 統計情報を取得
    pub fn stats(&self) -> MacroInferStats {
        let mut args_unknown = 0;
        let mut return_unknown = 0;
        for info in self.macros.values() {
            if info.args_infer_status == InferStatus::TypeUnknown {
                args_unknown += 1;
            }
            if info.return_infer_status == InferStatus::TypeUnknown {
                return_unknown += 1;
            }
        }
        MacroInferStats {
            total: self.macros.len(),
            confirmed: self.confirmed.len(),
            unconfirmed: self.unconfirmed.len(),
            args_unknown,
            return_unknown,
        }
    }

    /// Phase 1: MacroInferInfo の初期構築（パースまで、型推論なし）
    ///
    /// 返り値: (info, has_pasting_direct, has_thx_direct)
    /// - has_pasting_direct: マクロ本体に直接 ## が含まれるか
    /// - has_thx_direct: マクロ本体に直接 aTHX/tTHX/my_perl が含まれるか
    pub fn build_macro_info(
        &self,
        def: &MacroDef,
        macro_table: &MacroTable,
        interner: &StringInterner,
        files: &FileRegistry,
        rust_decl_dict: Option<&RustDeclDict>,
        typedefs: &HashSet<InternedStr>,
        thx_symbols: (InternedStr, InternedStr, InternedStr),
        no_expand: NoExpandSymbols,
    ) -> (MacroInferInfo, bool, bool) {
        let mut info = MacroInferInfo::new(def.name);
        info.is_target = def.is_target;
        info.has_body = !def.body.is_empty();
        info.is_function = matches!(def.kind, MacroKind::Function { .. });

        // パラメータの Expr を生成（各パラメータに ExprId を割り当て）
        if let MacroKind::Function { params, .. } = &def.kind {
            for &param_name in params {
                info.params.push(MacroParam::new(param_name, crate::source::SourceLocation::default()));
            }
        }

        // 直接 ## を含むかチェック
        let has_pasting_direct = def.body.iter().any(|t| matches!(t.kind, TokenKind::HashHash));

        // マクロ本体を展開（TokenExpander を使用）
        let mut expander = TokenExpander::new(macro_table, interner, files);
        if let Some(dict) = rust_decl_dict {
            expander.set_bindings_consts(&dict.consts);
        }
        // 特定マクロを展開しないよう登録（assert, SvANY など）
        for sym in no_expand.iter() {
            expander.add_no_expand(sym);
        }
        let expanded_tokens = expander.expand_with_calls(&def.body);

        // def-use 関係を収集（展開されたマクロの集合から）
        self.collect_uses_from_expanded(expander.expanded_macros(), &mut info);

        // THX 判定: 展開されたマクロに aTHX, tTHX が含まれるか、
        // または展開後トークンに my_perl が含まれるかをチェック
        let (sym_athx, sym_tthx, sym_my_perl) = thx_symbols;
        let has_thx_from_uses = info.uses.contains(&sym_athx) || info.uses.contains(&sym_tthx);
        let has_my_perl = expanded_tokens.iter().any(|t| {
            matches!(t.kind, TokenKind::Ident(id) if id == sym_my_perl)
        });
        let has_thx = has_thx_from_uses || has_my_perl;

        // 初期値を設定（後で propagate で上書きされる可能性あり）
        info.has_token_pasting = has_pasting_direct;
        info.is_thx_dependent = has_thx;

        // パースを試行
        info.parse_result = self.try_parse_tokens(&expanded_tokens, interner, files, typedefs);

        // パース成功した場合、assert 呼び出しを Assert 式に変換
        match &mut info.parse_result {
            ParseResult::Expression(expr) => {
                convert_assert_calls(expr, interner);
            }
            ParseResult::Statement(items) => {
                for item in items {
                    if let BlockItem::Stmt(stmt) = item {
                        convert_assert_calls_in_stmt(stmt, interner);
                    }
                }
            }
            ParseResult::Unparseable(_) => {}
        }

        (info, has_pasting_direct, has_thx)
    }

    /// Phase 2: 型推論の適用
    ///
    /// 既に登録済みの MacroInferInfo に対して型制約を収集する
    /// `return_types_cache` は確定済みマクロの戻り値型キャッシュ
    pub fn infer_macro_types<'a>(
        &mut self,
        name: InternedStr,
        params: &[InternedStr],
        interner: &'a StringInterner,
        files: &FileRegistry,
        apidoc: Option<&'a ApidocDict>,
        fields_dict: Option<&'a FieldsDict>,
        rust_decl_dict: Option<&'a RustDeclDict>,
        inline_fn_dict: Option<&'a InlineFnDict>,
        typedefs: &HashSet<InternedStr>,
        return_types_cache: &HashMap<String, String>,
    ) {
        let info = match self.macros.get_mut(&name) {
            Some(info) => info,
            None => return,
        };

        // パース成功した場合、型制約を収集
        if let ParseResult::Expression(ref expr) = info.parse_result {
            let mut analyzer = SemanticAnalyzer::with_rust_decl_dict(
                interner,
                apidoc,
                fields_dict,
                rust_decl_dict,
                inline_fn_dict,
            );

            // println!("expr macro {}: {:?}", interner.get(name), expr);
            // {
            //     print!("expr macro {}: ", interner.get(name));
            //     let stdout = io::stdout();
            //     let mut handler = stdout.lock();
            //     let mut printer = SexpPrinter::new(&mut handler, interner);
            //     let _ = printer.print_expr(expr);
            // }
            // println!("");

            // 確定済みマクロの戻り値型を設定（キャッシュへの参照を渡す）
            analyzer.set_macro_return_types(return_types_cache);

            // apidoc 型情報付きでパラメータをシンボルテーブルに登録
            analyzer.register_macro_params_from_apidoc(name, params, files, typedefs);

            // 全式の型制約を収集
            analyzer.collect_expr_constraints(expr, &mut info.type_env);

            // マクロ自体の戻り値型を制約として追加
            if let Some(apidoc_dict) = apidoc {
                let macro_name_str = interner.get(name);
                if let Some(entry) = apidoc_dict.get(macro_name_str) {
                    if let Some(ref return_type) = entry.return_type {
                        #[allow(deprecated)]
                        info.type_env.add_return_constraint(TypeConstraint::from_legacy(
                            expr.id,
                            return_type,
                            ConstraintSource::Apidoc,
                            format!("return type of macro {}", macro_name_str),
                        ));
                    }
                }
            }
        }

        // Statement の場合も型制約を収集
        if let ParseResult::Statement(ref block_items) = info.parse_result {
            let mut analyzer = SemanticAnalyzer::with_rust_decl_dict(
                interner,
                apidoc,
                fields_dict,
                rust_decl_dict,
                inline_fn_dict,
            );

            // 確定済みマクロの戻り値型を設定（キャッシュへの参照を渡す）
            analyzer.set_macro_return_types(return_types_cache);

            // apidoc 型情報付きでパラメータをシンボルテーブルに登録
            analyzer.register_macro_params_from_apidoc(name, params, files, typedefs);

            // 各 BlockItem について型制約を収集
            for item in block_items {
                if let BlockItem::Stmt(stmt) = item {
                    analyzer.collect_stmt_constraints(stmt, &mut info.type_env);
                }
            }
        }
    }

    /// マクロの戻り値型を取得（キャッシュ更新用）
    pub fn get_macro_return_type(&self, name: InternedStr, interner: &StringInterner) -> Option<(String, String)> {
        self.macros.get(&name).and_then(|info| {
            info.get_return_type().map(|ty| {
                (interner.get(name).to_string(), ty.to_display_string(interner))
            })
        })
    }

    /// SvANY パターンから型制約を追加
    ///
    /// `((XPVAV*) SvANY(av))` のようなパターンから、引数 `av` の型が `AV*` であることを推論する。
    ///
    /// # Arguments
    /// * `name` - マクロ名
    /// * `fields_dict` - SV ファミリーの構造体情報
    /// * `no_expand` - 展開抑制シンボル（SvANY の ID を含む）
    /// * `interner` - 文字列インターナー
    ///
    /// # Returns
    /// 検出されたパターン数
    pub fn apply_sv_any_constraints(
        &mut self,
        name: InternedStr,
        fields_dict: &FieldsDict,
        no_expand: NoExpandSymbols,
        interner: &StringInterner,
    ) -> usize {
        let sv_any_id = no_expand.sv_any;

        let info = match self.macros.get(&name) {
            Some(info) => info,
            None => return 0,
        };

        // SvANY を使用していなければスキップ
        if !info.uses.contains(&sv_any_id) {
            return 0;
        }

        // パターン検出（式マクロと文マクロの両方に対応）
        let patterns: Vec<SvAnyPattern> = match &info.parse_result {
            ParseResult::Expression(expr) => {
                detect_sv_any_patterns(expr, sv_any_id, interner)
            }
            ParseResult::Statement(block_items) => {
                let mut patterns = Vec::new();
                for item in block_items {
                    collect_sv_any_patterns_from_block_item(item, sv_any_id, interner, &mut patterns);
                }
                patterns
            }
            ParseResult::Unparseable(_) => return 0,
        };

        // パラメータに対する制約を収集
        // パラメータの ExprId を使って expr_constraints に追加
        let mut constraints_to_add = Vec::new();
        for pattern in &patterns {
            // パラメータの ExprId を検索
            let param_expr_id = match info.find_param_expr_id(pattern.arg_ident) {
                Some(id) => id,
                None => continue,  // パラメータでなければスキップ
            };

            // typeName から構造体名を取得
            let struct_name = match fields_dict.get_struct_for_sv_head_type(&pattern.cast_type) {
                Some(name) => name,
                None => continue,
            };

            // 型制約を準備
            // 例: av の型は AV* (= struct av*)
            let struct_name_str = interner.get(struct_name);
            let type_str = format!("{}*", struct_name_str.to_uppercase());
            let context = format!(
                "SvANY pattern: ({}*) SvANY({})",
                pattern.cast_type,
                interner.get(pattern.arg_ident)
            );

            // TypeRepr を直接構築（struct name* として表現）
            let type_repr = crate::type_repr::TypeRepr::CType {
                specs: crate::type_repr::CTypeSpecs::Struct {
                    name: Some(struct_name),
                    is_union: false,
                },
                derived: vec![crate::type_repr::CDerivedType::Pointer {
                    is_const: false,
                    is_volatile: false,
                    is_restrict: false,
                }],
                source: crate::type_repr::CTypeSource::Apidoc {
                    raw: type_str,  // 表示用に "AV*" を保持
                },
            };

            constraints_to_add.push((param_expr_id, type_repr, context));
        }

        let constraint_count = constraints_to_add.len();

        // 制約を追加（expr_constraints を使用）
        if let Some(info) = self.macros.get_mut(&name) {
            for (expr_id, type_repr, context) in constraints_to_add {
                info.type_env.add_expr_constraint(
                    crate::type_env::TypeConstraint::new(expr_id, type_repr, context),
                );
            }
        }

        constraint_count
    }

    /// sv_u フィールドアクセスパターンから型制約を適用
    ///
    /// マクロ本体で `arg->sv_u.svu_XXX` パターンを検出し、
    /// `arg` パラメータに対応する SV ファミリー型の制約を追加する。
    ///
    /// 例:
    /// - `hv->sv_u.svu_hash` → `hv: HV*`
    /// - `io->sv_u.svu_fp` → `io: IO*`
    /// - `av->sv_u.svu_array` → `av: AV*`
    pub fn apply_sv_u_field_constraints(
        &mut self,
        name: InternedStr,
        interner: &StringInterner,
    ) -> usize {
        let info = match self.macros.get(&name) {
            Some(info) => info,
            None => return 0,
        };

        // パターン検出（式マクロと文マクロの両方に対応）
        let patterns: Vec<SvUFieldPattern> = match &info.parse_result {
            ParseResult::Expression(expr) => {
                detect_sv_u_field_patterns(expr, interner)
            }
            ParseResult::Statement(block_items) => {
                let sv_u_id = match interner.lookup("sv_u") {
                    Some(id) => id,
                    None => return 0,
                };
                let mut patterns = Vec::new();
                for item in block_items {
                    collect_sv_u_patterns_from_block_item(item, sv_u_id, interner, &mut patterns);
                }
                patterns
            }
            ParseResult::Unparseable(_) => return 0,
        };

        if patterns.is_empty() {
            return 0;
        }

        // パラメータに対する制約を収集
        let mut constraints_to_add = Vec::new();
        for pattern in &patterns {
            // パラメータの ExprId を検索
            let param_expr_id = match info.find_param_expr_id(pattern.arg_ident) {
                Some(id) => id,
                None => continue,  // パラメータでなければスキップ
            };

            // 型表現を構築
            // 例: "HV*" → struct hv* として表現
            let struct_name = pattern.inferred_type.to_lowercase();
            let struct_name_interned = match interner.lookup(&struct_name) {
                Some(id) => id,
                None => continue,  // 構造体名が interner になければスキップ
            };

            let type_str = format!("{}*", pattern.inferred_type);
            let context = format!(
                "sv_u field pattern: {}->sv_u.{}",
                interner.get(pattern.arg_ident),
                interner.get(pattern.sv_u_field),
            );

            // TypeRepr を直接構築（struct name* として表現）
            let type_repr = crate::type_repr::TypeRepr::CType {
                specs: crate::type_repr::CTypeSpecs::Struct {
                    name: Some(struct_name_interned),
                    is_union: false,
                },
                derived: vec![crate::type_repr::CDerivedType::Pointer {
                    is_const: false,
                    is_volatile: false,
                    is_restrict: false,
                }],
                source: crate::type_repr::CTypeSource::Apidoc {
                    raw: type_str,  // 表示用に "HV*" を保持
                },
            };

            constraints_to_add.push((param_expr_id, type_repr, context));
        }

        let constraint_count = constraints_to_add.len();

        // 制約を追加（expr_constraints を使用）
        if let Some(info) = self.macros.get_mut(&name) {
            for (expr_id, type_repr, context) in constraints_to_add {
                info.type_env.add_expr_constraint(
                    crate::type_env::TypeConstraint::new(expr_id, type_repr, context),
                );
            }
        }

        constraint_count
    }

    /// マクロを解析して MacroInferInfo を作成（従来のAPI - 互換性のため保持）
    ///
    /// 1. マクロ本体をパース（式 or 文）
    /// 2. def-use 関係を収集（使用するマクロ/関数）
    /// 3. 初期型制約を収集
    #[allow(dead_code)]
    pub fn analyze_macro<'a>(
        &mut self,
        def: &MacroDef,
        macro_table: &MacroTable,
        thx_macros: &HashSet<InternedStr>,
        pasting_macros: &HashSet<InternedStr>,
        interner: &'a StringInterner,
        files: &FileRegistry,
        apidoc: Option<&'a ApidocDict>,
        fields_dict: Option<&'a FieldsDict>,
        rust_decl_dict: Option<&'a RustDeclDict>,
        typedefs: &HashSet<InternedStr>,
    ) {
        let mut info = MacroInferInfo::new(def.name);
        info.is_target = def.is_target;
        info.has_body = !def.body.is_empty();
        info.is_function = matches!(def.kind, MacroKind::Function { .. });
        info.is_thx_dependent = thx_macros.contains(&def.name);
        info.has_token_pasting = pasting_macros.contains(&def.name);

        // 関数形式マクロの場合、パラメータを取得
        let params: Vec<InternedStr> = match &def.kind {
            MacroKind::Function { params, .. } => params.clone(),
            MacroKind::Object => vec![],
        };

        // マクロ本体を展開（TokenExpander を使用）
        // expand_with_calls() を使用して関数形式マクロも展開
        // （DEBUG_l 等の関数マクロが複合文を引数に取る場合に必要）
        let mut expander = TokenExpander::new(macro_table, interner, files);
        if let Some(dict) = rust_decl_dict {
            expander.set_bindings_consts(&dict.consts);
        }
        let expanded_tokens = expander.expand_with_calls(&def.body);

        // def-use 関係を収集（展開されたマクロの集合から）
        self.collect_uses_from_expanded(expander.expanded_macros(), &mut info);

        // パースを試行
        info.parse_result = self.try_parse_tokens(&expanded_tokens, interner, files, typedefs);

        // パース成功した場合、型制約を収集
        if let ParseResult::Expression(ref expr) = info.parse_result {
            let mut analyzer = SemanticAnalyzer::with_rust_decl_dict(
                interner,
                apidoc,
                fields_dict,
                rust_decl_dict,
                None, // inline_fn_dict (deprecated function doesn't use it)
            );

            // apidoc 型情報付きでパラメータをシンボルテーブルに登録
            analyzer.register_macro_params_from_apidoc(def.name, &params, files, typedefs);

            // 全式の型制約を収集（collect_expr_constraints が全式の型を計算）
            analyzer.collect_expr_constraints(expr, &mut info.type_env);

            // マクロ自体の戻り値型を制約として追加
            if let Some(apidoc_dict) = apidoc {
                let macro_name_str = interner.get(def.name);
                if let Some(entry) = apidoc_dict.get(macro_name_str) {
                    if let Some(ref return_type) = entry.return_type {
                        #[allow(deprecated)]
                        info.type_env.add_return_constraint(TypeConstraint::from_legacy(
                            expr.id,
                            return_type,
                            ConstraintSource::Apidoc,
                            format!("return type of macro {}", macro_name_str),
                        ));
                    }
                }
            }
        }

        // Statement の場合も型制約を収集
        if let ParseResult::Statement(ref block_items) = info.parse_result {
            let mut analyzer = SemanticAnalyzer::with_rust_decl_dict(
                interner,
                apidoc,
                fields_dict,
                rust_decl_dict,
                None, // inline_fn_dict (deprecated function doesn't use it)
            );

            // apidoc 型情報付きでパラメータをシンボルテーブルに登録
            analyzer.register_macro_params_from_apidoc(def.name, &params, files, typedefs);

            // 各 BlockItem について型制約を収集
            for item in block_items {
                if let BlockItem::Stmt(stmt) = item {
                    analyzer.collect_stmt_constraints(stmt, &mut info.type_env);
                }
            }
        }

        self.register(info);
    }

    /// トークン列から使用するマクロ/関数を収集
    /// 展開されたマクロを uses に追加
    ///
    /// TokenExpander が展開したマクロの集合から、自分自身を除いて uses に追加する。
    fn collect_uses_from_expanded(
        &self,
        expanded_macros: &HashSet<InternedStr>,
        info: &mut MacroInferInfo,
    ) {
        for &id in expanded_macros {
            if id != info.name {
                info.add_use(id);
            }
        }
    }

    /// トークン列を式または文としてパース試行
    fn try_parse_tokens(
        &self,
        tokens: &[crate::token::Token],
        interner: &StringInterner,
        files: &FileRegistry,
        typedefs: &HashSet<InternedStr>,
    ) -> ParseResult {
        if tokens.is_empty() {
            return ParseResult::Unparseable(Some("empty token sequence".to_string()));
        }

        // 空白・改行をスキップして最初の有効なトークンを探す
        let first_significant = tokens.iter().find(|t| {
            !matches!(t.kind, TokenKind::Space | TokenKind::Newline)
        });

        // 先頭トークンが KwDo または KwIf なら文としてパース試行
        let is_statement_start = first_significant
            .is_some_and(|t| matches!(t.kind, TokenKind::KwDo | TokenKind::KwIf));
        if is_statement_start {
            match parse_statement_from_tokens_ref(tokens.to_vec(), interner, files, typedefs) {
                Ok(stmt) => return ParseResult::Statement(vec![BlockItem::Stmt(stmt)]),
                Err(_) => {} // フォールスルーして式としてパース
            }
        }

        // 式としてパースを試行
        match parse_expression_from_tokens_ref(tokens.to_vec(), interner, files, typedefs) {
            Ok(expr) => ParseResult::Expression(Box::new(expr)),
            Err(err) => ParseResult::Unparseable(Some(err.format_with_files(files))),
        }
    }

    /// 全マクロから THX 依存関係を収集（定義順序に依存しない）
    ///
    /// 2パスで推移的閉包を計算:
    /// 1. 直接 aTHX, tTHX, my_perl を含むマクロを収集
    /// 2. THX マクロを使用するマクロも THX 依存として追加（収束まで繰り返し）
    ///
    /// Note: 現在は propagate_flag_via_used_by で代替
    #[allow(dead_code)]
    fn collect_thx_dependencies(
        &self,
        macro_table: &MacroTable,
        thx_symbols: (InternedStr, InternedStr, InternedStr),
    ) -> HashSet<InternedStr> {
        let (sym_athx, sym_tthx, sym_my_perl) = thx_symbols;

        // Phase 1: 直接 THX トークンを含むマクロを収集
        let mut thx_macros = HashSet::new();
        for (name, def) in macro_table.iter() {
            for token in &def.body {
                if let TokenKind::Ident(id) = token.kind {
                    if id == sym_athx || id == sym_tthx || id == sym_my_perl {
                        thx_macros.insert(*name);
                        break;
                    }
                }
            }
        }

        // Phase 2: 推移的閉包を計算（THX マクロを使用するマクロも THX 依存）
        loop {
            let mut added = false;
            for (name, def) in macro_table.iter() {
                if thx_macros.contains(name) {
                    continue;
                }
                for token in &def.body {
                    if let TokenKind::Ident(id) = token.kind {
                        if thx_macros.contains(&id) {
                            thx_macros.insert(*name);
                            added = true;
                            break;
                        }
                    }
                }
            }
            if !added {
                break;
            }
        }

        thx_macros
    }

    /// トークン連結 (##) 依存を収集（推移的閉包）
    ///
    /// Note: 現在は propagate_flag_via_used_by で代替
    #[allow(dead_code)]
    fn collect_pasting_dependencies(
        &self,
        macro_table: &MacroTable,
    ) -> HashSet<InternedStr> {
        // Phase 1: 直接 ## を含むマクロを収集
        let mut pasting_macros = HashSet::new();
        for (name, def) in macro_table.iter() {
            for token in &def.body {
                if matches!(token.kind, TokenKind::HashHash) {
                    pasting_macros.insert(*name);
                    break;
                }
            }
        }

        // Phase 2: 推移的閉包を計算（## マクロを使用するマクロも ## 依存）
        loop {
            let mut added = false;
            for (name, def) in macro_table.iter() {
                if pasting_macros.contains(name) {
                    continue;
                }
                for token in &def.body {
                    if let TokenKind::Ident(id) = token.kind {
                        if pasting_macros.contains(&id) {
                            pasting_macros.insert(*name);
                            added = true;
                            break;
                        }
                    }
                }
            }
            if !added {
                break;
            }
        }

        pasting_macros
    }

    /// 全ターゲットマクロを解析
    ///
    /// MacroTable 内の全ターゲットマクロに対して analyze_macro を実行し、
    /// def-use 関係を構築して初期分類を行う。
    pub fn analyze_all_macros<'a>(
        &mut self,
        macro_table: &MacroTable,
        interner: &'a StringInterner,
        files: &FileRegistry,
        apidoc: Option<&'a ApidocDict>,
        fields_dict: Option<&'a FieldsDict>,
        rust_decl_dict: Option<&'a RustDeclDict>,
        inline_fn_dict: Option<&'a InlineFnDict>,
        typedefs: &HashSet<InternedStr>,
        thx_symbols: (InternedStr, InternedStr, InternedStr),
        no_expand: NoExpandSymbols,
    ) {
        // Step 1: 全マクロの初期構築（パースのみ、型推論なし）
        let mut thx_initial = HashSet::new();
        let mut pasting_initial = HashSet::new();

        for def in macro_table.iter_target_macros() {
            let (info, has_pasting, has_thx) = self.build_macro_info(
                def, macro_table, interner, files, rust_decl_dict, typedefs, thx_symbols, no_expand
            );
            if has_pasting {
                pasting_initial.insert(def.name);
            }
            if has_thx {
                thx_initial.insert(def.name);
            }
            self.register(info);
        }

        // Step 2: used_by を構築
        self.build_use_relations();

        // Step 3: THX の推移閉包を計算（used_by 経由）
        self.propagate_flag_via_used_by(&thx_initial, true);

        // Step 4: ## の推移閉包を計算（used_by 経由）
        self.propagate_flag_via_used_by(&pasting_initial, false);

        // Step 5: 全マクロを unconfirmed に
        for name in self.macros.keys().copied().collect::<Vec<_>>() {
            self.unconfirmed.insert(name);
        }

        // Step 6: 依存順に型推論
        self.infer_types_in_dependency_order(
            macro_table, interner, files, apidoc, fields_dict, rust_decl_dict, inline_fn_dict, typedefs
        );
    }

    /// used_by を辿ってフラグを推移的に伝播
    ///
    /// is_thx が true の場合は is_thx_dependent を、false の場合は has_token_pasting を設定
    fn propagate_flag_via_used_by(&mut self, initial_set: &HashSet<InternedStr>, is_thx: bool) {
        // 初期集合のフラグを設定
        for name in initial_set {
            if let Some(info) = self.macros.get_mut(name) {
                if is_thx {
                    info.is_thx_dependent = true;
                } else {
                    info.has_token_pasting = true;
                }
            }
        }

        // used_by を辿って伝播
        let mut to_propagate: Vec<InternedStr> = initial_set.iter().copied().collect();

        while let Some(name) = to_propagate.pop() {
            let used_by_list: Vec<InternedStr> = self.macros
                .get(&name)
                .map(|info| info.used_by.iter().copied().collect())
                .unwrap_or_default();

            for user in used_by_list {
                if let Some(user_info) = self.macros.get_mut(&user) {
                    let flag = if is_thx {
                        &mut user_info.is_thx_dependent
                    } else {
                        &mut user_info.has_token_pasting
                    };
                    if !*flag {
                        *flag = true;
                        to_propagate.push(user);
                    }
                }
            }
        }
    }

    /// 依存順に型推論を実行
    fn infer_types_in_dependency_order<'a>(
        &mut self,
        macro_table: &MacroTable,
        interner: &'a StringInterner,
        files: &FileRegistry,
        apidoc: Option<&'a ApidocDict>,
        fields_dict: Option<&'a FieldsDict>,
        rust_decl_dict: Option<&'a RustDeclDict>,
        inline_fn_dict: Option<&'a InlineFnDict>,
        typedefs: &HashSet<InternedStr>,
    ) {
        // 確定済みマクロの戻り値型キャッシュ（O(N²) を避けるため）
        let mut return_types_cache: HashMap<String, String> = HashMap::new();

        loop {
            let candidates = self.get_inference_candidates();
            if candidates.is_empty() {
                // 残りを全て unknown へ
                let remaining: Vec<_> = self.unconfirmed.iter().copied().collect();
                for name in remaining {
                    self.move_to_unknown(name);
                }
                break;
            }

            for name in candidates {
                // パラメータを取得
                let params: Vec<InternedStr> = macro_table
                    .get(name)
                    .map(|def| match &def.kind {
                        MacroKind::Function { params, .. } => params.clone(),
                        MacroKind::Object => vec![],
                    })
                    .unwrap_or_default();

                // 型推論を実行（キャッシュを渡す）
                self.infer_macro_types(
                    name, &params, interner, files, apidoc, fields_dict, rust_decl_dict, inline_fn_dict, typedefs,
                    &return_types_cache,
                );

                // 推論結果に基づいて分類
                let is_confirmed = self.macros.get(&name)
                    .map(|info| {
                        // 戻り値型が決まっていれば confirmed とする
                        // MacroInferInfo::get_return_type() を使用（ルート式の型も考慮）
                        info.get_return_type().is_some()
                    })
                    .unwrap_or(false);

                if is_confirmed {
                    // キャッシュに戻り値型を追加
                    if let Some((macro_name, return_type)) = self.get_macro_return_type(name, interner) {
                        return_types_cache.insert(macro_name, return_type);
                    }
                    self.mark_confirmed(name);
                } else {
                    self.move_to_unknown(name);
                }
            }
        }
    }

    /// 式から使用される関数/マクロを再帰的に収集
    pub fn collect_uses_from_expr(
        expr: &Expr,
        uses: &mut HashSet<InternedStr>,
    ) {
        match &expr.kind {
            ExprKind::Call { func, args } => {
                // 関数名を収集
                if let ExprKind::Ident(name) = &func.kind {
                    uses.insert(*name);
                }
                Self::collect_uses_from_expr(func, uses);
                for arg in args {
                    Self::collect_uses_from_expr(arg, uses);
                }
            }
            ExprKind::Ident(name) => {
                uses.insert(*name);
            }
            ExprKind::Binary { lhs, rhs, .. } => {
                Self::collect_uses_from_expr(lhs, uses);
                Self::collect_uses_from_expr(rhs, uses);
            }
            ExprKind::Cast { expr: inner, .. }
            | ExprKind::PreInc(inner)
            | ExprKind::PreDec(inner)
            | ExprKind::PostInc(inner)
            | ExprKind::PostDec(inner)
            | ExprKind::AddrOf(inner)
            | ExprKind::Deref(inner)
            | ExprKind::UnaryPlus(inner)
            | ExprKind::UnaryMinus(inner)
            | ExprKind::BitNot(inner)
            | ExprKind::LogNot(inner)
            | ExprKind::Sizeof(inner) => {
                Self::collect_uses_from_expr(inner, uses);
            }
            ExprKind::Index { expr: base, index } => {
                Self::collect_uses_from_expr(base, uses);
                Self::collect_uses_from_expr(index, uses);
            }
            ExprKind::Member { expr: base, .. } | ExprKind::PtrMember { expr: base, .. } => {
                Self::collect_uses_from_expr(base, uses);
            }
            ExprKind::Conditional { cond, then_expr, else_expr } => {
                Self::collect_uses_from_expr(cond, uses);
                Self::collect_uses_from_expr(then_expr, uses);
                Self::collect_uses_from_expr(else_expr, uses);
            }
            ExprKind::Assign { lhs, rhs, .. } => {
                Self::collect_uses_from_expr(lhs, uses);
                Self::collect_uses_from_expr(rhs, uses);
            }
            ExprKind::Comma { lhs, rhs } => {
                Self::collect_uses_from_expr(lhs, uses);
                Self::collect_uses_from_expr(rhs, uses);
            }
            _ => {}
        }
    }
}

impl Default for MacroInferContext {
    fn default() -> Self {
        Self::new()
    }
}

/// マクロ名がアサーションマクロかどうかを判定
fn detect_assert_kind(name: &str) -> Option<AssertKind> {
    match name {
        "assert" => Some(AssertKind::Assert),
        "assert_" => Some(AssertKind::AssertUnderscore),
        _ => None,
    }
}

/// AST 内の assert/assert_ 呼び出しを Assert 式に変換
///
/// パース後に呼び出し、`Call { func: Ident("assert"), args }` を
/// `Assert { kind, condition }` に変換する。
fn convert_assert_calls(expr: &mut Expr, interner: &StringInterner) {
    match &mut expr.kind {
        ExprKind::Call { func, args } => {
            // 子を先に処理
            convert_assert_calls(func, interner);
            for arg in args.iter_mut() {
                convert_assert_calls(arg, interner);
            }

            // assert/assert_ 呼び出しを検出
            if let ExprKind::Ident(name) = &func.kind {
                let name_str = interner.get(*name);
                if let Some(kind) = detect_assert_kind(name_str) {
                    if let Some(condition) = args.pop() {
                        expr.kind = ExprKind::Assert {
                            kind,
                            condition: Box::new(condition),
                        };
                    }
                }
            }
        }
        ExprKind::Binary { lhs, rhs, .. } => {
            convert_assert_calls(lhs, interner);
            convert_assert_calls(rhs, interner);
        }
        ExprKind::Cast { expr: inner, .. }
        | ExprKind::PreInc(inner)
        | ExprKind::PreDec(inner)
        | ExprKind::PostInc(inner)
        | ExprKind::PostDec(inner)
        | ExprKind::AddrOf(inner)
        | ExprKind::Deref(inner)
        | ExprKind::UnaryPlus(inner)
        | ExprKind::UnaryMinus(inner)
        | ExprKind::BitNot(inner)
        | ExprKind::LogNot(inner)
        | ExprKind::Sizeof(inner) => {
            convert_assert_calls(inner, interner);
        }
        ExprKind::Index { expr: base, index } => {
            convert_assert_calls(base, interner);
            convert_assert_calls(index, interner);
        }
        ExprKind::Member { expr: base, .. } | ExprKind::PtrMember { expr: base, .. } => {
            convert_assert_calls(base, interner);
        }
        ExprKind::Conditional { cond, then_expr, else_expr } => {
            convert_assert_calls(cond, interner);
            convert_assert_calls(then_expr, interner);
            convert_assert_calls(else_expr, interner);
        }
        ExprKind::Assign { lhs, rhs, .. } => {
            convert_assert_calls(lhs, interner);
            convert_assert_calls(rhs, interner);
        }
        ExprKind::Comma { lhs, rhs } => {
            convert_assert_calls(lhs, interner);
            convert_assert_calls(rhs, interner);
        }
        ExprKind::Assert { condition, .. } => {
            convert_assert_calls(condition, interner);
        }
        ExprKind::CompoundLit { init, .. } => {
            for item in init {
                if let crate::ast::Initializer::Expr(e) = &mut item.init {
                    convert_assert_calls(e, interner);
                }
            }
        }
        ExprKind::StmtExpr(compound) => {
            for item in &mut compound.items {
                if let BlockItem::Stmt(stmt) = item {
                    convert_assert_calls_in_stmt(stmt, interner);
                }
            }
        }
        // リテラルや識別子など、再帰不要
        ExprKind::Ident(_)
        | ExprKind::IntLit(_)
        | ExprKind::UIntLit(_)
        | ExprKind::FloatLit(_)
        | ExprKind::CharLit(_)
        | ExprKind::StringLit(_)
        | ExprKind::SizeofType(_)
        | ExprKind::Alignof(_) => {}
    }
}

/// Statement 内の assert 呼び出しを変換
fn convert_assert_calls_in_stmt(stmt: &mut crate::ast::Stmt, interner: &StringInterner) {
    use crate::ast::Stmt;
    match stmt {
        Stmt::Expr(Some(expr), _) => convert_assert_calls(expr, interner),
        Stmt::If { cond, then_stmt, else_stmt, .. } => {
            convert_assert_calls(cond, interner);
            convert_assert_calls_in_stmt(then_stmt, interner);
            if let Some(else_s) = else_stmt {
                convert_assert_calls_in_stmt(else_s, interner);
            }
        }
        Stmt::While { cond, body, .. } => {
            convert_assert_calls(cond, interner);
            convert_assert_calls_in_stmt(body, interner);
        }
        Stmt::DoWhile { body, cond, .. } => {
            convert_assert_calls_in_stmt(body, interner);
            convert_assert_calls(cond, interner);
        }
        Stmt::For { init, cond, step, body, .. } => {
            if let Some(crate::ast::ForInit::Expr(e)) = init {
                convert_assert_calls(e, interner);
            }
            if let Some(c) = cond {
                convert_assert_calls(c, interner);
            }
            if let Some(s) = step {
                convert_assert_calls(s, interner);
            }
            convert_assert_calls_in_stmt(body, interner);
        }
        Stmt::Switch { expr, body, .. } => {
            convert_assert_calls(expr, interner);
            convert_assert_calls_in_stmt(body, interner);
        }
        Stmt::Return(Some(expr), _) => convert_assert_calls(expr, interner),
        Stmt::Compound(compound) => {
            for item in &mut compound.items {
                match item {
                    BlockItem::Stmt(s) => convert_assert_calls_in_stmt(s, interner),
                    BlockItem::Decl(_) => {}
                }
            }
        }
        Stmt::Label { stmt: s, .. }
        | Stmt::Case { stmt: s, .. }
        | Stmt::Default { stmt: s, .. } => {
            convert_assert_calls_in_stmt(s, interner);
        }
        _ => {}
    }
}

/// 推論統計
#[derive(Debug, Clone, Copy)]
pub struct MacroInferStats {
    pub total: usize,
    pub confirmed: usize,
    pub unconfirmed: usize,
    /// 引数の型が unknown のマクロ数
    pub args_unknown: usize,
    /// 戻り値の型が unknown のマクロ数
    pub return_unknown: usize,
}

impl std::fmt::Display for MacroInferStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "MacroInferStats {{ total: {}, confirmed: {}, unconfirmed: {}, args_unknown: {}, return_unknown: {} }}",
            self.total, self.confirmed, self.unconfirmed, self.args_unknown, self.return_unknown
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intern::StringInterner;

    #[test]
    fn test_macro_infer_info_new() {
        let mut interner = StringInterner::new();
        let name = interner.intern("MY_MACRO");

        let info = MacroInferInfo::new(name);

        assert_eq!(info.name, name);
        assert!(!info.is_target);
        assert!(!info.is_thx_dependent);
        assert!(!info.has_token_pasting);
        assert!(info.uses.is_empty());
        assert!(info.used_by.is_empty());
        assert!(!info.is_parseable());
        assert_eq!(info.args_infer_status, InferStatus::Pending);
        assert_eq!(info.return_infer_status, InferStatus::Pending);
    }

    #[test]
    fn test_macro_infer_context_register() {
        let mut interner = StringInterner::new();
        let name = interner.intern("FOO");

        let mut ctx = MacroInferContext::new();
        let info = MacroInferInfo::new(name);
        ctx.register(info);

        assert!(ctx.get(name).is_some());
        assert_eq!(ctx.macros.len(), 1);
    }

    #[test]
    fn test_build_use_relations() {
        let mut interner = StringInterner::new();
        let foo = interner.intern("FOO");
        let bar = interner.intern("BAR");
        let baz = interner.intern("BAZ");

        let mut ctx = MacroInferContext::new();

        // FOO uses BAR
        let mut foo_info = MacroInferInfo::new(foo);
        foo_info.add_use(bar);
        ctx.register(foo_info);

        // BAR uses BAZ
        let mut bar_info = MacroInferInfo::new(bar);
        bar_info.add_use(baz);
        ctx.register(bar_info);

        // BAZ is standalone
        let baz_info = MacroInferInfo::new(baz);
        ctx.register(baz_info);

        // Build relations
        ctx.build_use_relations();

        // BAR should be used_by FOO
        assert!(ctx.get(bar).unwrap().used_by.contains(&foo));
        // BAZ should be used_by BAR
        assert!(ctx.get(baz).unwrap().used_by.contains(&bar));
    }

    #[test]
    fn test_inference_candidates() {
        let mut interner = StringInterner::new();
        let foo = interner.intern("FOO");
        let bar = interner.intern("BAR");
        let baz = interner.intern("BAZ");

        let mut ctx = MacroInferContext::new();

        // FOO uses BAR
        let mut foo_info = MacroInferInfo::new(foo);
        foo_info.add_use(bar);
        ctx.register(foo_info);

        // BAR uses BAZ
        let mut bar_info = MacroInferInfo::new(bar);
        bar_info.add_use(baz);
        ctx.register(bar_info);

        // BAZ is standalone (confirmed)
        let mut baz_info = MacroInferInfo::new(baz);
        baz_info.args_infer_status = InferStatus::TypeComplete;
        baz_info.return_infer_status = InferStatus::TypeComplete;
        ctx.register(baz_info);

        ctx.classify_initial();

        // Initially, only BAZ is confirmed
        assert!(ctx.confirmed.contains(&baz));
        assert!(ctx.unconfirmed.contains(&foo));
        assert!(ctx.unconfirmed.contains(&bar));

        // Candidates: BAR (uses BAZ which is confirmed)
        let candidates = ctx.get_inference_candidates();
        assert_eq!(candidates, vec![bar]);

        // After confirming BAR
        ctx.mark_confirmed(bar);
        let candidates = ctx.get_inference_candidates();
        assert_eq!(candidates, vec![foo]);
    }

    // ============================================================================
    // SvANY パターン検出テスト
    // ============================================================================

    use crate::ast::{AbstractDeclarator, DeclSpecs, DerivedDecl, StructSpec, TypeQualifiers};
    use crate::source::SourceLocation;

    /// テスト用のヘルパー: 識別子式を作成
    fn make_ident_expr(id: InternedStr) -> Expr {
        Expr::new(ExprKind::Ident(id), SourceLocation::default())
    }

    /// テスト用のヘルパー: 関数呼び出し式を作成
    fn make_call_expr(func: Expr, args: Vec<Expr>) -> Expr {
        Expr::new(
            ExprKind::Call {
                func: Box::new(func),
                args,
            },
            SourceLocation::default(),
        )
    }

    /// テスト用のヘルパー: キャスト式を作成
    fn make_cast_expr(type_name: TypeName, expr: Expr) -> Expr {
        Expr::new(
            ExprKind::Cast {
                type_name: Box::new(type_name),
                expr: Box::new(expr),
            },
            SourceLocation::default(),
        )
    }

    /// テスト用のヘルパー: ポインタ型の TypeName を作成
    fn make_pointer_type(type_name: InternedStr) -> TypeName {
        TypeName {
            specs: DeclSpecs {
                storage: None,
                type_specs: vec![TypeSpec::TypedefName(type_name)],
                qualifiers: TypeQualifiers::default(),
                is_inline: false,
            },
            declarator: Some(AbstractDeclarator {
                derived: vec![DerivedDecl::Pointer(TypeQualifiers::default())],
            }),
        }
    }

    #[test]
    fn test_extract_sv_any_arg_simple() {
        let mut interner = StringInterner::new();
        let sv_any = interner.intern("SvANY");
        let av = interner.intern("av");

        // SvANY(av)
        let sv_any_call = make_call_expr(make_ident_expr(sv_any), vec![make_ident_expr(av)]);

        let result = extract_sv_any_arg(&sv_any_call, sv_any);
        assert_eq!(result, Some(av));
    }

    #[test]
    fn test_extract_sv_any_arg_wrong_function() {
        let mut interner = StringInterner::new();
        let sv_any = interner.intern("SvANY");
        let other = interner.intern("OtherFunc");
        let av = interner.intern("av");

        // OtherFunc(av)
        let call = make_call_expr(make_ident_expr(other), vec![make_ident_expr(av)]);

        let result = extract_sv_any_arg(&call, sv_any);
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_pointer_base_type_typedef() {
        let mut interner = StringInterner::new();
        let xpvav = interner.intern("XPVAV");

        let type_name = make_pointer_type(xpvav);
        let result = extract_pointer_base_type(&type_name, &interner);
        assert_eq!(result, Some("XPVAV".to_string()));
    }

    #[test]
    fn test_extract_pointer_base_type_struct() {
        let mut interner = StringInterner::new();
        let xpvav = interner.intern("XPVAV");

        let type_name = TypeName {
            specs: DeclSpecs {
                storage: None,
                type_specs: vec![TypeSpec::Struct(StructSpec {
                    name: Some(xpvav),
                    members: None,
                    loc: SourceLocation::default(),
                })],
                qualifiers: TypeQualifiers::default(),
                is_inline: false,
            },
            declarator: Some(AbstractDeclarator {
                derived: vec![DerivedDecl::Pointer(TypeQualifiers::default())],
            }),
        };

        let result = extract_pointer_base_type(&type_name, &interner);
        assert_eq!(result, Some("XPVAV".to_string()));
    }

    #[test]
    fn test_extract_pointer_base_type_non_pointer() {
        let mut interner = StringInterner::new();
        let xpvav = interner.intern("XPVAV");

        // 非ポインタ型
        let type_name = TypeName {
            specs: DeclSpecs {
                storage: None,
                type_specs: vec![TypeSpec::TypedefName(xpvav)],
                qualifiers: TypeQualifiers::default(),
                is_inline: false,
            },
            declarator: None, // ポインタなし
        };

        let result = extract_pointer_base_type(&type_name, &interner);
        assert_eq!(result, None);
    }

    #[test]
    fn test_detect_sv_any_patterns_simple_cast() {
        let mut interner = StringInterner::new();
        let sv_any = interner.intern("SvANY");
        let av = interner.intern("av");
        let xpvav = interner.intern("XPVAV");

        // (XPVAV*) SvANY(av)
        let sv_any_call = make_call_expr(make_ident_expr(sv_any), vec![make_ident_expr(av)]);
        let cast_expr = make_cast_expr(make_pointer_type(xpvav), sv_any_call);

        let patterns = detect_sv_any_patterns(&cast_expr, sv_any, &interner);

        assert_eq!(patterns.len(), 1);
        assert_eq!(patterns[0].cast_type, "XPVAV");
        assert_eq!(patterns[0].arg_ident, av);
    }

    #[test]
    fn test_detect_sv_any_patterns_with_member_access() {
        let mut interner = StringInterner::new();
        let sv_any = interner.intern("SvANY");
        let av = interner.intern("av");
        let xpvav = interner.intern("XPVAV");
        let xav_alloc = interner.intern("xav_alloc");

        // ((XPVAV*) SvANY(av))->xav_alloc
        let sv_any_call = make_call_expr(make_ident_expr(sv_any), vec![make_ident_expr(av)]);
        let cast_expr = make_cast_expr(make_pointer_type(xpvav), sv_any_call);
        let ptr_member_expr = Expr::new(
            ExprKind::PtrMember {
                expr: Box::new(cast_expr),
                member: xav_alloc,
            },
            SourceLocation::default(),
        );

        let patterns = detect_sv_any_patterns(&ptr_member_expr, sv_any, &interner);

        assert_eq!(patterns.len(), 1);
        assert_eq!(patterns[0].cast_type, "XPVAV");
        assert_eq!(patterns[0].arg_ident, av);
    }

    #[test]
    fn test_detect_sv_any_patterns_through_mutable_ptr() {
        let mut interner = StringInterner::new();
        let sv_any = interner.intern("SvANY");
        let mutable_ptr = interner.intern("MUTABLE_PTR");
        let sv = interner.intern("sv");
        let xpvcv = interner.intern("XPVCV");

        // (XPVCV*) MUTABLE_PTR(SvANY(sv))
        let sv_any_call = make_call_expr(make_ident_expr(sv_any), vec![make_ident_expr(sv)]);
        let mutable_ptr_call = make_call_expr(make_ident_expr(mutable_ptr), vec![sv_any_call]);
        let cast_expr = make_cast_expr(make_pointer_type(xpvcv), mutable_ptr_call);

        let patterns = detect_sv_any_patterns(&cast_expr, sv_any, &interner);

        assert_eq!(patterns.len(), 1);
        assert_eq!(patterns[0].cast_type, "XPVCV");
        assert_eq!(patterns[0].arg_ident, sv);
    }

    #[test]
    fn test_detect_sv_any_patterns_nested() {
        let mut interner = StringInterner::new();
        let sv_any = interner.intern("SvANY");
        let av = interner.intern("av");
        let gv = interner.intern("gv");
        let xpvav = interner.intern("XPVAV");
        let xpvgv = interner.intern("XPVGV");

        // 複数のパターンがネストした場合:
        // cond ? ((XPVAV*) SvANY(av)) : ((XPVGV*) SvANY(gv))
        let sv_any_av = make_call_expr(make_ident_expr(sv_any), vec![make_ident_expr(av)]);
        let cast_av = make_cast_expr(make_pointer_type(xpvav), sv_any_av);

        let sv_any_gv = make_call_expr(make_ident_expr(sv_any), vec![make_ident_expr(gv)]);
        let cast_gv = make_cast_expr(make_pointer_type(xpvgv), sv_any_gv);

        let cond = make_ident_expr(interner.intern("cond"));
        let conditional = Expr::new(
            ExprKind::Conditional {
                cond: Box::new(cond),
                then_expr: Box::new(cast_av),
                else_expr: Box::new(cast_gv),
            },
            SourceLocation::default(),
        );

        let patterns = detect_sv_any_patterns(&conditional, sv_any, &interner);

        assert_eq!(patterns.len(), 2);

        // 順序は検出順序に依存するため、どちらかが含まれていることを確認
        let types: Vec<_> = patterns.iter().map(|p| p.cast_type.as_str()).collect();
        assert!(types.contains(&"XPVAV"));
        assert!(types.contains(&"XPVGV"));
    }

    #[test]
    fn test_no_expand_symbols_new() {
        let mut interner = StringInterner::new();
        let symbols = NoExpandSymbols::new(&mut interner);

        assert_eq!(interner.get(symbols.assert), "assert");
        assert_eq!(interner.get(symbols.assert_), "assert_");
        assert_eq!(interner.get(symbols.sv_any), "SvANY");
    }

    #[test]
    fn test_no_expand_symbols_iter() {
        let mut interner = StringInterner::new();
        let symbols = NoExpandSymbols::new(&mut interner);

        let syms: Vec<_> = symbols.iter().collect();
        assert_eq!(syms.len(), 3);
        assert!(syms.contains(&symbols.assert));
        assert!(syms.contains(&symbols.assert_));
        assert!(syms.contains(&symbols.sv_any));
    }
}
