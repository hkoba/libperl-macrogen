//! マクロ解析モジュール
//!
//! マクロ関数のRust関数化のための解析を行う:
//! - Def-Use chain の構築
//! - マクロ展開結果の分類（式/文/その他）
//! - 戻り値型の推論
//! - マクロ本体のパースとAST生成

use std::collections::{HashMap, HashSet};

use crate::ast::Expr;
use crate::error::Result;
use crate::fields_dict::FieldsDict;
use crate::intern::{InternedStr, StringInterner};
use crate::macro_def::{MacroDef, MacroTable};
use crate::parser::parse_expression_from_tokens;
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

/// マクロの解析結果
#[derive(Debug, Clone)]
pub struct MacroInfo {
    /// マクロ名
    pub name: InternedStr,
    /// 使用しているマクロの集合
    pub uses: HashSet<InternedStr>,
    /// このマクロを使用しているマクロの集合
    pub used_by: HashSet<InternedStr>,
    /// マクロのカテゴリ
    pub category: MacroCategory,
    /// 推論された戻り値型（式マクロの場合）
    pub return_type: Option<String>,
    /// パラメータの推論された型
    pub param_types: HashMap<InternedStr, String>,
    /// 定義位置
    pub def_loc: SourceLocation,
    /// 対象ディレクトリ内かどうか
    pub is_target: bool,
}

/// マクロ解析器
pub struct MacroAnalyzer<'a> {
    /// 文字列インターナー
    interner: &'a StringInterner,
    /// ファイルレジストリ
    files: &'a FileRegistry,
    /// マクロ情報（マクロ名 -> 解析結果）
    info: HashMap<InternedStr, MacroInfo>,
    /// フィールド辞書（型推論用）
    fields_dict: &'a FieldsDict,
    /// 対象ディレクトリ
    target_dirs: Vec<String>,
}

impl<'a> MacroAnalyzer<'a> {
    /// 新しい解析器を作成
    pub fn new(
        interner: &'a StringInterner,
        files: &'a FileRegistry,
        fields_dict: &'a FieldsDict,
    ) -> Self {
        Self {
            interner,
            files,
            info: HashMap::new(),
            fields_dict,
            target_dirs: vec!["/usr/lib64/perl5/CORE".to_string()],
        }
    }

    /// 対象ディレクトリを設定
    pub fn set_target_dirs(&mut self, dirs: Vec<String>) {
        self.target_dirs = dirs;
    }

    /// マクロテーブルを解析
    pub fn analyze(&mut self, macros: &MacroTable) {
        // Phase 1: 各マクロの使用関係を収集
        self.collect_usage(macros);

        // Phase 2: used_by を構築（逆参照）
        self.build_used_by();

        // Phase 3: マクロカテゴリを分類
        self.classify_macros(macros);

        // Phase 4: 式マクロの戻り値型を推論
        self.infer_return_types(macros);
    }

    /// Phase 1: 使用関係を収集
    fn collect_usage(&mut self, macros: &MacroTable) {
        for (name, def) in macros.iter() {
            // 対象ディレクトリ内かチェック
            let is_target = self.is_target_location(&def.def_loc);

            let mut uses = HashSet::new();

            // マクロ本体内の識別子を走査
            for token in &def.body {
                if let TokenKind::Ident(ident) = token.kind {
                    // この識別子がマクロとして定義されているか
                    if macros.is_defined(ident) && ident != *name {
                        uses.insert(ident);
                    }
                }
            }

            self.info.insert(
                *name,
                MacroInfo {
                    name: *name,
                    uses,
                    used_by: HashSet::new(),
                    category: MacroCategory::Other,
                    return_type: None,
                    param_types: HashMap::new(),
                    def_loc: def.def_loc.clone(),
                    is_target,
                },
            );
        }
    }

    /// Phase 2: used_by を構築
    fn build_used_by(&mut self) {
        // 使用関係を収集
        let usage_pairs: Vec<(InternedStr, InternedStr)> = self
            .info
            .iter()
            .flat_map(|(user, info)| {
                info.uses.iter().map(move |&used| (*user, used))
            })
            .collect();

        // used_by を更新
        for (user, used) in usage_pairs {
            if let Some(used_info) = self.info.get_mut(&used) {
                used_info.used_by.insert(user);
            }
        }
    }

    /// Phase 3: マクロカテゴリを分類
    fn classify_macros(&mut self, macros: &MacroTable) {
        for (name, def) in macros.iter() {
            let category = self.classify_macro_body(def, macros);
            if let Some(info) = self.info.get_mut(name) {
                info.category = category;
            }
        }
    }

    /// マクロ本体を分類
    fn classify_macro_body(&self, def: &MacroDef, macros: &MacroTable) -> MacroCategory {
        let body = &def.body;

        if body.is_empty() {
            return MacroCategory::Other;
        }

        // 展開してトークン列を取得
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

    /// マクロ本体を再帰的に展開（使用マクロを追跡）
    fn expand_macro_body(
        &self,
        def: &MacroDef,
        macros: &MacroTable,
        visited: &mut HashSet<InternedStr>,
    ) -> Vec<Token> {
        if visited.contains(&def.name) {
            // 循環参照を検出
            return def.body.clone();
        }
        visited.insert(def.name);

        let mut result = Vec::new();

        for token in &def.body {
            match &token.kind {
                TokenKind::Ident(ident) => {
                    // マクロ展開
                    if let Some(macro_def) = macros.get(*ident) {
                        // オブジェクトマクロの場合は展開
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

        // 括弧のバランスをチェック
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
                // 文を示すキーワード
                TokenKind::KwIf | TokenKind::KwWhile | TokenKind::KwFor
                | TokenKind::KwSwitch | TokenKind::KwReturn | TokenKind::KwGoto => {
                    // 条件式の一部でない場合は文
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

        // 全ての括弧が閉じていること
        paren_depth == 0 && brace_depth == 0 && bracket_depth == 0
    }

    /// Phase 4: 戻り値型を推論
    fn infer_return_types(&mut self, macros: &MacroTable) {
        // トポロジカルソートで依存関係順に処理
        let order = self.topological_sort();

        for name in order {
            if let Some(def) = macros.get(name) {
                if let Some(info) = self.info.get(&name) {
                    if info.category == MacroCategory::Expression && info.is_target {
                        let return_type = self.infer_return_type(def, macros);
                        if let Some(info) = self.info.get_mut(&name) {
                            info.return_type = return_type;
                        }
                    }
                }
            }
        }
    }

    /// トポロジカルソート（依存しないマクロから順に）
    fn topological_sort(&self) -> Vec<InternedStr> {
        let mut result = Vec::new();
        let mut visited = HashSet::new();
        let mut in_progress = HashSet::new();

        for name in self.info.keys() {
            self.visit_for_sort(*name, &mut result, &mut visited, &mut in_progress);
        }

        result
    }

    fn visit_for_sort(
        &self,
        name: InternedStr,
        result: &mut Vec<InternedStr>,
        visited: &mut HashSet<InternedStr>,
        in_progress: &mut HashSet<InternedStr>,
    ) {
        if visited.contains(&name) {
            return;
        }
        if in_progress.contains(&name) {
            // 循環参照
            return;
        }

        in_progress.insert(name);

        if let Some(info) = self.info.get(&name) {
            for &used in &info.uses {
                self.visit_for_sort(used, result, visited, in_progress);
            }
        }

        in_progress.remove(&name);
        visited.insert(name);
        result.push(name);
    }

    /// 単一マクロの戻り値型を推論
    fn infer_return_type(&self, def: &MacroDef, macros: &MacroTable) -> Option<String> {
        let body = &def.body;

        if body.is_empty() {
            return None;
        }

        // 展開したトークン列を解析
        let expanded = self.expand_macro_body(def, macros, &mut HashSet::new());

        // 型推論を試みる
        self.infer_type_from_tokens(&expanded, def, macros)
    }

    /// トークン列から型を推論
    fn infer_type_from_tokens(
        &self,
        tokens: &[Token],
        def: &MacroDef,
        _macros: &MacroTable,
    ) -> Option<String> {
        if tokens.is_empty() {
            return None;
        }

        // キャストがある場合はその型
        if let Some(ty) = self.extract_cast_type(tokens) {
            return Some(ty);
        }

        // 数値リテラルの場合
        if tokens.len() == 1 {
            match &tokens[0].kind {
                TokenKind::IntLit(_) => return Some("c_int".to_string()),
                TokenKind::UIntLit(_) => return Some("c_uint".to_string()),
                TokenKind::FloatLit(_) => return Some("c_double".to_string()),
                TokenKind::StringLit(_) => return Some("*const c_char".to_string()),
                _ => {}
            }
        }

        // メンバアクセス式 (x)->field の場合、フィールド辞書から型を推論
        if let Some(ty) = self.infer_from_member_access(tokens, def) {
            return Some(ty);
        }

        // 比較演算子がある場合はbool
        for token in tokens {
            match &token.kind {
                TokenKind::EqEq | TokenKind::BangEq
                | TokenKind::Lt | TokenKind::Gt
                | TokenKind::LtEq | TokenKind::GtEq
                | TokenKind::AmpAmp | TokenKind::PipePipe => {
                    return Some("bool".to_string());
                }
                _ => {}
            }
        }

        // 他のマクロを呼び出している場合、そのマクロの戻り値型を使用
        for token in tokens {
            if let TokenKind::Ident(ident) = token.kind {
                if let Some(info) = self.info.get(&ident) {
                    if let Some(ref ret_ty) = info.return_type {
                        return Some(ret_ty.clone());
                    }
                }
            }
        }

        None
    }

    /// キャスト式から型を抽出
    fn extract_cast_type(&self, tokens: &[Token]) -> Option<String> {
        // (type)expr パターンを探す
        if tokens.len() < 4 {
            return None;
        }

        if !matches!(tokens[0].kind, TokenKind::LParen) {
            return None;
        }

        // 閉じ括弧を探す
        let mut depth = 1;
        let mut close_idx = None;
        for (i, token) in tokens[1..].iter().enumerate() {
            match token.kind {
                TokenKind::LParen => depth += 1,
                TokenKind::RParen => {
                    depth -= 1;
                    if depth == 0 {
                        close_idx = Some(i + 1);
                        break;
                    }
                }
                _ => {}
            }
        }

        let close_idx = close_idx?;

        // 括弧内が型名かどうかをチェック
        let type_tokens = &tokens[1..close_idx];
        if self.looks_like_type(type_tokens) {
            return Some(self.tokens_to_string(type_tokens));
        }

        None
    }

    /// トークン列が型名っぽいかチェック
    fn looks_like_type(&self, tokens: &[Token]) -> bool {
        if tokens.is_empty() {
            return false;
        }

        // 型キーワードで始まる
        match &tokens[0].kind {
            TokenKind::KwVoid | TokenKind::KwChar | TokenKind::KwShort
            | TokenKind::KwInt | TokenKind::KwLong | TokenKind::KwFloat
            | TokenKind::KwDouble | TokenKind::KwSigned | TokenKind::KwUnsigned
            | TokenKind::KwStruct | TokenKind::KwUnion | TokenKind::KwEnum
            | TokenKind::KwConst | TokenKind::KwVolatile => true,
            TokenKind::Ident(_) => {
                // typedef名の可能性
                // ポインタ修飾子が続く場合は型として扱う
                tokens.iter().any(|t| matches!(t.kind, TokenKind::Star))
            }
            _ => false,
        }
    }

    /// メンバアクセスから型を推論
    fn infer_from_member_access(&self, tokens: &[Token], _def: &MacroDef) -> Option<String> {
        // (param)->field パターンを探す
        // または param->field パターン
        for (i, token) in tokens.iter().enumerate() {
            if matches!(token.kind, TokenKind::Arrow) {
                if i + 1 < tokens.len() {
                    if let TokenKind::Ident(field_name) = tokens[i + 1].kind {
                        let field_str = self.interner.get(field_name);

                        // フィールド辞書から構造体型を取得
                        if let Some(struct_name) = self.fields_dict.lookup_unique(field_name) {
                            let struct_str = self.interner.get(struct_name);

                            // 既知のフィールド型を返す
                            if let Some(ty) = self.get_field_type(struct_str, field_str) {
                                return Some(ty);
                            }

                            // フィールド辞書にあるが既知の型がない場合は構造体へのポインタを返す
                            return Some(format!("*mut {}", struct_str));
                        }
                    }
                }
            }
        }

        None
    }

    /// 既知のフィールド型を取得
    fn get_field_type(&self, struct_name: &str, field_name: &str) -> Option<String> {
        // Perl SVの既知フィールド
        match (struct_name, field_name) {
            ("sv", "sv_any") => Some("*mut c_void".to_string()),
            ("sv", "sv_refcnt") => Some("U32".to_string()),
            ("sv", "sv_flags") => Some("U32".to_string()),
            _ => None,
        }
    }

    /// トークン列を文字列に変換
    fn tokens_to_string(&self, tokens: &[Token]) -> String {
        let mut result = String::new();
        for token in tokens {
            if !result.is_empty() {
                result.push(' ');
            }
            result.push_str(&self.token_to_string(token));
        }
        result
    }

    /// 単一トークンを文字列に変換
    fn token_to_string(&self, token: &Token) -> String {
        match &token.kind {
            TokenKind::Ident(id) => self.interner.get(*id).to_string(),
            TokenKind::IntLit(n) => n.to_string(),
            TokenKind::Star => "*".to_string(),
            TokenKind::KwVoid => "void".to_string(),
            TokenKind::KwChar => "char".to_string(),
            TokenKind::KwShort => "short".to_string(),
            TokenKind::KwInt => "int".to_string(),
            TokenKind::KwLong => "long".to_string(),
            TokenKind::KwFloat => "float".to_string(),
            TokenKind::KwDouble => "double".to_string(),
            TokenKind::KwSigned => "signed".to_string(),
            TokenKind::KwUnsigned => "unsigned".to_string(),
            TokenKind::KwConst => "const".to_string(),
            TokenKind::KwVolatile => "volatile".to_string(),
            TokenKind::KwStruct => "struct".to_string(),
            TokenKind::KwUnion => "union".to_string(),
            TokenKind::KwEnum => "enum".to_string(),
            _ => format!("{:?}", token.kind),
        }
    }

    /// 位置が対象ディレクトリ内かチェック
    fn is_target_location(&self, _loc: &SourceLocation) -> bool {
        // TODO: ファイルパスを取得して判定
        // 今は全てを対象とする
        true
    }

    /// 解析結果を取得
    pub fn get_info(&self, name: InternedStr) -> Option<&MacroInfo> {
        self.info.get(&name)
    }

    /// 全解析結果をイテレート
    pub fn iter(&self) -> impl Iterator<Item = (&InternedStr, &MacroInfo)> {
        self.info.iter()
    }

    /// 式マクロのみをイテレート（対象ディレクトリ内）
    pub fn expression_macros(&self) -> impl Iterator<Item = (&InternedStr, &MacroInfo)> {
        self.info.iter().filter(|(_, info)| {
            info.category == MacroCategory::Expression && info.is_target
        })
    }

    /// マクロ本体をパースしてASTを取得
    ///
    /// 式マクロの本体をC言語の式としてパースし、AST（抽象構文木）を返す。
    /// オブジェクトマクロは再帰的に展開され、関数マクロは識別子として残る。
    ///
    /// # Arguments
    /// * `def` - パース対象のマクロ定義
    /// * `macros` - マクロテーブル（再帰展開用）
    ///
    /// # Returns
    /// パースが成功した場合は `Ok(Expr)`、失敗した場合は `Err`
    pub fn parse_macro_body(&self, def: &MacroDef, macros: &MacroTable) -> Result<Expr> {
        // 1. マクロ本体を再帰的に展開（オブジェクトマクロのみ）
        let expanded = self.expand_macro_body(def, macros, &mut HashSet::new());

        // 2. トークン列をパース
        // 注: インターナーとファイルレジストリはクローンが必要
        // （パーサーが可変参照を要求するため）
        let interner = self.interner.clone();
        let files = self.files.clone();

        parse_expression_from_tokens(expanded, interner, files)
    }

    /// 展開済みマクロ本体を取得
    ///
    /// デバッグや中間結果の確認用。
    pub fn get_expanded_body(&self, def: &MacroDef, macros: &MacroTable) -> Vec<Token> {
        self.expand_macro_body(def, macros, &mut HashSet::new())
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

        format!(
            "=== Macro Analysis Stats ===\n\
             Total macros: {}\n\
             Target macros: {}\n\
             Expression macros: {}\n\
             Statement macros: {}\n\
             Other macros: {}\n\
             With inferred type: {}\n",
            total, target, expressions, statements, other, with_type
        )
    }

    /// Def-Use chain をダンプ
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

            if !info.used_by.is_empty() {
                let mut used_by: Vec<_> = info.used_by.iter()
                    .map(|n| self.interner.get(*n))
                    .collect();
                used_by.sort();
                result.push_str(&format!("  used_by: {}\n", used_by.join(", ")));
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
