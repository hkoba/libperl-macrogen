//! 関数呼び出しからの型推論
//!
//! 関数呼び出しの引数と呼び出し先関数のパラメータ型を照合し、
//! 未知の引数の型を推論する。

use std::collections::HashMap;

use crate::ast::{BlockItem, CompoundStmt, Expr, Stmt};
use crate::intern::{InternedStr, StringInterner};
use crate::rust_decl::RustDeclDict;

/// 式から関数呼び出しを解析してパラメータの型を推論
pub fn infer_param_types_from_expr(
    expr: &Expr,
    params: &[InternedStr],
    rust_decls: &RustDeclDict,
    interner: &StringInterner,
) -> HashMap<InternedStr, String> {
    let mut result = HashMap::new();
    let param_set: std::collections::HashSet<_> = params.iter().copied().collect();

    visit_expr(expr, &param_set, rust_decls, interner, &mut result);

    result
}

/// 複合文（関数本体）から関数呼び出しを解析してパラメータの型を推論
pub fn infer_param_types_from_body(
    body: &CompoundStmt,
    params: &[InternedStr],
    rust_decls: &RustDeclDict,
    interner: &StringInterner,
) -> HashMap<InternedStr, String> {
    let mut result = HashMap::new();
    let param_set: std::collections::HashSet<_> = params.iter().copied().collect();

    visit_compound_stmt(body, &param_set, rust_decls, interner, &mut result);

    result
}

/// 式を再帰的に走査して関数呼び出しを処理
fn visit_expr(
    expr: &Expr,
    params: &std::collections::HashSet<InternedStr>,
    rust_decls: &RustDeclDict,
    interner: &StringInterner,
    result: &mut HashMap<InternedStr, String>,
) {
    match expr {
        Expr::Call { func, args, .. } => {
            // 関数名を取得
            if let Expr::Ident(func_name, _) = func.as_ref() {
                let func_name_str = interner.get(*func_name);

                // RustDeclDictから関数シグネチャを取得
                if let Some(rust_fn) = rust_decls.fns.get(func_name_str) {
                    // 引数とパラメータを照合
                    for (i, arg) in args.iter().enumerate() {
                        if i < rust_fn.params.len() {
                            let expected_type = &rust_fn.params[i].ty;
                            infer_from_arg(arg, expected_type, params, result);
                        }
                    }
                }
            }

            // 引数内の式も再帰的に走査
            for arg in args {
                visit_expr(arg, params, rust_decls, interner, result);
            }

            // 関数式自体も走査（関数ポインタ経由の呼び出し等）
            visit_expr(func, params, rust_decls, interner, result);
        }

        // 他の式タイプを再帰的に走査
        Expr::Binary { lhs, rhs, .. } => {
            visit_expr(lhs, params, rust_decls, interner, result);
            visit_expr(rhs, params, rust_decls, interner, result);
        }
        Expr::UnaryPlus(inner, _)
        | Expr::UnaryMinus(inner, _)
        | Expr::BitNot(inner, _)
        | Expr::LogNot(inner, _)
        | Expr::Deref(inner, _)
        | Expr::AddrOf(inner, _)
        | Expr::PreInc(inner, _)
        | Expr::PreDec(inner, _)
        | Expr::PostInc(inner, _)
        | Expr::PostDec(inner, _) => {
            visit_expr(inner, params, rust_decls, interner, result);
        }
        Expr::Member { expr, .. } | Expr::PtrMember { expr, .. } => {
            visit_expr(expr, params, rust_decls, interner, result);
        }
        Expr::Index { expr, index, .. } => {
            visit_expr(expr, params, rust_decls, interner, result);
            visit_expr(index, params, rust_decls, interner, result);
        }
        Expr::Cast { expr, .. } => {
            visit_expr(expr, params, rust_decls, interner, result);
        }
        Expr::Sizeof(inner, _) => {
            visit_expr(inner, params, rust_decls, interner, result);
        }
        Expr::Conditional { cond, then_expr, else_expr, .. } => {
            visit_expr(cond, params, rust_decls, interner, result);
            visit_expr(then_expr, params, rust_decls, interner, result);
            visit_expr(else_expr, params, rust_decls, interner, result);
        }
        Expr::Comma { lhs, rhs, .. } => {
            visit_expr(lhs, params, rust_decls, interner, result);
            visit_expr(rhs, params, rust_decls, interner, result);
        }
        Expr::Assign { lhs, rhs, .. } => {
            visit_expr(lhs, params, rust_decls, interner, result);
            visit_expr(rhs, params, rust_decls, interner, result);
        }
        Expr::StmtExpr(compound, _) => {
            visit_compound_stmt(compound, params, rust_decls, interner, result);
        }
        // リテラルや識別子は走査不要
        Expr::Ident(_, _)
        | Expr::IntLit(_, _)
        | Expr::UIntLit(_, _)
        | Expr::FloatLit(_, _)
        | Expr::CharLit(_, _)
        | Expr::StringLit(_, _)
        | Expr::SizeofType(_, _)
        | Expr::Alignof(_, _)
        | Expr::CompoundLit { .. } => {}
    }
}

/// 複合文を走査
fn visit_compound_stmt(
    compound: &CompoundStmt,
    params: &std::collections::HashSet<InternedStr>,
    rust_decls: &RustDeclDict,
    interner: &StringInterner,
    result: &mut HashMap<InternedStr, String>,
) {
    for item in &compound.items {
        match item {
            BlockItem::Decl(decl) => {
                // 初期化子内の式を走査
                for init_decl in &decl.declarators {
                    if let Some(ref init) = init_decl.init {
                        visit_initializer(init, params, rust_decls, interner, result);
                    }
                }
            }
            BlockItem::Stmt(stmt) => {
                visit_stmt(stmt, params, rust_decls, interner, result);
            }
        }
    }
}

/// 初期化子を走査
fn visit_initializer(
    init: &crate::ast::Initializer,
    params: &std::collections::HashSet<InternedStr>,
    rust_decls: &RustDeclDict,
    interner: &StringInterner,
    result: &mut HashMap<InternedStr, String>,
) {
    match init {
        crate::ast::Initializer::Expr(expr) => {
            visit_expr(expr, params, rust_decls, interner, result);
        }
        crate::ast::Initializer::List(items) => {
            for item in items {
                visit_initializer(&item.init, params, rust_decls, interner, result);
            }
        }
    }
}

/// 文を走査
fn visit_stmt(
    stmt: &Stmt,
    params: &std::collections::HashSet<InternedStr>,
    rust_decls: &RustDeclDict,
    interner: &StringInterner,
    result: &mut HashMap<InternedStr, String>,
) {
    match stmt {
        Stmt::Compound(compound) => {
            visit_compound_stmt(compound, params, rust_decls, interner, result);
        }
        Stmt::Expr(Some(expr), _) => {
            visit_expr(expr, params, rust_decls, interner, result);
        }
        Stmt::Expr(None, _) => {}
        Stmt::If { cond, then_stmt, else_stmt, .. } => {
            visit_expr(cond, params, rust_decls, interner, result);
            visit_stmt(then_stmt, params, rust_decls, interner, result);
            if let Some(else_s) = else_stmt {
                visit_stmt(else_s, params, rust_decls, interner, result);
            }
        }
        Stmt::Switch { expr, body, .. } => {
            visit_expr(expr, params, rust_decls, interner, result);
            visit_stmt(body, params, rust_decls, interner, result);
        }
        Stmt::While { cond, body, .. } => {
            visit_expr(cond, params, rust_decls, interner, result);
            visit_stmt(body, params, rust_decls, interner, result);
        }
        Stmt::DoWhile { body, cond, .. } => {
            visit_stmt(body, params, rust_decls, interner, result);
            visit_expr(cond, params, rust_decls, interner, result);
        }
        Stmt::For { init, cond, step, body, .. } => {
            if let Some(init) = init {
                match init {
                    crate::ast::ForInit::Expr(expr) => {
                        visit_expr(expr, params, rust_decls, interner, result);
                    }
                    crate::ast::ForInit::Decl(decl) => {
                        for init_decl in &decl.declarators {
                            if let Some(ref init) = init_decl.init {
                                visit_initializer(init, params, rust_decls, interner, result);
                            }
                        }
                    }
                }
            }
            if let Some(cond) = cond {
                visit_expr(cond, params, rust_decls, interner, result);
            }
            if let Some(step) = step {
                visit_expr(step, params, rust_decls, interner, result);
            }
            visit_stmt(body, params, rust_decls, interner, result);
        }
        Stmt::Return(Some(expr), _) => {
            visit_expr(expr, params, rust_decls, interner, result);
        }
        Stmt::Return(None, _) | Stmt::Goto(_, _) | Stmt::Continue(_) | Stmt::Break(_) | Stmt::Asm { .. } => {}
        Stmt::Label { stmt, .. } => {
            visit_stmt(stmt, params, rust_decls, interner, result);
        }
        Stmt::Case { expr, stmt, .. } => {
            visit_expr(expr, params, rust_decls, interner, result);
            visit_stmt(stmt, params, rust_decls, interner, result);
        }
        Stmt::Default { stmt, .. } => {
            visit_stmt(stmt, params, rust_decls, interner, result);
        }
    }
}

/// 引数から型を推論
fn infer_from_arg(
    arg: &Expr,
    expected_type: &str,
    params: &std::collections::HashSet<InternedStr>,
    result: &mut HashMap<InternedStr, String>,
) {
    match arg {
        // 単純な識別子: 直接その型を使用
        Expr::Ident(id, _) => {
            if params.contains(id) && !result.contains_key(id) {
                result.insert(*id, normalize_type(expected_type));
            }
        }
        // &param: ポインタ型から参照先の型を導出
        Expr::AddrOf(inner, _) => {
            if let Expr::Ident(id, _) = inner.as_ref() {
                if params.contains(id) && !result.contains_key(id) {
                    // *const T や *mut T から T を取り出す
                    if let Some(pointee_type) = strip_pointer_type(expected_type) {
                        result.insert(*id, pointee_type);
                    }
                }
            }
        }
        // (param as Type) のようなキャスト
        Expr::Cast { expr, .. } => {
            if let Expr::Ident(id, _) = expr.as_ref() {
                if params.contains(id) && !result.contains_key(id) {
                    result.insert(*id, normalize_type(expected_type));
                }
            }
        }
        _ => {}
    }
}

/// ポインタ型から参照先の型を取り出す
/// "*const T" -> "T", "*mut T" -> "T"
fn strip_pointer_type(ty: &str) -> Option<String> {
    let trimmed = ty.trim();
    if trimmed.starts_with("* const ") {
        Some(trimmed[8..].trim().to_string())
    } else if trimmed.starts_with("*const ") {
        Some(trimmed[7..].trim().to_string())
    } else if trimmed.starts_with("* mut ") {
        Some(trimmed[6..].trim().to_string())
    } else if trimmed.starts_with("*mut ") {
        Some(trimmed[5..].trim().to_string())
    } else {
        None
    }
}

/// 型を正規化（synの出力形式からRust標準形式に）
fn normalize_type(ty: &str) -> String {
    // synは空白を入れることがある: "* const SV" -> "*const SV"
    ty.replace("* const ", "*const ")
      .replace("* mut ", "*mut ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_pointer_type() {
        assert_eq!(strip_pointer_type("*const SV"), Some("SV".to_string()));
        assert_eq!(strip_pointer_type("*mut STRLEN"), Some("STRLEN".to_string()));
        assert_eq!(strip_pointer_type("* const SV"), Some("SV".to_string()));
        assert_eq!(strip_pointer_type("* mut STRLEN"), Some("STRLEN".to_string()));
        assert_eq!(strip_pointer_type("c_int"), None);
    }

    #[test]
    fn test_normalize_type() {
        assert_eq!(normalize_type("* const SV"), "*const SV");
        assert_eq!(normalize_type("* mut STRLEN"), "*mut STRLEN");
        assert_eq!(normalize_type("c_int"), "c_int");
    }
}
