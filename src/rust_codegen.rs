//! Rust コード生成モジュール
//!
//! 型推論結果から Rust コードを生成する。

use std::io::{self, Write};

use crate::ast::{AssertKind, AssignOp, BinOp, BlockItem, CompoundStmt, Declaration, DeclSpecs, DerivedDecl, Expr, ExprKind, ForInit, FunctionDef, Initializer, ParamDecl, Stmt, TypeSpec};
use crate::enum_dict::EnumDict;
use crate::infer_api::InferResult;
use crate::intern::StringInterner;
use crate::macro_infer::{MacroInferContext, MacroInferInfo, MacroParam, ParseResult};
use crate::sexp::SexpPrinter;

/// Rust の予約語リスト（strict keywords + reserved keywords）
const RUST_KEYWORDS: &[&str] = &[
    // Strict keywords
    "as", "async", "await", "break", "const", "continue", "crate", "dyn",
    "else", "enum", "extern", "false", "fn", "for", "if", "impl", "in",
    "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return",
    "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while",
    // Reserved keywords
    "abstract", "become", "box", "do", "final", "gen", "macro", "override",
    "priv", "try", "typeof", "unsized", "virtual", "yield",
];

/// Rust の予約語をエスケープ（必要なら r# を付ける）
fn escape_rust_keyword(name: &str) -> String {
    if RUST_KEYWORDS.contains(&name) {
        format!("r#{}", name)
    } else {
        name.to_string()
    }
}

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

/// 式がゼロ定数かどうかを判定
fn is_zero_constant(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::IntLit(0) => true,
        ExprKind::UIntLit(0) => true,
        _ => false,
    }
}

/// 式が bool として扱える形式かどうかを判定
///
/// キャスト `(expr as bool)` を含む場合も true を返す
fn is_boolean_expr(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Binary { op, .. } => matches!(op,
            BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge |
            BinOp::Eq | BinOp::Ne | BinOp::LogAnd | BinOp::LogOr
        ),
        // (expr as bool) も bool を返す
        ExprKind::Cast { type_name, .. } => {
            // TypeSpec が Bool かチェック
            type_name.specs.type_specs.iter().any(|ts| {
                matches!(ts, TypeSpec::Bool)
            })
        }
        // LogNot は常に bool を返す（if 式として生成される）
        ExprKind::LogNot(_) => true,
        _ => false,
    }
}


/// 式を bool 条件に変換するためのラッパー文字列を生成
///
/// 既に bool を返す式なら expr_str をそのまま返す。
/// そうでなければ `((expr_str) != 0)` を返す。
fn wrap_as_bool_condition(expr: &Expr, expr_str: &str) -> String {
    // AST から bool 判定
    if is_boolean_expr(expr) {
        return expr_str.to_string();
    }
    // 生成された文字列から bool キャストを検出（TypedefName("bool") 対応）
    // パターン: "... as bool)" で終わる場合
    if expr_str.ends_with(" as bool)") {
        return expr_str.to_string();
    }
    format!("(({}) != 0)", expr_str)
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
    /// ヘッダーに出力する use 文
    /// 空の場合はデフォルトの use 文を出力
    pub use_statements: Vec<String>,
}

impl Default for CodegenConfig {
    fn default() -> Self {
        Self {
            emit_inline_fns: true,
            emit_macros: true,
            include_source_location: true,
            use_statements: Vec::new(),
        }
    }
}

impl CodegenConfig {
    /// デフォルトの use 文を取得
    ///
    /// 生成コードで使用される C 型をインポートする。
    /// `size_t` などは Rust 組み込み型のエイリアスとして定義。
    pub fn default_use_statements() -> Vec<String> {
        vec![
            "use std::ffi::{c_void, c_char, c_int, c_uint, c_long, c_ulong, c_short, c_ushort}".to_string(),
            "#[allow(non_camel_case_types)] type size_t = usize".to_string(),
            "#[allow(non_camel_case_types)] type ssize_t = isize".to_string(),
            "#[allow(non_camel_case_types)] type SSize_t = isize".to_string(),
        ]
    }

    /// use 文を設定
    pub fn with_use_statements(mut self, statements: Vec<String>) -> Self {
        self.use_statements = statements;
        self
    }

    /// use 文を追加
    pub fn add_use_statement(mut self, statement: impl Into<String>) -> Self {
        self.use_statements.push(statement.into());
        self
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
    /// 利用不可関数を呼び出す（コメント出力）
    CallsUnavailable,
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
    /// 利用不可関数呼び出しマクロ数
    pub macros_calls_unavailable: usize,
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
    /// Enum バリアント辞書（パターンマッチ用）
    enum_dict: &'a EnumDict,
    /// マクロ推論コンテキスト（THX マクロ呼び出し判定用）
    macro_ctx: &'a MacroInferContext,
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
    /// Enum バリアント辞書（パターンマッチ用）
    enum_dict: &'a EnumDict,
    /// マクロ推論コンテキスト（THX マクロ呼び出し判定用）
    macro_ctx: &'a MacroInferContext,
    config: CodegenConfig,
    stats: CodegenStats,
}

impl<'a> RustCodegen<'a> {
    /// 新しい単一関数用コード生成器を作成
    pub fn new(
        interner: &'a StringInterner,
        enum_dict: &'a EnumDict,
        macro_ctx: &'a MacroInferContext,
    ) -> Self {
        Self {
            interner,
            enum_dict,
            macro_ctx,
            buffer: String::new(),
            incomplete_count: 0,
        }
    }

    /// 呼び出し先が THX マクロで、my_perl が不足しているかチェック
    ///
    /// 以下の条件を満たす場合に true を返す：
    /// 1. 呼び出し先が MacroInferContext.macros に存在する
    /// 2. その MacroInferInfo.is_thx_dependent が true
    /// 3. 実引数が仮引数より1つ少ない（my_perl が不足）
    fn needs_my_perl_for_call(&self, func_name: crate::InternedStr, actual_arg_count: usize) -> bool {
        if let Some(callee_info) = self.macro_ctx.macros.get(&func_name) {
            if callee_info.is_thx_dependent {
                // THX マクロの期待引数数 = params.len() + 1 (my_perl)
                let expected_count = callee_info.params.len() + 1;
                // 実引数が1つ少ない場合、my_perl が必要
                return actual_arg_count + 1 == expected_count;
            }
        }
        false
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

        // ジェネリック句を生成
        let generic_clause = self.build_generic_clause(info);

        // パラメータリストを構築（型情報付き）
        // type/cast パラメータは値引数ではないので除外
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
        let generic_info = if !generic_clause.is_empty() { " [generic]" } else { "" };
        self.writeln(&format!("/// {}{}{} - macro function", name_str, thx_info, generic_info));
        self.writeln("#[inline]");

        // 関数定義（ジェネリック句付き）
        self.writeln(&format!("pub unsafe fn {}{}({}) -> {} {{", name_str, generic_clause, params_str, return_type));

        // unsafe 操作（関数呼び出し or デリファレンス）を含む場合のみ unsafe ブロックを生成
        let needs_unsafe = info.has_unsafe_ops();
        let body_indent = if needs_unsafe { "        " } else { "    " };

        if needs_unsafe {
            self.writeln("    unsafe {");
        }

        match &info.parse_result {
            ParseResult::Expression(expr) => {
                let rust_expr = self.expr_to_rust(expr, info);
                self.writeln(&format!("{}{}", body_indent, rust_expr));
            }
            ParseResult::Statement(block_items) => {
                for item in block_items {
                    if let BlockItem::Stmt(stmt) = item {
                        let rust_stmt = self.stmt_to_rust(stmt, info);
                        self.writeln(&format!("{}{}", body_indent, rust_stmt));
                    }
                }
            }
            ParseResult::Unparseable(_) => {
                self.writeln(&format!("{}unimplemented!()", body_indent));
            }
        }

        if needs_unsafe {
            self.writeln("    }");
        }

        self.writeln("}");
        self.writeln("");

        self.into_generated_code()
    }

    /// ジェネリック句を生成（例: "<T>" or "<T, U>"）
    fn build_generic_clause(&self, info: &MacroInferInfo) -> String {
        if info.generic_type_params.is_empty() {
            return String::new();
        }

        // 型パラメータを収集（重複排除、ソート）
        let mut params: Vec<&String> = info.generic_type_params.values().collect();
        params.sort();
        params.dedup();

        format!("<{}>", params.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "))
    }

    /// パラメータリストを構築（型情報付き）
    /// type/cast パラメータは型パラメータなので値引数からは除外する
    fn build_param_list(&mut self, info: &MacroInferInfo) -> String {
        info.params.iter()
            .enumerate()
            .filter(|(i, _)| {
                // type/cast パラメータは値引数ではないので除外
                !info.generic_type_params.contains_key(&(*i as i32))
            })
            .map(|(i, p)| {
                let name = escape_rust_keyword(self.interner.get(p.name));
                let ty = self.get_param_type(p, info, i);
                format!("{}: {}", name, ty)
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// パラメータの型を取得
    fn get_param_type(&mut self, param: &MacroParam, info: &MacroInferInfo, param_index: usize) -> String {
        // ジェネリック型パラメータかチェック
        if let Some(generic_name) = info.generic_type_params.get(&(param_index as i32)) {
            return generic_name.clone();
        }

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
    ///
    /// ジェネリック戻り値型を優先し、なければ
    /// MacroInferInfo::get_return_type() を使用して、
    /// return_constraints（apidoc由来）を expr_constraints より優先する
    fn get_return_type(&mut self, info: &MacroInferInfo) -> String {
        // ジェネリック戻り値型かチェック（-1 = return type）
        if let Some(generic_name) = info.generic_type_params.get(&-1) {
            return generic_name.clone();
        }

        match &info.parse_result {
            ParseResult::Expression(_) => {
                if let Some(ty) = info.get_return_type() {
                    return self.type_repr_to_rust(ty);
                }
                self.unknown_marker().to_string()
            }
            ParseResult::Statement(_) => "()".to_string(),
            ParseResult::Unparseable(_) => "()".to_string(),
        }
    }

    /// TypeRepr を Rust 型文字列に変換
    ///
    /// 戻り値に `/*` が含まれていたら不完全型としてカウントする
    fn type_repr_to_rust(&mut self, ty: &crate::type_repr::TypeRepr) -> String {
        let result = ty.to_rust_string(self.interner);
        if result.contains("/*") {
            self.incomplete_count += 1;
        }
        result
    }

    /// MUTABLE_PTR パターンを検出
    ///
    /// `({ void *p_ = (expr); p_; })` のような構造を検出し、
    /// 初期化子の式を返す。
    fn detect_mutable_ptr_pattern<'b>(&self, compound: &'b CompoundStmt) -> Option<&'b Expr> {
        // 2つの要素: 宣言 + 式文
        if compound.items.len() != 2 {
            return None;
        }

        // 最初の要素: 宣言
        let decl = match &compound.items[0] {
            BlockItem::Decl(d) => d,
            _ => return None,
        };

        // 宣言子が1つで、初期化子がある
        if decl.declarators.len() != 1 {
            return None;
        }
        let init_decl = &decl.declarators[0];
        let declared_name = init_decl.declarator.name?;
        let init = init_decl.init.as_ref()?;

        // 初期化子は式
        let init_expr = match init {
            Initializer::Expr(e) => e.as_ref(),
            _ => return None,
        };

        // 2番目の要素: 式文で、宣言した変数を参照
        let last_expr = match &compound.items[1] {
            BlockItem::Stmt(Stmt::Expr(Some(e), _)) => e,
            _ => return None,
        };

        // 最後の式が宣言した変数への参照
        if let ExprKind::Ident(name) = &last_expr.kind {
            if *name == declared_name {
                return Some(init_expr);
            }
        }

        None
    }

    /// 式を Rust コードに変換
    fn expr_to_rust(&mut self, expr: &Expr, info: &MacroInferInfo) -> String {
        match &expr.kind {
            ExprKind::Ident(name) => {
                escape_rust_keyword(self.interner.get(*name))
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
                // 論理演算子の場合、オペランドを bool に変換
                match op {
                    BinOp::LogAnd | BinOp::LogOr => {
                        let l_bool = wrap_as_bool_condition(lhs, &l);
                        let r_bool = wrap_as_bool_condition(rhs, &r);
                        format!("({} {} {})", l_bool, bin_op_to_rust(*op), r_bool)
                    }
                    _ => format!("({} {} {})", l, bin_op_to_rust(*op), r)
                }
            }
            ExprKind::Call { func, args } => {
                // __builtin_expect(cond, expected) -> cond
                // GCC の分岐予測ヒントは Rust では無視
                if let ExprKind::Ident(name) = &func.kind {
                    let func_name = self.interner.get(*name);
                    if func_name == "__builtin_expect" && args.len() >= 1 {
                        return self.expr_to_rust(&args[0], info);
                    }
                }
                let f = self.expr_to_rust(func, info);

                // THX マクロで my_perl が不足しているかチェック
                let needs_my_perl = if let ExprKind::Ident(name) = &func.kind {
                    self.needs_my_perl_for_call(*name, args.len())
                } else {
                    false
                };

                let mut a: Vec<String> = if needs_my_perl {
                    vec!["my_perl".to_string()]
                } else {
                    vec![]
                };
                a.extend(args.iter().map(|arg| self.expr_to_rust(arg, info)));
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
                // void キャストは式の値を捨てる（(expr as ()) は無効）
                if t == "()" {
                    format!("{{ {}; }}", e)
                } else {
                    format!("({} as {})", e, t)
                }
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
                // 内部式を bool に変換してから論理否定
                let cond = wrap_as_bool_condition(inner, &e);
                format!("(!{})", cond)
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
                // 条件が既に bool なら != 0 を追加しない
                let cond_str = wrap_as_bool_condition(cond, &c);
                format!("(if {} {{ {} }} else {{ {} }})", cond_str, t, e)
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
                let assert_expr = if is_boolean_expr(condition) {
                    format!("assert!({})", cond)
                } else {
                    format!("assert!(({}) != 0)", cond)
                };
                match kind {
                    AssertKind::Assert => assert_expr,
                    AssertKind::AssertUnderscore => format!("{{ {}; }}", assert_expr),
                }
            }
            ExprKind::StmtExpr(compound) => {
                // GCC Statement Expression: ({ decl; stmt; ...; expr })
                //
                // MUTABLE_PTR パターンを検出:
                // ({ void *p_ = (expr); p_; }) => expr
                if let Some(init_expr) = self.detect_mutable_ptr_pattern(compound) {
                    return self.expr_to_rust(init_expr, info);
                }

                // 通常の statement expression: Rust のブロック式として出力
                let mut parts = Vec::new();
                for item in &compound.items {
                    match item {
                        BlockItem::Stmt(Stmt::Expr(Some(e), _)) => {
                            parts.push(self.expr_to_rust(e, info));
                        }
                        BlockItem::Stmt(stmt) => {
                            parts.push(self.stmt_to_rust(stmt, info));
                        }
                        BlockItem::Decl(_) => {
                            // 宣言はスキップ
                        }
                    }
                }
                if parts.is_empty() {
                    "{ }".to_string()
                } else if parts.len() == 1 {
                    parts.pop().unwrap()
                } else {
                    let last = parts.pop().unwrap();
                    let stmts = parts.join("; ");
                    format!("{{ {}; {} }}", stmts, last)
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
        // decl_specs_to_rust でベース型を取得（プリミティブ型も正しく変換）
        let base_type = self.decl_specs_to_rust(&type_name.specs);

        // 宣言子からポインタ/配列/関数を適用
        let mut result = if let Some(ref decl) = type_name.declarator {
            self.apply_derived_to_type(&base_type, &decl.derived)
        } else {
            base_type
        };

        // C の const 修飾子（例: const char*）を Rust の *const に反映
        // 最も内側のポインタを *const にする
        if type_name.specs.qualifiers.is_const {
            if let Some(pos) = result.rfind("*mut ") {
                result.replace_range(pos..pos + 5, "*const ");
            }
        }

        result
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

    /// 派生型を型に適用（関数ポインタを含む完全な処理）
    fn apply_derived_to_type(&mut self, base: &str, derived: &[DerivedDecl]) -> String {
        // Function を探す
        let fn_idx = derived
            .iter()
            .position(|d| matches!(d, DerivedDecl::Function(_)));

        if let Some(idx) = fn_idx {
            if let DerivedDecl::Function(param_list) = &derived[idx] {
                // Function の直前が Pointer なら関数ポインタ
                let is_fn_pointer =
                    idx > 0 && matches!(derived[idx - 1], DerivedDecl::Pointer(_));

                // 戻り値型の派生（Function と fn ptr Pointer を除く）
                let return_end = if is_fn_pointer { idx - 1 } else { idx };
                let return_derived = &derived[..return_end];
                let return_type = self.apply_simple_derived(base, return_derived);

                // パラメータリストを生成（型のみ、名前なし）
                let params: Vec<_> = param_list
                    .params
                    .iter()
                    .map(|p| self.param_type_only(p))
                    .collect();
                let params_str = params.join(", ");

                // 関数型を生成
                let fn_type =
                    format!("unsafe extern \"C\" fn({}) -> {}", params_str, return_type);

                // 関数ポインタの場合は Option でラップ（NULL 許容）
                if is_fn_pointer {
                    return format!("Option<{}>", fn_type);
                }
                return fn_type;
            }
        }

        // 通常の型変換（Function を含まない場合）
        self.apply_simple_derived(base, derived)
    }

    /// 単純な派生型の適用（Pointer と Array のみ）
    fn apply_simple_derived(&self, base: &str, derived: &[DerivedDecl]) -> String {
        let mut result = base.to_string();
        for d in derived.iter().rev() {
            match d {
                DerivedDecl::Pointer(quals) => {
                    // void ポインタの場合は c_void を使用
                    if result == "()" {
                        result = "c_void".to_string();
                    }
                    if quals.is_const {
                        result = format!("*const {}", result);
                    } else {
                        result = format!("*mut {}", result);
                    }
                }
                DerivedDecl::Array(arr) => {
                    // void 配列の場合は c_void を使用
                    if result == "()" {
                        result = "c_void".to_string();
                    }
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
                    // この関数では Function は処理しない（apply_derived_to_type で処理）
                }
            }
        }
        result
    }

    /// ParamDecl から型のみを取得（名前なし）
    fn param_type_only(&mut self, param: &ParamDecl) -> String {
        let ty = self.decl_specs_to_rust(&param.specs);
        if let Some(ref declarator) = param.declarator {
            self.apply_derived_to_type(&ty, &declarator.derived)
        } else {
            ty
        }
    }

    /// inline 関数を生成（self を消費）
    pub fn generate_inline_fn(mut self, name: crate::InternedStr, func_def: &FunctionDef) -> GeneratedCode {
        let name_str = self.interner.get(name);

        // パラメータリストを取得
        let params_str = self.build_fn_param_list(&func_def.declarator.derived);

        // 戻り値の型を取得（基本型）
        let return_type = self.decl_specs_to_rust(&func_def.specs);

        // declarator の派生型（ポインタなど）を適用（Function を除く）
        // 例: HEK * func(...) の場合、derived = [Pointer, Function]
        //     戻り値型は HEK に Pointer を適用して *mut HEK になる
        let return_derived: Vec<_> = func_def.declarator.derived.iter()
            .filter(|d| !matches!(d, DerivedDecl::Function(_)))
            .cloned()
            .collect();
        let return_type = self.apply_derived_to_type(&return_type, &return_derived);

        // ドキュメントコメント
        self.writeln(&format!("/// {} - inline function", name_str));
        self.writeln("#[inline]");

        // 関数定義
        self.writeln(&format!("pub unsafe fn {}({}) -> {} {{", name_str, params_str, return_type));

        // unsafe 操作（関数呼び出し or デリファレンス）を含む場合のみ unsafe ブロックを生成
        let needs_unsafe = func_def.function_call_count > 0 || func_def.deref_count > 0;

        if needs_unsafe {
            self.writeln("    unsafe {");
            let body_str = self.compound_stmt_to_string(&func_def.body, "        ");
            self.buffer.push_str(&body_str);
            self.writeln("    }");
        } else {
            let body_str = self.compound_stmt_to_string(&func_def.body, "    ");
            self.buffer.push_str(&body_str);
        }

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
            .map(|n| escape_rust_keyword(self.interner.get(n)))
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

    /// Declaration を Rust の let 宣言に変換
    fn decl_to_rust_let(&mut self, decl: &Declaration, indent: &str) -> String {
        let mut result = String::new();

        // 基本型を取得
        let base_type = self.decl_specs_to_rust(&decl.specs);

        // 各宣言子を処理
        for init_decl in &decl.declarators {
            let name = init_decl.declarator.name
                .map(|n| escape_rust_keyword(self.interner.get(n)))
                .unwrap_or_else(|| "_".to_string());

            // 派生型（ポインタなど）を適用
            let ty = self.apply_derived_to_type(&base_type, &init_decl.declarator.derived);

            // 初期化子
            if let Some(ref init) = init_decl.init {
                match init {
                    Initializer::Expr(expr) => {
                        let init_expr = self.expr_to_rust_inline(expr);
                        result.push_str(&format!("{}let {}: {} = {};\n", indent, name, ty, init_expr));
                    }
                    Initializer::List(_) => {
                        // 初期化リストは複雑なので TODO
                        result.push_str(&format!("{}let {}: {} = /* init list */;\n", indent, name, ty));
                    }
                }
            } else {
                // 初期化子なし（未初期化変数 - Rust では unsafe かデフォルト値が必要）
                result.push_str(&format!("{}let {}: {}; // uninitialized\n", indent, name, ty));
            }
        }

        result
    }

    /// 複合文を文字列として生成
    fn compound_stmt_to_string(&mut self, stmt: &CompoundStmt, indent: &str) -> String {
        let mut result = String::new();
        for item in &stmt.items {
            match item {
                BlockItem::Decl(decl) => {
                    result.push_str(&self.decl_to_rust_let(decl, indent));
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
                // 代入式は値を返さない形式で出力
                if let ExprKind::Assign { op, lhs, rhs } = &expr.kind {
                    let l = self.expr_to_rust_inline(lhs);
                    let r = self.expr_to_rust_inline(rhs);
                    match op {
                        AssignOp::Assign => format!("{}{} = {};", indent, l, r),
                        _ => format!("{}{} {} {};", indent, l, assign_op_to_rust(*op), r),
                    }
                } else {
                    format!("{}{};", indent, self.expr_to_rust_inline(expr))
                }
            }
            Stmt::Expr(None, _) => format!("{};", indent),
            Stmt::Return(Some(expr), _) => {
                format!("{}return {};", indent, self.expr_to_rust_inline(expr))
            }
            Stmt::Return(None, _) => format!("{}return;", indent),
            Stmt::If { cond, then_stmt, else_stmt, .. } => {
                let cond_str = self.expr_to_rust_inline(cond);
                // 条件が既に bool なら != 0 を追加しない
                let cond_bool = wrap_as_bool_condition(cond, &cond_str);
                let mut result = format!("{}if {} {{\n", indent, cond_bool);
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
                        BlockItem::Decl(decl) => {
                            let nested_indent = format!("{}    ", indent);
                            result.push_str(&self.decl_to_rust_let(decl, &nested_indent));
                        }
                    }
                }
                result.push_str(&format!("{}}}", indent));
                result
            }
            Stmt::While { cond, body, .. } => {
                let cond_str = self.expr_to_rust_inline(cond);
                // 条件が既に bool なら != 0 を追加しない
                let cond_bool = wrap_as_bool_condition(cond, &cond_str);
                let mut result = format!("{}while {} {{\n", indent, cond_bool);
                let nested_indent = format!("{}    ", indent);
                result.push_str(&self.stmt_to_rust_inline(body, &nested_indent));
                result.push_str("\n");
                result.push_str(&format!("{}}}", indent));
                result
            }
            Stmt::For { init, cond, step, body, .. } => {
                let mut result = format!("{}{{\n", indent);
                let nested_indent = format!("{}    ", indent);

                // 初期化部分
                if let Some(for_init) = init {
                    match for_init {
                        ForInit::Expr(expr) => {
                            result.push_str(&format!("{}{};\n", nested_indent, self.expr_to_rust_inline(expr)));
                        }
                        ForInit::Decl(decl) => {
                            result.push_str(&self.decl_to_rust_let(decl, &nested_indent));
                        }
                    }
                }

                // ループ部分
                if let Some(cond_expr) = cond {
                    let cond_str = self.expr_to_rust_inline(cond_expr);
                    // 条件が既に bool なら != 0 を追加しない
                    let cond_bool = wrap_as_bool_condition(cond_expr, &cond_str);
                    result.push_str(&format!("{}while {} {{\n", nested_indent, cond_bool));
                } else {
                    result.push_str(&format!("{}loop {{\n", nested_indent));
                }

                let body_indent = format!("{}    ", nested_indent);

                // ループ本体
                result.push_str(&self.stmt_to_rust_inline(body, &body_indent));
                result.push_str("\n");

                // ステップ部分
                if let Some(step_expr) = step {
                    result.push_str(&format!("{}{};\n", body_indent, self.expr_to_rust_inline(step_expr)));
                }

                result.push_str(&format!("{}}}\n", nested_indent));
                result.push_str(&format!("{}}}", indent));
                result
            }
            Stmt::DoWhile { body, cond, .. } => {
                // do { ... } while (0) パターンは一度だけ実行される loop として出力
                // これにより内部の break; が正しく動作する
                if is_zero_constant(cond) {
                    let mut result = format!("{}loop {{\n", indent);
                    let nested_indent = format!("{}    ", indent);
                    result.push_str(&self.stmt_to_rust_inline(body, &nested_indent));
                    result.push_str("\n");
                    result.push_str(&format!("{}    break;\n", indent));
                    result.push_str(&format!("{}}}", indent));
                    return result;
                }

                // 一般的な do-while 文: loop { body; if !cond { break; } }
                let mut result = format!("{}loop {{\n", indent);
                let nested_indent = format!("{}    ", indent);
                result.push_str(&self.stmt_to_rust_inline(body, &nested_indent));
                result.push_str("\n");
                let cond_str = self.expr_to_rust_inline(cond);
                // bool 式なら !cond、そうでなければ cond == 0
                let break_cond = if is_boolean_expr(cond) {
                    format!("!{}", cond_str)
                } else {
                    format!("{} == 0", cond_str)
                };
                result.push_str(&format!("{}    if {} {{ break; }}\n", indent, break_cond));
                result.push_str(&format!("{}}}", indent));
                result
            }
            Stmt::Switch { expr, body, .. } => {
                let expr_str = self.expr_to_rust_inline(expr);
                let mut result = format!("{}match {} {{\n", indent, expr_str);
                let nested_indent = format!("{}    ", indent);

                // body から Case/Default を収集
                self.collect_switch_cases(body, &nested_indent, &mut result);

                result.push_str(&format!("{}}}", indent));
                result
            }
            Stmt::Case { expr: case_expr, stmt: case_stmt, .. } => {
                // Switch 外で Case が出現した場合（通常は Switch 内で処理される）
                let case_val = self.expr_to_rust_inline(case_expr);
                let mut result = format!("{}{} => {{\n", indent, case_val);
                let body_indent = format!("{}    ", indent);
                result.push_str(&self.stmt_to_rust_inline(case_stmt, &body_indent));
                result.push_str("\n");
                result.push_str(&format!("{}}}", indent));
                result
            }
            Stmt::Default { stmt: default_stmt, .. } => {
                let mut result = format!("{}_ => {{\n", indent);
                let body_indent = format!("{}    ", indent);
                result.push_str(&self.stmt_to_rust_inline(default_stmt, &body_indent));
                result.push_str("\n");
                result.push_str(&format!("{}}}", indent));
                result
            }
            Stmt::Goto(label, _) => {
                let label_str = self.interner.get(*label);
                format!("{}break '{}; // goto", indent, label_str)
            }
            Stmt::Label { name, stmt: label_stmt, .. } => {
                let label_str = self.interner.get(*name);
                let mut result = format!("{}'{}: {{\n", indent, label_str);
                let nested_indent = format!("{}    ", indent);
                result.push_str(&self.stmt_to_rust_inline(label_stmt, &nested_indent));
                result.push_str("\n");
                result.push_str(&format!("{}}}", indent));
                result
            }
            Stmt::Break(_) => format!("{}break;", indent),
            Stmt::Continue(_) => format!("{}continue;", indent),
            _ => self.todo_marker(&format!("{:?}", std::mem::discriminant(stmt)))
        }
    }

    /// Switch 文の body から Case/Default を収集して match アームを生成
    fn collect_switch_cases(&mut self, stmt: &Stmt, indent: &str, result: &mut String) {
        // パス1: case/default とそれに続く文を収集
        struct SwitchCase {
            patterns: Vec<String>,   // case 式のリスト（複数 case ラベル対応）、空 = default
            body_stmts: Vec<String>,
            is_default: bool,
        }

        let mut cases: Vec<SwitchCase> = Vec::new();
        let body_indent = format!("{}    ", indent);

        // Compound の中身をフラット化して処理
        fn collect_items<'a>(stmt: &'a Stmt, items: &mut Vec<&'a BlockItem>) {
            if let Stmt::Compound(compound) = stmt {
                for item in &compound.items {
                    items.push(item);
                }
            }
        }

        // ネストされた case/default を展開して patterns と最終的な stmt を取得
        fn flatten_case_chain<'a>(stmt: &'a Stmt, patterns: &mut Vec<&'a Expr>) -> (&'a Stmt, bool) {
            match stmt {
                Stmt::Case { expr, stmt: inner_stmt, .. } => {
                    patterns.push(expr);
                    flatten_case_chain(inner_stmt, patterns)
                }
                Stmt::Default { stmt: inner_stmt, .. } => {
                    // default に到達
                    (inner_stmt, true)
                }
                other => (other, false)
            }
        }

        let mut items: Vec<&BlockItem> = Vec::new();
        collect_items(stmt, &mut items);

        for item in items {
            match item {
                BlockItem::Stmt(s) => {
                    match s {
                        Stmt::Case { expr: case_expr, stmt: case_stmt, .. } => {
                            // ネストされた case をフラット化
                            let mut patterns: Vec<&Expr> = vec![case_expr];
                            let (final_stmt, has_default) = flatten_case_chain(case_stmt, &mut patterns);

                            // パターンは enum バリアントをフルパスで出力
                            let pattern_strs: Vec<String> = patterns.iter()
                                .map(|e| self.expr_to_rust_pattern(e))
                                .collect();

                            // final_stmt が Break の場合はスキップ（Rust の match は break 不要）
                            let body_stmts = if matches!(final_stmt, Stmt::Break(_)) {
                                vec![]
                            } else {
                                vec![self.stmt_to_rust_inline(final_stmt, &body_indent)]
                            };
                            cases.push(SwitchCase {
                                patterns: pattern_strs,
                                body_stmts,
                                is_default: has_default,
                            });
                        }
                        Stmt::Default { stmt: default_stmt, .. } => {
                            // ネストされた case をフラット化
                            let mut patterns: Vec<&Expr> = Vec::new();
                            let (final_stmt, _) = flatten_case_chain(default_stmt, &mut patterns);

                            // パターンは enum バリアントをフルパスで出力
                            let pattern_strs: Vec<String> = patterns.iter()
                                .map(|e| self.expr_to_rust_pattern(e))
                                .collect();

                            // final_stmt が Break の場合はスキップ（Rust の match は break 不要）
                            let body_stmts = if matches!(final_stmt, Stmt::Break(_)) {
                                vec![]
                            } else {
                                vec![self.stmt_to_rust_inline(final_stmt, &body_indent)]
                            };
                            cases.push(SwitchCase {
                                patterns: pattern_strs,
                                body_stmts,
                                is_default: true,
                            });
                        }
                        Stmt::Break(_) => {
                            // Rust の match はフォールスルーしないので break は不要
                            // スキップする
                        }
                        other => {
                            // 直前の case に追加
                            if let Some(last) = cases.last_mut() {
                                last.body_stmts.push(self.stmt_to_rust_inline(other, &body_indent));
                            }
                            // case がまだない場合は無視
                        }
                    }
                }
                BlockItem::Decl(decl) => {
                    // 宣言は直前の case に追加
                    if let Some(last) = cases.last_mut() {
                        last.body_stmts.push(self.decl_to_rust_let(decl, &body_indent));
                    }
                }
            }
        }

        // パス2: 収集した cases から match アームを生成
        for case in cases {
            let pattern = if case.is_default {
                if case.patterns.is_empty() {
                    "_".to_string()
                } else {
                    // case A: case B: default: ... のパターン
                    format!("{} | _", case.patterns.join(" | "))
                }
            } else {
                case.patterns.join(" | ")
            };

            result.push_str(&format!("{}{} => {{\n", indent, pattern));
            for stmt in &case.body_stmts {
                result.push_str(stmt);
                result.push_str("\n");
            }
            result.push_str(&format!("{}}}\n", indent));
        }
    }

    /// 式を Rust コードに変換（インライン関数用）
    fn expr_to_rust_inline(&mut self, expr: &Expr) -> String {
        match &expr.kind {
            ExprKind::Ident(name) => {
                escape_rust_keyword(self.interner.get(*name))
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
                // 論理演算子の場合、オペランドを bool に変換
                match op {
                    BinOp::LogAnd | BinOp::LogOr => {
                        let l_bool = wrap_as_bool_condition(lhs, &l);
                        let r_bool = wrap_as_bool_condition(rhs, &r);
                        format!("({} {} {})", l_bool, bin_op_to_rust(*op), r_bool)
                    }
                    _ => format!("({} {} {})", l, bin_op_to_rust(*op), r)
                }
            }
            ExprKind::Call { func, args } => {
                // __builtin_expect(cond, expected) -> cond
                if let ExprKind::Ident(name) = &func.kind {
                    let func_name = self.interner.get(*name);
                    if func_name == "__builtin_expect" && args.len() >= 1 {
                        return self.expr_to_rust_inline(&args[0]);
                    }
                }
                let f = self.expr_to_rust_inline(func);

                // THX マクロで my_perl が不足しているかチェック
                let needs_my_perl = if let ExprKind::Ident(name) = &func.kind {
                    self.needs_my_perl_for_call(*name, args.len())
                } else {
                    false
                };

                let mut a: Vec<String> = if needs_my_perl {
                    vec!["my_perl".to_string()]
                } else {
                    vec![]
                };
                a.extend(args.iter().map(|arg| self.expr_to_rust_inline(arg)));
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
                // void キャストは式の値を捨てる（(expr as ()) は無効）
                if t == "()" {
                    format!("{{ {}; }}", e)
                } else {
                    format!("({} as {})", e, t)
                }
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
                // 内部式を bool に変換してから論理否定
                let cond = wrap_as_bool_condition(inner, &e);
                format!("(!{})", cond)
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
                // 条件が既に bool なら != 0 を追加しない
                let cond_str = wrap_as_bool_condition(cond, &c);
                format!("(if {} {{ {} }} else {{ {} }})", cond_str, t, e)
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
                let assert_expr = if is_boolean_expr(condition) {
                    format!("assert!({})", cond)
                } else {
                    format!("assert!(({}) != 0)", cond)
                };
                match kind {
                    AssertKind::Assert => assert_expr,
                    AssertKind::AssertUnderscore => format!("{{ {}; }}", assert_expr),
                }
            }
            ExprKind::StmtExpr(compound) => {
                // GCC Statement Expression: ({ decl; stmt; ...; expr })
                //
                // MUTABLE_PTR パターンを検出:
                // ({ void *p_ = (expr); p_; }) => expr
                if let Some(init_expr) = self.detect_mutable_ptr_pattern(compound) {
                    return self.expr_to_rust_inline(init_expr);
                }

                // 通常の statement expression: Rust のブロック式として出力
                let mut parts = Vec::new();
                for item in &compound.items {
                    match item {
                        BlockItem::Stmt(Stmt::Expr(Some(e), _)) => {
                            parts.push(self.expr_to_rust_inline(e));
                        }
                        BlockItem::Stmt(stmt) => {
                            parts.push(self.stmt_to_rust_inline(stmt, ""));
                        }
                        BlockItem::Decl(_) => {
                            // 宣言はスキップ（MUTABLE_PTR パターン以外では無視）
                        }
                    }
                }
                if parts.is_empty() {
                    "{ }".to_string()
                } else if parts.len() == 1 {
                    parts.pop().unwrap()
                } else {
                    let last = parts.pop().unwrap();
                    let stmts = parts.join("; ");
                    format!("{{ {}; {} }}", stmts, last)
                }
            }
            _ => self.todo_marker(&format!("{:?}", std::mem::discriminant(&expr.kind)))
        }
    }

    /// match パターン用の式を Rust に変換
    ///
    /// 通常の式変換と異なり、enum バリアントをフルパスで出力する。
    /// Rust の match パターンでは、単純な識別子は変数束縛として扱われるため、
    /// enum バリアントは `crate::EnumName::VariantName` 形式で出力する必要がある。
    fn expr_to_rust_pattern(&mut self, expr: &Expr) -> String {
        match &expr.kind {
            ExprKind::Ident(name) => {
                // enum バリアントかチェック
                if let Some(enum_name) = self.enum_dict.get_enum_for_variant(*name) {
                    let enum_str = self.interner.get(enum_name);
                    let variant_str = self.interner.get(*name);
                    format!("crate::{}::{}", enum_str, variant_str)
                } else {
                    escape_rust_keyword(self.interner.get(*name))
                }
            }
            // 他の式は通常の変換
            _ => self.expr_to_rust_inline(expr)
        }
    }
}

impl<'a, W: Write> CodegenDriver<'a, W> {
    /// 新しいコード生成ドライバを作成
    pub fn new(
        writer: W,
        interner: &'a StringInterner,
        enum_dict: &'a EnumDict,
        macro_ctx: &'a MacroInferContext,
        config: CodegenConfig,
    ) -> Self {
        Self {
            writer,
            interner,
            enum_dict,
            macro_ctx,
            config,
            stats: CodegenStats::default(),
        }
    }

    /// 統計情報を取得
    pub fn stats(&self) -> &CodegenStats {
        &self.stats
    }

    /// 呼び出し先が THX マクロで、my_perl が不足しているかチェック
    ///
    /// 以下の条件を満たす場合に true を返す：
    /// 1. 呼び出し先が MacroInferContext.macros に存在する
    /// 2. その MacroInferInfo.is_thx_dependent が true
    /// 3. 実引数が仮引数より1つ少ない（my_perl が不足）
    fn needs_my_perl_for_call(&self, func_name: crate::InternedStr, actual_arg_count: usize) -> bool {
        if let Some(callee_info) = self.macro_ctx.macros.get(&func_name) {
            if callee_info.is_thx_dependent {
                // THX マクロの期待引数数 = params.len() + 1 (my_perl)
                let expected_count = callee_info.params.len() + 1;
                // 実引数が1つ少ない場合、my_perl が必要
                return actual_arg_count + 1 == expected_count;
            }
        }
        false
    }

    /// 全体を生成
    // デバッグ用: ビルド時のタイムスタンプを埋め込む場合はコメントを外す
    // const BUILD_TIMESTAMP: &'static str = "2025-01-24T17:50:00+09:00";

    pub fn generate(&mut self, result: &InferResult) -> io::Result<()> {
        // ヘッダーコメント
        writeln!(self.writer, "// Auto-generated Rust bindings")?;
        // デバッグ用: タイムスタンプを出力する場合はコメントを外す
        // writeln!(self.writer, "// Generated by libperl-macrogen (build: {})", Self::BUILD_TIMESTAMP)?;
        writeln!(self.writer, "// Generated by libperl-macrogen")?;
        writeln!(self.writer)?;

        // use 文を出力
        self.generate_use_statements()?;

        // target enum のバリアントを import
        self.generate_enum_imports(result)?;

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

    /// use 文を生成
    fn generate_use_statements(&mut self) -> io::Result<()> {
        let statements = if self.config.use_statements.is_empty() {
            CodegenConfig::default_use_statements()
        } else {
            self.config.use_statements.clone()
        };

        if !statements.is_empty() {
            for stmt in &statements {
                writeln!(self.writer, "{};", stmt)?;
            }
            writeln!(self.writer)?;
        }

        Ok(())
    }

    /// target enum のバリアントを import
    ///
    /// bindings.rs に存在する enum のみ import する
    fn generate_enum_imports(&mut self, result: &InferResult) -> io::Result<()> {
        let enum_names = result.enum_dict.target_enum_names(self.interner);
        let bindings_enums = result.rust_decl_dict.as_ref().map(|d| &d.enums);

        // bindings.rs に存在する enum のみフィルタリング
        let filtered_names: Vec<_> = enum_names
            .into_iter()
            .filter(|name| {
                bindings_enums.map_or(true, |enums| enums.contains(*name))
            })
            .collect();

        if !filtered_names.is_empty() {
            writeln!(self.writer, "// Enum variant imports")?;
            for name in filtered_names {
                writeln!(self.writer, "#[allow(unused_imports)]")?;
                writeln!(self.writer, "use crate::{}::*;", name)?;
            }
            writeln!(self.writer)?;
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
            let codegen = RustCodegen::new(self.interner, self.enum_dict, self.macro_ctx);
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

        // 関数本体（unsafe ブロックで囲む - Rust 2024 edition 対応）
        writeln!(self.writer, "    unsafe {{")?;
        self.generate_compound_stmt(&func_def.body, "        ")?;
        writeln!(self.writer, "    }}")?;

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
            .map(|n| escape_rust_keyword(self.interner.get(n)))
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
                TypeSpec::Signed => {} // signed is default, no action needed
                TypeSpec::Unsigned => is_unsigned = true,
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
            "()".to_string()
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

    /// 派生型を型に適用（関数ポインタを含む完全な処理）
    fn apply_derived_to_type(&self, base: &str, derived: &[DerivedDecl]) -> String {
        // Function を探す
        let fn_idx = derived
            .iter()
            .position(|d| matches!(d, DerivedDecl::Function(_)));

        if let Some(idx) = fn_idx {
            if let DerivedDecl::Function(param_list) = &derived[idx] {
                // Function の直前が Pointer なら関数ポインタ
                let is_fn_pointer =
                    idx > 0 && matches!(derived[idx - 1], DerivedDecl::Pointer(_));

                // 戻り値型の派生（Function と fn ptr Pointer を除く）
                let return_end = if is_fn_pointer { idx - 1 } else { idx };
                let return_derived = &derived[..return_end];
                let return_type = self.apply_simple_derived(base, return_derived);

                // パラメータリストを生成（型のみ、名前なし）
                let params: Vec<_> = param_list
                    .params
                    .iter()
                    .map(|p| self.param_type_only(p))
                    .collect();
                let params_str = params.join(", ");

                // 関数型を生成
                let fn_type =
                    format!("unsafe extern \"C\" fn({}) -> {}", params_str, return_type);

                // 関数ポインタの場合は Option でラップ（NULL 許容）
                if is_fn_pointer {
                    return format!("Option<{}>", fn_type);
                }
                return fn_type;
            }
        }

        // 通常の型変換（Function を含まない場合）
        self.apply_simple_derived(base, derived)
    }

    /// 単純な派生型の適用（Pointer と Array のみ）
    fn apply_simple_derived(&self, base: &str, derived: &[DerivedDecl]) -> String {
        let mut result = base.to_string();
        for d in derived.iter().rev() {
            match d {
                DerivedDecl::Pointer(quals) => {
                    // void ポインタの場合は c_void を使用
                    if result == "()" {
                        result = "c_void".to_string();
                    }
                    if quals.is_const {
                        result = format!("*const {}", result);
                    } else {
                        result = format!("*mut {}", result);
                    }
                }
                DerivedDecl::Array(arr) => {
                    // void 配列の場合は c_void を使用
                    if result == "()" {
                        result = "c_void".to_string();
                    }
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
                    // この関数では Function は処理しない（apply_derived_to_type で処理）
                }
            }
        }
        result
    }

    /// ParamDecl から型のみを取得（名前なし）
    fn param_type_only(&self, param: &ParamDecl) -> String {
        let ty = self.decl_specs_to_rust(&param.specs);
        if let Some(ref declarator) = param.declarator {
            self.apply_derived_to_type(&ty, &declarator.derived)
        } else {
            ty
        }
    }

    /// Declaration を Rust の let 宣言に変換
    fn decl_to_rust_let(&self, decl: &Declaration, indent: &str) -> String {
        let mut result = String::new();

        // 基本型を取得
        let base_type = self.decl_specs_to_rust(&decl.specs);

        // 各宣言子を処理
        for init_decl in &decl.declarators {
            let name = init_decl.declarator.name
                .map(|n| escape_rust_keyword(self.interner.get(n)))
                .unwrap_or_else(|| "_".to_string());

            // 派生型（ポインタなど）を適用
            let ty = self.apply_derived_to_type(&base_type, &init_decl.declarator.derived);

            // 初期化子
            if let Some(ref init) = init_decl.init {
                match init {
                    Initializer::Expr(expr) => {
                        let init_expr = self.expr_to_rust_inline(expr);
                        result.push_str(&format!("{}let {}: {} = {};\n", indent, name, ty, init_expr));
                    }
                    Initializer::List(_) => {
                        // 初期化リストは複雑なので TODO
                        result.push_str(&format!("{}let {}: {} = /* init list */;\n", indent, name, ty));
                    }
                }
            } else {
                // 初期化子なし（未初期化変数 - Rust では unsafe かデフォルト値が必要）
                result.push_str(&format!("{}let {}: {}; // uninitialized\n", indent, name, ty));
            }
        }

        result
    }

    /// CompoundStmt を出力
    fn generate_compound_stmt(&mut self, stmt: &CompoundStmt, indent: &str) -> io::Result<()> {
        for item in &stmt.items {
            match item {
                BlockItem::Decl(decl) => {
                    write!(self.writer, "{}", self.decl_to_rust_let(decl, indent))?;
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
                // 代入式は値を返さない形式で出力
                if let ExprKind::Assign { op, lhs, rhs } = &expr.kind {
                    let l = self.expr_to_rust_inline(lhs);
                    let r = self.expr_to_rust_inline(rhs);
                    match op {
                        AssignOp::Assign => format!("{}{} = {};", indent, l, r),
                        _ => format!("{}{} {} {};", indent, l, assign_op_to_rust(*op), r),
                    }
                } else {
                    format!("{}{};", indent, self.expr_to_rust_inline(expr))
                }
            }
            Stmt::Expr(None, _) => format!("{};", indent),
            Stmt::Return(Some(expr), _) => {
                format!("{}return {};", indent, self.expr_to_rust_inline(expr))
            }
            Stmt::Return(None, _) => format!("{}return;", indent),
            Stmt::If { cond, then_stmt, else_stmt, .. } => {
                let cond_str = self.expr_to_rust_inline(cond);
                // 条件が既に bool なら != 0 を追加しない
                let cond_bool = wrap_as_bool_condition(cond, &cond_str);
                let mut result = format!("{}if {} {{\n", indent, cond_bool);
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
                        BlockItem::Decl(decl) => {
                            let nested_indent = format!("{}    ", indent);
                            result.push_str(&self.decl_to_rust_let(decl, &nested_indent));
                        }
                    }
                }
                result.push_str(&format!("{}}}", indent));
                result
            }
            Stmt::While { cond, body, .. } => {
                let cond_str = self.expr_to_rust_inline(cond);
                // 条件が既に bool なら != 0 を追加しない
                let cond_bool = wrap_as_bool_condition(cond, &cond_str);
                let mut result = format!("{}while {} {{\n", indent, cond_bool);
                let nested_indent = format!("{}    ", indent);
                result.push_str(&self.stmt_to_rust_inline(body, &nested_indent));
                result.push_str("\n");
                result.push_str(&format!("{}}}", indent));
                result
            }
            Stmt::For { init, cond, step, body, .. } => {
                let mut result = format!("{}{{\n", indent);
                let nested_indent = format!("{}    ", indent);

                // 初期化部分
                if let Some(for_init) = init {
                    match for_init {
                        ForInit::Expr(expr) => {
                            result.push_str(&format!("{}{};\n", nested_indent, self.expr_to_rust_inline(expr)));
                        }
                        ForInit::Decl(decl) => {
                            result.push_str(&self.decl_to_rust_let(decl, &nested_indent));
                        }
                    }
                }

                // ループ部分
                if let Some(cond_expr) = cond {
                    let cond_str = self.expr_to_rust_inline(cond_expr);
                    // 条件が既に bool なら != 0 を追加しない
                    let cond_bool = wrap_as_bool_condition(cond_expr, &cond_str);
                    result.push_str(&format!("{}while {} {{\n", nested_indent, cond_bool));
                } else {
                    result.push_str(&format!("{}loop {{\n", nested_indent));
                }

                let body_indent = format!("{}    ", nested_indent);

                // ループ本体
                result.push_str(&self.stmt_to_rust_inline(body, &body_indent));
                result.push_str("\n");

                // ステップ部分
                if let Some(step_expr) = step {
                    result.push_str(&format!("{}{};\n", body_indent, self.expr_to_rust_inline(step_expr)));
                }

                result.push_str(&format!("{}}}\n", nested_indent));
                result.push_str(&format!("{}}}", indent));
                result
            }
            Stmt::DoWhile { body, cond, .. } => {
                // do { ... } while (0) パターンは一度だけ実行される loop として出力
                // これにより内部の break; が正しく動作する
                if is_zero_constant(cond) {
                    let mut result = format!("{}loop {{\n", indent);
                    let nested_indent = format!("{}    ", indent);
                    result.push_str(&self.stmt_to_rust_inline(body, &nested_indent));
                    result.push_str("\n");
                    result.push_str(&format!("{}    break;\n", indent));
                    result.push_str(&format!("{}}}", indent));
                    return result;
                }

                // 一般的な do-while 文: loop { body; if !cond { break; } }
                let mut result = format!("{}loop {{\n", indent);
                let nested_indent = format!("{}    ", indent);
                result.push_str(&self.stmt_to_rust_inline(body, &nested_indent));
                result.push_str("\n");
                let cond_str = self.expr_to_rust_inline(cond);
                // bool 式なら !cond、そうでなければ cond == 0
                let break_cond = if is_boolean_expr(cond) {
                    format!("!{}", cond_str)
                } else {
                    format!("{} == 0", cond_str)
                };
                result.push_str(&format!("{}    if {} {{ break; }}\n", indent, break_cond));
                result.push_str(&format!("{}}}", indent));
                result
            }
            Stmt::Switch { expr, body, .. } => {
                let expr_str = self.expr_to_rust_inline(expr);
                let mut result = format!("{}match {} {{\n", indent, expr_str);
                let nested_indent = format!("{}    ", indent);

                // body から Case/Default を収集
                self.collect_switch_cases(body, &nested_indent, &mut result);

                result.push_str(&format!("{}}}", indent));
                result
            }
            Stmt::Case { expr: case_expr, stmt: case_stmt, .. } => {
                let case_val = self.expr_to_rust_inline(case_expr);
                let mut result = format!("{}{} => {{\n", indent, case_val);
                let body_indent = format!("{}    ", indent);
                result.push_str(&self.stmt_to_rust_inline(case_stmt, &body_indent));
                result.push_str("\n");
                result.push_str(&format!("{}}}", indent));
                result
            }
            Stmt::Default { stmt: default_stmt, .. } => {
                let mut result = format!("{}_ => {{\n", indent);
                let body_indent = format!("{}    ", indent);
                result.push_str(&self.stmt_to_rust_inline(default_stmt, &body_indent));
                result.push_str("\n");
                result.push_str(&format!("{}}}", indent));
                result
            }
            Stmt::Goto(label, _) => {
                let label_str = self.interner.get(*label);
                format!("{}break '{}; // goto", indent, label_str)
            }
            Stmt::Label { name, stmt: label_stmt, .. } => {
                let label_str = self.interner.get(*name);
                let mut result = format!("{}'{}: {{\n", indent, label_str);
                let nested_indent = format!("{}    ", indent);
                result.push_str(&self.stmt_to_rust_inline(label_stmt, &nested_indent));
                result.push_str("\n");
                result.push_str(&format!("{}}}", indent));
                result
            }
            Stmt::Break(_) => format!("{}break;", indent),
            Stmt::Continue(_) => format!("{}continue;", indent),
            _ => format!("{}/* TODO: {:?} */", indent, std::mem::discriminant(stmt))
        }
    }

    /// Switch 文の body から Case/Default を収集して match アームを生成
    fn collect_switch_cases(&self, stmt: &Stmt, indent: &str, result: &mut String) {
        // パス1: case/default とそれに続く文を収集
        struct SwitchCase {
            patterns: Vec<String>,   // case 式のリスト（複数の case が連続する場合）
            body_stmts: Vec<String>,
            is_default: bool,
        }

        let mut cases: Vec<SwitchCase> = Vec::new();
        let body_indent = format!("{}    ", indent);

        // Compound の中身をフラット化して処理
        fn collect_items<'a>(stmt: &'a Stmt, items: &mut Vec<&'a BlockItem>) {
            if let Stmt::Compound(compound) = stmt {
                for item in &compound.items {
                    items.push(item);
                }
            }
        }

        // ネストされた case/default を展開して patterns と最終的な stmt を取得
        fn flatten_case_chain<'a>(stmt: &'a Stmt, patterns: &mut Vec<&'a Expr>) -> (&'a Stmt, bool) {
            match stmt {
                Stmt::Case { expr, stmt: inner_stmt, .. } => {
                    patterns.push(expr);
                    flatten_case_chain(inner_stmt, patterns)
                }
                Stmt::Default { stmt: inner_stmt, .. } => {
                    // default に到達
                    (inner_stmt, true)
                }
                other => (other, false)
            }
        }

        let mut items: Vec<&BlockItem> = Vec::new();
        collect_items(stmt, &mut items);

        for item in items {
            match item {
                BlockItem::Stmt(s) => {
                    match s {
                        Stmt::Case { expr: case_expr, stmt: case_stmt, .. } => {
                            // ネストされた case をフラット化
                            let mut patterns: Vec<&Expr> = vec![case_expr];
                            let (final_stmt, has_default) = flatten_case_chain(case_stmt, &mut patterns);

                            // パターンは enum バリアントをフルパスで出力
                            let pattern_strs: Vec<String> = patterns.iter()
                                .map(|e| self.expr_to_rust_pattern(e))
                                .collect();

                            // final_stmt が Break の場合はスキップ（Rust の match は break 不要）
                            let body_stmts = if matches!(final_stmt, Stmt::Break(_)) {
                                vec![]
                            } else {
                                vec![self.stmt_to_rust_inline(final_stmt, &body_indent)]
                            };
                            cases.push(SwitchCase {
                                patterns: pattern_strs,
                                body_stmts,
                                is_default: has_default,
                            });
                        }
                        Stmt::Default { stmt: default_stmt, .. } => {
                            // ネストされた case をフラット化
                            let mut patterns: Vec<&Expr> = Vec::new();
                            let (final_stmt, _) = flatten_case_chain(default_stmt, &mut patterns);

                            // パターンは enum バリアントをフルパスで出力
                            let pattern_strs: Vec<String> = patterns.iter()
                                .map(|e| self.expr_to_rust_pattern(e))
                                .collect();

                            // final_stmt が Break の場合はスキップ（Rust の match は break 不要）
                            let body_stmts = if matches!(final_stmt, Stmt::Break(_)) {
                                vec![]
                            } else {
                                vec![self.stmt_to_rust_inline(final_stmt, &body_indent)]
                            };
                            cases.push(SwitchCase {
                                patterns: pattern_strs,
                                body_stmts,
                                is_default: true,
                            });
                        }
                        Stmt::Break(_) => {
                            // Rust の match はフォールスルーしないので break は不要
                            // スキップする
                        }
                        other => {
                            // 直前の case に追加
                            if let Some(last) = cases.last_mut() {
                                last.body_stmts.push(self.stmt_to_rust_inline(other, &body_indent));
                            }
                            // case がまだない場合は無視
                        }
                    }
                }
                BlockItem::Decl(decl) => {
                    // 宣言は直前の case に追加
                    if let Some(last) = cases.last_mut() {
                        last.body_stmts.push(self.decl_to_rust_let(decl, &body_indent));
                    }
                }
            }
        }

        // パス2: 収集した cases から match アームを生成
        for case in cases {
            let pattern = if case.is_default {
                if case.patterns.is_empty() {
                    "_".to_string()
                } else {
                    // case A: case B: default: ... のパターン
                    format!("{} | _", case.patterns.join(" | "))
                }
            } else {
                case.patterns.join(" | ")
            };

            result.push_str(&format!("{}{} => {{\n", indent, pattern));
            for stmt in &case.body_stmts {
                result.push_str(stmt);
                result.push_str("\n");
            }
            result.push_str(&format!("{}}}\n", indent));
        }
    }

    /// 式を Rust コードに変換（インライン関数用）
    fn expr_to_rust_inline(&self, expr: &Expr) -> String {
        match &expr.kind {
            ExprKind::Ident(name) => {
                escape_rust_keyword(self.interner.get(*name))
            }
            ExprKind::IntLit(n) => format!("{}", n),
            ExprKind::UIntLit(n) => format!("{}u64", n),
            ExprKind::FloatLit(f) => format!("{}", f),
            ExprKind::CharLit(c) => format!("'{}'", escape_char(*c)),
            ExprKind::StringLit(s) => format!("c\"{}\"", escape_string(s)),
            ExprKind::Binary { op, lhs, rhs } => {
                let l = self.expr_to_rust_inline(lhs);
                let r = self.expr_to_rust_inline(rhs);
                // 論理演算子の場合、オペランドを bool に変換
                match op {
                    BinOp::LogAnd | BinOp::LogOr => {
                        let l_bool = wrap_as_bool_condition(lhs, &l);
                        let r_bool = wrap_as_bool_condition(rhs, &r);
                        format!("({} {} {})", l_bool, bin_op_to_rust(*op), r_bool)
                    }
                    _ => format!("({} {} {})", l, bin_op_to_rust(*op), r)
                }
            }
            ExprKind::Call { func, args } => {
                // __builtin_expect(cond, expected) -> cond
                if let ExprKind::Ident(name) = &func.kind {
                    let func_name = self.interner.get(*name);
                    if func_name == "__builtin_expect" && args.len() >= 1 {
                        return self.expr_to_rust_inline(&args[0]);
                    }
                }
                let f = self.expr_to_rust_inline(func);

                // THX マクロで my_perl が不足しているかチェック
                let needs_my_perl = if let ExprKind::Ident(name) = &func.kind {
                    self.needs_my_perl_for_call(*name, args.len())
                } else {
                    false
                };

                let mut a: Vec<String> = if needs_my_perl {
                    vec!["my_perl".to_string()]
                } else {
                    vec![]
                };
                a.extend(args.iter().map(|arg| self.expr_to_rust_inline(arg)));
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
                // void キャストは式の値を捨てる（(expr as ()) は無効）
                if t == "()" {
                    format!("{{ {}; }}", e)
                } else {
                    format!("({} as {})", e, t)
                }
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
                // 内部式を bool に変換してから論理否定
                let cond = wrap_as_bool_condition(inner, &e);
                format!("(!{})", cond)
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
                // 条件が既に bool なら != 0 を追加しない
                let cond_str = wrap_as_bool_condition(cond, &c);
                format!("(if {} {{ {} }} else {{ {} }})", cond_str, t, e)
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
                let assert_expr = if is_boolean_expr(condition) {
                    format!("assert!({})", cond)
                } else {
                    format!("assert!(({}) != 0)", cond)
                };
                match kind {
                    AssertKind::Assert => assert_expr,
                    AssertKind::AssertUnderscore => format!("{{ {}; }}", assert_expr),
                }
            }
            ExprKind::StmtExpr(compound) => {
                // GCC Statement Expression: ({ decl; stmt; ...; expr })
                let mut parts = Vec::new();
                for item in &compound.items {
                    match item {
                        BlockItem::Stmt(Stmt::Expr(Some(e), _)) => {
                            parts.push(self.expr_to_rust_inline(e));
                        }
                        BlockItem::Stmt(stmt) => {
                            parts.push(self.stmt_to_rust_inline(stmt, ""));
                        }
                        BlockItem::Decl(_) => {}
                    }
                }
                if parts.is_empty() {
                    "{ }".to_string()
                } else if parts.len() == 1 {
                    parts.pop().unwrap()
                } else {
                    let last = parts.pop().unwrap();
                    let stmts = parts.join("; ");
                    format!("{{ {}; {} }}", stmts, last)
                }
            }
            _ => format!("/* TODO: {:?} */", std::mem::discriminant(&expr.kind))
        }
    }

    /// match パターン用の式を Rust に変換
    ///
    /// 通常の式変換と異なり、enum バリアントをフルパスで出力する。
    /// Rust の match パターンでは、単純な識別子は変数束縛として扱われるため、
    /// enum バリアントは `crate::EnumName::VariantName` 形式で出力する必要がある。
    fn expr_to_rust_pattern(&self, expr: &Expr) -> String {
        match &expr.kind {
            ExprKind::Ident(name) => {
                // enum バリアントかチェック
                if let Some(enum_name) = self.enum_dict.get_enum_for_variant(*name) {
                    let enum_str = self.interner.get(enum_name);
                    let variant_str = self.interner.get(*name);
                    format!("crate::{}::{}", enum_str, variant_str)
                } else {
                    escape_rust_keyword(self.interner.get(*name))
                }
            }
            // 他の式は通常の変換
            _ => self.expr_to_rust_inline(expr)
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
                    let codegen = RustCodegen::new(self.interner, self.enum_dict, self.macro_ctx);
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
                GenerateStatus::CallsUnavailable => {
                    self.generate_macro_calls_unavailable(info, result)?;
                    self.stats.macros_calls_unavailable += 1;
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

        // 関数形式マクロのみ含める（オブジェクトマクロは常にインライン展開される）
        info.is_function
    }

    /// マクロの生成ステータスを判定
    fn get_macro_status(&self, info: &MacroInferInfo) -> GenerateStatus {
        // 利用不可関数を呼び出すマクロは CallsUnavailable
        if info.calls_unavailable {
            return GenerateStatus::CallsUnavailable;
        }

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

    /// TypeName を Rust 型文字列に変換
    fn type_name_to_rust(&self, type_name: &crate::ast::TypeName) -> String {
        // decl_specs_to_rust でベース型を取得（プリミティブ型も正しく変換）
        let base_type = self.decl_specs_to_rust(&type_name.specs);

        // 宣言子からポインタ/配列/関数を適用
        let mut result = if let Some(ref decl) = type_name.declarator {
            self.apply_derived_to_type(&base_type, &decl.derived)
        } else {
            base_type
        };

        // C の const 修飾子（例: const char*）を Rust の *const に反映
        // 最も内側のポインタを *const にする
        if type_name.specs.qualifiers.is_const {
            if let Some(pos) = result.rfind("*mut ") {
                result.replace_range(pos..pos + 5, "*const ");
            }
        }

        result
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

    /// 利用不可関数呼び出しマクロをコメント出力
    fn generate_macro_calls_unavailable(&mut self, info: &MacroInferInfo, result: &InferResult) -> io::Result<()> {
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

        // THX 情報
        let thx_info = if info.is_thx_dependent { " [THX]" } else { "" };

        writeln!(self.writer, "// [CALLS_UNAVAILABLE] {}{}{} - calls unavailable function(s)", name_str, params_str, thx_info)?;

        // 利用不可関数を特定して出力
        let unavailable_fns: Vec<_> = info.called_functions.iter()
            .filter(|&fn_id| {
                let fn_name = self.interner.get(*fn_id);
                // bindings.rs にもマクロにも存在しない関数を検出
                !self.is_function_available(*fn_id, fn_name, result)
            })
            .map(|fn_id| self.interner.get(*fn_id))
            .collect();

        if !unavailable_fns.is_empty() {
            writeln!(self.writer, "// Unavailable: {}", unavailable_fns.join(", "))?;
        }

        writeln!(self.writer)?;

        Ok(())
    }

    /// 関数が利用可能かチェック
    fn is_function_available(&self, fn_id: crate::InternedStr, fn_name: &str, result: &InferResult) -> bool {
        // マクロとして存在する場合はOK
        if result.infer_ctx.macros.contains_key(&fn_id) {
            return true;
        }

        // bindings.rs に存在する場合はOK
        if let Some(rust_decl_dict) = &result.rust_decl_dict {
            if rust_decl_dict.fns.contains_key(fn_name) {
                return true;
            }
        }

        // インライン関数として存在する場合はOK
        if result.inline_fn_dict.get(fn_id).is_some() {
            return true;
        }

        // ビルトイン関数リスト（macro_infer.rs と同じ）
        let builtin_fns = [
            "__builtin_expect",
            "__builtin_offsetof",
            "__builtin_types_compatible_p",
            "__builtin_constant_p",
            "__builtin_choose_expr",
            "__builtin_unreachable",
            "__builtin_trap",
            "__builtin_assume",
            "__builtin_bswap16",
            "__builtin_bswap32",
            "__builtin_bswap64",
            "__builtin_popcount",
            "__builtin_clz",
            "__builtin_ctz",
            "__errno_location",
            "pthread_mutex_lock",
            "pthread_mutex_unlock",
            "pthread_rwlock_rdlock",
            "pthread_rwlock_wrlock",
            "pthread_rwlock_unlock",
            "memchr",
            "memcpy",
            "memmove",
            "memset",
            "strlen",
            "strcmp",
            "strncmp",
            "strcpy",
            "strncpy",
        ];

        if builtin_fns.contains(&fn_name) {
            return true;
        }

        false
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
