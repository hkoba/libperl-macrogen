//! Phase 2: 関数/マクロ本体の名前 (パラメータ・局所変数) 使用解析
//!
//! パラメータと局所変数それぞれについて、以下を **一度の走査** で判定する:
//!
//! - `needs_mut` — 再代入・複合代入・増減・AddrOf があるか (GH #16: `let mut` / `mut` param)
//! - `used` — 1 回でも参照されたか (GH #22: 未使用なら `_` prefix)
//! - `addr_taken_before_assign` — 無条件の初期化より前にアドレスを取られたか
//!   (GH #23: 未初期化宣言の out-param パターン → zeroed 初期化)
//!
//! Pass Separation Rule に従い解析は本モジュール (Phase 2) に置き、
//! Phase 3 (rust_codegen) は結果を読むだけにする。旧 `collect_mut_params`
//! 系 (rust_codegen.rs) の置き換えであり、旧実装が落としていた
//! Switch/Label/Case/Default・ネストした宣言・ForInit::Decl も走査する。
//!
//! 名前の登録は走査順 (= ソース順) に行うため、C の「宣言より前の同名参照」
//! は外側スコープの別実体として自然に無視される。シャドウイングは名前単位で
//! マージする (保守的)。

use std::collections::{HashMap, HashSet};

use crate::ast::{
    BlockItem, BuiltinArg, CompoundStmt, Declaration, DerivedDecl, Expr, ExprKind, ForInit,
    FunctionDef, Initializer, InitializerItem, Stmt,
};
use crate::intern::InternedStr;
use crate::macro_infer::{MacroParam, ParseResult};

/// 1 つの名前 (パラメータまたは局所変数) の使用状況
#[derive(Debug, Default, Clone)]
pub struct NameUsage {
    /// 読み書きいずれかで 1 回でも参照されたか
    pub used: bool,
    /// 再代入・複合代入・増減・AddrOf があり `mut` 宣言が必要か
    pub needs_mut: bool,
    /// 無条件の初期化 (宣言時初期化子 or 無条件文脈の単純代入) より前に
    /// アドレスを取られたか。未初期化宣言でこれが立つ場合、deferred
    /// initialization (`let x: T;`) では E0381 になるため zeroed 初期化が必要
    pub addr_taken_before_assign: bool,
    /// 走査内部状態: 無条件文脈 (If/Switch/ループ本体/三項の外) で
    /// 単純代入または宣言時初期化があったか
    assigned_unconditionally: bool,
}

/// 本体走査の結果 (名前 → NameUsage)
#[derive(Debug, Default, Clone)]
pub struct LocalUsageAnalysis {
    names: HashMap<InternedStr, NameUsage>,
}

impl LocalUsageAnalysis {
    /// `mut` 宣言が必要か
    pub fn needs_mut(&self, name: InternedStr) -> bool {
        self.names.get(&name).is_some_and(|u| u.needs_mut)
    }

    /// 追跡対象でありかつ一度も参照されていないか (→ `_` prefix)
    pub fn is_unused(&self, name: InternedStr) -> bool {
        self.names.get(&name).is_some_and(|u| !u.used)
    }

    /// 未初期化宣言に zeroed 初期化が必要か (代入前 AddrOf があったか)
    pub fn needs_zeroed_init(&self, name: InternedStr) -> bool {
        self.names.get(&name).is_some_and(|u| u.addr_taken_before_assign)
    }

    /// `needs_mut` な名前の集合 (旧 `collect_mut_params` の戻り値互換)
    pub fn mut_names(&self) -> HashSet<InternedStr> {
        self.names
            .iter()
            .filter(|(_, u)| u.needs_mut)
            .map(|(n, _)| *n)
            .collect()
    }

    fn register(&mut self, name: InternedStr) {
        self.names.entry(name).or_default();
    }

    fn mark_used(&mut self, name: InternedStr) {
        if let Some(u) = self.names.get_mut(&name) {
            u.used = true;
        }
    }

    fn mark_mut(&mut self, name: InternedStr) {
        if let Some(u) = self.names.get_mut(&name) {
            u.needs_mut = true;
        }
    }

    fn mark_addr_of(&mut self, name: InternedStr) {
        if let Some(u) = self.names.get_mut(&name) {
            u.needs_mut = true;
            if !u.assigned_unconditionally {
                u.addr_taken_before_assign = true;
            }
        }
    }

    fn mark_assigned(&mut self, name: InternedStr, unconditional: bool) {
        if let Some(u) = self.names.get_mut(&name) {
            u.needs_mut = true;
            if unconditional {
                u.assigned_unconditionally = true;
            }
        }
    }
}

/// inline 関数 (FunctionDef) を解析。
/// パラメータと、ネストも含む全 `BlockItem::Decl` の局所変数が対象。
pub fn analyze_function(func_def: &FunctionDef) -> LocalUsageAnalysis {
    let mut analysis = LocalUsageAnalysis::default();
    for d in &func_def.declarator.derived {
        if let DerivedDecl::Function(param_list) = d {
            for p in &param_list.params {
                if let Some(ref declarator) = p.declarator {
                    if let Some(param_name) = declarator.name {
                        analysis.register(param_name);
                    }
                }
            }
        }
    }
    let mut walker = Walker { analysis: &mut analysis, cond_depth: 0 };
    walker.items(&func_def.body.items);
    analysis
}

/// マクロ (ParseResult) を仮引数付きで解析。
pub fn analyze_macro(parse_result: &ParseResult, params: &[MacroParam]) -> LocalUsageAnalysis {
    let mut analysis = LocalUsageAnalysis::default();
    for p in params {
        analysis.register(p.name);
    }
    let mut walker = Walker { analysis: &mut analysis, cond_depth: 0 };
    match parse_result {
        ParseResult::Expression(expr) => walker.expr(expr),
        ParseResult::Statement(items) => walker.items(items),
        ParseResult::Unparseable(_) => {}
    }
    analysis
}

struct Walker<'a> {
    analysis: &'a mut LocalUsageAnalysis,
    /// If/Switch/ループ本体/三項演算子の内側の深さ。
    /// > 0 の間の単純代入は「無条件の初期化」と見なさない
    /// (`if c { x = 1 } use(&x)` の x を E0381 から救うための保守判定)
    cond_depth: u32,
}

impl Walker<'_> {
    fn items(&mut self, items: &[BlockItem]) {
        for item in items {
            match item {
                BlockItem::Stmt(stmt) => self.stmt(stmt),
                BlockItem::Decl(decl) => self.decl(decl),
            }
        }
    }

    fn decl(&mut self, decl: &Declaration) {
        for init_decl in &decl.declarators {
            // 初期化子は自身の登録より先に走査する
            // (`int x = x0;` の x0 参照、シャドウ元の同名参照を正しく数える)
            if let Some(ref init) = init_decl.init {
                self.initializer(init);
            }
            if let Some(name) = init_decl.declarator.name {
                self.analysis.register(name);
                if init_decl.init.is_some() {
                    if let Some(u) = self.analysis.names.get_mut(&name) {
                        if self.cond_depth == 0 {
                            u.assigned_unconditionally = true;
                        }
                    }
                }
            }
        }
    }

    fn initializer(&mut self, init: &Initializer) {
        match init {
            Initializer::Expr(e) => self.expr(e),
            Initializer::List(items) => self.initializer_items(items),
        }
    }

    fn initializer_items(&mut self, items: &[InitializerItem]) {
        for item in items {
            self.initializer(&item.init);
        }
    }

    fn stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Compound(compound) => self.compound(compound),
            Stmt::Expr(Some(expr), _) => self.expr(expr),
            Stmt::Expr(None, _) => {}
            Stmt::If { cond, then_stmt, else_stmt, .. } => {
                self.expr(cond);
                self.cond_depth += 1;
                self.stmt(then_stmt);
                if let Some(else_s) = else_stmt {
                    self.stmt(else_s);
                }
                self.cond_depth -= 1;
            }
            Stmt::Switch { expr, body, .. } => {
                self.expr(expr);
                self.cond_depth += 1;
                self.stmt(body);
                self.cond_depth -= 1;
            }
            Stmt::While { cond, body, .. } => {
                // 条件は少なくとも 1 回無条件に評価される
                self.expr(cond);
                self.cond_depth += 1;
                self.stmt(body);
                self.cond_depth -= 1;
            }
            Stmt::DoWhile { body, cond, .. } => {
                // do-while の本体と条件は少なくとも 1 回無条件に実行される
                self.stmt(body);
                self.expr(cond);
            }
            Stmt::For { init, cond, step, body, .. } => {
                match init {
                    Some(ForInit::Expr(e)) => self.expr(e),
                    Some(ForInit::Decl(decl)) => self.decl(decl),
                    None => {}
                }
                if let Some(c) = cond {
                    self.expr(c);
                }
                self.cond_depth += 1;
                self.stmt(body);
                if let Some(s) = step {
                    self.expr(s);
                }
                self.cond_depth -= 1;
            }
            Stmt::Return(Some(expr), _) => self.expr(expr),
            Stmt::Return(None, _) => {}
            Stmt::Label { stmt, .. } => self.stmt(stmt),
            Stmt::Case { expr, stmt, .. } => {
                self.expr(expr);
                self.stmt(stmt);
            }
            Stmt::Default { stmt, .. } => self.stmt(stmt),
            Stmt::Goto(..) | Stmt::Continue(..) | Stmt::Break(..) | Stmt::Asm { .. } => {}
        }
    }

    fn compound(&mut self, compound: &CompoundStmt) {
        self.items(&compound.items);
    }

    fn expr(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::Ident(name) => self.analysis.mark_used(*name),
            ExprKind::AddrOf(inner) => {
                if let ExprKind::Ident(name) = &inner.kind {
                    self.analysis.mark_addr_of(*name);
                }
                self.expr(inner);
            }
            ExprKind::Assign { op, lhs, rhs } => {
                if let ExprKind::Ident(name) = &lhs.kind {
                    let unconditional = self.cond_depth == 0
                        && *op == crate::ast::AssignOp::Assign;
                    self.analysis.mark_assigned(*name, unconditional);
                }
                self.expr(lhs);
                self.expr(rhs);
            }
            ExprKind::PreInc(inner) | ExprKind::PreDec(inner)
            | ExprKind::PostInc(inner) | ExprKind::PostDec(inner) => {
                if let ExprKind::Ident(name) = &inner.kind {
                    self.analysis.mark_mut(*name);
                }
                self.expr(inner);
            }
            ExprKind::Conditional { cond, then_expr, else_expr } => {
                self.expr(cond);
                self.cond_depth += 1;
                self.expr(then_expr);
                self.expr(else_expr);
                self.cond_depth -= 1;
            }
            ExprKind::Binary { lhs, rhs, .. } | ExprKind::Comma { lhs, rhs } => {
                self.expr(lhs);
                self.expr(rhs);
            }
            ExprKind::Deref(inner)
            | ExprKind::UnaryPlus(inner)
            | ExprKind::UnaryMinus(inner)
            | ExprKind::BitNot(inner)
            | ExprKind::LogNot(inner)
            | ExprKind::Sizeof(inner)
            | ExprKind::Cast { expr: inner, .. } => self.expr(inner),
            ExprKind::Index { expr: base, index } => {
                self.expr(base);
                self.expr(index);
            }
            ExprKind::Call { func, args } => {
                self.expr(func);
                for arg in args {
                    self.expr(arg);
                }
            }
            ExprKind::MacroCall { expanded, args, .. } => {
                // used / mut は展開結果と元引数の両方から拾う
                // (codegen はどちらの形でも emit しうるため保守的に併合)
                self.expr(expanded);
                for arg in args {
                    self.expr(arg);
                }
            }
            ExprKind::BuiltinCall { args, .. } => {
                for arg in args {
                    if let BuiltinArg::Expr(e) = arg {
                        self.expr(e);
                    }
                }
            }
            ExprKind::Member { expr: inner, .. } | ExprKind::PtrMember { expr: inner, .. } => {
                self.expr(inner);
            }
            ExprKind::StmtExpr(compound) => self.compound(compound),
            ExprKind::Assert { condition, .. } => self.expr(condition),
            ExprKind::CompoundLit { init, .. } => self.initializer_items(init),
            ExprKind::IntLit(_)
            | ExprKind::UIntLit(_)
            | ExprKind::FloatLit(_)
            | ExprKind::CharLit(_)
            | ExprKind::StringLit(_)
            | ExprKind::SizeofType(_)
            | ExprKind::Alignof(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{AssignOp, Stmt};
    use crate::intern::StringInterner;
    use crate::source::SourceLocation;

    fn ident(name: InternedStr) -> Expr {
        Expr::new(ExprKind::Ident(name), SourceLocation::default())
    }

    fn int_lit(n: i64) -> Expr {
        Expr::new(ExprKind::IntLit(n), SourceLocation::default())
    }

    fn assign(name: InternedStr, value: Expr) -> Expr {
        Expr::new(
            ExprKind::Assign {
                op: AssignOp::Assign,
                lhs: Box::new(ident(name)),
                rhs: Box::new(value),
            },
            SourceLocation::default(),
        )
    }

    fn addr_of(name: InternedStr) -> Expr {
        Expr::new(ExprKind::AddrOf(Box::new(ident(name))), SourceLocation::default())
    }

    fn expr_stmt(e: Expr) -> Stmt {
        Stmt::Expr(Some(Box::new(e)), SourceLocation::default())
    }

    fn macro_params(names: &[InternedStr]) -> Vec<MacroParam> {
        names.iter().map(|n| MacroParam::new(*n, SourceLocation::default())).collect()
    }

    fn analyze_stmts(stmts: Vec<Stmt>, params: &[MacroParam]) -> LocalUsageAnalysis {
        let items = stmts.into_iter().map(BlockItem::Stmt).collect();
        analyze_macro(&ParseResult::Statement(items), params)
    }

    /// ループ本体内の再代入 (Perl_isC9_STRICT_UTF8_CHAR 型の E0384) を
    /// ネスト越しに検出できること
    #[test]
    fn test_reassign_in_nested_loop_needs_mut() {
        let mut interner = StringInterner::new();
        let s = interner.intern("s");
        let params = macro_params(&[s]);
        // while (s) { { s = 1; } }
        let body = Stmt::Compound(CompoundStmt {
            items: vec![BlockItem::Stmt(expr_stmt(assign(s, int_lit(1))))],
            info: Default::default(),
        });
        let stmts = vec![Stmt::While {
            cond: Box::new(ident(s)),
            body: Box::new(Stmt::Compound(CompoundStmt {
                items: vec![BlockItem::Stmt(body)],
                info: Default::default(),
            })),
            loc: SourceLocation::default(),
        }];
        let analysis = analyze_stmts(stmts, &params);
        assert!(analysis.needs_mut(s));
        assert!(!analysis.is_unused(s));
    }

    /// 本体で参照されないパラメータを未使用と判定できること
    #[test]
    fn test_unused_param() {
        let mut interner = StringInterner::new();
        let used = interner.intern("used");
        let unused = interner.intern("unused");
        let params = macro_params(&[used, unused]);
        let stmts = vec![expr_stmt(ident(used))];
        let analysis = analyze_stmts(stmts, &params);
        assert!(!analysis.is_unused(used));
        assert!(analysis.is_unused(unused));
        // 追跡外の名前は「未使用」と主張しない
        let other = interner.intern("other");
        assert!(!analysis.is_unused(other));
    }

    /// 代入前の AddrOf (out-param パターン) は zeroed 初期化が必要、
    /// 無条件代入後の AddrOf は不要
    #[test]
    fn test_addr_taken_before_assign() {
        let mut interner = StringInterner::new();
        let out = interner.intern("out");
        let ok = interner.intern("ok");
        let params = macro_params(&[out, ok]);
        // foo(&out); out = 1; ok = 2; bar(&ok);
        let stmts = vec![
            expr_stmt(addr_of(out)),
            expr_stmt(assign(out, int_lit(1))),
            expr_stmt(assign(ok, int_lit(2))),
            expr_stmt(addr_of(ok)),
        ];
        let analysis = analyze_stmts(stmts, &params);
        assert!(analysis.needs_zeroed_init(out));
        assert!(!analysis.needs_zeroed_init(ok));
        // AddrOf は mut 扱い (既存 collect_mut_params 互換)
        assert!(analysis.needs_mut(out));
        assert!(analysis.needs_mut(ok));
    }

    /// 条件付きの代入は「無条件の初期化」と見なさない
    /// (`if c { x = 1 } use(&x)` は E0381 になりうるので zeroed が必要)
    #[test]
    fn test_conditional_assign_does_not_clear_window() {
        let mut interner = StringInterner::new();
        let x = interner.intern("x");
        let c = interner.intern("c");
        let params = macro_params(&[x, c]);
        let stmts = vec![
            Stmt::If {
                cond: Box::new(ident(c)),
                then_stmt: Box::new(expr_stmt(assign(x, int_lit(1)))),
                else_stmt: None,
                loc: SourceLocation::default(),
            },
            expr_stmt(addr_of(x)),
        ];
        let analysis = analyze_stmts(stmts, &params);
        assert!(analysis.needs_zeroed_init(x));
    }
}
