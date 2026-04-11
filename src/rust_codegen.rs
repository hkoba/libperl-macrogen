//! Rust コード生成モジュール
//!
//! 型推論結果から Rust コードを生成する。

use std::collections::{HashMap, HashSet};
use std::io::{self, Write};

use crate::ast::{AssertKind, AssignOp, BinOp, BlockItem, CompoundStmt, Declaration, DeclSpecs, DerivedDecl, Expr, ExprKind, ForInit, FunctionDef, Initializer, ParamDecl, Stmt, TypeSpec};

/// 式が生成されるコンテキスト（括弧制御用）
#[derive(Clone, Copy, PartialEq, Eq)]
enum ExprContext {
    /// デフォルト: 括弧が必要な可能性がある位置（Binary のオペランド等）
    Default,
    /// 括弧不要のトップレベル位置（関数引数、let RHS、return 値、代入 RHS）
    Top,
}
use crate::intern::InternedStr;
use crate::enum_dict::EnumDict;
use crate::infer_api::InferResult;
use crate::intern::StringInterner;
use crate::macro_infer::{MacroInferContext, MacroInferInfo, MacroParam, ParseResult};
use crate::rust_decl::RustDeclDict;
use crate::unified_type::UnifiedType;
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
pub fn is_boolean_expr(expr: &Expr) -> bool {
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


/// is_boolean_expr の再帰版: __builtin_expect(cond, val) を透過する
fn is_boolean_expr_recursive(expr: &Expr, interner: &StringInterner) -> bool {
    if is_boolean_expr(expr) {
        return true;
    }
    match &expr.kind {
        ExprKind::Call { func, args } => {
            if let ExprKind::Ident(name) = &func.kind {
                if interner.get(*name) == "__builtin_expect" && !args.is_empty() {
                    return is_boolean_expr_recursive(&args[0], interner);
                }
            }
        }
        // Cast to bool: 内側が bool なら全体も bool
        ExprKind::Cast { type_name, expr: inner } => {
            if type_name.specs.type_specs.iter().any(|ts| matches!(ts, TypeSpec::Bool)) {
                return true;
            }
            return is_boolean_expr_recursive(inner, interner);
        }
        _ => {}
    }
    false
}

/// コンテキスト付き bool 式判定: 呼び出し先マクロ/外部関数の戻り値型も考慮
pub fn is_boolean_expr_with_context(
    expr: &Expr,
    bool_return_macros: &HashSet<InternedStr>,
    bool_return_externals: &HashSet<InternedStr>,
) -> bool {
    if is_boolean_expr(expr) {
        return true;
    }
    match &expr.kind {
        ExprKind::Call { func, .. } => {
            if let ExprKind::Ident(name) = &func.kind {
                return bool_return_macros.contains(name)
                    || bool_return_externals.contains(name);
            }
        }
        ExprKind::MacroCall { name, .. } => {
            return bool_return_macros.contains(name)
                || bool_return_externals.contains(name);
        }
        _ => {}
    }
    false
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


/// 式文字列の最外レベルの不要な括弧を除去する。
/// "(expr)" → "expr" （先頭の '(' と末尾の ')' が対応する場合のみ）
fn strip_outer_parens(s: &str) -> &str {
    let s = s.trim();
    if s.len() < 2 || !s.starts_with('(') || !s.ends_with(')') {
        return s;
    }
    // 先頭の '(' と末尾の ')' が対応するかチェック
    let inner = &s[1..s.len() - 1];
    let mut depth = 0i32;
    for ch in inner.chars() {
        match ch {
            '(' | '{' | '[' => depth += 1,
            ')' | '}' | ']' => {
                depth -= 1;
                if depth < 0 {
                    // 内部で閉じ括弧が余る → 先頭と末尾は非対応
                    return s;
                }
            }
            _ => {}
        }
    }
    if depth == 0 {
        inner
    } else {
        s
    }
}

/// 式が NULL リテラル（整数 0 または (void*)0 のような Cast）かどうか判定
/// assert(expr || !"message") パターンの RHS からメッセージ文字列を抽出
fn extract_assert_message(expr: &Expr) -> Option<String> {
    if let ExprKind::LogNot(inner) = &expr.kind {
        if let ExprKind::StringLit(bytes) = &inner.kind {
            return Some(String::from_utf8_lossy(bytes).into_owned());
        }
    }
    None
}

/// assert 条件が `real_cond || !"message"` パターンかどうかを分解する
fn decompose_assert_with_message(condition: &Expr) -> Option<(&Expr, String)> {
    if let ExprKind::Binary { op: BinOp::LogOr, lhs, rhs } = &condition.kind {
        if let Some(msg) = extract_assert_message(rhs) {
            return Some((lhs, msg));
        }
    }
    None
}

/// Perl の SV サブタイプ（GV, HV, AV, CV, IO 等）から SV へのポインタキャストかどうかを判定
fn is_sv_subtype_cast(from: &UnifiedType, to: &UnifiedType) -> bool {
    let from_name = match from.inner_type() {
        Some(UnifiedType::Named(name)) => name.as_str(),
        _ => return false,
    };
    let to_name = match to.inner_type() {
        Some(UnifiedType::Named(name)) => name.as_str(),
        _ => return false,
    };
    // SV サブタイプのリスト
    const SV_SUBTYPES: &[&str] = &["GV", "HV", "AV", "CV", "IO", "p5rx", "REGEXP"];
    // SV ↔ サブタイプ（双方向）
    (SV_SUBTYPES.contains(&from_name) && to_name == "SV")
        || (from_name == "SV" && SV_SUBTYPES.contains(&to_name))
        // c_void ↔ 任意のポインタ
        || to_name == "c_void"
        || from_name == "c_void"
}

fn is_null_literal(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::IntLit(0) => true,
        ExprKind::Cast { expr: inner, .. } => is_null_literal(inner),
        _ => false,
    }
}

/// 生成済み文字列が bool 式かどうかを判定
fn is_string_bool_expr(s: &str) -> bool {
    s.ends_with("!= 0)")
        || s.ends_with("== 0)")
        || s.ends_with(".is_null()")
        || s.ends_with(" as bool)")
        || s == "true"
        || s == "false"
}

/// ポインタ型に対応する null ポインタ式を生成
fn null_ptr_expr(return_type: &UnifiedType) -> String {
    if return_type.is_const_pointer() {
        "std::ptr::null()".to_string()
    } else {
        "std::ptr::null_mut()".to_string()
    }
}

/// 型文字列を正規化して整数型なら canonical Rust primitive に変換
fn normalize_integer_type(ty: &str) -> Option<&'static str> {
    match ty {
        "u8" | "U8" | "c_uchar" => Some("u8"),
        "u16" | "U16" | "c_ushort" => Some("u16"),
        "u32" | "U32" | "c_uint" => Some("u32"),
        "u64" | "U64" | "UV" | "c_ulong" | "c_ulonglong"
            | "PERL_UINTMAX_T" => Some("u64"),
        "i8" | "I8" | "c_schar" | "c_char" => Some("i8"),
        "i16" | "I16" | "c_short" => Some("i16"),
        "i32" | "I32" | "c_int" => Some("i32"),
        "i64" | "I64" | "IV" | "c_long" | "c_longlong" => Some("i64"),
        "usize" | "STRLEN" => Some("usize"),
        "isize" | "SSize_t" | "ssize_t" | "PADOFFSET" => Some("isize"),
        _ => None,
    }
}

/// 64-bit プラットフォームで i64/isize, u64/usize を同一視して比較
fn integer_types_compatible(a: &str, b: &str) -> bool {
    if a == b { return true; }
    matches!((a, b),
        ("i64", "isize") | ("isize", "i64") |
        ("u64", "usize") | ("usize", "u64")
    )
}

/// 整数型の幅ランク (昇格順序判定用)
/// returns (is_signed, width_rank)
fn integer_type_rank(ty: &str) -> Option<(bool, u8)> {
    match normalize_integer_type(ty)? {
        "u8" => Some((false, 1)), "i8" => Some((true, 1)),
        "u16" => Some((false, 2)), "i16" => Some((true, 2)),
        "u32" => Some((false, 4)), "i32" => Some((true, 4)),
        "u64" => Some((false, 8)), "i64" => Some((true, 8)),
        "usize" => Some((false, 8)), "isize" => Some((true, 8)),
        _ => None,
    }
}

/// 二項ビット演算で C の整数昇格に従い広い方の型を返す
/// 同一正規化型なら None（キャスト不要）
fn wider_integer_type(a: &str, b: &str) -> Option<&'static str> {
    let na = normalize_integer_type(a)?;
    let nb = normalize_integer_type(b)?;
    if na == nb { return None; }
    let (a_signed, a_rank) = integer_type_rank(a)?;
    let (_b_signed, b_rank) = integer_type_rank(b)?;
    if a_rank == b_rank {
        // 同一幅: unsigned が勝つ (C規格 6.3.1.8)
        Some(if a_signed { nb } else { na })
    } else if a_rank > b_rank {
        Some(na)
    } else {
        Some(nb)
    }
}

/// マクロ本体を走査し、`&mut param` や代入先として使用されるパラメータを検出する
/// ポインタパラメータが *mut である必要があるかを判定する。
/// callee_const_params: 呼び出し先マクロで *const に確定したパラメータ情報
///   key = マクロ名(InternedStr), value = const パラメータの引数位置集合
pub fn collect_must_mut_pointer_params(
    parse_result: &ParseResult,
    params: &[MacroParam],
    callee_const_params: &HashMap<InternedStr, HashSet<usize>>,
) -> HashSet<InternedStr> {
    let param_names: HashSet<InternedStr> = params.iter().map(|p| p.name).collect();
    let mut result = HashSet::new();
    match parse_result {
        ParseResult::Expression(expr) => {
            collect_must_mut_from_expr(expr, &param_names, callee_const_params, &mut result);
        }
        ParseResult::Statement(items) => {
            for item in items {
                if let BlockItem::Stmt(stmt) = item {
                    collect_must_mut_from_stmt(stmt, &param_names, callee_const_params, &mut result);
                }
            }
        }
        ParseResult::Unparseable(_) => {}
    }
    result
}

pub fn collect_must_mut_from_stmt(
    stmt: &Stmt,
    params: &HashSet<InternedStr>,
    callee_const: &HashMap<InternedStr, HashSet<usize>>,
    result: &mut HashSet<InternedStr>,
) {
    match stmt {
        Stmt::Expr(Some(expr), _) | Stmt::Return(Some(expr), _) => {
            collect_must_mut_from_expr(expr, params, callee_const, result);
        }
        Stmt::Compound(compound) => {
            for item in &compound.items {
                match item {
                    BlockItem::Stmt(s) => collect_must_mut_from_stmt(s, params, callee_const, result),
                    BlockItem::Decl(decl) => {
                        for init_decl in &decl.declarators {
                            if let Some(Initializer::Expr(init)) = &init_decl.init {
                                collect_must_mut_from_expr(init, params, callee_const, result);
                            }
                        }
                    }
                }
            }
        }
        Stmt::If { cond, then_stmt, else_stmt, .. } => {
            collect_must_mut_from_expr(cond, params, callee_const, result);
            collect_must_mut_from_stmt(then_stmt, params, callee_const, result);
            if let Some(e) = else_stmt {
                collect_must_mut_from_stmt(e, params, callee_const, result);
            }
        }
        Stmt::While { cond, body, .. } | Stmt::DoWhile { cond, body, .. } => {
            collect_must_mut_from_expr(cond, params, callee_const, result);
            collect_must_mut_from_stmt(body, params, callee_const, result);
        }
        Stmt::For { init, cond, step, body, .. } => {
            if let Some(ForInit::Expr(e)) = init { collect_must_mut_from_expr(e, params, callee_const, result); }
            if let Some(e) = cond { collect_must_mut_from_expr(e, params, callee_const, result); }
            if let Some(e) = step { collect_must_mut_from_expr(e, params, callee_const, result); }
            collect_must_mut_from_stmt(body, params, callee_const, result);
        }
        Stmt::Switch { expr, body, .. } => {
            collect_must_mut_from_expr(expr, params, callee_const, result);
            collect_must_mut_from_stmt(body, params, callee_const, result);
        }
        _ => {}
    }
}

pub fn collect_must_mut_from_expr(
    expr: &Expr,
    params: &HashSet<InternedStr>,
    callee_const: &HashMap<InternedStr, HashSet<usize>>,
    result: &mut HashSet<InternedStr>,
) {
    match &expr.kind {
        // *param = expr, param->field = expr → param must be *mut
        ExprKind::Assign { lhs, rhs, .. } => {
            mark_lvalue_mut(lhs, params, result);
            collect_must_mut_from_expr(lhs, params, callee_const, result);
            collect_must_mut_from_expr(rhs, params, callee_const, result);
        }
        // ++(*param), (*param)++ 等
        ExprKind::PreInc(inner) | ExprKind::PreDec(inner) |
        ExprKind::PostInc(inner) | ExprKind::PostDec(inner) => {
            mark_lvalue_mut(inner, params, result);
            collect_must_mut_from_expr(inner, params, callee_const, result);
        }
        // func(param) — 呼び出し先の引数 mutability をチェック
        ExprKind::Call { func, args } => {
            // 呼び出し先マクロの const 情報をチェック
            if let ExprKind::Ident(func_name) = &func.kind {
                let const_arg_positions = callee_const.get(func_name);
                for (i, arg) in args.iter().enumerate() {
                    if let ExprKind::Ident(arg_name) = &arg.kind {
                        if params.contains(arg_name) {
                            // 呼び出し先の i 番目が const なら mut 不要
                            let is_const_at_callee = const_arg_positions
                                .map_or(false, |positions| positions.contains(&i));
                            if !is_const_at_callee {
                                // 呼び出し先が const でない（or 情報なし）→ mut 必要
                                result.insert(*arg_name);
                            }
                        }
                    }
                    collect_must_mut_from_expr(arg, params, callee_const, result);
                }
            } else {
                for arg in args {
                    collect_must_mut_from_expr(arg, params, callee_const, result);
                }
            }
            collect_must_mut_from_expr(func, params, callee_const, result);
        }
        // 再帰
        ExprKind::Binary { lhs, rhs, .. } | ExprKind::Comma { lhs, rhs } => {
            collect_must_mut_from_expr(lhs, params, callee_const, result);
            collect_must_mut_from_expr(rhs, params, callee_const, result);
        }
        ExprKind::Conditional { cond, then_expr, else_expr } => {
            collect_must_mut_from_expr(cond, params, callee_const, result);
            collect_must_mut_from_expr(then_expr, params, callee_const, result);
            collect_must_mut_from_expr(else_expr, params, callee_const, result);
        }
        // MacroCall(name, args) — 呼び出し先マクロの引数 mutability をチェック
        ExprKind::MacroCall { name, args, expanded, .. } => {
            let const_arg_positions = callee_const.get(name);
            for (i, arg) in args.iter().enumerate() {
                if let ExprKind::Ident(arg_name) = &arg.kind {
                    if params.contains(arg_name) {
                        let is_const_at_callee = const_arg_positions
                            .map_or(false, |positions| positions.contains(&i));
                        if !is_const_at_callee {
                            result.insert(*arg_name);
                        }
                    }
                }
                collect_must_mut_from_expr(arg, params, callee_const, result);
            }
            collect_must_mut_from_expr(expanded, params, callee_const, result);
        }
        ExprKind::Deref(inner) | ExprKind::UnaryMinus(inner) | ExprKind::BitNot(inner) |
        ExprKind::LogNot(inner) | ExprKind::AddrOf(inner) |
        ExprKind::Cast { expr: inner, .. } => {
            collect_must_mut_from_expr(inner, params, callee_const, result);
        }
        ExprKind::Member { expr: inner, .. } | ExprKind::PtrMember { expr: inner, .. } => {
            collect_must_mut_from_expr(inner, params, callee_const, result);
        }
        ExprKind::Sizeof(inner) => {
            collect_must_mut_from_expr(inner, params, callee_const, result);
        }
        ExprKind::Assert { condition, .. } => {
            collect_must_mut_from_expr(condition, params, callee_const, result);
        }
        ExprKind::StmtExpr(compound) => {
            for item in &compound.items {
                match item {
                    BlockItem::Stmt(s) => collect_must_mut_from_stmt(s, params, callee_const, result),
                    BlockItem::Decl(decl) => {
                        for init_decl in &decl.declarators {
                            if let Some(Initializer::Expr(init)) = &init_decl.init {
                                collect_must_mut_from_expr(init, params, callee_const, result);
                            }
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

/// 代入先の式に含まれるパラメータを must-mut としてマークする
pub fn mark_lvalue_mut(expr: &Expr, params: &HashSet<InternedStr>, result: &mut HashSet<InternedStr>) {
    match &expr.kind {
        // *param = ... → param must be *mut
        ExprKind::Deref(inner) => {
            if let ExprKind::Ident(name) = &inner.kind {
                if params.contains(name) {
                    result.insert(*name);
                }
            }
            // (*param).field の場合も再帰的にチェック
            mark_lvalue_mut(inner, params, result);
        }
        // param->field = ... → param must be *mut
        ExprKind::PtrMember { expr: inner, .. } => {
            if let ExprKind::Ident(name) = &inner.kind {
                if params.contains(name) {
                    result.insert(*name);
                }
            }
            mark_lvalue_mut(inner, params, result);
        }
        // (*param).field = ... → param must be *mut
        ExprKind::Member { expr: inner, .. } => {
            mark_lvalue_mut(inner, params, result);
        }
        // (SomeType*)param → キャスト先が *mut ならパラメータも mut
        ExprKind::Cast { expr: inner, type_name } => {
            if let ExprKind::Ident(name) = &inner.kind {
                if params.contains(name) {
                    // キャスト先がポインタで non-const なら mut 必要
                    let has_non_const_ptr = type_name.declarator.as_ref()
                        .map(|d| d.derived.iter().any(|dd| {
                            matches!(dd, crate::ast::DerivedDecl::Pointer(q) if !q.is_const)
                        }))
                        .unwrap_or(false);
                    if has_non_const_ptr {
                        result.insert(*name);
                    }
                }
            }
            mark_lvalue_mut(inner, params, result);
        }
        // MacroCall(name, expanded) → expanded 形式で lvalue を再帰チェック
        ExprKind::MacroCall { expanded, args, .. } => {
            mark_lvalue_mut(expanded, params, result);
            // 引数にパラメータが直接渡されている場合もチェック
            for arg in args {
                mark_lvalue_mut(arg, params, result);
            }
        }
        // Call の lvalue 使用: func(param) が lvalue として使われる場合
        // マクロ関数の呼び出し結果が lvalue なら、引数パラメータは *mut 必要
        ExprKind::Call { args, .. } => {
            for arg in args {
                if let ExprKind::Ident(name) = &arg.kind {
                    if params.contains(name) {
                        result.insert(*name);
                    }
                }
                mark_lvalue_mut(arg, params, result);
            }
        }
        _ => {}
    }
}

fn collect_mut_params(parse_result: &ParseResult, params: &[MacroParam]) -> HashSet<InternedStr> {
    let param_names: HashSet<InternedStr> = params.iter().map(|p| p.name).collect();
    let mut result = HashSet::new();
    match parse_result {
        ParseResult::Expression(expr) => collect_mut_params_from_expr(expr, &param_names, &mut result),
        ParseResult::Statement(items) => {
            for item in items {
                if let BlockItem::Stmt(stmt) = item {
                    collect_mut_params_from_stmt(stmt, &param_names, &mut result);
                }
            }
        }
        ParseResult::Unparseable(_) => {}
    }
    result
}

fn collect_mut_params_from_expr(expr: &Expr, params: &HashSet<InternedStr>, result: &mut HashSet<InternedStr>) {
    match &expr.kind {
        ExprKind::AddrOf(inner) => {
            // &mut param → param needs mut
            if let ExprKind::Ident(name) = &inner.kind {
                if params.contains(name) {
                    result.insert(*name);
                }
            }
            collect_mut_params_from_expr(inner, params, result);
        }
        ExprKind::Assign { lhs, rhs, .. } => {
            // param = ... or param += ... → param needs mut
            if let ExprKind::Ident(name) = &lhs.kind {
                if params.contains(name) {
                    result.insert(*name);
                }
            }
            collect_mut_params_from_expr(lhs, params, result);
            collect_mut_params_from_expr(rhs, params, result);
        }
        ExprKind::PreInc(inner) | ExprKind::PreDec(inner) |
        ExprKind::PostInc(inner) | ExprKind::PostDec(inner) => {
            if let ExprKind::Ident(name) = &inner.kind {
                if params.contains(name) {
                    result.insert(*name);
                }
            }
            collect_mut_params_from_expr(inner, params, result);
        }
        // Recurse into subexpressions
        ExprKind::Binary { lhs, rhs, .. } => {
            collect_mut_params_from_expr(lhs, params, result);
            collect_mut_params_from_expr(rhs, params, result);
        }
        ExprKind::Deref(inner) | ExprKind::UnaryMinus(inner) | ExprKind::BitNot(inner) |
        ExprKind::LogNot(inner) | ExprKind::Cast { expr: inner, .. } => {
            collect_mut_params_from_expr(inner, params, result);
        }
        ExprKind::Call { func, args } => {
            collect_mut_params_from_expr(func, params, result);
            for arg in args {
                collect_mut_params_from_expr(arg, params, result);
            }
        }
        ExprKind::MacroCall { expanded, args, .. } => {
            collect_mut_params_from_expr(expanded, params, result);
            for arg in args {
                collect_mut_params_from_expr(arg, params, result);
            }
        }
        ExprKind::Conditional { cond, then_expr, else_expr } => {
            collect_mut_params_from_expr(cond, params, result);
            collect_mut_params_from_expr(then_expr, params, result);
            collect_mut_params_from_expr(else_expr, params, result);
        }
        ExprKind::Comma { lhs, rhs } => {
            collect_mut_params_from_expr(lhs, params, result);
            collect_mut_params_from_expr(rhs, params, result);
        }
        ExprKind::Member { expr: inner, .. } | ExprKind::PtrMember { expr: inner, .. } => {
            collect_mut_params_from_expr(inner, params, result);
        }
        ExprKind::StmtExpr(compound) => {
            for item in &compound.items {
                if let BlockItem::Stmt(stmt) = item {
                    collect_mut_params_from_stmt(stmt, params, result);
                }
            }
        }
        _ => {}
    }
}

fn collect_mut_params_from_stmt(stmt: &Stmt, params: &HashSet<InternedStr>, result: &mut HashSet<InternedStr>) {
    match stmt {
        Stmt::Expr(Some(expr), _) => collect_mut_params_from_expr(expr, params, result),
        Stmt::Return(Some(expr), _) => collect_mut_params_from_expr(expr, params, result),
        Stmt::If { cond, then_stmt, else_stmt, .. } => {
            collect_mut_params_from_expr(cond, params, result);
            collect_mut_params_from_stmt(then_stmt, params, result);
            if let Some(else_s) = else_stmt {
                collect_mut_params_from_stmt(else_s, params, result);
            }
        }
        Stmt::Compound(compound) => {
            for item in &compound.items {
                if let BlockItem::Stmt(s) = item {
                    collect_mut_params_from_stmt(s, params, result);
                }
            }
        }
        Stmt::While { cond, body, .. } | Stmt::DoWhile { body, cond, .. } => {
            collect_mut_params_from_expr(cond, params, result);
            collect_mut_params_from_stmt(body, params, result);
        }
        Stmt::For { init, cond, step, body, .. } => {
            if let Some(ForInit::Expr(e)) = init {
                collect_mut_params_from_expr(e, params, result);
            }
            if let Some(c) = cond {
                collect_mut_params_from_expr(c, params, result);
            }
            if let Some(s) = step {
                collect_mut_params_from_expr(s, params, result);
            }
            collect_mut_params_from_stmt(body, params, result);
        }
        _ => {}
    }
}

/// 構造体フィールド名 → 型の逆引きマップを構築
/// 全構造体で同名フィールドの型が一致する場合のみ含む
fn build_field_type_map(dict: Option<&RustDeclDict>) -> HashMap<String, UnifiedType> {
    let mut map: HashMap<String, UnifiedType> = HashMap::new();
    let mut conflicts: HashSet<String> = HashSet::new();
    if let Some(dict) = dict {
        for st in dict.structs.values() {
            for field in &st.fields {
                if conflicts.contains(&field.name) {
                    continue;
                }
                match map.entry(field.name.clone()) {
                    std::collections::hash_map::Entry::Vacant(e) => {
                        e.insert(field.uty.clone());
                    }
                    std::collections::hash_map::Entry::Occupied(e) => {
                        if e.get() != &field.uty {
                            conflicts.insert(field.name.clone());
                            e.remove();
                        }
                    }
                }
            }
        }
    }
    map
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
    /// AST ダンプ対象関数名（デバッグ用）
    pub dump_ast_for: Option<String>,
}

impl Default for CodegenConfig {
    fn default() -> Self {
        Self {
            emit_inline_fns: true,
            emit_macros: true,
            include_source_location: true,
            use_statements: Vec::new(),
            dump_ast_for: None,
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
    /// ジェネリクス型パラメータを含む（Rust の as T キャスト不可）
    GenericUnsupported,
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
    /// ジェネリクス未対応マクロ数
    pub macros_generic_unsupported: usize,
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
    /// codegen で検出されたエラー（コメントアウトの理由）
    pub codegen_errors: Vec<String>,
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
    /// 現在生成中の関数の戻り値型
    current_return_type: Option<UnifiedType>,
    /// Call式のlvalue展開時に使用するパラメータ置換テーブル
    /// マクロ仮引数名 → 実引数のRust文字列
    param_substitutions: HashMap<InternedStr, String>,
    /// 現在生成中の関数のパラメータ型情報
    /// パラメータ名 → 型
    current_param_types: HashMap<InternedStr, UnifiedType>,
    /// 既知シンボル集合への参照（未解決シンボル検出用）
    known_symbols: &'a KnownSymbols,
    /// 現在の関数のローカルスコープ（パラメータ名 + ローカル変数名）
    current_local_names: HashSet<InternedStr>,
    /// 検出された未解決シンボル名（重複なし、出現順）
    unresolved_names: Vec<String>,
    /// 使用された libc 関数名
    used_libc_fns: HashSet<String>,
    /// Rust 宣言辞書への参照（関数パラメータ型参照用）
    rust_decl_dict: Option<&'a RustDeclDict>,
    /// inline 関数辞書への参照（戻り値型/引数型判定用）
    inline_fn_dict: Option<&'a crate::inline_fn::InlineFnDict>,
    /// 構造体フィールド名 → 型の逆引きマップ
    field_type_map: HashMap<String, UnifiedType>,
    /// AST ダンプ対象関数名（デバッグ用）
    dump_ast_for: Option<String>,
    /// const ポインタに変換可能なパラメータの引数位置集合
    const_pointer_positions: HashSet<usize>,
    /// 再代入されるローカル変数名の集合（let mut 判定用）
    mut_local_names: HashSet<InternedStr>,
    /// codegen で検出されたエラー
    codegen_errors: Vec<String>,
    /// このマクロが bool を返すと判定されたか
    is_bool_return: bool,
    /// codegen で bool を返すと判定されたマクロの集合（呼び出し先の bool 判定用）
    bool_return_macros: HashSet<InternedStr>,
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
    /// const ポインタに変換可能なマクロパラメータ: マクロ名 → const パラメータの引数位置集合
    const_pointer_params: HashMap<InternedStr, HashSet<usize>>,
    /// bool を返すと判定されたマクロの集合
    bool_return_macros: HashSet<InternedStr>,
}

impl<'a> RustCodegen<'a> {
    /// 新しい単一関数用コード生成器を作成
    pub fn new(
        interner: &'a StringInterner,
        enum_dict: &'a EnumDict,
        macro_ctx: &'a MacroInferContext,
        bindings_info: BindingsInfo,
        known_symbols: &'a KnownSymbols,
        rust_decl_dict: Option<&'a RustDeclDict>,
        inline_fn_dict: Option<&'a crate::inline_fn::InlineFnDict>,
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
            rust_decl_dict,
            inline_fn_dict,
            field_type_map: build_field_type_map(rust_decl_dict),
            dump_ast_for: None,
            const_pointer_positions: HashSet::new(),
            is_bool_return: false,
            bool_return_macros: HashSet::new(),
            mut_local_names: HashSet::new(),
            codegen_errors: Vec::new(),
        }
    }

    /// AST ダンプ対象関数名を設定（デバッグ用）
    pub fn with_dump_ast_for(mut self, name: Option<String>) -> Self {
        self.dump_ast_for = name;
        self
    }

    /// const ポインタ位置を設定
    pub fn with_const_pointer_positions(mut self, positions: HashSet<usize>) -> Self {
        self.const_pointer_positions = positions;
        self
    }

    /// bool 戻り値フラグと bool マクロ集合を設定
    pub fn with_bool_return(mut self, is_bool: bool, bool_macros: HashSet<InternedStr>) -> Self {
        self.is_bool_return = is_bool;
        self.bool_return_macros = bool_macros;
        self
    }

    /// 指定された関数名が AST ダンプ対象かどうかを判定し、対象なら AST をコメントとして出力
    fn dump_ast_comment_for_expr(&mut self, name_str: &str, parse_result: &ParseResult) {
        if self.dump_ast_for.as_deref() != Some(name_str) {
            return;
        }
        let sexp = match parse_result {
            ParseResult::Expression(expr) => {
                let mut buf = Vec::new();
                let mut printer = SexpPrinter::new(&mut buf, self.interner);
                let _ = printer.print_expr(expr);
                String::from_utf8_lossy(&buf).into_owned()
            }
            ParseResult::Statement(block_items) => {
                let mut buf = Vec::new();
                let mut printer = SexpPrinter::new(&mut buf, self.interner);
                for item in block_items {
                    if let BlockItem::Stmt(stmt) = item {
                        let _ = printer.print_stmt(stmt);
                    } else if let BlockItem::Decl(decl) = item {
                        let _ = printer.print_declaration(decl);
                    }
                }
                String::from_utf8_lossy(&buf).into_owned()
            }
            ParseResult::Unparseable(msg) => {
                format!("(unparseable: {})", msg.as_deref().unwrap_or("unknown"))
            }
        };
        self.writeln(&format!("// [AST dump for {}]", name_str));
        for line in sexp.lines() {
            self.writeln(&format!("// {}", line));
        }
    }

    /// 指定された関数名が AST ダンプ対象かどうかを判定し、対象なら CompoundStmt をコメントとして出力
    fn dump_ast_comment_for_body(&mut self, name_str: &str, body: &CompoundStmt) {
        if self.dump_ast_for.as_deref() != Some(name_str) {
            return;
        }
        let mut buf = Vec::new();
        let mut printer = SexpPrinter::new(&mut buf, self.interner);
        for item in &body.items {
            match item {
                BlockItem::Stmt(stmt) => { let _ = printer.print_stmt(stmt); }
                BlockItem::Decl(decl) => { let _ = printer.print_declaration(decl); }
            }
        }
        let sexp = String::from_utf8_lossy(&buf).into_owned();
        self.writeln(&format!("// [AST dump for {}]", name_str));
        for line in sexp.lines() {
            self.writeln(&format!("// {}", line));
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
                let ut = UnifiedType::from_rust_str(&t);
                if ut.is_pointer() { TypeHint::Pointer }
                else if ut.is_bool() { TypeHint::Bool }
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
                // current_param_types（ローカル変数含む）
                if let Some(ut) = self.current_param_types.get(name) {
                    if ut.is_pointer() {
                        return TypeHint::Pointer;
                    }
                    if ut.is_bool() {
                        return TypeHint::Bool;
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
                    // bindings.rs の関数戻り値型を参照
                    if let Some(ret_ut) = self.get_callee_return_type(self.interner.get(*name)) {
                        if ret_ut.is_pointer() {
                            return TypeHint::Pointer;
                        }
                    }
                }
                TypeHint::Unknown
            }
            ExprKind::MacroCall { name, expanded, .. } => {
                if let Some(callee) = self.macro_ctx.macros.get(name) {
                    for c in &callee.type_env.return_constraints {
                        if is_type_repr_pointer(&c.ty) {
                            return TypeHint::Pointer;
                        }
                    }
                }
                // return_constraints が無ければ expanded にフォールバック
                self.infer_type_hint(expanded, info)
            }
            ExprKind::AddrOf(_) => TypeHint::Pointer,
            ExprKind::Deref(inner) => {
                // ポインタ to ポインタの deref → ポインタ
                if let Some(ut) = self.infer_expr_type(inner, info) {
                    if let Some(derefed) = ut.inner_type() {
                        if derefed.is_pointer() {
                            return TypeHint::Pointer;
                        }
                    }
                }
                TypeHint::Unknown
            }
            ExprKind::PtrMember { member, .. } | ExprKind::Member { member, .. } => {
                let member_str = self.interner.get(*member);
                if let Some(ut) = self.field_type_map.get(member_str) {
                    if ut.is_pointer() {
                        TypeHint::Pointer
                    } else if ut.is_bool() {
                        TypeHint::Bool
                    } else {
                        TypeHint::Integer
                    }
                } else {
                    TypeHint::Unknown
                }
            }
            ExprKind::Binary { op, lhs, rhs } => {
                match op {
                    BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge |
                    BinOp::LogAnd | BinOp::LogOr => TypeHint::Bool,
                    BinOp::Add => {
                        // ポインタ + 整数 → ポインタ、整数 + ポインタ → ポインタ
                        let lh = self.infer_type_hint(lhs, info);
                        if lh == TypeHint::Pointer { return TypeHint::Pointer; }
                        let rh = self.infer_type_hint(rhs, info);
                        if rh == TypeHint::Pointer { TypeHint::Pointer } else { TypeHint::Unknown }
                    }
                    BinOp::Sub => {
                        // ポインタ - 整数 → ポインタ（ポインタ - ポインタ → 整数）
                        let lh = self.infer_type_hint(lhs, info);
                        if lh == TypeHint::Pointer {
                            let rh = self.infer_type_hint(rhs, info);
                            if rh == TypeHint::Pointer { TypeHint::Integer } else { TypeHint::Pointer }
                        } else {
                            TypeHint::Unknown
                        }
                    }
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
        if self.is_bool_expr_with_dict(expr) {
            return expr_str.to_string();
        }
        // パラメータが bool 型なら != 0 不要
        if let ExprKind::Ident(name) = &expr.kind {
            if let Some(ut) = self.current_param_types.get(name) {
                if ut.is_bool() {
                    return expr_str.to_string();
                }
            }
        }
        // フィールドが bool 型なら != 0 不要
        if let ExprKind::Member { member, .. } | ExprKind::PtrMember { member, .. } = &expr.kind {
            let member_str = self.interner.get(*member);
            if let Some(ut) = self.field_type_map.get(member_str) {
                if ut.is_bool() {
                    return expr_str.to_string();
                }
            }
        }
        // __builtin_expect(cond, val) → cond の型をチェック
        if let ExprKind::Call { func, args, .. } = &expr.kind {
            if let ExprKind::Ident(name) = &func.kind {
                if self.interner.get(*name) == "__builtin_expect" && !args.is_empty() {
                    return self.wrap_as_bool_condition_macro(&args[0], expr_str, info);
                }
            }
        }
        if expr_str.ends_with(" as bool)") || expr_str.ends_with("!= 0)") || expr_str.ends_with(".is_null()") {
            return expr_str.to_string();
        }
        if self.infer_type_hint(expr, info) == TypeHint::Pointer
            || self.infer_expr_type(expr, info).is_some_and(|ut| ut.is_pointer()) {
            return format!("!{}.is_null()", expr_str);
        }
        format!("({} != 0)", strip_outer_parens(expr_str))
    }

    /// ポインタ式をbool条件に変換するラッパー（inline関数用）
    fn wrap_as_bool_condition_inline(&self, expr: &Expr, expr_str: &str) -> String {
        if self.is_bool_expr_with_dict(expr) {
            return expr_str.to_string();
        }
        // __builtin_expect(cond, val) → cond の型をチェック
        if let ExprKind::Call { func, args, .. } = &expr.kind {
            if let ExprKind::Ident(name) = &func.kind {
                if self.interner.get(*name) == "__builtin_expect" && !args.is_empty() {
                    return self.wrap_as_bool_condition_inline(&args[0], expr_str);
                }
            }
        }
        // bool 型変数の検出
        if let ExprKind::Ident(name) = &expr.kind {
            if let Some(ut) = self.current_param_types.get(name) {
                if ut.is_bool() {
                    return expr_str.to_string();
                }
            }
        }
        // フィールドが bool 型なら != 0 不要
        if let ExprKind::Member { member, .. } | ExprKind::PtrMember { member, .. } = &expr.kind {
            let member_str = self.interner.get(*member);
            if let Some(ut) = self.field_type_map.get(member_str) {
                if ut.is_bool() {
                    return expr_str.to_string();
                }
            }
        }
        if expr_str.ends_with(" as bool)") || expr_str.ends_with("!= 0)") || expr_str.ends_with(".is_null()") {
            return expr_str.to_string();
        }
        if self.is_pointer_expr_inline(expr)
            || self.infer_expr_type_inline(expr).is_some_and(|ut| ut.is_pointer()) {
            return format!("!{}.is_null()", expr_str);
        }
        format!("({} != 0)", strip_outer_parens(expr_str))
    }

    /// 式がポインタ型かどうかを current_param_types から推定（inline関数用）
    fn is_pointer_expr_inline(&self, expr: &Expr) -> bool {
        match &expr.kind {
            ExprKind::Ident(name) => {
                if let Some(ut) = self.current_param_types.get(name) {
                    return ut.is_pointer();
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
            ExprKind::Member { member, .. } | ExprKind::PtrMember { member, .. } => {
                let member_str = self.interner.get(*member);
                self.field_type_map.get(member_str).is_some_and(|ut| ut.is_pointer())
            }
            ExprKind::Deref(inner) => {
                // ポインタ to ポインタの deref
                if let Some(ut) = self.infer_expr_type_inline(inner) {
                    if let Some(derefed) = ut.inner_type() {
                        return derefed.is_pointer();
                    }
                }
                false
            }
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
                    // rust_decl_dict の関数戻り値型を参照
                    if let Some(ret_ut) = self.get_callee_return_type(self.interner.get(*name)) {
                        return ret_ut.is_pointer();
                    }
                }
                false
            }
            ExprKind::MacroCall { name, expanded, .. } => {
                // マクロの戻り値型を参照
                if let Some(callee) = self.macro_ctx.macros.get(name) {
                    for c in &callee.type_env.return_constraints {
                        if is_type_repr_pointer(&c.ty) {
                            return true;
                        }
                    }
                }
                // expanded にフォールバック
                self.is_pointer_expr_inline(expanded)
            }
            ExprKind::Binary { op, lhs, rhs } => {
                match op {
                    BinOp::Add => {
                        // ポインタ + 整数 → ポインタ
                        self.is_pointer_expr_inline(lhs) || self.is_pointer_expr_inline(rhs)
                    }
                    BinOp::Sub => {
                        // ポインタ - 整数 → ポインタ（ポインタ - ポインタ → 整数）
                        self.is_pointer_expr_inline(lhs) && !self.is_pointer_expr_inline(rhs)
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }

    /// 式の型を推定（inline 関数用）
    /// 型が判明しない場合は None を返す
    fn infer_expr_type_inline(&self, expr: &Expr) -> Option<UnifiedType> {
        match &expr.kind {
            ExprKind::Ident(name) => {
                // パラメータ/ローカル変数の型
                if let Some(ut) = self.current_param_types.get(name) {
                    return Some(ut.clone());
                }
                // 定数の型
                if let Some(dict) = self.rust_decl_dict {
                    let name_str = self.interner.get(*name);
                    if let Some(c) = dict.consts.get(name_str) {
                        return Some(c.uty.clone());
                    }
                }
                None
            }
            ExprKind::Cast { type_name, .. } => {
                Some(UnifiedType::from_rust_str(&self.type_name_to_type_str_readonly(type_name)))
            }
            ExprKind::Member { member, .. } | ExprKind::PtrMember { member, .. } => {
                let member_str = self.interner.get(*member);
                self.field_type_map.get(member_str).cloned()
            }
            ExprKind::Deref(inner) => {
                let inner_ut = self.infer_expr_type_inline(inner)?;
                inner_ut.inner_type().cloned()
            }
            ExprKind::Binary { op, lhs, rhs } => {
                match op {
                    BinOp::Shl | BinOp::Shr => self.infer_expr_type_inline(lhs),
                    BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => {
                        let lt = self.infer_expr_type_inline(lhs);
                        let rt = self.infer_expr_type_inline(rhs);
                        match (&lt, &rt) {
                            (Some(l), Some(r)) => {
                                let ls = l.to_rust_string();
                                let rs = r.to_rust_string();
                                wider_integer_type(&ls, &rs)
                                    .map(|w| UnifiedType::from_rust_str(w))
                                    .or(lt)
                            }
                            (Some(_), None) => lt,
                            (None, Some(_)) => rt,
                            _ => None,
                        }
                    }
                    BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt
                    | BinOp::Le | BinOp::Ge | BinOp::LogAnd | BinOp::LogOr => {
                        Some(UnifiedType::Bool)
                    }
                    // 算術演算: LHS の型を返す（Add, Sub, Mul, Div, Mod）
                    _ => {
                        let lt = self.infer_expr_type_inline(lhs);
                        if lt.is_some() { return lt; }
                        self.infer_expr_type_inline(rhs)
                    }
                }
            }
            ExprKind::BitNot(inner) | ExprKind::UnaryMinus(inner) => self.infer_expr_type_inline(inner),
            ExprKind::CharLit(_) => Some(UnifiedType::from_rust_str("i8")),
            ExprKind::UIntLit(_) => Some(UnifiedType::Int { signed: false, size: crate::unified_type::IntSize::LongLong }),
            ExprKind::Sizeof(_) | ExprKind::SizeofType(_) => Some(UnifiedType::Int { signed: false, size: crate::unified_type::IntSize::Long }),
            ExprKind::Call { func, .. } => {
                // メソッド呼び出し: receiver.method(arg)
                // offset/wrapping_add/wrapping_sub はレシーバと同じ型を返す
                if let ExprKind::Member { expr: receiver, member, .. } = &func.kind {
                    let method_name = self.interner.get(*member);
                    if matches!(method_name, "offset" | "wrapping_add" | "wrapping_sub" | "wrapping_offset") {
                        return self.infer_expr_type_inline(receiver);
                    }
                }
                if let ExprKind::Ident(name) = &func.kind {
                    let func_name = self.interner.get(*name);
                    if let Some(ret_ut) = self.get_callee_return_type(func_name) {
                        return Some(ret_ut.clone());
                    }
                }
                None
            }
            ExprKind::MacroCall { name, expanded, .. } => {
                // マクロの戻り値型を参照
                if let Some(macro_info) = self.macro_ctx.macros.get(name) {
                    if let Some(ty) = macro_info.get_return_type() {
                        return Some(UnifiedType::from_rust_str(&ty.to_rust_string(self.interner)));
                    }
                }
                // expanded にフォールバック
                self.infer_expr_type_inline(expanded)
            }
            ExprKind::Conditional { then_expr, .. } => {
                self.infer_expr_type_inline(then_expr)
            }
            _ => None,
        }
    }

    /// TypeName から型文字列を取得（読み取り専用、整数型を正しく解決する版）
    fn type_name_to_type_str_readonly(&self, type_name: &crate::ast::TypeName) -> String {
        // ポインタの個数をカウント
        let pointer_count = type_name.declarator.as_ref()
            .map(|d| d.derived.iter().filter(|dd| matches!(dd, crate::ast::DerivedDecl::Pointer(_))).count())
            .unwrap_or(0);
        // const チェック:
        // C の "const T *p" → pointee は const → Rust: *const T
        //   この場合 const は specs.qualifiers.is_const にある
        // C の "T * const p" → ポインタ自体が const (再代入不可) → Rust: *mut T
        //   こ���場合 const は Pointer(qualifiers.is_const) にある → Rust の *const ではない
        let is_const_ptr = pointer_count == 1 && type_name.specs.qualifiers.is_const;
        // 基本型を取得
        let base = self.base_type_str_readonly(&type_name.specs.type_specs);
        // ポインタをラップ
        let mut result = base;
        for _ in 0..pointer_count {
            let prefix = if is_const_ptr { "*const " } else { "*mut " };
            result = format!("{}{}", prefix, result);
        }
        result
    }

    /// TypeSpec リストから基本型名を取得する（読み取り専用）
    fn base_type_str_readonly(&self, type_specs: &[TypeSpec]) -> String {
        // typedef 名を優先
        for spec in type_specs {
            if let TypeSpec::TypedefName(name) = spec {
                return self.interner.get(*name).to_string();
            }
        }
        // struct/union/enum 名
        for spec in type_specs {
            match spec {
                TypeSpec::Struct(s) | TypeSpec::Union(s) => {
                    if let Some(n) = &s.name {
                        return self.interner.get(*n).to_string();
                    }
                }
                TypeSpec::Enum(e) => {
                    if let Some(n) = &e.name {
                        return self.interner.get(*n).to_string();
                    }
                }
                _ => {}
            }
        }
        let mut is_void = false;
        let mut is_char = false;
        let mut is_int = false;
        let mut is_short = false;
        let mut is_long = 0usize;
        let mut is_unsigned = false;
        for spec in type_specs {
            match spec {
                TypeSpec::Void => is_void = true,
                TypeSpec::Char => is_char = true,
                TypeSpec::Int => is_int = true,
                TypeSpec::Short => is_short = true,
                TypeSpec::Long => is_long += 1,
                TypeSpec::Unsigned => is_unsigned = true,
                TypeSpec::Signed => {}
                TypeSpec::Bool => return "bool".to_string(),
                _ => {}
            }
        }
        if is_void { return "c_void".to_string(); }
        if is_char { return if is_unsigned { "c_uchar".to_string() } else { "c_char".to_string() }; }
        if is_short { return if is_unsigned { "c_ushort".to_string() } else { "c_short".to_string() }; }
        if is_long >= 2 { return if is_unsigned { "c_ulonglong".to_string() } else { "c_longlong".to_string() }; }
        if is_long == 1 { return if is_unsigned { "c_ulong".to_string() } else { "c_long".to_string() }; }
        if is_int || is_unsigned { return if is_unsigned { "c_uint".to_string() } else { "c_int".to_string() }; }
        "c_int".to_string()
    }

    /// type_name_to_rust の読み取り専用版（&self で呼び出し可能）
    fn type_name_to_rust_readonly(&self, type_name: &crate::ast::TypeName) -> String {
        self.type_name_to_type_str_readonly(type_name)
    }

    /// 式の型を推定（macro 関数用）
    fn infer_expr_type(&self, expr: &Expr, info: &MacroInferInfo) -> Option<UnifiedType> {
        match &expr.kind {
            ExprKind::Ident(name) => {
                // パラメータの型制約から取得
                if let Some(constraints) = info.type_env.param_constraints.get(name) {
                    if let Some(c) = constraints.first() {
                        return Some(UnifiedType::from_rust_str(&c.ty.to_rust_string(self.interner)));
                    }
                }
                // param_to_exprs 経由の expr_constraints
                if let Some(expr_ids) = info.type_env.param_to_exprs.get(name) {
                    for expr_id in expr_ids {
                        if let Some(constraints) = info.type_env.expr_constraints.get(expr_id) {
                            if let Some(c) = constraints.first() {
                                return Some(UnifiedType::from_rust_str(&c.ty.to_rust_string(self.interner)));
                            }
                        }
                    }
                }
                // current_param_types（ローカル変数含む）
                if let Some(ut) = self.current_param_types.get(name) {
                    return Some(ut.clone());
                }
                // 定数の型
                if let Some(dict) = self.rust_decl_dict {
                    let name_str = self.interner.get(*name);
                    if let Some(c) = dict.consts.get(name_str) {
                        return Some(c.uty.clone());
                    }
                }
                None
            }
            ExprKind::Cast { type_name, .. } => {
                Some(UnifiedType::from_rust_str(&self.type_name_to_type_str_readonly(type_name)))
            }
            ExprKind::Member { member, .. } | ExprKind::PtrMember { member, .. } => {
                let member_str = self.interner.get(*member);
                self.field_type_map.get(member_str).cloned()
            }
            ExprKind::Deref(inner) => {
                let inner_ut = self.infer_expr_type(inner, info)?;
                inner_ut.inner_type().cloned()
            }
            ExprKind::Binary { op, lhs, rhs } => {
                match op {
                    BinOp::Shl | BinOp::Shr => self.infer_expr_type(lhs, info),
                    BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => {
                        let lt = self.infer_expr_type(lhs, info);
                        let rt = self.infer_expr_type(rhs, info);
                        match (&lt, &rt) {
                            (Some(l), Some(r)) => {
                                let ls = l.to_rust_string();
                                let rs = r.to_rust_string();
                                wider_integer_type(&ls, &rs)
                                    .map(|w| UnifiedType::from_rust_str(w))
                                    .or(lt)
                            }
                            (Some(_), None) => lt,
                            (None, Some(_)) => rt,
                            _ => None,
                        }
                    }
                    BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt
                    | BinOp::Le | BinOp::Ge | BinOp::LogAnd | BinOp::LogOr => {
                        Some(UnifiedType::Bool)
                    }
                    // 算術演算: LHS の型を返す（Add, Sub, Mul, Div, Mod）
                    _ => {
                        let lt = self.infer_expr_type(lhs, info);
                        if lt.is_some() { return lt; }
                        self.infer_expr_type(rhs, info)
                    }
                }
            }
            ExprKind::BitNot(inner) | ExprKind::UnaryMinus(inner) => self.infer_expr_type(inner, info),
            ExprKind::CharLit(_) => Some(UnifiedType::from_rust_str("i8")),
            ExprKind::UIntLit(_) => Some(UnifiedType::Int { signed: false, size: crate::unified_type::IntSize::LongLong }),
            ExprKind::Sizeof(_) | ExprKind::SizeofType(_) => Some(UnifiedType::Int { signed: false, size: crate::unified_type::IntSize::Long }),
            ExprKind::Call { func, .. } => {
                // メソッド呼び出し: receiver.method(arg)
                // offset/wrapping_add/wrapping_sub はレシーバと同じ型を返す
                if let ExprKind::Member { expr: receiver, member, .. } = &func.kind {
                    let method_name = self.interner.get(*member);
                    if matches!(method_name, "offset" | "wrapping_add" | "wrapping_sub" | "wrapping_offset") {
                        return self.infer_expr_type(receiver, info);
                    }
                }
                if let ExprKind::Ident(name) = &func.kind {
                    let func_name = self.interner.get(*name);
                    if let Some(ret_ut) = self.get_callee_return_type(func_name) {
                        return Some(ret_ut.clone());
                    }
                    // 自家生成マクロの戻り値型
                    if let Some(macro_info) = self.macro_ctx.macros.get(name) {
                        if let Some(ty) = macro_info.get_return_type() {
                            return Some(UnifiedType::from_rust_str(&ty.to_rust_string(self.interner)));
                        }
                    }
                }
                None
            }
            ExprKind::MacroCall { name, expanded, .. } => {
                // マクロの戻り値型を参照
                if let Some(macro_info) = self.macro_ctx.macros.get(name) {
                    if let Some(ty) = macro_info.get_return_type() {
                        return Some(UnifiedType::from_rust_str(&ty.to_rust_string(self.interner)));
                    }
                }
                // 展開済みの式から推定
                self.infer_expr_type(expanded, info)
            }
            ExprKind::Conditional { then_expr, .. } => {
                self.infer_expr_type(then_expr, info)
            }
            _ => None,
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

    /// 呼び出し先関数の指定引数位置の型を取得
    fn get_callee_param_type(&self, func_name: &str, arg_index: usize) -> Option<&UnifiedType> {
        self.rust_decl_dict?.fns.get(func_name).and_then(|f| {
            f.params.get(arg_index).map(|p| &p.uty)
        })
    }

    /// 呼び出し先関数のパラメータ型を取得（inline 関数/マクロ関数もフォールバック）
    fn get_callee_param_type_extended(&mut self, func_name: &str, arg_index: usize) -> Option<UnifiedType> {
        // 1. bindings.rs
        if let Some(ut) = self.get_callee_param_type(func_name, arg_index) {
            return Some(ut.clone());
        }
        if let Some(interned) = self.interner.lookup(func_name) {
            // 2. inline 関数
            if let Some(dict) = self.inline_fn_dict {
                if let Some(func_def) = dict.get(interned) {
                    for d in &func_def.declarator.derived {
                        if let DerivedDecl::Function(param_list) = d {
                            if let Some(param) = param_list.params.get(arg_index) {
                                let ty = self.param_type_only(param);
                                return Some(UnifiedType::from_rust_str(&ty));
                            }
                            break;
                        }
                    }
                }
            }
            // 3. 自家生成マクロ関数の type_env からパラメータ型を取得
            if let Some(macro_info) = self.macro_ctx.macros.get(&interned) {
                // THX 依存の場合、arg_index 0 は my_perl なのでスキップ
                let macro_param_idx = if macro_info.is_thx_dependent {
                    if arg_index == 0 {
                        return Some(UnifiedType::from_rust_str("*mut PerlInterpreter"));
                    }
                    arg_index - 1
                } else {
                    arg_index
                };
                if let Some(param) = macro_info.params.get(macro_param_idx) {
                    // 方法1: param_to_exprs 逆引き辞書
                    if let Some(expr_ids) = macro_info.type_env.param_to_exprs.get(&param.name) {
                        for expr_id in expr_ids {
                            if let Some(constraints) = macro_info.type_env.expr_constraints.get(expr_id) {
                                for c in constraints {
                                    if !c.ty.is_void() {
                                        let ty = c.ty.to_rust_string(self.interner);
                                        return Some(UnifiedType::from_rust_str(&ty));
                                    }
                                }
                            }
                        }
                    }
                    // 方法2: MacroParam の ExprId
                    let expr_id = param.expr_id();
                    if let Some(constraints) = macro_info.type_env.expr_constraints.get(&expr_id) {
                        if let Some(first) = constraints.first() {
                            let ty = first.ty.to_rust_string(self.interner);
                            return Some(UnifiedType::from_rust_str(&ty));
                        }
                    }
                }
            }
        }
        None
    }

    /// 呼び出し先関数の指定引数位置が bool 型かどうか判定（自家生成マクロも参照）
    fn callee_param_is_bool(&self, func_name: &str, arg_index: usize) -> bool {
        // 1. bindings.rs の関数
        if let Some(param_ut) = self.get_callee_param_type(func_name, arg_index) {
            return param_ut.is_bool();
        }
        // 2. 自家生成マクロ関数のパラメータ型
        if let Some(interned) = self.interner.lookup(func_name) {
            if let Some(macro_info) = self.macro_ctx.macros.get(&interned) {
                // THX マクロは my_perl が自動挿入されるのでオフセットを引く
                let macro_arg_index = if macro_info.is_thx_dependent && arg_index > 0 {
                    arg_index - 1
                } else {
                    arg_index
                };
                if let Some(param) = macro_info.params.get(macro_arg_index) {
                    // type_env からパラメータの型制約を取得
                    if let Some(expr_ids) = macro_info.type_env.param_to_exprs.get(&param.name) {
                        for expr_id in expr_ids {
                            if let Some(constraints) = macro_info.type_env.expr_constraints.get(expr_id) {
                                for c in constraints {
                                    let rust_ty = c.ty.to_rust_string(self.interner);
                                    if rust_ty == "bool" {
                                        return true;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            // 3. 自家生成 inline 関数のパラメータ型
            if let Some(dict) = self.inline_fn_dict {
                if let Some(func_def) = dict.get(interned) {
                    for d in &func_def.declarator.derived {
                        if let DerivedDecl::Function(param_list) = d {
                            if let Some(param) = param_list.params.get(arg_index) {
                                let has_bool = param.specs.type_specs.iter().any(|ts| matches!(ts, TypeSpec::Bool));
                                let has_pointer = param.declarator.as_ref().map_or(false, |decl| {
                                    decl.derived.iter().any(|d| matches!(d, DerivedDecl::Pointer(_)))
                                });
                                if has_bool && !has_pointer {
                                    return true;
                                }
                            }
                            break;
                        }
                    }
                }
            }
        }
        false
    }

    /// 呼び出し先関数の戻り値型を取得
    fn get_callee_return_type(&self, func_name: &str) -> Option<&UnifiedType> {
        self.rust_decl_dict?.fns.get(func_name).and_then(|f| {
            f.uret_ty.as_ref()
        })
    }

    /// 式が bool を返すかどうかを判定（関数の戻り値型も考慮）
    fn is_bool_expr_with_dict(&self, expr: &Expr) -> bool {
        if is_boolean_expr_recursive(expr, self.interner) {
            return true;
        }
        // 配列インデックス: 配列要素型が bool なら bool
        if let ExprKind::Index { expr: base, .. } = &expr.kind {
            if let ExprKind::Ident(name) = &base.kind {
                if let Some(dict) = self.rust_decl_dict {
                    let name_str = self.interner.get(*name);
                    if let Some(c) = dict.consts.get(name_str) {
                        if let Some(inner) = c.uty.inner_type() {
                            if inner.is_bool() {
                                return true;
                            }
                        }
                    }
                    // static 配列: 名前が PL_valid_types_* なら bool 配列
                    if dict.statics.contains(name_str) && name_str.starts_with("PL_valid_types_") {
                        return true;
                    }
                }
            }
        }
        // パラメータが bool 型
        if let ExprKind::Ident(name) = &expr.kind {
            if let Some(ut) = self.current_param_types.get(name) {
                if ut.is_bool() {
                    return true;
                }
            }
        }
        // フィールドが bool 型
        if let ExprKind::Member { member, .. } | ExprKind::PtrMember { member, .. } = &expr.kind {
            let member_str = self.interner.get(*member);
            if let Some(ut) = self.field_type_map.get(member_str) {
                if ut.is_bool() {
                    return true;
                }
            }
        }
        // 関数呼び出しの戻り値型が bool かチェック
        if let ExprKind::Call { func, .. } = &expr.kind {
            if let ExprKind::Ident(name) = &func.kind {
                // 0. codegen で bool と判定されたマクロ
                if self.bool_return_macros.contains(name) {
                    return true;
                }
                let func_name = self.interner.get(*name);
                // 1. bindings.rs の関数
                if let Some(ret_ut) = self.get_callee_return_type(func_name) {
                    return ret_ut.is_bool();
                }
                // 2. 自家生成マクロ関数の戻り値型
                if let Some(macro_info) = self.macro_ctx.macros.get(name) {
                    if let Some(ty) = macro_info.get_return_type() {
                        let rust_ty = ty.to_rust_string(self.interner);
                        return rust_ty == "bool";
                    }
                }
                // 3. 自家生成 inline 関数の戻り値型
                if let Some(dict) = self.inline_fn_dict {
                    if let Some(func_def) = dict.get(*name) {
                        let has_bool_return = func_def.specs.type_specs.iter().any(|ts| matches!(ts, TypeSpec::Bool));
                        let has_return_pointer = func_def.declarator.derived.iter().any(|d| matches!(d, DerivedDecl::Pointer(_)));
                        if has_bool_return && !has_return_pointer {
                            return true;
                        }
                    }
                }
            }
        }
        // MacroCall の場合も bool_return_macros をチェック
        if let ExprKind::MacroCall { name, .. } = &expr.kind {
            if self.bool_return_macros.contains(name) {
                return true;
            }
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
        // null pointer パラメータへの 0 リテラル変換
        if is_null_literal(expr) {
            if let Some(callee_name) = callee {
                let func_name = self.interner.get(callee_name).to_string();
                if let Some(expected_ut) = self.get_callee_param_type_extended(&func_name, arg_index) {
                    if expected_ut.is_pointer() {
                        return null_ptr_expr(&expected_ut);
                    }
                }
            }
        }
        // bool パラメータへの整数リテラル変換
        if let Some(callee_name) = callee {
            let func_name = self.interner.get(callee_name);
            if self.callee_param_is_bool(func_name, arg_index) {
                match &expr.kind {
                    ExprKind::IntLit(0) => return "false".to_string(),
                    ExprKind::IntLit(1) => return "true".to_string(),
                    _ => {}
                }
            }
        }
        let result = self.expr_to_rust_ctx(expr, info, ExprContext::Top);
        // 整数型の幅不一致キャスト挿入 (bindings.rs + inline 関数)
        if let Some(callee_name) = callee {
            let func_name = self.interner.get(callee_name).to_string();
            if let Some(expected_ut) = self.get_callee_param_type_extended(&func_name, arg_index) {
                let actual_ut = self.infer_expr_type(expr, info);
                let actual_ty_str = actual_ut.as_ref().map(|ut| ut.to_rust_string());
                let expected_ty_str = expected_ut.to_rust_string();
                let casted = self.cast_integer_arg_if_needed(&result, actual_ty_str.as_deref(), &expected_ty_str);
                return strip_outer_parens(&casted).to_string();
            }
        }
        strip_outer_parens(&result).to_string()
    }

    /// 整数型の幅が不一致の場合、またはポインタ型のサブタイプ変換が必要な場合に `as` キャストを挿入する
    fn cast_integer_arg_if_needed(&self, arg_str: &str, actual_ty: Option<&str>, expected_ty: &str) -> String {
        if let Some(actual) = actual_ty {
            let na = normalize_integer_type(actual);
            let ne = normalize_integer_type(expected_ty);
            if let (Some(a), Some(e)) = (na, ne) {
                if !integer_types_compatible(a, e) {
                    return format!("{} as {}", arg_str, e);
                }
            }
            // ポインタ型のサブタイプ変換 (e.g., *mut GV → *mut SV)
            if actual != expected_ty {
                let actual_ut = UnifiedType::from_rust_str(actual);
                let expected_ut = UnifiedType::from_rust_str(expected_ty);
                if actual_ut.is_pointer() && expected_ut.is_pointer() {
                    if is_sv_subtype_cast(&actual_ut, &expected_ut) {
                        // const/mut を保持: actual が const なら *const SV にキャスト
                        let cast_ty = if actual.contains("*const") {
                            expected_ty.replace("*mut", "*const")
                        } else {
                            expected_ty.to_string()
                        };
                        return format!("{} as {}", arg_str, cast_ty);
                    }
                    // const→mut 変換は安全でないため行わない
                    // Phase 2 の Tier ベース推論で解決すべき
                }
            }
        }
        // フォールバック: actual 不明でも expected が SV ポインタ型なら
        // SV subtype のキャストを試行（safe direction: *mut GV → *const SV 等）
        let expected_ut = UnifiedType::from_rust_str(expected_ty);
        if expected_ut.is_pointer() {
            if let Some(inner) = expected_ut.inner_type() {
                if let UnifiedType::Named(name) = inner {
                    let n = name.as_str();
                    if n == "SV" || n == "GV" || n == "HV" || n == "AV" || n == "CV" || n == "IO" {
                        // 引数の文字列が関数呼び出しなら as キャスト
                        if arg_str.contains('(') && !arg_str.starts_with('(') {
                            return format!("{} as {}", arg_str, expected_ty);
                        }
                    }
                }
            }
        }
        arg_str.to_string()
    }

    /// 返り値式の型キャストが必要ならキャスト済み文字列を返す
    fn cast_return_expr_if_needed(&self, expr: &Expr, info: &MacroInferInfo, rust_expr: &str) -> Option<String> {
        let ret_ut = self.current_return_type.as_ref()?;
        let expr_ut = self.infer_expr_type(expr, info)?;
        let ret_s = ret_ut.to_rust_string();
        let expr_s = expr_ut.to_rust_string();
        // 整数型キャスト
        if let (Some(nr), Some(ne)) = (normalize_integer_type(&ret_s), normalize_integer_type(&expr_s)) {
            if !integer_types_compatible(nr, ne) {
                return Some(format!("({} as {})", rust_expr, nr));
            }
        }
        // ポインタ const→mut キャストは安全でないため行わない
        None
    }

    /// 返り値式の型キャストが必要ならキャスト済み文字列を返す (inline 版)
    fn cast_return_expr_if_needed_inline(&self, expr: &Expr, rust_expr: &str) -> Option<String> {
        let ret_ut = self.current_return_type.as_ref()?;
        let expr_ut = self.infer_expr_type_inline(expr)?;
        let ret_s = ret_ut.to_rust_string();
        let expr_s = expr_ut.to_rust_string();
        // 整数型キャスト
        if let (Some(nr), Some(ne)) = (normalize_integer_type(&ret_s), normalize_integer_type(&expr_s)) {
            if !integer_types_compatible(nr, ne) {
                return Some(format!("({} as {})", rust_expr, nr));
            }
        }
        // ポインタ const→mut キャストは安全でないため行わない
        None
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
                self.current_param_types.insert(name, UnifiedType::from_rust_str(&ty));
            }
        }
    }

    /// Declaration からローカル変数の型のみ収集して current_param_types に追加
    /// (current_local_names には追加しない — 未解決シンボル検出に影響しないように)
    fn collect_decl_types(&mut self, decl: &Declaration) {
        let base_type = self.decl_specs_to_rust(&decl.specs);
        for init_decl in &decl.declarators {
            if let Some(name) = init_decl.declarator.name {
                let ty = self.apply_derived_to_type(&base_type, &init_decl.declarator.derived);
                self.current_param_types.insert(name, UnifiedType::from_rust_str(&ty));
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
            codegen_errors: self.codegen_errors,
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
        self.current_return_type = Some(UnifiedType::from_rust_str(&return_type));

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

        // AST ダンプ（デバッグ用）
        self.dump_ast_comment_for_expr(name_str, &info.parse_result);

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
                let type_hint = self.current_return_type.as_ref().map(|ut| ut.to_rust_string());
                let rust_expr = self.expr_with_type_hint(expr, info, type_hint.as_deref());
                if self.current_return_type.as_ref().is_some_and(|ut| ut.is_void()) {
                    // void 関数: 式の結果を捨てる
                    self.writeln(&format!("{}{};", body_indent, rust_expr));
                } else if self.current_return_type.as_ref().is_some_and(|ut| ut.is_bool())
                    && !self.is_bool_expr_with_dict(expr)
                    && !is_string_bool_expr(&rust_expr) {
                    // bool 関数: 式がポインタなら .is_null() で変換
                    if self.infer_type_hint(expr, info) == TypeHint::Pointer
                        || self.infer_expr_type(expr, info).is_some_and(|ut| ut.is_pointer()) {
                        self.writeln(&format!("{}!{}.is_null()", body_indent, rust_expr));
                    } else {
                        self.writeln(&format!("{}{} != 0", body_indent, strip_outer_parens(&rust_expr)));
                    }
                } else if let Some(casted) = self.cast_return_expr_if_needed(expr, info, &rust_expr) {
                    self.writeln(&format!("{}{}", body_indent, strip_outer_parens(&casted)));
                } else {
                    self.writeln(&format!("{}{}", body_indent, strip_outer_parens(&rust_expr)));
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
    /// 副作用: 各パラメータの型を current_param_types に登録する
    fn build_param_list(&mut self, info: &MacroInferInfo) -> String {
        let mut_params = collect_mut_params(&info.parse_result, &info.params);
        let mut parts = Vec::new();
        for (i, p) in info.params.iter().enumerate() {
            if info.generic_type_params.contains_key(&(i as i32)) {
                continue;
            }
            let name = escape_rust_keyword(self.interner.get(p.name));
            let ty = self.get_param_type(p, info, i);
            // current_param_types に登録（bool 判定等で使用）
            self.current_param_types.insert(p.name, UnifiedType::from_rust_str(&ty));
            let mut_prefix = if mut_params.contains(&p.name) { "mut " } else { "" };
            parts.push(format!("{}{}: {}", mut_prefix, name, ty));
        }
        parts.join(", ")
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
        let should_be_const = self.const_pointer_positions.contains(&param_index);

        // 全制約を Tier 順（高い方優先）で収集し、最高 Tier の型を採用
        let mut best: Option<(&crate::type_repr::TypeRepr, u8)> = None;

        let expr_ids_from_param_to_exprs: Vec<_> = info.type_env.param_to_exprs
            .get(&param_name)
            .map(|ids| ids.iter().cloned().collect())
            .unwrap_or_default();
        let all_expr_ids = {
            let mut ids = expr_ids_from_param_to_exprs;
            ids.push(param.expr_id());
            ids
        };

        for expr_id in &all_expr_ids {
            if let Some(constraints) = info.type_env.expr_constraints.get(expr_id) {
                for c in constraints {
                    if c.ty.is_void() { continue; }
                    let tier = c.ty.confidence_tier();
                    if best.is_none() || tier < best.unwrap().1 {
                        best = Some((&c.ty, tier));
                    }
                }
            }
        }

        if let Some((ty, _tier)) = best {
            let mut ty = ty.clone();
            if should_be_const {
                ty.make_outer_pointer_const();
            } else if ty.has_outer_pointer() {
                // must-mut: Phase 2 で *mut と確定 → *const になっていたら *mut に戻す
                ty.make_outer_pointer_mut();
            }
            return self.type_repr_to_rust(&ty);
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

        // 依存順パスで bool と判定されていればそのまま bool を返す
        if self.is_bool_return {
            return "bool".to_string();
        }

        let macro_name = self.interner.get(info.name);
        match &info.parse_result {
            ParseResult::Expression(expr) => {
                if let Some(ty) = info.get_return_type() {
                    let mut ty_str = self.type_repr_to_rust(ty);
                    if ty_str != "()" {
                        // 式の推論型がポインタで const なら戻り値も const に
                        if ty_str.contains("*mut") {
                            if let Some(expr_ut) = self.infer_expr_type(expr, info) {
                                if expr_ut.is_const_pointer() {
                                    ty_str = ty_str.replace("*mut", "*const");
                                }
                            }
                        }
                        return ty_str;
                    }
                    // "()" が返された場合: 式の実際の型を確認
                    // 式が本当に void を返す(void 関数呼び出し等)なら "()" で正しい
                    // そうでなければ型推論の誤りなのでフォールバック
                    if let Some(ut) = self.infer_expr_type(expr, info) {
                        let s = ut.to_rust_string();
                        if s != "()" {
                            return s;
                        }
                    }
                    // 式型推論でも "()" → void で正しい
                    return ty_str;
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
        self.expr_to_rust_ctx(expr, info, ExprContext::Default)
    }

    fn expr_to_rust_ctx(&mut self, expr: &Expr, info: &MacroInferInfo, ctx: ExprContext) -> String {
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
                // C の char は i8 なので b'x' as i8 として出力
                if c.is_ascii() {
                    format!("b'{}' as i8", escape_char(*c))
                } else {
                    format!("0x{:02x}u8 as i8", c)
                }
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
                    if is_null_literal(rhs) {
                        let is_ptr = lh == TypeHint::Pointer
                            || self.infer_expr_type(lhs, info).is_some_and(|ut| ut.is_pointer());
                        if is_ptr {
                            let l = self.expr_to_rust(lhs, info);
                            return if *op == BinOp::Eq {
                                format!("{}.is_null()", l)
                            } else {
                                format!("!{}.is_null()", l)
                            };
                        }
                    }
                    if is_null_literal(lhs) {
                        let is_ptr = rh == TypeHint::Pointer
                            || self.infer_expr_type(rhs, info).is_some_and(|ut| ut.is_pointer());
                        if is_ptr {
                            let r = self.expr_to_rust(rhs, info);
                            return if *op == BinOp::Eq {
                                format!("{}.is_null()", r)
                            } else {
                                format!("!{}.is_null()", r)
                            };
                        }
                    }
                    // bool_expr != 0 → bool_expr, bool_expr == 0 → !bool_expr
                    if self.is_bool_expr_with_dict(lhs) {
                        match (&rhs.kind, op) {
                            (ExprKind::IntLit(0), BinOp::Ne) => {
                                return self.expr_to_rust(lhs, info);
                            }
                            (ExprKind::IntLit(0), BinOp::Eq) => {
                                let l = self.expr_to_rust(lhs, info);
                                return format!("!{}", l);
                            }
                            (ExprKind::IntLit(1), BinOp::Eq) => {
                                return self.expr_to_rust(lhs, info);
                            }
                            (ExprKind::IntLit(1), BinOp::Ne) => {
                                let l = self.expr_to_rust(lhs, info);
                                return format!("!{}", l);
                            }
                            _ => {}
                        }
                    }
                    if self.is_bool_expr_with_dict(rhs) {
                        match (&lhs.kind, op) {
                            (ExprKind::IntLit(0), BinOp::Ne) => {
                                return self.expr_to_rust(rhs, info);
                            }
                            (ExprKind::IntLit(0), BinOp::Eq) => {
                                let r = self.expr_to_rust(rhs, info);
                                return format!("!{}", r);
                            }
                            (ExprKind::IntLit(1), BinOp::Eq) => {
                                return self.expr_to_rust(rhs, info);
                            }
                            (ExprKind::IntLit(1), BinOp::Ne) => {
                                let r = self.expr_to_rust(rhs, info);
                                return format!("!{}", r);
                            }
                            _ => {}
                        }
                    }
                }

                // ポインタ ± 整数 → .offset()
                if matches!(op, BinOp::Add | BinOp::Sub) {
                    let lp = lh == TypeHint::Pointer
                        || self.infer_expr_type(lhs, info).is_some_and(|ut| ut.is_pointer());
                    let rp = rh == TypeHint::Pointer
                        || self.infer_expr_type(rhs, info).is_some_and(|ut| ut.is_pointer());
                    if lp && !rp {
                        let l = self.expr_to_rust(lhs, info);
                        let r = self.expr_to_rust(rhs, info);
                        return if *op == BinOp::Add {
                            format!("{}.offset({} as isize)", l, r)
                        } else {
                            format!("{}.offset(-({} as isize))", l, r)
                        };
                    }
                    if rp && !lp && *op == BinOp::Add {
                        let l = self.expr_to_rust(lhs, info);
                        let r = self.expr_to_rust(rhs, info);
                        return format!("{}.offset({} as isize)", r, l);
                    }
                    // ポインタ - ポインタ → .offset_from()
                    if lp && rp && *op == BinOp::Sub {
                        let l = self.expr_to_rust(lhs, info);
                        let r = self.expr_to_rust(rhs, info);
                        return format!("{}.offset_from({})", l, r);
                    }
                }

                // float vs int literal → int literal を float に変換
                if matches!(&rhs.kind, ExprKind::IntLit(_)) {
                    if let Some(lut) = self.infer_expr_type(lhs, info) {
                        if lut.is_float() {
                            let l = self.expr_to_rust(lhs, info);
                            if let ExprKind::IntLit(v) = &rhs.kind {
                                return format!("({} {} {}.0)", l, bin_op_to_rust(*op), v);
                            }
                        }
                    }
                }
                if matches!(&lhs.kind, ExprKind::IntLit(_)) {
                    if let Some(rut) = self.infer_expr_type(rhs, info) {
                        if rut.is_float() {
                            let r = self.expr_to_rust(rhs, info);
                            if let ExprKind::IntLit(v) = &lhs.kind {
                                return format!("({}.0 {} {})", v, bin_op_to_rust(*op), r);
                            }
                        }
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
                    BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor
                    | BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod
                    | BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
                        let lt = self.infer_expr_type(lhs, info);
                        let rt = self.infer_expr_type(rhs, info);
                        if let (Some(lut), Some(rut)) = (&lt, &rt) {
                            // bool オペランドを整数にキャスト（C の暗黙変換）
                            if rut.is_bool() {
                                let ls = lut.to_rust_string();
                                if let Some(nl) = normalize_integer_type(&ls) {
                                    return format!("({} {} ({} as {}))", l, bin_op_to_rust(*op), r, nl);
                                }
                            }
                            if lut.is_bool() {
                                let rs = rut.to_rust_string();
                                if let Some(nr) = normalize_integer_type(&rs) {
                                    return format!("(({} as {}) {} {})", l, nr, bin_op_to_rust(*op), r);
                                }
                            }
                            // float vs integer: 整数オペランドを float にキャスト
                            if lut.is_float() && !rut.is_float() {
                                let ls = lut.to_rust_string();
                                let float_ty = if ls == "c_float" || ls == "f32" { "f32" } else { "f64" };
                                return format!("({} {} ({} as {}))", l, bin_op_to_rust(*op), r, float_ty);
                            }
                            if rut.is_float() && !lut.is_float() {
                                let rs = rut.to_rust_string();
                                let float_ty = if rs == "c_float" || rs == "f32" { "f32" } else { "f64" };
                                return format!("(({} as {}) {} {})", l, float_ty, bin_op_to_rust(*op), r);
                            }
                            let ls = lut.to_rust_string();
                            let rs = rut.to_rust_string();
                            if let Some(wider) = wider_integer_type(&ls, &rs) {
                                let norm_l = normalize_integer_type(&ls);
                                if norm_l != Some(wider) {
                                    return format!("(({} as {}) {} {})", l, wider, bin_op_to_rust(*op), r);
                                } else {
                                    return format!("({} {} ({} as {}))", l, bin_op_to_rust(*op), r, wider);
                                }
                            }
                        }
                        // float vs integer (片方のみ型が判明): 整数を float にキャスト
                        match (&lt, &rt) {
                            (Some(lut), None) if lut.is_float() => {
                                let ls = lut.to_rust_string();
                                let float_ty = if ls == "c_float" || ls == "f32" { "f32" } else { "f64" };
                                return format!("({} {} ({} as {}))", l, bin_op_to_rust(*op), r, float_ty);
                            }
                            (None, Some(rut)) if rut.is_float() => {
                                let rs = rut.to_rust_string();
                                let float_ty = if rs == "c_float" || rs == "f32" { "f32" } else { "f64" };
                                return format!("(({} as {}) {} {})", l, float_ty, bin_op_to_rust(*op), r);
                            }
                            _ => {}
                        }
                        // ビット演算で片方のみ型が判明している場合、他方をキャスト
                        if matches!(op, BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor) {
                            match (&lt, &rt) {
                                (Some(lut), None) => {
                                    let ls = lut.to_rust_string();
                                    if let Some(nl) = normalize_integer_type(&ls) {
                                        return format!("({} {} ({} as {}))", l, bin_op_to_rust(*op), r, nl);
                                    }
                                }
                                (None, Some(rut)) => {
                                    let rs = rut.to_rust_string();
                                    if let Some(nr) = normalize_integer_type(&rs) {
                                        return format!("(({} as {}) {} {})", l, nr, bin_op_to_rust(*op), r);
                                    }
                                }
                                _ => {}
                            }
                        }
                        format!("({} {} {})", l, bin_op_to_rust(*op), r)
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
                    // 内側が既に bool を返す式なら != 0 は不要
                    if self.is_bool_expr_with_dict(inner) {
                        e
                    } else if self.infer_type_hint(inner, info) == TypeHint::Pointer
                        || self.infer_expr_type(inner, info).is_some_and(|ut| ut.is_pointer()) {
                        // ポインタ → bool: !ptr.is_null()
                        format!("!{}.is_null()", e)
                    } else {
                        format!("({} != 0)", strip_outer_parens(&e))
                    }
                } else if self.is_enum_cast_target(type_name) {
                    // enum へのキャストは transmute を使用
                    format!("std::mem::transmute::<_, {}>({})", t, e)
                } else if ctx == ExprContext::Top {
                    format!("{} as {}", e, t)
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
                if self.infer_type_hint(inner, info) == TypeHint::Pointer {
                    format!("{{ {} = {}.wrapping_add(1); {} }}", e, e, e)
                } else {
                    format!("{{ {} += 1; {} }}", e, e)
                }
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
                if self.infer_type_hint(inner, info) == TypeHint::Pointer {
                    format!("{{ {} = {}.wrapping_sub(1); {} }}", e, e, e)
                } else {
                    format!("{{ {} -= 1; {} }}", e, e)
                }
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
                if self.infer_type_hint(inner, info) == TypeHint::Pointer {
                    format!("{{ let _t = {}; {} = {}.wrapping_add(1); _t }}", e, e, e)
                } else {
                    format!("{{ let _t = {}; {} += 1; _t }}", e, e)
                }
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
                if self.infer_type_hint(inner, info) == TypeHint::Pointer {
                    format!("{{ let _t = {}; {} = {}.wrapping_sub(1); _t }}", e, e, e)
                } else {
                    format!("{{ let _t = {}; {} -= 1; _t }}", e, e)
                }
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
                    // usize/u64 等の unsigned 型に単項マイナスは不可
                    if let Some(ut) = self.infer_expr_type(inner, info) {
                        let ts = ut.to_rust_string();
                        if matches!(normalize_integer_type(&ts), Some("usize" | "u8" | "u16" | "u32" | "u64")) {
                            self.codegen_errors.push(format!("cannot negate unsigned type: -({}: {})", e, ts));
                        }
                    }
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
                let cond_str = self.wrap_as_bool_condition_macro(cond, &c, info);
                let type_hint = self.current_return_type.as_ref().map(|ut| ut.to_rust_string());
                // null リテラル分岐の型推論
                let tt = self.infer_expr_type(then_expr, info);
                let et = self.infer_expr_type(else_expr, info);
                if is_null_literal(else_expr) {
                    if let Some(ref tut) = tt {
                        if tut.is_pointer() {
                            let t = self.expr_with_type_hint(then_expr, info, type_hint.as_deref());
                            let e = null_ptr_expr(tut);
                            return format!("(if {} {{ {} }} else {{ {} }})", strip_outer_parens(&cond_str), t, e);
                        }
                    }
                }
                if is_null_literal(then_expr) {
                    if let Some(ref eut) = et {
                        if eut.is_pointer() {
                            let t = null_ptr_expr(eut);
                            let e = self.expr_with_type_hint(else_expr, info, type_hint.as_deref());
                            return format!("(if {} {{ {} }} else {{ {} }})", strip_outer_parens(&cond_str), t, e);
                        }
                    }
                }
                let t = self.expr_with_type_hint(then_expr, info, type_hint.as_deref());
                let e = self.expr_with_type_hint(else_expr, info, type_hint.as_deref());
                // if/else ブランチの型が異なる場合、wider type にキャスト
                if let (Some(tut), Some(eut)) = (&tt, &et) {
                    let ts = tut.to_rust_string();
                    let es = eut.to_rust_string();
                    let nt = normalize_integer_type(&ts);
                    let ne = normalize_integer_type(&es);
                    if let (Some(tn), Some(en)) = (nt, ne) {
                        if tn != en {
                            if let Some(wider) = wider_integer_type(&ts, &es) {
                                let norm_t = normalize_integer_type(&ts);
                                if norm_t != Some(wider) {
                                    return format!("(if {} {{ {} as {} }} else {{ {} }})", strip_outer_parens(&cond_str), t, wider, e);
                                } else {
                                    return format!("(if {} {{ {} }} else {{ {} as {} }})", strip_outer_parens(&cond_str), t, e, wider);
                                }
                            }
                        }
                    }
                }
                format!("(if {} {{ {} }} else {{ {} }})", strip_outer_parens(&cond_str), t, e)
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
                    match self.try_expand_call_as_lvalue(func, args, info) {
                        Some(expanded) => expanded,
                        None => {
                            let lhs_str = self.expr_to_rust(lhs, info);
                            self.codegen_errors.push(format!("invalid lvalue: {} cannot be assigned to", lhs_str));
                            lhs_str
                        }
                    }
                } else {
                    self.expr_to_rust(lhs, info)
                };
                let lhs_ut = self.infer_expr_type(lhs, info);
                let r = if is_null_literal(rhs) && *op == AssignOp::Assign {
                    if let Some(ref lut) = lhs_ut {
                        if lut.is_pointer() {
                            if lut.is_const_pointer() {
                                "std::ptr::null()".to_string()
                            } else {
                                "std::ptr::null_mut()".to_string()
                            }
                        } else {
                            "0".to_string()
                        }
                    } else {
                        "std::ptr::null_mut()".to_string()
                    }
                } else {
                    let r_str = self.expr_to_rust(rhs, info);
                    if *op == AssignOp::Assign {
                        if let Some(ref lut) = lhs_ut {
                            if let Some(rut) = self.infer_expr_type(rhs, info) {
                                let ls = lut.to_rust_string();
                                let rs = rut.to_rust_string();
                                if let (Some(nl), Some(nr)) = (normalize_integer_type(&ls), normalize_integer_type(&rs)) {
                                    if !integer_types_compatible(nl, nr) {
                                        let r_stripped = strip_outer_parens(&r_str);
                                        let r_cast = if r_stripped.contains(' ') {
                                            format!("({}) as {}", r_stripped, nl)
                                        } else {
                                            format!("{} as {}", r_stripped, nl)
                                        };
                                        return format!("{{ {} = {}; {} }}", l, r_cast, l);
                                    }
                                }
                            }
                        }
                    }
                    r_str
                };
                match op {
                    AssignOp::Assign => format!("{{ {} = {}; {} }}", l, strip_outer_parens(&r), l),
                    AssignOp::AddAssign | AssignOp::SubAssign => {
                        if self.infer_type_hint(lhs, info) == TypeHint::Pointer {
                            let method = if *op == AssignOp::AddAssign { "wrapping_add" } else { "wrapping_sub" };
                            format!("{{ {} = {}.{}({} as usize); {} }}", l, l, method, r, l)
                        } else {
                            format!("{{ {} {} {}; {} }}", l, assign_op_to_rust(*op), strip_outer_parens(&r), l)
                        }
                    }
                    AssignOp::AndAssign | AssignOp::OrAssign | AssignOp::XorAssign => {
                        let lt = self.infer_expr_type(lhs, info);
                        let rt = self.infer_expr_type(rhs, info);
                        if let (Some(lut), Some(rut)) = (&lt, &rt) {
                            let ls = lut.to_rust_string();
                            let rs = rut.to_rust_string();
                            let nl = normalize_integer_type(&ls);
                            let nr = normalize_integer_type(&rs);
                            if nl.is_some() && nr.is_some() && nl != nr {
                                let target = nl.unwrap();
                                return format!("{{ {} {} {} as {}; {} }}", l, assign_op_to_rust(*op), r, target, l);
                            }
                        }
                        // 片方のみ型が判明: LHS型に合わせてRHSをキャスト
                        if let (Some(lut), None) = (&lt, &rt) {
                            let ls = lut.to_rust_string();
                            if let Some(nl) = normalize_integer_type(&ls) {
                                return format!("{{ {} {} {} as {}; {} }}", l, assign_op_to_rust(*op), r, nl, l);
                            }
                        }
                        format!("{{ {} {} {}; {} }}", l, assign_op_to_rust(*op), strip_outer_parens(&r), l)
                    }
                    _ => format!("{{ {} {} {}; {} }}", l, assign_op_to_rust(*op), strip_outer_parens(&r), l),
                }
            }
            ExprKind::Assert { kind, condition } => {
                // assert(expr || !"message") パターンの検出
                let assert_expr = if let Some((real_cond, msg)) = decompose_assert_with_message(condition) {
                    let c = self.expr_to_rust(real_cond, info);
                    let cond_str = self.wrap_as_bool_condition_macro(real_cond, &c, info);
                    format!("assert!({}, \"{}\")", strip_outer_parens(&cond_str), msg)
                } else {
                    let cond = self.expr_to_rust(condition, info);
                    if is_boolean_expr(condition) || self.is_bool_expr_with_dict(condition) {
                        format!("assert!({})", strip_outer_parens(&cond))
                    } else if self.infer_type_hint(condition, info) == TypeHint::Pointer {
                        format!("assert!(!{}.is_null())", cond)
                    } else {
                        format!("assert!({} != 0)", strip_outer_parens(&cond))
                    }
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
                            self.collect_decl_types(decl);
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
            let ut = UnifiedType::from_rust_str(ty);
            if ut.is_pointer() && is_null_literal(expr) {
                return null_ptr_expr(&ut);
            }
            if ut.is_bool() {
                match &expr.kind {
                    ExprKind::IntLit(0) => return "false".to_string(),
                    ExprKind::IntLit(1) => return "true".to_string(),
                    _ => {}
                }
            }
        }
        self.expr_to_rust_ctx(expr, info, ExprContext::Top)
    }

    /// 式を Rust コードに変換（型ヒント付き、inline 関数用）
    fn expr_with_type_hint_inline(&mut self, expr: &Expr, type_hint: Option<&str>) -> String {
        if let Some(ty) = type_hint {
            let ut = UnifiedType::from_rust_str(ty);
            if ut.is_pointer() && is_null_literal(expr) {
                return null_ptr_expr(&ut);
            }
            if ut.is_bool() {
                match &expr.kind {
                    ExprKind::IntLit(0) => return "false".to_string(),
                    ExprKind::IntLit(1) => return "true".to_string(),
                    _ => {}
                }
            }
        }
        self.expr_to_rust_inline_ctx(expr, ExprContext::Top)
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
                    if rt.is_pointer() && is_null_literal(expr) {
                        return format!("return {};", null_ptr_expr(rt));
                    }
                    if rt.is_bool() {
                        match &expr.kind {
                            ExprKind::IntLit(0) => return "return false;".to_string(),
                            ExprKind::IntLit(1) => return "return true;".to_string(),
                            _ => {
                                let e = self.expr_to_rust(expr, info);
                                if !self.is_bool_expr_with_dict(expr) && !is_string_bool_expr(&e) {
                                    return format!("return {} != 0;", strip_outer_parens(&e));
                                }
                                return format!("return {};", strip_outer_parens(&e));
                            }
                        }
                    }
                }
                let e = self.expr_to_rust(expr, info);
                if let Some(casted) = self.cast_return_expr_if_needed(expr, info, &e) {
                    return format!("return {};", strip_outer_parens(&casted));
                }
                format!("return {};", strip_outer_parens(&e))
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
                let name_str = self.interner.get(*name).to_string();
                // 未定義型名の検出
                if !self.known_symbols.contains(&name_str) {
                    self.codegen_errors.push(format!("undefined type: {}", name_str));
                }
                return name_str;
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
        self.apply_simple_derived_with_specs_const(base, derived, false)
    }

    /// 派生型を適用（specs の const 情報を考慮）
    fn apply_simple_derived_with_specs_const(&self, base: &str, derived: &[DerivedDecl], specs_is_const: bool) -> String {
        let mut result = base.to_string();
        let mut is_first_pointer = true;
        for d in derived.iter().rev() {
            match d {
                DerivedDecl::Pointer(quals) => {
                    // void ポインタの場合は c_void を使用
                    if result == "()" {
                        result = "c_void".to_string();
                    }
                    // C の "const T *p": specs.is_const が pointee const を表す
                    // C の "T * const p": quals.is_const はポインタ自体の const (Rust では *mut)
                    // ただし "const T * const p" は両方 const → *const
                    let pointee_const = if is_first_pointer {
                        specs_is_const
                    } else {
                        quals.is_const
                    };
                    if pointee_const {
                        result = format!("*const {}", result);
                    } else {
                        result = format!("*mut {}", result);
                    }
                    is_first_pointer = false;
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

        // mutable パラメータ/ローカル変数を検出
        let mut_params = {
            let mut all_names = HashSet::new();
            // パラメータ名を収集
            for d in &func_def.declarator.derived {
                if let DerivedDecl::Function(param_list) = d {
                    for p in &param_list.params {
                        if let Some(ref declarator) = p.declarator {
                            if let Some(param_name) = declarator.name {
                                all_names.insert(param_name);
                            }
                        }
                    }
                }
            }
            // ローカル変数名も収集
            for item in &func_def.body.items {
                if let BlockItem::Decl(decl) = item {
                    for init_decl in &decl.declarators {
                        if let Some(var_name) = init_decl.declarator.name {
                            all_names.insert(var_name);
                        }
                    }
                }
            }
            let mut result = HashSet::new();
            for item in &func_def.body.items {
                if let BlockItem::Stmt(stmt) = item {
                    collect_mut_params_from_stmt(stmt, &all_names, &mut result);
                }
            }
            result
        };

        // mut ローカル変数名を保存（decl_to_rust_let で使用）
        self.mut_local_names = mut_params.clone();

        // パラメータリストを取得
        let params_str = self.build_fn_param_list(&func_def.declarator.derived, &mut_params);

        // 戻り値の型を取得（基本型）
        let return_type = self.decl_specs_to_rust(&func_def.specs);

        // declarator の派生型（ポインタなど）を適用（Function を除く）
        // 例: HEK * func(...) の場合、derived = [Pointer, Function]
        //     戻り値型は HEK に Pointer を適用して *mut HEK になる
        let return_derived: Vec<_> = func_def.declarator.derived.iter()
            .filter(|d| !matches!(d, DerivedDecl::Function(_)))
            .cloned()
            .collect();
        let return_type = self.apply_simple_derived_with_specs_const(&return_type, &return_derived, func_def.specs.qualifiers.is_const);
        self.current_return_type = Some(UnifiedType::from_rust_str(&return_type));

        // パラメータの型情報を収集 + ローカルスコープに登録
        for d in &func_def.declarator.derived {
            if let DerivedDecl::Function(param_list) = d {
                for p in &param_list.params {
                    if let Some(ref declarator) = p.declarator {
                        if let Some(param_name) = declarator.name {
                            let ty = self.param_type_only(p);
                            self.current_param_types.insert(param_name, UnifiedType::from_rust_str(&ty));
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

        // AST ダンプ（デバッグ用）
        self.dump_ast_comment_for_body(name_str, &func_def.body);

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
    fn build_fn_param_list(&mut self, derived: &[DerivedDecl], mut_params: &HashSet<InternedStr>) -> String {
        for d in derived {
            if let DerivedDecl::Function(param_list) = d {
                let params: Vec<_> = param_list.params.iter()
                    .map(|p| self.param_decl_to_rust(p, mut_params))
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
    fn param_decl_to_rust(&mut self, param: &ParamDecl, mut_params: &HashSet<InternedStr>) -> String {
        let param_name_interned = param.declarator
            .as_ref()
            .and_then(|d| d.name);
        let name = param_name_interned
            .map(|n| escape_rust_keyword(self.interner.get(n)))
            .unwrap_or_else(|| "_".to_string());

        let ty = self.decl_specs_to_rust(&param.specs);

        // ポインタ派生型を適用（specs の const を考慮）
        let ty = if let Some(ref declarator) = param.declarator {
            self.apply_simple_derived_with_specs_const(&ty, &declarator.derived, param.specs.qualifiers.is_const)
        } else {
            ty
        };

        // current_param_types に登録（型推論で使用）
        if let Some(n) = param_name_interned {
            self.current_param_types.insert(n, UnifiedType::from_rust_str(&ty));
        }

        let mut_prefix = if param_name_interned.is_some_and(|n| mut_params.contains(&n)) {
            "mut "
        } else {
            ""
        };

        format!("{}{}: {}", mut_prefix, name, ty)
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
                        // 宣言型と式の推論型が異なる整数型なら as キャストを挿入
                        let init_expr = if let Some(expr_ut) = self.infer_expr_type_inline(expr) {
                            let decl_s = ty.clone();
                            let expr_s = expr_ut.to_rust_string();
                            let nd = normalize_integer_type(&decl_s);
                            let ne = normalize_integer_type(&expr_s);
                            if let (Some(d), Some(e)) = (nd, ne) {
                                if !integer_types_compatible(d, e) {
                                    format!("({} as {})", init_expr, d)
                                } else {
                                    init_expr
                                }
                            } else {
                                init_expr
                            }
                        } else {
                            init_expr
                        };
                        // ポインタの const/mut 不一致: 変数型を *const に変更
                        let ty = if ty.contains("*mut") && !ty.contains("*const") {
                            if let Some(expr_ut) = self.infer_expr_type_inline(expr) {
                                if expr_ut.is_const_pointer() {
                                    ty.replace("*mut", "*const")
                                } else {
                                    ty
                                }
                            } else if init_expr.contains("as *const") {
                                ty.replace("*mut", "*const")
                            } else {
                                ty
                            }
                        } else {
                            ty
                        };
                        // null リテラルの場合は型推論に任せて std::ptr::null_mut()
                        let init_expr = if is_null_literal(expr) && ty.contains("*mut") {
                            "std::ptr::null_mut()".to_string()
                        } else if is_null_literal(expr) && ty.contains("*const") {
                            "std::ptr::null()".to_string()
                        } else {
                            init_expr
                        };
                        let mut_kw = if init_decl.declarator.name.is_some_and(|n| self.mut_local_names.contains(&n)) { "mut " } else { "" };
                        result.push_str(&format!("{}let {}{}: {} = {};\n", indent, mut_kw, name, ty, strip_outer_parens(&init_expr)));
                    }
                    Initializer::List(_) => {
                        // 初期化リストは複雑なので TODO
                        result.push_str(&format!("{}let {}: {} = /* init list */;\n", indent, name, ty));
                    }
                }
            } else {
                // 初期化子なし（未初期化変数 - Rust では unsafe かデフォルト値が必要）
                let mut_kw = if init_decl.declarator.name.is_some_and(|n| self.mut_local_names.contains(&n)) { "mut " } else { "" };
                result.push_str(&format!("{}let {}{}: {}; // uninitialized\n", indent, mut_kw, name, ty));
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
                    self.collect_decl_types(decl);
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
                        match self.try_expand_call_as_lvalue_inline(func, args) {
                            Some(expanded) => expanded,
                            None => {
                                let lhs_str = self.expr_to_rust_inline(lhs);
                                self.codegen_errors.push(format!("invalid lvalue: {} cannot be assigned to", lhs_str));
                                lhs_str
                            }
                        }
                    } else {
                        self.expr_to_rust_inline(lhs)
                    };
                    let lhs_ut = self.infer_expr_type_inline(lhs);
                    let r = if is_null_literal(rhs) && *op == AssignOp::Assign {
                        // null リテラル: LHS がポインタなら null_mut/null, 整数なら 0
                        if let Some(ref lut) = lhs_ut {
                            if lut.is_pointer() {
                                if lut.is_const_pointer() {
                                    "std::ptr::null()".to_string()
                                } else {
                                    "std::ptr::null_mut()".to_string()
                                }
                            } else {
                                "0".to_string()
                            }
                        } else {
                            "std::ptr::null_mut()".to_string()
                        }
                    } else {
                        let r_str = self.expr_to_rust_inline_ctx(rhs, ExprContext::Top);
                        // 整数型の幅不一致キャスト
                        if *op == AssignOp::Assign {
                            if let Some(ref lut) = lhs_ut {
                                if let Some(rut) = self.infer_expr_type_inline(rhs) {
                                    let ls = lut.to_rust_string();
                                    let rs = rut.to_rust_string();
                                    if let (Some(nl), Some(nr)) = (normalize_integer_type(&ls), normalize_integer_type(&rs)) {
                                        if !integer_types_compatible(nl, nr) {
                                            // Binary 式等の場合は括弧が必要（as の優先順位が高い）
                                            let r_stripped = strip_outer_parens(&r_str);
                                            let r_cast = if r_stripped.contains(' ') {
                                                format!("({}) as {}", r_stripped, nl)
                                            } else {
                                                format!("{} as {}", r_stripped, nl)
                                            };
                                            return format!("{}{} = {};", indent, l, r_cast);
                                        }
                                    }
                                }
                            }
                        }
                        r_str
                    };
                    match op {
                        AssignOp::Assign => format!("{}{} = {};", indent, l, strip_outer_parens(&r)),
                        AssignOp::AddAssign | AssignOp::SubAssign => {
                            if self.is_pointer_expr_inline(lhs) {
                                let method = if *op == AssignOp::AddAssign { "wrapping_add" } else { "wrapping_sub" };
                                format!("{}{} = {}.{}({} as usize);", indent, l, l, method, r)
                            } else {
                                format!("{}{} {} {};", indent, l, assign_op_to_rust(*op), strip_outer_parens(&r))
                            }
                        }
                        AssignOp::AndAssign | AssignOp::OrAssign | AssignOp::XorAssign => {
                            let lt = self.infer_expr_type_inline(lhs);
                            let rt = self.infer_expr_type_inline(rhs);
                            if let (Some(lut), Some(rut)) = (&lt, &rt) {
                                let ls = lut.to_rust_string();
                                let rs = rut.to_rust_string();
                                let nl = normalize_integer_type(&ls);
                                let nr = normalize_integer_type(&rs);
                                if nl.is_some() && nr.is_some() && nl != nr {
                                    let target = nl.unwrap();
                                    return format!("{}{} {} {} as {};", indent, l, assign_op_to_rust(*op), r, target);
                                }
                            }
                            // 片方のみ型が判明: LHS型に合わせてRHSをキャスト
                            if let (Some(lut), None) = (&lt, &rt) {
                                let ls = lut.to_rust_string();
                                if let Some(nl) = normalize_integer_type(&ls) {
                                    return format!("{}{} {} {} as {};", indent, l, assign_op_to_rust(*op), r, nl);
                                }
                            }
                            format!("{}{} {} {};", indent, l, assign_op_to_rust(*op), strip_outer_parens(&r))
                        }
                        _ => format!("{}{} {} {};", indent, l, assign_op_to_rust(*op), strip_outer_parens(&r)),
                    }
                } else {
                    format!("{}{};", indent, self.expr_to_rust_inline(expr))
                }
            }
            Stmt::Expr(None, _) => String::new(),  // 空文は出力しない
            Stmt::Return(Some(expr), _) => {
                if let Some(ref rt) = self.current_return_type {
                    if rt.is_pointer() && is_null_literal(expr) {
                        return format!("{}return {};", indent, null_ptr_expr(rt));
                    }
                    if rt.is_bool() {
                        match &expr.kind {
                            ExprKind::IntLit(0) => return format!("{}return false;", indent),
                            ExprKind::IntLit(1) => return format!("{}return true;", indent),
                            _ => {
                                let e = self.expr_to_rust_inline(expr);
                                if !self.is_bool_expr_with_dict(expr) && !is_string_bool_expr(&e) {
                                    return format!("{}return {} != 0;", indent, strip_outer_parens(&e));
                                }
                                return format!("{}return {};", indent, strip_outer_parens(&e));
                            }
                        }
                    }
                }
                let e = self.expr_to_rust_inline(expr);
                if let Some(casted) = self.cast_return_expr_if_needed_inline(expr, &e) {
                    return format!("{}return {};", indent, strip_outer_parens(&casted));
                }
                format!("{}return {};", indent, strip_outer_parens(&e))
            }
            Stmt::Return(None, _) => format!("{}return;", indent),
            Stmt::If { cond, then_stmt, else_stmt, .. } => {
                let cond_str = self.expr_to_rust_inline(cond);
                // 条件が既に bool なら != 0 を追加しない
                let cond_bool = self.wrap_as_bool_condition_inline(cond, &cond_str);
                let mut result = format!("{}if {} {{\n", indent, strip_outer_parens(&cond_bool));
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
                            self.collect_decl_types(decl);
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
                            self.collect_decl_types(decl);
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
                    self.collect_decl_types(decl);
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
        self.expr_to_rust_inline_ctx(expr, ExprContext::Default)
    }

    fn expr_to_rust_inline_ctx(&mut self, expr: &Expr, ctx: ExprContext) -> String {
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
                // C の char は i8 なので b'x' as i8 として出力
                if c.is_ascii() {
                    format!("b'{}' as i8", escape_char(*c))
                } else {
                    format!("0x{:02x}u8 as i8", c)
                }
            }
            ExprKind::StringLit(s) => {
                format!("c\"{}\"", escape_string(s))
            }
            ExprKind::Binary { op, lhs, rhs } => {
                // ポインタ == 0 / != 0 → .is_null() (マクロ codegen と対称)
                if matches!(op, BinOp::Eq | BinOp::Ne) {
                    if is_null_literal(rhs) {
                        let is_ptr = self.is_pointer_expr_inline(lhs)
                            || self.infer_expr_type_inline(lhs).is_some_and(|ut| ut.is_pointer());
                        if is_ptr {
                            let l = self.expr_to_rust_inline(lhs);
                            return if *op == BinOp::Eq {
                                format!("{}.is_null()", l)
                            } else {
                                format!("!{}.is_null()", l)
                            };
                        }
                    }
                    if is_null_literal(lhs) {
                        let is_ptr = self.is_pointer_expr_inline(rhs)
                            || self.infer_expr_type_inline(rhs).is_some_and(|ut| ut.is_pointer());
                        if is_ptr {
                            let r = self.expr_to_rust_inline(rhs);
                            return if *op == BinOp::Eq {
                                format!("{}.is_null()", r)
                            } else {
                                format!("!{}.is_null()", r)
                            };
                        }
                    }
                    // bool_expr != 0 → bool_expr, bool_expr == 0 → !bool_expr
                    if self.is_bool_expr_with_dict(lhs) {
                        match (&rhs.kind, op) {
                            (ExprKind::IntLit(0), BinOp::Ne) => {
                                return self.expr_to_rust_inline(lhs);
                            }
                            (ExprKind::IntLit(0), BinOp::Eq) => {
                                let l = self.expr_to_rust_inline(lhs);
                                return format!("!{}", l);
                            }
                            (ExprKind::IntLit(1), BinOp::Eq) => {
                                return self.expr_to_rust_inline(lhs);
                            }
                            (ExprKind::IntLit(1), BinOp::Ne) => {
                                let l = self.expr_to_rust_inline(lhs);
                                return format!("!{}", l);
                            }
                            _ => {}
                        }
                    }
                    if self.is_bool_expr_with_dict(rhs) {
                        match (&lhs.kind, op) {
                            (ExprKind::IntLit(0), BinOp::Ne) => {
                                return self.expr_to_rust_inline(rhs);
                            }
                            (ExprKind::IntLit(0), BinOp::Eq) => {
                                let r = self.expr_to_rust_inline(rhs);
                                return format!("!{}", r);
                            }
                            (ExprKind::IntLit(1), BinOp::Eq) => {
                                return self.expr_to_rust_inline(rhs);
                            }
                            (ExprKind::IntLit(1), BinOp::Ne) => {
                                let r = self.expr_to_rust_inline(rhs);
                                return format!("!{}", r);
                            }
                            _ => {}
                        }
                    }
                }
                // ポインタ ± 整数 → .offset() (マクロ codegen と対称)
                if matches!(op, BinOp::Add | BinOp::Sub) {
                    let lp = self.is_pointer_expr_inline(lhs)
                        || self.infer_expr_type_inline(lhs).is_some_and(|ut| ut.is_pointer());
                    let rp = self.is_pointer_expr_inline(rhs)
                        || self.infer_expr_type_inline(rhs).is_some_and(|ut| ut.is_pointer());
                    if lp && !rp {
                        let l = self.expr_to_rust_inline(lhs);
                        let r = self.expr_to_rust_inline(rhs);
                        return if *op == BinOp::Add {
                            format!("{}.offset({} as isize)", l, r)
                        } else {
                            format!("{}.offset(-({} as isize))", l, r)
                        };
                    }
                    if rp && !lp && *op == BinOp::Add {
                        let l = self.expr_to_rust_inline(lhs);
                        let r = self.expr_to_rust_inline(rhs);
                        return format!("{}.offset({} as isize)", r, l);
                    }
                    // ポインタ - ポインタ → .offset_from()
                    if lp && rp && *op == BinOp::Sub {
                        let l = self.expr_to_rust_inline(lhs);
                        let r = self.expr_to_rust_inline(rhs);
                        return format!("{}.offset_from({})", l, r);
                    }
                }
                // float vs int literal → int literal を float に変換
                if matches!(&rhs.kind, ExprKind::IntLit(_)) {
                    if let Some(lut) = self.infer_expr_type_inline(lhs) {
                        if lut.is_float() {
                            let l = self.expr_to_rust_inline(lhs);
                            if let ExprKind::IntLit(v) = &rhs.kind {
                                return format!("({} {} {}.0)", l, bin_op_to_rust(*op), v);
                            }
                        }
                    }
                }
                if matches!(&lhs.kind, ExprKind::IntLit(_)) {
                    if let Some(rut) = self.infer_expr_type_inline(rhs) {
                        if rut.is_float() {
                            let r = self.expr_to_rust_inline(rhs);
                            if let ExprKind::IntLit(v) = &lhs.kind {
                                return format!("({}.0 {} {})", v, bin_op_to_rust(*op), r);
                            }
                        }
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
                    BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor
                    | BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod
                    | BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
                        let lt = self.infer_expr_type_inline(lhs);
                        let rt = self.infer_expr_type_inline(rhs);
                        if let (Some(lut), Some(rut)) = (&lt, &rt) {
                            // bool オペランドを整数にキャスト（C の暗黙変換）
                            if rut.is_bool() {
                                let ls = lut.to_rust_string();
                                if let Some(nl) = normalize_integer_type(&ls) {
                                    return format!("({} {} ({} as {}))", l, bin_op_to_rust(*op), r, nl);
                                }
                            }
                            if lut.is_bool() {
                                let rs = rut.to_rust_string();
                                if let Some(nr) = normalize_integer_type(&rs) {
                                    return format!("(({} as {}) {} {})", l, nr, bin_op_to_rust(*op), r);
                                }
                            }
                            // float vs integer: 整数オペランドを float にキャスト
                            if lut.is_float() && !rut.is_float() {
                                let ls = lut.to_rust_string();
                                let float_ty = if ls == "c_float" || ls == "f32" { "f32" } else { "f64" };
                                return format!("({} {} ({} as {}))", l, bin_op_to_rust(*op), r, float_ty);
                            }
                            if rut.is_float() && !lut.is_float() {
                                let rs = rut.to_rust_string();
                                let float_ty = if rs == "c_float" || rs == "f32" { "f32" } else { "f64" };
                                return format!("(({} as {}) {} {})", l, float_ty, bin_op_to_rust(*op), r);
                            }
                            let ls = lut.to_rust_string();
                            let rs = rut.to_rust_string();
                            if let Some(wider) = wider_integer_type(&ls, &rs) {
                                let norm_l = normalize_integer_type(&ls);
                                if norm_l != Some(wider) {
                                    return format!("(({} as {}) {} {})", l, wider, bin_op_to_rust(*op), r);
                                } else {
                                    return format!("({} {} ({} as {}))", l, bin_op_to_rust(*op), r, wider);
                                }
                            }
                        }
                        // float vs integer (片方のみ型が判明): 整数を float にキャスト
                        match (&lt, &rt) {
                            (Some(lut), None) if lut.is_float() => {
                                let ls = lut.to_rust_string();
                                let float_ty = if ls == "c_float" || ls == "f32" { "f32" } else { "f64" };
                                return format!("({} {} ({} as {}))", l, bin_op_to_rust(*op), r, float_ty);
                            }
                            (None, Some(rut)) if rut.is_float() => {
                                let rs = rut.to_rust_string();
                                let float_ty = if rs == "c_float" || rs == "f32" { "f32" } else { "f64" };
                                return format!("(({} as {}) {} {})", l, float_ty, bin_op_to_rust(*op), r);
                            }
                            _ => {}
                        }
                        // ビット演算で片方のみ型が判明している場合、他方をキャスト
                        if matches!(op, BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor) {
                            match (&lt, &rt) {
                                (Some(lut), None) => {
                                    let ls = lut.to_rust_string();
                                    if let Some(nl) = normalize_integer_type(&ls) {
                                        return format!("({} {} ({} as {}))", l, bin_op_to_rust(*op), r, nl);
                                    }
                                }
                                (None, Some(rut)) => {
                                    let rs = rut.to_rust_string();
                                    if let Some(nr) = normalize_integer_type(&rs) {
                                        return format!("(({} as {}) {} {})", l, nr, bin_op_to_rust(*op), r);
                                    }
                                }
                                _ => {}
                            }
                        }
                        format!("({} {} {})", l, bin_op_to_rust(*op), r)
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
                            value_args.push(self.expr_to_rust_inline_ctx(arg, ExprContext::Top));
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
                    let param_idx = i + arg_offset;
                    // null pointer パラメータへの 0 リテラル変換
                    if is_null_literal(arg) {
                        if let Some(expected_ut) = self.get_callee_param_type_extended(&f, param_idx) {
                            if expected_ut.is_pointer() {
                                return null_ptr_expr(&expected_ut);
                            }
                        }
                    }
                    if self.callee_param_is_bool(&f, param_idx) {
                        match &arg.kind {
                            ExprKind::IntLit(0) => return "false".to_string(),
                            ExprKind::IntLit(1) => return "true".to_string(),
                            _ => {}
                        }
                    }
                    let result = self.expr_to_rust_inline_ctx(arg, ExprContext::Top);
                    // 整数型の幅不一致キャスト挿入 (bindings.rs + inline 関数)
                    if let Some(expected_ut) = self.get_callee_param_type_extended(&f, param_idx) {
                        let actual_ut = self.infer_expr_type_inline(arg);
                        let actual_ty_str = actual_ut.as_ref().map(|ut| ut.to_rust_string());
                        let expected_ty_str = expected_ut.to_rust_string();
                        return self.cast_integer_arg_if_needed(&result, actual_ty_str.as_deref(), &expected_ty_str);
                    }
                    result
                }));
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
                    // 内側が既に bool を返す式なら != 0 は不要
                    if self.is_bool_expr_with_dict(inner) {
                        e
                    } else if self.is_pointer_expr_inline(inner)
                        || self.infer_expr_type_inline(inner).is_some_and(|ut| ut.is_pointer()) {
                        // ポインタ → bool: !ptr.is_null()
                        format!("!{}.is_null()", e)
                    } else {
                        format!("({} != 0)", strip_outer_parens(&e))
                    }
                } else if self.is_enum_cast_target(type_name) {
                    // enum へのキャストは transmute を使用
                    format!("std::mem::transmute::<_, {}>({})", t, e)
                } else if ctx == ExprContext::Top {
                    format!("{} as {}", e, t)
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
                if self.is_pointer_expr_inline(inner) {
                    format!("{{ {} = {}.wrapping_add(1); {} }}", e, e, e)
                } else {
                    format!("{{ {} += 1; {} }}", e, e)
                }
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
                if self.is_pointer_expr_inline(inner) {
                    format!("{{ {} = {}.wrapping_sub(1); {} }}", e, e, e)
                } else {
                    format!("{{ {} -= 1; {} }}", e, e)
                }
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
                if self.is_pointer_expr_inline(inner) {
                    format!("{{ let _t = {}; {} = {}.wrapping_add(1); _t }}", e, e, e)
                } else {
                    format!("{{ let _t = {}; {} += 1; _t }}", e, e)
                }
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
                if self.is_pointer_expr_inline(inner) {
                    format!("{{ let _t = {}; {} = {}.wrapping_sub(1); _t }}", e, e, e)
                } else {
                    format!("{{ let _t = {}; {} -= 1; _t }}", e, e)
                }
            }
            ExprKind::UnaryPlus(inner) => self.expr_to_rust_inline(inner),
            ExprKind::UnaryMinus(inner) => {
                let e = self.expr_to_rust_inline(inner);
                if is_unsigned_cast_expr(&e) {
                    format!("({}).wrapping_neg()", e.trim_start_matches('-'))
                } else {
                    if let Some(ut) = self.infer_expr_type_inline(inner) {
                        let ts = ut.to_rust_string();
                        if matches!(normalize_integer_type(&ts), Some("usize" | "u8" | "u16" | "u32" | "u64")) {
                            self.codegen_errors.push(format!("cannot negate unsigned type: -({}: {})", e, ts));
                        }
                    }
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
                let cond_str = self.wrap_as_bool_condition_inline(cond, &c);
                let type_hint = self.current_return_type.as_ref().map(|ut| ut.to_rust_string());
                // null リテラル分岐の型推論: 他方のポインタ型に合わせて null_mut()/null()
                let tt = self.infer_expr_type_inline(then_expr);
                let et = self.infer_expr_type_inline(else_expr);
                if is_null_literal(else_expr) {
                    if let Some(ref tut) = tt {
                        if tut.is_pointer() {
                            let t = self.expr_with_type_hint_inline(then_expr, type_hint.as_deref());
                            let e = null_ptr_expr(tut);
                            return format!("(if {} {{ {} }} else {{ {} }})", strip_outer_parens(&cond_str), t, e);
                        }
                    }
                }
                if is_null_literal(then_expr) {
                    if let Some(ref eut) = et {
                        if eut.is_pointer() {
                            let t = null_ptr_expr(eut);
                            let e = self.expr_with_type_hint_inline(else_expr, type_hint.as_deref());
                            return format!("(if {} {{ {} }} else {{ {} }})", strip_outer_parens(&cond_str), t, e);
                        }
                    }
                }
                let t = self.expr_with_type_hint_inline(then_expr, type_hint.as_deref());
                let e = self.expr_with_type_hint_inline(else_expr, type_hint.as_deref());
                // if/else ブランチの型が異なる場合、wider type にキャスト
                if let (Some(tut), Some(eut)) = (&tt, &et) {
                    let ts = tut.to_rust_string();
                    let es = eut.to_rust_string();
                    let nt = normalize_integer_type(&ts);
                    let ne = normalize_integer_type(&es);
                    if let (Some(tn), Some(en)) = (nt, ne) {
                        if tn != en {
                            if let Some(wider) = wider_integer_type(&ts, &es) {
                                let norm_t = normalize_integer_type(&ts);
                                if norm_t != Some(wider) {
                                    return format!("(if {} {{ {} as {} }} else {{ {} }})", strip_outer_parens(&cond_str), t, wider, e);
                                } else {
                                    return format!("(if {} {{ {} }} else {{ {} as {} }})", strip_outer_parens(&cond_str), t, e, wider);
                                }
                            }
                        }
                    }
                }
                format!("(if {} {{ {} }} else {{ {} }})", strip_outer_parens(&cond_str), t, e)
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
                let r = if is_null_literal(rhs) && *op == AssignOp::Assign {
                    if self.infer_expr_type_inline(lhs).is_some_and(|ut| ut.is_const_pointer()) {
                        "std::ptr::null()".to_string()
                    } else {
                        "std::ptr::null_mut()".to_string()
                    }
                } else {
                    self.expr_to_rust_inline(rhs)
                };
                match op {
                    AssignOp::Assign => format!("{{ {} = {}; {} }}", l, strip_outer_parens(&r), l),
                    AssignOp::AddAssign | AssignOp::SubAssign => {
                        if self.is_pointer_expr_inline(lhs) {
                            let method = if *op == AssignOp::AddAssign { "wrapping_add" } else { "wrapping_sub" };
                            format!("{{ {} = {}.{}({} as usize); {} }}", l, l, method, r, l)
                        } else {
                            format!("{{ {} {} {}; {} }}", l, assign_op_to_rust(*op), strip_outer_parens(&r), l)
                        }
                    }
                    AssignOp::AndAssign | AssignOp::OrAssign | AssignOp::XorAssign => {
                        let lt = self.infer_expr_type_inline(lhs);
                        let rt = self.infer_expr_type_inline(rhs);
                        if let (Some(lut), Some(rut)) = (&lt, &rt) {
                            let ls = lut.to_rust_string();
                            let rs = rut.to_rust_string();
                            let nl = normalize_integer_type(&ls);
                            let nr = normalize_integer_type(&rs);
                            if nl.is_some() && nr.is_some() && nl != nr {
                                let target = nl.unwrap();
                                return format!("{{ {} {} {} as {}; {} }}", l, assign_op_to_rust(*op), r, target, l);
                            }
                        }
                        // 片方のみ型が判明: LHS型に合わせてRHSをキャスト
                        if let (Some(lut), None) = (&lt, &rt) {
                            let ls = lut.to_rust_string();
                            if let Some(nl) = normalize_integer_type(&ls) {
                                return format!("{{ {} {} {} as {}; {} }}", l, assign_op_to_rust(*op), r, nl, l);
                            }
                        }
                        format!("{{ {} {} {}; {} }}", l, assign_op_to_rust(*op), strip_outer_parens(&r), l)
                    }
                    _ => format!("{{ {} {} {}; {} }}", l, assign_op_to_rust(*op), strip_outer_parens(&r), l),
                }
            }
            ExprKind::Assert { kind, condition } => {
                // assert(expr || !"message") パターンの検出
                let assert_expr = if let Some((real_cond, msg)) = decompose_assert_with_message(condition) {
                    let c = self.expr_to_rust_inline(real_cond);
                    let cond_str = self.wrap_as_bool_condition_inline(real_cond, &c);
                    format!("assert!({}, \"{}\")", strip_outer_parens(&cond_str), msg)
                } else {
                    let cond = self.expr_to_rust_inline(condition);
                    if is_boolean_expr(condition) || self.is_bool_expr_with_dict(condition) {
                        format!("assert!({})", strip_outer_parens(&cond))
                    } else if self.is_pointer_expr_inline(condition) {
                        format!("assert!(!{}.is_null())", cond)
                    } else {
                        format!("assert!({} != 0)", strip_outer_parens(&cond))
                    }
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
                            self.collect_decl_types(decl);
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
            const_pointer_params: HashMap::new(),
            bool_return_macros: HashSet::new(),
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

        // Phase 2 の解析結果を収集（inline/macro 両方で使用）
        for (&name, info) in &result.infer_ctx.macros {
            if info.is_bool_return {
                self.bool_return_macros.insert(name);
            }
        }

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
                    result.rust_decl_dict.as_ref(), Some(&result.inline_fn_dict),
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
            CodegenError { code: String, errors: Vec<String> },
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

            let codegen = RustCodegen::new(self.interner, self.enum_dict, self.macro_ctx, self.bindings_info.clone(), known_symbols, result.rust_decl_dict.as_ref(), Some(&result.inline_fn_dict))
                .with_dump_ast_for(self.config.dump_ast_for.clone())
                .with_bool_return(false, self.bool_return_macros.clone());
            let generated = codegen.generate_inline_fn(**name, func_def);

            if generated.has_unresolved_names() {
                gen_results.push((**name, InlineGenResult::UnresolvedNames {
                    code: generated.code,
                    unresolved: generated.unresolved_names,
                }));
            } else if !generated.codegen_errors.is_empty() {
                gen_results.push((**name, InlineGenResult::CodegenError {
                    code: generated.code,
                    errors: generated.codegen_errors,
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
                InlineGenResult::CodegenError { code, errors } => {
                    let name_str = self.interner.get(name);
                    writeln!(self.writer, "// [CODEGEN_ERROR] {} - inline function", name_str)?;
                    for err in &errors {
                        writeln!(self.writer, "//   {}", err)?;
                    }
                    for line in code.lines() {
                        writeln!(self.writer, "// {}", line)?;
                    }
                    writeln!(self.writer)?;
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

        // ── Phase 2 の解析結果を参照 ──
        // const/mut, bool 戻り値は Phase 2 (resolve_param_and_return_types) で確定済み
        // MacroInferInfo.const_pointer_positions, is_bool_return を参照する
        let mut callee_const_params: HashMap<InternedStr, HashSet<usize>> = HashMap::new();
        let mut bool_return_macros: HashSet<InternedStr> = HashSet::new();
        for (&name, info) in &result.infer_ctx.macros {
            if !info.const_pointer_positions.is_empty() {
                callee_const_params.insert(name, info.const_pointer_positions.clone());
            }
            if info.is_bool_return {
                bool_return_macros.insert(name);
            }
        }
        self.const_pointer_params = callee_const_params;
        self.bool_return_macros = bool_return_macros;

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
                    let const_positions = self.const_pointer_params.get(&name)
                        .cloned().unwrap_or_default();
                    let is_bool = self.bool_return_macros.contains(&name);
                    let codegen = RustCodegen::new(self.interner, self.enum_dict, self.macro_ctx, self.bindings_info.clone(), known_symbols, result.rust_decl_dict.as_ref(), Some(&result.inline_fn_dict))
                        .with_dump_ast_for(self.config.dump_ast_for.clone())
                        .with_const_pointer_positions(const_positions)
                        .with_bool_return(is_bool, self.bool_return_macros.clone());
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
                    } else if !generated.codegen_errors.is_empty() {
                        // codegen エラー検出：コメントアウトして問題点列挙
                        let name_str = self.interner.get(info.name);
                        let thx_info = if info.is_thx_dependent { " [THX]" } else { "" };
                        writeln!(self.writer, "// [CODEGEN_ERROR] {}{} - macro function", name_str, thx_info)?;
                        for err in &generated.codegen_errors {
                            writeln!(self.writer, "//   {}", err)?;
                        }
                        for line in generated.code.lines() {
                            writeln!(self.writer, "// {}", line)?;
                        }
                        writeln!(self.writer)?;
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
                GenerateStatus::GenericUnsupported => {
                    let name_str = self.interner.get(info.name);
                    let thx_info = if info.is_thx_dependent { " [THX]" } else { "" };
                    writeln!(self.writer, "// [GENERIC_UNSUPPORTED] {}{} - Rust cannot cast to generic type T", name_str, thx_info)?;
                    // コメントアウトしたコードを出力
                    let const_positions = self.const_pointer_params.get(&name)
                        .cloned().unwrap_or_default();
                    let is_bool = self.bool_return_macros.contains(&name);
                    let codegen = RustCodegen::new(self.interner, self.enum_dict, self.macro_ctx, self.bindings_info.clone(), known_symbols, result.rust_decl_dict.as_ref(), Some(&result.inline_fn_dict))
                        .with_const_pointer_positions(const_positions)
                        .with_bool_return(is_bool, self.bool_return_macros.clone());
                    let generated = codegen.generate_macro(info);
                    for line in generated.code.lines() {
                        writeln!(self.writer, "// {}", line)?;
                    }
                    writeln!(self.writer)?;
                    self.stats.macros_generic_unsupported += 1;
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

        // ジェネリクス型パラメータを含むマクロは生成不可
        // Rust の as T キャストや T + u32 演算が不可
        if !info.generic_type_params.is_empty() {
            return GenerateStatus::GenericUnsupported;
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
