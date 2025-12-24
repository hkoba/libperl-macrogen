//! C言語パーサー
//!
//! tinyccのparser部分に相当。再帰下降パーサーで実装。

use std::collections::HashSet;

use crate::ast::*;
use crate::error::{CompileError, ParseError, Result};
use crate::intern::InternedStr;
use crate::preprocessor::Preprocessor;
use crate::token::{Token, TokenKind};

/// パーサー
pub struct Parser<'a> {
    pp: &'a mut Preprocessor,
    current: Token,
    /// typedef名のセット
    typedefs: HashSet<InternedStr>,
    /// キーワードのインターン済みID
    kw: Keywords,
}

/// キーワードのインターン済みID
struct Keywords {
    // ストレージクラス
    kw_auto: InternedStr,
    kw_extern: InternedStr,
    kw_register: InternedStr,
    kw_static: InternedStr,
    kw_typedef: InternedStr,
    // 型指定子
    kw_void: InternedStr,
    kw_char: InternedStr,
    kw_short: InternedStr,
    kw_int: InternedStr,
    kw_long: InternedStr,
    kw_float: InternedStr,
    kw_double: InternedStr,
    kw_signed: InternedStr,
    kw_unsigned: InternedStr,
    kw_bool: InternedStr,
    kw_complex: InternedStr,
    // 型修飾子
    kw_const: InternedStr,
    kw_volatile: InternedStr,
    kw_restrict: InternedStr,
    kw_atomic: InternedStr,
    // 構造体・共用体・列挙
    kw_struct: InternedStr,
    kw_union: InternedStr,
    kw_enum: InternedStr,
    // 制御フロー
    kw_if: InternedStr,
    kw_else: InternedStr,
    kw_switch: InternedStr,
    kw_case: InternedStr,
    kw_default: InternedStr,
    kw_while: InternedStr,
    kw_do: InternedStr,
    kw_for: InternedStr,
    kw_goto: InternedStr,
    kw_continue: InternedStr,
    kw_break: InternedStr,
    kw_return: InternedStr,
    // その他
    kw_inline: InternedStr,
    kw_sizeof: InternedStr,
    kw_alignof: InternedStr,
}

impl Keywords {
    fn new(interner: &mut crate::intern::StringInterner) -> Self {
        Self {
            kw_auto: interner.intern("auto"),
            kw_extern: interner.intern("extern"),
            kw_register: interner.intern("register"),
            kw_static: interner.intern("static"),
            kw_typedef: interner.intern("typedef"),
            kw_void: interner.intern("void"),
            kw_char: interner.intern("char"),
            kw_short: interner.intern("short"),
            kw_int: interner.intern("int"),
            kw_long: interner.intern("long"),
            kw_float: interner.intern("float"),
            kw_double: interner.intern("double"),
            kw_signed: interner.intern("signed"),
            kw_unsigned: interner.intern("unsigned"),
            kw_bool: interner.intern("_Bool"),
            kw_complex: interner.intern("_Complex"),
            kw_const: interner.intern("const"),
            kw_volatile: interner.intern("volatile"),
            kw_restrict: interner.intern("restrict"),
            kw_atomic: interner.intern("_Atomic"),
            kw_struct: interner.intern("struct"),
            kw_union: interner.intern("union"),
            kw_enum: interner.intern("enum"),
            kw_if: interner.intern("if"),
            kw_else: interner.intern("else"),
            kw_switch: interner.intern("switch"),
            kw_case: interner.intern("case"),
            kw_default: interner.intern("default"),
            kw_while: interner.intern("while"),
            kw_do: interner.intern("do"),
            kw_for: interner.intern("for"),
            kw_goto: interner.intern("goto"),
            kw_continue: interner.intern("continue"),
            kw_break: interner.intern("break"),
            kw_return: interner.intern("return"),
            kw_inline: interner.intern("inline"),
            kw_sizeof: interner.intern("sizeof"),
            kw_alignof: interner.intern("_Alignof"),
        }
    }
}

impl<'a> Parser<'a> {
    /// 新しいパーサーを作成
    pub fn new(pp: &'a mut Preprocessor) -> Result<Self> {
        let kw = Keywords::new(pp.interner_mut());
        let current = pp.next_token()?;
        Ok(Self {
            pp,
            current,
            typedefs: HashSet::new(),
            kw,
        })
    }

    /// 翻訳単位をパース
    pub fn parse(&mut self) -> Result<TranslationUnit> {
        let mut decls = Vec::new();

        while !self.is_eof() {
            let decl = self.parse_external_decl()?;
            decls.push(decl);
        }

        Ok(TranslationUnit { decls })
    }

    /// 外部宣言をパース
    fn parse_external_decl(&mut self) -> Result<ExternalDecl> {
        let comments = self.current.leading_comments.clone();
        let loc = self.current.loc.clone();

        // 宣言指定子をパース
        let specs = self.parse_decl_specs()?;

        // ; のみの場合（構造体宣言など）
        if self.check(&TokenKind::Semi) {
            self.advance()?;
            return Ok(ExternalDecl::Declaration(Declaration {
                specs,
                declarators: Vec::new(),
                loc,
                comments,
            }));
        }

        // 宣言子をパース
        let declarator = self.parse_declarator()?;

        // 関数定義かどうかを判定
        // 関数定義: 宣言子の後に { が来る
        if self.check(&TokenKind::LBrace) {
            let body = self.parse_compound_stmt()?;
            return Ok(ExternalDecl::FunctionDef(FunctionDef {
                specs,
                declarator,
                body,
                loc,
                comments,
            }));
        }

        // 宣言の続きをパース
        let mut declarators = Vec::new();

        // 最初の宣言子（初期化子あり）
        let init = if self.check(&TokenKind::Eq) {
            self.advance()?;
            Some(self.parse_initializer()?)
        } else {
            None
        };
        declarators.push(InitDeclarator { declarator, init });

        // 追加の宣言子
        while self.check(&TokenKind::Comma) {
            self.advance()?;
            let declarator = self.parse_declarator()?;
            let init = if self.check(&TokenKind::Eq) {
                self.advance()?;
                Some(self.parse_initializer()?)
            } else {
                None
            };
            declarators.push(InitDeclarator { declarator, init });
        }

        self.expect(&TokenKind::Semi)?;

        // typedef の場合、名前を登録
        if specs.storage == Some(StorageClass::Typedef) {
            for d in &declarators {
                if let Some(name) = d.declarator.name {
                    self.typedefs.insert(name);
                }
            }
        }

        Ok(ExternalDecl::Declaration(Declaration {
            specs,
            declarators,
            loc,
            comments,
        }))
    }

    /// 宣言指定子をパース
    fn parse_decl_specs(&mut self) -> Result<DeclSpecs> {
        let mut specs = DeclSpecs::default();

        loop {
            if let Some(id) = self.current_ident() {
                if id == self.kw.kw_typedef {
                    specs.storage = Some(StorageClass::Typedef);
                    self.advance()?;
                } else if id == self.kw.kw_extern {
                    specs.storage = Some(StorageClass::Extern);
                    self.advance()?;
                } else if id == self.kw.kw_static {
                    specs.storage = Some(StorageClass::Static);
                    self.advance()?;
                } else if id == self.kw.kw_auto {
                    specs.storage = Some(StorageClass::Auto);
                    self.advance()?;
                } else if id == self.kw.kw_register {
                    specs.storage = Some(StorageClass::Register);
                    self.advance()?;
                } else if id == self.kw.kw_inline {
                    specs.is_inline = true;
                    self.advance()?;
                } else if id == self.kw.kw_const {
                    specs.qualifiers.is_const = true;
                    self.advance()?;
                } else if id == self.kw.kw_volatile {
                    specs.qualifiers.is_volatile = true;
                    self.advance()?;
                } else if id == self.kw.kw_restrict {
                    specs.qualifiers.is_restrict = true;
                    self.advance()?;
                } else if id == self.kw.kw_atomic {
                    specs.qualifiers.is_atomic = true;
                    self.advance()?;
                } else if id == self.kw.kw_void {
                    specs.type_specs.push(TypeSpec::Void);
                    self.advance()?;
                } else if id == self.kw.kw_char {
                    specs.type_specs.push(TypeSpec::Char);
                    self.advance()?;
                } else if id == self.kw.kw_short {
                    specs.type_specs.push(TypeSpec::Short);
                    self.advance()?;
                } else if id == self.kw.kw_int {
                    specs.type_specs.push(TypeSpec::Int);
                    self.advance()?;
                } else if id == self.kw.kw_long {
                    specs.type_specs.push(TypeSpec::Long);
                    self.advance()?;
                } else if id == self.kw.kw_float {
                    specs.type_specs.push(TypeSpec::Float);
                    self.advance()?;
                } else if id == self.kw.kw_double {
                    specs.type_specs.push(TypeSpec::Double);
                    self.advance()?;
                } else if id == self.kw.kw_signed {
                    specs.type_specs.push(TypeSpec::Signed);
                    self.advance()?;
                } else if id == self.kw.kw_unsigned {
                    specs.type_specs.push(TypeSpec::Unsigned);
                    self.advance()?;
                } else if id == self.kw.kw_bool {
                    specs.type_specs.push(TypeSpec::Bool);
                    self.advance()?;
                } else if id == self.kw.kw_complex {
                    specs.type_specs.push(TypeSpec::Complex);
                    self.advance()?;
                } else if id == self.kw.kw_struct {
                    specs.type_specs.push(self.parse_struct_or_union(true)?);
                } else if id == self.kw.kw_union {
                    specs.type_specs.push(self.parse_struct_or_union(false)?);
                } else if id == self.kw.kw_enum {
                    specs.type_specs.push(self.parse_enum()?);
                } else if self.typedefs.contains(&id) {
                    specs.type_specs.push(TypeSpec::TypedefName(id));
                    self.advance()?;
                } else {
                    break;
                }
            } else {
                break;
            }
        }

        Ok(specs)
    }

    /// 構造体/共用体をパース
    fn parse_struct_or_union(&mut self, is_struct: bool) -> Result<TypeSpec> {
        let loc = self.current.loc.clone();
        self.advance()?; // struct/union

        // 名前（オプション）
        let name = self.current_ident();
        if name.is_some() {
            self.advance()?;
        }

        // メンバーリスト（オプション）
        let members = if self.check(&TokenKind::LBrace) {
            self.advance()?;
            let mut members = Vec::new();
            while !self.check(&TokenKind::RBrace) {
                members.push(self.parse_struct_member()?);
            }
            self.expect(&TokenKind::RBrace)?;
            Some(members)
        } else {
            None
        };

        let spec = StructSpec { name, members, loc };
        if is_struct {
            Ok(TypeSpec::Struct(spec))
        } else {
            Ok(TypeSpec::Union(spec))
        }
    }

    /// 構造体メンバーをパース
    fn parse_struct_member(&mut self) -> Result<StructMember> {
        let specs = self.parse_decl_specs()?;
        let mut declarators = Vec::new();

        loop {
            let declarator = if self.check(&TokenKind::Colon) {
                None
            } else if self.check(&TokenKind::Semi) {
                None
            } else {
                Some(self.parse_declarator()?)
            };

            let bitfield = if self.check(&TokenKind::Colon) {
                self.advance()?;
                Some(Box::new(self.parse_conditional_expr()?))
            } else {
                None
            };

            declarators.push(StructDeclarator { declarator, bitfield });

            if !self.check(&TokenKind::Comma) {
                break;
            }
            self.advance()?;
        }

        self.expect(&TokenKind::Semi)?;

        Ok(StructMember { specs, declarators })
    }

    /// 列挙型をパース
    fn parse_enum(&mut self) -> Result<TypeSpec> {
        let loc = self.current.loc.clone();
        self.advance()?; // enum

        // 名前（オプション）
        let name = self.current_ident();
        if name.is_some() {
            self.advance()?;
        }

        // 列挙子リスト（オプション）
        let enumerators = if self.check(&TokenKind::LBrace) {
            self.advance()?;
            let mut enums = Vec::new();
            while !self.check(&TokenKind::RBrace) {
                let eloc = self.current.loc.clone();
                let ename = self.expect_ident()?;
                let value = if self.check(&TokenKind::Eq) {
                    self.advance()?;
                    Some(Box::new(self.parse_conditional_expr()?))
                } else {
                    None
                };
                enums.push(Enumerator {
                    name: ename,
                    value,
                    loc: eloc,
                });
                if !self.check(&TokenKind::Comma) {
                    break;
                }
                self.advance()?;
            }
            self.expect(&TokenKind::RBrace)?;
            Some(enums)
        } else {
            None
        };

        Ok(TypeSpec::Enum(EnumSpec {
            name,
            enumerators,
            loc,
        }))
    }

    /// 宣言子をパース
    fn parse_declarator(&mut self) -> Result<Declarator> {
        let loc = self.current.loc.clone();
        let mut derived = Vec::new();

        // ポインタ
        while self.check(&TokenKind::Star) {
            self.advance()?;
            let qualifiers = self.parse_type_qualifiers()?;
            derived.push(DerivedDecl::Pointer(qualifiers));
        }

        // 直接宣言子
        let (name, inner_derived) = self.parse_direct_declarator()?;
        derived.extend(inner_derived);

        Ok(Declarator {
            name,
            derived,
            loc,
        })
    }

    /// 直接宣言子をパース
    fn parse_direct_declarator(&mut self) -> Result<(Option<InternedStr>, Vec<DerivedDecl>)> {
        let mut derived = Vec::new();

        // 識別子または ( declarator )
        let name = if self.check(&TokenKind::LParen) {
            self.advance()?;
            let inner = self.parse_declarator()?;
            self.expect(&TokenKind::RParen)?;
            // 内側の派生型を先頭に追加
            derived = inner.derived;
            inner.name
        } else if let Some(id) = self.current_ident() {
            // キーワードでない識別子のみ
            if !self.is_keyword(id) {
                self.advance()?;
                Some(id)
            } else {
                None
            }
        } else {
            None
        };

        // 配列・関数の後置修飾
        loop {
            if self.check(&TokenKind::LBracket) {
                derived.push(self.parse_array_declarator()?);
            } else if self.check(&TokenKind::LParen) {
                derived.push(self.parse_function_declarator()?);
            } else {
                break;
            }
        }

        Ok((name, derived))
    }

    /// 配列宣言子をパース
    fn parse_array_declarator(&mut self) -> Result<DerivedDecl> {
        self.advance()?; // [

        let mut qualifiers = TypeQualifiers::default();
        let mut is_static = false;
        let mut is_vla = false;

        // static と型修飾子
        loop {
            if let Some(id) = self.current_ident() {
                if id == self.kw.kw_static {
                    is_static = true;
                    self.advance()?;
                } else if id == self.kw.kw_const {
                    qualifiers.is_const = true;
                    self.advance()?;
                } else if id == self.kw.kw_volatile {
                    qualifiers.is_volatile = true;
                    self.advance()?;
                } else if id == self.kw.kw_restrict {
                    qualifiers.is_restrict = true;
                    self.advance()?;
                } else {
                    break;
                }
            } else {
                break;
            }
        }

        // サイズ式
        let size = if self.check(&TokenKind::RBracket) {
            None
        } else if self.check(&TokenKind::Star) {
            is_vla = true;
            self.advance()?;
            None
        } else {
            Some(Box::new(self.parse_assignment_expr()?))
        };

        self.expect(&TokenKind::RBracket)?;

        Ok(DerivedDecl::Array(ArrayDecl {
            size,
            qualifiers,
            is_static,
            is_vla,
        }))
    }

    /// 関数宣言子をパース
    fn parse_function_declarator(&mut self) -> Result<DerivedDecl> {
        self.advance()?; // (

        if self.check(&TokenKind::RParen) {
            self.advance()?;
            return Ok(DerivedDecl::Function(ParamList {
                params: Vec::new(),
                is_variadic: false,
            }));
        }

        let mut params = Vec::new();
        let mut is_variadic = false;

        loop {
            if self.check(&TokenKind::Ellipsis) {
                is_variadic = true;
                self.advance()?;
                break;
            }

            let loc = self.current.loc.clone();
            let specs = self.parse_decl_specs()?;
            let declarator = if self.check(&TokenKind::Comma) || self.check(&TokenKind::RParen) {
                None
            } else {
                Some(self.parse_declarator()?)
            };

            params.push(ParamDecl {
                specs,
                declarator,
                loc,
            });

            if !self.check(&TokenKind::Comma) {
                break;
            }
            self.advance()?;
        }

        self.expect(&TokenKind::RParen)?;

        Ok(DerivedDecl::Function(ParamList { params, is_variadic }))
    }

    /// 型修飾子をパース
    fn parse_type_qualifiers(&mut self) -> Result<TypeQualifiers> {
        let mut qualifiers = TypeQualifiers::default();

        loop {
            if let Some(id) = self.current_ident() {
                if id == self.kw.kw_const {
                    qualifiers.is_const = true;
                    self.advance()?;
                } else if id == self.kw.kw_volatile {
                    qualifiers.is_volatile = true;
                    self.advance()?;
                } else if id == self.kw.kw_restrict {
                    qualifiers.is_restrict = true;
                    self.advance()?;
                } else if id == self.kw.kw_atomic {
                    qualifiers.is_atomic = true;
                    self.advance()?;
                } else {
                    break;
                }
            } else {
                break;
            }
        }

        Ok(qualifiers)
    }

    /// 初期化子をパース
    fn parse_initializer(&mut self) -> Result<Initializer> {
        if self.check(&TokenKind::LBrace) {
            self.advance()?;
            let mut items = Vec::new();

            while !self.check(&TokenKind::RBrace) {
                let designation = self.parse_designation()?;
                let init = self.parse_initializer()?;
                items.push(InitializerItem { designation, init });

                if !self.check(&TokenKind::Comma) {
                    break;
                }
                self.advance()?;
            }
            self.expect(&TokenKind::RBrace)?;

            Ok(Initializer::List(items))
        } else {
            Ok(Initializer::Expr(Box::new(self.parse_assignment_expr()?)))
        }
    }

    /// 指示子列をパース
    fn parse_designation(&mut self) -> Result<Vec<Designator>> {
        let mut designators = Vec::new();

        loop {
            if self.check(&TokenKind::LBracket) {
                self.advance()?;
                let index = self.parse_conditional_expr()?;
                self.expect(&TokenKind::RBracket)?;
                designators.push(Designator::Index(Box::new(index)));
            } else if self.check(&TokenKind::Dot) {
                self.advance()?;
                let name = self.expect_ident()?;
                designators.push(Designator::Member(name));
            } else {
                break;
            }
        }

        if !designators.is_empty() {
            self.expect(&TokenKind::Eq)?;
        }

        Ok(designators)
    }

    /// 型名をパース
    fn parse_type_name(&mut self) -> Result<TypeName> {
        let specs = self.parse_decl_specs()?;
        let declarator = if self.check(&TokenKind::RParen) {
            None
        } else {
            Some(self.parse_abstract_declarator()?)
        };
        Ok(TypeName { specs, declarator })
    }

    /// 抽象宣言子をパース
    fn parse_abstract_declarator(&mut self) -> Result<AbstractDeclarator> {
        let mut derived = Vec::new();

        // ポインタ
        while self.check(&TokenKind::Star) {
            self.advance()?;
            let qualifiers = self.parse_type_qualifiers()?;
            derived.push(DerivedDecl::Pointer(qualifiers));
        }

        // 直接抽象宣言子
        if self.check(&TokenKind::LParen) {
            // ( abstract-declarator ) または ( parameter-list )
            // 先読みで判定が必要だが、簡略化のためここでは単純に処理
            self.advance()?;
            if !self.check(&TokenKind::RParen) && !self.is_type_start() {
                let inner = self.parse_abstract_declarator()?;
                self.expect(&TokenKind::RParen)?;
                derived.extend(inner.derived);
            } else {
                // パラメータリストとして処理
                let params = self.parse_param_list_inner()?;
                derived.push(DerivedDecl::Function(params));
            }
        }

        // 配列・関数の後置修飾
        loop {
            if self.check(&TokenKind::LBracket) {
                derived.push(self.parse_array_declarator()?);
            } else if self.check(&TokenKind::LParen) {
                derived.push(self.parse_function_declarator()?);
            } else {
                break;
            }
        }

        Ok(AbstractDeclarator { derived })
    }

    /// パラメータリストの内部をパース（RParen消費済み前提ではない）
    fn parse_param_list_inner(&mut self) -> Result<ParamList> {
        if self.check(&TokenKind::RParen) {
            self.advance()?;
            return Ok(ParamList {
                params: Vec::new(),
                is_variadic: false,
            });
        }

        let mut params = Vec::new();
        let mut is_variadic = false;

        loop {
            if self.check(&TokenKind::Ellipsis) {
                is_variadic = true;
                self.advance()?;
                break;
            }

            let loc = self.current.loc.clone();
            let specs = self.parse_decl_specs()?;
            let declarator = if self.check(&TokenKind::Comma) || self.check(&TokenKind::RParen) {
                None
            } else if self.check(&TokenKind::Star) || self.check(&TokenKind::LParen) || self.check(&TokenKind::LBracket) {
                Some(self.parse_declarator()?)
            } else if let Some(id) = self.current_ident() {
                if !self.is_keyword(id) && !self.typedefs.contains(&id) {
                    Some(self.parse_declarator()?)
                } else {
                    None
                }
            } else {
                None
            };

            params.push(ParamDecl {
                specs,
                declarator,
                loc,
            });

            if !self.check(&TokenKind::Comma) {
                break;
            }
            self.advance()?;
        }

        self.expect(&TokenKind::RParen)?;

        Ok(ParamList { params, is_variadic })
    }

    // ==================== 文のパース ====================

    /// 複合文をパース
    fn parse_compound_stmt(&mut self) -> Result<CompoundStmt> {
        let loc = self.current.loc.clone();
        self.expect(&TokenKind::LBrace)?;

        let mut items = Vec::new();
        while !self.check(&TokenKind::RBrace) {
            items.push(self.parse_block_item()?);
        }

        self.expect(&TokenKind::RBrace)?;

        Ok(CompoundStmt { items, loc })
    }

    /// ブロック項目をパース
    fn parse_block_item(&mut self) -> Result<BlockItem> {
        if self.is_declaration_start() {
            Ok(BlockItem::Decl(self.parse_declaration()?))
        } else {
            Ok(BlockItem::Stmt(self.parse_stmt()?))
        }
    }

    /// 宣言をパース
    fn parse_declaration(&mut self) -> Result<Declaration> {
        let comments = self.current.leading_comments.clone();
        let loc = self.current.loc.clone();
        let specs = self.parse_decl_specs()?;

        if self.check(&TokenKind::Semi) {
            self.advance()?;
            return Ok(Declaration {
                specs,
                declarators: Vec::new(),
                loc,
                comments,
            });
        }

        let mut declarators = Vec::new();

        loop {
            let declarator = self.parse_declarator()?;
            let init = if self.check(&TokenKind::Eq) {
                self.advance()?;
                Some(self.parse_initializer()?)
            } else {
                None
            };
            declarators.push(InitDeclarator { declarator, init });

            if !self.check(&TokenKind::Comma) {
                break;
            }
            self.advance()?;
        }

        self.expect(&TokenKind::Semi)?;

        // typedef の場合、名前を登録
        if specs.storage == Some(StorageClass::Typedef) {
            for d in &declarators {
                if let Some(name) = d.declarator.name {
                    self.typedefs.insert(name);
                }
            }
        }

        Ok(Declaration {
            specs,
            declarators,
            loc,
            comments,
        })
    }

    /// 文をパース
    fn parse_stmt(&mut self) -> Result<Stmt> {
        let loc = self.current.loc.clone();

        // ラベル文のチェック
        if let Some(id) = self.current_ident() {
            if id == self.kw.kw_case {
                return self.parse_case_stmt();
            } else if id == self.kw.kw_default {
                return self.parse_default_stmt();
            }

            // 通常のラベル（identifier :）
            // 2トークン先読みが必要だが、簡略化
        }

        if self.check(&TokenKind::LBrace) {
            return Ok(Stmt::Compound(self.parse_compound_stmt()?));
        }

        if let Some(id) = self.current_ident() {
            if id == self.kw.kw_if {
                return self.parse_if_stmt();
            } else if id == self.kw.kw_switch {
                return self.parse_switch_stmt();
            } else if id == self.kw.kw_while {
                return self.parse_while_stmt();
            } else if id == self.kw.kw_do {
                return self.parse_do_while_stmt();
            } else if id == self.kw.kw_for {
                return self.parse_for_stmt();
            } else if id == self.kw.kw_goto {
                self.advance()?;
                let name = self.expect_ident()?;
                self.expect(&TokenKind::Semi)?;
                return Ok(Stmt::Goto(name, loc));
            } else if id == self.kw.kw_continue {
                self.advance()?;
                self.expect(&TokenKind::Semi)?;
                return Ok(Stmt::Continue(loc));
            } else if id == self.kw.kw_break {
                self.advance()?;
                self.expect(&TokenKind::Semi)?;
                return Ok(Stmt::Break(loc));
            } else if id == self.kw.kw_return {
                self.advance()?;
                let expr = if self.check(&TokenKind::Semi) {
                    None
                } else {
                    Some(Box::new(self.parse_expr()?))
                };
                self.expect(&TokenKind::Semi)?;
                return Ok(Stmt::Return(expr, loc));
            }
        }

        // 式文
        if self.check(&TokenKind::Semi) {
            self.advance()?;
            return Ok(Stmt::Expr(None, loc));
        }

        let expr = self.parse_expr()?;

        // ラベルのチェック（識別子 : の場合）
        if self.check(&TokenKind::Colon) {
            if let Expr::Ident(name, _) = expr {
                self.advance()?;
                let stmt = self.parse_stmt()?;
                return Ok(Stmt::Label {
                    name,
                    stmt: Box::new(stmt),
                    loc,
                });
            }
        }

        self.expect(&TokenKind::Semi)?;
        Ok(Stmt::Expr(Some(Box::new(expr)), loc))
    }

    fn parse_if_stmt(&mut self) -> Result<Stmt> {
        let loc = self.current.loc.clone();
        self.advance()?; // if
        self.expect(&TokenKind::LParen)?;
        let cond = Box::new(self.parse_expr()?);
        self.expect(&TokenKind::RParen)?;
        let then_stmt = Box::new(self.parse_stmt()?);

        let else_stmt = if let Some(id) = self.current_ident() {
            if id == self.kw.kw_else {
                self.advance()?;
                Some(Box::new(self.parse_stmt()?))
            } else {
                None
            }
        } else {
            None
        };

        Ok(Stmt::If {
            cond,
            then_stmt,
            else_stmt,
            loc,
        })
    }

    fn parse_switch_stmt(&mut self) -> Result<Stmt> {
        let loc = self.current.loc.clone();
        self.advance()?; // switch
        self.expect(&TokenKind::LParen)?;
        let expr = Box::new(self.parse_expr()?);
        self.expect(&TokenKind::RParen)?;
        let body = Box::new(self.parse_stmt()?);

        Ok(Stmt::Switch { expr, body, loc })
    }

    fn parse_while_stmt(&mut self) -> Result<Stmt> {
        let loc = self.current.loc.clone();
        self.advance()?; // while
        self.expect(&TokenKind::LParen)?;
        let cond = Box::new(self.parse_expr()?);
        self.expect(&TokenKind::RParen)?;
        let body = Box::new(self.parse_stmt()?);

        Ok(Stmt::While { cond, body, loc })
    }

    fn parse_do_while_stmt(&mut self) -> Result<Stmt> {
        let loc = self.current.loc.clone();
        self.advance()?; // do
        let body = Box::new(self.parse_stmt()?);
        self.expect_kw(self.kw.kw_while)?;
        self.expect(&TokenKind::LParen)?;
        let cond = Box::new(self.parse_expr()?);
        self.expect(&TokenKind::RParen)?;
        self.expect(&TokenKind::Semi)?;

        Ok(Stmt::DoWhile { body, cond, loc })
    }

    fn parse_for_stmt(&mut self) -> Result<Stmt> {
        let loc = self.current.loc.clone();
        self.advance()?; // for
        self.expect(&TokenKind::LParen)?;

        // 初期化部
        let init = if self.check(&TokenKind::Semi) {
            self.advance()?;
            None
        } else if self.is_declaration_start() {
            let decl = self.parse_declaration()?;
            Some(ForInit::Decl(decl))
        } else {
            let expr = self.parse_expr()?;
            self.expect(&TokenKind::Semi)?;
            Some(ForInit::Expr(Box::new(expr)))
        };

        // 条件部
        let cond = if self.check(&TokenKind::Semi) {
            None
        } else {
            Some(Box::new(self.parse_expr()?))
        };
        self.expect(&TokenKind::Semi)?;

        // 更新部
        let step = if self.check(&TokenKind::RParen) {
            None
        } else {
            Some(Box::new(self.parse_expr()?))
        };
        self.expect(&TokenKind::RParen)?;

        let body = Box::new(self.parse_stmt()?);

        Ok(Stmt::For {
            init,
            cond,
            step,
            body,
            loc,
        })
    }

    fn parse_case_stmt(&mut self) -> Result<Stmt> {
        let loc = self.current.loc.clone();
        self.advance()?; // case
        let expr = Box::new(self.parse_conditional_expr()?);
        self.expect(&TokenKind::Colon)?;
        let stmt = Box::new(self.parse_stmt()?);

        Ok(Stmt::Case { expr, stmt, loc })
    }

    fn parse_default_stmt(&mut self) -> Result<Stmt> {
        let loc = self.current.loc.clone();
        self.advance()?; // default
        self.expect(&TokenKind::Colon)?;
        let stmt = Box::new(self.parse_stmt()?);

        Ok(Stmt::Default { stmt, loc })
    }

    // ==================== 式のパース ====================

    /// 式をパース（コンマ式を含む）
    fn parse_expr(&mut self) -> Result<Expr> {
        let lhs = self.parse_assignment_expr()?;

        if self.check(&TokenKind::Comma) {
            let loc = self.current.loc.clone();
            self.advance()?;
            let rhs = self.parse_expr()?;
            return Ok(Expr::Comma {
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                loc,
            });
        }

        Ok(lhs)
    }

    /// 代入式をパース
    fn parse_assignment_expr(&mut self) -> Result<Expr> {
        let lhs = self.parse_conditional_expr()?;

        let op = match &self.current.kind {
            TokenKind::Eq => Some(AssignOp::Assign),
            TokenKind::StarEq => Some(AssignOp::MulAssign),
            TokenKind::SlashEq => Some(AssignOp::DivAssign),
            TokenKind::PercentEq => Some(AssignOp::ModAssign),
            TokenKind::PlusEq => Some(AssignOp::AddAssign),
            TokenKind::MinusEq => Some(AssignOp::SubAssign),
            TokenKind::LtLtEq => Some(AssignOp::ShlAssign),
            TokenKind::GtGtEq => Some(AssignOp::ShrAssign),
            TokenKind::AmpEq => Some(AssignOp::AndAssign),
            TokenKind::CaretEq => Some(AssignOp::XorAssign),
            TokenKind::PipeEq => Some(AssignOp::OrAssign),
            _ => None,
        };

        if let Some(op) = op {
            let loc = self.current.loc.clone();
            self.advance()?;
            let rhs = self.parse_assignment_expr()?;
            return Ok(Expr::Assign {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                loc,
            });
        }

        Ok(lhs)
    }

    /// 条件式をパース
    fn parse_conditional_expr(&mut self) -> Result<Expr> {
        let cond = self.parse_logical_or_expr()?;

        if self.check(&TokenKind::Question) {
            let loc = self.current.loc.clone();
            self.advance()?;
            let then_expr = self.parse_expr()?;
            self.expect(&TokenKind::Colon)?;
            let else_expr = self.parse_conditional_expr()?;
            return Ok(Expr::Conditional {
                cond: Box::new(cond),
                then_expr: Box::new(then_expr),
                else_expr: Box::new(else_expr),
                loc,
            });
        }

        Ok(cond)
    }

    /// 論理OR式をパース
    fn parse_logical_or_expr(&mut self) -> Result<Expr> {
        let mut lhs = self.parse_logical_and_expr()?;

        while self.check(&TokenKind::PipePipe) {
            let loc = self.current.loc.clone();
            self.advance()?;
            let rhs = self.parse_logical_and_expr()?;
            lhs = Expr::Binary {
                op: BinOp::LogOr,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                loc,
            };
        }

        Ok(lhs)
    }

    /// 論理AND式をパース
    fn parse_logical_and_expr(&mut self) -> Result<Expr> {
        let mut lhs = self.parse_bitwise_or_expr()?;

        while self.check(&TokenKind::AmpAmp) {
            let loc = self.current.loc.clone();
            self.advance()?;
            let rhs = self.parse_bitwise_or_expr()?;
            lhs = Expr::Binary {
                op: BinOp::LogAnd,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                loc,
            };
        }

        Ok(lhs)
    }

    /// ビットOR式をパース
    fn parse_bitwise_or_expr(&mut self) -> Result<Expr> {
        let mut lhs = self.parse_bitwise_xor_expr()?;

        while self.check(&TokenKind::Pipe) {
            let loc = self.current.loc.clone();
            self.advance()?;
            let rhs = self.parse_bitwise_xor_expr()?;
            lhs = Expr::Binary {
                op: BinOp::BitOr,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                loc,
            };
        }

        Ok(lhs)
    }

    /// ビットXOR式をパース
    fn parse_bitwise_xor_expr(&mut self) -> Result<Expr> {
        let mut lhs = self.parse_bitwise_and_expr()?;

        while self.check(&TokenKind::Caret) {
            let loc = self.current.loc.clone();
            self.advance()?;
            let rhs = self.parse_bitwise_and_expr()?;
            lhs = Expr::Binary {
                op: BinOp::BitXor,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                loc,
            };
        }

        Ok(lhs)
    }

    /// ビットAND式をパース
    fn parse_bitwise_and_expr(&mut self) -> Result<Expr> {
        let mut lhs = self.parse_equality_expr()?;

        while self.check(&TokenKind::Amp) {
            let loc = self.current.loc.clone();
            self.advance()?;
            let rhs = self.parse_equality_expr()?;
            lhs = Expr::Binary {
                op: BinOp::BitAnd,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                loc,
            };
        }

        Ok(lhs)
    }

    /// 等価式をパース
    fn parse_equality_expr(&mut self) -> Result<Expr> {
        let mut lhs = self.parse_relational_expr()?;

        loop {
            let op = match &self.current.kind {
                TokenKind::EqEq => BinOp::Eq,
                TokenKind::BangEq => BinOp::Ne,
                _ => break,
            };
            let loc = self.current.loc.clone();
            self.advance()?;
            let rhs = self.parse_relational_expr()?;
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                loc,
            };
        }

        Ok(lhs)
    }

    /// 関係式をパース
    fn parse_relational_expr(&mut self) -> Result<Expr> {
        let mut lhs = self.parse_shift_expr()?;

        loop {
            let op = match &self.current.kind {
                TokenKind::Lt => BinOp::Lt,
                TokenKind::Gt => BinOp::Gt,
                TokenKind::LtEq => BinOp::Le,
                TokenKind::GtEq => BinOp::Ge,
                _ => break,
            };
            let loc = self.current.loc.clone();
            self.advance()?;
            let rhs = self.parse_shift_expr()?;
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                loc,
            };
        }

        Ok(lhs)
    }

    /// シフト式をパース
    fn parse_shift_expr(&mut self) -> Result<Expr> {
        let mut lhs = self.parse_additive_expr()?;

        loop {
            let op = match &self.current.kind {
                TokenKind::LtLt => BinOp::Shl,
                TokenKind::GtGt => BinOp::Shr,
                _ => break,
            };
            let loc = self.current.loc.clone();
            self.advance()?;
            let rhs = self.parse_additive_expr()?;
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                loc,
            };
        }

        Ok(lhs)
    }

    /// 加減式をパース
    fn parse_additive_expr(&mut self) -> Result<Expr> {
        let mut lhs = self.parse_multiplicative_expr()?;

        loop {
            let op = match &self.current.kind {
                TokenKind::Plus => BinOp::Add,
                TokenKind::Minus => BinOp::Sub,
                _ => break,
            };
            let loc = self.current.loc.clone();
            self.advance()?;
            let rhs = self.parse_multiplicative_expr()?;
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                loc,
            };
        }

        Ok(lhs)
    }

    /// 乗除式をパース
    fn parse_multiplicative_expr(&mut self) -> Result<Expr> {
        let mut lhs = self.parse_cast_expr()?;

        loop {
            let op = match &self.current.kind {
                TokenKind::Star => BinOp::Mul,
                TokenKind::Slash => BinOp::Div,
                TokenKind::Percent => BinOp::Mod,
                _ => break,
            };
            let loc = self.current.loc.clone();
            self.advance()?;
            let rhs = self.parse_cast_expr()?;
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                loc,
            };
        }

        Ok(lhs)
    }

    /// キャスト式をパース
    fn parse_cast_expr(&mut self) -> Result<Expr> {
        // ( type-name ) cast-expression
        if self.check(&TokenKind::LParen) && self.is_type_start_after_lparen() {
            let loc = self.current.loc.clone();
            self.advance()?; // (
            let type_name = self.parse_type_name()?;
            self.expect(&TokenKind::RParen)?;

            // 複合リテラルのチェック
            if self.check(&TokenKind::LBrace) {
                self.advance()?;
                let mut items = Vec::new();
                while !self.check(&TokenKind::RBrace) {
                    let designation = self.parse_designation()?;
                    let init = self.parse_initializer()?;
                    items.push(InitializerItem { designation, init });
                    if !self.check(&TokenKind::Comma) {
                        break;
                    }
                    self.advance()?;
                }
                self.expect(&TokenKind::RBrace)?;
                return Ok(Expr::CompoundLit {
                    type_name: Box::new(type_name),
                    init: items,
                    loc,
                });
            }

            let expr = self.parse_cast_expr()?;
            return Ok(Expr::Cast {
                type_name: Box::new(type_name),
                expr: Box::new(expr),
                loc,
            });
        }

        self.parse_unary_expr()
    }

    /// 単項式をパース
    fn parse_unary_expr(&mut self) -> Result<Expr> {
        let loc = self.current.loc.clone();

        match &self.current.kind {
            TokenKind::PlusPlus => {
                self.advance()?;
                let expr = self.parse_unary_expr()?;
                Ok(Expr::PreInc(Box::new(expr), loc))
            }
            TokenKind::MinusMinus => {
                self.advance()?;
                let expr = self.parse_unary_expr()?;
                Ok(Expr::PreDec(Box::new(expr), loc))
            }
            TokenKind::Amp => {
                self.advance()?;
                let expr = self.parse_cast_expr()?;
                Ok(Expr::AddrOf(Box::new(expr), loc))
            }
            TokenKind::Star => {
                self.advance()?;
                let expr = self.parse_cast_expr()?;
                Ok(Expr::Deref(Box::new(expr), loc))
            }
            TokenKind::Plus => {
                self.advance()?;
                let expr = self.parse_cast_expr()?;
                Ok(Expr::UnaryPlus(Box::new(expr), loc))
            }
            TokenKind::Minus => {
                self.advance()?;
                let expr = self.parse_cast_expr()?;
                Ok(Expr::UnaryMinus(Box::new(expr), loc))
            }
            TokenKind::Tilde => {
                self.advance()?;
                let expr = self.parse_cast_expr()?;
                Ok(Expr::BitNot(Box::new(expr), loc))
            }
            TokenKind::Bang => {
                self.advance()?;
                let expr = self.parse_cast_expr()?;
                Ok(Expr::LogNot(Box::new(expr), loc))
            }
            TokenKind::Ident(id) if *id == self.kw.kw_sizeof => {
                self.advance()?;
                if self.check(&TokenKind::LParen) && self.is_type_start_after_lparen() {
                    self.advance()?;
                    let type_name = self.parse_type_name()?;
                    self.expect(&TokenKind::RParen)?;
                    Ok(Expr::SizeofType(Box::new(type_name), loc))
                } else {
                    let expr = self.parse_unary_expr()?;
                    Ok(Expr::Sizeof(Box::new(expr), loc))
                }
            }
            TokenKind::Ident(id) if *id == self.kw.kw_alignof => {
                self.advance()?;
                self.expect(&TokenKind::LParen)?;
                let type_name = self.parse_type_name()?;
                self.expect(&TokenKind::RParen)?;
                Ok(Expr::Alignof(Box::new(type_name), loc))
            }
            _ => self.parse_postfix_expr(),
        }
    }

    /// 後置式をパース
    fn parse_postfix_expr(&mut self) -> Result<Expr> {
        let mut expr = self.parse_primary_expr()?;

        loop {
            let loc = self.current.loc.clone();
            match &self.current.kind {
                TokenKind::LBracket => {
                    self.advance()?;
                    let index = self.parse_expr()?;
                    self.expect(&TokenKind::RBracket)?;
                    expr = Expr::Index {
                        expr: Box::new(expr),
                        index: Box::new(index),
                        loc,
                    };
                }
                TokenKind::LParen => {
                    self.advance()?;
                    let mut args = Vec::new();
                    if !self.check(&TokenKind::RParen) {
                        loop {
                            args.push(self.parse_assignment_expr()?);
                            if !self.check(&TokenKind::Comma) {
                                break;
                            }
                            self.advance()?;
                        }
                    }
                    self.expect(&TokenKind::RParen)?;
                    expr = Expr::Call {
                        func: Box::new(expr),
                        args,
                        loc,
                    };
                }
                TokenKind::Dot => {
                    self.advance()?;
                    let member = self.expect_ident()?;
                    expr = Expr::Member {
                        expr: Box::new(expr),
                        member,
                        loc,
                    };
                }
                TokenKind::Arrow => {
                    self.advance()?;
                    let member = self.expect_ident()?;
                    expr = Expr::PtrMember {
                        expr: Box::new(expr),
                        member,
                        loc,
                    };
                }
                TokenKind::PlusPlus => {
                    self.advance()?;
                    expr = Expr::PostInc(Box::new(expr), loc);
                }
                TokenKind::MinusMinus => {
                    self.advance()?;
                    expr = Expr::PostDec(Box::new(expr), loc);
                }
                _ => break,
            }
        }

        Ok(expr)
    }

    /// 一次式をパース
    fn parse_primary_expr(&mut self) -> Result<Expr> {
        let loc = self.current.loc.clone();

        match &self.current.kind {
            TokenKind::Ident(id) => {
                let id = *id;
                self.advance()?;
                Ok(Expr::Ident(id, loc))
            }
            TokenKind::IntLit(n) => {
                let n = *n;
                self.advance()?;
                Ok(Expr::IntLit(n, loc))
            }
            TokenKind::UIntLit(n) => {
                let n = *n;
                self.advance()?;
                Ok(Expr::UIntLit(n, loc))
            }
            TokenKind::FloatLit(f) => {
                let f = *f;
                self.advance()?;
                Ok(Expr::FloatLit(f, loc))
            }
            TokenKind::CharLit(c) => {
                let c = *c;
                self.advance()?;
                Ok(Expr::CharLit(c, loc))
            }
            TokenKind::StringLit(s) => {
                let mut bytes = s.clone();
                self.advance()?;
                // 連続した文字列リテラルを結合
                while let TokenKind::StringLit(s2) = &self.current.kind {
                    bytes.extend_from_slice(s2);
                    self.advance()?;
                }
                Ok(Expr::StringLit(bytes, loc))
            }
            TokenKind::LParen => {
                self.advance()?;
                let expr = self.parse_expr()?;
                self.expect(&TokenKind::RParen)?;
                Ok(expr)
            }
            _ => Err(CompileError::Parse {
                loc,
                kind: ParseError::UnexpectedToken {
                    expected: "primary expression".to_string(),
                    found: self.current.kind.clone(),
                },
            }),
        }
    }

    // ==================== ユーティリティ ====================

    fn advance(&mut self) -> Result<Token> {
        let old = std::mem::replace(&mut self.current, self.pp.next_token()?);
        Ok(old)
    }

    fn expect(&mut self, kind: &TokenKind) -> Result<Token> {
        if self.check(kind) {
            self.advance()
        } else {
            Err(CompileError::Parse {
                loc: self.current.loc.clone(),
                kind: ParseError::UnexpectedToken {
                    expected: format!("{:?}", kind),
                    found: self.current.kind.clone(),
                },
            })
        }
    }

    fn expect_kw(&mut self, kw: InternedStr) -> Result<Token> {
        if let Some(id) = self.current_ident() {
            if id == kw {
                return self.advance();
            }
        }
        let kw_str = self.pp.interner().get(kw);
        Err(CompileError::Parse {
            loc: self.current.loc.clone(),
            kind: ParseError::UnexpectedToken {
                expected: kw_str.to_string(),
                found: self.current.kind.clone(),
            },
        })
    }

    fn expect_ident(&mut self) -> Result<InternedStr> {
        if let TokenKind::Ident(id) = self.current.kind {
            self.advance()?;
            Ok(id)
        } else {
            Err(CompileError::Parse {
                loc: self.current.loc.clone(),
                kind: ParseError::UnexpectedToken {
                    expected: "identifier".to_string(),
                    found: self.current.kind.clone(),
                },
            })
        }
    }

    fn check(&self, kind: &TokenKind) -> bool {
        std::mem::discriminant(&self.current.kind) == std::mem::discriminant(kind)
    }

    fn is_eof(&self) -> bool {
        matches!(self.current.kind, TokenKind::Eof)
    }

    fn current_ident(&self) -> Option<InternedStr> {
        if let TokenKind::Ident(id) = self.current.kind {
            Some(id)
        } else {
            None
        }
    }

    fn is_keyword(&self, id: InternedStr) -> bool {
        id == self.kw.kw_auto
            || id == self.kw.kw_extern
            || id == self.kw.kw_register
            || id == self.kw.kw_static
            || id == self.kw.kw_typedef
            || id == self.kw.kw_void
            || id == self.kw.kw_char
            || id == self.kw.kw_short
            || id == self.kw.kw_int
            || id == self.kw.kw_long
            || id == self.kw.kw_float
            || id == self.kw.kw_double
            || id == self.kw.kw_signed
            || id == self.kw.kw_unsigned
            || id == self.kw.kw_bool
            || id == self.kw.kw_complex
            || id == self.kw.kw_const
            || id == self.kw.kw_volatile
            || id == self.kw.kw_restrict
            || id == self.kw.kw_atomic
            || id == self.kw.kw_struct
            || id == self.kw.kw_union
            || id == self.kw.kw_enum
            || id == self.kw.kw_if
            || id == self.kw.kw_else
            || id == self.kw.kw_switch
            || id == self.kw.kw_case
            || id == self.kw.kw_default
            || id == self.kw.kw_while
            || id == self.kw.kw_do
            || id == self.kw.kw_for
            || id == self.kw.kw_goto
            || id == self.kw.kw_continue
            || id == self.kw.kw_break
            || id == self.kw.kw_return
            || id == self.kw.kw_inline
            || id == self.kw.kw_sizeof
            || id == self.kw.kw_alignof
    }

    fn is_type_start(&self) -> bool {
        if let Some(id) = self.current_ident() {
            id == self.kw.kw_void
                || id == self.kw.kw_char
                || id == self.kw.kw_short
                || id == self.kw.kw_int
                || id == self.kw.kw_long
                || id == self.kw.kw_float
                || id == self.kw.kw_double
                || id == self.kw.kw_signed
                || id == self.kw.kw_unsigned
                || id == self.kw.kw_bool
                || id == self.kw.kw_complex
                || id == self.kw.kw_const
                || id == self.kw.kw_volatile
                || id == self.kw.kw_restrict
                || id == self.kw.kw_atomic
                || id == self.kw.kw_struct
                || id == self.kw.kw_union
                || id == self.kw.kw_enum
                || self.typedefs.contains(&id)
        } else {
            false
        }
    }

    fn is_type_start_after_lparen(&self) -> bool {
        // 簡易実装：LParen の次が型指定子かどうか
        // 本来は先読みが必要だが、現在のトークンが LParen のときに
        // 次のトークンを見て判定する必要がある
        // ここでは現状の current を見て、その後ろを推測する
        // TODO: より正確な実装
        true // 一旦 true で試す
    }

    fn is_declaration_start(&self) -> bool {
        if let Some(id) = self.current_ident() {
            id == self.kw.kw_typedef
                || id == self.kw.kw_extern
                || id == self.kw.kw_static
                || id == self.kw.kw_auto
                || id == self.kw.kw_register
                || self.is_type_start()
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preprocessor::PPConfig;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn parse_str(code: &str) -> Result<TranslationUnit> {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(code.as_bytes()).unwrap();

        let mut pp = Preprocessor::new(PPConfig::default());
        pp.process_file(file.path())?;

        let mut parser = Parser::new(&mut pp)?;
        parser.parse()
    }

    #[test]
    fn test_simple_function() {
        let tu = parse_str("int main(void) { return 0; }").unwrap();
        assert_eq!(tu.decls.len(), 1);
        assert!(matches!(tu.decls[0], ExternalDecl::FunctionDef(_)));
    }

    #[test]
    fn test_variable_declaration() {
        let tu = parse_str("int x;").unwrap();
        assert_eq!(tu.decls.len(), 1);
        assert!(matches!(tu.decls[0], ExternalDecl::Declaration(_)));
    }

    #[test]
    fn test_struct_declaration() {
        let tu = parse_str("struct Point { int x; int y; };").unwrap();
        assert_eq!(tu.decls.len(), 1);
    }

    #[test]
    fn test_typedef() {
        let tu = parse_str("typedef int INT; INT x;").unwrap();
        assert_eq!(tu.decls.len(), 2);
    }

    #[test]
    fn test_expression() {
        let tu = parse_str("int x = 1 + 2 * 3;").unwrap();
        assert_eq!(tu.decls.len(), 1);
    }
}
