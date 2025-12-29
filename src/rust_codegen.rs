//! Rust コード生成モジュール
//!
//! C言語のマクロ関数をRust関数に変換する。

use crate::ast::{
    AssignOp, BinOp, BlockItem, CompoundStmt, Declaration, DeclSpecs, DerivedDecl,
    Expr, ForInit, FunctionDef, ParamDecl, Stmt, TypeName, TypeSpec,
};
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
    fn type_spec_to_rust(&self, specs: &crate::ast::DeclSpecs) -> String {
        // unsigned があるかどうかをチェック
        let is_unsigned = specs.type_specs.iter().any(|s| matches!(s, TypeSpec::Unsigned));

        // 基本型を探す
        for spec in &specs.type_specs {
            match spec {
                TypeSpec::Void => return "c_void".to_string(),
                TypeSpec::Char => {
                    return if is_unsigned { "c_uchar" } else { "c_char" }.to_string();
                }
                TypeSpec::Short => {
                    return if is_unsigned { "c_ushort" } else { "c_short" }.to_string();
                }
                TypeSpec::Int => {
                    return if is_unsigned { "c_uint" } else { "c_int" }.to_string();
                }
                TypeSpec::Long => {
                    return if is_unsigned { "c_ulong" } else { "c_long" }.to_string();
                }
                TypeSpec::Float => return "c_float".to_string(),
                TypeSpec::Double => return "c_double".to_string(),
                TypeSpec::Bool => return "bool".to_string(),
                TypeSpec::Signed | TypeSpec::Unsigned => continue,
                // typedef名はそのまま出力
                TypeSpec::TypedefName(id) => {
                    return self.interner.get(*id).to_string();
                }
                // struct/union/enum
                TypeSpec::Struct(s) => {
                    if let Some(name) = s.name {
                        return self.interner.get(name).to_string();
                    }
                    return "/* anonymous struct */".to_string();
                }
                TypeSpec::Union(s) => {
                    if let Some(name) = s.name {
                        return self.interner.get(name).to_string();
                    }
                    return "/* anonymous union */".to_string();
                }
                TypeSpec::Enum(e) => {
                    if let Some(name) = e.name {
                        return self.interner.get(name).to_string();
                    }
                    return "/* anonymous enum */".to_string();
                }
                _ => continue,
            }
        }

        // unsigned/signed だけの場合は int
        if is_unsigned {
            "c_uint".to_string()
        } else if specs.type_specs.iter().any(|s| matches!(s, TypeSpec::Signed)) {
            "c_int".to_string()
        } else {
            "/* unknown type */".to_string()
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

    // ==================== Inline Function Conversion ====================

    /// インライン関数をRust関数に変換
    pub fn inline_fn_to_rust(&self, func: &FunctionDef) -> Result<String, String> {
        // 関数名を取得
        let name = func.declarator.name
            .map(|id| self.interner.get(id).to_string())
            .ok_or_else(|| "anonymous function".to_string())?;

        // パラメータリストを取得
        let params = self.extract_fn_params(&func.declarator.derived)?;

        // 戻り値型を取得
        let ret_ty = self.extract_return_type(&func.specs, &func.declarator.derived);

        // 関数本体を変換
        let body = self.compound_stmt_to_rust(&func.body, 1);

        Ok(format!(
            "#[inline]\npub unsafe fn {}({}) -> {} {{\n{}}}\n",
            name, params, ret_ty, body
        ))
    }

    /// 関数パラメータを抽出してフォーマット
    fn extract_fn_params(&self, derived: &[DerivedDecl]) -> Result<String, String> {
        for d in derived {
            if let DerivedDecl::Function(param_list) = d {
                let params: Vec<String> = param_list.params.iter()
                    .filter_map(|p| self.param_decl_to_rust(p))
                    .collect();
                return Ok(params.join(", "));
            }
        }
        Ok(String::new())
    }

    /// パラメータ宣言をRust形式に変換
    fn param_decl_to_rust(&self, param: &ParamDecl) -> Option<String> {
        let name = param.declarator.as_ref()
            .and_then(|d| d.name)
            .map(|id| self.interner.get(id).to_string())
            .unwrap_or_else(|| "_".to_string());

        let ty = self.decl_to_rust_type(&param.specs, param.declarator.as_ref());
        Some(format!("{}: {}", name, ty))
    }

    /// パラメータ宣言から型のみを取得（公開API）
    pub fn param_decl_to_rust_type(&self, param: &ParamDecl) -> String {
        self.decl_to_rust_type(&param.specs, param.declarator.as_ref())
    }

    /// 宣言からRust型を生成
    fn decl_to_rust_type(&self, specs: &DeclSpecs, declarator: Option<&crate::ast::Declarator>) -> String {
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

        let base_ty = self.type_spec_to_rust(specs);
        format!("{}{}", ptr_prefix, base_ty)
    }

    /// 戻り値型を抽出
    fn extract_return_type(&self, specs: &DeclSpecs, derived: &[DerivedDecl]) -> String {
        // void の場合
        if specs.type_specs.iter().any(|s| matches!(s, TypeSpec::Void)) {
            // ポインタがあるか確認
            let has_pointer = derived.iter().any(|d| {
                if let DerivedDecl::Pointer(_) = d { true } else { false }
            });
            if !has_pointer {
                return "()".to_string();
            }
        }

        self.type_spec_to_rust(specs)
    }

    /// 関数の戻り値型を抽出（公開API）
    /// 戻り値: Some(型文字列) または None（voidの場合）
    pub fn extract_fn_return_type(&self, specs: &DeclSpecs, derived: &[DerivedDecl]) -> Option<String> {
        let ty = self.extract_return_type(specs, derived);
        if ty == "()" {
            None
        } else {
            Some(ty)
        }
    }

    /// 複合文をRustに変換
    fn compound_stmt_to_rust(&self, stmt: &CompoundStmt, indent: usize) -> String {
        let mut result = String::new();

        for item in &stmt.items {
            match item {
                BlockItem::Decl(decl) => {
                    result.push_str(&self.decl_to_rust(decl, indent));
                }
                BlockItem::Stmt(s) => {
                    result.push_str(&self.stmt_to_rust(s, indent));
                }
            }
        }

        result
    }

    /// 宣言をRustに変換
    fn decl_to_rust(&self, decl: &Declaration, indent: usize) -> String {
        let indent_str = "    ".repeat(indent);
        let mut result = String::new();

        for init_decl in &decl.declarators {
            let name = init_decl.declarator.name
                .map(|id| self.interner.get(id).to_string())
                .unwrap_or_else(|| "_".to_string());

            let ty = self.decl_to_rust_type(&decl.specs, Some(&init_decl.declarator));

            if let Some(ref init) = init_decl.init {
                match init {
                    crate::ast::Initializer::Expr(expr) => {
                        let expr_str = self.expr_to_rust(expr);
                        result.push_str(&format!("{}let mut {}: {} = {};\n", indent_str, name, ty, expr_str));
                    }
                    crate::ast::Initializer::List(_) => {
                        result.push_str(&format!("{}let mut {}: {} = /* initializer list */;\n", indent_str, name, ty));
                    }
                }
            } else {
                result.push_str(&format!("{}let mut {}: {};\n", indent_str, name, ty));
            }
        }

        result
    }

    /// 文をRustに変換
    fn stmt_to_rust(&self, stmt: &Stmt, indent: usize) -> String {
        let indent_str = "    ".repeat(indent);

        match stmt {
            Stmt::Compound(compound) => {
                let inner = self.compound_stmt_to_rust(compound, indent + 1);
                format!("{}{{\n{}{}}}\n", indent_str, inner, indent_str)
            }

            Stmt::Expr(Some(expr), _) => {
                let expr_str = self.expr_to_rust(expr);
                format!("{}{};\n", indent_str, expr_str)
            }

            Stmt::Expr(None, _) => {
                // 空文
                String::new()
            }

            Stmt::If { cond, then_stmt, else_stmt, .. } => {
                let cond_str = self.expr_to_rust(cond);
                let then_str = self.stmt_to_rust_block(then_stmt, indent);

                if let Some(else_s) = else_stmt {
                    let else_str = self.stmt_to_rust_block(else_s, indent);
                    format!("{}if {} != 0 {} else {}\n", indent_str, cond_str, then_str, else_str)
                } else {
                    format!("{}if {} != 0 {}\n", indent_str, cond_str, then_str)
                }
            }

            Stmt::While { cond, body, .. } => {
                let cond_str = self.expr_to_rust(cond);
                let body_str = self.stmt_to_rust_block(body, indent);
                format!("{}while {} != 0 {}\n", indent_str, cond_str, body_str)
            }

            Stmt::DoWhile { body, cond, .. } => {
                let cond_str = self.expr_to_rust(cond);
                let body_str = self.stmt_to_rust_block(body, indent);
                format!("{}loop {}\n{}if !({} != 0) {{ break; }}\n", indent_str, body_str, indent_str, cond_str)
            }

            Stmt::For { init, cond, step, body, .. } => {
                let mut result = String::new();

                // init
                if let Some(init) = init {
                    match init {
                        ForInit::Expr(expr) => {
                            result.push_str(&format!("{}{};\n", indent_str, self.expr_to_rust(expr)));
                        }
                        ForInit::Decl(decl) => {
                            result.push_str(&self.decl_to_rust(decl, indent));
                        }
                    }
                }

                // while loop
                let cond_str = cond.as_ref()
                    .map(|c| format!("{} != 0", self.expr_to_rust(c)))
                    .unwrap_or_else(|| "true".to_string());

                result.push_str(&format!("{}while {} {{\n", indent_str, cond_str));

                // body
                if let Stmt::Compound(compound) = body.as_ref() {
                    result.push_str(&self.compound_stmt_to_rust(compound, indent + 1));
                } else {
                    result.push_str(&self.stmt_to_rust(body, indent + 1));
                }

                // step
                if let Some(step) = step {
                    result.push_str(&format!("{}    {};\n", indent_str, self.expr_to_rust(step)));
                }

                result.push_str(&format!("{}}}\n", indent_str));
                result
            }

            Stmt::Return(Some(expr), _) => {
                let expr_str = self.expr_to_rust(expr);
                format!("{}return {};\n", indent_str, expr_str)
            }

            Stmt::Return(None, _) => {
                format!("{}return;\n", indent_str)
            }

            Stmt::Break(_) => {
                format!("{}break;\n", indent_str)
            }

            Stmt::Continue(_) => {
                format!("{}continue;\n", indent_str)
            }

            Stmt::Goto(label, _) => {
                let label_str = self.interner.get(*label);
                format!("{}/* goto {} */\n", indent_str, label_str)
            }

            Stmt::Label { name, stmt, .. } => {
                let name_str = self.interner.get(*name);
                let stmt_str = self.stmt_to_rust(stmt, indent);
                format!("{}/* label: {} */\n{}", indent_str, name_str, stmt_str)
            }

            Stmt::Switch { expr, body, .. } => {
                let expr_str = self.expr_to_rust(expr);
                let body_str = self.stmt_to_rust(body, indent);
                format!("{}match {} {{\n{}{}    _ => {{}}\n{}}}\n",
                    indent_str, expr_str, body_str, indent_str, indent_str)
            }

            Stmt::Case { expr, stmt, .. } => {
                let expr_str = self.expr_to_rust(expr);
                let stmt_str = self.stmt_to_rust(stmt, indent + 1);
                format!("{}    {} => {{\n{}{}}}\n", indent_str, expr_str, stmt_str, indent_str)
            }

            Stmt::Default { stmt, .. } => {
                let stmt_str = self.stmt_to_rust(stmt, indent + 1);
                format!("{}    _ => {{\n{}{}}}\n", indent_str, stmt_str, indent_str)
            }

            Stmt::Asm { .. } => {
                format!("{}/* asm */\n", indent_str)
            }
        }
    }

    /// 文をブロック形式に変換（if/while等のbody用）
    fn stmt_to_rust_block(&self, stmt: &Stmt, indent: usize) -> String {
        match stmt {
            Stmt::Compound(compound) => {
                let inner = self.compound_stmt_to_rust(compound, indent + 1);
                let indent_str = "    ".repeat(indent);
                format!("{{\n{}{}}}", inner, indent_str)
            }
            _ => {
                let inner = self.stmt_to_rust(stmt, indent + 1);
                let indent_str = "    ".repeat(indent);
                format!("{{\n{}{}}}", inner, indent_str)
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
