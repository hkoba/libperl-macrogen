//! 反復型推論
//!
//! マクロ関数とinline関数の引数型を反復的に推論する。
//! - bindings.rs の関数から始めて、
//! - マクロ関数とinline関数を呼び出し先の型情報を使って推論し、
//! - 推論が確定した関数を次の推論に利用する。

use std::collections::{HashMap, HashSet};

use crate::apidoc::{ApidocDict, ApidocEntry};
use crate::ast::{CompoundStmt, Expr};
use crate::fields_dict::FieldsDict;
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

    /// ApidocEntryから変換
    pub fn from_apidoc_entry(entry: &ApidocEntry) -> Self {
        Self {
            name: entry.name.clone(),
            params: entry.args.iter()
                .map(|arg| {
                    let rust_ty = c_type_to_rust(&arg.ty);
                    (arg.name.clone(), rust_ty)
                })
                .collect(),
            ret_ty: entry.return_type.as_ref().map(|t| c_type_to_rust(t)),
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

/// C型をRust型に変換
///
/// 例:
/// - "SV *" -> "*mut SV"
/// - "const char *" -> "*const c_char"
/// - "int" -> "c_int"
/// - "void" -> "()"
pub fn c_type_to_rust(c_type: &str) -> String {
    let trimmed = c_type.trim();

    // 空の場合
    if trimmed.is_empty() {
        return String::new();
    }

    // void
    if trimmed == "void" {
        return "()".to_string();
    }

    // 可変長引数
    if trimmed == "..." {
        return "...".to_string();
    }

    // ポインタ型の処理
    // "const char *" -> "*const c_char"
    // "SV *" -> "*mut SV"
    // "const SV *" -> "*const SV"
    // "char * const" -> "*mut c_char" (const after * is ignored for Rust)
    if let Some(ptr_type) = parse_pointer_type(trimmed) {
        return ptr_type;
    }

    // 基本型の変換
    convert_basic_type(trimmed)
}

/// ポインタ型をパース
fn parse_pointer_type(s: &str) -> Option<String> {
    // 末尾の "* const" や "*" を探す
    let s = s.trim();

    // パターン: "type * const" or "type *"
    // 末尾から * を探す
    if let Some(star_pos) = s.rfind('*') {
        let before_star = s[..star_pos].trim();
        let after_star = s[star_pos + 1..].trim();

        // after_star が "const" の場合は無視（ポインタ自体のconst）
        let _ptr_const = after_star == "const";

        // before_star から型を解析
        // "const char" -> is_const=true, base="char"
        // "SV" -> is_const=false, base="SV"
        let (is_const, base_type) = if before_star.starts_with("const ") {
            (true, before_star[6..].trim())
        } else if before_star.ends_with(" const") {
            (true, before_star[..before_star.len() - 6].trim())
        } else {
            (false, before_star)
        };

        // 再帰的にポインタをチェック（ダブルポインタなど）
        let inner_type = if base_type.contains('*') {
            // ネストしたポインタ
            parse_pointer_type(base_type).unwrap_or_else(|| convert_basic_type(base_type))
        } else {
            convert_basic_type(base_type)
        };

        let ptr_prefix = if is_const { "*const " } else { "*mut " };
        return Some(format!("{}{}", ptr_prefix, inner_type));
    }

    None
}

/// 基本型を変換
fn convert_basic_type(s: &str) -> String {
    let s = s.trim();

    // unsigned/signed の処理
    let (is_unsigned, base) = if s.starts_with("unsigned ") {
        (true, s[9..].trim())
    } else if s.starts_with("signed ") {
        (false, s[7..].trim())
    } else {
        (false, s)
    };

    // const を除去
    let base = base.trim_start_matches("const ").trim();

    // 基本型の変換
    let converted = match base {
        "char" if is_unsigned => "c_uchar",
        "char" => "c_char",
        "short" | "short int" if is_unsigned => "c_ushort",
        "short" | "short int" => "c_short",
        "int" if is_unsigned => "c_uint",
        "int" => "c_int",
        "long" | "long int" if is_unsigned => "c_ulong",
        "long" | "long int" => "c_long",
        "long long" | "long long int" if is_unsigned => "c_ulonglong",
        "long long" | "long long int" => "c_longlong",
        "float" => "c_float",
        "double" => "c_double",
        "size_t" => "usize",
        "ssize_t" => "isize",
        "bool" | "_Bool" => "bool",

        // Perl固有の型はそのまま
        // SV, AV, HV, CV, GV, IO, STRLEN, I32, U32, IV, UV, NV, etc.
        _ => {
            // そのまま返す（Perl内部型など）
            return base.to_string();
        }
    };

    converted.to_string()
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
    /// フィールド辞書（動的型推論用）
    fields_dict: &'a FieldsDict,
}

impl<'a> InferenceContext<'a> {
    /// 新しいコンテキストを作成
    pub fn new(interner: &'a StringInterner, fields_dict: &'a FieldsDict) -> Self {
        Self {
            confirmed: HashMap::new(),
            pending: Vec::new(),
            interner,
            fields_dict,
        }
    }

    /// bindings.rsから確定済み関数を読み込む
    pub fn load_bindings(&mut self, rust_decls: &RustDeclDict) {
        for (name, rust_fn) in &rust_decls.fns {
            self.confirmed.insert(name.clone(), FunctionSignature::from_rust_fn(rust_fn));
        }
    }

    /// apidocから確定済み関数を読み込む
    /// 既にbindings.rsで読み込まれている関数は上書きしない
    pub fn load_apidoc(&mut self, apidoc: &ApidocDict) -> usize {
        let mut added = 0;
        for (name, entry) in apidoc.iter() {
            // 既に確定済みの関数は上書きしない
            if self.confirmed.contains_key(name) {
                continue;
            }
            // 引数がない場合はスキップ（型推論に使えない）
            // ただし引数なしマクロでも戻り値型は有用なので含める
            let sig = FunctionSignature::from_apidoc_entry(entry);
            self.confirmed.insert(name.clone(), sig);
            added += 1;
        }
        added
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

            // 戻り値型を推論（まだ不明の場合）
            if pending.ret_ty.is_none() {
                pending.ret_ty = self.infer_return_type_from_expr(expr);
            }
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
            Expr::Member { expr: inner, member, .. } => {
                // sv_u.svu_* パターンの検出
                // (ptr)->sv_u.svu_pv のような形式から ptr の型を推論
                if let Some((pointer_type, _field_type)) = self.infer_from_sv_u_field(*member, inner) {
                    if let Some(param_id) = self.find_base_param(inner, params) {
                        if !known_types.contains_key(&param_id) {
                            known_types.insert(param_id, pointer_type);
                        }
                    }
                }
                self.infer_from_expr(inner, params, known_types);
            }
            Expr::PtrMember { expr, .. } => {
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

    /// sv_u.svu_* パターンから型を推論
    ///
    /// 例: (sv)->sv_u.svu_pv の svu_pv から SV 型を推論
    /// 戻り値: (ポインタ型, フィールド型)
    /// - svu_pv → (*mut SV, *mut c_char)
    /// - svu_iv → (*mut SV, IV)
    /// - svu_uv → (*mut SV, UV)
    /// - svu_rv → (*mut SV, *mut SV)
    /// - svu_array → (*mut AV, *mut *mut SV)
    /// - svu_hash → (*mut HV, *mut *mut HE)
    /// - svu_gp → (*mut GV, *mut GP)
    /// - svu_fp → (*, *mut PerlIO)
    fn infer_from_sv_u_field(&self, field: InternedStr, inner: &Expr) -> Option<(String, String)> {
        let field_name = self.interner.get(field);

        // svu_* フィールドでなければスキップ
        if !field_name.starts_with("svu_") {
            return None;
        }

        // 内部式が .sv_u へのアクセスかチェック
        let sv_u_base = match inner {
            Expr::Member { expr, member, .. } => {
                if self.interner.get(*member) == "sv_u" {
                    Some(expr.as_ref())
                } else {
                    None
                }
            }
            Expr::PtrMember { expr, member, .. } => {
                if self.interner.get(*member) == "sv_u" {
                    Some(expr.as_ref())
                } else {
                    None
                }
            }
            _ => None,
        };

        // sv_u へのアクセスでなければスキップ
        sv_u_base?;

        // フィールド名からポインタ型とフィールド型を決定
        match field_name {
            "svu_pv" => Some(("*mut SV".to_string(), "*mut c_char".to_string())),
            "svu_iv" => Some(("*mut SV".to_string(), "IV".to_string())),
            "svu_uv" => Some(("*mut SV".to_string(), "UV".to_string())),
            "svu_rv" => Some(("*mut SV".to_string(), "*mut SV".to_string())),
            "svu_array" => Some(("*mut AV".to_string(), "*mut *mut SV".to_string())),
            "svu_hash" => Some(("*mut HV".to_string(), "*mut *mut HE".to_string())),
            "svu_gp" => Some(("*mut GV".to_string(), "*mut GP".to_string())),
            "svu_fp" => Some(("*mut SV".to_string(), "*mut PerlIO".to_string())),
            _ => None,
        }
    }

    /// 式からベースとなるパラメータを探す
    fn find_base_param(&self, expr: &Expr, params: &HashSet<InternedStr>) -> Option<InternedStr> {
        match expr {
            Expr::Ident(id, _) => {
                if params.contains(id) {
                    Some(*id)
                } else {
                    None
                }
            }
            // sv_u へのアクセスの場合、さらに内側を探す
            Expr::Member { expr: inner, member, .. } => {
                if self.interner.get(*member) == "sv_u" {
                    self.find_base_param(inner, params)
                } else {
                    None
                }
            }
            Expr::PtrMember { expr: inner, member, .. } => {
                if self.interner.get(*member) == "sv_u" {
                    self.find_base_param(inner, params)
                } else {
                    None
                }
            }
            // 括弧で囲まれた式
            Expr::Deref(inner, _) => self.find_base_param(inner, params),
            _ => None,
        }
    }

    /// 式から戻り値型を推論
    fn infer_return_type_from_expr(&self, expr: &Expr) -> Option<String> {
        match expr {
            // フィールドアクセスの場合、フィールド型を返す (. または ->)
            Expr::Member { expr: inner, member, .. }
            | Expr::PtrMember { expr: inner, member, .. } => {
                // sv_u.svu_* パターン
                if let Some((_pointer_type, field_type)) = self.infer_from_sv_u_field(*member, inner) {
                    return Some(field_type);
                }
                // フィールド辞書から動的に型を取得
                if let Some(field_type) = self.lookup_field_type(*member) {
                    return Some(field_type);
                }
                None
            }
            // 括弧やデリファレンスを透過
            Expr::Deref(inner, _) => self.infer_return_type_from_expr(inner),
            // 条件式の場合、then/else両方から推論を試みる
            Expr::Conditional { then_expr, else_expr, .. } => {
                self.infer_return_type_from_expr(then_expr)
                    .or_else(|| self.infer_return_type_from_expr(else_expr))
            }
            // 二項演算の場合、どちらかのオペランドから推論を試みる
            // 例: 0 + (gv)->sv_u.svu_gp では右側から型を推論
            Expr::Binary { lhs, rhs, .. } => {
                // 一方がリテラル0の場合、もう一方から推論
                let lhs_is_zero = matches!(lhs.as_ref(), Expr::IntLit(0, _));
                let rhs_is_zero = matches!(rhs.as_ref(), Expr::IntLit(0, _));

                if lhs_is_zero {
                    self.infer_return_type_from_expr(rhs)
                } else if rhs_is_zero {
                    self.infer_return_type_from_expr(lhs)
                } else {
                    // 両方から試みる
                    self.infer_return_type_from_expr(lhs)
                        .or_else(|| self.infer_return_type_from_expr(rhs))
                }
            }
            // 関数呼び出しの場合、確定済み関数の戻り値型を使用
            Expr::Call { func, .. } => {
                if let Expr::Ident(func_name, _) = func.as_ref() {
                    let func_name_str = self.interner.get(*func_name);
                    if let Some(sig) = self.confirmed.get(func_name_str) {
                        return sig.ret_ty.clone();
                    }
                }
                None
            }
            // キャストの場合は型があるが、ここでは扱わない
            // （型情報の解析が必要になるため）
            _ => None,
        }
    }

    /// フィールドの型を動的に検索
    ///
    /// FieldsDictから収集された構造体フィールドの型情報を使用
    fn lookup_field_type(&self, field: InternedStr) -> Option<String> {
        // フィールド辞書から動的に型を取得
        if let Some(field_type) = self.fields_dict.get_unique_field_type(field) {
            return Some(field_type.rust_type.clone());
        }

        // 一意に特定できない場合でも、構造体名が分かれば型を取得
        if let Some(struct_name) = self.fields_dict.lookup_unique(field) {
            if let Some(field_type) = self.fields_dict.get_field_type(struct_name, field) {
                return Some(field_type.rust_type.clone());
            }
        }

        None
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

    #[test]
    fn test_c_type_to_rust_basic() {
        assert_eq!(c_type_to_rust("int"), "c_int");
        assert_eq!(c_type_to_rust("unsigned int"), "c_uint");
        assert_eq!(c_type_to_rust("char"), "c_char");
        assert_eq!(c_type_to_rust("unsigned char"), "c_uchar");
        assert_eq!(c_type_to_rust("long"), "c_long");
        assert_eq!(c_type_to_rust("void"), "()");
        assert_eq!(c_type_to_rust("size_t"), "usize");
        assert_eq!(c_type_to_rust("bool"), "bool");
    }

    #[test]
    fn test_c_type_to_rust_pointer() {
        assert_eq!(c_type_to_rust("SV *"), "*mut SV");
        assert_eq!(c_type_to_rust("const SV *"), "*const SV");
        assert_eq!(c_type_to_rust("char *"), "*mut c_char");
        assert_eq!(c_type_to_rust("const char *"), "*const c_char");
        assert_eq!(c_type_to_rust("SV * const"), "*mut SV");
        // 二重 const: "const char * const" は "*const c_char" と同じ
        assert_eq!(c_type_to_rust("const char * const"), "*const c_char");
    }

    #[test]
    fn test_c_type_to_rust_double_pointer() {
        assert_eq!(c_type_to_rust("SV **"), "*mut *mut SV");
        assert_eq!(c_type_to_rust("char **"), "*mut *mut c_char");
    }

    #[test]
    fn test_c_type_to_rust_perl_types() {
        // Perl固有の型はそのまま
        assert_eq!(c_type_to_rust("STRLEN"), "STRLEN");
        assert_eq!(c_type_to_rust("I32"), "I32");
        assert_eq!(c_type_to_rust("U32"), "U32");
        assert_eq!(c_type_to_rust("IV"), "IV");
        assert_eq!(c_type_to_rust("UV"), "UV");
        assert_eq!(c_type_to_rust("NV"), "NV");
        assert_eq!(c_type_to_rust("AV *"), "*mut AV");
        assert_eq!(c_type_to_rust("HV *"), "*mut HV");
    }
}
