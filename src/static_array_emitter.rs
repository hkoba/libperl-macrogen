//! `GlobalConstDict` の静的 const 配列を Rust の `static` 配列定義として
//! 出力する。
//!
//! 典型例: `sv_inline.h` の `bodies_by_type[]` を `Perl_newSV_type` 等で
//! 参照可能にするため、Rust 側で同じ初期値の `static` を生成する。
//! 元の C コードでは `static const` (翻訳単位ローカル) なので、別 TU 相当
//! として独自にデータを持つことに意味論的な問題はない。
//!
//! ### 翻訳ルール
//!
//! 各 initializer エントリ（無名 compound `{ a, b, c, ... }`）を struct
//! literal に変換する。位置順なので、対応する `StructDef.members` の名前
//! を順に取り出して `body_details { body_size: a, copy: b, ... }` の形式に
//! 整形する。
//!
//! Bitfield 連続グループは値を pack して `_bitfield_N: ((v0 as u8) & mask0)
//! | ((v1 as u8) << shift1) | ...` の形で 1 つのフィールドにまとめる。
//!
//! 各値の式翻訳は `translate_const_expr` で行う。マクロは preprocessor
//! で展開済みのため、`+`, `-`, `*`, `?:`, `cast`, `sizeof`, `__builtin_offsetof`
//! など純粋な C 式のみを扱えば良い。

use crate::ast::{BuiltinArg, Expr, ExprKind, Initializer};
use crate::fields_dict::{FieldsDict, StructDef};
use crate::global_const_dict::{GlobalConstDecl, GlobalConstDict};
use crate::intern::{InternedStr, StringInterner};
use crate::rust_decl::RustDeclDict;
use crate::type_repr::TypeRepr;

/// 出力結果と、正常に emit できた static 配列名の集合。
/// 名前集合は下流の codegen で `.as_ptr()` 減衰判定 (is_array_like_expr)
/// などに使う。
pub struct EmittedStaticArrays {
    pub source: String,
    pub emitted_names: std::collections::HashSet<String>,
    /// 名前 → Rust 型文字列 (`"[ELEMENT; N]"` 形式)。
    /// `BindingsInfo::static_types` にマージして要素型抽出に使う。
    pub emitted_types: std::collections::HashMap<String, String>,
}

/// `GlobalConstDict` の全エントリを Rust ソースとして出力する。
/// 要素型の StructDef が見つからない、bindings.rs に既存等の場合はスキップ。
pub fn emit_static_arrays(
    global_const_dict: &GlobalConstDict,
    fields_dict: &FieldsDict,
    rust_decl_dict: Option<&RustDeclDict>,
    interner: &StringInterner,
) -> EmittedStaticArrays {
    let mut out = String::new();
    let mut emitted_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut emitted_types: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let bindings_consts: std::collections::HashSet<String> = rust_decl_dict
        .map(|d| d.consts.keys().cloned().collect())
        .unwrap_or_default();

    let mut entries: Vec<(InternedStr, &GlobalConstDecl)> = global_const_dict
        .iter()
        .map(|(n, d)| (*n, d))
        .collect();
    entries.sort_by(|a, b| interner.get(a.0).cmp(interner.get(b.0)));

    let mut header_emitted = false;
    for (name, decl) in entries {
        let name_str = interner.get(name);
        // bindings.rs に同名 const があればスキップ
        if bindings_consts.contains(name_str) {
            continue;
        }
        // 要素型から struct 名を解決
        let struct_name = match decl.element_type.type_name() {
            Some(n) => n,
            None => continue,
        };
        let struct_def = match fields_dict.get_struct_def(struct_name) {
            Some(d) => d,
            None => continue,  // 構造体定義が無ければ翻訳不可
        };
        let result = emit_one_array(name_str, decl, struct_def, interner);
        match result {
            Ok(s) => {
                if !header_emitted {
                    out.push_str("// === Auto-generated static const arrays ===\n");
                    out.push_str("// `static const X NAME[] = {...}` declarations from C headers,\n");
                    out.push_str("// translated to Rust `static` so that referencing inline functions/macros\n");
                    out.push_str("// can resolve them. Each entry's expression is translated using\n");
                    out.push_str("// core::mem::{size_of,offset_of,size_of_val_raw}.\n\n");
                    header_emitted = true;
                }
                out.push_str(&s);
                out.push('\n');
                emitted_names.insert(name_str.to_string());
                let elem_str = decl.element_type.to_rust_string(interner);
                let count = match &decl.initializer {
                    Initializer::List(items) => items.len(),
                    _ => 0,
                };
                emitted_types.insert(
                    name_str.to_string(),
                    format!("[{}; {}]", elem_str, count),
                );
            }
            Err(reason) => {
                if !header_emitted {
                    out.push_str("// === Auto-generated static const arrays ===\n\n");
                    header_emitted = true;
                }
                out.push_str(&format!(
                    "// [SKIPPED] static {} — {}\n\n", name_str, reason
                ));
            }
        }
    }
    EmittedStaticArrays { source: out, emitted_names, emitted_types }
}

fn emit_one_array(
    name: &str,
    decl: &GlobalConstDecl,
    struct_def: &StructDef,
    interner: &StringInterner,
) -> Result<String, String> {
    let items = match &decl.initializer {
        Initializer::List(items) => items,
        _ => return Err("initializer is not a list".into()),
    };
    let elem_type_str = decl.element_type.to_rust_string(interner);
    let count = items.len();

    // 各エントリの内側 Initializer も List のはず
    let mut entries_rust: Vec<String> = Vec::new();
    for (idx, item) in items.iter().enumerate() {
        match &item.init {
            Initializer::List(inner) => {
                let entry_str = build_struct_literal(
                    &elem_type_str, struct_def, inner, interner,
                ).map_err(|e| format!("entry [{}]: {}", idx, e))?;
                entries_rust.push(entry_str);
            }
            Initializer::Expr(_) => return Err(format!("entry [{}] is not a compound", idx)),
        }
    }

    let mut out = String::new();
    out.push_str(&format!(
        "#[allow(non_upper_case_globals)]\n\
         pub static {}: [{}; {}] = [\n",
        name, elem_type_str, count
    ));
    for (i, e) in entries_rust.iter().enumerate() {
        out.push_str(&format!("    /* [{}] */ {},\n", i, e));
    }
    out.push_str("];\n");
    Ok(out)
}

/// 1 つの struct literal を組み立てる。
/// inner は `{ a, b, c, ... }` の各値の InitializerItem 列。
/// struct_def のフィールド順に対応付け、bitfield 連続グループは pack する。
fn build_struct_literal(
    type_str: &str,
    struct_def: &StructDef,
    inner: &[crate::ast::InitializerItem],
    interner: &StringInterner,
) -> Result<String, String> {
    // 値式を順に取り出す
    let values: Vec<&Expr> = inner.iter().filter_map(|ii| match &ii.init {
        Initializer::Expr(e) => Some(e.as_ref()),
        _ => None,
    }).collect();

    let mut field_strs: Vec<String> = Vec::new();
    let mut value_idx = 0usize;
    let mut i = 0usize;
    let mut bitfield_group_idx = 0usize;
    while i < struct_def.members.len() {
        let m = &struct_def.members[i];
        if m.bitfield_width.is_some() {
            // bitfield グループ全体の値を pack
            let group_start = i;
            let mut total_width = 0u32;
            let mut group_widths: Vec<u32> = Vec::new();
            while i < struct_def.members.len() && struct_def.members[i].bitfield_width.is_some() {
                let w = struct_def.members[i].bitfield_width.unwrap();
                group_widths.push(w);
                total_width += w;
                i += 1;
            }
            let pack_ty = if total_width <= 8 { "u8" }
                else if total_width <= 16 { "u16" }
                else if total_width <= 32 { "u32" }
                else { "u64" };
            // group_widths.len() 個の値を取り出して pack
            if value_idx + group_widths.len() > values.len() {
                return Err(format!("not enough initializer values for bitfield group at member {}",
                    interner.get(struct_def.members[group_start].name)));
            }
            let mut shift = 0u32;
            let mut parts: Vec<String> = Vec::new();
            for (gi, w) in group_widths.iter().enumerate() {
                let val_expr = values[value_idx + gi];
                let val_rust = translate_const_expr(val_expr, interner);
                let mask = (1u64 << w) - 1;
                let part = format!("(({}) as {} & {:#x}) << {}",
                    val_rust, pack_ty, mask, shift);
                parts.push(crate::syn_codegen::normalize_parens(&part));
                shift += w;
            }
            field_strs.push(format!("_bitfield_{}: {}",
                bitfield_group_idx,
                parts.join(" | ")));
            bitfield_group_idx += 1;
            value_idx += group_widths.len();
        } else {
            // 通常フィールド
            if value_idx >= values.len() {
                return Err(format!("not enough initializer values for member {}",
                    interner.get(m.name)));
            }
            let val_expr = values[value_idx];
            let mut val_rust = translate_const_expr(val_expr, interner);
            let target_ty = m.type_repr.to_rust_string(interner);
            if is_integer_target(&target_ty) {
                val_rust = format!("({}) as {}", val_rust, target_ty);
            }
            let val_rust = crate::syn_codegen::normalize_parens(&val_rust);
            field_strs.push(format!("{}: {}", interner.get(m.name), val_rust));
            value_idx += 1;
            i += 1;
        }
    }

    if value_idx < values.len() {
        return Err(format!("excess initializer values: {} > {}", values.len(), value_idx));
    }

    Ok(format!("{} {{ {} }}", type_str, field_strs.join(", ")))
}

fn is_integer_target(ty: &str) -> bool {
    matches!(ty,
        "u8" | "u16" | "u32" | "u64" | "u128" | "usize" |
        "i8" | "i16" | "i32" | "i64" | "i128" | "isize" |
        "U8" | "U16" | "U32" | "U64" | "I8" | "I16" | "I32" | "I64" |
        "IV" | "UV" | "STRLEN" | "Size_t" | "SSize_t" | "ssize_t" | "size_t")
}

/// 純粋な C const 式を Rust に翻訳する。
/// `+`, `-`, `*`, `?:`, `cast`, `sizeof`, `__builtin_offsetof`, ident, intlit
/// などを扱う。値そのものは **const-eval 可能な Rust 式**を返す。
fn translate_const_expr(expr: &Expr, interner: &StringInterner) -> String {
    match &expr.kind {
        ExprKind::IntLit(n) => format!("{}", n),
        ExprKind::UIntLit(n) => format!("{}", n),
        ExprKind::Ident(name) => {
            // 既知の C 由来 enum/const はそのまま使う（bindings.rs に存在前提）
            interner.get(*name).to_string()
        }
        ExprKind::SizeofType(tn) => {
            let t = type_name_to_rust(tn, interner);
            format!("core::mem::size_of::<{}>()", t)
        }
        ExprKind::Sizeof(inner) => {
            // sizeof(expr) を const-eval 可能な形に翻訳。
            // copy_length(T, m) 展開時の典型形 `((T*)X)->field_chain` に
            // 限定して `core::mem::size_of_val(&core::mem::zeroed::<T>().field_chain)`
            // を生成する。core::mem::zeroed と size_of_val は const-stable
            // (Rust 1.75 / 1.85)。
            if let Some((type_name, field_path)) = match_sizeof_field_pattern(inner, interner) {
                // 一時値の lifetime 問題（E0716）回避のため let 束縛してから
                // 参照を取る。union field アクセスを含む可能性があるため
                // 全体を unsafe block で包む。
                format!(
                    "{{ let _z = unsafe {{ core::mem::zeroed::<{}>() }}; \
                     core::mem::size_of_val(unsafe {{ &_z.{} }}) }}",
                    type_name, field_path
                )
            } else {
                let inner_rust = translate_const_expr(inner, interner);
                // フォールバック: 配列/プリミティブの場合は size_of_val
                format!(
                    "core::mem::size_of_val(&{{ {} }})",
                    inner_rust
                )
            }
        }
        ExprKind::BuiltinCall { name, args } => {
            let func_name = interner.get(*name);
            if matches!(func_name, "offsetof" | "__builtin_offsetof" | "STRUCT_OFFSET")
                && args.len() == 2
            {
                let type_str = match &args[0] {
                    BuiltinArg::TypeName(tn) => type_name_to_rust(tn, interner),
                    BuiltinArg::Expr(e) => translate_const_expr(e, interner),
                };
                let field_path = match &args[1] {
                    BuiltinArg::Expr(e) => expr_to_field_path(e, interner)
                        .unwrap_or_else(|| translate_const_expr(e, interner)),
                    _ => String::from("__UNRESOLVED_FIELD_PATH__"),
                };
                format!("core::mem::offset_of!({}, {})", type_str, field_path)
            } else {
                format!("__UNSUPPORTED_BUILTIN_{}__()", func_name)
            }
        }
        ExprKind::Cast { type_name, expr: inner } => {
            let t = type_name_to_rust(type_name, interner);
            let inner_rust = translate_const_expr(inner, interner);
            // ポインタターゲットの場合 raw pointer cast
            if t.contains('*') {
                format!("(({}) as {})", inner_rust, t)
            } else {
                format!("(({}) as {})", inner_rust, t)
            }
        }
        ExprKind::Binary { op, lhs, rhs } => {
            let l = translate_const_expr(lhs, interner);
            let r = translate_const_expr(rhs, interner);
            let op_str = bin_op_to_rust(op);
            format!("({} {} {})", l, op_str, r)
        }
        ExprKind::UnaryPlus(inner) => translate_const_expr(inner, interner),
        ExprKind::UnaryMinus(inner) => format!("-({})", translate_const_expr(inner, interner)),
        ExprKind::Conditional { cond, then_expr, else_expr } => {
            // C の `cond ? a : b` は値式。Rust 側は const if 式を使う。
            // cond が比較式 (==, !=, <, <=, >, >=, &&, ||) なら既に bool なので
            // そのまま使う。整数式なら `!= 0` で bool 化。
            let c = translate_const_expr(cond, interner);
            let t = translate_const_expr(then_expr, interner);
            let e = translate_const_expr(else_expr, interner);
            if is_bool_expr(cond) {
                format!("(if ({}) {{ {} }} else {{ {} }})", c, t, e)
            } else {
                format!("(if ({}) != 0 {{ {} }} else {{ {} }})", c, t, e)
            }
        }
        ExprKind::Member { expr: base, member } => {
            let b = translate_const_expr(base, interner);
            format!("({}.{})", b, interner.get(*member))
        }
        ExprKind::PtrMember { expr: base, member } => {
            // a->b → (*a).b （place 式を維持。括弧は曖昧性回避用）
            let b = translate_const_expr(base, interner);
            format!("((*{}).{})", b, interner.get(*member))
        }
        ExprKind::Deref(inner) => {
            let i = translate_const_expr(inner, interner);
            format!("(*({}))", i)
        }
        // フォールバック
        _ => format!("/* UNSUPPORTED EXPR: {:?} */ 0",
                     std::mem::discriminant(&expr.kind)),
    }
}

/// `cond ? a : b` の cond 部分が既に bool 値を返すか判定。
/// Comparison/logical op の場合は true。
fn is_bool_expr(expr: &Expr) -> bool {
    use crate::ast::BinOp::*;
    match &expr.kind {
        ExprKind::Binary { op, .. } => matches!(op,
            Lt | Gt | Le | Ge | Eq | Ne | LogAnd | LogOr),
        ExprKind::LogNot(_) => true,
        _ => false,
    }
}

fn bin_op_to_rust(op: &crate::ast::BinOp) -> &'static str {
    use crate::ast::BinOp::*;
    match op {
        Add => "+", Sub => "-", Mul => "*", Div => "/", Mod => "%",
        Lt => "<", Gt => ">", Le => "<=", Ge => ">=",
        Eq => "==", Ne => "!=",
        BitAnd => "&", BitOr => "|", BitXor => "^",
        Shl => "<<", Shr => ">>",
        LogAnd => "&&", LogOr => "||",
    }
}

fn type_name_to_rust(tn: &crate::ast::TypeName, interner: &StringInterner) -> String {
    let repr = TypeRepr::from_type_name(tn, interner);
    repr.to_rust_string(interner)
}

/// `((T*)X)->f1.f2` 形式の sizeof 引数を検出し、(T 名, f1.f2) を返す。
/// `copy_length(T, last_member)` マクロ展開で典型的に現れる:
/// `sizeof((T*)SvANY((const SV*)0))->last_member))` のような形。
fn match_sizeof_field_pattern(
    expr: &Expr,
    interner: &StringInterner,
) -> Option<(String, String)> {
    // outer は Member { base, member } か PtrMember { base, member }
    // base を辿って最終的に Cast { type_name: T*, .. } に到達するまで field path を蓄積
    let mut path: Vec<String> = Vec::new();
    let mut cur = expr;
    loop {
        match &cur.kind {
            ExprKind::Member { expr: base, member }
            | ExprKind::PtrMember { expr: base, member } => {
                path.push(interner.get(*member).to_string());
                cur = base;
            }
            ExprKind::Cast { type_name, expr: _ } => {
                // type_name の派生にポインタが 1 つあれば、その指す型を返す
                let has_ptr = type_name
                    .declarator
                    .as_ref()
                    .map(|d| d.derived.iter().any(|x| matches!(x, crate::ast::DerivedDecl::Pointer(_))))
                    .unwrap_or(false);
                if has_ptr {
                    let mut tn_no_ptr = (**type_name).clone();
                    if let Some(d) = tn_no_ptr.declarator.as_mut() {
                        d.derived.retain(|x| !matches!(x, crate::ast::DerivedDecl::Pointer(_)));
                    }
                    let type_str = type_name_to_rust(&tn_no_ptr, interner);
                    if path.is_empty() { return None; }
                    path.reverse();
                    return Some((type_str, path.join(".")));
                }
                return None;
            }
            _ => return None,
        }
    }
}

/// `xpv_len_u.xpvlenu_len` のような field path を文字列化（offset_of! 用）
fn expr_to_field_path(expr: &Expr, interner: &StringInterner) -> Option<String> {
    match &expr.kind {
        ExprKind::Ident(name) => Some(interner.get(*name).to_string()),
        ExprKind::Member { expr: base, member } => {
            let b = expr_to_field_path(base, interner)?;
            Some(format!("{}.{}", b, interner.get(*member)))
        }
        ExprKind::PtrMember { expr: base, member } => {
            // C `a->b` を offset_of の field path として「a.b」に正規化（パーサ側で
            // anonymous union 等を扱う場合の想定）
            let b = expr_to_field_path(base, interner)?;
            Some(format!("{}.{}", b, interner.get(*member)))
        }
        _ => None,
    }
}
