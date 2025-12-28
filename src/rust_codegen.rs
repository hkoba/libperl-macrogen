//! Rust コード生成モジュール
//!
//! C言語のマクロ関数をRust関数に変換する。

use crate::ast::{AssignOp, BinOp, Expr, TypeName, TypeSpec};
use crate::fields_dict::FieldsDict;
use crate::intern::StringInterner;
use crate::macro_analysis::MacroInfo;
use crate::macro_def::{MacroDef, MacroKind};

/// Rustコード生成器
pub struct RustCodeGen<'a> {
    /// 文字列インターナー
    interner: &'a StringInterner,
    /// フィールド辞書（型推論用）
    #[allow(dead_code)]
    fields_dict: &'a FieldsDict,
}

impl<'a> RustCodeGen<'a> {
    /// 新しいコード生成器を作成
    pub fn new(interner: &'a StringInterner, fields_dict: &'a FieldsDict) -> Self {
        Self {
            interner,
            fields_dict,
        }
    }

    /// 式をRustコードに変換
    pub fn expr_to_rust(&self, expr: &Expr) -> String {
        match expr {
            Expr::IntLit(n, _) => n.to_string(),

            Expr::UIntLit(n, _) => format!("{}u64", n),

            Expr::FloatLit(f, _) => f.to_string(),

            Expr::CharLit(c, _) => format!("'{}' as c_char", self.escape_char(*c as char)),

            Expr::StringLit(s, _) => {
                let escaped = self.escape_bytes(s);
                format!("c\"{}\"", escaped)
            }

            Expr::Ident(id, _) => self.interner.get(*id).to_string(),

            Expr::Binary { op, lhs, rhs, .. } => {
                let left_str = self.expr_to_rust(lhs);
                let right_str = self.expr_to_rust(rhs);
                let op_str = self.bin_op_to_rust(op);
                format!("({} {} {})", left_str, op_str, right_str)
            }

            // 単項演算子
            Expr::UnaryPlus(inner, _) => {
                format!("(+{})", self.expr_to_rust(inner))
            }
            Expr::UnaryMinus(inner, _) => {
                format!("(-{})", self.expr_to_rust(inner))
            }
            Expr::BitNot(inner, _) => {
                format!("(!{})", self.expr_to_rust(inner))
            }
            Expr::LogNot(inner, _) => {
                format!("(!{})", self.expr_to_rust(inner))
            }
            Expr::Deref(inner, _) => {
                format!("(*{})", self.expr_to_rust(inner))
            }
            Expr::AddrOf(inner, _) => {
                format!("(&{})", self.expr_to_rust(inner))
            }
            Expr::PreInc(inner, _) => {
                format!("/* ++{} */", self.expr_to_rust(inner))
            }
            Expr::PreDec(inner, _) => {
                format!("/* --{} */", self.expr_to_rust(inner))
            }
            Expr::PostInc(inner, _) => {
                format!("/* {}++ */", self.expr_to_rust(inner))
            }
            Expr::PostDec(inner, _) => {
                format!("/* {}-- */", self.expr_to_rust(inner))
            }

            // メンバアクセス
            Expr::Member { expr, member, .. } => {
                let expr_str = self.expr_to_rust(expr);
                let member_str = self.interner.get(*member);
                format!("{}.{}", expr_str, member_str)
            }
            Expr::PtrMember { expr, member, .. } => {
                let expr_str = self.expr_to_rust(expr);
                let member_str = self.interner.get(*member);
                // ptr->field => (*ptr).field
                format!("(*{}).{}", expr_str, member_str)
            }

            Expr::Index { expr, index, .. } => {
                let expr_str = self.expr_to_rust(expr);
                let index_str = self.expr_to_rust(index);
                format!("{}[{} as usize]", expr_str, index_str)
            }

            Expr::Call { func, args, .. } => {
                let func_str = self.expr_to_rust(func);
                let args_str: Vec<String> = args.iter()
                    .map(|a| self.expr_to_rust(a))
                    .collect();
                format!("{}({})", func_str, args_str.join(", "))
            }

            Expr::Cast { type_name, expr, .. } => {
                let expr_str = self.expr_to_rust(expr);
                let ty_str = self.type_name_to_rust(type_name);
                format!("({} as {})", expr_str, ty_str)
            }

            Expr::Sizeof(inner, _) => {
                format!("std::mem::size_of_val(&{})", self.expr_to_rust(inner))
            }

            Expr::SizeofType(type_name, _) => {
                let ty_str = self.type_name_to_rust(type_name);
                format!("std::mem::size_of::<{}>()", ty_str)
            }

            Expr::Alignof(type_name, _) => {
                let ty_str = self.type_name_to_rust(type_name);
                format!("std::mem::align_of::<{}>()", ty_str)
            }

            Expr::Conditional { cond, then_expr, else_expr, .. } => {
                let cond_str = self.expr_to_rust(cond);
                let then_str = self.expr_to_rust(then_expr);
                let else_str = self.expr_to_rust(else_expr);
                format!("(if {} != 0 {{ {} }} else {{ {} }})", cond_str, then_str, else_str)
            }

            Expr::Comma { lhs, rhs, .. } => {
                // Rustではカンマ演算子がないので、ブロック式にする
                let left_str = self.expr_to_rust(lhs);
                let right_str = self.expr_to_rust(rhs);
                format!("{{ let _ = {}; {} }}", left_str, right_str)
            }

            Expr::Assign { op, lhs, rhs, .. } => {
                let left_str = self.expr_to_rust(lhs);
                let right_str = self.expr_to_rust(rhs);
                let op_str = self.assign_op_to_rust(op);
                format!("{} {} {}", left_str, op_str, right_str)
            }

            Expr::CompoundLit { .. } => {
                "/* compound literal */".to_string()
            }

            Expr::StmtExpr(_, _) => {
                "/* statement expression */".to_string()
            }
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
    fn type_name_to_rust(&self, ty: &TypeName) -> String {
        let mut result = String::new();

        // ポインタを考慮
        if let Some(ref decl) = ty.declarator {
            for derived in &decl.derived {
                if let crate::ast::DerivedDecl::Pointer(_) = derived {
                    if ty.specs.qualifiers.is_const {
                        result.push_str("*const ");
                    } else {
                        result.push_str("*mut ");
                    }
                }
            }
        }

        // 基本型
        let type_str = self.type_spec_to_rust(&ty.specs);
        format!("{}{}", result, type_str)
    }

    /// 型指定子をRust型に変換
    fn type_spec_to_rust(&self, specs: &crate::ast::DeclSpecs) -> &'static str {
        // unsigned があるかどうかをチェック
        let is_unsigned = specs.type_specs.iter().any(|s| matches!(s, TypeSpec::Unsigned));

        // 基本型を探す
        for spec in &specs.type_specs {
            match spec {
                TypeSpec::Void => return "c_void",
                TypeSpec::Char => {
                    return if is_unsigned { "c_uchar" } else { "c_char" };
                }
                TypeSpec::Short => {
                    return if is_unsigned { "c_ushort" } else { "c_short" };
                }
                TypeSpec::Int => {
                    return if is_unsigned { "c_uint" } else { "c_int" };
                }
                TypeSpec::Long => {
                    return if is_unsigned { "c_ulong" } else { "c_long" };
                }
                TypeSpec::Float => return "c_float",
                TypeSpec::Double => return "c_double",
                TypeSpec::Bool => return "bool",
                TypeSpec::Signed | TypeSpec::Unsigned => continue,
                _ => continue,
            }
        }

        // unsigned/signed だけの場合は int
        if is_unsigned {
            "c_uint"
        } else if specs.type_specs.iter().any(|s| matches!(s, TypeSpec::Signed)) {
            "c_int"
        } else {
            "/* unknown type */"
        }
    }

    /// マクロをRust関数に変換
    pub fn macro_to_rust_fn(&self, def: &MacroDef, info: &MacroInfo, expr: &Expr) -> String {
        let name = self.interner.get(def.name);

        // パラメータを構築
        let params = self.format_params(def, info);

        // 戻り値型
        let ret_ty = info.return_type.as_deref().unwrap_or("()");

        // 本体
        let body = self.expr_to_rust(expr);

        format!(
            "#[inline]\npub unsafe fn {}({}) -> {} {{\n    {}\n}}\n",
            name, params, ret_ty, body
        )
    }

    /// パラメータをフォーマット
    fn format_params(&self, def: &MacroDef, info: &MacroInfo) -> String {
        if let MacroKind::Function { ref params, .. } = def.kind {
            params.iter()
                .map(|p| {
                    let name = self.interner.get(*p);
                    let ty = info.param_types.get(p)
                        .map(|s| s.as_str())
                        .unwrap_or("/* unknown */");
                    format!("{}: {}", name, ty)
                })
                .collect::<Vec<_>>()
                .join(", ")
        } else {
            String::new()
        }
    }

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
    use crate::source::SourceLocation;

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

        let expr = Expr::IntLit(42, SourceLocation::default());
        assert_eq!(codegen.expr_to_rust(&expr), "42");
    }

    #[test]
    fn test_binary_op() {
        let mut interner = make_interner();
        let fields = make_fields_dict();

        let x = interner.intern("x");
        let y = interner.intern("y");
        let loc = SourceLocation::default();

        let codegen = RustCodeGen::new(&interner, &fields);

        let expr = Expr::Binary {
            op: BinOp::Add,
            lhs: Box::new(Expr::Ident(x, loc.clone())),
            rhs: Box::new(Expr::Ident(y, loc.clone())),
            loc,
        };
        assert_eq!(codegen.expr_to_rust(&expr), "(x + y)");
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
        let expr = Expr::PtrMember {
            expr: Box::new(Expr::Ident(sv, loc.clone())),
            member: sv_any,
            loc,
        };
        assert_eq!(codegen.expr_to_rust(&expr), "(*sv).sv_any");
    }
}
