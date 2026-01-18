//! Rust コード生成モジュール
//!
//! 型推論結果から Rust コードを生成する。

use std::io::{self, Write};

use crate::ast::{AssertKind, AssignOp, BinOp, BlockItem, CompoundStmt, DeclSpecs, DerivedDecl, Expr, ExprKind, FunctionDef, ParamDecl, Stmt, TypeSpec};
use crate::infer_api::InferResult;
use crate::intern::StringInterner;
use crate::macro_infer::{MacroInferInfo, MacroParam, ParseResult};
use crate::sexp::SexpPrinter;

/// 二項演算子を Rust 形式に変換
fn bin_op_to_rust(op: BinOp) -> &'static str {
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

/// 代入演算子を Rust 形式に変換
fn assign_op_to_rust(op: AssignOp) -> &'static str {
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

/// 文字をエスケープ
fn escape_char(c: u8) -> String {
    match c {
        b'\'' => "\\'".to_string(),
        b'\\' => "\\\\".to_string(),
        b'\n' => "\\n".to_string(),
        b'\r' => "\\r".to_string(),
        b'\t' => "\\t".to_string(),
        c if c.is_ascii_graphic() || c == b' ' => (c as char).to_string(),
        c => format!("\\x{:02x}", c),
    }
}

/// 文字列をエスケープ
fn escape_string(s: &[u8]) -> String {
    s.iter().map(|&c| escape_char(c)).collect()
}

/// 式がブール型を返すかどうかを判定（フリー関数版）
///
/// 注: LogNot は含めない。現在の LogNot -> Rust 変換は
/// `(if x { 0 } else { 1 })` という int 値を返すため。
fn is_boolean_expr_kind(kind: &ExprKind) -> bool {
    match kind {
        ExprKind::Binary { op, .. } => matches!(op,
            BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge |
            BinOp::Eq | BinOp::Ne | BinOp::LogAnd | BinOp::LogOr
        ),
        _ => false,
    }
}

/// コード生成の設定
#[derive(Debug, Clone)]
pub struct CodegenConfig {
    /// inline 関数を出力するか
    pub emit_inline_fns: bool,
    /// マクロを出力するか
    pub emit_macros: bool,
    /// コメントにソース位置を含めるか
    pub include_source_location: bool,
}

impl Default for CodegenConfig {
    fn default() -> Self {
        Self {
            emit_inline_fns: true,
            emit_macros: true,
            include_source_location: true,
        }
    }
}

/// 生成ステータス
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerateStatus {
    /// 正常生成
    Success,
    /// パース失敗（S式をコメント出力）
    ParseFailed,
    /// 型推論不完全（型付S式をコメント出力）
    TypeIncomplete,
    /// スキップ（対象外）
    Skip,
}

/// コード生成統計
#[derive(Debug, Clone, Default)]
pub struct CodegenStats {
    /// 正常生成されたマクロ数
    pub macros_success: usize,
    /// パース失敗マクロ数
    pub macros_parse_failed: usize,
    /// 型推論失敗マクロ数
    pub macros_type_incomplete: usize,
    /// 正常生成された inline 関数数
    pub inline_fns_success: usize,
    /// 型推論失敗 inline 関数数
    pub inline_fns_type_incomplete: usize,
}

/// 一つの関数の生成結果
#[derive(Debug, Clone)]
pub struct GeneratedCode {
    /// 生成されたコード
    pub code: String,
    /// 不完全マーカーの数
    pub incomplete_count: usize,
}

impl GeneratedCode {
    /// 生成が完全かどうか（不完全マーカーがないか）
    pub fn is_complete(&self) -> bool {
        self.incomplete_count == 0
    }
}

/// 単一関数を生成するためのコード生成器（使い捨て）
///
/// 各関数の生成ごとにフレッシュなインスタンスを作成して使用する。
/// 生成中に不完全マーカーが出力された回数をカウントし、
/// 生成完了時に `GeneratedCode` として結果を返す。
pub struct RustCodegen<'a> {
    interner: &'a StringInterner,
    /// 内部バッファ（生成結果を蓄積）
    buffer: String,
    /// 不完全マーカーの生成回数
    incomplete_count: usize,
}

/// コード生成全体を管理する構造体
///
/// 実際の出力先（Write）を保持し、生成の成功/失敗に応じて
/// 適切な形式で出力する。
pub struct CodegenDriver<'a, W: Write> {
    writer: W,
    interner: &'a StringInterner,
    config: CodegenConfig,
    stats: CodegenStats,
}

impl<'a> RustCodegen<'a> {
    /// 新しい単一関数用コード生成器を作成
    pub fn new(interner: &'a StringInterner) -> Self {
        Self {
            interner,
            buffer: String::new(),
            incomplete_count: 0,
        }
    }

    /// 不完全マーカー: 型が不明
    fn unknown_marker(&mut self) -> &'static str {
        self.incomplete_count += 1;
        "/* unknown */"
    }

    /// 不完全マーカー: TODO
    fn todo_marker(&mut self, msg: &str) -> String {
        self.incomplete_count += 1;
        format!("/* TODO: {} */", msg)
    }

    /// 不完全マーカー: 型
    fn type_marker(&mut self) -> &'static str {
        self.incomplete_count += 1;
        "/* type */"
    }

    /// バッファに書き込み
    fn write(&mut self, s: &str) {
        self.buffer.push_str(s);
    }

    /// バッファに行を書き込み
    fn writeln(&mut self, s: &str) {
        self.buffer.push_str(s);
        self.buffer.push('\n');
    }

    /// 生成結果を取得（self を消費）
    fn into_generated_code(self) -> GeneratedCode {
        GeneratedCode {
            code: self.buffer,
            incomplete_count: self.incomplete_count,
        }
    }

    /// マクロ関数を生成（self を消費）
    pub fn generate_macro(mut self, info: &MacroInferInfo) -> GeneratedCode {
        let name_str = self.interner.get(info.name);

        // パラメータリストを構築（型情報付き）
        let params_with_types = self.build_param_list(info);

        // 戻り値の型を取得
        let return_type = self.get_return_type(info);

        // THX 依存の場合は my_perl パラメータを追加
        let thx_param = if info.is_thx_dependent {
            "my_perl: *mut PerlInterpreter"
        } else {
            ""
        };

        // 関数シグネチャ
        let params_str = if thx_param.is_empty() {
            params_with_types.clone()
        } else if params_with_types.is_empty() {
            thx_param.to_string()
        } else {
            format!("{}, {}", thx_param, params_with_types)
        };

        // ドキュメントコメント
        let thx_info = if info.is_thx_dependent { " [THX]" } else { "" };
        self.writeln(&format!("/// {}{} - macro function", name_str, thx_info));
        self.writeln("#[inline]");

        // 関数定義
        self.writeln(&format!("pub unsafe fn {}({}) -> {} {{", name_str, params_str, return_type));

        // 関数本体
        match &info.parse_result {
            ParseResult::Expression(expr) => {
                let rust_expr = self.expr_to_rust(expr, info);
                self.writeln(&format!("    {}", rust_expr));
            }
            ParseResult::Statement(block_items) => {
                for item in block_items {
                    if let BlockItem::Stmt(stmt) = item {
                        let rust_stmt = self.stmt_to_rust(stmt, info);
                        self.writeln(&format!("    {}", rust_stmt));
                    }
                }
            }
            ParseResult::Unparseable(_) => {
                self.writeln("    unimplemented!()");
            }
        }

        self.writeln("}");
        self.writeln("");

        self.into_generated_code()
    }

    /// パラメータリストを構築（型情報付き）
    fn build_param_list(&mut self, info: &MacroInferInfo) -> String {
        info.params.iter()
            .map(|p| {
                let name = self.interner.get(p.name);
                let ty = self.get_param_type(p, info);
                format!("{}: {}", name, ty)
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// パラメータの型を取得
    fn get_param_type(&mut self, param: &MacroParam, info: &MacroInferInfo) -> String {
        let param_name = param.name;

        // 方法1: パラメータを参照する式の型制約から取得（逆引き辞書を使用）
        if let Some(expr_ids) = info.type_env.param_to_exprs.get(&param_name) {
            for expr_id in expr_ids {
                if let Some(constraints) = info.type_env.expr_constraints.get(expr_id) {
                    if let Some(first) = constraints.first() {
                        return self.type_repr_to_rust(&first.ty);
                    }
                }
            }
        }

        // 方法2: 従来の方法（MacroParam の ExprId）- フォールバック
        let expr_id = param.expr_id();
        if let Some(constraints) = info.type_env.expr_constraints.get(&expr_id) {
            if let Some(first) = constraints.first() {
                return self.type_repr_to_rust(&first.ty);
            }
        }

        self.unknown_marker().to_string()
    }

    /// 戻り値の型を取得
    fn get_return_type(&mut self, info: &MacroInferInfo) -> String {
        match &info.parse_result {
            ParseResult::Expression(expr) => {
                if let Some(constraints) = info.type_env.expr_constraints.get(&expr.id) {
                    if let Some(first) = constraints.first() {
                        return self.type_repr_to_rust(&first.ty);
                    }
                }
                self.unknown_marker().to_string()
            }
            ParseResult::Statement(_) => "()".to_string(),
            ParseResult::Unparseable(_) => "()".to_string(),
        }
    }

    /// TypeRepr を Rust 型文字列に変換
    fn type_repr_to_rust(&self, ty: &crate::type_repr::TypeRepr) -> String {
        ty.to_rust_string(self.interner)
    }

    /// 式がブール型を返すかどうかを判定
    fn is_boolean_expr(&self, expr: &Expr) -> bool {
        is_boolean_expr_kind(&expr.kind)
    }

    /// 式を Rust コードに変換
    fn expr_to_rust(&mut self, expr: &Expr, info: &MacroInferInfo) -> String {
        match &expr.kind {
            ExprKind::Ident(name) => {
                self.interner.get(*name).to_string()
            }
            ExprKind::IntLit(n) => {
                format!("{}", n)
            }
            ExprKind::UIntLit(n) => {
                format!("{}u64", n)
            }
            ExprKind::FloatLit(f) => {
                format!("{}", f)
            }
            ExprKind::CharLit(c) => {
                format!("'{}'", escape_char(*c))
            }
            ExprKind::StringLit(s) => {
                format!("c\"{}\"", escape_string(s))
            }
            ExprKind::Binary { op, lhs, rhs } => {
                let l = self.expr_to_rust(lhs, info);
                let r = self.expr_to_rust(rhs, info);
                format!("({} {} {})", l, bin_op_to_rust(*op), r)
            }
            ExprKind::Call { func, args } => {
                let f = self.expr_to_rust(func, info);
                let a: Vec<_> = args.iter()
                    .map(|a| self.expr_to_rust(a, info))
                    .collect();
                format!("{}({})", f, a.join(", "))
            }
            ExprKind::Member { expr: base, member } => {
                let e = self.expr_to_rust(base, info);
                let m = self.interner.get(*member);
                format!("({}).{}", e, m)
            }
            ExprKind::PtrMember { expr: base, member } => {
                let e = self.expr_to_rust(base, info);
                let m = self.interner.get(*member);
                format!("(*{}).{}", e, m)
            }
            ExprKind::Index { expr: base, index } => {
                let b = self.expr_to_rust(base, info);
                let i = self.expr_to_rust(index, info);
                format!("(*{}.offset({} as isize))", b, i)
            }
            ExprKind::Cast { type_name, expr: inner } => {
                let e = self.expr_to_rust(inner, info);
                let t = self.type_name_to_rust(type_name);
                format!("({} as {})", e, t)
            }
            ExprKind::Deref(inner) => {
                let e = self.expr_to_rust(inner, info);
                format!("(*{})", e)
            }
            ExprKind::AddrOf(inner) => {
                let e = self.expr_to_rust(inner, info);
                format!("(&mut {})", e)
            }
            ExprKind::PreInc(inner) => {
                let e = self.expr_to_rust(inner, info);
                format!("{{ {} += 1; {} }}", e, e)
            }
            ExprKind::PreDec(inner) => {
                let e = self.expr_to_rust(inner, info);
                format!("{{ {} -= 1; {} }}", e, e)
            }
            ExprKind::PostInc(inner) => {
                let e = self.expr_to_rust(inner, info);
                format!("{{ let _t = {}; {} += 1; _t }}", e, e)
            }
            ExprKind::PostDec(inner) => {
                let e = self.expr_to_rust(inner, info);
                format!("{{ let _t = {}; {} -= 1; _t }}", e, e)
            }
            ExprKind::UnaryPlus(inner) => {
                self.expr_to_rust(inner, info)
            }
            ExprKind::UnaryMinus(inner) => {
                let e = self.expr_to_rust(inner, info);
                format!("(-{})", e)
            }
            ExprKind::BitNot(inner) => {
                let e = self.expr_to_rust(inner, info);
                format!("(!{})", e)
            }
            ExprKind::LogNot(inner) => {
                let e = self.expr_to_rust(inner, info);
                format!("(if {} {{ 0 }} else {{ 1 }})", e)
            }
            ExprKind::Sizeof(inner) => {
                let e = self.expr_to_rust(inner, info);
                format!("std::mem::size_of_val(&{})", e)
            }
            ExprKind::SizeofType(type_name) => {
                let t = self.type_name_to_rust(type_name);
                format!("std::mem::size_of::<{}>()", t)
            }
            ExprKind::Conditional { cond, then_expr, else_expr } => {
                let c = self.expr_to_rust(cond, info);
                let t = self.expr_to_rust(then_expr, info);
                let e = self.expr_to_rust(else_expr, info);
                format!("(if {} != 0 {{ {} }} else {{ {} }})", c, t, e)
            }
            ExprKind::Comma { lhs, rhs } => {
                let l = self.expr_to_rust(lhs, info);
                let r = self.expr_to_rust(rhs, info);
                format!("{{ {}; {} }}", l, r)
            }
            ExprKind::Assign { op, lhs, rhs } => {
                let l = self.expr_to_rust(lhs, info);
                let r = self.expr_to_rust(rhs, info);
                match op {
                    AssignOp::Assign => format!("{{ {} = {}; {} }}", l, r, l),
                    _ => format!("{{ {} {} {}; {} }}", l, assign_op_to_rust(*op), r, l),
                }
            }
            ExprKind::Assert { kind, condition } => {
                let cond = self.expr_to_rust(condition, info);
                let assert_expr = if self.is_boolean_expr(condition) {
                    format!("assert!({})", cond)
                } else {
                    format!("assert!(({}) != 0)", cond)
                };
                match kind {
                    AssertKind::Assert => assert_expr,
                    AssertKind::AssertUnderscore => format!("{{ {}; }}", assert_expr),
                }
            }
            _ => {
                self.todo_marker(&format!("{:?}", std::mem::discriminant(&expr.kind)))
            }
        }
    }

    /// 文を Rust コードに変換
    fn stmt_to_rust(&mut self, stmt: &Stmt, info: &MacroInferInfo) -> String {
        match stmt {
            Stmt::Expr(Some(expr), _) => {
                format!("{};", self.expr_to_rust(expr, info))
            }
            Stmt::Expr(None, _) => ";".to_string(),
            Stmt::Return(Some(expr), _) => {
                format!("return {};", self.expr_to_rust(expr, info))
            }
            Stmt::Return(None, _) => "return;".to_string(),
            _ => self.todo_marker("stmt")
        }
    }

    /// TypeName を Rust 型文字列に変換
    fn type_name_to_rust(&mut self, type_name: &crate::ast::TypeName) -> String {
        // 簡易実装: typedef 名があればそれを使用
        for spec in &type_name.specs.type_specs {
            if let crate::ast::TypeSpec::TypedefName(name) = spec {
                return self.interner.get(*name).to_string();
            }
        }
        self.type_marker().to_string()
    }

    /// DeclSpecs を Rust 型文字列に変換
    fn decl_specs_to_rust(&mut self, specs: &DeclSpecs) -> String {
        // typedef 名を優先
        for spec in &specs.type_specs {
            if let TypeSpec::TypedefName(name) = spec {
                return self.interner.get(*name).to_string();
            }
        }

        // 基本型をチェック
        let mut is_void = false;
        let mut is_char = false;
        let mut is_int = false;
        let mut is_short = false;
        let mut is_long = 0usize;
        let mut is_unsigned = false;
        let mut is_float = false;
        let mut is_double = false;

        for spec in &specs.type_specs {
            match spec {
                TypeSpec::Void => is_void = true,
                TypeSpec::Char => is_char = true,
                TypeSpec::Int => is_int = true,
                TypeSpec::Short => is_short = true,
                TypeSpec::Long => is_long += 1,
                TypeSpec::Unsigned => is_unsigned = true,
                TypeSpec::Signed => {}
                TypeSpec::Float => is_float = true,
                TypeSpec::Double => is_double = true,
                TypeSpec::Bool => return "bool".to_string(),
                TypeSpec::Struct(spec) => {
                    if let Some(n) = spec.name {
                        return self.interner.get(n).to_string();
                    } else {
                        return self.type_marker().to_string();
                    }
                }
                TypeSpec::Union(spec) => {
                    if let Some(n) = spec.name {
                        return self.interner.get(n).to_string();
                    } else {
                        return self.type_marker().to_string();
                    }
                }
                TypeSpec::Enum(spec) => {
                    if let Some(n) = spec.name {
                        return self.interner.get(n).to_string();
                    } else {
                        return "c_int".to_string();
                    }
                }
                _ => {}
            }
        }

        if is_void {
            return "()".to_string();
        }

        if is_float {
            return "c_float".to_string();
        }

        if is_double {
            return if is_long > 0 { "c_longdouble".to_string() } else { "c_double".to_string() };
        }

        if is_char {
            return if is_unsigned { "c_uchar".to_string() } else { "c_char".to_string() };
        }

        if is_short {
            return if is_unsigned { "c_ushort".to_string() } else { "c_short".to_string() };
        }

        if is_long >= 2 {
            return if is_unsigned { "c_ulonglong".to_string() } else { "c_longlong".to_string() };
        }

        if is_long == 1 {
            return if is_unsigned { "c_ulong".to_string() } else { "c_long".to_string() };
        }

        if is_int || is_unsigned {
            return if is_unsigned { "c_uint".to_string() } else { "c_int".to_string() };
        }

        self.type_marker().to_string()
    }

    /// 派生型を型に適用
    fn apply_derived_to_type(&self, base: &str, derived: &[DerivedDecl]) -> String {
        let mut result = base.to_string();
        for d in derived.iter().rev() {
            match d {
                DerivedDecl::Pointer(quals) => {
                    if quals.is_const {
                        result = format!("*const {}", result);
                    } else {
                        result = format!("*mut {}", result);
                    }
                }
                DerivedDecl::Array(arr) => {
                    if let Some(ref size_expr) = arr.size {
                        // 定数サイズ配列
                        if let ExprKind::IntLit(n) = &size_expr.kind {
                            result = format!("[{}; {}]", result, n);
                        } else {
                            result = format!("*mut {}", result);
                        }
                    } else {
                        result = format!("*mut {}", result);
                    }
                }
                DerivedDecl::Function(_) => {
                    // 関数ポインタは複雑なので簡易実装
                    result = format!("/* fn */ {}", result);
                }
            }
        }
        result
    }

    /// inline 関数を生成（self を消費）
    pub fn generate_inline_fn(mut self, name: crate::InternedStr, func_def: &FunctionDef) -> GeneratedCode {
        let name_str = self.interner.get(name);

        // パラメータリストを取得
        let params_str = self.build_fn_param_list(&func_def.declarator.derived);

        // 戻り値の型を取得
        let return_type = self.decl_specs_to_rust(&func_def.specs);

        // ドキュメントコメント
        self.writeln(&format!("/// {} - inline function", name_str));
        self.writeln("#[inline]");

        // 関数定義
        self.writeln(&format!("pub unsafe fn {}({}) -> {} {{", name_str, params_str, return_type));

        // 関数本体
        let body_str = self.compound_stmt_to_string(&func_def.body, "    ");
        self.buffer.push_str(&body_str);

        self.writeln("}");
        self.writeln("");

        self.into_generated_code()
    }

    /// DerivedDecl から関数パラメータリストを構築
    fn build_fn_param_list(&mut self, derived: &[DerivedDecl]) -> String {
        for d in derived {
            if let DerivedDecl::Function(param_list) = d {
                let params: Vec<_> = param_list.params.iter()
                    .map(|p| self.param_decl_to_rust(p))
                    .collect();
                let mut result = params.join(", ");
                if param_list.is_variadic {
                    if !result.is_empty() {
                        result.push_str(", ");
                    }
                    result.push_str("...");
                }
                return result;
            }
        }
        String::new()
    }

    /// ParamDecl を Rust パラメータ宣言に変換
    fn param_decl_to_rust(&mut self, param: &ParamDecl) -> String {
        let name = param.declarator
            .as_ref()
            .and_then(|d| d.name)
            .map(|n| self.interner.get(n).to_string())
            .unwrap_or_else(|| "_".to_string());

        let ty = self.decl_specs_to_rust(&param.specs);

        // ポインタ派生型を適用
        let ty = if let Some(ref declarator) = param.declarator {
            self.apply_derived_to_type(&ty, &declarator.derived)
        } else {
            ty
        };

        format!("{}: {}", name, ty)
    }

    /// 複合文を文字列として生成
    fn compound_stmt_to_string(&mut self, stmt: &CompoundStmt, indent: &str) -> String {
        let mut result = String::new();
        for item in &stmt.items {
            match item {
                BlockItem::Decl(decl) => {
                    result.push_str(&format!("{}// local decl: {:?}\n", indent, decl.specs));
                }
                BlockItem::Stmt(s) => {
                    let rust_stmt = self.stmt_to_rust_inline(s, indent);
                    result.push_str(&rust_stmt);
                    result.push('\n');
                }
            }
        }
        result
    }

    /// 文を Rust コードに変換（インライン関数用）
    fn stmt_to_rust_inline(&mut self, stmt: &Stmt, indent: &str) -> String {
        match stmt {
            Stmt::Expr(Some(expr), _) => {
                format!("{}{};", indent, self.expr_to_rust_inline(expr))
            }
            Stmt::Expr(None, _) => format!("{};", indent),
            Stmt::Return(Some(expr), _) => {
                format!("{}return {};", indent, self.expr_to_rust_inline(expr))
            }
            Stmt::Return(None, _) => format!("{}return;", indent),
            Stmt::If { cond, then_stmt, else_stmt, .. } => {
                let cond_str = self.expr_to_rust_inline(cond);
                let mut result = format!("{}if {} != 0 {{\n", indent, cond_str);
                let nested_indent = format!("{}    ", indent);
                result.push_str(&self.stmt_to_rust_inline(then_stmt, &nested_indent));
                result.push_str("\n");
                result.push_str(&format!("{}}}", indent));
                if let Some(else_stmt) = else_stmt {
                    result.push_str(" else {\n");
                    result.push_str(&self.stmt_to_rust_inline(else_stmt, &nested_indent));
                    result.push_str("\n");
                    result.push_str(&format!("{}}}", indent));
                }
                result
            }
            Stmt::Compound(compound) => {
                let mut result = format!("{}{{\n", indent);
                for item in &compound.items {
                    match item {
                        BlockItem::Stmt(s) => {
                            let nested_indent = format!("{}    ", indent);
                            result.push_str(&self.stmt_to_rust_inline(s, &nested_indent));
                            result.push_str("\n");
                        }
                        BlockItem::Decl(_) => {
                            result.push_str(&format!("{}    // local decl\n", indent));
                        }
                    }
                }
                result.push_str(&format!("{}}}", indent));
                result
            }
            _ => self.todo_marker(&format!("{:?}", std::mem::discriminant(stmt)))
        }
    }

    /// 式を Rust コードに変換（インライン関数用）
    fn expr_to_rust_inline(&mut self, expr: &Expr) -> String {
        match &expr.kind {
            ExprKind::Ident(name) => {
                self.interner.get(*name).to_string()
            }
            ExprKind::IntLit(n) => {
                format!("{}", n)
            }
            ExprKind::UIntLit(n) => {
                format!("{}u64", n)
            }
            ExprKind::FloatLit(f) => {
                format!("{}", f)
            }
            ExprKind::CharLit(c) => {
                format!("'{}'", escape_char(*c))
            }
            ExprKind::StringLit(s) => {
                format!("c\"{}\"", escape_string(s))
            }
            ExprKind::Binary { op, lhs, rhs } => {
                let l = self.expr_to_rust_inline(lhs);
                let r = self.expr_to_rust_inline(rhs);
                format!("({} {} {})", l, bin_op_to_rust(*op), r)
            }
            ExprKind::Call { func, args } => {
                let f = self.expr_to_rust_inline(func);
                let a: Vec<_> = args.iter()
                    .map(|a| self.expr_to_rust_inline(a))
                    .collect();
                format!("{}({})", f, a.join(", "))
            }
            ExprKind::Member { expr: base, member } => {
                let e = self.expr_to_rust_inline(base);
                let m = self.interner.get(*member);
                format!("({}).{}", e, m)
            }
            ExprKind::PtrMember { expr: base, member } => {
                let e = self.expr_to_rust_inline(base);
                let m = self.interner.get(*member);
                format!("(*{}).{}", e, m)
            }
            ExprKind::Index { expr: base, index } => {
                let b = self.expr_to_rust_inline(base);
                let i = self.expr_to_rust_inline(index);
                format!("(*{}.offset({} as isize))", b, i)
            }
            ExprKind::Cast { type_name, expr: inner } => {
                let e = self.expr_to_rust_inline(inner);
                let t = self.type_name_to_rust(type_name);
                format!("({} as {})", e, t)
            }
            ExprKind::Deref(inner) => {
                let e = self.expr_to_rust_inline(inner);
                format!("(*{})", e)
            }
            ExprKind::AddrOf(inner) => {
                let e = self.expr_to_rust_inline(inner);
                format!("(&mut {})", e)
            }
            ExprKind::PreInc(inner) => {
                let e = self.expr_to_rust_inline(inner);
                format!("{{ {} += 1; {} }}", e, e)
            }
            ExprKind::PreDec(inner) => {
                let e = self.expr_to_rust_inline(inner);
                format!("{{ {} -= 1; {} }}", e, e)
            }
            ExprKind::PostInc(inner) => {
                let e = self.expr_to_rust_inline(inner);
                format!("{{ let _t = {}; {} += 1; _t }}", e, e)
            }
            ExprKind::PostDec(inner) => {
                let e = self.expr_to_rust_inline(inner);
                format!("{{ let _t = {}; {} -= 1; _t }}", e, e)
            }
            ExprKind::UnaryPlus(inner) => self.expr_to_rust_inline(inner),
            ExprKind::UnaryMinus(inner) => {
                let e = self.expr_to_rust_inline(inner);
                format!("(-{})", e)
            }
            ExprKind::BitNot(inner) => {
                let e = self.expr_to_rust_inline(inner);
                format!("(!{})", e)
            }
            ExprKind::LogNot(inner) => {
                let e = self.expr_to_rust_inline(inner);
                format!("(if {} {{ 0 }} else {{ 1 }})", e)
            }
            ExprKind::Sizeof(inner) => {
                let e = self.expr_to_rust_inline(inner);
                format!("std::mem::size_of_val(&{})", e)
            }
            ExprKind::SizeofType(type_name) => {
                let t = self.type_name_to_rust(type_name);
                format!("std::mem::size_of::<{}>()", t)
            }
            ExprKind::Conditional { cond, then_expr, else_expr } => {
                let c = self.expr_to_rust_inline(cond);
                let t = self.expr_to_rust_inline(then_expr);
                let e = self.expr_to_rust_inline(else_expr);
                format!("(if {} != 0 {{ {} }} else {{ {} }})", c, t, e)
            }
            ExprKind::Comma { lhs, rhs } => {
                let l = self.expr_to_rust_inline(lhs);
                let r = self.expr_to_rust_inline(rhs);
                format!("{{ {}; {} }}", l, r)
            }
            ExprKind::Assign { op, lhs, rhs } => {
                let l = self.expr_to_rust_inline(lhs);
                let r = self.expr_to_rust_inline(rhs);
                match op {
                    AssignOp::Assign => format!("{{ {} = {}; {} }}", l, r, l),
                    _ => format!("{{ {} {} {}; {} }}", l, assign_op_to_rust(*op), r, l),
                }
            }
            ExprKind::Assert { kind, condition } => {
                let cond = self.expr_to_rust_inline(condition);
                let assert_expr = if self.is_boolean_expr(condition) {
                    format!("assert!({})", cond)
                } else {
                    format!("assert!(({}) != 0)", cond)
                };
                match kind {
                    AssertKind::Assert => assert_expr,
                    AssertKind::AssertUnderscore => format!("{{ {}; }}", assert_expr),
                }
            }
            _ => self.todo_marker(&format!("{:?}", std::mem::discriminant(&expr.kind)))
        }
    }
}

impl<'a, W: Write> CodegenDriver<'a, W> {
    /// 新しいコード生成ドライバを作成
    pub fn new(writer: W, interner: &'a StringInterner, config: CodegenConfig) -> Self {
        Self {
            writer,
            interner,
            config,
            stats: CodegenStats::default(),
        }
    }

    /// 統計情報を取得
    pub fn stats(&self) -> &CodegenStats {
        &self.stats
    }

    /// 全体を生成
    pub fn generate(&mut self, result: &InferResult) -> io::Result<()> {
        // ヘッダーコメント
        writeln!(self.writer, "// Auto-generated Rust bindings")?;
        writeln!(self.writer, "// Generated by libperl-macrogen")?;
        writeln!(self.writer)?;

        // inline 関数セクション
        if self.config.emit_inline_fns {
            self.generate_inline_fns(result)?;
        }

        // マクロセクション
        if self.config.emit_macros {
            self.generate_macros(result)?;
        }

        Ok(())
    }

    /// inline 関数セクションを生成
    pub fn generate_inline_fns(&mut self, result: &InferResult) -> io::Result<()> {
        writeln!(self.writer, "// =============================================================================")?;
        writeln!(self.writer, "// Inline Functions")?;
        writeln!(self.writer, "// =============================================================================")?;
        writeln!(self.writer)?;

        // 名前順にソート
        let mut fns: Vec<_> = result.inline_fn_dict.iter()
            .filter(|(_, func_def)| func_def.is_target)
            .collect();
        fns.sort_by_key(|(name, _)| self.interner.get(**name));

        for (name, func_def) in fns {
            // 新しい RustCodegen を使って inline 関数を生成
            let codegen = RustCodegen::new(self.interner);
            let generated = codegen.generate_inline_fn(*name, func_def);

            if generated.is_complete() {
                // 完全な生成：そのまま出力
                write!(self.writer, "{}", generated.code)?;
                self.stats.inline_fns_success += 1;
            } else {
                // 不完全な生成：コメントアウトして出力
                let name_str = self.interner.get(*name);
                writeln!(self.writer, "// [CODEGEN_INCOMPLETE] {} - inline function", name_str)?;
                for line in generated.code.lines() {
                    writeln!(self.writer, "// {}", line)?;
                }
                writeln!(self.writer)?;
                self.stats.inline_fns_type_incomplete += 1;
            }
        }

        writeln!(self.writer)?;
        Ok(())
    }

    // 以下は旧 generate_inline_fn（削除予定）
    #[allow(dead_code)]
    fn generate_inline_fn_old(&mut self, name: crate::InternedStr, func_def: &FunctionDef) -> io::Result<()> {
        let name_str = self.interner.get(name);

        // パラメータリストを取得
        let params_str = self.build_fn_param_list(&func_def.declarator.derived);

        // 戻り値の型を取得
        let return_type = self.decl_specs_to_rust(&func_def.specs);

        // ドキュメントコメント
        writeln!(self.writer, "/// {} - inline function", name_str)?;
        writeln!(self.writer, "#[inline]")?;

        // 関数定義
        writeln!(self.writer, "pub unsafe fn {}({}) -> {} {{", name_str, params_str, return_type)?;

        // 関数本体
        self.generate_compound_stmt(&func_def.body, "    ")?;

        writeln!(self.writer, "}}")?;
        writeln!(self.writer)?;

        Ok(())
    }

    /// DerivedDecl から関数パラメータリストを構築
    fn build_fn_param_list(&self, derived: &[DerivedDecl]) -> String {
        for d in derived {
            if let DerivedDecl::Function(param_list) = d {
                let params: Vec<_> = param_list.params.iter()
                    .map(|p| self.param_decl_to_rust(p))
                    .collect();
                let mut result = params.join(", ");
                if param_list.is_variadic {
                    if !result.is_empty() {
                        result.push_str(", ");
                    }
                    result.push_str("...");
                }
                return result;
            }
        }
        String::new()
    }

    /// ParamDecl を Rust パラメータ宣言に変換
    fn param_decl_to_rust(&self, param: &ParamDecl) -> String {
        let name = param.declarator
            .as_ref()
            .and_then(|d| d.name)
            .map(|n| self.interner.get(n).to_string())
            .unwrap_or_else(|| "_".to_string());

        let ty = self.decl_specs_to_rust(&param.specs);

        // ポインタ派生型を適用
        let ty = if let Some(ref declarator) = param.declarator {
            self.apply_derived_to_type(&ty, &declarator.derived)
        } else {
            ty
        };

        format!("{}: {}", name, ty)
    }

    /// DeclSpecs を Rust 型文字列に変換
    fn decl_specs_to_rust(&self, specs: &DeclSpecs) -> String {
        // typedef 名を優先
        for spec in &specs.type_specs {
            if let TypeSpec::TypedefName(name) = spec {
                return self.interner.get(*name).to_string();
            }
        }

        // 基本型をチェック
        let mut is_void = false;
        let mut is_char = false;
        let mut is_int = false;
        let mut is_float = false;
        let mut is_double = false;
        let mut is_signed = true;
        let mut is_short = false;
        let mut is_long = 0i32;
        let mut is_unsigned = false;

        for spec in &specs.type_specs {
            match spec {
                TypeSpec::Void => is_void = true,
                TypeSpec::Char => is_char = true,
                TypeSpec::Short => is_short = true,
                TypeSpec::Int => is_int = true,
                TypeSpec::Long => is_long += 1,
                TypeSpec::Float => is_float = true,
                TypeSpec::Double => is_double = true,
                TypeSpec::Signed => is_signed = true,
                TypeSpec::Unsigned => { is_signed = false; is_unsigned = true; }
                TypeSpec::Bool => return "bool".to_string(),
                TypeSpec::Struct(spec) => {
                    if let Some(n) = spec.name {
                        return self.interner.get(n).to_string();
                    }
                }
                TypeSpec::Union(spec) => {
                    if let Some(n) = spec.name {
                        return self.interner.get(n).to_string();
                    }
                }
                TypeSpec::Enum(spec) => {
                    if let Some(n) = spec.name {
                        return self.interner.get(n).to_string();
                    }
                }
                _ => {}
            }
        }

        if is_void {
            "c_void".to_string()
        } else if is_char {
            if is_unsigned { "c_uchar" } else { "c_char" }.to_string()
        } else if is_float {
            "c_float".to_string()
        } else if is_double {
            if is_long > 0 { "c_double" } else { "c_double" }.to_string()
        } else if is_short {
            if is_unsigned { "c_ushort" } else { "c_short" }.to_string()
        } else if is_long >= 2 {
            if is_unsigned { "c_ulonglong" } else { "c_longlong" }.to_string()
        } else if is_long == 1 {
            if is_unsigned { "c_ulong" } else { "c_long" }.to_string()
        } else if is_int || is_unsigned {
            if is_unsigned { "c_uint" } else { "c_int" }.to_string()
        } else {
            "c_int".to_string()
        }
    }

    /// 派生型を型に適用
    fn apply_derived_to_type(&self, base: &str, derived: &[DerivedDecl]) -> String {
        let mut result = base.to_string();
        for d in derived.iter().rev() {
            match d {
                DerivedDecl::Pointer(quals) => {
                    if quals.is_const {
                        result = format!("*const {}", result);
                    } else {
                        result = format!("*mut {}", result);
                    }
                }
                DerivedDecl::Array(arr) => {
                    if let Some(ref size_expr) = arr.size {
                        // 定数サイズ配列
                        if let ExprKind::IntLit(n) = &size_expr.kind {
                            result = format!("[{}; {}]", result, n);
                        } else {
                            result = format!("*mut {}", result);
                        }
                    } else {
                        result = format!("*mut {}", result);
                    }
                }
                DerivedDecl::Function(_) => {
                    // 関数ポインタは複雑なので簡易実装
                    result = format!("/* fn */ {}", result);
                }
            }
        }
        result
    }

    /// CompoundStmt を出力
    fn generate_compound_stmt(&mut self, stmt: &CompoundStmt, indent: &str) -> io::Result<()> {
        for item in &stmt.items {
            match item {
                BlockItem::Decl(decl) => {
                    writeln!(self.writer, "{}// local decl: {:?}", indent, decl.specs)?;
                }
                BlockItem::Stmt(s) => {
                    let rust_stmt = self.stmt_to_rust_inline(s, indent);
                    writeln!(self.writer, "{}", rust_stmt)?;
                }
            }
        }
        Ok(())
    }

    /// 文を Rust コードに変換（インライン関数用）
    fn stmt_to_rust_inline(&self, stmt: &Stmt, indent: &str) -> String {
        match stmt {
            Stmt::Expr(Some(expr), _) => {
                format!("{}{};", indent, self.expr_to_rust_inline(expr))
            }
            Stmt::Expr(None, _) => format!("{};", indent),
            Stmt::Return(Some(expr), _) => {
                format!("{}return {};", indent, self.expr_to_rust_inline(expr))
            }
            Stmt::Return(None, _) => format!("{}return;", indent),
            Stmt::If { cond, then_stmt, else_stmt, .. } => {
                let cond_str = self.expr_to_rust_inline(cond);
                let mut result = format!("{}if {} != 0 {{\n", indent, cond_str);
                let nested_indent = format!("{}    ", indent);
                result.push_str(&self.stmt_to_rust_inline(then_stmt, &nested_indent));
                result.push_str("\n");
                result.push_str(&format!("{}}}", indent));
                if let Some(else_stmt) = else_stmt {
                    result.push_str(" else {\n");
                    result.push_str(&self.stmt_to_rust_inline(else_stmt, &nested_indent));
                    result.push_str("\n");
                    result.push_str(&format!("{}}}", indent));
                }
                result
            }
            Stmt::Compound(compound) => {
                let mut result = format!("{}{{\n", indent);
                for item in &compound.items {
                    match item {
                        BlockItem::Stmt(s) => {
                            let nested_indent = format!("{}    ", indent);
                            result.push_str(&self.stmt_to_rust_inline(s, &nested_indent));
                            result.push_str("\n");
                        }
                        BlockItem::Decl(_) => {
                            result.push_str(&format!("{}    // local decl\n", indent));
                        }
                    }
                }
                result.push_str(&format!("{}}}", indent));
                result
            }
            _ => format!("{}/* TODO: {:?} */", indent, std::mem::discriminant(stmt))
        }
    }

    /// 式を Rust コードに変換（インライン関数用）
    fn expr_to_rust_inline(&self, expr: &Expr) -> String {
        match &expr.kind {
            ExprKind::Ident(name) => {
                self.interner.get(*name).to_string()
            }
            ExprKind::IntLit(n) => format!("{}", n),
            ExprKind::UIntLit(n) => format!("{}u64", n),
            ExprKind::FloatLit(f) => format!("{}", f),
            ExprKind::CharLit(c) => format!("'{}'", escape_char(*c)),
            ExprKind::StringLit(s) => format!("c\"{}\"", escape_string(s)),
            ExprKind::Binary { op, lhs, rhs } => {
                let l = self.expr_to_rust_inline(lhs);
                let r = self.expr_to_rust_inline(rhs);
                format!("({} {} {})", l, bin_op_to_rust(*op), r)
            }
            ExprKind::Call { func, args } => {
                let f = self.expr_to_rust_inline(func);
                let a: Vec<_> = args.iter()
                    .map(|a| self.expr_to_rust_inline(a))
                    .collect();
                format!("{}({})", f, a.join(", "))
            }
            ExprKind::Member { expr: base, member } => {
                let e = self.expr_to_rust_inline(base);
                let m = self.interner.get(*member);
                format!("({}).{}", e, m)
            }
            ExprKind::PtrMember { expr: base, member } => {
                let e = self.expr_to_rust_inline(base);
                let m = self.interner.get(*member);
                format!("(*{}).{}", e, m)
            }
            ExprKind::Index { expr: base, index } => {
                let b = self.expr_to_rust_inline(base);
                let i = self.expr_to_rust_inline(index);
                format!("(*{}.offset({} as isize))", b, i)
            }
            ExprKind::Cast { type_name, expr: inner } => {
                let e = self.expr_to_rust_inline(inner);
                let t = self.type_name_to_rust(type_name);
                format!("({} as {})", e, t)
            }
            ExprKind::Deref(inner) => {
                let e = self.expr_to_rust_inline(inner);
                format!("(*{})", e)
            }
            ExprKind::AddrOf(inner) => {
                let e = self.expr_to_rust_inline(inner);
                format!("(&mut {})", e)
            }
            ExprKind::PreInc(inner) => {
                let e = self.expr_to_rust_inline(inner);
                format!("{{ {} += 1; {} }}", e, e)
            }
            ExprKind::PreDec(inner) => {
                let e = self.expr_to_rust_inline(inner);
                format!("{{ {} -= 1; {} }}", e, e)
            }
            ExprKind::PostInc(inner) => {
                let e = self.expr_to_rust_inline(inner);
                format!("{{ let _t = {}; {} += 1; _t }}", e, e)
            }
            ExprKind::PostDec(inner) => {
                let e = self.expr_to_rust_inline(inner);
                format!("{{ let _t = {}; {} -= 1; _t }}", e, e)
            }
            ExprKind::UnaryPlus(inner) => self.expr_to_rust_inline(inner),
            ExprKind::UnaryMinus(inner) => {
                let e = self.expr_to_rust_inline(inner);
                format!("(-{})", e)
            }
            ExprKind::BitNot(inner) => {
                let e = self.expr_to_rust_inline(inner);
                format!("(!{})", e)
            }
            ExprKind::LogNot(inner) => {
                let e = self.expr_to_rust_inline(inner);
                format!("(if {} {{ 0 }} else {{ 1 }})", e)
            }
            ExprKind::Sizeof(inner) => {
                let e = self.expr_to_rust_inline(inner);
                format!("std::mem::size_of_val(&{})", e)
            }
            ExprKind::SizeofType(type_name) => {
                let t = self.type_name_to_rust(type_name);
                format!("std::mem::size_of::<{}>()", t)
            }
            ExprKind::Conditional { cond, then_expr, else_expr } => {
                let c = self.expr_to_rust_inline(cond);
                let t = self.expr_to_rust_inline(then_expr);
                let e = self.expr_to_rust_inline(else_expr);
                format!("(if {} != 0 {{ {} }} else {{ {} }})", c, t, e)
            }
            ExprKind::Comma { lhs, rhs } => {
                let l = self.expr_to_rust_inline(lhs);
                let r = self.expr_to_rust_inline(rhs);
                format!("{{ {}; {} }}", l, r)
            }
            ExprKind::Assign { op, lhs, rhs } => {
                let l = self.expr_to_rust_inline(lhs);
                let r = self.expr_to_rust_inline(rhs);
                match op {
                    AssignOp::Assign => format!("{{ {} = {}; {} }}", l, r, l),
                    _ => format!("{{ {} {} {}; {} }}", l, assign_op_to_rust(*op), r, l),
                }
            }
            ExprKind::Assert { kind, condition } => {
                let cond = self.expr_to_rust_inline(condition);
                let assert_expr = if is_boolean_expr_kind(&condition.kind) {
                    format!("assert!({})", cond)
                } else {
                    format!("assert!(({}) != 0)", cond)
                };
                match kind {
                    AssertKind::Assert => assert_expr,
                    AssertKind::AssertUnderscore => format!("{{ {}; }}", assert_expr),
                }
            }
            _ => format!("/* TODO: {:?} */", std::mem::discriminant(&expr.kind))
        }
    }

    /// マクロセクションを生成
    pub fn generate_macros(&mut self, result: &InferResult) -> io::Result<()> {
        writeln!(self.writer, "// =============================================================================")?;
        writeln!(self.writer, "// Macro Functions")?;
        writeln!(self.writer, "// =============================================================================")?;
        writeln!(self.writer)?;

        // 対象マクロを収集して名前順にソート
        let mut macros: Vec<_> = result.infer_ctx.macros.iter()
            .filter(|(_, info)| self.should_include_macro(info))
            .collect();
        macros.sort_by_key(|(name, _)| self.interner.get(**name));

        for (name, info) in macros {
            let status = self.get_macro_status(info);
            match status {
                GenerateStatus::Success => {
                    // 新しい RustCodegen を使ってマクロを生成
                    let codegen = RustCodegen::new(self.interner);
                    let generated = codegen.generate_macro(info);

                    if generated.is_complete() {
                        // 完全な生成：そのまま出力
                        write!(self.writer, "{}", generated.code)?;
                        self.stats.macros_success += 1;
                    } else {
                        // 不完全な生成：コメントアウトして出力
                        let name_str = self.interner.get(info.name);
                        let thx_info = if info.is_thx_dependent { " [THX]" } else { "" };
                        writeln!(self.writer, "// [CODEGEN_INCOMPLETE] {}{} - macro function", name_str, thx_info)?;
                        for line in generated.code.lines() {
                            writeln!(self.writer, "// {}", line)?;
                        }
                        writeln!(self.writer)?;
                        self.stats.macros_type_incomplete += 1;
                    }
                }
                GenerateStatus::ParseFailed => {
                    self.generate_macro_parse_failed(info)?;
                    self.stats.macros_parse_failed += 1;
                }
                GenerateStatus::TypeIncomplete => {
                    self.generate_macro_type_incomplete(info, result)?;
                    self.stats.macros_type_incomplete += 1;
                }
                GenerateStatus::Skip => {
                    // 何もしない
                    let _ = name;
                }
            }
        }

        Ok(())
    }

    /// マクロを出力対象にするかどうか
    fn should_include_macro(&self, info: &MacroInferInfo) -> bool {
        // ターゲットでなければスキップ
        if !info.is_target {
            return false;
        }

        // 本体がなければスキップ
        if !info.has_body {
            return false;
        }

        // 関数形式マクロまたは THX 依存オブジェクトマクロ
        info.is_function || info.is_thx_dependent
    }

    /// マクロの生成ステータスを判定
    fn get_macro_status(&self, info: &MacroInferInfo) -> GenerateStatus {
        match &info.parse_result {
            ParseResult::Unparseable(_) => GenerateStatus::ParseFailed,
            ParseResult::Expression(_) | ParseResult::Statement(_) => {
                if info.is_fully_confirmed() {
                    GenerateStatus::Success
                } else {
                    GenerateStatus::TypeIncomplete
                }
            }
        }
    }

    /// パラメータリストを構築（型情報付き）
    fn build_param_list(&self, info: &MacroInferInfo) -> String {
        info.params.iter()
            .map(|p| {
                let name = self.interner.get(p.name);
                let ty = self.get_param_type(p, info);
                format!("{}: {}", name, ty)
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// パラメータの型を取得
    fn get_param_type(&self, param: &MacroParam, info: &MacroInferInfo) -> String {
        let param_name = param.name;

        // 方法1: パラメータを参照する式の型制約から取得（逆引き辞書を使用）
        if let Some(expr_ids) = info.type_env.param_to_exprs.get(&param_name) {
            for expr_id in expr_ids {
                if let Some(constraints) = info.type_env.expr_constraints.get(expr_id) {
                    if let Some(first) = constraints.first() {
                        return self.type_repr_to_rust(&first.ty);
                    }
                }
            }
        }

        // 方法2: 従来の方法（MacroParam の ExprId）- フォールバック
        let expr_id = param.expr_id();
        if let Some(constraints) = info.type_env.expr_constraints.get(&expr_id) {
            if let Some(first) = constraints.first() {
                return self.type_repr_to_rust(&first.ty);
            }
        }

        "/* unknown */".to_string()
    }

    /// 戻り値の型を取得
    fn get_return_type(&self, info: &MacroInferInfo) -> String {
        match &info.parse_result {
            ParseResult::Expression(expr) => {
                if let Some(constraints) = info.type_env.expr_constraints.get(&expr.id) {
                    if let Some(first) = constraints.first() {
                        return self.type_repr_to_rust(&first.ty);
                    }
                }
                "/* unknown */".to_string()
            }
            ParseResult::Statement(_) => "()".to_string(),
            ParseResult::Unparseable(_) => "()".to_string(),
        }
    }

    /// TypeRepr を Rust 型文字列に変換
    fn type_repr_to_rust(&self, ty: &crate::type_repr::TypeRepr) -> String {
        ty.to_rust_string(self.interner)
    }

    /// 式を Rust コードに変換
    fn expr_to_rust(&self, expr: &Expr, info: &MacroInferInfo) -> String {
        match &expr.kind {
            ExprKind::Ident(name) => {
                self.interner.get(*name).to_string()
            }
            ExprKind::IntLit(n) => {
                format!("{}", n)
            }
            ExprKind::UIntLit(n) => {
                format!("{}u64", n)
            }
            ExprKind::FloatLit(f) => {
                format!("{}", f)
            }
            ExprKind::CharLit(c) => {
                format!("'{}'", escape_char(*c))
            }
            ExprKind::StringLit(s) => {
                format!("c\"{}\"", escape_string(s))
            }
            ExprKind::Binary { op, lhs, rhs } => {
                let l = self.expr_to_rust(lhs, info);
                let r = self.expr_to_rust(rhs, info);
                format!("({} {} {})", l, bin_op_to_rust(*op), r)
            }
            ExprKind::Call { func, args } => {
                let f = self.expr_to_rust(func, info);
                let a: Vec<_> = args.iter()
                    .map(|a| self.expr_to_rust(a, info))
                    .collect();
                format!("{}({})", f, a.join(", "))
            }
            ExprKind::Member { expr: base, member } => {
                let e = self.expr_to_rust(base, info);
                let m = self.interner.get(*member);
                format!("({}).{}", e, m)
            }
            ExprKind::PtrMember { expr: base, member } => {
                let e = self.expr_to_rust(base, info);
                let m = self.interner.get(*member);
                format!("(*{}).{}", e, m)
            }
            ExprKind::Index { expr: base, index } => {
                let b = self.expr_to_rust(base, info);
                let i = self.expr_to_rust(index, info);
                format!("(*{}.offset({} as isize))", b, i)
            }
            ExprKind::Cast { type_name, expr: inner } => {
                let e = self.expr_to_rust(inner, info);
                let t = self.type_name_to_rust(type_name);
                format!("({} as {})", e, t)
            }
            ExprKind::Deref(inner) => {
                let e = self.expr_to_rust(inner, info);
                format!("(*{})", e)
            }
            ExprKind::AddrOf(inner) => {
                let e = self.expr_to_rust(inner, info);
                format!("(&mut {})", e)
            }
            ExprKind::PreInc(inner) => {
                let e = self.expr_to_rust(inner, info);
                format!("{{ {} += 1; {} }}", e, e)
            }
            ExprKind::PreDec(inner) => {
                let e = self.expr_to_rust(inner, info);
                format!("{{ {} -= 1; {} }}", e, e)
            }
            ExprKind::PostInc(inner) => {
                let e = self.expr_to_rust(inner, info);
                format!("{{ let _t = {}; {} += 1; _t }}", e, e)
            }
            ExprKind::PostDec(inner) => {
                let e = self.expr_to_rust(inner, info);
                format!("{{ let _t = {}; {} -= 1; _t }}", e, e)
            }
            ExprKind::UnaryPlus(inner) => {
                self.expr_to_rust(inner, info)
            }
            ExprKind::UnaryMinus(inner) => {
                let e = self.expr_to_rust(inner, info);
                format!("(-{})", e)
            }
            ExprKind::BitNot(inner) => {
                let e = self.expr_to_rust(inner, info);
                format!("(!{})", e)
            }
            ExprKind::LogNot(inner) => {
                let e = self.expr_to_rust(inner, info);
                format!("(if {} {{ 0 }} else {{ 1 }})", e)
            }
            ExprKind::Sizeof(inner) => {
                let e = self.expr_to_rust(inner, info);
                format!("std::mem::size_of_val(&{})", e)
            }
            ExprKind::SizeofType(type_name) => {
                let t = self.type_name_to_rust(type_name);
                format!("std::mem::size_of::<{}>()", t)
            }
            ExprKind::Conditional { cond, then_expr, else_expr } => {
                let c = self.expr_to_rust(cond, info);
                let t = self.expr_to_rust(then_expr, info);
                let e = self.expr_to_rust(else_expr, info);
                format!("(if {} != 0 {{ {} }} else {{ {} }})", c, t, e)
            }
            ExprKind::Comma { lhs, rhs } => {
                let l = self.expr_to_rust(lhs, info);
                let r = self.expr_to_rust(rhs, info);
                format!("{{ {}; {} }}", l, r)
            }
            ExprKind::Assign { op, lhs, rhs } => {
                let l = self.expr_to_rust(lhs, info);
                let r = self.expr_to_rust(rhs, info);
                match op {
                    AssignOp::Assign => format!("{{ {} = {}; {} }}", l, r, l),
                    _ => format!("{{ {} {} {}; {} }}", l, assign_op_to_rust(*op), r, l),
                }
            }
            _ => {
                format!("/* TODO: {:?} */", std::mem::discriminant(&expr.kind))
            }
        }
    }

    /// 文を Rust コードに変換
    fn stmt_to_rust(&self, stmt: &Stmt, info: &MacroInferInfo) -> String {
        match stmt {
            Stmt::Expr(Some(expr), _) => {
                format!("{};", self.expr_to_rust(expr, info))
            }
            Stmt::Expr(None, _) => ";".to_string(),
            Stmt::Return(Some(expr), _) => {
                format!("return {};", self.expr_to_rust(expr, info))
            }
            Stmt::Return(None, _) => "return;".to_string(),
            _ => format!("/* TODO: stmt */")
        }
    }

    /// TypeName を Rust 型文字列に変換
    fn type_name_to_rust(&self, type_name: &crate::ast::TypeName) -> String {
        // 簡易実装: typedef 名があればそれを使用
        for spec in &type_name.specs.type_specs {
            if let crate::ast::TypeSpec::TypedefName(name) = spec {
                return self.interner.get(*name).to_string();
            }
        }
        "/* type */".to_string()
    }

    /// パース失敗マクロをコメント出力
    fn generate_macro_parse_failed(&mut self, info: &MacroInferInfo) -> io::Result<()> {
        let name_str = self.interner.get(info.name);

        // パラメータリストを構築
        let params_str = if info.is_function {
            let params: Vec<_> = info.params.iter()
                .map(|p| self.interner.get(p.name).to_string())
                .collect();
            format!("({})", params.join(", "))
        } else {
            String::new()
        };

        writeln!(self.writer, "// [PARSE_FAILED] {}{}", name_str, params_str)?;

        // エラーメッセージがあれば出力
        if let ParseResult::Unparseable(Some(err_msg)) = &info.parse_result {
            writeln!(self.writer, "// Error: {}", err_msg)?;
        }

        // TODO: トークン列の S 式出力（Phase 2 で詳細実装）
        writeln!(self.writer, "// (tokens not available in parsed form)")?;
        writeln!(self.writer)?;

        Ok(())
    }

    /// 型推論失敗マクロをコメント出力
    fn generate_macro_type_incomplete(&mut self, info: &MacroInferInfo, result: &InferResult) -> io::Result<()> {
        let name_str = self.interner.get(info.name);

        // パラメータリストを構築
        let params_str = if info.is_function {
            let params: Vec<_> = info.params.iter()
                .map(|p| self.interner.get(p.name).to_string())
                .collect();
            format!("({})", params.join(", "))
        } else {
            String::new()
        };

        writeln!(self.writer, "// [TYPE_INCOMPLETE] {}{}", name_str, params_str)?;

        // 型推論状態を出力
        writeln!(self.writer, "// Args status: {:?}, Return status: {:?}",
            info.args_infer_status, info.return_infer_status)?;

        // 型付 S 式を出力
        writeln!(self.writer, "// Typed S-expression:")?;
        self.write_typed_sexp_comment(info, result)?;

        writeln!(self.writer)?;
        Ok(())
    }

    /// 型付 S 式をコメントとして出力
    fn write_typed_sexp_comment(&mut self, info: &MacroInferInfo, _result: &InferResult) -> io::Result<()> {
        match &info.parse_result {
            ParseResult::Expression(expr) => {
                self.write_expr_sexp_comment(expr, info, "//   ")?;
            }
            ParseResult::Statement(block_items) => {
                for item in block_items {
                    if let BlockItem::Stmt(stmt) = item {
                        self.write_stmt_sexp_comment(stmt, info, "//   ")?;
                    }
                }
            }
            ParseResult::Unparseable(_) => {
                writeln!(self.writer, "//   (unparseable)")?;
            }
        }
        Ok(())
    }

    /// 式の S 式をコメントとして出力
    fn write_expr_sexp_comment(&mut self, expr: &Expr, info: &MacroInferInfo, prefix: &str) -> io::Result<()> {
        // 簡易的な S 式出力
        let mut buf = Vec::new();
        {
            let mut printer = SexpPrinter::new(&mut buf, self.interner);
            let _ = printer.print_expr(expr);
        }

        // 型情報を追加
        let sexp_str = String::from_utf8_lossy(&buf);
        let type_info = self.get_expr_type_info(expr, info);

        // 各行にプレフィックスを付けて出力
        for line in sexp_str.lines() {
            writeln!(self.writer, "{}{}", prefix, line)?;
        }
        if !type_info.is_empty() {
            writeln!(self.writer, "{} :type {}", prefix, type_info)?;
        }

        Ok(())
    }

    /// 文の S 式をコメントとして出力
    fn write_stmt_sexp_comment(&mut self, stmt: &Stmt, _info: &MacroInferInfo, prefix: &str) -> io::Result<()> {
        let mut buf = Vec::new();
        {
            let mut printer = SexpPrinter::new(&mut buf, self.interner);
            let _ = printer.print_stmt(stmt);
        }

        let sexp_str = String::from_utf8_lossy(&buf);
        for line in sexp_str.lines() {
            writeln!(self.writer, "{}{}", prefix, line)?;
        }

        Ok(())
    }

    /// 式の型情報を取得
    fn get_expr_type_info(&self, expr: &Expr, info: &MacroInferInfo) -> String {
        // TypeEnv から型制約を取得
        if let Some(constraints) = info.type_env.expr_constraints.get(&expr.id) {
            if let Some(first) = constraints.first() {
                return first.ty.to_display_string(self.interner);
            }
        }
        "<unknown>".to_string()
    }
}
