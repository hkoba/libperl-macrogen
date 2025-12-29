//! 反復型推論
//!
//! マクロ関数とinline関数の引数型を反復的に推論する。
//! - bindings.rs の関数から始めて、
//! - マクロ関数とinline関数を呼び出し先の型情報を使って推論し、
//! - 推論が確定した関数を次の推論に利用する。

use std::collections::{HashMap, HashSet};

use crate::ast::{CompoundStmt, Expr};
use crate::intern::{InternedStr, StringInterner};
use crate::rust_decl::{RustDeclDict, RustFn};

/// 関数シグネチャ（確定済み）
#[derive(Debug, Clone)]
pub struct FunctionSignature {
    /// 関数名
    pub name: String,
    /// パラメータ（名前と型）
    pub params: Vec<(String, String)>,
    /// 戻り値型
    pub ret_ty: Option<String>,
}

impl FunctionSignature {
    /// RustFnから変換
    pub fn from_rust_fn(rust_fn: &RustFn) -> Self {
        Self {
            name: rust_fn.name.clone(),
            params: rust_fn.params.iter()
                .map(|p| (p.name.clone(), p.ty.clone()))
                .collect(),
            ret_ty: rust_fn.ret_ty.clone(),
        }
    }

    /// パラメータ数を取得
    pub fn param_count(&self) -> usize {
        self.params.len()
    }

    /// インデックスでパラメータの型を取得
    pub fn param_type(&self, index: usize) -> Option<&str> {
        self.params.get(index).map(|(_, ty)| ty.as_str())
    }
}

/// 未確定関数
#[derive(Debug)]
pub struct PendingFunction {
    /// 関数名
    pub name: String,
    /// パラメータ名のリスト
    pub param_names: Vec<InternedStr>,
    /// 確定済みのパラメータ型（パラメータ名 -> 型）
    pub known_types: HashMap<InternedStr, String>,
    /// 呼び出す関数のリスト（関数名）
    pub called_functions: Vec<String>,
    /// 戻り値型（判明している場合）
    pub ret_ty: Option<String>,
    /// 式（マクロ本体のパース結果）
    pub body_expr: Option<Expr>,
    /// 複合文（inline関数の本体）
    pub body_stmt: Option<CompoundStmt>,
}

impl PendingFunction {
    /// 未確定パラメータの数を取得
    pub fn unknown_param_count(&self) -> usize {
        self.param_names.len() - self.known_types.len()
    }

    /// 全パラメータが確定しているか
    pub fn is_fully_resolved(&self) -> bool {
        self.param_names.iter().all(|p| self.known_types.contains_key(p))
    }

    /// FunctionSignatureに変換（確定済みパラメータのみ）
    pub fn to_signature(&self, interner: &StringInterner) -> FunctionSignature {
        let params: Vec<_> = self.param_names.iter()
            .map(|p| {
                let name = interner.get(*p).to_string();
                let ty = self.known_types.get(p)
                    .cloned()
                    .unwrap_or_else(|| "UnknownType".to_string());
                (name, ty)
            })
            .collect();

        FunctionSignature {
            name: self.name.clone(),
            params,
            ret_ty: self.ret_ty.clone(),
        }
    }
}

/// 推論コンテキスト
pub struct InferenceContext<'a> {
    /// 確定済み関数シグネチャ
    confirmed: HashMap<String, FunctionSignature>,
    /// 未確定関数
    pending: Vec<PendingFunction>,
    /// 文字列インターナー
    interner: &'a StringInterner,
}

impl<'a> InferenceContext<'a> {
    /// 新しいコンテキストを作成
    pub fn new(interner: &'a StringInterner) -> Self {
        Self {
            confirmed: HashMap::new(),
            pending: Vec::new(),
            interner,
        }
    }

    /// bindings.rsから確定済み関数を読み込む
    pub fn load_bindings(&mut self, rust_decls: &RustDeclDict) {
        for (name, rust_fn) in &rust_decls.fns {
            self.confirmed.insert(name.clone(), FunctionSignature::from_rust_fn(rust_fn));
        }
    }

    /// 未確定関数を追加
    pub fn add_pending(&mut self, pending: PendingFunction) {
        self.pending.push(pending);
    }

    /// 確定済み関数を直接追加（inline関数など型が既知の場合）
    pub fn add_confirmed(&mut self, sig: FunctionSignature) {
        self.confirmed.insert(sig.name.clone(), sig);
    }

    /// 確定済み関数を取得
    pub fn get_confirmed(&self, name: &str) -> Option<&FunctionSignature> {
        self.confirmed.get(name)
    }

    /// 確定済み関数数を取得
    pub fn confirmed_count(&self) -> usize {
        self.confirmed.len()
    }

    /// 未確定関数数を取得
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// 反復推論を実行
    /// 戻り値: (推論が確定した関数数, 反復回数)
    pub fn run_inference(&mut self) -> (usize, usize) {
        let mut total_resolved = 0;
        let mut iterations = 0;

        loop {
            iterations += 1;
            let resolved_this_iteration = self.run_one_iteration();

            if resolved_this_iteration == 0 {
                break;
            }
            total_resolved += resolved_this_iteration;

            // 安全弁: 無限ループ防止
            if iterations > 1000 {
                eprintln!("Warning: Inference exceeded 1000 iterations, stopping");
                break;
            }
        }

        (total_resolved, iterations)
    }

    /// 1回の推論イテレーション
    /// 戻り値: この反復で確定した関数数
    fn run_one_iteration(&mut self) -> usize {
        // 呼び出す関数の数でソート（少ない順）
        self.pending.sort_by_key(|f| f.called_functions.len());

        // pending を一度取り出して処理
        let pending_list = std::mem::take(&mut self.pending);

        let mut newly_resolved = Vec::new();
        let mut still_pending = Vec::new();

        // 各未確定関数を処理
        for mut pending in pending_list {
            // 呼び出す関数から型を推論
            self.infer_types_for_pending(&mut pending);

            if pending.is_fully_resolved() {
                // 全パラメータが確定した
                let sig = pending.to_signature(self.interner);
                newly_resolved.push((pending.name.clone(), sig));
            } else {
                // まだ未確定パラメータがある
                still_pending.push(pending);
            }
        }

        let resolved_count = newly_resolved.len();

        // 確定した関数をconfirmedに追加
        for (name, sig) in newly_resolved {
            self.confirmed.insert(name, sig);
        }

        // まだ未確定な関数をpendingに戻す
        self.pending = still_pending;

        resolved_count
    }

    /// 未確定関数の型を推論
    fn infer_types_for_pending(&self, pending: &mut PendingFunction) {
        let param_set: HashSet<_> = pending.param_names.iter().copied().collect();

        // 式から推論
        if let Some(ref expr) = pending.body_expr {
            self.infer_from_expr(expr, &param_set, &mut pending.known_types);
        }

        // 複合文から推論
        if let Some(ref body) = pending.body_stmt {
            self.infer_from_compound_stmt(body, &param_set, &mut pending.known_types);
        }
    }

    /// 式から型を推論
    fn infer_from_expr(
        &self,
        expr: &Expr,
        params: &HashSet<InternedStr>,
        known_types: &mut HashMap<InternedStr, String>,
    ) {
        match expr {
            Expr::Call { func, args, .. } => {
                // 関数名を取得
                if let Expr::Ident(func_name, _) = func.as_ref() {
                    let func_name_str = self.interner.get(*func_name);

                    // 確定済み関数からシグネチャを取得
                    if let Some(sig) = self.confirmed.get(func_name_str) {
                        // 引数とパラメータを照合
                        for (i, arg) in args.iter().enumerate() {
                            if let Some(expected_type) = sig.param_type(i) {
                                self.infer_from_arg(arg, expected_type, params, known_types);
                            }
                        }
                    }
                }

                // 引数内の式も再帰的に走査
                for arg in args {
                    self.infer_from_expr(arg, params, known_types);
                }

                // 関数式自体も走査
                self.infer_from_expr(func, params, known_types);
            }

            // 他の式タイプを再帰的に走査
            Expr::Binary { lhs, rhs, .. } => {
                self.infer_from_expr(lhs, params, known_types);
                self.infer_from_expr(rhs, params, known_types);
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
                self.infer_from_expr(inner, params, known_types);
            }
            Expr::Member { expr, .. } | Expr::PtrMember { expr, .. } => {
                self.infer_from_expr(expr, params, known_types);
            }
            Expr::Index { expr, index, .. } => {
                self.infer_from_expr(expr, params, known_types);
                self.infer_from_expr(index, params, known_types);
            }
            Expr::Cast { expr, .. } => {
                self.infer_from_expr(expr, params, known_types);
            }
            Expr::Sizeof(inner, _) => {
                self.infer_from_expr(inner, params, known_types);
            }
            Expr::Conditional { cond, then_expr, else_expr, .. } => {
                self.infer_from_expr(cond, params, known_types);
                self.infer_from_expr(then_expr, params, known_types);
                self.infer_from_expr(else_expr, params, known_types);
            }
            Expr::Comma { lhs, rhs, .. } => {
                self.infer_from_expr(lhs, params, known_types);
                self.infer_from_expr(rhs, params, known_types);
            }
            Expr::Assign { lhs, rhs, .. } => {
                self.infer_from_expr(lhs, params, known_types);
                self.infer_from_expr(rhs, params, known_types);
            }
            Expr::StmtExpr(compound, _) => {
                self.infer_from_compound_stmt(compound, params, known_types);
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

    /// 複合文から型を推論
    fn infer_from_compound_stmt(
        &self,
        compound: &CompoundStmt,
        params: &HashSet<InternedStr>,
        known_types: &mut HashMap<InternedStr, String>,
    ) {
        use crate::ast::BlockItem;

        for item in &compound.items {
            match item {
                BlockItem::Decl(decl) => {
                    // 初期化子内の式を走査
                    for init_decl in &decl.declarators {
                        if let Some(ref init) = init_decl.init {
                            self.infer_from_initializer(init, params, known_types);
                        }
                    }
                }
                BlockItem::Stmt(stmt) => {
                    self.infer_from_stmt(stmt, params, known_types);
                }
            }
        }
    }

    /// 初期化子から型を推論
    fn infer_from_initializer(
        &self,
        init: &crate::ast::Initializer,
        params: &HashSet<InternedStr>,
        known_types: &mut HashMap<InternedStr, String>,
    ) {
        match init {
            crate::ast::Initializer::Expr(expr) => {
                self.infer_from_expr(expr, params, known_types);
            }
            crate::ast::Initializer::List(items) => {
                for item in items {
                    self.infer_from_initializer(&item.init, params, known_types);
                }
            }
        }
    }

    /// 文から型を推論
    fn infer_from_stmt(
        &self,
        stmt: &crate::ast::Stmt,
        params: &HashSet<InternedStr>,
        known_types: &mut HashMap<InternedStr, String>,
    ) {
        use crate::ast::{ForInit, Stmt};

        match stmt {
            Stmt::Compound(compound) => {
                self.infer_from_compound_stmt(compound, params, known_types);
            }
            Stmt::Expr(Some(expr), _) => {
                self.infer_from_expr(expr, params, known_types);
            }
            Stmt::Expr(None, _) => {}
            Stmt::If { cond, then_stmt, else_stmt, .. } => {
                self.infer_from_expr(cond, params, known_types);
                self.infer_from_stmt(then_stmt, params, known_types);
                if let Some(else_s) = else_stmt {
                    self.infer_from_stmt(else_s, params, known_types);
                }
            }
            Stmt::Switch { expr, body, .. } => {
                self.infer_from_expr(expr, params, known_types);
                self.infer_from_stmt(body, params, known_types);
            }
            Stmt::While { cond, body, .. } => {
                self.infer_from_expr(cond, params, known_types);
                self.infer_from_stmt(body, params, known_types);
            }
            Stmt::DoWhile { body, cond, .. } => {
                self.infer_from_stmt(body, params, known_types);
                self.infer_from_expr(cond, params, known_types);
            }
            Stmt::For { init, cond, step, body, .. } => {
                if let Some(init) = init {
                    match init {
                        ForInit::Expr(expr) => {
                            self.infer_from_expr(expr, params, known_types);
                        }
                        ForInit::Decl(decl) => {
                            for init_decl in &decl.declarators {
                                if let Some(ref init) = init_decl.init {
                                    self.infer_from_initializer(init, params, known_types);
                                }
                            }
                        }
                    }
                }
                if let Some(cond) = cond {
                    self.infer_from_expr(cond, params, known_types);
                }
                if let Some(step) = step {
                    self.infer_from_expr(step, params, known_types);
                }
                self.infer_from_stmt(body, params, known_types);
            }
            Stmt::Return(Some(expr), _) => {
                self.infer_from_expr(expr, params, known_types);
            }
            Stmt::Return(None, _) | Stmt::Goto(_, _) | Stmt::Continue(_) | Stmt::Break(_) | Stmt::Asm { .. } => {}
            Stmt::Label { stmt, .. } => {
                self.infer_from_stmt(stmt, params, known_types);
            }
            Stmt::Case { expr, stmt, .. } => {
                self.infer_from_expr(expr, params, known_types);
                self.infer_from_stmt(stmt, params, known_types);
            }
            Stmt::Default { stmt, .. } => {
                self.infer_from_stmt(stmt, params, known_types);
            }
        }
    }

    /// 引数から型を推論
    fn infer_from_arg(
        &self,
        arg: &Expr,
        expected_type: &str,
        params: &HashSet<InternedStr>,
        known_types: &mut HashMap<InternedStr, String>,
    ) {
        match arg {
            // 単純な識別子: 直接その型を使用
            Expr::Ident(id, _) => {
                if params.contains(id) && !known_types.contains_key(id) {
                    known_types.insert(*id, normalize_type(expected_type));
                }
            }
            // &param: ポインタ型から参照先の型を導出
            Expr::AddrOf(inner, _) => {
                if let Expr::Ident(id, _) = inner.as_ref() {
                    if params.contains(id) && !known_types.contains_key(id) {
                        // *const T や *mut T から T を取り出す
                        if let Some(pointee_type) = strip_pointer_type(expected_type) {
                            known_types.insert(*id, pointee_type);
                        }
                    }
                }
            }
            // (param as Type) のようなキャスト
            Expr::Cast { expr, .. } => {
                if let Expr::Ident(id, _) = expr.as_ref() {
                    if params.contains(id) && !known_types.contains_key(id) {
                        known_types.insert(*id, normalize_type(expected_type));
                    }
                }
            }
            _ => {}
        }
    }

    /// 未確定関数を消費して結果を取得
    /// 戻り値: (確定関数, 未確定のまま残った関数)
    pub fn into_results(self) -> (HashMap<String, FunctionSignature>, Vec<PendingFunction>) {
        (self.confirmed, self.pending)
    }

    /// イテレータで確定済み関数を列挙
    pub fn confirmed_iter(&self) -> impl Iterator<Item = (&String, &FunctionSignature)> {
        self.confirmed.iter()
    }

    /// イテレータで未確定関数を列挙
    pub fn pending_iter(&self) -> impl Iterator<Item = &PendingFunction> {
        self.pending.iter()
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

/// 式から呼び出される関数名を抽出
pub fn extract_called_functions(expr: &Expr, interner: &StringInterner) -> Vec<String> {
    let mut result = Vec::new();
    extract_called_functions_inner(expr, interner, &mut result);
    result
}

fn extract_called_functions_inner(expr: &Expr, interner: &StringInterner, result: &mut Vec<String>) {
    match expr {
        Expr::Call { func, args, .. } => {
            // 関数名を取得
            if let Expr::Ident(func_name, _) = func.as_ref() {
                result.push(interner.get(*func_name).to_string());
            }

            // 引数内の式も再帰的に走査
            for arg in args {
                extract_called_functions_inner(arg, interner, result);
            }

            // 関数式自体も走査
            extract_called_functions_inner(func, interner, result);
        }

        // 他の式タイプを再帰的に走査
        Expr::Binary { lhs, rhs, .. } => {
            extract_called_functions_inner(lhs, interner, result);
            extract_called_functions_inner(rhs, interner, result);
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
            extract_called_functions_inner(inner, interner, result);
        }
        Expr::Member { expr, .. } | Expr::PtrMember { expr, .. } => {
            extract_called_functions_inner(expr, interner, result);
        }
        Expr::Index { expr, index, .. } => {
            extract_called_functions_inner(expr, interner, result);
            extract_called_functions_inner(index, interner, result);
        }
        Expr::Cast { expr, .. } => {
            extract_called_functions_inner(expr, interner, result);
        }
        Expr::Sizeof(inner, _) => {
            extract_called_functions_inner(inner, interner, result);
        }
        Expr::Conditional { cond, then_expr, else_expr, .. } => {
            extract_called_functions_inner(cond, interner, result);
            extract_called_functions_inner(then_expr, interner, result);
            extract_called_functions_inner(else_expr, interner, result);
        }
        Expr::Comma { lhs, rhs, .. } => {
            extract_called_functions_inner(lhs, interner, result);
            extract_called_functions_inner(rhs, interner, result);
        }
        Expr::Assign { lhs, rhs, .. } => {
            extract_called_functions_inner(lhs, interner, result);
            extract_called_functions_inner(rhs, interner, result);
        }
        Expr::StmtExpr(compound, _) => {
            extract_called_functions_from_compound(compound, interner, result);
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

/// 複合文から呼び出される関数名を抽出
pub fn extract_called_functions_from_compound(
    compound: &CompoundStmt,
    interner: &StringInterner,
    result: &mut Vec<String>,
) {
    use crate::ast::BlockItem;

    for item in &compound.items {
        match item {
            BlockItem::Decl(decl) => {
                for init_decl in &decl.declarators {
                    if let Some(ref init) = init_decl.init {
                        extract_called_functions_from_initializer(init, interner, result);
                    }
                }
            }
            BlockItem::Stmt(stmt) => {
                extract_called_functions_from_stmt(stmt, interner, result);
            }
        }
    }
}

fn extract_called_functions_from_initializer(
    init: &crate::ast::Initializer,
    interner: &StringInterner,
    result: &mut Vec<String>,
) {
    match init {
        crate::ast::Initializer::Expr(expr) => {
            extract_called_functions_inner(expr, interner, result);
        }
        crate::ast::Initializer::List(items) => {
            for item in items {
                extract_called_functions_from_initializer(&item.init, interner, result);
            }
        }
    }
}

fn extract_called_functions_from_stmt(
    stmt: &crate::ast::Stmt,
    interner: &StringInterner,
    result: &mut Vec<String>,
) {
    use crate::ast::{ForInit, Stmt};

    match stmt {
        Stmt::Compound(compound) => {
            extract_called_functions_from_compound(compound, interner, result);
        }
        Stmt::Expr(Some(expr), _) => {
            extract_called_functions_inner(expr, interner, result);
        }
        Stmt::Expr(None, _) => {}
        Stmt::If { cond, then_stmt, else_stmt, .. } => {
            extract_called_functions_inner(cond, interner, result);
            extract_called_functions_from_stmt(then_stmt, interner, result);
            if let Some(else_s) = else_stmt {
                extract_called_functions_from_stmt(else_s, interner, result);
            }
        }
        Stmt::Switch { expr, body, .. } => {
            extract_called_functions_inner(expr, interner, result);
            extract_called_functions_from_stmt(body, interner, result);
        }
        Stmt::While { cond, body, .. } => {
            extract_called_functions_inner(cond, interner, result);
            extract_called_functions_from_stmt(body, interner, result);
        }
        Stmt::DoWhile { body, cond, .. } => {
            extract_called_functions_from_stmt(body, interner, result);
            extract_called_functions_inner(cond, interner, result);
        }
        Stmt::For { init, cond, step, body, .. } => {
            if let Some(init) = init {
                match init {
                    ForInit::Expr(expr) => {
                        extract_called_functions_inner(expr, interner, result);
                    }
                    ForInit::Decl(decl) => {
                        for init_decl in &decl.declarators {
                            if let Some(ref init) = init_decl.init {
                                extract_called_functions_from_initializer(init, interner, result);
                            }
                        }
                    }
                }
            }
            if let Some(cond) = cond {
                extract_called_functions_inner(cond, interner, result);
            }
            if let Some(step) = step {
                extract_called_functions_inner(step, interner, result);
            }
            extract_called_functions_from_stmt(body, interner, result);
        }
        Stmt::Return(Some(expr), _) => {
            extract_called_functions_inner(expr, interner, result);
        }
        Stmt::Return(None, _) | Stmt::Goto(_, _) | Stmt::Continue(_) | Stmt::Break(_) | Stmt::Asm { .. } => {}
        Stmt::Label { stmt, .. } => {
            extract_called_functions_from_stmt(stmt, interner, result);
        }
        Stmt::Case { expr, stmt, .. } => {
            extract_called_functions_inner(expr, interner, result);
            extract_called_functions_from_stmt(stmt, interner, result);
        }
        Stmt::Default { stmt, .. } => {
            extract_called_functions_from_stmt(stmt, interner, result);
        }
    }
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
