//! C言語パーサー
//!
//! tinyccのparser部分に相当。再帰下降パーサーで実装。

use std::collections::HashSet;

use crate::ast::*;
use crate::error::{CompileError, ParseError, Result};
use crate::intern::{InternedStr, StringInterner};
use crate::preprocessor::Preprocessor;
use crate::token::{Token, TokenKind};
use crate::token_source::{TokenSliceRef, TokenSource};

/// パーサー
///
/// 汎用のトークンソースからC言語をパースする。
/// `S` は `TokenSource` トレイトを実装する任意の型。
pub struct Parser<'a, S: TokenSource> {
    source: &'a mut S,
    current: Token,
    /// typedef名のセット
    typedefs: HashSet<InternedStr>,
}

/// Preprocessor 専用の後方互換コンストラクタ
impl<'a> Parser<'a, Preprocessor> {
    /// 新しいパーサーを作成（Preprocessor専用）
    pub fn new(pp: &'a mut Preprocessor) -> Result<Self> {
        Self::from_source(pp)
    }

    /// ストリーミング形式でパース
    ///
    /// 各外部宣言を順次パースし、結果をコールバックに渡す。
    /// コールバックが `ControlFlow::Break(())` を返すと処理を中断する。
    ///
    /// # Arguments
    /// * `callback` - 各宣言のパース結果、開始位置、パス、およびインターナーを受け取るクロージャ。
    ///   `ControlFlow::Continue(())` を返すと次の宣言を処理、
    ///   `ControlFlow::Break(())` を返すと処理を中断。
    pub fn parse_each<F>(&mut self, mut callback: F)
    where
        F: FnMut(Result<ExternalDecl>, &crate::source::SourceLocation, &std::path::Path, &StringInterner) -> std::ops::ControlFlow<()>,
    {
        while !self.is_eof() {
            let loc = self.current.loc.clone();
            let result = self.parse_external_decl();
            let path = self.source.files().get_path(loc.file_id);
            let interner = self.source.interner();
            if callback(result, &loc, path, interner).is_break() {
                break;
            }
        }
    }
}

/// 汎用のトークンソースに対するパーサー実装
impl<'a, S: TokenSource> Parser<'a, S> {
    /// トークンソースからパーサーを作成
    pub fn from_source(source: &'a mut S) -> Result<Self> {
        let current = source.next_token()?;

        // GCC builtin types を事前登録
        let mut typedefs = HashSet::new();
        typedefs.insert(source.interner_mut().intern("__builtin_va_list"));

        Ok(Self {
            source,
            current,
            typedefs,
        })
    }

    /// トークンソースからパーサーを作成（既存のtypedef情報を引き継ぐ）
    pub fn from_source_with_typedefs(source: &'a mut S, typedefs: HashSet<InternedStr>) -> Result<Self> {
        let current = source.next_token()?;

        Ok(Self {
            source,
            current,
            typedefs,
        })
    }

    /// StringInterner への参照を取得
    pub fn interner(&self) -> &crate::intern::StringInterner {
        self.source.interner()
    }

    /// typedef名のセットを取得
    pub fn typedefs(&self) -> &HashSet<InternedStr> {
        &self.typedefs
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

    /// 式のみをパース
    ///
    /// マクロ本体など、式だけをパースしたい場合に使用
    pub fn parse_expr_only(&mut self) -> Result<Expr> {
        self.parse_expr()
    }

    /// 外部宣言をパース
    fn parse_external_decl(&mut self) -> Result<ExternalDecl> {
        let comments = self.current.leading_comments.clone();
        let loc = self.current.loc.clone();
        let is_target = self.source.is_file_in_target(loc.file_id);

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
                is_target,
            }));
        }

        // 宣言子をパース
        let declarator = self.parse_declarator()?;

        // __attribute__ をスキップ
        self.try_skip_attribute()?;

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
                is_target,
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

        // GCC拡張: 宣言の最後の __attribute__((...)) をスキップ
        self.try_skip_attribute()?;

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
            is_target,
        }))
    }

    /// 宣言指定子をパース
    fn parse_decl_specs(&mut self) -> Result<DeclSpecs> {
        let mut specs = DeclSpecs::default();

        loop {
            match &self.current.kind {
                // GCC拡張: __extension__ は無視（TinyCC方式）
                TokenKind::KwExtension => {
                    self.advance()?;
                    continue;
                }
                // C11/GCC: _Thread_local, __thread は無視（TinyCC方式）
                TokenKind::KwThreadLocal | TokenKind::KwThread => {
                    self.advance()?;
                    continue;
                }
                // ストレージクラス
                TokenKind::KwTypedef => {
                    specs.storage = Some(StorageClass::Typedef);
                    self.advance()?;
                }
                TokenKind::KwExtern => {
                    specs.storage = Some(StorageClass::Extern);
                    self.advance()?;
                }
                TokenKind::KwStatic => {
                    specs.storage = Some(StorageClass::Static);
                    self.advance()?;
                }
                TokenKind::KwAuto => {
                    specs.storage = Some(StorageClass::Auto);
                    self.advance()?;
                }
                TokenKind::KwRegister => {
                    specs.storage = Some(StorageClass::Register);
                    self.advance()?;
                }
                // inline
                TokenKind::KwInline | TokenKind::KwInline2 | TokenKind::KwInline3 => {
                    specs.is_inline = true;
                    self.advance()?;
                }
                // 型修飾子
                TokenKind::KwConst | TokenKind::KwConst2 | TokenKind::KwConst3 => {
                    specs.qualifiers.is_const = true;
                    self.advance()?;
                }
                TokenKind::KwVolatile | TokenKind::KwVolatile2 | TokenKind::KwVolatile3 => {
                    specs.qualifiers.is_volatile = true;
                    self.advance()?;
                }
                TokenKind::KwRestrict | TokenKind::KwRestrict2 | TokenKind::KwRestrict3 => {
                    specs.qualifiers.is_restrict = true;
                    self.advance()?;
                }
                TokenKind::KwAtomic => {
                    specs.qualifiers.is_atomic = true;
                    self.advance()?;
                }
                // 型指定子
                TokenKind::KwVoid => {
                    specs.type_specs.push(TypeSpec::Void);
                    self.advance()?;
                }
                TokenKind::KwChar => {
                    specs.type_specs.push(TypeSpec::Char);
                    self.advance()?;
                }
                TokenKind::KwShort => {
                    specs.type_specs.push(TypeSpec::Short);
                    self.advance()?;
                }
                TokenKind::KwInt => {
                    specs.type_specs.push(TypeSpec::Int);
                    self.advance()?;
                }
                TokenKind::KwLong => {
                    specs.type_specs.push(TypeSpec::Long);
                    self.advance()?;
                }
                TokenKind::KwFloat => {
                    specs.type_specs.push(TypeSpec::Float);
                    self.advance()?;
                }
                TokenKind::KwDouble => {
                    specs.type_specs.push(TypeSpec::Double);
                    self.advance()?;
                }
                TokenKind::KwSigned | TokenKind::KwSigned2 => {
                    specs.type_specs.push(TypeSpec::Signed);
                    self.advance()?;
                }
                TokenKind::KwUnsigned => {
                    specs.type_specs.push(TypeSpec::Unsigned);
                    self.advance()?;
                }
                TokenKind::KwBool | TokenKind::KwBool2 => {
                    specs.type_specs.push(TypeSpec::Bool);
                    self.advance()?;
                }
                TokenKind::KwComplex => {
                    specs.type_specs.push(TypeSpec::Complex);
                    self.advance()?;
                }
                // GCC拡張浮動小数点型
                TokenKind::KwFloat16 => {
                    specs.type_specs.push(TypeSpec::Float16);
                    self.advance()?;
                }
                TokenKind::KwFloat32 => {
                    specs.type_specs.push(TypeSpec::Float32);
                    self.advance()?;
                }
                TokenKind::KwFloat64 => {
                    specs.type_specs.push(TypeSpec::Float64);
                    self.advance()?;
                }
                TokenKind::KwFloat128 => {
                    specs.type_specs.push(TypeSpec::Float128);
                    self.advance()?;
                }
                TokenKind::KwFloat32x => {
                    specs.type_specs.push(TypeSpec::Float32x);
                    self.advance()?;
                }
                TokenKind::KwFloat64x => {
                    specs.type_specs.push(TypeSpec::Float64x);
                    self.advance()?;
                }
                // GCC拡張: 128ビット整数
                TokenKind::KwInt128 => {
                    specs.type_specs.push(TypeSpec::Int128);
                    self.advance()?;
                }
                // typeof
                TokenKind::KwTypeof | TokenKind::KwTypeof2 | TokenKind::KwTypeof3 => {
                    self.advance()?;
                    self.expect(&TokenKind::LParen)?;
                    let expr = self.parse_expr()?;
                    self.expect(&TokenKind::RParen)?;
                    specs.type_specs.push(TypeSpec::TypeofExpr(Box::new(expr)));
                }
                // 構造体・共用体・列挙
                TokenKind::KwStruct => {
                    specs.type_specs.push(self.parse_struct_or_union(true)?);
                }
                TokenKind::KwUnion => {
                    specs.type_specs.push(self.parse_struct_or_union(false)?);
                }
                TokenKind::KwEnum => {
                    specs.type_specs.push(self.parse_enum()?);
                }
                // GCC拡張: __attribute__((...)) をスキップ
                TokenKind::KwAttribute | TokenKind::KwAttribute2 => {
                    self.skip_attribute()?;
                }
                // typedef名
                TokenKind::Ident(id) if self.typedefs.contains(id) => {
                    let id = *id;
                    specs.type_specs.push(TypeSpec::TypedefName(id));
                    self.advance()?;
                }
                // それ以外はループ終了
                _ => break,
            }
        }

        Ok(specs)
    }

    /// 構造体/共用体をパース
    fn parse_struct_or_union(&mut self, is_struct: bool) -> Result<TypeSpec> {
        let loc = self.current.loc.clone();
        self.advance()?; // struct/union

        // GCC拡張: struct __attribute__((...)) name { ... }
        self.try_skip_attribute()?;

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

            // GCC拡張: 宣言子の後の __attribute__ をスキップ
            self.try_skip_attribute()?;

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
            // 識別子（キーワードはTokenKind::Kw*なのでここには来ない）
            self.advance()?;
            Some(id)
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
            match &self.current.kind {
                TokenKind::KwStatic => {
                    is_static = true;
                    self.advance()?;
                }
                TokenKind::KwConst | TokenKind::KwConst2 | TokenKind::KwConst3 => {
                    qualifiers.is_const = true;
                    self.advance()?;
                }
                TokenKind::KwVolatile | TokenKind::KwVolatile2 | TokenKind::KwVolatile3 => {
                    qualifiers.is_volatile = true;
                    self.advance()?;
                }
                TokenKind::KwRestrict | TokenKind::KwRestrict2 | TokenKind::KwRestrict3 => {
                    qualifiers.is_restrict = true;
                    self.advance()?;
                }
                _ => break,
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

            // GCC拡張: パラメータ後の __attribute__((...)) をスキップ
            self.try_skip_attribute()?;

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
            match &self.current.kind {
                TokenKind::KwConst | TokenKind::KwConst2 | TokenKind::KwConst3 => {
                    qualifiers.is_const = true;
                    self.advance()?;
                }
                TokenKind::KwVolatile | TokenKind::KwVolatile2 | TokenKind::KwVolatile3 => {
                    qualifiers.is_volatile = true;
                    self.advance()?;
                }
                TokenKind::KwRestrict | TokenKind::KwRestrict2 | TokenKind::KwRestrict3 => {
                    qualifiers.is_restrict = true;
                    self.advance()?;
                }
                TokenKind::KwAtomic => {
                    qualifiers.is_atomic = true;
                    self.advance()?;
                }
                _ => break,
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
                // 識別子（キーワードはTokenKind::Kw*なのでここには来ない）
                // typedef名でなければ宣言子
                if !self.typedefs.contains(&id) {
                    Some(self.parse_declarator()?)
                } else {
                    None
                }
            } else {
                None
            };

            // GCC拡張: パラメータ後の __attribute__((...)) をスキップ
            self.try_skip_attribute()?;

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
        let is_target = self.source.is_file_in_target(loc.file_id);
        let specs = self.parse_decl_specs()?;

        if self.check(&TokenKind::Semi) {
            self.advance()?;
            return Ok(Declaration {
                specs,
                declarators: Vec::new(),
                loc,
                comments,
                is_target,
            });
        }

        let mut declarators = Vec::new();

        loop {
            let declarator = self.parse_declarator()?;

            // GCC拡張: 宣言子の後の __attribute__((...)) をスキップ
            self.try_skip_attribute()?;

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

        // GCC拡張: 宣言の最後の __attribute__((...)) をスキップ
        self.try_skip_attribute()?;

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
            is_target,
        })
    }

    /// 文をパース
    fn parse_stmt(&mut self) -> Result<Stmt> {
        let loc = self.current.loc.clone();

        // キーワードに基づく文のパース
        match &self.current.kind {
            // ラベル文
            TokenKind::KwCase => return self.parse_case_stmt(),
            TokenKind::KwDefault => return self.parse_default_stmt(),
            // 複合文
            TokenKind::LBrace => return Ok(Stmt::Compound(self.parse_compound_stmt()?)),
            // 制御フロー文
            TokenKind::KwIf => return self.parse_if_stmt(),
            TokenKind::KwSwitch => return self.parse_switch_stmt(),
            TokenKind::KwWhile => return self.parse_while_stmt(),
            TokenKind::KwDo => return self.parse_do_while_stmt(),
            TokenKind::KwFor => return self.parse_for_stmt(),
            TokenKind::KwGoto => {
                self.advance()?;
                let name = self.expect_ident()?;
                self.expect(&TokenKind::Semi)?;
                return Ok(Stmt::Goto(name, loc));
            }
            TokenKind::KwContinue => {
                self.advance()?;
                self.expect(&TokenKind::Semi)?;
                return Ok(Stmt::Continue(loc));
            }
            TokenKind::KwBreak => {
                self.advance()?;
                self.expect(&TokenKind::Semi)?;
                return Ok(Stmt::Break(loc));
            }
            TokenKind::KwReturn => {
                self.advance()?;
                let expr = if self.check(&TokenKind::Semi) {
                    None
                } else {
                    Some(Box::new(self.parse_expr()?))
                };
                self.expect(&TokenKind::Semi)?;
                return Ok(Stmt::Return(expr, loc));
            }
            // __asm__ 文
            TokenKind::KwAsm | TokenKind::KwAsm2 | TokenKind::KwAsm3 => {
                return self.parse_asm_stmt();
            }
            _ => {}
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

        let else_stmt = if matches!(self.current.kind, TokenKind::KwElse) {
            self.advance()?;
            Some(Box::new(self.parse_stmt()?))
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
        // expect 'while' keyword
        if !matches!(self.current.kind, TokenKind::KwWhile) {
            return Err(CompileError::Parse {
                loc: self.current.loc.clone(),
                kind: ParseError::UnexpectedToken {
                    expected: "while".to_string(),
                    found: self.current.kind.clone(),
                },
            });
        }
        self.advance()?;
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

    fn parse_asm_stmt(&mut self) -> Result<Stmt> {
        let loc = self.current.loc.clone();
        self.advance()?; // asm / __asm / __asm__

        // volatile / __volatile__ はスキップ
        while matches!(
            self.current.kind,
            TokenKind::KwVolatile | TokenKind::KwVolatile2 | TokenKind::KwVolatile3
        ) {
            self.advance()?;
        }

        // 括弧内をスキップ
        self.expect(&TokenKind::LParen)?;
        self.skip_balanced_parens()?;

        self.expect(&TokenKind::Semi)?;
        Ok(Stmt::Asm { loc })
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
        // ( type-name ) cast-expression のみをここで処理
        // ( expr ) は parse_primary_expr で処理し、postfix操作を許可する
        if self.check(&TokenKind::LParen) {
            let loc = self.current.loc.clone();
            self.advance()?; // (

            if self.is_type_start() {
                // キャストまたは複合リテラル
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
            } else if self.check(&TokenKind::LBrace) {
                // GCC拡張: ステートメント式 ({ ... })
                let stmt = self.parse_compound_stmt()?;
                self.expect(&TokenKind::RParen)?;
                // ステートメント式の後もpostfixを許可する
                return self.parse_postfix_on(Expr::StmtExpr(stmt, loc));
            } else {
                // 括弧で囲まれた式 - parse_primary_exprに任せる
                // いったん戻してparse_unary_exprに任せる
                // 注: ここではadvanceを巻き戻す代わりに、式をパースしてからpostfixを処理
                let expr = self.parse_expr()?;
                self.expect(&TokenKind::RParen)?;
                // postfix操作を許可する（->や.など）
                return self.parse_postfix_on(expr);
            }
        }

        self.parse_unary_expr()
    }

    /// 既存の式に対してpostfix操作をパース
    fn parse_postfix_on(&mut self, mut expr: Expr) -> Result<Expr> {
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
                    while !self.check(&TokenKind::RParen) {
                        args.push(self.parse_assignment_expr()?);
                        if !self.check(&TokenKind::Comma) {
                            break;
                        }
                        self.advance()?;
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
            TokenKind::KwSizeof => {
                self.advance()?;
                if self.check(&TokenKind::LParen) {
                    self.advance()?; // (
                    if self.is_type_start() {
                        // sizeof(type)
                        let type_name = self.parse_type_name()?;
                        self.expect(&TokenKind::RParen)?;
                        Ok(Expr::SizeofType(Box::new(type_name), loc))
                    } else {
                        // sizeof(expr) - 括弧付きの式
                        let expr = self.parse_expr()?;
                        self.expect(&TokenKind::RParen)?;
                        Ok(Expr::Sizeof(Box::new(expr), loc))
                    }
                } else {
                    let expr = self.parse_unary_expr()?;
                    Ok(Expr::Sizeof(Box::new(expr), loc))
                }
            }
            TokenKind::KwAlignof | TokenKind::KwAlignof2 | TokenKind::KwAlignof3 => {
                self.advance()?;
                self.expect(&TokenKind::LParen)?;
                let type_name = self.parse_type_name()?;
                self.expect(&TokenKind::RParen)?;
                Ok(Expr::Alignof(Box::new(type_name), loc))
            }
            // GCC拡張: __extension__ は無視して続行（TinyCC方式）
            TokenKind::KwExtension => {
                self.advance()?;
                self.parse_unary_expr()
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
                // GCC拡張: ステートメント式 ({ ... })
                if self.check(&TokenKind::LBrace) {
                    let stmt = self.parse_compound_stmt()?;
                    self.expect(&TokenKind::RParen)?;
                    Ok(Expr::StmtExpr(stmt, loc))
                } else {
                    let expr = self.parse_expr()?;
                    self.expect(&TokenKind::RParen)?;
                    Ok(expr)
                }
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
        let old = std::mem::replace(&mut self.current, self.source.next_token()?);
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

    fn is_type_start(&self) -> bool {
        match &self.current.kind {
            // 型指定子キーワード
            TokenKind::KwVoid
            | TokenKind::KwChar
            | TokenKind::KwShort
            | TokenKind::KwInt
            | TokenKind::KwLong
            | TokenKind::KwFloat
            | TokenKind::KwDouble
            | TokenKind::KwSigned
            | TokenKind::KwSigned2
            | TokenKind::KwUnsigned
            | TokenKind::KwBool
            | TokenKind::KwBool2
            | TokenKind::KwComplex
            | TokenKind::KwFloat16
            | TokenKind::KwFloat32
            | TokenKind::KwFloat64
            | TokenKind::KwFloat128
            | TokenKind::KwFloat32x
            | TokenKind::KwFloat64x
            | TokenKind::KwInt128 => true,
            // 型修飾子キーワード
            TokenKind::KwConst
            | TokenKind::KwConst2
            | TokenKind::KwConst3
            | TokenKind::KwVolatile
            | TokenKind::KwVolatile2
            | TokenKind::KwVolatile3
            | TokenKind::KwRestrict
            | TokenKind::KwRestrict2
            | TokenKind::KwRestrict3
            | TokenKind::KwAtomic => true,
            // 構造体・共用体・列挙
            TokenKind::KwStruct | TokenKind::KwUnion | TokenKind::KwEnum => true,
            // typeof
            TokenKind::KwTypeof | TokenKind::KwTypeof2 | TokenKind::KwTypeof3 => true,
            // typedef名
            TokenKind::Ident(id) => self.typedefs.contains(id),
            _ => false,
        }
    }

    fn is_declaration_start(&self) -> bool {
        match &self.current.kind {
            // ストレージクラス
            TokenKind::KwTypedef
            | TokenKind::KwExtern
            | TokenKind::KwStatic
            | TokenKind::KwAuto
            | TokenKind::KwRegister => true,
            // inline
            TokenKind::KwInline | TokenKind::KwInline2 | TokenKind::KwInline3 => true,
            // GCC拡張
            TokenKind::KwExtension => true,
            // thread-local
            TokenKind::KwThreadLocal | TokenKind::KwThread => true,
            _ => self.is_type_start(),
        }
    }

    /// __attribute__ / __asm__ があればスキップ（複数連続も対応）
    fn try_skip_attribute(&mut self) -> Result<()> {
        loop {
            match &self.current.kind {
                TokenKind::KwAttribute | TokenKind::KwAttribute2 => {
                    self.skip_attribute()?;
                }
                TokenKind::KwAsm | TokenKind::KwAsm2 | TokenKind::KwAsm3 => {
                    self.try_skip_asm_label()?;
                }
                _ => break,
            }
        }
        Ok(())
    }

    /// GCC拡張: __attribute__((...)) をスキップ
    fn skip_attribute(&mut self) -> Result<()> {
        self.advance()?; // __attribute__ / __attribute

        // 外側の ( を期待
        if !self.check(&TokenKind::LParen) {
            return Ok(()); // 引数なしの場合
        }
        self.advance()?;

        // 内側の ( を期待
        if !self.check(&TokenKind::LParen) {
            // 単一括弧の場合もある
            self.skip_balanced_parens()?;
            return Ok(());
        }
        self.advance()?;

        // 内側の括弧の中身をスキップ
        self.skip_balanced_parens()?;

        // 外側の ) を期待
        self.expect(&TokenKind::RParen)?;

        Ok(())
    }

    /// __asm__(label) があればスキップ
    fn try_skip_asm_label(&mut self) -> Result<()> {
        if matches!(
            self.current.kind,
            TokenKind::KwAsm | TokenKind::KwAsm2 | TokenKind::KwAsm3
        ) {
            self.advance()?; // asm / __asm / __asm__
            if self.check(&TokenKind::LParen) {
                self.advance()?; // (
                self.skip_balanced_parens()?; // 内容をスキップして ) を消費
            }
        }
        Ok(())
    }

    /// 括弧のバランスを取りながらスキップ
    fn skip_balanced_parens(&mut self) -> Result<()> {
        let mut depth = 1;
        while depth > 0 {
            match &self.current.kind {
                TokenKind::LParen => depth += 1,
                TokenKind::RParen => depth -= 1,
                TokenKind::Eof => {
                    return Err(CompileError::Parse {
                        loc: self.current.loc.clone(),
                        kind: ParseError::UnexpectedToken {
                            expected: ")".to_string(),
                            found: TokenKind::Eof,
                        },
                    });
                }
                _ => {}
            }
            if depth > 0 {
                self.advance()?;
            }
        }
        self.advance()?; // 最後の )
        Ok(())
    }
}

// ==================== ヘルパー関数 ====================

use crate::source::FileRegistry;
use crate::token_source::TokenSlice;

/// トークン列から式をパース
///
/// マクロ本体などのトークン列を式としてパースする際に使用。
///
/// # Arguments
/// * `tokens` - パースするトークン列
/// * `interner` - 文字列インターナー
/// * `files` - ファイルレジストリ
/// * `typedefs` - typedef名のセット（キャスト式の型名判定に使用）
pub fn parse_expression_from_tokens(
    tokens: Vec<Token>,
    interner: StringInterner,
    files: FileRegistry,
    typedefs: HashSet<InternedStr>,
) -> Result<Expr> {
    let mut source = TokenSlice::new(tokens, interner, files);
    let mut parser = Parser::from_source_with_typedefs(&mut source, typedefs)?;
    parser.parse_expr_only()
}

/// トークン列から式をパース（参照ベース版）
///
/// `parse_expression_from_tokens` の参照ベース版。
/// interner, files, typedefs をクローンせずに借用することで、
/// 高頻度の呼び出し時のオーバーヘッドを削減する。
///
/// # Arguments
/// * `tokens` - パースするトークン列
/// * `interner` - 文字列インターナーへの参照
/// * `files` - ファイルレジストリへの参照
/// * `typedefs` - typedef名のセットへの参照
pub fn parse_expression_from_tokens_ref(
    tokens: Vec<Token>,
    interner: &StringInterner,
    files: &FileRegistry,
    typedefs: &HashSet<InternedStr>,
) -> Result<Expr> {
    let mut source = TokenSliceRef::new(tokens, interner, files);
    let mut parser = Parser::from_source_with_typedefs(&mut source, typedefs.clone())?;
    parser.parse_expr_only()
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
