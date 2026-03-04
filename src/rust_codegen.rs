//! Rust コード生成モジュール
//!
//! 型推論結果から Rust コードを生成する。

use std::collections::{HashMap, HashSet};
use std::io::{self, Write};

use crate::ast::{AssertKind, AssignOp, BinOp, BlockItem, CompoundStmt, Declaration, DeclSpecs, DerivedDecl, Expr, ExprKind, ForInit, FunctionDef, Initializer, ParamDecl, Stmt, TypeSpec};
use crate::intern::InternedStr;
use crate::enum_dict::EnumDict;
use crate::infer_api::InferResult;
use crate::intern::StringInterner;
use crate::macro_infer::{MacroInferContext, MacroInferInfo, MacroParam, ParseResult};
use crate::rust_decl::RustDeclDict;
use crate::sexp::SexpPrinter;

/// bindings.rs から抽出した codegen 用情報
#[derive(Debug, Default, Clone)]
pub struct BindingsInfo {
    /// 配列型の extern static 変数名の集合
    pub static_arrays: HashSet<String>,
    /// ビットフィールドのメソッド名集合（構造体名 → メソッド名セット）
    pub bitfield_methods: HashMap<String, HashSet<String>>,
}

impl BindingsInfo {
    /// RustDeclDict から BindingsInfo を構築
    pub fn from_rust_decl_dict(dict: &RustDeclDict) -> Self {
        Self {
            static_arrays: dict.static_arrays.clone(),
            bitfield_methods: dict.bitfield_methods.clone(),
        }
    }
}

/// libc crate から提供される関数名のリスト
/// codegen がそのまま関数呼び出しとして出力する関数のみ
/// （`__builtin_expect` 等の codegen 変換済み関数は含めない）
const LIBC_FUNCTIONS: &[&str] = &[
    "strcmp", "strlen", "strncmp", "strcpy", "strncpy",
    "memset", "memchr", "memcpy", "memmove",
];

/// コード生成時に解決可能なシンボルの集合
///
/// bindings.rs、マクロ辞書、inline 関数辞書、ビルトイン関数等から
/// 既知のシンボル名を収集する。コード生成時に `ExprKind::Ident` が
/// この集合に含まれない場合、未解決シンボルとして検出する。
pub struct KnownSymbols {
    names: HashSet<String>,
}

impl KnownSymbols {
    /// InferResult から既知シンボル集合を構築
    pub fn new(result: &InferResult, interner: &StringInterner) -> Self {
        let mut names = HashSet::new();

        // bindings.rs の関数名
        if let Some(ref dict) = result.rust_decl_dict {
            for name in dict.fns.keys() {
                names.insert(name.clone());
            }
            for name in dict.consts.keys() {
                names.insert(name.clone());
            }
            for name in dict.types.keys() {
                names.insert(name.clone());
            }
            for name in dict.structs.keys() {
                names.insert(name.clone());
            }
            for name in &dict.enums {
                names.insert(name.clone());
            }
            for name in &dict.statics {
                names.insert(name.clone());
            }
            for name in &dict.static_arrays {
                names.insert(name.clone());
            }
        }

        // マクロ名（関数呼び出しとして保持されるもの）
        for (name_id, info) in &result.infer_ctx.macros {
            let name_str = interner.get(*name_id);
            // 関数マクロのみ既知とする（オブジェクトマクロは除外）
            // オブジェクトマクロ名（例: `n`, `s`, `c`）を登録すると、
            // ジェネリック誤検出で残ったパラメータ参照が既知扱いになってしまう
            if info.has_body && info.is_function {
                names.insert(name_str.to_string());
            }
        }

        // inline 関数名
        for (name_id, _) in result.inline_fn_dict.iter() {
            let name_str = interner.get(*name_id);
            names.insert(name_str.to_string());
        }

        // ビルトイン関数（codegen が変換・除去するもの）
        let builtins = [
            "__builtin_expect",
            "__builtin_offsetof",
            "offsetof",
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
            "pthread_mutex_lock",
            "pthread_mutex_unlock",
            "pthread_rwlock_rdlock",
            "pthread_rwlock_wrlock",
            "pthread_rwlock_unlock",
            "pthread_getspecific",
            "pthread_cond_wait",
            "pthread_cond_signal",
            "getenv",
            "ASSERT_IS_LITERAL",
            "ASSERT_IS_PTR",
            "ASSERT_NOT_PTR",
        ];
        for name in builtins {
            names.insert(name.to_string());
        }

        // libc 関数（use libc::{...} で利用可能になる）
        for name in LIBC_FUNCTIONS {
            names.insert(name.to_string());
        }

        // Rust プリミティブ / 標準識別子
        let rust_primitives = [
            "true", "false", "std", "crate", "self", "super",
            "null_mut", "null",
            "PerlInterpreter", "my_perl",
        ];
        for name in rust_primitives {
            names.insert(name.to_string());
        }

        Self { names }
    }

    /// シンボル名が既知かどうかチェック
    fn contains(&self, name: &str) -> bool {
        self.names.contains(name)
    }
}

/// Rust の予約語リスト（strict keywords + reserved keywords）
/// 注: true/false はリテラルなので含めない
const RUST_KEYWORDS: &[&str] = &[
    // Strict keywords (true/false は除外 - リテラルなのでエスケープ不要)
    "as", "async", "await", "break", "const", "continue", "crate", "dyn",
    "else", "enum", "extern", "fn", "for", "if", "impl", "in",
    "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return",
    "self", "Self", "static", "struct", "super", "trait", "type",
    "unsafe", "use", "where", "while",
    // Reserved keywords
    "abstract", "become", "box", "do", "final", "gen", "macro", "override",
    "priv", "try", "typeof", "unsized", "virtual", "yield",
];

/// 識別子を Rust コードに変換
///
/// - Rust の予約語は r# を付ける
/// - C のプリプロセッサマクロは Rust の同等品に変換
fn escape_rust_keyword(name: &str) -> String {
    match name {
        // C プリプロセッサマクロ → Rust マクロ
        "__FILE__" => "file!()".to_string(),
        "__LINE__" => "line!()".to_string(),
        // Rust 予約語はエスケープ
        _ if RUST_KEYWORDS.contains(&name) => format!("r#{}", name),
        // その他はそのまま
        _ => name.to_string(),
    }
}

/// 単語境界を考慮した文字列置換
///
/// 型パラメータ名の置換時に、部分文字列一致を避けるために使用。
/// 例: "XV" を "T" に置換するとき、"XPVNV" は変更しない。
fn replace_word(s: &str, word: &str, replacement: &str) -> String {
    if word.is_empty() {
        return s.to_string();
    }
    let mut result = String::with_capacity(s.len());
    let mut start = 0;
    let bytes = s.as_bytes();
    let word_bytes = word.as_bytes();
    while let Some(pos) = s[start..].find(word) {
        let abs_pos = start + pos;
        // 前方の境界チェック
        let before_ok = abs_pos == 0 || !is_ident_char(bytes[abs_pos - 1]);
        // 後方の境界チェック
        let after_pos = abs_pos + word.len();
        let after_ok = after_pos >= bytes.len() || !is_ident_char(bytes[after_pos]);

        if before_ok && after_ok {
            result.push_str(&s[start..abs_pos]);
            result.push_str(replacement);
            start = after_pos;
        } else {
            result.push_str(&s[start..abs_pos + word_bytes.len()]);
            start = abs_pos + word_bytes.len();
        }
    }
    result.push_str(&s[start..]);
    result
}

/// 識別子を構成する文字かどうか
fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
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


/// TypeRepr がポインタ型かどうか判定
fn is_type_repr_pointer(ty: &crate::type_repr::TypeRepr) -> bool {
    use crate::type_repr::TypeRepr;
    match ty {
        TypeRepr::CType { derived, .. } => {
            derived.iter().any(|d| matches!(d, crate::type_repr::CDerivedType::Pointer { .. }))
        }
        TypeRepr::RustType { repr, .. } => {
            matches!(repr, crate::type_repr::RustTypeRepr::Pointer { .. })
        }
        TypeRepr::Inferred(inferred) => {
            inferred.resolved_type()
                .map(|r| is_type_repr_pointer(r))
                .unwrap_or(false)
        }
    }
}

/// unsigned 型へのキャスト式かどうか判定
/// 例: "(x as usize)", "(x as u32)"
fn is_unsigned_cast_expr(expr_str: &str) -> bool {
    if let Some(pos) = expr_str.rfind(" as ") {
        let after = &expr_str[pos + 4..].trim_end_matches(')');
        matches!(*after, "usize" | "u8" | "u16" | "u32" | "u64" | "u128" | "c_uint" | "c_ulong" | "c_ulonglong")
    } else {
        false
    }
}

/// 型文字列がポインタ型かどうか判定
fn is_pointer_type_str(ty: &str) -> bool {
    ty.starts_with("*mut ") || ty.starts_with("*const ")
}

/// 型文字列が const ポインタ型かどうか判定
fn is_const_pointer_type_str(ty: &str) -> bool {
    ty.starts_with("*const ")
}

/// 式が NULL リテラル（整数 0）かどうか判定
fn is_null_literal(expr: &Expr) -> bool {
    matches!(expr.kind, ExprKind::IntLit(0))
}

/// ポインタ型に対応する null ポインタ式を生成
fn null_ptr_expr(return_type: &str) -> String {
    if is_const_pointer_type_str(return_type) {
        "std::ptr::null()".to_string()
    } else {
        "std::ptr::null_mut()".to_string()
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
            "use std::ffi::{c_void, c_char, c_uchar, c_int, c_uint, c_long, c_ulong, c_short, c_ushort}".to_string(),
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
    /// goto を含む（生成対象から除外）
    ContainsGoto,
    /// スキップ（対象外）
    Skip,
}

/// 文が goto を含むか再帰的に検査
fn stmt_contains_goto(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Goto(_, _) => true,
        Stmt::Compound(cs) => block_items_contain_goto(&cs.items),
        Stmt::If { then_stmt, else_stmt, .. } => {
            stmt_contains_goto(then_stmt)
                || else_stmt.as_ref().is_some_and(|s| stmt_contains_goto(s))
        }
        Stmt::Switch { body, .. }
        | Stmt::While { body, .. }
        | Stmt::For { body, .. } => stmt_contains_goto(body),
        Stmt::DoWhile { body, .. } => stmt_contains_goto(body),
        Stmt::Label { stmt, .. } => stmt_contains_goto(stmt),
        Stmt::Case { stmt, .. } | Stmt::Default { stmt, .. } => stmt_contains_goto(stmt),
        _ => false,
    }
}

/// ブロック項目リストが goto を含むか検査
fn block_items_contain_goto(items: &[BlockItem]) -> bool {
    items.iter().any(|item| match item {
        BlockItem::Stmt(stmt) => stmt_contains_goto(stmt),
        BlockItem::Decl(_) => false,
    })
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
    /// カスケード依存でコメントアウトされたマクロ数
    pub macros_cascade_unavailable: usize,
    /// 未解決シンボルを含むマクロ数
    pub macros_unresolved_names: usize,
    /// 正常生成された inline 関数数
    pub inline_fns_success: usize,
    /// 型推論失敗 inline 関数数
    pub inline_fns_type_incomplete: usize,
    /// 未解決シンボルを含む inline 関数数
    pub inline_fns_unresolved_names: usize,
    /// カスケード依存でコメントアウトされた inline 関数数
    pub inline_fns_cascade_unavailable: usize,
    /// goto を含む inline 関数数
    pub inline_fns_contains_goto: usize,
}

/// 一つの関数の生成結果
#[derive(Debug, Clone)]
pub struct GeneratedCode {
    /// 生成されたコード
    pub code: String,
    /// 不完全マーカーの数
    pub incomplete_count: usize,
    /// 検出された未解決シンボル名（重複なし、出現順）
    pub unresolved_names: Vec<String>,
    /// 使用された libc 関数名
    pub used_libc_fns: HashSet<String>,
}

impl GeneratedCode {
    /// 生成が完全かどうか（不完全マーカーがないか）
    pub fn is_complete(&self) -> bool {
        self.incomplete_count == 0
    }

    /// 未解決シンボルがあるかどうか
    pub fn has_unresolved_names(&self) -> bool {
        !self.unresolved_names.is_empty()
    }
}

/// 式の型ヒント（codegen用の簡易版）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TypeHint {
    Pointer,   // *mut T / *const T
    Integer,   // i32, u32, usize, etc.
    Bool,
    Unknown,
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
    /// bindings.rs から抽出した情報
    bindings_info: BindingsInfo,
    /// 内部バッファ（生成結果を蓄積）
    buffer: String,
    /// 不完全マーカーの生成回数
    incomplete_count: usize,
    /// 現在生成中のマクロの型パラメータマップ
    /// 仮引数名(InternedStr) → ジェネリック名("T", "U", ...)
    current_type_param_map: HashMap<InternedStr, String>,
    /// 現在生成中のマクロのリテラル文字列パラメータ名の集合
    current_literal_string_params: HashSet<InternedStr>,
    /// 現在生成中の関数の戻り値型文字列
    current_return_type: Option<String>,
    /// Call式のlvalue展開時に使用するパラメータ置換テーブル
    /// マクロ仮引数名 → 実引数のRust文字列
    param_substitutions: HashMap<InternedStr, String>,
    /// 現在生成中の関数のパラメータ型情報
    /// パラメータ名 → Rust型文字列
    current_param_types: HashMap<InternedStr, String>,
    /// 既知シンボル集合への参照（未解決シンボル検出用）
    known_symbols: &'a KnownSymbols,
    /// 現在の関数のローカルスコープ（パラメータ名 + ローカル変数名）
    current_local_names: HashSet<InternedStr>,
    /// 検出された未解決シンボル名（重複なし、出現順）
    unresolved_names: Vec<String>,
    /// 使用された libc 関数名
    used_libc_fns: HashSet<String>,
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
    /// bindings.rs から抽出した情報
    bindings_info: BindingsInfo,
    config: CodegenConfig,
    stats: CodegenStats,
    /// 生成されたコード全体で使用された libc 関数名
    used_libc_fns: HashSet<String>,
    /// 正常生成された inline 関数名（クロスドメインカスケード検出用）
    successfully_generated_inlines: HashSet<InternedStr>,
    /// 生成可能と予測されるマクロの集合（inline→macro カスケード検出用）
    generatable_macros: HashSet<InternedStr>,
}

impl<'a> RustCodegen<'a> {
    /// 新しい単一関数用コード生成器を作成
    pub fn new(
        interner: &'a StringInterner,
        enum_dict: &'a EnumDict,
        macro_ctx: &'a MacroInferContext,
        bindings_info: BindingsInfo,
        known_symbols: &'a KnownSymbols,
    ) -> Self {
        Self {
            interner,
            enum_dict,
            macro_ctx,
            bindings_info,
            buffer: String::new(),
            incomplete_count: 0,
            current_type_param_map: HashMap::new(),
            current_literal_string_params: HashSet::new(),
            current_return_type: None,
            param_substitutions: HashMap::new(),
            current_param_types: HashMap::new(),
            known_symbols,
            current_local_names: HashSet::new(),
            unresolved_names: Vec::new(),
            used_libc_fns: HashSet::new(),
        }
    }

    /// Call 式が lvalue マクロ呼び出しなら、展開済み lvalue 文字列を返す
    fn try_expand_call_as_lvalue(&mut self, func: &Expr, args: &[Expr], info: &MacroInferInfo) -> Option<String> {
        if let ExprKind::Ident(name) = &func.kind {
            if self.should_emit_as_macro_call(*name) {
                if let Some(macro_info) = self.macro_ctx.macros.get(name) {
                    if let ParseResult::Expression(body) = &macro_info.parse_result {
                        let body = body.clone();
                        // パラメータ名 → 実引数文字列のマッピングを作成
                        let saved_params = std::mem::take(&mut self.param_substitutions);
                        for (i, param) in macro_info.params.iter().enumerate() {
                            if let Some(arg) = args.get(i) {
                                let arg_str = self.expr_to_rust(arg, info);
                                self.param_substitutions.insert(param.name, arg_str);
                            }
                        }
                        let result = self.expr_to_rust(&body, info);
                        self.param_substitutions = saved_params;
                        return Some(result);
                    }
                }
            }
        }
        None
    }

    /// Call 式が lvalue マクロ呼び出しなら、展開済み lvalue 文字列を返す（inline版）
    fn try_expand_call_as_lvalue_inline(&mut self, func: &Expr, args: &[Expr]) -> Option<String> {
        if let ExprKind::Ident(name) = &func.kind {
            if self.should_emit_as_macro_call(*name) {
                if let Some(macro_info) = self.macro_ctx.macros.get(name) {
                    if let ParseResult::Expression(body) = &macro_info.parse_result {
                        let body = body.clone();
                        let saved_params = std::mem::take(&mut self.param_substitutions);
                        for (i, param) in macro_info.params.iter().enumerate() {
                            if let Some(arg) = args.get(i) {
                                let arg_str = self.expr_to_rust_inline(arg);
                                self.param_substitutions.insert(param.name, arg_str);
                            }
                        }
                        let result = self.expr_to_rust_inline(&body);
                        self.param_substitutions = saved_params;
                        return Some(result);
                    }
                }
            }
        }
        None
    }

    /// 式の型ヒントを推定する（codegen用の簡易版）
    fn infer_type_hint(&self, expr: &Expr, info: &MacroInferInfo) -> TypeHint {
        match &expr.kind {
            ExprKind::IntLit(_) | ExprKind::UIntLit(_) | ExprKind::CharLit(_) => TypeHint::Integer,
            ExprKind::Cast { type_name, .. } => {
                let t = self.type_name_to_rust_readonly(type_name);
                if is_pointer_type_str(&t) { TypeHint::Pointer }
                else if t == "bool" { TypeHint::Bool }
                else { TypeHint::Integer }
            }
            ExprKind::Ident(name) => {
                // パラメータの型制約を参照
                if let Some(constraints) = info.type_env.param_constraints.get(name) {
                    for c in constraints {
                        if is_type_repr_pointer(&c.ty) {
                            return TypeHint::Pointer;
                        }
                    }
                }
                // param_to_exprs 経由の expr_constraints も参照
                if let Some(expr_ids) = info.type_env.param_to_exprs.get(name) {
                    for expr_id in expr_ids {
                        if let Some(constraints) = info.type_env.expr_constraints.get(expr_id) {
                            for c in constraints {
                                if is_type_repr_pointer(&c.ty) {
                                    return TypeHint::Pointer;
                                }
                            }
                        }
                    }
                }
                TypeHint::Unknown
            }
            ExprKind::Call { func, .. } => {
                if let ExprKind::Ident(name) = &func.kind {
                    if let Some(callee) = self.macro_ctx.macros.get(name) {
                        for c in &callee.type_env.return_constraints {
                            if is_type_repr_pointer(&c.ty) {
                                return TypeHint::Pointer;
                            }
                        }
                    }
                }
                TypeHint::Unknown
            }
            ExprKind::MacroCall { name, .. } => {
                if let Some(callee) = self.macro_ctx.macros.get(name) {
                    for c in &callee.type_env.return_constraints {
                        if is_type_repr_pointer(&c.ty) {
                            return TypeHint::Pointer;
                        }
                    }
                }
                TypeHint::Unknown
            }
            ExprKind::AddrOf(_) => TypeHint::Pointer,
            ExprKind::Deref(_) => TypeHint::Unknown,
            ExprKind::PtrMember { .. } | ExprKind::Member { .. } => TypeHint::Unknown,
            ExprKind::Binary { op, .. } => {
                match op {
                    BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge |
                    BinOp::LogAnd | BinOp::LogOr => TypeHint::Bool,
                    _ => TypeHint::Unknown,
                }
            }
            ExprKind::LogNot(_) => TypeHint::Bool,
            ExprKind::Sizeof(_) | ExprKind::SizeofType(_) => TypeHint::Integer,
            ExprKind::BuiltinCall { .. } => TypeHint::Integer,
            _ => TypeHint::Unknown,
        }
    }

    /// ポインタ式をbool条件に変換するラッパー（マクロ用、infer_type_hint使用）
    fn wrap_as_bool_condition_macro(&self, expr: &Expr, expr_str: &str, info: &MacroInferInfo) -> String {
        if is_boolean_expr(expr) {
            return expr_str.to_string();
        }
        if expr_str.ends_with(" as bool)") || expr_str.ends_with("!= 0)") || expr_str.ends_with(".is_null()") {
            return expr_str.to_string();
        }
        if self.infer_type_hint(expr, info) == TypeHint::Pointer {
            return format!("!{}.is_null()", expr_str);
        }
        format!("(({}) != 0)", expr_str)
    }

    /// ポインタ式をbool条件に変換するラッパー（inline関数用）
    fn wrap_as_bool_condition_inline(&self, expr: &Expr, expr_str: &str) -> String {
        if is_boolean_expr(expr) {
            return expr_str.to_string();
        }
        if expr_str.ends_with(" as bool)") || expr_str.ends_with("!= 0)") || expr_str.ends_with(".is_null()") {
            return expr_str.to_string();
        }
        if self.is_pointer_expr_inline(expr) {
            return format!("!{}.is_null()", expr_str);
        }
        format!("(({}) != 0)", expr_str)
    }

    /// 式がポインタ型かどうかを current_param_types から推定（inline関数用）
    fn is_pointer_expr_inline(&self, expr: &Expr) -> bool {
        match &expr.kind {
            ExprKind::Ident(name) => {
                if let Some(ty) = self.current_param_types.get(name) {
                    return is_pointer_type_str(ty);
                }
                false
            }
            ExprKind::Cast { type_name, .. } => {
                let has_pointer = type_name.declarator.as_ref()
                    .map(|d| d.derived.iter().any(|dd| matches!(dd, crate::ast::DerivedDecl::Pointer { .. })))
                    .unwrap_or(false);
                has_pointer
            }
            ExprKind::AddrOf(_) => true,
            ExprKind::Call { func, .. } => {
                // マクロの戻り値型を参照
                if let ExprKind::Ident(name) = &func.kind {
                    if let Some(callee) = self.macro_ctx.macros.get(name) {
                        for c in &callee.type_env.return_constraints {
                            if is_type_repr_pointer(&c.ty) {
                                return true;
                            }
                        }
                    }
                }
                false
            }
            _ => false,
        }
    }

    /// type_name_to_rust の読み取り専用版（&self で呼び出し可能）
    fn type_name_to_rust_readonly(&self, type_name: &crate::ast::TypeName) -> String {
        // 簡易版：TypeSpec からポインタ型かどうかを判定
        let has_pointer = type_name.declarator.as_ref()
            .map(|d| d.derived.iter().any(|dd| matches!(dd, crate::ast::DerivedDecl::Pointer { .. })))
            .unwrap_or(false);
        if has_pointer {
            return "*mut unknown".to_string(); // ポインタ型であることが分かれば十分
        }
        // Bool チェック
        for spec in &type_name.specs.type_specs {
            if matches!(spec, TypeSpec::Bool) {
                return "bool".to_string();
            }
        }
        "int".to_string() // デフォルトは整数型
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

    /// 式が既知の static 配列名かどうかをチェック
    fn is_static_array_expr(&self, expr: &Expr) -> bool {
        if let ExprKind::Ident(name) = &expr.kind {
            let name_str = self.interner.get(*name);
            self.bindings_info.static_arrays.contains(name_str)
        } else {
            false
        }
    }

    /// フィールド名がビットフィールドメソッドかどうかをチェック
    ///
    /// 構造体名が不明な場合は、全構造体のビットフィールドメソッド名を検索
    fn is_bitfield_method(&self, member_name: &str) -> bool {
        self.bindings_info.bitfield_methods.values()
            .any(|methods| methods.contains(member_name))
    }

    /// 呼び出し先マクロのジェネリック型パラメータ情報を取得
    fn get_callee_generic_params(&self, func_name: InternedStr) -> Option<&HashMap<i32, String>> {
        let callee_info = self.macro_ctx.macros.get(&func_name)?;
        if callee_info.generic_type_params.is_empty() {
            return None;
        }
        if callee_info.generic_type_params.keys().any(|&k| k >= 0) {
            Some(&callee_info.generic_type_params)
        } else {
            None
        }
    }

    /// キャスト先の型が enum 型かどうか判定
    fn is_enum_cast_target(&self, type_name: &crate::ast::TypeName) -> bool {
        for spec in &type_name.specs.type_specs {
            match spec {
                TypeSpec::TypedefName(name) => return self.enum_dict.is_target_enum(*name),
                TypeSpec::Enum(_) => return true,
                _ => {}
            }
        }
        false
    }

    /// 呼び出し先マクロの特定の引数位置がリテラル文字列パラメータかチェック
    fn callee_expects_literal_string(&self, func_name: InternedStr, arg_index: usize) -> bool {
        if let Some(callee_info) = self.macro_ctx.macros.get(&func_name) {
            return callee_info.literal_string_params.contains(&arg_index);
        }
        false
    }

    /// 式の最内部にある literal_string_param の Ident を探す
    /// identity builtin（ASSERT_IS_LITERAL 等）を透過して中身をチェック
    fn find_literal_string_ident<'b>(&self, expr: &'b Expr) -> Option<&'b InternedStr> {
        match &expr.kind {
            ExprKind::Ident(name) if self.current_literal_string_params.contains(name) => {
                Some(name)
            }
            ExprKind::Call { func, args } if args.len() == 1 => {
                // identity builtin (ASSERT_IS_LITERAL, ASSERT_IS_PTR, ASSERT_NOT_PTR)
                if let ExprKind::Ident(fname) = &func.kind {
                    let func_name = self.interner.get(*fname);
                    if func_name == "ASSERT_IS_LITERAL"
                        || func_name == "ASSERT_IS_PTR"
                        || func_name == "ASSERT_NOT_PTR"
                    {
                        return self.find_literal_string_ident(&args[0]);
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// 式を Rust コードに変換し、literal_string_param の Ident に .as_ptr() を付与
    fn expr_to_rust_arg(&mut self, expr: &Expr, info: &MacroInferInfo, callee: Option<InternedStr>, arg_index: usize) -> String {
        if let Some(name) = self.find_literal_string_ident(expr) {
            // 呼び出し先も &str を受ける場合は変換不要
            if let Some(callee_name) = callee {
                if self.callee_expects_literal_string(callee_name, arg_index) {
                    return escape_rust_keyword(self.interner.get(*name));
                }
            }
            let param = escape_rust_keyword(self.interner.get(*name));
            return format!("{}.as_ptr() as *const c_char", param);
        }
        self.expr_to_rust(expr, info)
    }

    /// マクロ呼び出し形式で出力すべきかを判定
    ///
    /// 以下の場合にマクロ呼び出し形式で出力する：
    /// - MacroInferContext に登録されており、生成対象である
    ///
    /// そうでない場合は展開形式で出力する。
    fn should_emit_as_macro_call(&self, name: crate::InternedStr) -> bool {
        // MacroInferContext にマクロ情報があり、パース可能なら生成対象
        if let Some(info) = self.macro_ctx.macros.get(&name) {
            // パース成功し、利用不可関数を呼ばないマクロのみ
            return info.is_parseable() && !info.calls_unavailable;
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

    /// Declaration から変数名を収集して current_local_names に追加
    /// ローカル変数の型も current_param_types に登録（ポインタ検出用）
    fn collect_decl_names(&mut self, decl: &Declaration) {
        let base_type = self.decl_specs_to_rust(&decl.specs);
        for init_decl in &decl.declarators {
            if let Some(name) = init_decl.declarator.name {
                self.current_local_names.insert(name);
                let ty = self.apply_derived_to_type(&base_type, &init_decl.declarator.derived);
                self.current_param_types.insert(name, ty);
            }
        }
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
            unresolved_names: self.unresolved_names,
            used_libc_fns: self.used_libc_fns,
        }
    }

    /// マクロ関数を生成（self を消費）
    pub fn generate_macro(mut self, info: &MacroInferInfo) -> GeneratedCode {
        let name_str = self.interner.get(info.name);

        // ローカルスコープ: マクロのパラメータ名を登録
        for p in &info.params {
            self.current_local_names.insert(p.name);
        }

        // 型パラメータマップを構築
        self.current_type_param_map = info.generic_type_params.iter()
            .filter(|(idx, _)| **idx >= 0)
            .filter_map(|(idx, generic_name)| {
                info.params.get(*idx as usize).map(|p| (p.name, generic_name.clone()))
            })
            .collect();

        // 型パラメータになったパラメータは通常パラメータとしては存在しないので
        // current_local_names から除外する（ジェネリック誤検出時に unresolved 検出するため）
        for (name, _) in &self.current_type_param_map {
            self.current_local_names.remove(name);
        }

        // リテラル文字列パラメータの名前集合を構築
        self.current_literal_string_params = info.literal_string_params.iter()
            .filter_map(|&idx| info.params.get(idx).map(|p| p.name))
            .collect();

        // ジェネリック句を生成
        let generic_clause = self.build_generic_clause(info);

        // パラメータリストを構築（型情報付き）
        // type/cast パラメータは値引数ではないので除外
        let params_with_types = self.build_param_list(info);

        // 戻り値の型を取得（current_return_type にもセット）
        let return_type = self.get_return_type(info);
        self.current_return_type = Some(return_type.clone());

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
                let type_hint = self.current_return_type.clone();
                let rust_expr = self.expr_with_type_hint(expr, info, type_hint.as_deref());
                if self.current_return_type.as_deref() == Some("()") {
                    // void 関数: 式の結果を捨てる
                    self.writeln(&format!("{}{};", body_indent, rust_expr));
                } else {
                    self.writeln(&format!("{}{}", body_indent, rust_expr));
                }
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

        // リテラル文字列パラメータかチェック（apidoc の "..." 引数）
        if info.literal_string_params.contains(&param_index) {
            return "&str".to_string();
        }

        let param_name = param.name;

        // 方法1: パラメータを参照する式の型制約から取得（逆引き辞書を使用）
        // void 以外の型を優先的に選択する
        if let Some(expr_ids) = info.type_env.param_to_exprs.get(&param_name) {
            for expr_id in expr_ids {
                if let Some(constraints) = info.type_env.expr_constraints.get(expr_id) {
                    // void 以外の型を探す
                    for c in constraints {
                        if !c.ty.is_void() {
                            return self.type_repr_to_rust(&c.ty);
                        }
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
        let result = self.substitute_type_params(&result);
        if result.contains("/*") {
            self.incomplete_count += 1;
        }
        result
    }

    /// 型文字列中の型パラメータ名を generic 名に置換
    fn substitute_type_params(&self, type_str: &str) -> String {
        if self.current_type_param_map.is_empty() {
            return type_str.to_string();
        }
        let mut result = type_str.to_string();
        for (param_name, generic_name) in &self.current_type_param_map {
            let name_str = self.interner.get(*param_name);
            // 単語境界を考慮した置換（部分文字列一致を避ける）
            result = replace_word(&result, name_str, generic_name);
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
                // lvalue展開時のパラメータ置換
                if let Some(subst) = self.param_substitutions.get(name) {
                    return subst.clone();
                }
                let name_str = self.interner.get(*name);
                // libc 関数の使用を記録
                if LIBC_FUNCTIONS.contains(&name_str) {
                    self.used_libc_fns.insert(name_str.to_string());
                }
                // 未解決シンボルチェック
                // Note: type_param_map の名前が値コンテキストで出現する場合も
                // unresolved とする（ジェネリック誤検出の検出）
                if !self.current_local_names.contains(name)
                    && !self.enum_dict.is_enum_variant(*name)
                    && !self.known_symbols.contains(name_str)
                {
                    let s = name_str.to_string();
                    if !self.unresolved_names.contains(&s) {
                        self.unresolved_names.push(s);
                    }
                }
                escape_rust_keyword(name_str)
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
                // sizeof(literal_string_param) - 1 → param.len()
                if *op == BinOp::Sub {
                    if let ExprKind::Sizeof(inner) = &lhs.kind {
                        if let ExprKind::Ident(name) = &inner.kind {
                            if self.current_literal_string_params.contains(name) {
                                if let ExprKind::IntLit(1) = &rhs.kind {
                                    let param = escape_rust_keyword(self.interner.get(*name));
                                    return format!("{}.len()", param);
                                }
                            }
                        }
                    }
                }
                let lh = self.infer_type_hint(lhs, info);
                let rh = self.infer_type_hint(rhs, info);

                // ポインタ == 0 / != 0 → .is_null()
                if matches!(op, BinOp::Eq | BinOp::Ne) {
                    if lh == TypeHint::Pointer && is_null_literal(rhs) {
                        let l = self.expr_to_rust(lhs, info);
                        return if *op == BinOp::Eq {
                            format!("{}.is_null()", l)
                        } else {
                            format!("!{}.is_null()", l)
                        };
                    }
                    if rh == TypeHint::Pointer && is_null_literal(lhs) {
                        let r = self.expr_to_rust(rhs, info);
                        return if *op == BinOp::Eq {
                            format!("{}.is_null()", r)
                        } else {
                            format!("!{}.is_null()", r)
                        };
                    }
                }

                // ポインタ ± 整数 → .offset()
                if matches!(op, BinOp::Add | BinOp::Sub) {
                    if lh == TypeHint::Pointer && rh != TypeHint::Pointer {
                        let l = self.expr_to_rust(lhs, info);
                        let r = self.expr_to_rust(rhs, info);
                        return if *op == BinOp::Add {
                            format!("{}.offset({} as isize)", l, r)
                        } else {
                            format!("{}.offset(-({} as isize))", l, r)
                        };
                    }
                    if rh == TypeHint::Pointer && lh != TypeHint::Pointer && *op == BinOp::Add {
                        let l = self.expr_to_rust(lhs, info);
                        let r = self.expr_to_rust(rhs, info);
                        return format!("{}.offset({} as isize)", r, l);
                    }
                    // ポインタ - ポインタ → .offset_from()
                    if lh == TypeHint::Pointer && rh == TypeHint::Pointer && *op == BinOp::Sub {
                        let l = self.expr_to_rust(lhs, info);
                        let r = self.expr_to_rust(rhs, info);
                        return format!("{}.offset_from({})", l, r);
                    }
                }

                let l = self.expr_to_rust(lhs, info);
                let r = self.expr_to_rust(rhs, info);
                // 論理演算子の場合、オペランドを bool に変換
                match op {
                    BinOp::LogAnd | BinOp::LogOr => {
                        let l_bool = self.wrap_as_bool_condition_macro(lhs, &l, info);
                        let r_bool = self.wrap_as_bool_condition_macro(rhs, &r, info);
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
                    // __builtin_unreachable() -> std::hint::unreachable_unchecked()
                    if func_name == "__builtin_unreachable" {
                        return "std::hint::unreachable_unchecked()".to_string();
                    }
                    // __builtin_ctz(x) / __builtin_ctzl(x) -> (x).trailing_zeros()
                    if (func_name == "__builtin_ctz" || func_name == "__builtin_ctzl")
                        && args.len() == 1
                    {
                        let arg = self.expr_to_rust(&args[0], info);
                        return format!("({}).trailing_zeros()", arg);
                    }
                    // __builtin_clz(x) / __builtin_clzl(x) -> (x).leading_zeros()
                    if (func_name == "__builtin_clz" || func_name == "__builtin_clzl")
                        && args.len() == 1
                    {
                        let arg = self.expr_to_rust(&args[0], info);
                        return format!("({}).leading_zeros()", arg);
                    }
                    // ASSERT_IS_LITERAL(s) -> s, ASSERT_IS_PTR(x) -> x, ASSERT_NOT_PTR(x) -> x
                    // コンパイル時型チェックマクロ。Rust では不要なので引数をそのまま返す
                    if (func_name == "ASSERT_IS_LITERAL"
                        || func_name == "ASSERT_IS_PTR"
                        || func_name == "ASSERT_NOT_PTR")
                        && args.len() == 1
                    {
                        return self.expr_to_rust(&args[0], info);
                    }
                    // offsetof(type, field) → std::mem::offset_of!(Type, field_path)
                    if (func_name == "offsetof" || func_name == "__builtin_offsetof")
                        && args.len() == 2
                    {
                        let type_name = self.expr_to_rust(&args[0], info);
                        if let Some(field_path) = self.expr_to_field_path(&args[1]) {
                            return format!("std::mem::offset_of!({}, {})", type_name, field_path);
                        }
                    }
                }
                let f = self.expr_to_rust(func, info);

                // 呼び出し先の名前を取得（literal_string 変換の判定に使用）
                let callee_name = if let ExprKind::Ident(name) = &func.kind {
                    Some(*name)
                } else {
                    None
                };

                // THX マクロで my_perl が不足しているかチェック
                let needs_my_perl = callee_name
                    .map(|name| self.needs_my_perl_for_call(name, args.len()))
                    .unwrap_or(false);

                // ジェネリック型パラメータのチェック
                let callee_generics = callee_name
                    .and_then(|name| self.get_callee_generic_params(name).cloned());

                if let Some(ref generics) = callee_generics {
                    let mut type_args = Vec::new();
                    let mut value_args: Vec<String> = if needs_my_perl {
                        vec!["my_perl".to_string()]
                    } else {
                        vec![]
                    };
                    let mut value_idx = if needs_my_perl { 1usize } else { 0 };
                    for (i, arg) in args.iter().enumerate() {
                        if generics.contains_key(&(i as i32)) {
                            type_args.push(self.expr_to_rust(arg, info));
                        } else {
                            value_args.push(self.expr_to_rust_arg(arg, info, callee_name, value_idx));
                            value_idx += 1;
                        }
                    }
                    return format!("{}::<{}>({})", f, type_args.join(", "), value_args.join(", "));
                }

                let mut a: Vec<String> = if needs_my_perl {
                    vec!["my_perl".to_string()]
                } else {
                    vec![]
                };
                let arg_offset = if needs_my_perl { 1usize } else { 0 };
                a.extend(args.iter().enumerate().map(|(i, arg)| {
                    self.expr_to_rust_arg(arg, info, callee_name, i + arg_offset)
                }));
                format!("{}({})", f, a.join(", "))
            }
            ExprKind::Member { expr: base, member } => {
                let e = self.expr_to_rust(base, info);
                let m = self.interner.get(*member);
                if self.is_bitfield_method(m) {
                    format!("({}).{}()", e, m)
                } else {
                    format!("({}).{}", e, m)
                }
            }
            ExprKind::PtrMember { expr: base, member } => {
                let e = self.expr_to_rust(base, info);
                let m = self.interner.get(*member);
                if self.is_bitfield_method(m) {
                    format!("(*{}).{}()", e, m)
                } else {
                    format!("(*{}).{}", e, m)
                }
            }
            ExprKind::Index { expr: base, index } => {
                let b = self.expr_to_rust(base, info);
                let i = self.expr_to_rust(index, info);
                if self.is_static_array_expr(base) {
                    format!("(*{}.as_ptr().offset({} as isize))", b, i)
                } else {
                    format!("(*{}.offset({} as isize))", b, i)
                }
            }
            ExprKind::Cast { type_name, expr: inner } => {
                let e = self.expr_to_rust(inner, info);
                let t = self.type_name_to_rust(type_name);
                // void キャストは式の値を捨てる（(expr as ()) は無効）
                if t == "()" {
                    format!("{{ {}; }}", e)
                } else if t == "bool" {
                    // Rust では整数を as bool でキャストできない
                    format!("(({}) != 0)", e)
                } else if self.is_enum_cast_target(type_name) {
                    // enum へのキャストは transmute を使用
                    format!("std::mem::transmute::<_, {}>({})", t, e)
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
                // lvalue が MacroCall/Call の場合は展開形式を使用
                let e = if let ExprKind::MacroCall { expanded, .. } = &inner.kind {
                    self.expr_to_rust(expanded, info)
                } else if let ExprKind::Call { func, args } = &inner.kind {
                    self.try_expand_call_as_lvalue(func, args, info)
                        .unwrap_or_else(|| self.expr_to_rust(inner, info))
                } else {
                    self.expr_to_rust(inner, info)
                };
                format!("{{ {} += 1; {} }}", e, e)
            }
            ExprKind::PreDec(inner) => {
                let e = if let ExprKind::MacroCall { expanded, .. } = &inner.kind {
                    self.expr_to_rust(expanded, info)
                } else if let ExprKind::Call { func, args } = &inner.kind {
                    self.try_expand_call_as_lvalue(func, args, info)
                        .unwrap_or_else(|| self.expr_to_rust(inner, info))
                } else {
                    self.expr_to_rust(inner, info)
                };
                format!("{{ {} -= 1; {} }}", e, e)
            }
            ExprKind::PostInc(inner) => {
                let e = if let ExprKind::MacroCall { expanded, .. } = &inner.kind {
                    self.expr_to_rust(expanded, info)
                } else if let ExprKind::Call { func, args } = &inner.kind {
                    self.try_expand_call_as_lvalue(func, args, info)
                        .unwrap_or_else(|| self.expr_to_rust(inner, info))
                } else {
                    self.expr_to_rust(inner, info)
                };
                format!("{{ let _t = {}; {} += 1; _t }}", e, e)
            }
            ExprKind::PostDec(inner) => {
                let e = if let ExprKind::MacroCall { expanded, .. } = &inner.kind {
                    self.expr_to_rust(expanded, info)
                } else if let ExprKind::Call { func, args } = &inner.kind {
                    self.try_expand_call_as_lvalue(func, args, info)
                        .unwrap_or_else(|| self.expr_to_rust(inner, info))
                } else {
                    self.expr_to_rust(inner, info)
                };
                format!("{{ let _t = {}; {} -= 1; _t }}", e, e)
            }
            ExprKind::UnaryPlus(inner) => {
                self.expr_to_rust(inner, info)
            }
            ExprKind::UnaryMinus(inner) => {
                let e = self.expr_to_rust(inner, info);
                // unsigned 型のキャスト結果に対する負号は wrapping_neg を使用
                if is_unsigned_cast_expr(&e) {
                    format!("({}).wrapping_neg()", e.trim_start_matches('-'))
                } else {
                    format!("(-{})", e)
                }
            }
            ExprKind::BitNot(inner) => {
                let e = self.expr_to_rust(inner, info);
                format!("(!{})", e)
            }
            ExprKind::LogNot(inner) => {
                let e = self.expr_to_rust(inner, info);
                // 内部式を bool に変換してから論理否定
                let cond = self.wrap_as_bool_condition_macro(inner, &e, info);
                format!("(!{})", cond)
            }
            ExprKind::Sizeof(inner) => {
                // sizeof(literal_string_param) → param.len() + 1
                // C の sizeof はリテラル文字列の null 終端を含むサイズを返す
                if let ExprKind::Ident(name) = &inner.kind {
                    if self.current_literal_string_params.contains(name) {
                        let param = escape_rust_keyword(self.interner.get(*name));
                        return format!("({}.len() + 1)", param);
                    }
                }
                let e = self.expr_to_rust(inner, info);
                format!("std::mem::size_of_val(&{})", e)
            }
            ExprKind::SizeofType(type_name) => {
                let t = self.type_name_to_rust(type_name);
                format!("std::mem::size_of::<{}>()", t)
            }
            ExprKind::Conditional { cond, then_expr, else_expr } => {
                let c = self.expr_to_rust(cond, info);
                // 条件が既に bool なら != 0 を追加しない
                let cond_str = self.wrap_as_bool_condition_macro(cond, &c, info);
                let type_hint = self.current_return_type.clone();
                let t = self.expr_with_type_hint(then_expr, info, type_hint.as_deref());
                let e = self.expr_with_type_hint(else_expr, info, type_hint.as_deref());
                format!("(if {} {{ {} }} else {{ {} }})", cond_str, t, e)
            }
            ExprKind::Comma { lhs, rhs } => {
                let l = self.expr_to_rust(lhs, info);
                let r = self.expr_to_rust(rhs, info);
                format!("{{ {}; {} }}", l, r)
            }
            ExprKind::Assign { op, lhs, rhs } => {
                // LHS が MacroCall/Call の場合は展開形式で lvalue アクセス
                let l = if let ExprKind::MacroCall { expanded, .. } = &lhs.kind {
                    self.expr_to_rust(expanded, info)
                } else if let ExprKind::Call { func, args } = &lhs.kind {
                    self.try_expand_call_as_lvalue(func, args, info)
                        .unwrap_or_else(|| self.expr_to_rust(lhs, info))
                } else {
                    self.expr_to_rust(lhs, info)
                };
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
                } else if self.infer_type_hint(condition, info) == TypeHint::Pointer {
                    format!("assert!(!{}.is_null())", cond)
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
                        BlockItem::Decl(decl) => {
                            let decl_str = self.decl_to_rust_let(decl, "");
                            for line in decl_str.lines() {
                                let trimmed = line.trim();
                                if !trimmed.is_empty() {
                                    parts.push(trimmed.strip_suffix(';').unwrap_or(trimmed).to_string());
                                }
                            }
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
            ExprKind::MacroCall { name, args, expanded, .. } => {
                // マクロ呼び出しの処理：
                // - マクロが利用可能（生成対象 or bindings に存在）ならマクロ呼び出し形式
                // - そうでなければ展開形式
                if self.should_emit_as_macro_call(*name) {
                    let name_str = escape_rust_keyword(self.interner.get(*name));

                    // THX マクロで my_perl が不足しているかチェック
                    let needs_my_perl = self.needs_my_perl_for_call(*name, args.len());

                    let mut a: Vec<String> = if needs_my_perl {
                        vec!["my_perl".to_string()]
                    } else {
                        vec![]
                    };
                    a.extend(args.iter().map(|arg| self.expr_to_rust(arg, info)));
                    format!("{}({})", name_str, a.join(", "))
                } else {
                    // 展開形式で出力
                    self.expr_to_rust(expanded, info)
                }
            }
            ExprKind::BuiltinCall { name, args } => {
                let func_name = self.interner.get(*name);
                // offsetof(type, field) → std::mem::offset_of!(Type, field_path)
                if (func_name == "offsetof" || func_name == "__builtin_offsetof"
                        || func_name == "STRUCT_OFFSET")
                    && args.len() == 2
                {
                    let type_str = match &args[0] {
                        crate::ast::BuiltinArg::TypeName(tn) => self.type_name_to_rust(tn),
                        crate::ast::BuiltinArg::Expr(e) => self.expr_to_rust(e, info),
                    };
                    let field_expr = match &args[1] {
                        crate::ast::BuiltinArg::Expr(e) => self.expr_to_field_path(e),
                        _ => None,
                    };
                    if let Some(fp) = field_expr {
                        return format!("std::mem::offset_of!({}, {})", type_str, fp);
                    }
                }
                // フォールバック: 通常の関数呼び出しとして出力
                let a: Vec<String> = args.iter().map(|arg| match arg {
                    crate::ast::BuiltinArg::Expr(e) => self.expr_to_rust(e, info),
                    crate::ast::BuiltinArg::TypeName(tn) => self.type_name_to_rust(tn),
                }).collect();
                format!("{}({})", func_name, a.join(", "))
            }
            _ => {
                self.todo_marker(&format!("{:?}", std::mem::discriminant(&expr.kind)))
            }
        }
    }

    /// 式を Rust コードに変換（型ヒント付き）
    ///
    /// 型ヒントがポインタ型で式が IntLit(0) なら null_mut()/null() に変換。
    /// 型ヒントが bool で式が IntLit(0)/IntLit(1) なら false/true に変換。
    fn expr_with_type_hint(&mut self, expr: &Expr, info: &MacroInferInfo, type_hint: Option<&str>) -> String {
        if let Some(ty) = type_hint {
            if is_pointer_type_str(ty) && is_null_literal(expr) {
                return null_ptr_expr(ty);
            }
            if ty == "bool" {
                match &expr.kind {
                    ExprKind::IntLit(0) => return "false".to_string(),
                    ExprKind::IntLit(1) => return "true".to_string(),
                    _ => {}
                }
            }
        }
        self.expr_to_rust(expr, info)
    }

    /// 式を Rust コードに変換（型ヒント付き、inline 関数用）
    fn expr_with_type_hint_inline(&mut self, expr: &Expr, type_hint: Option<&str>) -> String {
        if let Some(ty) = type_hint {
            if is_pointer_type_str(ty) && is_null_literal(expr) {
                return null_ptr_expr(ty);
            }
            if ty == "bool" {
                match &expr.kind {
                    ExprKind::IntLit(0) => return "false".to_string(),
                    ExprKind::IntLit(1) => return "true".to_string(),
                    _ => {}
                }
            }
        }
        self.expr_to_rust_inline(expr)
    }

    /// 文を Rust コードに変換
    fn stmt_to_rust(&mut self, stmt: &Stmt, info: &MacroInferInfo) -> String {
        match stmt {
            Stmt::Expr(Some(expr), _) => {
                format!("{};", self.expr_to_rust(expr, info))
            }
            Stmt::Expr(None, _) => ";".to_string(),
            Stmt::Return(Some(expr), _) => {
                if let Some(ref rt) = self.current_return_type {
                    if is_pointer_type_str(rt) && is_null_literal(expr) {
                        return format!("return {};", null_ptr_expr(rt));
                    }
                    if rt == "bool" {
                        match &expr.kind {
                            ExprKind::IntLit(0) => return "return false;".to_string(),
                            ExprKind::IntLit(1) => return "return true;".to_string(),
                            _ => {}
                        }
                    }
                }
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
                // 型パラメータなら generic 名に置換
                if let Some(generic_name) = self.current_type_param_map.get(name) {
                    return generic_name.clone();
                }
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
        self.current_return_type = Some(return_type.clone());

        // パラメータの型情報を収集 + ローカルスコープに登録
        for d in &func_def.declarator.derived {
            if let DerivedDecl::Function(param_list) = d {
                for p in &param_list.params {
                    if let Some(ref declarator) = p.declarator {
                        if let Some(param_name) = declarator.name {
                            let ty = self.param_type_only(p);
                            self.current_param_types.insert(param_name, ty);
                            self.current_local_names.insert(param_name);
                        }
                    }
                }
            }
        }

        // 本体のローカル変数宣言もスコープに追加
        for item in &func_def.body.items {
            if let BlockItem::Decl(decl) = item {
                self.collect_decl_names(decl);
            }
        }

        // THX 依存性を判定
        let is_thx_dependent = self.is_inline_fn_thx_dependent(&func_def.declarator.derived);
        let thx_info = if is_thx_dependent { " [THX]" } else { "" };

        // ドキュメントコメント
        self.writeln(&format!("/// {}{} - inline function", name_str, thx_info));
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

    /// inline 関数が THX 依存かどうかを判定
    ///
    /// 最初のパラメータが `my_perl` という名前であれば THX 依存とみなす。
    fn is_inline_fn_thx_dependent(&self, derived: &[DerivedDecl]) -> bool {
        for d in derived {
            if let DerivedDecl::Function(param_list) = d {
                if let Some(first_param) = param_list.params.first() {
                    if let Some(ref declarator) = first_param.declarator {
                        if let Some(name) = declarator.name {
                            let name_str = self.interner.get(name);
                            return name_str == "my_perl";
                        }
                    }
                }
                return false;
            }
        }
        false
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
                    // LHS が MacroCall/Call の場合は展開形式で lvalue アクセス
                    let l = if let ExprKind::MacroCall { expanded, .. } = &lhs.kind {
                        self.expr_to_rust_inline(expanded)
                    } else if let ExprKind::Call { func, args } = &lhs.kind {
                        self.try_expand_call_as_lvalue_inline(func, args)
                            .unwrap_or_else(|| self.expr_to_rust_inline(lhs))
                    } else {
                        self.expr_to_rust_inline(lhs)
                    };
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
                if let Some(ref rt) = self.current_return_type {
                    if is_pointer_type_str(rt) && is_null_literal(expr) {
                        return format!("{}return {};", indent, null_ptr_expr(rt));
                    }
                    if rt == "bool" {
                        match &expr.kind {
                            ExprKind::IntLit(0) => return format!("{}return false;", indent),
                            ExprKind::IntLit(1) => return format!("{}return true;", indent),
                            _ => {}
                        }
                    }
                }
                format!("{}return {};", indent, self.expr_to_rust_inline(expr))
            }
            Stmt::Return(None, _) => format!("{}return;", indent),
            Stmt::If { cond, then_stmt, else_stmt, .. } => {
                let cond_str = self.expr_to_rust_inline(cond);
                // 条件が既に bool なら != 0 を追加しない
                let cond_bool = self.wrap_as_bool_condition_inline(cond, &cond_str);
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
                let cond_bool = self.wrap_as_bool_condition_inline(cond, &cond_str);
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
                    let cond_bool = self.wrap_as_bool_condition_inline(cond_expr, &cond_str);
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

    /// offsetof のフィールドパス式をドット区切り文字列に変換
    /// Ident("xnv_u") → "xnv_u"
    /// Member(Ident("xnv_u"), "xnv_nv") → "xnv_u.xnv_nv"
    fn expr_to_field_path(&self, expr: &Expr) -> Option<String> {
        match &expr.kind {
            ExprKind::Ident(name) => {
                Some(self.interner.get(*name).to_string())
            }
            ExprKind::Member { expr: base, member } => {
                let base_path = self.expr_to_field_path(base)?;
                let member_name = self.interner.get(*member);
                Some(format!("{}.{}", base_path, member_name))
            }
            _ => None,
        }
    }

    /// 式を Rust コードに変換（インライン関数用）
    fn expr_to_rust_inline(&mut self, expr: &Expr) -> String {
        match &expr.kind {
            ExprKind::Ident(name) => {
                // lvalue展開時のパラメータ置換
                if let Some(subst) = self.param_substitutions.get(name) {
                    return subst.clone();
                }
                let name_str = self.interner.get(*name);
                // libc 関数の使用を記録
                if LIBC_FUNCTIONS.contains(&name_str) {
                    self.used_libc_fns.insert(name_str.to_string());
                }
                // 未解決シンボルチェック
                // Note: type_param_map の名前が値コンテキストで出現する場合も
                // unresolved とする（ジェネリック誤検出の検出）
                if !self.current_local_names.contains(name)
                    && !self.enum_dict.is_enum_variant(*name)
                    && !self.known_symbols.contains(name_str)
                {
                    let s = name_str.to_string();
                    if !self.unresolved_names.contains(&s) {
                        self.unresolved_names.push(s);
                    }
                }
                escape_rust_keyword(name_str)
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
                // ポインタ == 0 / != 0 → .is_null() (マクロ codegen と対称)
                if matches!(op, BinOp::Eq | BinOp::Ne) {
                    if self.is_pointer_expr_inline(lhs) && is_null_literal(rhs) {
                        let l = self.expr_to_rust_inline(lhs);
                        return if *op == BinOp::Eq {
                            format!("{}.is_null()", l)
                        } else {
                            format!("!{}.is_null()", l)
                        };
                    }
                    if self.is_pointer_expr_inline(rhs) && is_null_literal(lhs) {
                        let r = self.expr_to_rust_inline(rhs);
                        return if *op == BinOp::Eq {
                            format!("{}.is_null()", r)
                        } else {
                            format!("!{}.is_null()", r)
                        };
                    }
                }
                let l = self.expr_to_rust_inline(lhs);
                let r = self.expr_to_rust_inline(rhs);
                // 論理演算子の場合、オペランドを bool に変換
                match op {
                    BinOp::LogAnd | BinOp::LogOr => {
                        let l_bool = self.wrap_as_bool_condition_inline(lhs, &l);
                        let r_bool = self.wrap_as_bool_condition_inline(rhs, &r);
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
                    // __builtin_unreachable() -> std::hint::unreachable_unchecked()
                    if func_name == "__builtin_unreachable" {
                        return "std::hint::unreachable_unchecked()".to_string();
                    }
                    // __builtin_ctz(x) / __builtin_ctzl(x) -> (x).trailing_zeros()
                    if (func_name == "__builtin_ctz" || func_name == "__builtin_ctzl")
                        && args.len() == 1
                    {
                        let arg = self.expr_to_rust_inline(&args[0]);
                        return format!("({}).trailing_zeros()", arg);
                    }
                    // __builtin_clz(x) / __builtin_clzl(x) -> (x).leading_zeros()
                    if (func_name == "__builtin_clz" || func_name == "__builtin_clzl")
                        && args.len() == 1
                    {
                        let arg = self.expr_to_rust_inline(&args[0]);
                        return format!("({}).leading_zeros()", arg);
                    }
                    // ASSERT_IS_LITERAL(s) -> s, ASSERT_IS_PTR(x) -> x, ASSERT_NOT_PTR(x) -> x
                    if (func_name == "ASSERT_IS_LITERAL"
                        || func_name == "ASSERT_IS_PTR"
                        || func_name == "ASSERT_NOT_PTR")
                        && args.len() == 1
                    {
                        return self.expr_to_rust_inline(&args[0]);
                    }
                    // offsetof(type, field) → std::mem::offset_of!(Type, field_path)
                    if (func_name == "offsetof" || func_name == "__builtin_offsetof")
                        && args.len() == 2
                    {
                        let type_name = self.expr_to_rust_inline(&args[0]);
                        if let Some(field_path) = self.expr_to_field_path(&args[1]) {
                            return format!("std::mem::offset_of!({}, {})", type_name, field_path);
                        }
                    }
                }
                let f = self.expr_to_rust_inline(func);

                // THX マクロで my_perl が不足しているかチェック
                let needs_my_perl = if let ExprKind::Ident(name) = &func.kind {
                    self.needs_my_perl_for_call(*name, args.len())
                } else {
                    false
                };

                // ジェネリック型パラメータのチェック
                let callee_generics = if let ExprKind::Ident(name) = &func.kind {
                    self.get_callee_generic_params(*name).cloned()
                } else {
                    None
                };

                if let Some(ref generics) = callee_generics {
                    let mut type_args = Vec::new();
                    let mut value_args: Vec<String> = if needs_my_perl {
                        vec!["my_perl".to_string()]
                    } else {
                        vec![]
                    };
                    for (i, arg) in args.iter().enumerate() {
                        if generics.contains_key(&(i as i32)) {
                            type_args.push(self.expr_to_rust_inline(arg));
                        } else {
                            value_args.push(self.expr_to_rust_inline(arg));
                        }
                    }
                    return format!("{}::<{}>({})", f, type_args.join(", "), value_args.join(", "));
                }

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
                if self.is_bitfield_method(m) {
                    format!("({}).{}()", e, m)
                } else {
                    format!("({}).{}", e, m)
                }
            }
            ExprKind::PtrMember { expr: base, member } => {
                let e = self.expr_to_rust_inline(base);
                let m = self.interner.get(*member);
                if self.is_bitfield_method(m) {
                    format!("(*{}).{}()", e, m)
                } else {
                    format!("(*{}).{}", e, m)
                }
            }
            ExprKind::Index { expr: base, index } => {
                let b = self.expr_to_rust_inline(base);
                let i = self.expr_to_rust_inline(index);
                if self.is_static_array_expr(base) {
                    format!("(*{}.as_ptr().offset({} as isize))", b, i)
                } else {
                    format!("(*{}.offset({} as isize))", b, i)
                }
            }
            ExprKind::Cast { type_name, expr: inner } => {
                let e = self.expr_to_rust_inline(inner);
                let t = self.type_name_to_rust(type_name);
                // void キャストは式の値を捨てる（(expr as ()) は無効）
                if t == "()" {
                    format!("{{ {}; }}", e)
                } else if t == "bool" {
                    // Rust では整数を as bool でキャストできない
                    format!("(({}) != 0)", e)
                } else if self.is_enum_cast_target(type_name) {
                    // enum へのキャストは transmute を使用
                    format!("std::mem::transmute::<_, {}>({})", t, e)
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
                let e = if let ExprKind::MacroCall { expanded, .. } = &inner.kind {
                    self.expr_to_rust_inline(expanded)
                } else if let ExprKind::Call { func, args } = &inner.kind {
                    self.try_expand_call_as_lvalue_inline(func, args)
                        .unwrap_or_else(|| self.expr_to_rust_inline(inner))
                } else {
                    self.expr_to_rust_inline(inner)
                };
                format!("{{ {} += 1; {} }}", e, e)
            }
            ExprKind::PreDec(inner) => {
                let e = if let ExprKind::MacroCall { expanded, .. } = &inner.kind {
                    self.expr_to_rust_inline(expanded)
                } else if let ExprKind::Call { func, args } = &inner.kind {
                    self.try_expand_call_as_lvalue_inline(func, args)
                        .unwrap_or_else(|| self.expr_to_rust_inline(inner))
                } else {
                    self.expr_to_rust_inline(inner)
                };
                format!("{{ {} -= 1; {} }}", e, e)
            }
            ExprKind::PostInc(inner) => {
                let e = if let ExprKind::MacroCall { expanded, .. } = &inner.kind {
                    self.expr_to_rust_inline(expanded)
                } else if let ExprKind::Call { func, args } = &inner.kind {
                    self.try_expand_call_as_lvalue_inline(func, args)
                        .unwrap_or_else(|| self.expr_to_rust_inline(inner))
                } else {
                    self.expr_to_rust_inline(inner)
                };
                format!("{{ let _t = {}; {} += 1; _t }}", e, e)
            }
            ExprKind::PostDec(inner) => {
                let e = if let ExprKind::MacroCall { expanded, .. } = &inner.kind {
                    self.expr_to_rust_inline(expanded)
                } else if let ExprKind::Call { func, args } = &inner.kind {
                    self.try_expand_call_as_lvalue_inline(func, args)
                        .unwrap_or_else(|| self.expr_to_rust_inline(inner))
                } else {
                    self.expr_to_rust_inline(inner)
                };
                format!("{{ let _t = {}; {} -= 1; _t }}", e, e)
            }
            ExprKind::UnaryPlus(inner) => self.expr_to_rust_inline(inner),
            ExprKind::UnaryMinus(inner) => {
                let e = self.expr_to_rust_inline(inner);
                if is_unsigned_cast_expr(&e) {
                    format!("({}).wrapping_neg()", e.trim_start_matches('-'))
                } else {
                    format!("(-{})", e)
                }
            }
            ExprKind::BitNot(inner) => {
                let e = self.expr_to_rust_inline(inner);
                format!("(!{})", e)
            }
            ExprKind::LogNot(inner) => {
                let e = self.expr_to_rust_inline(inner);
                // 内部式を bool に変換してから論理否定
                let cond = self.wrap_as_bool_condition_inline(inner, &e);
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
                // 条件が既に bool なら != 0 を追加しない
                let cond_str = self.wrap_as_bool_condition_inline(cond, &c);
                let type_hint = self.current_return_type.clone();
                let t = self.expr_with_type_hint_inline(then_expr, type_hint.as_deref());
                let e = self.expr_with_type_hint_inline(else_expr, type_hint.as_deref());
                format!("(if {} {{ {} }} else {{ {} }})", cond_str, t, e)
            }
            ExprKind::Comma { lhs, rhs } => {
                let l = self.expr_to_rust_inline(lhs);
                let r = self.expr_to_rust_inline(rhs);
                format!("{{ {}; {} }}", l, r)
            }
            ExprKind::Assign { op, lhs, rhs } => {
                // LHS が MacroCall/Call の場合は展開形式で lvalue アクセス
                let l = if let ExprKind::MacroCall { expanded, .. } = &lhs.kind {
                    self.expr_to_rust_inline(expanded)
                } else if let ExprKind::Call { func, args } = &lhs.kind {
                    self.try_expand_call_as_lvalue_inline(func, args)
                        .unwrap_or_else(|| self.expr_to_rust_inline(lhs))
                } else {
                    self.expr_to_rust_inline(lhs)
                };
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
                } else if self.is_pointer_expr_inline(condition) {
                    format!("assert!(!{}.is_null())", cond)
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
                        BlockItem::Decl(decl) => {
                            let decl_str = self.decl_to_rust_let(decl, "");
                            for line in decl_str.lines() {
                                let trimmed = line.trim();
                                if !trimmed.is_empty() {
                                    parts.push(trimmed.strip_suffix(';').unwrap_or(trimmed).to_string());
                                }
                            }
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
            ExprKind::BuiltinCall { name, args } => {
                let func_name = self.interner.get(*name);
                // offsetof(type, field) → std::mem::offset_of!(Type, field_path)
                if (func_name == "offsetof" || func_name == "__builtin_offsetof"
                        || func_name == "STRUCT_OFFSET")
                    && args.len() == 2
                {
                    let type_str = match &args[0] {
                        crate::ast::BuiltinArg::TypeName(tn) => self.type_name_to_rust(tn),
                        crate::ast::BuiltinArg::Expr(e) => self.expr_to_rust_inline(e),
                    };
                    let field_expr = match &args[1] {
                        crate::ast::BuiltinArg::Expr(e) => self.expr_to_field_path(e),
                        _ => None,
                    };
                    if let Some(fp) = field_expr {
                        return format!("std::mem::offset_of!({}, {})", type_str, fp);
                    }
                }
                // フォールバック: 通常の関数呼び出しとして出力
                let a: Vec<String> = args.iter().map(|arg| match arg {
                    crate::ast::BuiltinArg::Expr(e) => self.expr_to_rust_inline(e),
                    crate::ast::BuiltinArg::TypeName(tn) => self.type_name_to_rust(tn),
                }).collect();
                format!("{}({})", func_name, a.join(", "))
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
        bindings_info: BindingsInfo,
        config: CodegenConfig,
    ) -> Self {
        Self {
            writer,
            interner,
            enum_dict,
            macro_ctx,
            bindings_info,
            config,
            stats: CodegenStats::default(),
            used_libc_fns: HashSet::new(),
            successfully_generated_inlines: HashSet::new(),
            generatable_macros: HashSet::new(),
        }
    }

    /// 統計情報を取得
    pub fn stats(&self) -> &CodegenStats {
        &self.stats
    }

    /// 全体を生成
    // デバッグ用: ビルド時のタイムスタンプを埋め込む場合はコメントを外す
    // const BUILD_TIMESTAMP: &'static str = "2025-01-24T17:50:00+09:00";

    pub fn generate(&mut self, result: &InferResult) -> io::Result<()> {
        // 既知シンボル集合を構築（未解決シンボル検出用）
        let known_symbols = KnownSymbols::new(result, self.interner);

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

        // マクロの生成可能性を事前計算（inline→macro カスケード検出用）
        self.precompute_macro_generability(result, &known_symbols);

        // inline 関数セクション
        if self.config.emit_inline_fns {
            self.generate_inline_fns(result, &known_symbols)?;
        }

        // マクロセクション
        if self.config.emit_macros {
            self.generate_macros(result, &known_symbols)?;
        }

        // 使用された libc 関数の use 文を出力（rustfmt が先頭に移動する）
        if !self.used_libc_fns.is_empty() {
            let mut fns: Vec<_> = self.used_libc_fns.iter().cloned().collect();
            fns.sort();
            writeln!(self.writer, "use libc::{{{}}};", fns.join(", "))?;
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

    /// マクロの生成可能性を事前計算
    ///
    /// inline 関数生成前に呼び出し、マクロ→マクロのカスケード検査をシミュレートして
    /// 正常に生成可能なマクロの集合を構築する。
    /// inline→macro のカスケード検出に使用する。
    fn precompute_macro_generability(&mut self, result: &InferResult, known_symbols: &KnownSymbols) {
        // 対象マクロを収集
        let macros: Vec<_> = result.infer_ctx.macros.iter()
            .filter(|(_, info)| self.should_include_macro(info))
            .collect();
        let included_set: HashSet<InternedStr> = macros.iter().map(|(n, _)| **n).collect();

        // 依存順にソート（葉マクロ先頭）
        let sorted_names = self.topological_sort_macros(&macros);

        for name in sorted_names {
            let info = result.infer_ctx.macros.get(&name).unwrap();

            // カスケード検査: called_functions が生成不可なマクロを含むか
            let has_cascade_failure = info.called_functions.iter().any(|called| {
                if included_set.contains(called) {
                    return result.infer_ctx.macros.get(called)
                        .map(|u| u.is_parseable() && !u.calls_unavailable)
                        .unwrap_or(false)
                        && !self.generatable_macros.contains(called);
                }
                false
            });
            if has_cascade_failure {
                continue;
            }

            // get_macro_status + trial codegen による判定
            let status = self.get_macro_status(info);
            if status == GenerateStatus::Success {
                // 実際に codegen を試行して完全性を確認
                let codegen = RustCodegen::new(
                    self.interner, self.enum_dict, self.macro_ctx,
                    self.bindings_info.clone(), &known_symbols,
                );
                let generated = codegen.generate_macro(info);
                if generated.is_complete() && !generated.has_unresolved_names() {
                    self.generatable_macros.insert(name);
                }
            }
        }
    }

    /// inline 関数セクションを生成
    ///
    /// 2パス方式:
    /// - Pass 1: 各 inline 関数を生成し、結果を蓄積
    /// - Pass 2: カスケード検査 — 生成成功した関数が失敗した inline 関数や
    ///   マクロを呼び出している場合、CASCADE_UNAVAILABLE に降格
    pub fn generate_inline_fns(&mut self, result: &InferResult, known_symbols: &KnownSymbols) -> io::Result<()> {
        writeln!(self.writer, "// =============================================================================")?;
        writeln!(self.writer, "// Inline Functions")?;
        writeln!(self.writer, "// =============================================================================")?;
        writeln!(self.writer)?;

        // 名前順にソート
        let mut fns: Vec<_> = result.inline_fn_dict.iter()
            .filter(|(_, func_def)| func_def.is_target)
            .collect();
        fns.sort_by_key(|(name, _)| self.interner.get(**name));

        // 対象 inline 関数名の集合
        let inline_set: HashSet<InternedStr> = fns.iter().map(|(n, _)| **n).collect();

        // Pass 1: 各 inline 関数を生成
        enum InlineGenResult {
            CallsUnavailable,
            ContainsGoto,
            UnresolvedNames { code: String, unresolved: Vec<String> },
            Incomplete { code: String },
            Success { code: String, used_libc: HashSet<String> },
        }

        let mut gen_results: Vec<(InternedStr, InlineGenResult)> = Vec::new();

        for (name, func_def) in &fns {
            // 事前に unavailable と判定された関数はスキップ
            if result.inline_fn_dict.is_calls_unavailable(**name) {
                gen_results.push((**name, InlineGenResult::CallsUnavailable));
                continue;
            }

            if block_items_contain_goto(&func_def.body.items) {
                gen_results.push((**name, InlineGenResult::ContainsGoto));
                continue;
            }

            let codegen = RustCodegen::new(self.interner, self.enum_dict, self.macro_ctx, self.bindings_info.clone(), known_symbols);
            let generated = codegen.generate_inline_fn(**name, func_def);

            if generated.has_unresolved_names() {
                gen_results.push((**name, InlineGenResult::UnresolvedNames {
                    code: generated.code,
                    unresolved: generated.unresolved_names,
                }));
            } else if generated.is_complete() {
                gen_results.push((**name, InlineGenResult::Success {
                    code: generated.code,
                    used_libc: generated.used_libc_fns,
                }));
            } else {
                gen_results.push((**name, InlineGenResult::Incomplete {
                    code: generated.code,
                }));
            }
        }

        // Pass 1.5: successfully_generated_inlines を構築（Success のみ）
        for (name, gen_result) in &gen_results {
            if matches!(gen_result, InlineGenResult::Success { .. }) {
                self.successfully_generated_inlines.insert(*name);
            }
        }

        // Pass 2: カスケード検査 — Success だが失敗した依存先を呼び出す場合は降格
        // InlineFnDict の called_functions を使用（ad-hoc 収集不要）
        // inline→inline と inline→macro の両方を検査
        // 繰り返し伝播（fixpoint）
        let mut changed = true;
        while changed {
            changed = false;
            let current_success = self.successfully_generated_inlines.clone();
            for (name, _) in &gen_results {
                if !current_success.contains(name) {
                    continue;
                }
                if let Some(calls) = result.inline_fn_dict.get_called_functions(*name) {
                    let has_unavailable = calls.iter().any(|called| {
                        // inline→inline: 対象 inline が生成失敗
                        if inline_set.contains(called) && !current_success.contains(called) {
                            return true;
                        }
                        // inline→macro: 対象マクロが生成不可
                        if let Some(macro_info) = result.infer_ctx.macros.get(called) {
                            if macro_info.is_target && self.should_include_macro(macro_info) {
                                if !self.generatable_macros.contains(called) {
                                    return true;
                                }
                            }
                        }
                        false
                    });
                    if has_unavailable {
                        self.successfully_generated_inlines.remove(name);
                        changed = true;
                    }
                }
            }
        }

        // Pass 3: 出力
        for (name, gen_result) in gen_results {
            match gen_result {
                InlineGenResult::CallsUnavailable => {
                    let name_str = self.interner.get(name);
                    let unavailable: Vec<String> = result.inline_fn_dict.get_called_functions(name)
                        .map(|calls| calls.iter()
                            .filter(|c| {
                                // 呼び出し先が unavailable な inline 関数、
                                // または unavailable なマクロ、
                                // またはどこにも定義がない
                                let is_unavailable_inline = result.inline_fn_dict.get(**c).is_some()
                                    && result.inline_fn_dict.is_calls_unavailable(**c);
                                let is_unavailable_macro = result.infer_ctx.macros.get(c)
                                    .map(|info| info.calls_unavailable)
                                    .unwrap_or(false);
                                is_unavailable_inline || is_unavailable_macro
                            })
                            .map(|c| self.interner.get(*c).to_string())
                            .collect())
                        .unwrap_or_default();
                    writeln!(self.writer, "// [CASCADE_UNAVAILABLE] {} - calls unavailable: {}",
                        name_str, unavailable.join(", "))?;
                    writeln!(self.writer)?;
                    self.stats.inline_fns_cascade_unavailable += 1;
                }
                InlineGenResult::ContainsGoto => {
                    let name_str = self.interner.get(name);
                    writeln!(self.writer, "// [CONTAINS_GOTO] {} - excluded (contains goto)", name_str)?;
                    writeln!(self.writer)?;
                    self.stats.inline_fns_contains_goto += 1;
                }
                InlineGenResult::UnresolvedNames { code, unresolved } => {
                    let name_str = self.interner.get(name);
                    writeln!(self.writer, "// [UNRESOLVED_NAMES] {} - inline function", name_str)?;
                    writeln!(self.writer, "// Unresolved: {}", unresolved.join(", "))?;
                    for line in code.lines() {
                        writeln!(self.writer, "// {}", line)?;
                    }
                    writeln!(self.writer)?;
                    self.stats.inline_fns_unresolved_names += 1;
                }
                InlineGenResult::Incomplete { code } => {
                    let name_str = self.interner.get(name);
                    writeln!(self.writer, "// [CODEGEN_INCOMPLETE] {} - inline function", name_str)?;
                    for line in code.lines() {
                        writeln!(self.writer, "// {}", line)?;
                    }
                    writeln!(self.writer)?;
                    self.stats.inline_fns_type_incomplete += 1;
                }
                InlineGenResult::Success { code, used_libc } => {
                    if self.successfully_generated_inlines.contains(&name) {
                        // 正常出力
                        write!(self.writer, "{}", code)?;
                        self.used_libc_fns.extend(used_libc.iter().cloned());
                        self.stats.inline_fns_success += 1;
                    } else {
                        // カスケード降格: 呼び出し先の inline 関数が codegen 時に失敗
                        let name_str = self.interner.get(name);
                        let unavailable: Vec<String> = result.inline_fn_dict.get_called_functions(name)
                            .map(|calls| calls.iter()
                                .filter(|c| inline_set.contains(c) && !self.successfully_generated_inlines.contains(c))
                                .map(|c| self.interner.get(*c).to_string())
                                .collect())
                            .unwrap_or_default();
                        writeln!(self.writer, "// [CASCADE_UNAVAILABLE] {} - dependency not generated: {}",
                            name_str, unavailable.join(", "))?;
                        for line in code.lines() {
                            writeln!(self.writer, "// {}", line)?;
                        }
                        writeln!(self.writer)?;
                        self.stats.inline_fns_cascade_unavailable += 1;
                    }
                }
            }
        }

        writeln!(self.writer)?;
        Ok(())
    }

    /// マクロセクションを生成
    pub fn generate_macros(&mut self, result: &InferResult, known_symbols: &KnownSymbols) -> io::Result<()> {
        writeln!(self.writer, "// =============================================================================")?;
        writeln!(self.writer, "// Macro Functions")?;
        writeln!(self.writer, "// =============================================================================")?;
        writeln!(self.writer)?;

        // 対象マクロを収集
        let macros: Vec<_> = result.infer_ctx.macros.iter()
            .filter(|(_, info)| self.should_include_macro(info))
            .collect();
        let included_set: HashSet<InternedStr> = macros.iter().map(|(n, _)| **n).collect();

        // 依存順にソート（葉マクロ先頭）
        let sorted_names = self.topological_sort_macros(&macros);

        // 正常生成されたマクロを追跡
        let mut successfully_generated: HashSet<InternedStr> = HashSet::new();

        for name in sorted_names {
            let info = result.infer_ctx.macros.get(&name).unwrap();

            // ── カスケード検査 ──
            // called_functions のうち、生成対象だが生成に失敗した関数があれば
            // カスケード失敗。マクロ→マクロ依存と マクロ→inline 関数依存の両方を検査。
            // （uses ではなく called_functions を使う：uses はインライン展開された
            //   マクロも含むが、called_functions は AST 上の Call 式のみ）
            let unavailable_deps: Vec<String> = info.called_functions.iter()
                .filter(|called| {
                    // Case 1: マクロ→マクロ依存
                    if included_set.contains(called) {
                        return result.infer_ctx.macros.get(called)
                            .map(|u| u.is_parseable() && !u.calls_unavailable)
                            .unwrap_or(false)
                            && !successfully_generated.contains(called);
                    }
                    // Case 2: マクロ→inline 関数依存
                    if result.inline_fn_dict.get(**called)
                        .map(|f| f.is_target)
                        .unwrap_or(false)
                    {
                        return !self.successfully_generated_inlines.contains(called);
                    }
                    false
                })
                .map(|called| self.interner.get(*called).to_string())
                .collect();

            if !unavailable_deps.is_empty() {
                self.generate_macro_cascade_unavailable(info, &unavailable_deps)?;
                self.stats.macros_cascade_unavailable += 1;
                continue;
            }

            // ── 既存のステータス判定と生成 ──
            let status = self.get_macro_status(info);
            match status {
                GenerateStatus::Success => {
                    // 新しい RustCodegen を使ってマクロを生成
                    let codegen = RustCodegen::new(self.interner, self.enum_dict, self.macro_ctx, self.bindings_info.clone(), known_symbols);
                    let generated = codegen.generate_macro(info);

                    if generated.has_unresolved_names() {
                        // 未解決シンボルあり：コメントアウトして出力
                        let name_str = self.interner.get(info.name);
                        let thx_info = if info.is_thx_dependent { " [THX]" } else { "" };
                        writeln!(self.writer, "// [UNRESOLVED_NAMES] {}{} - macro function", name_str, thx_info)?;
                        writeln!(self.writer, "// Unresolved: {}", generated.unresolved_names.join(", "))?;
                        for line in generated.code.lines() {
                            writeln!(self.writer, "// {}", line)?;
                        }
                        writeln!(self.writer)?;
                        self.stats.macros_unresolved_names += 1;
                    } else if generated.is_complete() {
                        // 完全な生成：そのまま出力
                        write!(self.writer, "{}", generated.code)?;
                        self.used_libc_fns.extend(generated.used_libc_fns.iter().cloned());
                        self.stats.macros_success += 1;
                        successfully_generated.insert(name);
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
                GenerateStatus::ContainsGoto => {
                    let name_str = self.interner.get(info.name);
                    writeln!(self.writer, "// [CONTAINS_GOTO] {} - excluded (contains goto)", name_str)?;
                    writeln!(self.writer)?;
                }
                GenerateStatus::Skip => {
                    // 何もしない
                }
            }
        }

        Ok(())
    }

    /// マクロを依存順にソート（葉マクロ先頭、循環はアルファベット順で末尾）
    ///
    /// Kahn's algorithm を使用。`info.uses` の関係から DAG を構築し、
    /// 入次数 0 のノードから順に処理する。
    fn topological_sort_macros(
        &self,
        macros: &[(&InternedStr, &MacroInferInfo)],
    ) -> Vec<InternedStr> {
        use std::collections::VecDeque;

        let macro_set: HashSet<InternedStr> = macros.iter().map(|(n, _)| **n).collect();

        // 入次数マップと逆隣接リスト（dependency → dependents）を構築
        let mut in_degree: HashMap<InternedStr, usize> = HashMap::new();
        let mut dependents: HashMap<InternedStr, Vec<InternedStr>> = HashMap::new();

        for (name, _) in macros {
            in_degree.insert(**name, 0);
        }

        for (name, info) in macros {
            for used in &info.uses {
                if macro_set.contains(used) {
                    // name が used に依存 → used から name への辺
                    *in_degree.entry(**name).or_insert(0) += 1;
                    dependents.entry(*used).or_default().push(**name);
                }
            }
        }

        // 入次数 0 のマクロをキューに投入（アルファベット順で安定化）
        let mut queue: VecDeque<InternedStr> = {
            let mut zeros: Vec<_> = in_degree.iter()
                .filter(|(_, deg)| **deg == 0)
                .map(|(name, _)| *name)
                .collect();
            zeros.sort_by_key(|n| self.interner.get(*n));
            zeros.into_iter().collect()
        };

        let mut result = Vec::with_capacity(macros.len());

        while let Some(name) = queue.pop_front() {
            result.push(name);
            if let Some(deps) = dependents.get(&name) {
                // 依存先の入次数を減算
                let mut newly_ready: Vec<InternedStr> = Vec::new();
                for dep in deps {
                    if let Some(deg) = in_degree.get_mut(dep) {
                        *deg -= 1;
                        if *deg == 0 {
                            newly_ready.push(*dep);
                        }
                    }
                }
                // アルファベット順で安定化
                newly_ready.sort_by_key(|n| self.interner.get(*n));
                for n in newly_ready {
                    queue.push_back(n);
                }
            }
        }

        // 残り（循環メンバー）をアルファベット順で末尾に追加
        if result.len() < macros.len() {
            let result_set: HashSet<_> = result.iter().copied().collect();
            let mut remaining: Vec<_> = macro_set.iter()
                .filter(|n| !result_set.contains(n))
                .copied()
                .collect();
            remaining.sort_by_key(|n| self.interner.get(*n));
            result.extend(remaining);
        }

        result
    }

    /// カスケード依存によるコメントアウト出力
    fn generate_macro_cascade_unavailable(
        &mut self,
        info: &MacroInferInfo,
        unavailable_deps: &[String],
    ) -> io::Result<()> {
        let name_str = self.interner.get(info.name);
        let thx_info = if info.is_thx_dependent { " [THX]" } else { "" };
        writeln!(self.writer,
            "// [CASCADE_UNAVAILABLE] {}{} - dependency not generated: {}",
            name_str, thx_info, unavailable_deps.join(", "))?;
        writeln!(self.writer)?;
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
            ParseResult::Statement(items) => {
                // goto を含むマクロは除外
                if block_items_contain_goto(items) {
                    return GenerateStatus::ContainsGoto;
                }
                if info.is_fully_confirmed() {
                    GenerateStatus::Success
                } else {
                    GenerateStatus::TypeIncomplete
                }
            }
            ParseResult::Expression(_) => {
                if info.is_fully_confirmed() {
                    GenerateStatus::Success
                } else {
                    GenerateStatus::TypeIncomplete
                }
            }
        }
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
            "offsetof",
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
            "ASSERT_IS_LITERAL",
            "ASSERT_IS_PTR",
            "ASSERT_NOT_PTR",
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
