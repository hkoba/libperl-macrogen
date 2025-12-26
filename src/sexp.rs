//! S-expression形式でのAST出力
//!
//! ASTをS-expression形式で出力する。デバッグや解析に便利。

use std::io::{Result, Write};

use crate::ast::*;
use crate::intern::StringInterner;

/// S-expression出力プリンター
pub struct SexpPrinter<'a, W: Write> {
    writer: W,
    interner: &'a StringInterner,
    indent: usize,
    pretty: bool,
}

impl<'a, W: Write> SexpPrinter<'a, W> {
    /// 新しいプリンターを作成
    pub fn new(writer: W, interner: &'a StringInterner) -> Self {
        Self {
            writer,
            interner,
            indent: 0,
            pretty: true,
        }
    }

    /// 整形出力の有無を設定
    pub fn set_pretty(&mut self, pretty: bool) {
        self.pretty = pretty;
    }

    /// 改行を出力
    pub fn writeln(&mut self) -> Result<()> {
        writeln!(self.writer)
    }

    /// 翻訳単位を出力
    pub fn print_translation_unit(&mut self, tu: &TranslationUnit) -> Result<()> {
        self.write_open("translation-unit")?;
        for decl in &tu.decls {
            self.print_external_decl(decl)?;
        }
        self.write_close()?;
        if self.pretty {
            writeln!(self.writer)?;
        }
        Ok(())
    }

    /// 外部宣言を出力
    pub fn print_external_decl(&mut self, decl: &ExternalDecl) -> Result<()> {
        match decl {
            ExternalDecl::FunctionDef(func) => self.print_function_def(func),
            ExternalDecl::Declaration(decl) => self.print_declaration(decl),
        }
    }

    /// 関数定義を出力
    fn print_function_def(&mut self, func: &FunctionDef) -> Result<()> {
        self.write_open("function-def")?;
        self.print_decl_specs(&func.specs)?;
        self.print_declarator(&func.declarator)?;
        self.print_compound_stmt(&func.body)?;
        self.write_close()?;
        Ok(())
    }

    /// 宣言を出力
    fn print_declaration(&mut self, decl: &Declaration) -> Result<()> {
        self.write_open("declaration")?;
        self.print_decl_specs(&decl.specs)?;
        for init_decl in &decl.declarators {
            self.print_init_declarator(init_decl)?;
        }
        self.write_close()?;
        Ok(())
    }

    /// 宣言指定子を出力
    fn print_decl_specs(&mut self, specs: &DeclSpecs) -> Result<()> {
        self.write_open("decl-specs")?;

        if let Some(storage) = &specs.storage {
            let s = match storage {
                StorageClass::Typedef => "typedef",
                StorageClass::Extern => "extern",
                StorageClass::Static => "static",
                StorageClass::Auto => "auto",
                StorageClass::Register => "register",
            };
            self.write_atom(s)?;
        }

        if specs.is_inline {
            self.write_atom("inline")?;
        }

        self.print_type_qualifiers(&specs.qualifiers)?;

        for type_spec in &specs.type_specs {
            self.print_type_spec(type_spec)?;
        }

        self.write_close()?;
        Ok(())
    }

    /// 型指定子を出力
    fn print_type_spec(&mut self, spec: &TypeSpec) -> Result<()> {
        match spec {
            TypeSpec::Void => self.write_atom("void"),
            TypeSpec::Char => self.write_atom("char"),
            TypeSpec::Short => self.write_atom("short"),
            TypeSpec::Int => self.write_atom("int"),
            TypeSpec::Long => self.write_atom("long"),
            TypeSpec::Float => self.write_atom("float"),
            TypeSpec::Double => self.write_atom("double"),
            TypeSpec::Signed => self.write_atom("signed"),
            TypeSpec::Unsigned => self.write_atom("unsigned"),
            TypeSpec::Bool => self.write_atom("_Bool"),
            TypeSpec::Complex => self.write_atom("_Complex"),
            TypeSpec::Float16 => self.write_atom("_Float16"),
            TypeSpec::Float32 => self.write_atom("_Float32"),
            TypeSpec::Float64 => self.write_atom("_Float64"),
            TypeSpec::Float128 => self.write_atom("_Float128"),
            TypeSpec::Float32x => self.write_atom("_Float32x"),
            TypeSpec::Float64x => self.write_atom("_Float64x"),
            TypeSpec::Int128 => self.write_atom("__int128"),
            TypeSpec::TypeofExpr(expr) => {
                self.write_open("typeof")?;
                self.print_expr(expr)?;
                self.write_close()
            }
            TypeSpec::Struct(s) => self.print_struct_spec("struct", s),
            TypeSpec::Union(s) => self.print_struct_spec("union", s),
            TypeSpec::Enum(e) => self.print_enum_spec(e),
            TypeSpec::TypedefName(id) => {
                self.write_open("typedef-name")?;
                write!(self.writer, " {}", self.interner.get(*id))?;
                self.write_close()
            }
        }
    }

    /// 構造体/共用体指定を出力
    fn print_struct_spec(&mut self, kind: &str, spec: &StructSpec) -> Result<()> {
        self.write_open(kind)?;
        if let Some(name) = spec.name {
            write!(self.writer, " {}", self.interner.get(name))?;
        }
        if let Some(members) = &spec.members {
            for member in members {
                self.print_struct_member(member)?;
            }
        }
        self.write_close()?;
        Ok(())
    }

    /// 構造体メンバーを出力
    fn print_struct_member(&mut self, member: &StructMember) -> Result<()> {
        self.write_open("member")?;
        self.print_decl_specs(&member.specs)?;
        for decl in &member.declarators {
            if let Some(d) = &decl.declarator {
                self.print_declarator(d)?;
            }
            if let Some(bf) = &decl.bitfield {
                self.write_open("bitfield")?;
                self.print_expr(bf)?;
                self.write_close()?;
            }
        }
        self.write_close()?;
        Ok(())
    }

    /// 列挙型指定を出力
    fn print_enum_spec(&mut self, spec: &EnumSpec) -> Result<()> {
        self.write_open("enum")?;
        if let Some(name) = spec.name {
            write!(self.writer, " {}", self.interner.get(name))?;
        }
        if let Some(enumerators) = &spec.enumerators {
            for e in enumerators {
                self.write_open("enumerator")?;
                write!(self.writer, " {}", self.interner.get(e.name))?;
                if let Some(val) = &e.value {
                    self.print_expr(val)?;
                }
                self.write_close()?;
            }
        }
        self.write_close()?;
        Ok(())
    }

    /// 型修飾子を出力
    fn print_type_qualifiers(&mut self, quals: &TypeQualifiers) -> Result<()> {
        if quals.is_const {
            self.write_atom("const")?;
        }
        if quals.is_volatile {
            self.write_atom("volatile")?;
        }
        if quals.is_restrict {
            self.write_atom("restrict")?;
        }
        if quals.is_atomic {
            self.write_atom("_Atomic")?;
        }
        Ok(())
    }

    /// 宣言子を出力
    fn print_declarator(&mut self, decl: &Declarator) -> Result<()> {
        self.write_open("declarator")?;
        if let Some(name) = decl.name {
            write!(self.writer, " {}", self.interner.get(name))?;
        }
        for derived in &decl.derived {
            self.print_derived_decl(derived)?;
        }
        self.write_close()?;
        Ok(())
    }

    /// 派生宣言子を出力
    fn print_derived_decl(&mut self, derived: &DerivedDecl) -> Result<()> {
        match derived {
            DerivedDecl::Pointer(quals) => {
                self.write_open("pointer")?;
                self.print_type_qualifiers(quals)?;
                self.write_close()?;
            }
            DerivedDecl::Array(arr) => {
                self.write_open("array")?;
                if arr.is_static {
                    self.write_atom("static")?;
                }
                if arr.is_vla {
                    self.write_atom("vla")?;
                }
                self.print_type_qualifiers(&arr.qualifiers)?;
                if let Some(size) = &arr.size {
                    self.print_expr(size)?;
                }
                self.write_close()?;
            }
            DerivedDecl::Function(params) => {
                self.write_open("function")?;
                for param in &params.params {
                    self.print_param_decl(param)?;
                }
                if params.is_variadic {
                    self.write_atom("...")?;
                }
                self.write_close()?;
            }
        }
        Ok(())
    }

    /// パラメータ宣言を出力
    fn print_param_decl(&mut self, param: &ParamDecl) -> Result<()> {
        self.write_open("param")?;
        self.print_decl_specs(&param.specs)?;
        if let Some(decl) = &param.declarator {
            self.print_declarator(decl)?;
        }
        self.write_close()?;
        Ok(())
    }

    /// 初期化子付き宣言子を出力
    fn print_init_declarator(&mut self, init_decl: &InitDeclarator) -> Result<()> {
        self.write_open("init-declarator")?;
        self.print_declarator(&init_decl.declarator)?;
        if let Some(init) = &init_decl.init {
            self.print_initializer(init)?;
        }
        self.write_close()?;
        Ok(())
    }

    /// 初期化子を出力
    fn print_initializer(&mut self, init: &Initializer) -> Result<()> {
        match init {
            Initializer::Expr(expr) => self.print_expr(expr),
            Initializer::List(items) => {
                self.write_open("init-list")?;
                for item in items {
                    self.write_open("init-item")?;
                    for desig in &item.designation {
                        match desig {
                            Designator::Index(idx) => {
                                self.write_open("index")?;
                                self.print_expr(idx)?;
                                self.write_close()?;
                            }
                            Designator::Member(name) => {
                                self.write_open("member")?;
                                write!(self.writer, " {}", self.interner.get(*name))?;
                                self.write_close()?;
                            }
                        }
                    }
                    self.print_initializer(&item.init)?;
                    self.write_close()?;
                }
                self.write_close()?;
                Ok(())
            }
        }
    }

    /// 複合文を出力
    fn print_compound_stmt(&mut self, stmt: &CompoundStmt) -> Result<()> {
        self.write_open("compound-stmt")?;
        for item in &stmt.items {
            match item {
                BlockItem::Decl(decl) => self.print_declaration(decl)?,
                BlockItem::Stmt(stmt) => self.print_stmt(stmt)?,
            }
        }
        self.write_close()?;
        Ok(())
    }

    /// 文を出力
    fn print_stmt(&mut self, stmt: &Stmt) -> Result<()> {
        match stmt {
            Stmt::Compound(compound) => self.print_compound_stmt(compound),
            Stmt::Expr(expr, _) => {
                self.write_open("expr-stmt")?;
                if let Some(e) = expr {
                    self.print_expr(e)?;
                }
                self.write_close()
            }
            Stmt::If { cond, then_stmt, else_stmt, .. } => {
                self.write_open("if")?;
                self.print_expr(cond)?;
                self.print_stmt(then_stmt)?;
                if let Some(else_s) = else_stmt {
                    self.print_stmt(else_s)?;
                }
                self.write_close()
            }
            Stmt::Switch { expr, body, .. } => {
                self.write_open("switch")?;
                self.print_expr(expr)?;
                self.print_stmt(body)?;
                self.write_close()
            }
            Stmt::While { cond, body, .. } => {
                self.write_open("while")?;
                self.print_expr(cond)?;
                self.print_stmt(body)?;
                self.write_close()
            }
            Stmt::DoWhile { body, cond, .. } => {
                self.write_open("do-while")?;
                self.print_stmt(body)?;
                self.print_expr(cond)?;
                self.write_close()
            }
            Stmt::For { init, cond, step, body, .. } => {
                self.write_open("for")?;
                if let Some(i) = init {
                    match i {
                        ForInit::Expr(e) => self.print_expr(e)?,
                        ForInit::Decl(d) => self.print_declaration(d)?,
                    }
                }
                if let Some(c) = cond {
                    self.print_expr(c)?;
                }
                if let Some(s) = step {
                    self.print_expr(s)?;
                }
                self.print_stmt(body)?;
                self.write_close()
            }
            Stmt::Goto(name, _) => {
                self.write_open("goto")?;
                write!(self.writer, " {}", self.interner.get(*name))?;
                self.write_close()
            }
            Stmt::Continue(_) => self.write_atom("continue"),
            Stmt::Break(_) => self.write_atom("break"),
            Stmt::Return(expr, _) => {
                self.write_open("return")?;
                if let Some(e) = expr {
                    self.print_expr(e)?;
                }
                self.write_close()
            }
            Stmt::Label { name, stmt, .. } => {
                self.write_open("label")?;
                write!(self.writer, " {}", self.interner.get(*name))?;
                self.print_stmt(stmt)?;
                self.write_close()
            }
            Stmt::Case { expr, stmt, .. } => {
                self.write_open("case")?;
                self.print_expr(expr)?;
                self.print_stmt(stmt)?;
                self.write_close()
            }
            Stmt::Default { stmt, .. } => {
                self.write_open("default")?;
                self.print_stmt(stmt)?;
                self.write_close()
            }
        }
    }

    /// 式を出力
    fn print_expr(&mut self, expr: &Expr) -> Result<()> {
        match expr {
            Expr::Ident(id, _) => {
                self.write_open("ident")?;
                write!(self.writer, " {}", self.interner.get(*id))?;
                self.write_close()
            }
            Expr::IntLit(n, _) => {
                self.write_open("int")?;
                write!(self.writer, " {}", n)?;
                self.write_close()
            }
            Expr::UIntLit(n, _) => {
                self.write_open("uint")?;
                write!(self.writer, " {}", n)?;
                self.write_close()
            }
            Expr::FloatLit(f, _) => {
                self.write_open("float")?;
                write!(self.writer, " {}", f)?;
                self.write_close()
            }
            Expr::CharLit(c, _) => {
                self.write_open("char")?;
                write!(self.writer, " {}", c)?;
                self.write_close()
            }
            Expr::StringLit(s, _) => {
                self.write_open("string")?;
                write!(self.writer, " {:?}", String::from_utf8_lossy(s))?;
                self.write_close()
            }
            Expr::Index { expr, index, .. } => {
                self.write_open("index")?;
                self.print_expr(expr)?;
                self.print_expr(index)?;
                self.write_close()
            }
            Expr::Call { func, args, .. } => {
                self.write_open("call")?;
                self.print_expr(func)?;
                for arg in args {
                    self.print_expr(arg)?;
                }
                self.write_close()
            }
            Expr::Member { expr, member, .. } => {
                self.write_open("member")?;
                self.print_expr(expr)?;
                write!(self.writer, " {}", self.interner.get(*member))?;
                self.write_close()
            }
            Expr::PtrMember { expr, member, .. } => {
                self.write_open("ptr-member")?;
                self.print_expr(expr)?;
                write!(self.writer, " {}", self.interner.get(*member))?;
                self.write_close()
            }
            Expr::PostInc(e, _) => {
                self.write_open("post-inc")?;
                self.print_expr(e)?;
                self.write_close()
            }
            Expr::PostDec(e, _) => {
                self.write_open("post-dec")?;
                self.print_expr(e)?;
                self.write_close()
            }
            Expr::CompoundLit { type_name, init, .. } => {
                self.write_open("compound-lit")?;
                self.print_type_name(type_name)?;
                for item in init {
                    self.print_initializer(&Initializer::List(vec![item.clone()]))?;
                }
                self.write_close()
            }
            Expr::PreInc(e, _) => {
                self.write_open("pre-inc")?;
                self.print_expr(e)?;
                self.write_close()
            }
            Expr::PreDec(e, _) => {
                self.write_open("pre-dec")?;
                self.print_expr(e)?;
                self.write_close()
            }
            Expr::AddrOf(e, _) => {
                self.write_open("addr-of")?;
                self.print_expr(e)?;
                self.write_close()
            }
            Expr::Deref(e, _) => {
                self.write_open("deref")?;
                self.print_expr(e)?;
                self.write_close()
            }
            Expr::UnaryPlus(e, _) => {
                self.write_open("unary-plus")?;
                self.print_expr(e)?;
                self.write_close()
            }
            Expr::UnaryMinus(e, _) => {
                self.write_open("unary-minus")?;
                self.print_expr(e)?;
                self.write_close()
            }
            Expr::BitNot(e, _) => {
                self.write_open("bit-not")?;
                self.print_expr(e)?;
                self.write_close()
            }
            Expr::LogNot(e, _) => {
                self.write_open("log-not")?;
                self.print_expr(e)?;
                self.write_close()
            }
            Expr::Sizeof(e, _) => {
                self.write_open("sizeof")?;
                self.print_expr(e)?;
                self.write_close()
            }
            Expr::SizeofType(ty, _) => {
                self.write_open("sizeof-type")?;
                self.print_type_name(ty)?;
                self.write_close()
            }
            Expr::Alignof(ty, _) => {
                self.write_open("alignof")?;
                self.print_type_name(ty)?;
                self.write_close()
            }
            Expr::Cast { type_name, expr, .. } => {
                self.write_open("cast")?;
                self.print_type_name(type_name)?;
                self.print_expr(expr)?;
                self.write_close()
            }
            Expr::Binary { op, lhs, rhs, .. } => {
                let op_str = match op {
                    BinOp::Mul => "*",
                    BinOp::Div => "/",
                    BinOp::Mod => "%",
                    BinOp::Add => "+",
                    BinOp::Sub => "-",
                    BinOp::Shl => "<<",
                    BinOp::Shr => ">>",
                    BinOp::Lt => "<",
                    BinOp::Gt => ">",
                    BinOp::Le => "<=",
                    BinOp::Ge => ">=",
                    BinOp::Eq => "==",
                    BinOp::Ne => "!=",
                    BinOp::BitAnd => "&",
                    BinOp::BitXor => "^",
                    BinOp::BitOr => "|",
                    BinOp::LogAnd => "&&",
                    BinOp::LogOr => "||",
                };
                self.write_open(op_str)?;
                self.print_expr(lhs)?;
                self.print_expr(rhs)?;
                self.write_close()
            }
            Expr::Conditional { cond, then_expr, else_expr, .. } => {
                self.write_open("?")?;
                self.print_expr(cond)?;
                self.print_expr(then_expr)?;
                self.print_expr(else_expr)?;
                self.write_close()
            }
            Expr::Assign { op, lhs, rhs, .. } => {
                let op_str = match op {
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
                };
                self.write_open(op_str)?;
                self.print_expr(lhs)?;
                self.print_expr(rhs)?;
                self.write_close()
            }
            Expr::Comma { lhs, rhs, .. } => {
                self.write_open(",")?;
                self.print_expr(lhs)?;
                self.print_expr(rhs)?;
                self.write_close()
            }
            Expr::StmtExpr(stmt, _) => {
                self.write_open("stmt-expr")?;
                self.print_compound_stmt(stmt)?;
                self.write_close()
            }
        }
    }

    /// 型名を出力
    fn print_type_name(&mut self, type_name: &TypeName) -> Result<()> {
        self.write_open("type-name")?;
        self.print_decl_specs(&type_name.specs)?;
        if let Some(decl) = &type_name.declarator {
            self.print_abstract_declarator(decl)?;
        }
        self.write_close()?;
        Ok(())
    }

    /// 抽象宣言子を出力
    fn print_abstract_declarator(&mut self, decl: &AbstractDeclarator) -> Result<()> {
        self.write_open("abstract-declarator")?;
        for derived in &decl.derived {
            self.print_derived_decl(derived)?;
        }
        self.write_close()?;
        Ok(())
    }

    // ==================== ヘルパー ====================

    fn write_open(&mut self, name: &str) -> Result<()> {
        if self.pretty && self.indent > 0 {
            writeln!(self.writer)?;
            for _ in 0..self.indent {
                write!(self.writer, "  ")?;
            }
        }
        write!(self.writer, "({}", name)?;
        self.indent += 1;
        Ok(())
    }

    fn write_close(&mut self) -> Result<()> {
        self.indent -= 1;
        write!(self.writer, ")")?;
        Ok(())
    }

    fn write_atom(&mut self, name: &str) -> Result<()> {
        if self.pretty {
            writeln!(self.writer)?;
            for _ in 0..self.indent {
                write!(self.writer, "  ")?;
            }
        } else {
            write!(self.writer, " ")?;
        }
        write!(self.writer, "{}", name)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Parser;
    use crate::preprocessor::{PPConfig, Preprocessor};
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn sexp_str(code: &str) -> String {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(code.as_bytes()).unwrap();

        let mut pp = Preprocessor::new(PPConfig::default());
        pp.process_file(file.path()).unwrap();

        let mut parser = Parser::new(&mut pp).unwrap();
        let tu = parser.parse().unwrap();

        let mut output = Vec::new();
        {
            let mut printer = SexpPrinter::new(&mut output, pp.interner());
            printer.set_pretty(false);
            printer.print_translation_unit(&tu).unwrap();
        }

        String::from_utf8(output).unwrap()
    }

    #[test]
    fn test_simple_sexp() {
        let s = sexp_str("int x;");
        assert!(s.contains("(translation-unit"));
        assert!(s.contains("(declaration"));
        assert!(s.contains("int"));
    }

    #[test]
    fn test_function_sexp() {
        let s = sexp_str("int main(void) { return 0; }");
        assert!(s.contains("(function-def"));
        assert!(s.contains("(return"));
    }
}
