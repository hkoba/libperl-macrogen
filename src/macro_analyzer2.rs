//! マクロ解析モジュール v2
//!
//! SemanticAnalyzer を活用したマクロ解析を行う:
//! - マクロ展開とAST変換
//! - SemanticAnalyzer による型推論
//! - THX 依存検出（my_perl の推移閉包）
//! - マクロ分類（Expression/Statement/Other）
//! - 定数マクロ識別

use std::collections::{HashMap, HashSet};

use crate::apidoc::ApidocDict;
use crate::ast::Expr;
use crate::error::Result;
use crate::fields_dict::FieldsDict;
use crate::intern::{InternedStr, StringInterner};
use crate::macro_def::{MacroDef, MacroKind, MacroTable};
use crate::parser::parse_expression_from_tokens_ref;
use crate::rust_decl::RustDeclDict;
use crate::semantic::{SemanticAnalyzer, Type};
use crate::source::{FileRegistry, SourceLocation};
use crate::token::{Token, TokenKind};

/// マクロ展開結果の分類
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacroCategory {
    /// 完全なC式として評価可能
    Expression,
    /// 文（末尾の`;`を除く）
    Statement,
    /// 式でも文でもない（不完全、複数文など）
    Other,
}

/// ジェネリックパラメータの情報
/// polymorphic なフィールドアクセスを持つパラメータに対して生成される
#[derive(Debug, Clone)]
pub struct GenericParamInfo {
    /// 型パラメータ名 (e.g., "T", "T1", "T2")
    pub type_param: String,
    /// ポインタキャスト用の基底型 (e.g., "sv")
    pub base_type: String,
    /// ジェネリック検出のトリガーとなったフィールド
    pub polymorphic_field: InternedStr,
}

/// ジェネリック戻り値の情報
#[derive(Debug, Clone)]
pub struct GenericReturnInfo {
    /// 型パラメータ名 (e.g., "R")
    pub type_param: String,
    /// ポインタ型かどうか
    pub is_pointer: bool,
}

/// マクロの解析結果
#[derive(Debug, Clone)]
pub struct MacroInfo2 {
    /// マクロ名
    pub name: InternedStr,
    /// マクロのカテゴリ
    pub category: MacroCategory,
    /// 推論された戻り値型（Rust型文字列）
    pub return_type: Option<String>,
    /// パラメータの推論された型
    pub param_types: HashMap<InternedStr, String>,
    /// 対象ディレクトリ内かどうか
    pub is_target: bool,
    /// THX依存（my_perl引数が必要）
    pub needs_my_perl: bool,
    /// パース済みAST（Expressionマクロの場合）
    pub parsed_expr: Option<Expr>,
    /// 定義位置
    pub def_loc: SourceLocation,
    /// 使用しているマクロの集合
    pub uses: HashSet<InternedStr>,

    // ==================== Generic Type Parameters ====================
    /// ジェネリック型パラメータ情報
    /// パラメータ名 -> GenericParamInfo
    pub generic_params: HashMap<InternedStr, GenericParamInfo>,
    /// ジェネリック戻り値型情報
    pub generic_return: Option<GenericReturnInfo>,
}

/// Expression マクロの解析結果（内部用）
struct ExpressionMacroAnalysis {
    expr: Option<Expr>,
    return_type: Option<String>,
    param_types: HashMap<InternedStr, String>,
    generic_params: HashMap<InternedStr, GenericParamInfo>,
    generic_return: Option<GenericReturnInfo>,
}

/// SemanticAnalyzer ベースのマクロ解析器
pub struct MacroAnalyzer2<'a> {
    /// 文字列インターナー
    interner: &'a StringInterner,
    /// ファイルレジストリ
    files: &'a FileRegistry,
    /// Apidoc辞書
    apidoc: &'a ApidocDict,
    /// フィールド辞書
    fields_dict: &'a FieldsDict,
    /// RustDeclDict (bindings.rs の関数/型情報)
    rust_decl_dict: Option<&'a RustDeclDict>,
    /// 対象ディレクトリ
    target_dir: String,
    /// typedef名のセット（パース時のキャスト式判定用）
    typedefs: HashSet<InternedStr>,

    // 分析結果
    /// マクロ情報（マクロ名 -> 解析結果）
    info: HashMap<InternedStr, MacroInfo2>,
    /// 定数マクロの集合（展開を抑制する対象）
    constant_macros: HashSet<InternedStr>,
    /// bindings.rs に定義されている定数名
    bindings_consts: HashSet<String>,
    /// THX依存マクロの集合
    thx_macros: HashSet<InternedStr>,
    /// THX依存関数の集合（bindings.rsから取得）
    thx_functions: HashSet<String>,
}

impl<'a> MacroAnalyzer2<'a> {
    /// 新しい解析器を作成
    pub fn new(
        interner: &'a StringInterner,
        files: &'a FileRegistry,
        apidoc: &'a ApidocDict,
        fields_dict: &'a FieldsDict,
        target_dir: &str,
    ) -> Self {
        Self {
            interner,
            files,
            apidoc,
            fields_dict,
            rust_decl_dict: None,
            target_dir: target_dir.to_string(),
            typedefs: HashSet::new(),
            info: HashMap::new(),
            constant_macros: HashSet::new(),
            bindings_consts: HashSet::new(),
            thx_macros: HashSet::new(),
            thx_functions: HashSet::new(),
        }
    }

    /// RustDeclDict を設定
    pub fn set_rust_decl_dict(&mut self, rust_decl_dict: &'a RustDeclDict) {
        self.rust_decl_dict = Some(rust_decl_dict);
    }

    /// typedef名のセットを設定
    pub fn set_typedefs(&mut self, typedefs: HashSet<InternedStr>) {
        self.typedefs = typedefs;
    }

    /// bindings.rs の定数名を設定
    pub fn set_bindings_consts(&mut self, consts: HashSet<String>) {
        self.bindings_consts = consts;
    }

    /// THX依存関数の集合を設定（bindings.rsから）
    pub fn set_thx_functions(&mut self, fns: HashSet<String>) {
        self.thx_functions = fns;
    }

    // ========================================================================
    // 定数マクロ識別
    // ========================================================================

    /// 定数マクロを識別する
    ///
    /// Object マクロで本体が定数式のものを反復的に特定する。
    pub fn identify_constant_macros(&mut self, macros: &MacroTable) {
        loop {
            let mut found_new = false;

            for (name, def) in macros.iter() {
                if self.constant_macros.contains(name) {
                    continue;
                }

                if def.is_function() {
                    continue;
                }

                if self.is_constant_body(&def.body, macros) {
                    self.constant_macros.insert(*name);
                    found_new = true;
                }
            }

            if !found_new {
                break;
            }
        }
    }

    /// トークン列が定数式かどうかをチェック
    fn is_constant_body(&self, tokens: &[Token], macros: &MacroTable) -> bool {
        if tokens.is_empty() {
            return false;
        }

        for token in tokens {
            match &token.kind {
                TokenKind::IntLit(_) | TokenKind::UIntLit(_) => {}

                TokenKind::Ident(id) => {
                    let name_str = self.interner.get(*id);

                    if self.bindings_consts.contains(name_str) {
                        continue;
                    }

                    if self.constant_macros.contains(id) {
                        continue;
                    }

                    if macros.is_defined(*id) {
                        return false;
                    }

                    return false;
                }

                TokenKind::Plus | TokenKind::Minus | TokenKind::Star
                | TokenKind::Slash | TokenKind::Percent
                | TokenKind::Pipe | TokenKind::Amp | TokenKind::Caret
                | TokenKind::Tilde | TokenKind::LtLt | TokenKind::GtGt
                | TokenKind::LParen | TokenKind::RParen => {}

                _ => return false,
            }
        }

        true
    }

    /// 指定された名前が定数マクロかどうか
    pub fn is_constant_macro(&self, name: InternedStr) -> bool {
        self.constant_macros.contains(&name)
    }

    /// 定数マクロの集合を取得
    pub fn constant_macros(&self) -> &HashSet<InternedStr> {
        &self.constant_macros
    }

    // ========================================================================
    // THX 依存検出
    // ========================================================================

    /// THX依存マクロを識別する
    ///
    /// 以下のいずれかを含むマクロはTHX依存:
    /// 1. 展開後のトークン列に `my_perl` を含む
    /// 2. THX依存な関数/マクロを呼び出している（推移的）
    pub fn identify_thx_dependent_macros(&mut self, macros: &MacroTable) {
        // Phase 1: 直接 my_perl を含むマクロを識別
        for (name, def) in macros.iter() {
            let expanded = self.expand_macro_body(def, macros, &mut HashSet::new());
            if self.contains_my_perl(&expanded) {
                self.thx_macros.insert(*name);
            }
        }

        // Phase 2: 推移的依存を解決（収束するまで反復）
        loop {
            let mut found_new = false;
            let current_thx: HashSet<InternedStr> = self.thx_macros.clone();

            for (name, info) in &self.info {
                if current_thx.contains(name) {
                    continue;
                }

                for used in &info.uses {
                    if current_thx.contains(used) || self.is_thx_function(*used) {
                        self.thx_macros.insert(*name);
                        found_new = true;
                        break;
                    }
                }
            }

            if !found_new {
                break;
            }
        }

        // Phase 3: MacroInfo2のneeds_my_perlフラグを更新
        let thx_macros = self.thx_macros.clone();
        for (name, info) in self.info.iter_mut() {
            if thx_macros.contains(name) {
                info.needs_my_perl = true;
            }
        }
    }

    /// トークン列が my_perl を含むかチェック
    fn contains_my_perl(&self, tokens: &[Token]) -> bool {
        tokens.iter().any(|t| {
            if let TokenKind::Ident(id) = &t.kind {
                self.interner.get(*id) == "my_perl"
            } else {
                false
            }
        })
    }

    /// 指定された名前がTHX依存関数かどうか
    fn is_thx_function(&self, name: InternedStr) -> bool {
        let name_str = self.interner.get(name);
        if name_str.starts_with("Perl_") {
            return true;
        }
        self.thx_functions.contains(name_str)
    }

    /// 指定された名前がTHX依存マクロかどうか
    pub fn is_thx_macro(&self, name: InternedStr) -> bool {
        self.thx_macros.contains(&name)
    }

    /// THX依存マクロの集合を取得
    pub fn thx_macros(&self) -> &HashSet<InternedStr> {
        &self.thx_macros
    }

    // ========================================================================
    // マクロ分析（メイン処理）
    // ========================================================================

    /// マクロテーブルを解析
    pub fn analyze(&mut self, macros: &MacroTable) {
        // Phase 1: 各マクロの使用関係を収集＋分類
        for (name, def) in macros.iter() {
            let is_target = self.is_target_location(&def.def_loc);

            // 使用関係を収集
            let mut uses = HashSet::new();
            for token in &def.body {
                if let TokenKind::Ident(ident) = token.kind {
                    if macros.is_defined(ident) && ident != *name {
                        uses.insert(ident);
                    }
                }
            }

            // マクロを分類
            let category = self.classify_macro_body(def, macros);

            // パースとAST生成（Expressionマクロのみ）
            let analysis = if category == MacroCategory::Expression && is_target {
                self.analyze_expression_macro(def, macros)
            } else {
                ExpressionMacroAnalysis {
                    expr: None,
                    return_type: None,
                    param_types: HashMap::new(),
                    generic_params: HashMap::new(),
                    generic_return: None,
                }
            };

            self.info.insert(
                *name,
                MacroInfo2 {
                    name: *name,
                    category,
                    return_type: analysis.return_type,
                    param_types: analysis.param_types,
                    is_target,
                    needs_my_perl: false,
                    parsed_expr: analysis.expr,
                    def_loc: def.def_loc.clone(),
                    uses,
                    generic_params: analysis.generic_params,
                    generic_return: analysis.generic_return,
                },
            );
        }
    }

    /// Expression マクロを解析（パース、型推論、ジェネリック検出）
    fn analyze_expression_macro(
        &self,
        def: &MacroDef,
        macros: &MacroTable,
    ) -> ExpressionMacroAnalysis {
        // マクロ本体をパース
        let (_, parse_result) = self.parse_macro_body(def, macros);

        match parse_result {
            Ok(expr) => {
                // SemanticAnalyzer で型推論 (RustDeclDict を渡す)
                let mut semantic = SemanticAnalyzer::with_rust_decl_dict(
                    self.interner,
                    Some(self.apidoc),
                    Some(self.fields_dict),
                    self.rust_decl_dict,
                );

                // パラメータの型変数を登録
                let params = match &def.kind {
                    MacroKind::Function { params, .. } => params.clone(),
                    _ => vec![],
                };

                if !params.is_empty() {
                    semantic.begin_param_inference(&params);
                }

                // 戻り値型を推論 (同時に制約も収集される)
                let return_type = self.infer_return_type(&mut semantic, &expr);

                // 制約を解いてパラメータ型を取得
                let param_types = if !params.is_empty() {
                    let inferred = semantic.end_param_inference();
                    inferred
                        .into_iter()
                        .filter_map(|(name, ty)| {
                            self.type_to_rust_string(&ty).map(|s| (name, s))
                        })
                        .collect()
                } else {
                    HashMap::new()
                };

                // ジェネリックパラメータを検出
                let generic_params = match &def.kind {
                    MacroKind::Function { params, .. } if !params.is_empty() => {
                        self.detect_generic_params_for_macro(&expr, params)
                    }
                    _ => HashMap::new(),
                };

                // ジェネリック戻り値を検出
                let generic_return = self.detect_generic_return(&expr);

                ExpressionMacroAnalysis {
                    expr: Some(expr),
                    return_type,
                    param_types,
                    generic_params,
                    generic_return,
                }
            }
            Err(_) => ExpressionMacroAnalysis {
                expr: None,
                return_type: None,
                param_types: HashMap::new(),
                generic_params: HashMap::new(),
                generic_return: None,
            },
        }
    }

    /// マクロ本体を分類
    fn classify_macro_body(&self, def: &MacroDef, macros: &MacroTable) -> MacroCategory {
        let body = &def.body;

        if body.is_empty() {
            return MacroCategory::Other;
        }

        let expanded = self.expand_macro_body(def, macros, &mut HashSet::new());

        if expanded.is_empty() {
            return MacroCategory::Other;
        }

        // セミコロンで終わる場合は文
        if matches!(expanded.last().map(|t| &t.kind), Some(TokenKind::Semi)) {
            return MacroCategory::Statement;
        }

        // 式として妥当かチェック
        if self.is_valid_expression(&expanded) {
            MacroCategory::Expression
        } else {
            MacroCategory::Other
        }
    }

    /// マクロ本体を再帰的に展開
    fn expand_macro_body(
        &self,
        def: &MacroDef,
        macros: &MacroTable,
        visited: &mut HashSet<InternedStr>,
    ) -> Vec<Token> {
        if visited.contains(&def.name) {
            return def.body.clone();
        }
        visited.insert(def.name);

        let mut result = Vec::new();

        for token in &def.body {
            match &token.kind {
                TokenKind::Ident(ident) => {
                    // 定数マクロは展開しない
                    if self.constant_macros.contains(ident) {
                        result.push(token.clone());
                        continue;
                    }

                    // bindings.rs の定数も展開しない
                    let name_str = self.interner.get(*ident);
                    if self.bindings_consts.contains(name_str) {
                        result.push(token.clone());
                        continue;
                    }

                    // オブジェクトマクロは展開
                    if let Some(macro_def) = macros.get(*ident) {
                        if !macro_def.is_function() {
                            let expanded = self.expand_macro_body(macro_def, macros, visited);
                            result.extend(expanded);
                            continue;
                        }
                    }
                    result.push(token.clone());
                }
                _ => {
                    result.push(token.clone());
                }
            }
        }

        visited.remove(&def.name);
        result
    }

    /// トークン列が妥当な式かどうかをチェック
    fn is_valid_expression(&self, tokens: &[Token]) -> bool {
        if tokens.is_empty() {
            return false;
        }

        let mut paren_depth = 0i32;
        let mut brace_depth = 0i32;
        let mut bracket_depth = 0i32;

        for token in tokens {
            match &token.kind {
                TokenKind::LParen => paren_depth += 1,
                TokenKind::RParen => paren_depth -= 1,
                TokenKind::LBrace => brace_depth += 1,
                TokenKind::RBrace => brace_depth -= 1,
                TokenKind::LBracket => bracket_depth += 1,
                TokenKind::RBracket => bracket_depth -= 1,
                TokenKind::KwIf | TokenKind::KwWhile | TokenKind::KwFor
                | TokenKind::KwSwitch | TokenKind::KwReturn | TokenKind::KwGoto => {
                    if paren_depth == 0 {
                        return false;
                    }
                }
                _ => {}
            }

            if paren_depth < 0 || brace_depth < 0 || bracket_depth < 0 {
                return false;
            }
        }

        paren_depth == 0 && brace_depth == 0 && bracket_depth == 0
    }

    // ========================================================================
    // 型推論（SemanticAnalyzer ベース）
    // ========================================================================

    /// SemanticAnalyzer で戻り値型を推論
    fn infer_return_type(&self, semantic: &mut SemanticAnalyzer, expr: &Expr) -> Option<String> {
        let ty = semantic.infer_expr_type(expr);
        self.type_to_rust_string(&ty)
    }

    /// Type を Rust型文字列に変換
    ///
    /// UnifiedType を使用した統一的な型変換を行う
    fn type_to_rust_string(&self, ty: &Type) -> Option<String> {
        let unified = ty.to_unified(self.interner);
        match unified {
            crate::unified_type::UnifiedType::Unknown => None,
            _ => Some(unified.to_rust_string()),
        }
    }

    // ==================== Generic Parameter Detection ====================

    /// 式がパラメータ識別子かどうか
    fn is_param_ident(&self, expr: &Expr, param: InternedStr) -> bool {
        match expr {
            Expr::Ident(id, _) => *id == param,
            // キャストを通して識別子をチェック
            Expr::Cast { expr: inner, .. } => self.is_param_ident(inner, param),
            _ => false,
        }
    }

    /// パラメータのジェネリック情報を検出
    /// SVファミリーの共有フィールドへのアクセスがあればジェネリックパラメータとして扱う
    fn detect_generic_param(
        &self,
        expr: &Expr,
        param: InternedStr,
    ) -> Option<GenericParamInfo> {
        match expr {
            // パターン: param->field (SVファミリーの共有フィールド)
            Expr::PtrMember { expr: base, member, .. } => {
                if self.is_param_ident(base, param) {
                    // SVファミリーの共有フィールドかチェック
                    if self.fields_dict.is_sv_family_field(*member, self.interner) {
                        return Some(GenericParamInfo {
                            type_param: "T".to_string(),
                            base_type: "sv".to_string(),
                            polymorphic_field: *member,
                        });
                    }
                }
                // 再帰的に探索
                self.detect_generic_param(base, param)
            }

            // 二項演算子
            Expr::Binary { lhs, rhs, .. } => {
                self.detect_generic_param(lhs, param)
                    .or_else(|| self.detect_generic_param(rhs, param))
            }

            // 関数呼び出し
            Expr::Call { func, args, .. } => {
                for arg in args {
                    if let Some(info) = self.detect_generic_param(arg, param) {
                        return Some(info);
                    }
                }
                self.detect_generic_param(func, param)
            }

            // 単項演算子
            Expr::PreInc(inner, _)
            | Expr::PreDec(inner, _)
            | Expr::AddrOf(inner, _)
            | Expr::Deref(inner, _)
            | Expr::UnaryPlus(inner, _)
            | Expr::UnaryMinus(inner, _)
            | Expr::BitNot(inner, _)
            | Expr::LogNot(inner, _)
            | Expr::PostInc(inner, _)
            | Expr::PostDec(inner, _)
            | Expr::Sizeof(inner, _) => {
                self.detect_generic_param(inner, param)
            }

            // キャスト
            Expr::Cast { expr: inner, .. } => {
                self.detect_generic_param(inner, param)
            }

            // 条件演算子
            Expr::Conditional { cond, then_expr, else_expr, .. } => {
                self.detect_generic_param(cond, param)
                    .or_else(|| self.detect_generic_param(then_expr, param))
                    .or_else(|| self.detect_generic_param(else_expr, param))
            }

            // 配列添字
            Expr::Index { expr: base, index, .. } => {
                self.detect_generic_param(base, param)
                    .or_else(|| self.detect_generic_param(index, param))
            }

            // メンバアクセス
            Expr::Member { expr: base, .. } => {
                self.detect_generic_param(base, param)
            }

            _ => None,
        }
    }

    /// マクロのジェネリックパラメータを検出して設定
    fn detect_generic_params_for_macro(
        &self,
        expr: &Expr,
        params: &[InternedStr],
    ) -> HashMap<InternedStr, GenericParamInfo> {
        let mut result = HashMap::new();
        let mut type_param_counter = 0;

        for param in params {
            if let Some(mut info) = self.detect_generic_param(expr, *param) {
                // 複数のジェネリックパラメータがある場合は番号付け
                if type_param_counter > 0 || params.len() > 1 {
                    info.type_param = format!("T{}", type_param_counter);
                }
                result.insert(*param, info);
                type_param_counter += 1;
            }
        }

        result
    }

    /// 戻り値がジェネリックかどうか検出
    /// SVファミリーの sv_any フィールドへのアクセスで戻り値が決まる場合
    fn detect_generic_return(&self, expr: &Expr) -> Option<GenericReturnInfo> {
        // トップレベルで sv_any へのアクセスがあれば戻り値もジェネリック
        match expr {
            Expr::PtrMember { member, .. } => {
                let member_str = self.interner.get(*member);
                // sv_any は構造体ごとに型が異なるのでジェネリック戻り値が必要
                if member_str == "sv_any" {
                    return Some(GenericReturnInfo {
                        type_param: "R".to_string(),
                        is_pointer: true,
                    });
                }
                None
            }
            // キャストを通した場合も検出
            Expr::Cast { expr: inner, .. } => self.detect_generic_return(inner),
            _ => None,
        }
    }

    // ========================================================================
    // マクロ本体のパース
    // ========================================================================

    /// マクロ本体をパースしてASTを取得
    pub fn parse_macro_body(&self, def: &MacroDef, macros: &MacroTable) -> (Vec<Token>, Result<Expr>) {
        let expanded = self.expand_macro_body(def, macros, &mut HashSet::new());
        let result = parse_expression_from_tokens_ref(
            expanded.clone(),
            self.interner,
            self.files,
            &self.typedefs,
        );
        (expanded, result)
    }

    // ========================================================================
    // ユーティリティ
    // ========================================================================

    /// 位置が対象ディレクトリ内かチェック
    fn is_target_location(&self, loc: &SourceLocation) -> bool {
        let path = self.files.get_path(loc.file_id);
        let path_str = path.to_string_lossy();
        path_str.starts_with(&self.target_dir)
    }

    // ========================================================================
    // クエリAPI
    // ========================================================================

    /// 解析結果を取得
    pub fn get_info(&self, name: InternedStr) -> Option<&MacroInfo2> {
        self.info.get(&name)
    }

    /// 全解析結果をイテレート
    pub fn iter(&self) -> impl Iterator<Item = (&InternedStr, &MacroInfo2)> {
        self.info.iter()
    }

    /// 式マクロのみをイテレート（対象ディレクトリ内）
    pub fn expression_macros(&self) -> impl Iterator<Item = &MacroInfo2> {
        self.info.values().filter(|info| {
            info.category == MacroCategory::Expression && info.is_target
        })
    }

    /// 統計情報をダンプ
    pub fn dump_stats(&self) -> String {
        let total = self.info.len();
        let target = self.info.values().filter(|i| i.is_target).count();
        let expressions = self.info.values()
            .filter(|i| i.category == MacroCategory::Expression && i.is_target)
            .count();
        let statements = self.info.values()
            .filter(|i| i.category == MacroCategory::Statement && i.is_target)
            .count();
        let other = self.info.values()
            .filter(|i| i.category == MacroCategory::Other && i.is_target)
            .count();
        let with_type = self.info.values()
            .filter(|i| i.return_type.is_some() && i.is_target)
            .count();
        let thx_count = self.thx_macros.len();
        let const_count = self.constant_macros.len();

        format!(
            "=== MacroAnalyzer2 Stats ===\n\
             Total macros: {}\n\
             Target macros: {}\n\
             Expression macros: {}\n\
             Statement macros: {}\n\
             Other macros: {}\n\
             With inferred type: {}\n\
             THX-dependent macros: {}\n\
             Constant macros: {}\n",
            total, target, expressions, statements, other, with_type, thx_count, const_count
        )
    }

    /// Def-Use チェーンをダンプ
    pub fn dump_def_use(&self) -> String {
        let mut result = String::new();
        result.push_str("=== Def-Use Chain ===\n");

        let mut items: Vec<_> = self.info.iter()
            .filter(|(_, info)| info.is_target)
            .collect();
        items.sort_by_key(|(name, _)| self.interner.get(**name));

        for (name, info) in items {
            let name_str = self.interner.get(*name);
            result.push_str(&format!("{}:\n", name_str));

            if !info.uses.is_empty() {
                let mut uses: Vec<_> = info.uses.iter()
                    .map(|n| self.interner.get(*n))
                    .collect();
                uses.sort();
                result.push_str(&format!("  uses: {}\n", uses.join(", ")));
            }

            result.push_str(&format!("  category: {:?}\n", info.category));

            if let Some(ref ty) = info.return_type {
                result.push_str(&format!("  return_type: {}\n", ty));
            }
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_macro_category() {
        assert_eq!(MacroCategory::Expression, MacroCategory::Expression);
        assert_ne!(MacroCategory::Expression, MacroCategory::Statement);
    }
}
