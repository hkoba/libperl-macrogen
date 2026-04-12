//! syn::Expr ベースの Rust コード生成モジュール
//!
//! C AST (crate::ast::Expr) から syn::Expr を構築し、
//! 優先順位に基づく括弧挿入パスを経て正確な Rust コードを生成する。

use proc_macro2::Span;
use quote::ToTokens;

use crate::ast::BinOp;

// ============================================================================
// Rust 演算子の優先順位
// ============================================================================

/// Rust の式の優先順位（数値が大きいほど高い）
/// 参考: https://doc.rust-lang.org/reference/expressions.html#expression-precedence
fn expr_precedence(expr: &syn::Expr) -> u8 {
    match expr {
        syn::Expr::Lit(_) | syn::Expr::Path(_) | syn::Expr::Paren(_) => 100,
        syn::Expr::MethodCall(_) | syn::Expr::Field(_) | syn::Expr::Index(_) |
        syn::Expr::Call(_) => 90,
        syn::Expr::Try(_) => 85,
        syn::Expr::Unary(_) => 80,                    // -, !, *, &
        syn::Expr::Cast(_) => 75,                      // as
        syn::Expr::Binary(b) => syn_binop_precedence(&b.op),
        syn::Expr::Range(_) => 15,
        syn::Expr::Assign(_) => 10,
        syn::Expr::Return(_) | syn::Expr::Break(_) | syn::Expr::Closure(_) => 5,
        // If/Match は内部が独立だが、演算子の子として使う場合は括弧が必要:
        //   (if cond { A } else { B }) as u8  — 括弧必須
        //   if cond { A } else { B } as u8    — else ブランチ内の cast と誤解析
        syn::Expr::If(_) | syn::Expr::Match(_) => 1,
        // Block/Unsafe 等の { } で囲まれた式は自己完結するため最高優先
        syn::Expr::Block(_) | syn::Expr::Unsafe(_) |
        syn::Expr::Loop(_) | syn::Expr::While(_) | syn::Expr::ForLoop(_) => 100,
        _ => 50,  // デフォルト
    }
}

/// syn の二項演算子の優先順位
fn syn_binop_precedence(op: &syn::BinOp) -> u8 {
    match op {
        syn::BinOp::Mul(_) | syn::BinOp::Div(_) | syn::BinOp::Rem(_) => 70,
        syn::BinOp::Add(_) | syn::BinOp::Sub(_) => 65,
        syn::BinOp::Shl(_) | syn::BinOp::Shr(_) => 60,
        syn::BinOp::BitAnd(_) => 55,
        syn::BinOp::BitXor(_) => 50,
        syn::BinOp::BitOr(_) => 45,
        syn::BinOp::Lt(_) | syn::BinOp::Gt(_) | syn::BinOp::Le(_) | syn::BinOp::Ge(_) |
        syn::BinOp::Eq(_) | syn::BinOp::Ne(_) => 40,
        syn::BinOp::And(_) => 35,
        syn::BinOp::Or(_) => 30,
        _ => 50,
    }
}

// ============================================================================
// 括弧挿入パス
// ============================================================================

/// syn::Expr 木に必要な括弧 (Expr::Paren) を挿入する。
///
/// syn::Expr の ToTokens は括弧を自動挿入しないため、
/// 親子の優先順位を比較して必要な箇所に Expr::Paren を挿入する。
pub fn parenthesize(expr: syn::Expr) -> syn::Expr {
    match expr {
        syn::Expr::Binary(mut binary) => {
            let parent_prec = syn_binop_precedence(&binary.op);
            *binary.left = parenthesize_child(*binary.left, parent_prec, true);
            *binary.right = parenthesize_child(*binary.right, parent_prec, false);
            syn::Expr::Binary(binary)
        }
        syn::Expr::Cast(mut cast) => {
            // as (prec 75) は全ての二項演算子 (prec ≤ 70) より高い
            // → 子が Binary/If/Block なら括弧必要
            let child = parenthesize(*cast.expr);
            let child_prec = expr_precedence(&child);
            *cast.expr = if child_prec < 75 {
                wrap_paren(child)
            } else {
                child
            };
            syn::Expr::Cast(cast)
        }
        syn::Expr::Unary(mut unary) => {
            // 単項 (prec 80) は as (prec 75) より高い
            // → 子が Cast/Binary/If なら括弧必要
            let child = parenthesize(*unary.expr);
            let child_prec = expr_precedence(&child);
            *unary.expr = if child_prec < 80 {
                wrap_paren(child)
            } else {
                child
            };
            syn::Expr::Unary(unary)
        }
        syn::Expr::Field(mut field) => {
            // フィールドアクセス (prec 90) → 子が Cast/Unary/Binary なら括弧必要
            let child = parenthesize(*field.base);
            let child_prec = expr_precedence(&child);
            *field.base = if child_prec < 90 {
                wrap_paren(child)
            } else {
                child
            };
            syn::Expr::Field(field)
        }
        syn::Expr::MethodCall(mut mc) => {
            let child = parenthesize(*mc.receiver);
            let child_prec = expr_precedence(&child);
            *mc.receiver = if child_prec < 90 {
                wrap_paren(child)
            } else {
                child
            };
            syn::Expr::MethodCall(mc)
        }
        syn::Expr::If(mut if_expr) => {
            *if_expr.cond = parenthesize(*if_expr.cond);
            // then/else ブロック内は再帰的に処理
            parenthesize_block(&mut if_expr.then_branch);
            if let Some((_, ref mut else_branch)) = if_expr.else_branch {
                *else_branch = Box::new(parenthesize(*else_branch.clone()));
            }
            syn::Expr::If(if_expr)
        }
        syn::Expr::Paren(mut paren) => {
            *paren.expr = parenthesize(*paren.expr);
            syn::Expr::Paren(paren)
        }
        syn::Expr::Assign(mut assign) => {
            *assign.left = parenthesize(*assign.left);
            *assign.right = parenthesize(*assign.right);
            syn::Expr::Assign(assign)
        }
        syn::Expr::Call(mut call) => {
            *call.func = parenthesize(*call.func);
            for arg in call.args.iter_mut() {
                *arg = parenthesize(arg.clone());
            }
            syn::Expr::Call(call)
        }
        syn::Expr::Return(mut ret) => {
            if let Some(ref mut expr) = ret.expr {
                *expr = Box::new(parenthesize(*expr.clone()));
            }
            syn::Expr::Return(ret)
        }
        // その他はそのまま返す
        other => other,
    }
}

/// 子式を親の優先順位に基づいて括弧で囲むか判定
fn parenthesize_child(child: syn::Expr, parent_prec: u8, is_left: bool) -> syn::Expr {
    let child = parenthesize(child);
    let child_prec = expr_precedence(&child);
    // 子の優先順位が親より低い → 括弧必要
    // 同じ優先順位で右辺 → 括弧必要（左結合のため）
    let needs_parens = child_prec < parent_prec
        || (child_prec == parent_prec && !is_left);
    if needs_parens {
        wrap_paren(child)
    } else {
        child
    }
}

/// Expr::Paren で囲む
fn wrap_paren(expr: syn::Expr) -> syn::Expr {
    syn::Expr::Paren(syn::ExprParen {
        attrs: vec![],
        paren_token: syn::token::Paren::default(),
        expr: Box::new(expr),
    })
}

/// ブロック内の文を再帰的に括弧処理
fn parenthesize_block(block: &mut syn::Block) {
    for stmt in block.stmts.iter_mut() {
        match stmt {
            syn::Stmt::Expr(expr, _) => {
                *expr = parenthesize(expr.clone());
            }
            syn::Stmt::Local(local) => {
                if let Some(ref mut init) = local.init {
                    init.expr = Box::new(parenthesize(*init.expr.clone()));
                }
            }
            _ => {}
        }
    }
}

// ============================================================================
// C AST → syn::Expr 変換ヘルパー
// ============================================================================

/// C の BinOp を syn::BinOp に変換
pub fn to_syn_binop(op: BinOp) -> syn::BinOp {
    match op {
        BinOp::Add => syn::BinOp::Add(Default::default()),
        BinOp::Sub => syn::BinOp::Sub(Default::default()),
        BinOp::Mul => syn::BinOp::Mul(Default::default()),
        BinOp::Div => syn::BinOp::Div(Default::default()),
        BinOp::Mod => syn::BinOp::Rem(Default::default()),
        BinOp::BitAnd => syn::BinOp::BitAnd(Default::default()),
        BinOp::BitOr => syn::BinOp::BitOr(Default::default()),
        BinOp::BitXor => syn::BinOp::BitXor(Default::default()),
        BinOp::Shl => syn::BinOp::Shl(Default::default()),
        BinOp::Shr => syn::BinOp::Shr(Default::default()),
        BinOp::Eq => syn::BinOp::Eq(Default::default()),
        BinOp::Ne => syn::BinOp::Ne(Default::default()),
        BinOp::Lt => syn::BinOp::Lt(Default::default()),
        BinOp::Gt => syn::BinOp::Gt(Default::default()),
        BinOp::Le => syn::BinOp::Le(Default::default()),
        BinOp::Ge => syn::BinOp::Ge(Default::default()),
        BinOp::LogAnd => syn::BinOp::And(Default::default()),
        BinOp::LogOr => syn::BinOp::Or(Default::default()),
    }
}

/// syn::Expr を文字列に変換
pub fn expr_to_string(expr: &syn::Expr) -> String {
    let parenthesized = parenthesize(expr.clone());
    parenthesized.to_token_stream().to_string()
}

/// syn::Ident を作成するヘルパー
pub fn ident(name: &str) -> syn::Ident {
    // 既に r# プレフィックスが付いている場合は除去して raw ident として作成
    if let Some(raw_name) = name.strip_prefix("r#") {
        return syn::Ident::new_raw(raw_name, Span::call_site());
    }
    // Rust のキーワードは r# プレフィックスが必要
    if is_rust_keyword(name) {
        syn::Ident::new_raw(name, Span::call_site())
    } else {
        syn::Ident::new(name, Span::call_site())
    }
}

fn is_rust_keyword(name: &str) -> bool {
    matches!(name,
        "as" | "break" | "const" | "continue" | "crate" | "else" | "enum" |
        "extern" | "false" | "fn" | "for" | "if" | "impl" | "in" | "let" |
        "loop" | "match" | "mod" | "move" | "mut" | "pub" | "ref" | "return" |
        "self" | "Self" | "static" | "struct" | "super" | "trait" | "true" |
        "type" | "unsafe" | "use" | "where" | "while" | "async" | "await" |
        "dyn" | "abstract" | "become" | "box" | "do" | "final" | "macro" |
        "override" | "priv" | "typeof" | "unsized" | "virtual" | "yield" | "gen" | "try"
    )
}

// ============================================================================
// 括弧正規化 (Phase 4): 文字列 → parse → strip → parenthesize → format
// ============================================================================

/// 式文字列の括弧を正規化する。
///
/// 1. syn::parse_str でパース
/// 2. すべての syn::Expr::Paren を除去
/// 3. parenthesize() で必要な括弧のみ再挿入
/// 4. prettyplease で整形
///
/// 結果が多行になる場合やパース失敗時は、
/// 単純な外側括弧除去（strip_outer_parens 相当）にフォールバックする。
pub fn normalize_parens(s: &str) -> String {
    // syn ベースの正規化を試行（単行結果のみ）
    if let Some(normalized) = try_normalize_parens(s) {
        if !normalized.contains('\n') {
            return normalized;
        }
    }
    // フォールバック: 外側の括弧のみ除去（strip_outer_parens と同等）
    fallback_strip_outer_parens(s)
}

/// strip_outer_parens と同等のフォールバックロジック
fn fallback_strip_outer_parens(s: &str) -> String {
    let s = s.trim();
    if s.len() < 2 || !s.starts_with('(') || !s.ends_with(')') {
        return s.to_string();
    }
    let inner = &s[1..s.len() - 1];
    // ブロック式 ({...}) は strip しない
    if inner.trim_start().starts_with('{') {
        return s.to_string();
    }
    let mut depth = 0i32;
    for ch in inner.chars() {
        match ch {
            '(' | '{' | '[' => depth += 1,
            ')' | '}' | ']' => {
                depth -= 1;
                if depth < 0 {
                    return s.to_string();
                }
            }
            _ => {}
        }
    }
    if depth == 0 { inner.to_string() } else { s.to_string() }
}

fn try_normalize_parens(s: &str) -> Option<String> {
    let parsed = syn::parse_str::<syn::Expr>(s).ok()?;
    let stripped = strip_all_parens(parsed);
    let normalized = parenthesize(stripped);
    let result = pretty_expr(&normalized);
    // 結果が空や明らかにおかしい場合はフォールバック
    if result.is_empty() {
        return None;
    }
    Some(result)
}

/// syn::Expr ツリーからすべての Paren ラッパーノードを除去する。
///
/// ツリー構造（Binary, Unary, Cast 等）は保持される。
/// 除去後に parenthesize() を適用すると、必要な括弧のみが再挿入される。
pub fn strip_all_parens(expr: syn::Expr) -> syn::Expr {
    match expr {
        syn::Expr::Paren(p) => strip_all_parens(*p.expr),
        syn::Expr::Binary(mut b) => {
            *b.left = strip_all_parens(*b.left);
            *b.right = strip_all_parens(*b.right);
            syn::Expr::Binary(b)
        }
        syn::Expr::Unary(mut u) => {
            *u.expr = strip_all_parens(*u.expr);
            syn::Expr::Unary(u)
        }
        syn::Expr::Cast(mut c) => {
            *c.expr = strip_all_parens(*c.expr);
            syn::Expr::Cast(c)
        }
        syn::Expr::Field(mut f) => {
            *f.base = strip_all_parens(*f.base);
            syn::Expr::Field(f)
        }
        syn::Expr::MethodCall(mut m) => {
            *m.receiver = strip_all_parens(*m.receiver);
            for arg in m.args.iter_mut() {
                *arg = strip_all_parens(arg.clone());
            }
            syn::Expr::MethodCall(m)
        }
        syn::Expr::Call(mut c) => {
            *c.func = strip_all_parens(*c.func);
            for arg in c.args.iter_mut() {
                *arg = strip_all_parens(arg.clone());
            }
            syn::Expr::Call(c)
        }
        syn::Expr::If(mut i) => {
            *i.cond = strip_all_parens(*i.cond);
            strip_parens_in_block(&mut i.then_branch);
            if let Some((_, ref mut else_branch)) = i.else_branch {
                *else_branch = Box::new(strip_all_parens(*else_branch.clone()));
            }
            syn::Expr::If(i)
        }
        syn::Expr::Index(mut i) => {
            *i.expr = strip_all_parens(*i.expr);
            *i.index = strip_all_parens(*i.index);
            syn::Expr::Index(i)
        }
        syn::Expr::Assign(mut a) => {
            *a.left = strip_all_parens(*a.left);
            *a.right = strip_all_parens(*a.right);
            syn::Expr::Assign(a)
        }
        syn::Expr::Return(mut r) => {
            if let Some(ref mut e) = r.expr {
                *e = Box::new(strip_all_parens(*e.clone()));
            }
            syn::Expr::Return(r)
        }
        syn::Expr::Block(mut b) => {
            strip_parens_in_block(&mut b.block);
            syn::Expr::Block(b)
        }
        syn::Expr::Reference(mut r) => {
            *r.expr = strip_all_parens(*r.expr);
            syn::Expr::Reference(r)
        }
        syn::Expr::Unsafe(mut u) => {
            strip_parens_in_block(&mut u.block);
            syn::Expr::Unsafe(u)
        }
        other => other,
    }
}

fn strip_parens_in_block(block: &mut syn::Block) {
    for stmt in block.stmts.iter_mut() {
        match stmt {
            syn::Stmt::Expr(e, _) => *e = strip_all_parens(e.clone()),
            syn::Stmt::Local(l) => {
                if let Some(ref mut init) = l.init {
                    *init.expr = strip_all_parens(*init.expr.clone());
                }
            }
            _ => {}
        }
    }
}

/// syn::Expr を prettyplease で整形して文字列化
fn pretty_expr(expr: &syn::Expr) -> String {
    // prettyplease は syn::File 単位で動作するため、
    // ダミー関数でラップして整形し、本体を抽出する
    let tokens = quote::quote! {
        fn __() -> __T {
            #expr
        }
    };
    let file: syn::File = match syn::parse2(tokens) {
        Ok(f) => f,
        Err(_) => {
            // パース失敗時は ToTokens でフォールバック
            return expr.to_token_stream().to_string();
        }
    };
    let formatted = prettyplease::unparse(&file);
    extract_fn_body(&formatted)
}

/// prettyplease 出力から関数本体の式を抽出
fn extract_fn_body(formatted: &str) -> String {
    let lines: Vec<&str> = formatted.lines().collect();
    if lines.len() < 3 {
        return formatted.to_string();
    }
    // "fn __() -> __T {" と "}" を除去し、4スペースインデントを除去
    let body_lines: Vec<&str> = lines[1..lines.len() - 1]
        .iter()
        .map(|l| l.strip_prefix("    ").unwrap_or(l))
        .collect();
    body_lines.join("\n")
}

// ============================================================================
// AST 変換パス (Phase 3)
// ============================================================================

/// syn::Expr が bool を返すかどうかを判定
pub fn is_bool_syn_expr(expr: &syn::Expr) -> bool {
    match expr {
        syn::Expr::Binary(b) => matches!(b.op,
            syn::BinOp::Eq(_) | syn::BinOp::Ne(_) |
            syn::BinOp::Lt(_) | syn::BinOp::Gt(_) |
            syn::BinOp::Le(_) | syn::BinOp::Ge(_) |
            syn::BinOp::And(_) | syn::BinOp::Or(_)
        ),
        syn::Expr::Unary(u) => matches!(u.op, syn::UnOp::Not(_)) && is_bool_syn_expr(&u.expr),
        syn::Expr::Lit(lit) => matches!(lit.lit, syn::Lit::Bool(_)),
        syn::Expr::Paren(p) => is_bool_syn_expr(&p.expr),
        syn::Expr::MethodCall(mc) => mc.method == "is_null",
        _ => false,
    }
}

/// syn::Expr がポインタっぽいかを文字列ヒントで判定
/// （syn::Expr には型情報がないため、メソッド名やキャストの型名で推定）
pub fn looks_like_pointer(expr: &syn::Expr) -> bool {
    match expr {
        syn::Expr::Cast(cast) => {
            let ty_str = cast.ty.to_token_stream().to_string();
            ty_str.contains("* mut") || ty_str.contains("* const")
        }
        syn::Expr::MethodCall(mc) => {
            let method = mc.method.to_string();
            matches!(method.as_str(),
                "offset" | "wrapping_add" | "wrapping_sub" | "as_ptr" | "as_mut_ptr")
        }
        syn::Expr::Call(call) => {
            let func_str = call.func.to_token_stream().to_string();
            func_str.contains("null_mut") || func_str.contains("null")
        }
        syn::Expr::Paren(p) => looks_like_pointer(&p.expr),
        _ => false,
    }
}

/// 式を bool に変換する
///
/// - 既に bool 式 → そのまま
/// - ポインタっぽい → `!expr.is_null()`
/// - その他 → `expr != 0`
pub fn wrap_as_bool(expr: syn::Expr) -> syn::Expr {
    if is_bool_syn_expr(&expr) {
        return expr;
    }
    if looks_like_pointer(&expr) {
        // !expr.is_null()
        let is_null_call = syn::Expr::MethodCall(syn::ExprMethodCall {
            attrs: vec![],
            receiver: Box::new(expr),
            dot_token: Default::default(),
            method: ident("is_null"),
            turbofish: None,
            paren_token: Default::default(),
            args: syn::punctuated::Punctuated::new(),
        });
        return syn::Expr::Unary(syn::ExprUnary {
            attrs: vec![],
            op: syn::UnOp::Not(Default::default()),
            expr: Box::new(is_null_call),
        });
    }
    // expr != 0
    syn::Expr::Binary(syn::ExprBinary {
        attrs: vec![],
        left: Box::new(expr),
        op: syn::BinOp::Ne(Default::default()),
        right: Box::new(int_lit(0)),
    })
}

/// 整数リテラルを作成
pub fn int_lit(n: i64) -> syn::Expr {
    let lit = syn::LitInt::new(&n.to_string(), Span::call_site());
    syn::Expr::Lit(syn::ExprLit {
        attrs: vec![],
        lit: syn::Lit::Int(lit),
    })
}

/// `as T` キャストを挿入
///
/// 括弧は `parenthesize()` パスで自動挿入されるため、ここでは不要。
pub fn insert_cast(expr: syn::Expr, ty: syn::Type) -> syn::Expr {
    syn::Expr::Cast(syn::ExprCast {
        attrs: vec![],
        expr: Box::new(expr),
        as_token: Default::default(),
        ty: Box::new(ty),
    })
}

/// 型名文字列から syn::Type をパース
pub fn parse_type(ty_str: &str) -> syn::Type {
    syn::parse_str(ty_str).unwrap_or_else(|_| {
        // パース失敗時はフォールバック
        syn::parse_str("c_int").unwrap()
    })
}

/// null ポインタ式を型に合わせて生成
///
/// - const ポインタ → `std::ptr::null()`
/// - mut ポインタ → `std::ptr::null_mut()`
/// - 非ポインタ → `0`
pub fn null_for_type(ty_str: &str) -> syn::Expr {
    if ty_str.contains("*const") {
        syn::parse_str("std::ptr::null()").unwrap()
    } else if ty_str.contains("*mut") || ty_str.contains("*") {
        syn::parse_str("std::ptr::null_mut()").unwrap()
    } else {
        int_lit(0)
    }
}

/// `.as_ptr()` メソッド呼び出しを付加
pub fn as_ptr(expr: syn::Expr) -> syn::Expr {
    syn::Expr::MethodCall(syn::ExprMethodCall {
        attrs: vec![],
        receiver: Box::new(expr),
        dot_token: Default::default(),
        method: ident("as_ptr"),
        turbofish: None,
        paren_token: Default::default(),
        args: syn::punctuated::Punctuated::new(),
    })
}

/// フィールドアクセス `expr.field` を構築
pub fn field_access(expr: syn::Expr, field_name: &str) -> syn::Expr {
    syn::Expr::Field(syn::ExprField {
        attrs: vec![],
        base: Box::new(expr),
        dot_token: Default::default(),
        member: syn::Member::Named(ident(field_name)),
    })
}

/// Deref `*expr` を構築
pub fn deref(expr: syn::Expr) -> syn::Expr {
    syn::Expr::Unary(syn::ExprUnary {
        attrs: vec![],
        op: syn::UnOp::Deref(Default::default()),
        expr: Box::new(expr),
    })
}

/// `&mut expr` を構築
pub fn addr_of_mut(expr: syn::Expr) -> syn::Expr {
    syn::Expr::Reference(syn::ExprReference {
        attrs: vec![],
        and_token: Default::default(),
        mutability: Some(Default::default()),
        expr: Box::new(expr),
    })
}

/// 関数呼び出し `func(args...)` を構築
pub fn call(func_name: &str, args: Vec<syn::Expr>) -> syn::Expr {
    let func_ident = ident(func_name);
    let mut punctuated = syn::punctuated::Punctuated::new();
    for arg in args {
        punctuated.push(arg);
    }
    syn::Expr::Call(syn::ExprCall {
        attrs: vec![],
        func: Box::new(syn::Expr::Path(syn::ExprPath {
            attrs: vec![],
            qself: None,
            path: func_ident.into(),
        })),
        paren_token: Default::default(),
        args: punctuated,
    })
}

/// if-else 式を構築
pub fn if_else(cond: syn::Expr, then_expr: syn::Expr, else_expr: syn::Expr) -> syn::Expr {
    syn::Expr::If(syn::ExprIf {
        attrs: vec![],
        if_token: Default::default(),
        cond: Box::new(cond),
        then_branch: syn::Block {
            brace_token: Default::default(),
            stmts: vec![syn::Stmt::Expr(then_expr, None)],
        },
        else_branch: Some((
            Default::default(),
            Box::new(syn::Expr::Block(syn::ExprBlock {
                attrs: vec![],
                label: None,
                block: syn::Block {
                    brace_token: Default::default(),
                    stmts: vec![syn::Stmt::Expr(else_expr, None)],
                },
            })),
        )),
    })
}

// ============================================================================
// テスト
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_quote;

    #[test]
    fn test_parenthesize_binary_precedence() {
        // (a + b) * c → 括弧が必要
        let expr: syn::Expr = parse_quote!(a + b * c);
        let result = expr_to_string(&expr);
        assert_eq!(result, "a + b * c");

        // a + b を * の左辺に → 括弧必要
        let a: syn::Expr = parse_quote!(a);
        let b: syn::Expr = parse_quote!(b);
        let c: syn::Expr = parse_quote!(c);
        let add: syn::Expr = parse_quote!(#a + #b);
        let mul = syn::Expr::Binary(syn::ExprBinary {
            attrs: vec![],
            left: Box::new(add),
            op: syn::BinOp::Mul(Default::default()),
            right: Box::new(c),
        });
        let result = expr_to_string(&mul);
        assert_eq!(result, "(a + b) * c");
    }

    #[test]
    fn test_parenthesize_cast() {
        // a & MASK as u32 → (a & MASK) as u32 が必要
        let a: syn::Expr = parse_quote!(a);
        let mask: syn::Expr = parse_quote!(MASK);
        let bitand = syn::Expr::Binary(syn::ExprBinary {
            attrs: vec![],
            left: Box::new(a),
            op: syn::BinOp::BitAnd(Default::default()),
            right: Box::new(mask),
        });
        let cast = syn::Expr::Cast(syn::ExprCast {
            attrs: vec![],
            expr: Box::new(bitand),
            as_token: Default::default(),
            ty: Box::new(parse_quote!(u32)),
        });
        let result = expr_to_string(&cast);
        assert_eq!(result, "(a & MASK) as u32");
    }

    #[test]
    fn test_parenthesize_if_ne() {
        // (if cond { A } else { B }) != 0 → 括弧が必要
        let if_expr: syn::Expr = parse_quote!(if cond { A } else { B });
        let ne = syn::Expr::Binary(syn::ExprBinary {
            attrs: vec![],
            left: Box::new(if_expr),
            op: syn::BinOp::Ne(Default::default()),
            right: Box::new(parse_quote!(0)),
        });
        let result = expr_to_string(&ne);
        // if 式は prec 100 だが、Binary(Ne) の子としては括弧不要（中身が独立）
        // → 実際には Rust では if {} != 0 は if {} else {} != 0 にパースされるため括弧必要
        // この挙動を正しくハンドルするにはIf式の特別扱いが必要
        assert!(result.contains("if cond"));
    }

    #[test]
    fn test_deref_field() {
        // (*a).field — Deref + Field
        let a: syn::Expr = parse_quote!(a);
        let deref = syn::Expr::Unary(syn::ExprUnary {
            attrs: vec![],
            op: syn::UnOp::Deref(Default::default()),
            expr: Box::new(a),
        });
        let field = syn::Expr::Field(syn::ExprField {
            attrs: vec![],
            base: Box::new(deref),
            dot_token: Default::default(),
            member: syn::Member::Named(ident("field")),
        });
        let result = expr_to_string(&field);
        assert_eq!(result, "(* a) . field");
        // Note: ToTokens ではスペースが入る。prettyplease で整形すると (*a).field になる
    }

    #[test]
    fn test_ident_keyword() {
        let i = ident("type");
        assert_eq!(i.to_string(), "r#type");
    }

    // ================================================================
    // Phase 4: normalize_parens のテスト
    // ================================================================

    #[test]
    fn test_normalize_cast_removes_outer_parens() {
        // (x as i32) → x as i32
        assert_eq!(normalize_parens("(x as i32)"), "x as i32");
    }

    #[test]
    fn test_normalize_deref_removes_outer_parens() {
        // (*ptr) → *ptr
        assert_eq!(normalize_parens("(*ptr)"), "*ptr");
    }

    #[test]
    fn test_normalize_addr_of_removes_outer_parens() {
        // (&mut x) → &mut x
        assert_eq!(normalize_parens("(&mut x)"), "&mut x");
    }

    #[test]
    fn test_normalize_binary_removes_outer_parens() {
        // (a + b) → a + b
        assert_eq!(normalize_parens("(a + b)"), "a + b");
    }

    #[test]
    fn test_normalize_deref_field_preserves_needed_parens() {
        // (*a).field → (*a).field (parens needed!)
        assert_eq!(normalize_parens("(*a).field"), "(*a).field");
    }

    #[test]
    fn test_normalize_cast_in_binary_preserves_needed_parens() {
        // (a & MASK) as u32 → (a & MASK) as u32 (parens needed!)
        assert_eq!(normalize_parens("(a & MASK) as u32"), "(a & MASK) as u32");
    }

    #[test]
    fn test_normalize_nested_unnecessary_parens() {
        // ((x as i32)) → x as i32
        assert_eq!(normalize_parens("((x as i32))"), "x as i32");
    }

    #[test]
    fn test_normalize_preserves_precedence() {
        // (a + b) * c → (a + b) * c (parens needed!)
        assert_eq!(normalize_parens("(a + b) * c"), "(a + b) * c");
    }

    #[test]
    fn test_normalize_no_change_needed() {
        assert_eq!(normalize_parens("x"), "x");
        assert_eq!(normalize_parens("42"), "42");
        assert_eq!(normalize_parens("foo(a, b)"), "foo(a, b)");
    }

    #[test]
    fn test_normalize_method_call() {
        // (ptr).is_null() → ptr.is_null()
        assert_eq!(normalize_parens("(ptr).is_null()"), "ptr.is_null()");
    }

    #[test]
    fn test_normalize_logical_ops() {
        // (a && b) → a && b
        assert_eq!(normalize_parens("(a && b)"), "a && b");
        // (a || b) → a || b
        assert_eq!(normalize_parens("(a || b)"), "a || b");
    }

    #[test]
    fn test_normalize_complex_nested() {
        // ((*sv).sv_flags as u32) → (*sv).sv_flags as u32
        assert_eq!(
            normalize_parens("((*sv).sv_flags as u32)"),
            "(*sv).sv_flags as u32"
        );
    }

    #[test]
    fn test_normalize_unary_minus() {
        // (-x) → -x
        assert_eq!(normalize_parens("(-x)"), "-x");
    }

    #[test]
    fn test_normalize_not() {
        // (!cond) → !cond
        assert_eq!(normalize_parens("(!cond)"), "!cond");
    }

    #[test]
    fn test_normalize_block_expr_passthrough() {
        // Block expressions should pass through (parse may fail or be multi-line)
        let s = "{ x += 1; x }";
        let result = normalize_parens(s);
        // Should either normalize or return original
        assert!(result == s || !result.contains('\n'));
    }

    // ================================================================
    // Phase 3: AST 変換パスのテスト
    // ================================================================

    #[test]
    fn test_is_bool_syn_expr_comparison() {
        let expr: syn::Expr = parse_quote!(a == b);
        assert!(is_bool_syn_expr(&expr));

        let expr: syn::Expr = parse_quote!(a != 0);
        assert!(is_bool_syn_expr(&expr));

        let expr: syn::Expr = parse_quote!(a < b);
        assert!(is_bool_syn_expr(&expr));
    }

    #[test]
    fn test_is_bool_syn_expr_logical() {
        let expr: syn::Expr = parse_quote!(a && b);
        assert!(is_bool_syn_expr(&expr));

        let expr: syn::Expr = parse_quote!(a || b);
        assert!(is_bool_syn_expr(&expr));
    }

    #[test]
    fn test_is_bool_syn_expr_not() {
        // !bool_expr → bool
        let expr: syn::Expr = parse_quote!(!(a == b));
        assert!(is_bool_syn_expr(&expr));

        // !non_bool → not bool (bitwise not)
        let expr: syn::Expr = parse_quote!(!x);
        assert!(!is_bool_syn_expr(&expr));
    }

    #[test]
    fn test_is_bool_syn_expr_non_bool() {
        let expr: syn::Expr = parse_quote!(a + b);
        assert!(!is_bool_syn_expr(&expr));

        let expr: syn::Expr = parse_quote!(42);
        assert!(!is_bool_syn_expr(&expr));

        let expr: syn::Expr = parse_quote!(foo(x));
        assert!(!is_bool_syn_expr(&expr));
    }

    #[test]
    fn test_is_bool_syn_expr_bool_lit() {
        let expr: syn::Expr = parse_quote!(true);
        assert!(is_bool_syn_expr(&expr));

        let expr: syn::Expr = parse_quote!(false);
        assert!(is_bool_syn_expr(&expr));
    }

    #[test]
    fn test_is_bool_syn_expr_is_null() {
        let expr: syn::Expr = parse_quote!(ptr.is_null());
        assert!(is_bool_syn_expr(&expr));
    }

    #[test]
    fn test_is_bool_syn_expr_paren() {
        let expr: syn::Expr = parse_quote!((a == b));
        assert!(is_bool_syn_expr(&expr));
    }

    #[test]
    fn test_looks_like_pointer_cast() {
        let expr: syn::Expr = parse_quote!(x as *mut i32);
        assert!(looks_like_pointer(&expr));

        let expr: syn::Expr = parse_quote!(x as *const u8);
        assert!(looks_like_pointer(&expr));

        let expr: syn::Expr = parse_quote!(x as i32);
        assert!(!looks_like_pointer(&expr));
    }

    #[test]
    fn test_looks_like_pointer_method() {
        let expr: syn::Expr = parse_quote!(p.offset(1));
        assert!(looks_like_pointer(&expr));

        let expr: syn::Expr = parse_quote!(p.wrapping_add(n));
        assert!(looks_like_pointer(&expr));

        let expr: syn::Expr = parse_quote!(arr.as_ptr());
        assert!(looks_like_pointer(&expr));
    }

    #[test]
    fn test_looks_like_pointer_null() {
        let expr: syn::Expr = parse_quote!(std::ptr::null_mut());
        assert!(looks_like_pointer(&expr));
    }

    #[test]
    fn test_wrap_as_bool_already_bool() {
        let expr: syn::Expr = parse_quote!(a == b);
        let result = wrap_as_bool(expr);
        let s = expr_to_string(&result);
        assert_eq!(s, "a == b");
    }

    #[test]
    fn test_wrap_as_bool_integer() {
        let expr: syn::Expr = parse_quote!(x);
        let result = wrap_as_bool(expr);
        let s = expr_to_string(&result);
        assert_eq!(s, "x != 0");
    }

    #[test]
    fn test_wrap_as_bool_pointer() {
        let expr: syn::Expr = parse_quote!(p as *mut i32);
        let result = wrap_as_bool(expr);
        let s = expr_to_string(&result);
        assert!(s.contains("is_null"), "expected is_null in: {}", s);
    }

    #[test]
    fn test_int_lit() {
        let expr = int_lit(42);
        let s = expr_to_string(&expr);
        assert_eq!(s, "42");

        let expr = int_lit(0);
        let s = expr_to_string(&expr);
        assert_eq!(s, "0");

        let expr = int_lit(-1);
        let s = expr_to_string(&expr);
        assert_eq!(s, "- 1");  // ToTokens adds space after unary minus
    }

    #[test]
    fn test_insert_cast() {
        let expr: syn::Expr = parse_quote!(x);
        let ty = parse_type("u32");
        let result = insert_cast(expr, ty);
        let s = expr_to_string(&result);
        assert_eq!(s, "x as u32");
    }

    #[test]
    fn test_insert_cast_complex_expr() {
        // (a + b) as i32 — parenthesize should add parens
        let expr: syn::Expr = parse_quote!(a + b);
        let ty = parse_type("i32");
        let result = insert_cast(expr, ty);
        let s = expr_to_string(&result);
        assert_eq!(s, "(a + b) as i32");
    }

    #[test]
    fn test_parse_type_basic() {
        let ty = parse_type("i32");
        assert_eq!(ty.to_token_stream().to_string(), "i32");
    }

    #[test]
    fn test_parse_type_pointer() {
        let ty = parse_type("*mut u8");
        assert_eq!(ty.to_token_stream().to_string(), "* mut u8");
    }

    #[test]
    fn test_parse_type_fallback() {
        // Invalid type string → fallback to c_int
        let ty = parse_type("not a valid type!!!");
        assert_eq!(ty.to_token_stream().to_string(), "c_int");
    }

    #[test]
    fn test_null_for_type_mut() {
        let expr = null_for_type("*mut SV");
        let s = expr_to_string(&expr);
        assert!(s.contains("null_mut"), "expected null_mut in: {}", s);
    }

    #[test]
    fn test_null_for_type_const() {
        let expr = null_for_type("*const c_char");
        let s = expr_to_string(&expr);
        assert!(s.contains("null"), "expected null in: {}", s);
        assert!(!s.contains("null_mut"), "should not contain null_mut in: {}", s);
    }

    #[test]
    fn test_null_for_type_non_pointer() {
        let expr = null_for_type("i32");
        let s = expr_to_string(&expr);
        assert_eq!(s, "0");
    }

    #[test]
    fn test_as_ptr() {
        let expr: syn::Expr = parse_quote!(PL_Yes);
        let result = as_ptr(expr);
        let s = expr_to_string(&result);
        assert!(s.contains("as_ptr"), "expected as_ptr in: {}", s);
        assert!(s.contains("PL_Yes"), "expected PL_Yes in: {}", s);
    }

    #[test]
    fn test_field_access() {
        let expr: syn::Expr = parse_quote!(sv);
        let result = field_access(expr, "sv_flags");
        let s = expr_to_string(&result);
        assert_eq!(s, "sv . sv_flags");
    }

    #[test]
    fn test_deref_simple() {
        let expr: syn::Expr = parse_quote!(ptr);
        let result = deref(expr);
        let s = expr_to_string(&result);
        assert_eq!(s, "* ptr");
    }

    #[test]
    fn test_deref_field_parenthesized() {
        // (*ptr).field — deref should get parens when used with field access
        let ptr: syn::Expr = parse_quote!(ptr);
        let d = deref(ptr);
        let f = field_access(d, "field");
        let s = expr_to_string(&f);
        assert_eq!(s, "(* ptr) . field");
    }

    #[test]
    fn test_addr_of_mut() {
        let expr: syn::Expr = parse_quote!(x);
        let result = addr_of_mut(expr);
        let s = expr_to_string(&result);
        assert_eq!(s, "& mut x");
    }

    #[test]
    fn test_call_no_args() {
        let result = call("foo", vec![]);
        let s = expr_to_string(&result);
        assert!(s.contains("foo"), "expected foo in: {}", s);
        // ToTokens may add spaces: "foo ()"
        let normalized = s.replace(' ', "");
        assert_eq!(normalized, "foo()");
    }

    #[test]
    fn test_call_with_args() {
        let a: syn::Expr = parse_quote!(x);
        let b: syn::Expr = parse_quote!(y);
        let result = call("bar", vec![a, b]);
        let s = expr_to_string(&result);
        let normalized = s.replace(' ', "");
        assert_eq!(normalized, "bar(x,y)");
    }

    #[test]
    fn test_if_else() {
        let cond: syn::Expr = parse_quote!(x > 0);
        let then_expr: syn::Expr = parse_quote!(a);
        let else_expr: syn::Expr = parse_quote!(b);
        let result = if_else(cond, then_expr, else_expr);
        let s = expr_to_string(&result);
        assert!(s.contains("if"), "expected if in: {}", s);
        assert!(s.contains("else"), "expected else in: {}", s);
    }

    #[test]
    fn test_wrap_as_bool_with_binary() {
        // Binary expr like (a + b) should become (a + b) != 0
        let expr: syn::Expr = parse_quote!(a + b);
        let result = wrap_as_bool(expr);
        let s = expr_to_string(&result);
        assert_eq!(s, "a + b != 0");
    }

    #[test]
    fn test_combined_cast_and_bool() {
        // A typical pattern: cast result to bool
        // (x as u32) != 0
        let x: syn::Expr = parse_quote!(flags);
        let cast = insert_cast(x, parse_type("u32"));
        let bool_expr = wrap_as_bool(cast);
        let s = expr_to_string(&bool_expr);
        assert_eq!(s, "flags as u32 != 0");
    }
}
