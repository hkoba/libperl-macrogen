//! Rust コード生成モジュール
//!
//! C言語のマクロ関数をRust関数に変換する。

use std::collections::HashSet;

use crate::ast::{
    AssignOp, BinOp, BlockItem, CompoundStmt, Declaration, DeclSpecs, DerivedDecl,
    Expr, ExprKind, ForInit, FunctionDef, ParamDecl, Stmt, TypeName, TypeSpec,
};
use crate::fields_dict::FieldsDict;
use crate::intern::{InternedStr, StringInterner};
use crate::macro_analyzer2::MacroInfo2;
use crate::macro_def::{MacroDef, MacroKind};
use crate::source::FileRegistry;

// ==================== CodeGenOptions ====================

/// コード生成オプション
#[derive(Debug, Clone, Default)]
pub struct CodeGenOptions {
    /// マクロ定義位置をコメントとして出力するか
    pub emit_macro_comments: bool,
}

// ==================== CodeFragment (Synthesized Attribute) ====================

/// コード生成結果（Synthesized Attribute を含む）
#[derive(Debug, Clone)]
pub struct CodeFragment {
    /// 生成されたコード
    pub code: String,
    /// 生成中に発生した問題
    pub issues: Vec<CodeIssue>,
    /// 使用された定数マクロ
    pub used_constants: HashSet<InternedStr>,
    /// my_perl引数が必要かどうか（THX依存）
    pub needs_my_perl: bool,
}

/// コード生成で発生した問題
#[derive(Debug, Clone)]
pub struct CodeIssue {
    /// 問題の種類
    pub kind: CodeIssueKind,
    /// 問題の説明
    pub description: String,
}

/// 問題の種類
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeIssueKind {
    /// 未サポートの構文（statement expression, compound literal 等）
    UnsupportedConstruct,
    /// インクリメント/デクリメント演算子
    IncrementDecrement,
    /// Goto/Label
    ControlFlow,
    /// インラインアセンブリ
    InlineAsm,
    /// 匿名型（anonymous struct/union/enum）
    AnonymousType,
    /// 不明な型
    UnknownType,
    /// 初期化子リスト
    InitializerList,
}

impl CodeFragment {
    /// 成功（問題なし）
    pub fn ok(code: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            issues: vec![],
            used_constants: HashSet::new(),
            needs_my_perl: false,
        }
    }

    /// 問題あり
    pub fn with_issue(code: impl Into<String>, issue: CodeIssue) -> Self {
        Self {
            code: code.into(),
            issues: vec![issue],
            used_constants: HashSet::new(),
            needs_my_perl: false,
        }
    }

    /// 定数参照あり
    pub fn with_constant(code: impl Into<String>, constant: InternedStr) -> Self {
        let mut used_constants = HashSet::new();
        used_constants.insert(constant);
        Self {
            code: code.into(),
            issues: vec![],
            used_constants,
            needs_my_perl: false,
        }
    }

    /// 問題があるかどうか
    pub fn has_issues(&self) -> bool {
        !self.issues.is_empty()
    }

    /// 子の CodeFragment からの問題と定数参照、THX依存をマージ
    pub fn merge(&mut self, other: &CodeFragment) {
        self.issues.extend(other.issues.iter().cloned());
        self.used_constants.extend(other.used_constants.iter().cloned());
        self.needs_my_perl = self.needs_my_perl || other.needs_my_perl;
    }

    /// 子の CodeFragment からの問題をマージ（後方互換）
    pub fn merge_issues(&mut self, other: &CodeFragment) {
        self.merge(other);
    }

    /// 複数の CodeFragment を結合
    pub fn concat(fragments: impl IntoIterator<Item = CodeFragment>, sep: &str) -> Self {
        let mut code_parts = Vec::new();
        let mut all_issues = Vec::new();
        let mut all_constants = HashSet::new();
        let mut any_needs_my_perl = false;
        for frag in fragments {
            code_parts.push(frag.code);
            all_issues.extend(frag.issues);
            all_constants.extend(frag.used_constants);
            any_needs_my_perl = any_needs_my_perl || frag.needs_my_perl;
        }
        Self {
            code: code_parts.join(sep),
            issues: all_issues,
            used_constants: all_constants,
            needs_my_perl: any_needs_my_perl,
        }
    }

    /// 問題の説明を結合した文字列を返す
    pub fn issues_summary(&self) -> String {
        self.issues
            .iter()
            .map(|i| i.description.clone())
            .collect::<Vec<_>>()
            .join("; ")
    }
}

impl CodeIssue {
    pub fn new(kind: CodeIssueKind, description: impl Into<String>) -> Self {
        Self {
            kind,
            description: description.into(),
        }
    }
}

/// Rustコード生成器
pub struct RustCodeGen<'a> {
    /// 文字列インターナー
    interner: &'a StringInterner,
    /// フィールド辞書（型推論用）
    #[allow(dead_code)]
    fields_dict: &'a FieldsDict,
    /// ファイルレジストリ（パス解決用）
    files: Option<&'a FileRegistry>,
    /// コード生成オプション
    options: CodeGenOptions,
    /// 定数マクロの集合（展開されずに識別子として残ったもの）
    constant_macros: HashSet<InternedStr>,
    /// THX依存マクロの集合
    thx_macros: HashSet<InternedStr>,
    /// THX依存関数の集合（bindings.rsから）
    thx_functions: HashSet<String>,
}

impl<'a> RustCodeGen<'a> {
    /// 新しいコード生成器を作成
    pub fn new(interner: &'a StringInterner, fields_dict: &'a FieldsDict) -> Self {
        Self {
            interner,
            fields_dict,
            files: None,
            options: CodeGenOptions::default(),
            constant_macros: HashSet::new(),
            thx_macros: HashSet::new(),
            thx_functions: HashSet::new(),
        }
    }

    /// ファイルレジストリを設定
    pub fn set_files(&mut self, files: &'a FileRegistry) {
        self.files = Some(files);
    }

    /// コード生成オプションを設定
    pub fn set_options(&mut self, options: CodeGenOptions) {
        self.options = options;
    }

    /// 定数マクロ情報を設定
    pub fn set_constant_macros(&mut self, constants: HashSet<InternedStr>) {
        self.constant_macros = constants;
    }

    /// 定数マクロかどうかをチェック
    fn is_constant_macro(&self, name: InternedStr) -> bool {
        self.constant_macros.contains(&name)
    }

    /// THX依存マクロ情報を設定
    pub fn set_thx_macros(&mut self, macros: HashSet<InternedStr>) {
        self.thx_macros = macros;
    }

    /// THX依存関数情報を設定
    pub fn set_thx_functions(&mut self, functions: HashSet<String>) {
        self.thx_functions = functions;
    }

    /// 指定された関数/マクロがTHX依存かどうかをチェック
    fn is_thx_dependent(&self, name: InternedStr) -> bool {
        // THX依存マクロかチェック
        if self.thx_macros.contains(&name) {
            return true;
        }
        // THX依存関数かチェック（Perl_*も含む）
        let name_str = self.interner.get(name);
        if name_str.starts_with("Perl_") {
            return true;
        }
        self.thx_functions.contains(name_str)
    }

    /// 式をRustコードに変換（Synthesized Attribute 版）
    pub fn expr_to_rust(&self, expr: &Expr) -> CodeFragment {
        match &expr.kind {
            ExprKind::IntLit(n) => CodeFragment::ok(n.to_string()),

            ExprKind::UIntLit(n) => CodeFragment::ok(format!("{}u64", n)),

            ExprKind::FloatLit(f) => CodeFragment::ok(f.to_string()),

            ExprKind::CharLit(c) => CodeFragment::ok(format!("'{}' as c_char", self.escape_char(*c as char))),

            ExprKind::StringLit(s) => {
                let escaped = self.escape_bytes(s);
                CodeFragment::ok(format!("c\"{}\"", escaped))
            }

            ExprKind::Ident(id) => {
                let name = self.interner.get(*id).to_string();
                if self.is_constant_macro(*id) {
                    CodeFragment::with_constant(name, *id)
                } else {
                    CodeFragment::ok(name)
                }
            }

            ExprKind::Binary { op, lhs, rhs } => {
                let left = self.expr_to_rust(lhs);
                let right = self.expr_to_rust(rhs);
                let op_str = self.bin_op_to_rust(op);
                let mut result = CodeFragment::ok(format!("({} {} {})", left.code, op_str, right.code));
                result.merge_issues(&left);
                result.merge_issues(&right);
                result
            }

            // 単項演算子
            ExprKind::UnaryPlus(inner) => {
                let inner_frag = self.expr_to_rust(inner);
                let mut result = CodeFragment::ok(format!("(+{})", inner_frag.code));
                result.merge_issues(&inner_frag);
                result
            }
            ExprKind::UnaryMinus(inner) => {
                let inner_frag = self.expr_to_rust(inner);
                let mut result = CodeFragment::ok(format!("(-{})", inner_frag.code));
                result.merge_issues(&inner_frag);
                result
            }
            ExprKind::BitNot(inner) => {
                let inner_frag = self.expr_to_rust(inner);
                let mut result = CodeFragment::ok(format!("(!{})", inner_frag.code));
                result.merge_issues(&inner_frag);
                result
            }
            ExprKind::LogNot(inner) => {
                let inner_frag = self.expr_to_rust(inner);
                let mut result = CodeFragment::ok(format!("(!{})", inner_frag.code));
                result.merge_issues(&inner_frag);
                result
            }
            ExprKind::Deref(inner) => {
                // *ptr-- パターン: ポインタを post-decrement してから dereference
                if let ExprKind::PostDec(ptr_expr) = &inner.kind {
                    let ptr_frag = self.expr_to_rust(ptr_expr);
                    let mut result = CodeFragment::ok(format!(
                        "{{ let __ptr = {ptr}; {ptr} = {ptr}.sub(1); *__ptr }}",
                        ptr = ptr_frag.code
                    ));
                    result.merge_issues(&ptr_frag);
                    return result;
                }
                // *ptr++ パターン: ポインタを post-increment してから dereference
                if let ExprKind::PostInc(ptr_expr) = &inner.kind {
                    let ptr_frag = self.expr_to_rust(ptr_expr);
                    let mut result = CodeFragment::ok(format!(
                        "{{ let __ptr = {ptr}; {ptr} = {ptr}.add(1); *__ptr }}",
                        ptr = ptr_frag.code
                    ));
                    result.merge_issues(&ptr_frag);
                    return result;
                }
                let inner_frag = self.expr_to_rust(inner);
                let mut result = CodeFragment::ok(format!("(*{})", inner_frag.code));
                result.merge_issues(&inner_frag);
                result
            }
            ExprKind::AddrOf(inner) => {
                let inner_frag = self.expr_to_rust(inner);
                let mut result = CodeFragment::ok(format!("(&{})", inner_frag.code));
                result.merge_issues(&inner_frag);
                result
            }
            ExprKind::PreInc(inner) => {
                let inner_frag = self.expr_to_rust(inner);
                let mut result = CodeFragment::with_issue(
                    format!("/* ++{} */", inner_frag.code),
                    CodeIssue::new(CodeIssueKind::IncrementDecrement, format!("pre-increment: ++{}", inner_frag.code)),
                );
                result.merge_issues(&inner_frag);
                result
            }
            ExprKind::PreDec(inner) => {
                let inner_frag = self.expr_to_rust(inner);
                let mut result = CodeFragment::with_issue(
                    format!("/* --{} */", inner_frag.code),
                    CodeIssue::new(CodeIssueKind::IncrementDecrement, format!("pre-decrement: --{}", inner_frag.code)),
                );
                result.merge_issues(&inner_frag);
                result
            }
            ExprKind::PostInc(inner) => {
                // (*ptr)++ パターン: ポインタを dereference した値を post-increment
                // *ptr がポインタ型の場合は .add(1) を使用
                if let ExprKind::Deref(ptr_expr) = &inner.kind {
                    let ptr_frag = self.expr_to_rust(ptr_expr);
                    let mut result = CodeFragment::ok(format!(
                        "{{ let __ptr = *{ptr}; *{ptr} = (*{ptr}).add(1); __ptr }}",
                        ptr = ptr_frag.code
                    ));
                    result.merge_issues(&ptr_frag);
                    return result;
                }
                let inner_frag = self.expr_to_rust(inner);
                let mut result = CodeFragment::with_issue(
                    format!("/* {}++ */", inner_frag.code),
                    CodeIssue::new(CodeIssueKind::IncrementDecrement, format!("post-increment: {}++", inner_frag.code)),
                );
                result.merge_issues(&inner_frag);
                result
            }
            ExprKind::PostDec(inner) => {
                // (*ptr)-- パターン: ポインタを dereference した値を post-decrement
                // *ptr がポインタ型の場合は .sub(1) を使用
                if let ExprKind::Deref(ptr_expr) = &inner.kind {
                    let ptr_frag = self.expr_to_rust(ptr_expr);
                    let mut result = CodeFragment::ok(format!(
                        "{{ let __ptr = *{ptr}; *{ptr} = (*{ptr}).sub(1); __ptr }}",
                        ptr = ptr_frag.code
                    ));
                    result.merge_issues(&ptr_frag);
                    return result;
                }
                let inner_frag = self.expr_to_rust(inner);
                let mut result = CodeFragment::with_issue(
                    format!("/* {}-- */", inner_frag.code),
                    CodeIssue::new(CodeIssueKind::IncrementDecrement, format!("post-decrement: {}--", inner_frag.code)),
                );
                result.merge_issues(&inner_frag);
                result
            }

            // メンバアクセス
            ExprKind::Member { expr: inner_expr, member } => {
                let expr_frag = self.expr_to_rust(inner_expr);
                let member_str = self.interner.get(*member);
                let mut result = CodeFragment::ok(format!("{}.{}", expr_frag.code, member_str));
                result.merge_issues(&expr_frag);
                result
            }
            ExprKind::PtrMember { expr: inner_expr, member } => {
                let expr_frag = self.expr_to_rust(inner_expr);
                let member_str = self.interner.get(*member);
                // ptr->field => (*ptr).field
                let mut result = CodeFragment::ok(format!("(*{}).{}", expr_frag.code, member_str));
                result.merge_issues(&expr_frag);
                result
            }

            ExprKind::Index { expr: inner_expr, index } => {
                let expr_frag = self.expr_to_rust(inner_expr);
                let index_frag = self.expr_to_rust(index);
                let mut result = CodeFragment::ok(format!("{}[{} as usize]", expr_frag.code, index_frag.code));
                result.merge_issues(&expr_frag);
                result.merge_issues(&index_frag);
                result
            }

            ExprKind::Call { func, args } => {
                // __builtin_expect(x, c) を x に置き換え
                // GCCの分岐予測ヒントはRustでは実装できないため、第1引数をそのまま返す
                if let ExprKind::Ident(id) = &func.kind {
                    let name = self.interner.get(*id);
                    if name == "__builtin_expect" && args.len() == 2 {
                        return self.expr_to_rust(&args[0]);
                    }
                }

                let func_frag = self.expr_to_rust(func);
                let args_frags: Vec<CodeFragment> = args.iter()
                    .map(|a| self.expr_to_rust(a))
                    .collect();

                // 呼び出し先がTHX依存かチェック
                let callee_needs_my_perl = if let ExprKind::Ident(id) = &func.kind {
                    self.is_thx_dependent(*id)
                } else {
                    false
                };

                // 第一引数が既に my_perl でなければ追加
                let first_arg_is_my_perl = args_frags.first()
                    .map(|f| f.code == "my_perl")
                    .unwrap_or(false);

                let args_str = if callee_needs_my_perl && !first_arg_is_my_perl {
                    let mut strs: Vec<&str> = vec!["my_perl"];
                    strs.extend(args_frags.iter().map(|f| f.code.as_str()));
                    strs
                } else {
                    args_frags.iter().map(|f| f.code.as_str()).collect()
                };

                let mut result = CodeFragment::ok(format!("{}({})", func_frag.code, args_str.join(", ")));
                result.merge_issues(&func_frag);
                for arg_frag in &args_frags {
                    result.merge_issues(arg_frag);
                }
                // 呼び出し先がTHX依存なら自身もTHX依存
                if callee_needs_my_perl {
                    result.needs_my_perl = true;
                }
                result
            }

            ExprKind::Cast { type_name, expr: inner_expr } => {
                let expr_frag = self.expr_to_rust(inner_expr);
                let ty_frag = self.type_name_to_rust(type_name);
                let mut result = CodeFragment::ok(format!("({} as {})", expr_frag.code, ty_frag.code));
                result.merge_issues(&expr_frag);
                result.merge_issues(&ty_frag);
                result
            }

            ExprKind::Sizeof(inner) => {
                let inner_frag = self.expr_to_rust(inner);
                let mut result = CodeFragment::ok(format!("std::mem::size_of_val(&{})", inner_frag.code));
                result.merge_issues(&inner_frag);
                result
            }

            ExprKind::SizeofType(type_name) => {
                let ty_frag = self.type_name_to_rust(type_name);
                let mut result = CodeFragment::ok(format!("std::mem::size_of::<{}>()", ty_frag.code));
                result.merge_issues(&ty_frag);
                result
            }

            ExprKind::Alignof(type_name) => {
                let ty_frag = self.type_name_to_rust(type_name);
                let mut result = CodeFragment::ok(format!("std::mem::align_of::<{}>()", ty_frag.code));
                result.merge_issues(&ty_frag);
                result
            }

            ExprKind::Conditional { cond, then_expr, else_expr } => {
                let cond_frag = self.expr_to_rust(cond);
                let then_frag = self.expr_to_rust(then_expr);
                let else_frag = self.expr_to_rust(else_expr);
                let mut result = CodeFragment::ok(format!(
                    "(if {} != 0 {{ {} }} else {{ {} }})",
                    cond_frag.code, then_frag.code, else_frag.code
                ));
                result.merge_issues(&cond_frag);
                result.merge_issues(&then_frag);
                result.merge_issues(&else_frag);
                result
            }

            ExprKind::Comma { lhs, rhs } => {
                // Rustではカンマ演算子がないので、ブロック式にする
                let left_frag = self.expr_to_rust(lhs);
                let right_frag = self.expr_to_rust(rhs);
                let mut result = CodeFragment::ok(format!("{{ let _ = {}; {} }}", left_frag.code, right_frag.code));
                result.merge_issues(&left_frag);
                result.merge_issues(&right_frag);
                result
            }

            ExprKind::Assign { op, lhs, rhs } => {
                let left_frag = self.expr_to_rust(lhs);
                let right_frag = self.expr_to_rust(rhs);
                let op_str = self.assign_op_to_rust(op);
                let mut result = CodeFragment::ok(format!("{} {} {}", left_frag.code, op_str, right_frag.code));
                result.merge_issues(&left_frag);
                result.merge_issues(&right_frag);
                result
            }

            ExprKind::CompoundLit { .. } => {
                CodeFragment::with_issue(
                    "/* compound literal */",
                    CodeIssue::new(CodeIssueKind::UnsupportedConstruct, "compound literal"),
                )
            }

            ExprKind::StmtExpr(compound) => {
                // ({ T x = expr; x; }) パターンを認識して簡略化を試みる
                if let Some(simplified) = self.try_simplify_stmt_expr(compound) {
                    self.expr_to_rust(&simplified)
                } else {
                    CodeFragment::with_issue(
                        "/* statement expression */",
                        CodeIssue::new(CodeIssueKind::UnsupportedConstruct, "statement expression"),
                    )
                }
            }
        }
    }

    /// Statement expression の簡略化を試みる
    ///
    /// `({ T x = expr; x; })` パターンを認識し、`expr` を返す。
    /// MUTABLE_PTR などのマクロがこのパターンを使用している。
    fn try_simplify_stmt_expr(&self, compound: &CompoundStmt) -> Option<Expr> {
        // パターン: 2つの項目（宣言 + 式文）
        if compound.items.len() != 2 {
            return None;
        }

        // 最初の項目は宣言であること
        let decl = match &compound.items[0] {
            BlockItem::Decl(d) => d,
            _ => return None,
        };

        // 宣言が1つの宣言子を持ち、初期化式があること
        if decl.declarators.len() != 1 {
            return None;
        }
        let init_decl = &decl.declarators[0];
        let decl_name = init_decl.declarator.name?;

        // 初期化式を取得
        let init_expr = match &init_decl.init {
            Some(crate::ast::Initializer::Expr(e)) => (**e).clone(),
            _ => return None,
        };

        // 2番目の項目は式文であること
        let final_stmt = match &compound.items[1] {
            BlockItem::Stmt(Stmt::Expr(Some(e), _)) => e,
            _ => return None,
        };

        // 式文が宣言した変数への参照であること
        match &final_stmt.kind {
            ExprKind::Ident(id) if *id == decl_name => Some(init_expr),
            _ => None,
        }
    }

    /// 二項演算子をRustに変換
    fn bin_op_to_rust(&self, op: &BinOp) -> &'static str {
        match op {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Mod => "%",
            BinOp::BitAnd => "&",
            BinOp::BitOr => "|",
            BinOp::BitXor => "^",
            BinOp::Shl => "<<",
            BinOp::Shr => ">>",
            BinOp::Lt => "<",
            BinOp::Gt => ">",
            BinOp::Le => "<=",
            BinOp::Ge => ">=",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::LogAnd => "&&",
            BinOp::LogOr => "||",
        }
    }

    /// 代入演算子をRustに変換
    fn assign_op_to_rust(&self, op: &AssignOp) -> &'static str {
        match op {
            AssignOp::Assign => "=",
            AssignOp::MulAssign => "*=",
            AssignOp::DivAssign => "/=",
            AssignOp::ModAssign => "%=",
            AssignOp::AddAssign => "+=",
            AssignOp::SubAssign => "-=",
            AssignOp::ShlAssign => "<<=",
            AssignOp::ShrAssign => ">>=",
            AssignOp::AndAssign => "&=",
            AssignOp::XorAssign => "^=",
            AssignOp::OrAssign => "|=",
        }
    }

    /// TypeName をRust型に変換
    fn type_name_to_rust(&self, ty: &TypeName) -> CodeFragment {
        let mut ptr_prefix = String::new();

        // ポインタを考慮
        if let Some(ref decl) = ty.declarator {
            for derived in &decl.derived {
                if let crate::ast::DerivedDecl::Pointer(_) = derived {
                    if ty.specs.qualifiers.is_const {
                        ptr_prefix.push_str("*const ");
                    } else {
                        ptr_prefix.push_str("*mut ");
                    }
                }
            }
        }

        // 基本型
        let type_frag = self.type_spec_to_rust(&ty.specs);
        let mut result = CodeFragment::ok(format!("{}{}", ptr_prefix, type_frag.code));
        result.merge_issues(&type_frag);
        result
    }

    /// 型指定子をRust型に変換
    fn type_spec_to_rust(&self, specs: &crate::ast::DeclSpecs) -> CodeFragment {
        // unsigned があるかどうかをチェック
        let is_unsigned = specs.type_specs.iter().any(|s| matches!(s, TypeSpec::Unsigned));

        // 基本型を探す
        for spec in &specs.type_specs {
            match spec {
                TypeSpec::Void => return CodeFragment::ok("c_void"),
                TypeSpec::Char => {
                    return CodeFragment::ok(if is_unsigned { "c_uchar" } else { "c_char" });
                }
                TypeSpec::Short => {
                    return CodeFragment::ok(if is_unsigned { "c_ushort" } else { "c_short" });
                }
                TypeSpec::Int => {
                    return CodeFragment::ok(if is_unsigned { "c_uint" } else { "c_int" });
                }
                TypeSpec::Long => {
                    return CodeFragment::ok(if is_unsigned { "c_ulong" } else { "c_long" });
                }
                TypeSpec::Float => return CodeFragment::ok("c_float"),
                TypeSpec::Double => return CodeFragment::ok("c_double"),
                TypeSpec::Bool => return CodeFragment::ok("bool"),
                TypeSpec::Signed | TypeSpec::Unsigned => continue,
                // typedef名はそのまま出力
                TypeSpec::TypedefName(id) => {
                    return CodeFragment::ok(self.interner.get(*id).to_string());
                }
                // struct/union/enum
                TypeSpec::Struct(s) => {
                    if let Some(name) = s.name {
                        return CodeFragment::ok(self.interner.get(name).to_string());
                    }
                    return CodeFragment::with_issue(
                        "/* anonymous struct */",
                        CodeIssue::new(CodeIssueKind::AnonymousType, "anonymous struct"),
                    );
                }
                TypeSpec::Union(s) => {
                    if let Some(name) = s.name {
                        return CodeFragment::ok(self.interner.get(name).to_string());
                    }
                    return CodeFragment::with_issue(
                        "/* anonymous union */",
                        CodeIssue::new(CodeIssueKind::AnonymousType, "anonymous union"),
                    );
                }
                TypeSpec::Enum(e) => {
                    if let Some(name) = e.name {
                        return CodeFragment::ok(self.interner.get(name).to_string());
                    }
                    return CodeFragment::with_issue(
                        "/* anonymous enum */",
                        CodeIssue::new(CodeIssueKind::AnonymousType, "anonymous enum"),
                    );
                }
                _ => continue,
            }
        }

        // unsigned/signed だけの場合は int
        if is_unsigned {
            CodeFragment::ok("c_uint")
        } else if specs.type_specs.iter().any(|s| matches!(s, TypeSpec::Signed)) {
            CodeFragment::ok("c_int")
        } else {
            CodeFragment::with_issue(
                "/* unknown type */",
                CodeIssue::new(CodeIssueKind::UnknownType, "unknown type"),
            )
        }
    }

    /// マクロ定義位置をコメントとしてフォーマット
    ///
    /// 出力例: `// Defined at: CORE/sv.h:123`
    fn format_macro_comment(&self, def: &MacroDef) -> Option<String> {
        if !self.options.emit_macro_comments {
            return None;
        }

        let files = self.files?;
        let path = files.get_path(def.def_loc.file_id);

        // パスからファイル名部分を抽出（長すぎる場合は末尾のみ）
        let path_str = path.to_string_lossy();
        let display_path = if path_str.len() > 60 {
            // 長いパスは末尾のコンポーネントのみ表示
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path_str.into_owned())
        } else {
            path_str.into_owned()
        };

        Some(format!(
            "// Defined at: {}:{}\n",
            display_path, def.def_loc.line
        ))
    }

    /// マクロをRust関数に変換
    pub fn macro_to_rust_fn(&self, def: &MacroDef, info: &MacroInfo2, expr: &Expr) -> CodeFragment {
        // ジェネリックパラメータがあればジェネリック関数として生成
        if !info.generic_params.is_empty() {
            return self.macro_to_generic_rust_fn(def, info, expr);
        }

        let name = self.interner.get(def.name);

        // パラメータを構築
        let params_frag = self.format_params(def, info);

        // 戻り値型
        let ret_ty = info.return_type.as_deref().unwrap_or("()");

        // 本体
        let body_frag = self.expr_to_rust(expr);

        // マクロコメント（オプション）
        let comment = self.format_macro_comment(def).unwrap_or_default();

        let code = format!(
            "{}#[inline]\npub unsafe fn {}({}) -> {} {{\n    {}\n}}\n",
            comment, name, params_frag.code, ret_ty, body_frag.code
        );

        let mut result = CodeFragment::ok(code);
        result.merge_issues(&params_frag);
        result.merge_issues(&body_frag);
        result
    }

    /// ジェネリックなマクロをRust関数に変換
    ///
    /// SV ファミリーの polymorphic フィールドアクセスをジェネリック関数として生成:
    /// ```text
    /// #[inline]
    /// pub unsafe fn SvANY<T, R>(sv: *mut T) -> *mut R {
    ///     (*(sv as *const sv)).sv_any as *mut R
    /// }
    /// ```
    fn macro_to_generic_rust_fn(&self, def: &MacroDef, info: &MacroInfo2, expr: &Expr) -> CodeFragment {
        let name = self.interner.get(def.name);

        // ジェネリック型パラメータ部分を生成 (e.g., "<T, R>" or "<T>")
        let type_params = self.format_generic_type_params(info);

        // パラメータを構築（ジェネリック版）
        let params_frag = self.format_generic_params(def, info);

        // 戻り値型（ジェネリック戻り値があれば使用）
        let ret_ty = if let Some(ref generic_ret) = info.generic_return {
            if generic_ret.is_pointer {
                format!("*mut {}", generic_ret.type_param)
            } else {
                generic_ret.type_param.clone()
            }
        } else {
            info.return_type.as_deref().unwrap_or("()").to_string()
        };

        // 本体（ジェネリックキャスト付き）
        let body_frag = self.expr_to_rust_with_generic_cast(expr, info);

        // マクロコメント（オプション）
        let comment = self.format_macro_comment(def).unwrap_or_default();

        let code = format!(
            "{}#[inline]\npub unsafe fn {}{type_params}({}) -> {} {{\n    {}\n}}\n",
            comment, name, params_frag.code, ret_ty, body_frag.code
        );

        let mut result = CodeFragment::ok(code);
        result.merge_issues(&params_frag);
        result.merge_issues(&body_frag);
        result
    }

    /// ジェネリック型パラメータ部分を生成 (e.g., "<T, R>" or "<T>")
    fn format_generic_type_params(&self, info: &MacroInfo2) -> String {
        let mut params: Vec<&str> = Vec::new();

        // 入力パラメータの型パラメータを収集
        let mut type_param_names: Vec<&str> = info.generic_params.values()
            .map(|gp| gp.type_param.as_str())
            .collect();
        type_param_names.sort();
        type_param_names.dedup();
        params.extend(type_param_names);

        // 戻り値の型パラメータ
        if let Some(ref generic_ret) = info.generic_return {
            params.push(&generic_ret.type_param);
        }

        if params.is_empty() {
            String::new()
        } else {
            format!("<{}>", params.join(", "))
        }
    }

    /// ジェネリックパラメータをフォーマット
    fn format_generic_params(&self, def: &MacroDef, info: &MacroInfo2) -> CodeFragment {
        if let MacroKind::Function { ref params, .. } = def.kind {
            let mut all_issues = Vec::new();
            let mut all_params: Vec<String> = Vec::new();

            // THX依存なら先頭に my_perl を追加
            if info.needs_my_perl {
                all_params.push("my_perl: *mut PerlInterpreter".to_string());
            }

            // マクロのパラメータを追加
            for p in params {
                let name = self.interner.get(*p);

                // ジェネリックパラメータかどうかチェック
                if let Some(generic_info) = info.generic_params.get(p) {
                    // ジェネリック: *mut T 形式
                    all_params.push(format!("{}: *mut {}", name, generic_info.type_param));
                } else {
                    // 通常パラメータ: 推論された型を使用
                    let ty = info.param_types.get(p)
                        .map(|s| s.as_str())
                        .unwrap_or_else(|| {
                            all_issues.push(CodeIssue::new(
                                CodeIssueKind::UnknownType,
                                format!("unknown type for parameter '{}'", name),
                            ));
                            "/* unknown */"
                        });
                    all_params.push(format!("{}: {}", name, ty));
                }
            }

            CodeFragment {
                code: all_params.join(", "),
                issues: all_issues,
                used_constants: HashSet::new(),
                needs_my_perl: false,
            }
        } else {
            CodeFragment::ok(String::new())
        }
    }

    /// ジェネリックキャスト付きで式を Rust に変換
    ///
    /// param->field を (*(param as *const base_type)).field に変換し、
    /// sv_any などのジェネリック戻り値フィールドには `as *mut R` を付加
    fn expr_to_rust_with_generic_cast(&self, expr: &Expr, info: &MacroInfo2) -> CodeFragment {
        match &expr.kind {
            // ptr->field パターン: ジェネリックキャストが必要な場合
            ExprKind::PtrMember { expr: base, member } => {
                let member_str = self.interner.get(*member);

                // ベースがジェネリックパラメータかどうかチェック
                if let ExprKind::Ident(id) = &base.kind {
                    if let Some(generic_info) = info.generic_params.get(id) {
                        // ジェネリックパラメータなので基底型にキャスト
                        let param_name = self.interner.get(*id);
                        let base_cast = format!(
                            "(*({} as *const {}))",
                            param_name,
                            generic_info.base_type
                        );

                        // sv_any はジェネリック戻り値が必要
                        let field_access = if info.generic_return.is_some() && member_str == "sv_any" {
                            format!("{}.{} as *mut R", base_cast, member_str)
                        } else {
                            format!("{}.{}", base_cast, member_str)
                        };

                        return CodeFragment::ok(field_access);
                    }
                }

                // 通常の変換にフォールバック
                let expr_frag = self.expr_to_rust_with_generic_cast(base, info);
                let mut result = CodeFragment::ok(format!("(*{}).{}", expr_frag.code, member_str));
                result.merge_issues(&expr_frag);
                result
            }

            // キャストを通した PtrMember
            ExprKind::Cast { expr: inner, type_name } => {
                if let ExprKind::PtrMember { .. } = &inner.kind {
                    // 内部のPtrMemberを先に処理
                    let inner_frag = self.expr_to_rust_with_generic_cast(inner, info);
                    let ty_frag = self.type_name_to_rust(type_name);
                    let mut result = CodeFragment::ok(format!("({} as {})", inner_frag.code, ty_frag.code));
                    result.merge_issues(&inner_frag);
                    result.merge_issues(&ty_frag);
                    return result;
                }
                // 通常のキャスト
                self.expr_to_rust(expr)
            }

            // 二項演算子
            ExprKind::Binary { op, lhs, rhs } => {
                let left = self.expr_to_rust_with_generic_cast(lhs, info);
                let right = self.expr_to_rust_with_generic_cast(rhs, info);
                let op_str = self.bin_op_to_rust(op);
                let mut result = CodeFragment::ok(format!("({} {} {})", left.code, op_str, right.code));
                result.merge_issues(&left);
                result.merge_issues(&right);
                result
            }

            // 単項演算子
            ExprKind::Deref(inner) => {
                let inner_frag = self.expr_to_rust_with_generic_cast(inner, info);
                let mut result = CodeFragment::ok(format!("(*{})", inner_frag.code));
                result.merge_issues(&inner_frag);
                result
            }

            ExprKind::AddrOf(inner) => {
                let inner_frag = self.expr_to_rust_with_generic_cast(inner, info);
                let mut result = CodeFragment::ok(format!("(&{})", inner_frag.code));
                result.merge_issues(&inner_frag);
                result
            }

            // 関数呼び出し
            ExprKind::Call { func, args } => {
                let func_frag = self.expr_to_rust_with_generic_cast(func, info);
                let args_frags: Vec<CodeFragment> = args.iter()
                    .map(|a| self.expr_to_rust_with_generic_cast(a, info))
                    .collect();

                let args_str: Vec<&str> = args_frags.iter().map(|f| f.code.as_str()).collect();
                let mut result = CodeFragment::ok(format!("{}({})", func_frag.code, args_str.join(", ")));
                result.merge_issues(&func_frag);
                for arg_frag in &args_frags {
                    result.merge_issues(arg_frag);
                }
                result
            }

            // 条件演算子
            ExprKind::Conditional { cond, then_expr, else_expr } => {
                let cond_frag = self.expr_to_rust_with_generic_cast(cond, info);
                let then_frag = self.expr_to_rust_with_generic_cast(then_expr, info);
                let else_frag = self.expr_to_rust_with_generic_cast(else_expr, info);
                let mut result = CodeFragment::ok(format!(
                    "(if {} != 0 {{ {} }} else {{ {} }})",
                    cond_frag.code, then_frag.code, else_frag.code
                ));
                result.merge_issues(&cond_frag);
                result.merge_issues(&then_frag);
                result.merge_issues(&else_frag);
                result
            }

            // メンバアクセス (dot operator)
            ExprKind::Member { expr: base, member } => {
                let expr_frag = self.expr_to_rust_with_generic_cast(base, info);
                let member_str = self.interner.get(*member);
                let mut result = CodeFragment::ok(format!("{}.{}", expr_frag.code, member_str));
                result.merge_issues(&expr_frag);
                result
            }

            // 配列添字
            ExprKind::Index { expr: base, index } => {
                let expr_frag = self.expr_to_rust_with_generic_cast(base, info);
                let index_frag = self.expr_to_rust_with_generic_cast(index, info);
                let mut result = CodeFragment::ok(format!("{}[{} as usize]", expr_frag.code, index_frag.code));
                result.merge_issues(&expr_frag);
                result.merge_issues(&index_frag);
                result
            }

            // その他の式は通常変換
            _ => self.expr_to_rust(expr),
        }
    }

    /// パラメータをフォーマット
    fn format_params(&self, def: &MacroDef, info: &MacroInfo2) -> CodeFragment {
        if let MacroKind::Function { ref params, .. } = def.kind {
            let mut all_issues = Vec::new();
            let mut all_params: Vec<String> = Vec::new();

            // THX依存なら先頭に my_perl を追加
            if info.needs_my_perl {
                all_params.push("my_perl: *mut PerlInterpreter".to_string());
            }

            // マクロのパラメータを追加
            for p in params {
                let name = self.interner.get(*p);
                let ty = info.param_types.get(p)
                    .map(|s| s.as_str())
                    .unwrap_or_else(|| {
                        all_issues.push(CodeIssue::new(
                            CodeIssueKind::UnknownType,
                            format!("unknown type for parameter '{}'", name),
                        ));
                        "/* unknown */"
                    });
                all_params.push(format!("{}: {}", name, ty));
            }

            CodeFragment {
                code: all_params.join(", "),
                issues: all_issues,
                used_constants: HashSet::new(),
                needs_my_perl: false,
            }
        } else {
            CodeFragment::ok(String::new())
        }
    }

    // ==================== Inline Function Conversion ====================

    /// インライン関数定義位置をコメントとしてフォーマット
    ///
    /// 出力例: `// Defined at: CORE/util.c:123`
    fn format_function_comment(&self, func: &FunctionDef) -> Option<String> {
        if !self.options.emit_macro_comments {
            return None;
        }

        let files = self.files?;
        let path = files.get_path(func.info.loc.file_id);

        // パスからファイル名部分を抽出（長すぎる場合は末尾のみ）
        let path_str = path.to_string_lossy();
        let display_path = if path_str.len() > 60 {
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path_str.into_owned())
        } else {
            path_str.into_owned()
        };

        Some(format!(
            "// Defined at: {}:{}\n",
            display_path, func.info.loc.line
        ))
    }

    /// インライン関数をRust関数に変換
    pub fn inline_fn_to_rust(&self, func: &FunctionDef) -> CodeFragment {
        // 関数名を取得
        let name = match func.declarator.name {
            Some(id) => self.interner.get(id).to_string(),
            None => {
                return CodeFragment::with_issue(
                    "/* anonymous function */",
                    CodeIssue::new(CodeIssueKind::UnsupportedConstruct, "anonymous function"),
                );
            }
        };

        // パラメータリストを取得
        let params_frag = self.extract_fn_params(&func.declarator.derived);

        // 戻り値型を取得
        let ret_frag = self.extract_return_type(&func.specs, &func.declarator.derived);

        // 関数本体を変換
        let body_frag = self.compound_stmt_to_rust(&func.body, 1);

        // 定義位置コメント（オプション）
        let comment = self.format_function_comment(func).unwrap_or_default();

        let code = format!(
            "{}#[inline]\npub unsafe fn {}({}) -> {} {{\n{}}}\n",
            comment, name, params_frag.code, ret_frag.code, body_frag.code
        );

        let mut result = CodeFragment::ok(code);
        result.merge_issues(&params_frag);
        result.merge_issues(&ret_frag);
        result.merge_issues(&body_frag);
        result
    }

    /// 関数パラメータを抽出してフォーマット
    fn extract_fn_params(&self, derived: &[DerivedDecl]) -> CodeFragment {
        for d in derived {
            if let DerivedDecl::Function(param_list) = d {
                let param_frags: Vec<CodeFragment> = param_list.params.iter()
                    .map(|p| self.param_decl_to_rust_frag(p))
                    .collect();
                return CodeFragment::concat(param_frags, ", ");
            }
        }
        CodeFragment::ok(String::new())
    }

    /// パラメータ宣言をRust形式に変換（CodeFragment版）
    fn param_decl_to_rust_frag(&self, param: &ParamDecl) -> CodeFragment {
        let name = param.declarator.as_ref()
            .and_then(|d| d.name)
            .map(|id| self.interner.get(id).to_string())
            .unwrap_or_else(|| "_".to_string());

        let ty_frag = self.decl_to_rust_type_frag(&param.specs, param.declarator.as_ref());
        let mut result = CodeFragment::ok(format!("{}: {}", name, ty_frag.code));
        result.merge_issues(&ty_frag);
        result
    }

    /// パラメータ宣言から型のみを取得（公開API - 文字列版、後方互換性のため）
    pub fn param_decl_to_rust_type(&self, param: &ParamDecl) -> String {
        self.decl_to_rust_type_frag(&param.specs, param.declarator.as_ref()).code
    }

    /// 宣言からRust型を生成（CodeFragment版）
    fn decl_to_rust_type_frag(&self, specs: &DeclSpecs, declarator: Option<&crate::ast::Declarator>) -> CodeFragment {
        let mut ptr_prefix = String::new();

        // ポインタを処理
        if let Some(decl) = declarator {
            for derived in &decl.derived {
                if let DerivedDecl::Pointer(quals) = derived {
                    if quals.is_const {
                        ptr_prefix.push_str("*const ");
                    } else {
                        ptr_prefix.push_str("*mut ");
                    }
                }
            }
        }

        let base_frag = self.type_spec_to_rust(specs);
        let mut result = CodeFragment::ok(format!("{}{}", ptr_prefix, base_frag.code));
        result.merge_issues(&base_frag);
        result
    }

    /// 戻り値型を抽出
    fn extract_return_type(&self, specs: &DeclSpecs, derived: &[DerivedDecl]) -> CodeFragment {
        // void の場合
        if specs.type_specs.iter().any(|s| matches!(s, TypeSpec::Void)) {
            // ポインタがあるか確認
            let has_pointer = derived.iter().any(|d| {
                matches!(d, DerivedDecl::Pointer(_))
            });
            if !has_pointer {
                return CodeFragment::ok("()");
            }
        }

        self.type_spec_to_rust(specs)
    }

    /// 関数の戻り値型を抽出（公開API - 文字列版、後方互換性のため）
    /// 戻り値: Some(型文字列) または None（voidの場合）
    pub fn extract_fn_return_type(&self, specs: &DeclSpecs, derived: &[DerivedDecl]) -> Option<String> {
        let ty_frag = self.extract_return_type(specs, derived);
        if ty_frag.code == "()" {
            None
        } else {
            Some(ty_frag.code)
        }
    }

    /// 複合文をRustに変換
    fn compound_stmt_to_rust(&self, stmt: &CompoundStmt, indent: usize) -> CodeFragment {
        let mut code = String::new();
        let mut issues = Vec::new();
        let mut used_constants = HashSet::new();
        let mut needs_my_perl = false;

        for item in &stmt.items {
            let frag = match item {
                BlockItem::Decl(decl) => self.decl_to_rust(decl, indent),
                BlockItem::Stmt(s) => self.stmt_to_rust(s, indent),
            };
            code.push_str(&frag.code);
            issues.extend(frag.issues);
            used_constants.extend(frag.used_constants);
            needs_my_perl = needs_my_perl || frag.needs_my_perl;
        }

        CodeFragment { code, issues, used_constants, needs_my_perl }
    }

    /// 宣言をRustに変換
    fn decl_to_rust(&self, decl: &Declaration, indent: usize) -> CodeFragment {
        let indent_str = "    ".repeat(indent);
        let mut code = String::new();
        let mut issues = Vec::new();
        let mut used_constants = HashSet::new();
        let mut needs_my_perl = false;

        for init_decl in &decl.declarators {
            let name = init_decl.declarator.name
                .map(|id| self.interner.get(id).to_string())
                .unwrap_or_else(|| "_".to_string());

            let ty_frag = self.decl_to_rust_type_frag(&decl.specs, Some(&init_decl.declarator));
            issues.extend(ty_frag.issues);
            used_constants.extend(ty_frag.used_constants);
            needs_my_perl = needs_my_perl || ty_frag.needs_my_perl;

            if let Some(ref init) = init_decl.init {
                match init {
                    crate::ast::Initializer::Expr(expr) => {
                        let expr_frag = self.expr_to_rust(expr);
                        code.push_str(&format!("{}let mut {}: {} = {};\n", indent_str, name, ty_frag.code, expr_frag.code));
                        issues.extend(expr_frag.issues);
                        used_constants.extend(expr_frag.used_constants);
                        needs_my_perl = needs_my_perl || expr_frag.needs_my_perl;
                    }
                    crate::ast::Initializer::List(_) => {
                        code.push_str(&format!("{}let mut {}: {} = /* initializer list */;\n", indent_str, name, ty_frag.code));
                        issues.push(CodeIssue::new(CodeIssueKind::InitializerList, "initializer list"));
                    }
                }
            } else {
                code.push_str(&format!("{}let mut {}: {};\n", indent_str, name, ty_frag.code));
            }
        }

        CodeFragment { code, issues, used_constants, needs_my_perl }
    }

    /// 文をRustに変換
    fn stmt_to_rust(&self, stmt: &Stmt, indent: usize) -> CodeFragment {
        let indent_str = "    ".repeat(indent);

        match stmt {
            Stmt::Compound(compound) => {
                let inner = self.compound_stmt_to_rust(compound, indent + 1);
                let mut result = CodeFragment::ok(format!("{}{{\n{}{}}}\n", indent_str, inner.code, indent_str));
                result.merge_issues(&inner);
                result
            }

            Stmt::Expr(Some(expr), _) => {
                let expr_frag = self.expr_to_rust(expr);
                let mut result = CodeFragment::ok(format!("{}{};\n", indent_str, expr_frag.code));
                result.merge_issues(&expr_frag);
                result
            }

            Stmt::Expr(None, _) => {
                // 空文
                CodeFragment::ok(String::new())
            }

            Stmt::If { cond, then_stmt, else_stmt, .. } => {
                let cond_frag = self.expr_to_rust(cond);
                let then_frag = self.stmt_to_rust_block(then_stmt, indent);

                let code = if let Some(else_s) = else_stmt {
                    let else_frag = self.stmt_to_rust_block(else_s, indent);
                    let mut result = CodeFragment::ok(format!(
                        "{}if {} != 0 {} else {}\n",
                        indent_str, cond_frag.code, then_frag.code, else_frag.code
                    ));
                    result.merge_issues(&cond_frag);
                    result.merge_issues(&then_frag);
                    result.merge_issues(&else_frag);
                    return result;
                } else {
                    format!("{}if {} != 0 {}\n", indent_str, cond_frag.code, then_frag.code)
                };

                let mut result = CodeFragment::ok(code);
                result.merge_issues(&cond_frag);
                result.merge_issues(&then_frag);
                result
            }

            Stmt::While { cond, body, .. } => {
                let cond_frag = self.expr_to_rust(cond);
                let body_frag = self.stmt_to_rust_block(body, indent);
                let mut result = CodeFragment::ok(format!(
                    "{}while {} != 0 {}\n",
                    indent_str, cond_frag.code, body_frag.code
                ));
                result.merge_issues(&cond_frag);
                result.merge_issues(&body_frag);
                result
            }

            Stmt::DoWhile { body, cond, .. } => {
                let cond_frag = self.expr_to_rust(cond);
                let body_frag = self.stmt_to_rust_block(body, indent);
                let mut result = CodeFragment::ok(format!(
                    "{}loop {}\n{}if !({} != 0) {{ break; }}\n",
                    indent_str, body_frag.code, indent_str, cond_frag.code
                ));
                result.merge_issues(&body_frag);
                result.merge_issues(&cond_frag);
                result
            }

            Stmt::For { init, cond, step, body, .. } => {
                let mut code = String::new();
                let mut issues = Vec::new();
                let mut used_constants = HashSet::new();
                let mut needs_my_perl = false;

                // init
                if let Some(init) = init {
                    match init {
                        ForInit::Expr(expr) => {
                            let frag = self.expr_to_rust(expr);
                            code.push_str(&format!("{}{};\n", indent_str, frag.code));
                            issues.extend(frag.issues);
                            used_constants.extend(frag.used_constants);
                            needs_my_perl = needs_my_perl || frag.needs_my_perl;
                        }
                        ForInit::Decl(decl) => {
                            let frag = self.decl_to_rust(decl, indent);
                            code.push_str(&frag.code);
                            issues.extend(frag.issues);
                            used_constants.extend(frag.used_constants);
                            needs_my_perl = needs_my_perl || frag.needs_my_perl;
                        }
                    }
                }

                // while loop
                let cond_str = if let Some(c) = cond {
                    let frag = self.expr_to_rust(c);
                    issues.extend(frag.issues);
                    used_constants.extend(frag.used_constants);
                    needs_my_perl = needs_my_perl || frag.needs_my_perl;
                    format!("{} != 0", frag.code)
                } else {
                    "true".to_string()
                };

                code.push_str(&format!("{}while {} {{\n", indent_str, cond_str));

                // body
                let body_frag = if let Stmt::Compound(compound) = body.as_ref() {
                    self.compound_stmt_to_rust(compound, indent + 1)
                } else {
                    self.stmt_to_rust(body, indent + 1)
                };
                code.push_str(&body_frag.code);
                issues.extend(body_frag.issues);
                used_constants.extend(body_frag.used_constants);
                needs_my_perl = needs_my_perl || body_frag.needs_my_perl;

                // step
                if let Some(step) = step {
                    let frag = self.expr_to_rust(step);
                    code.push_str(&format!("{}    {};\n", indent_str, frag.code));
                    issues.extend(frag.issues);
                    used_constants.extend(frag.used_constants);
                    needs_my_perl = needs_my_perl || frag.needs_my_perl;
                }

                code.push_str(&format!("{}}}\n", indent_str));
                CodeFragment { code, issues, used_constants, needs_my_perl }
            }

            Stmt::Return(Some(expr), _) => {
                let expr_frag = self.expr_to_rust(expr);
                let mut result = CodeFragment::ok(format!("{}return {};\n", indent_str, expr_frag.code));
                result.merge_issues(&expr_frag);
                result
            }

            Stmt::Return(None, _) => {
                CodeFragment::ok(format!("{}return;\n", indent_str))
            }

            Stmt::Break(_) => {
                CodeFragment::ok(format!("{}break;\n", indent_str))
            }

            Stmt::Continue(_) => {
                CodeFragment::ok(format!("{}continue;\n", indent_str))
            }

            Stmt::Goto(label, _) => {
                let label_str = self.interner.get(*label);
                CodeFragment::with_issue(
                    format!("{}/* goto {} */\n", indent_str, label_str),
                    CodeIssue::new(CodeIssueKind::ControlFlow, format!("goto {}", label_str)),
                )
            }

            Stmt::Label { name, stmt, .. } => {
                let name_str = self.interner.get(*name);
                let stmt_frag = self.stmt_to_rust(stmt, indent);
                let mut result = CodeFragment::with_issue(
                    format!("{}/* label: {} */\n{}", indent_str, name_str, stmt_frag.code),
                    CodeIssue::new(CodeIssueKind::ControlFlow, format!("label: {}", name_str)),
                );
                result.merge_issues(&stmt_frag);
                result
            }

            Stmt::Switch { expr, body, .. } => {
                let expr_frag = self.expr_to_rust(expr);
                let body_frag = self.stmt_to_rust(body, indent);
                let mut result = CodeFragment::ok(format!(
                    "{}match {} {{\n{}{}    _ => {{}}\n{}}}\n",
                    indent_str, expr_frag.code, body_frag.code, indent_str, indent_str
                ));
                result.merge_issues(&expr_frag);
                result.merge_issues(&body_frag);
                result
            }

            Stmt::Case { expr, stmt, .. } => {
                let expr_frag = self.expr_to_rust(expr);
                let stmt_frag = self.stmt_to_rust(stmt, indent + 1);
                let mut result = CodeFragment::ok(format!(
                    "{}    {} => {{\n{}{}}}\n",
                    indent_str, expr_frag.code, stmt_frag.code, indent_str
                ));
                result.merge_issues(&expr_frag);
                result.merge_issues(&stmt_frag);
                result
            }

            Stmt::Default { stmt, .. } => {
                let stmt_frag = self.stmt_to_rust(stmt, indent + 1);
                let mut result = CodeFragment::ok(format!(
                    "{}    _ => {{\n{}{}}}\n",
                    indent_str, stmt_frag.code, indent_str
                ));
                result.merge_issues(&stmt_frag);
                result
            }

            Stmt::Asm { .. } => {
                CodeFragment::with_issue(
                    format!("{}/* asm */\n", indent_str),
                    CodeIssue::new(CodeIssueKind::InlineAsm, "inline assembly"),
                )
            }
        }
    }

    /// 文をブロック形式に変換（if/while等のbody用）
    fn stmt_to_rust_block(&self, stmt: &Stmt, indent: usize) -> CodeFragment {
        let indent_str = "    ".repeat(indent);
        match stmt {
            Stmt::Compound(compound) => {
                let inner = self.compound_stmt_to_rust(compound, indent + 1);
                let mut result = CodeFragment::ok(format!("{{\n{}{}}}", inner.code, indent_str));
                result.merge_issues(&inner);
                result
            }
            _ => {
                let inner = self.stmt_to_rust(stmt, indent + 1);
                let mut result = CodeFragment::ok(format!("{{\n{}{}}}", inner.code, indent_str));
                result.merge_issues(&inner);
                result
            }
        }
    }

    // ==================== Helper Functions ====================

    /// 文字をエスケープ
    fn escape_char(&self, c: char) -> String {
        match c {
            '\'' => "\\'".to_string(),
            '\\' => "\\\\".to_string(),
            '\n' => "\\n".to_string(),
            '\r' => "\\r".to_string(),
            '\t' => "\\t".to_string(),
            c if c.is_ascii_graphic() || c == ' ' => c.to_string(),
            c => format!("\\x{:02x}", c as u32),
        }
    }

    /// バイト列をエスケープ
    fn escape_bytes(&self, bytes: &[u8]) -> String {
        bytes.iter()
            .map(|&b| {
                match b {
                    b'"' => "\\\"".to_string(),
                    b'\\' => "\\\\".to_string(),
                    b'\n' => "\\n".to_string(),
                    b'\r' => "\\r".to_string(),
                    b'\t' => "\\t".to_string(),
                    b if b.is_ascii_graphic() || b == b' ' => (b as char).to_string(),
                    b => format!("\\x{:02x}", b),
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{FileRegistry, SourceLocation};
    use crate::token::Token;

    fn make_interner() -> StringInterner {
        StringInterner::new()
    }

    fn make_fields_dict() -> FieldsDict {
        FieldsDict::new()
    }

    #[test]
    fn test_int_literal() {
        let interner = make_interner();
        let fields = make_fields_dict();
        let codegen = RustCodeGen::new(&interner, &fields);

        let expr = Expr::new(ExprKind::IntLit(42), SourceLocation::default());
        assert_eq!(codegen.expr_to_rust(&expr).code, "42");
    }

    #[test]
    fn test_macro_comment_disabled() {
        let mut interner = make_interner();
        let fields = make_fields_dict();
        let mut files = FileRegistry::new();
        let file_id = files.register("test.h".into());

        let name = interner.intern("TEST_MACRO");
        let x = interner.intern("x");
        let def = MacroDef::function(
            name,
            vec![x],
            false,
            vec![Token::new(crate::token::TokenKind::Ident(x), SourceLocation::default())],
            SourceLocation { file_id, line: 10, column: 1 },
        );

        let info = crate::macro_analyzer2::MacroInfo2 {
            name,
            category: crate::macro_analyzer2::MacroCategory::Expression,
            return_type: Some("c_int".to_string()),
            param_types: std::collections::HashMap::new(),
            is_target: true,
            needs_my_perl: false,
            parsed_expr: None,
            def_loc: SourceLocation { file_id, line: 10, column: 1 },
            uses: std::collections::HashSet::new(),
            generic_params: std::collections::HashMap::new(),
            generic_return: None,
        };

        let expr = Expr::new(ExprKind::Ident(x), SourceLocation::default());

        let mut codegen = RustCodeGen::new(&interner, &fields);
        codegen.set_files(&files);
        // emit_macro_comments is false by default

        let result = codegen.macro_to_rust_fn(&def, &info, &expr);
        assert!(!result.code.contains("// Defined at:"));
    }

    #[test]
    fn test_macro_comment_enabled() {
        let mut interner = make_interner();
        let fields = make_fields_dict();
        let mut files = FileRegistry::new();
        let file_id = files.register("test.h".into());

        let name = interner.intern("TEST_MACRO");
        let x = interner.intern("x");
        let def = MacroDef::function(
            name,
            vec![x],
            false,
            vec![Token::new(crate::token::TokenKind::Ident(x), SourceLocation::default())],
            SourceLocation { file_id, line: 10, column: 1 },
        );

        let info = crate::macro_analyzer2::MacroInfo2 {
            name,
            category: crate::macro_analyzer2::MacroCategory::Expression,
            return_type: Some("c_int".to_string()),
            param_types: std::collections::HashMap::new(),
            is_target: true,
            needs_my_perl: false,
            parsed_expr: None,
            def_loc: SourceLocation { file_id, line: 10, column: 1 },
            uses: std::collections::HashSet::new(),
            generic_params: std::collections::HashMap::new(),
            generic_return: None,
        };

        let expr = Expr::new(ExprKind::Ident(x), SourceLocation::default());

        let mut codegen = RustCodeGen::new(&interner, &fields);
        codegen.set_files(&files);
        codegen.set_options(CodeGenOptions { emit_macro_comments: true });

        let result = codegen.macro_to_rust_fn(&def, &info, &expr);
        assert!(result.code.contains("// Defined at: test.h:10"));
    }

    #[test]
    fn test_macro_comment_without_files() {
        let mut interner = make_interner();
        let fields = make_fields_dict();

        let name = interner.intern("TEST_MACRO");
        let x = interner.intern("x");
        let def = MacroDef::function(
            name,
            vec![x],
            false,
            vec![Token::new(crate::token::TokenKind::Ident(x), SourceLocation::default())],
            SourceLocation::default(),
        );

        let info = crate::macro_analyzer2::MacroInfo2 {
            name,
            category: crate::macro_analyzer2::MacroCategory::Expression,
            return_type: Some("c_int".to_string()),
            param_types: std::collections::HashMap::new(),
            is_target: true,
            needs_my_perl: false,
            parsed_expr: None,
            def_loc: SourceLocation::default(),
            uses: std::collections::HashSet::new(),
            generic_params: std::collections::HashMap::new(),
            generic_return: None,
        };

        let expr = Expr::new(ExprKind::Ident(x), SourceLocation::default());

        let mut codegen = RustCodeGen::new(&interner, &fields);
        // No files set
        codegen.set_options(CodeGenOptions { emit_macro_comments: true });

        let result = codegen.macro_to_rust_fn(&def, &info, &expr);
        // Should not panic and should not include comment
        assert!(!result.code.contains("// Defined at:"));
    }

    #[test]
    fn test_binary_op() {
        let mut interner = make_interner();
        let fields = make_fields_dict();

        let x = interner.intern("x");
        let y = interner.intern("y");
        let loc = SourceLocation::default();

        let codegen = RustCodeGen::new(&interner, &fields);

        let expr = Expr::new(
            ExprKind::Binary {
                op: BinOp::Add,
                lhs: Box::new(Expr::new(ExprKind::Ident(x), loc.clone())),
                rhs: Box::new(Expr::new(ExprKind::Ident(y), loc.clone())),
            },
            loc,
        );
        assert_eq!(codegen.expr_to_rust(&expr).code, "(x + y)");
    }

    #[test]
    fn test_ptr_member_access() {
        let mut interner = make_interner();
        let fields = make_fields_dict();

        let sv = interner.intern("sv");
        let sv_any = interner.intern("sv_any");
        let loc = SourceLocation::default();

        let codegen = RustCodeGen::new(&interner, &fields);

        // sv->sv_any
        let expr = Expr::new(
            ExprKind::PtrMember {
                expr: Box::new(Expr::new(ExprKind::Ident(sv), loc.clone())),
                member: sv_any,
            },
            loc,
        );
        assert_eq!(codegen.expr_to_rust(&expr).code, "(*sv).sv_any");
    }
}
