//! マクロ型推論エンジン
//!
//! マクロ定義から型情報を推論するためのモジュール。
//! ExprId を活用し、複数ソースからの型制約を収集・管理する。

use std::collections::{HashMap, HashSet};

use crate::apidoc::ApidocDict;
use crate::ast::{BlockItem, Expr, ExprKind};
use crate::fields_dict::FieldsDict;
use crate::intern::{InternedStr, StringInterner};
use crate::macro_def::{MacroDef, MacroKind, MacroTable};
use crate::parser::{parse_expression_from_tokens_ref, parse_statement_from_tokens_ref};
use crate::rust_decl::RustDeclDict;
use crate::semantic::SemanticAnalyzer;
use crate::source::FileRegistry;
use crate::token::TokenKind;
use crate::token_expander::TokenExpander;
use crate::type_env::{ConstraintSource, TypeConstraint, TypeEnv};

/// マクロのパース結果
#[derive(Debug, Clone)]
pub enum ParseResult {
    /// 式としてパース成功
    Expression(Box<Expr>),
    /// 文としてパース成功
    Statement(Vec<BlockItem>),
    /// パース不能（エラーメッセージ付き）
    Unparseable(Option<String>),
}

/// 推論状態
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InferStatus {
    /// 未処理
    Pending,
    /// 全ての型が確定
    TypeComplete,
    /// 一部の型が未確定
    TypeIncomplete,
    /// 型推論不能
    TypeUnknown,
}

impl Default for InferStatus {
    fn default() -> Self {
        Self::Pending
    }
}

/// マクロの型推論情報
#[derive(Debug, Clone)]
pub struct MacroInferInfo {
    /// マクロ名
    pub name: InternedStr,
    /// ターゲットマクロかどうか
    pub is_target: bool,
    /// マクロ本体にトークンがあるかどうか
    pub has_body: bool,
    /// 関数形式マクロかどうか
    pub is_function: bool,

    /// このマクロが使用する他のマクロ（def-use 関係）
    pub uses: HashSet<InternedStr>,
    /// このマクロを使用するマクロ（use-def 関係）
    pub used_by: HashSet<InternedStr>,

    /// THX 依存（aTHX, tTHX, my_perl を含む）
    pub is_thx_dependent: bool,

    /// トークン連結 (##) を含む（推移的）
    pub has_token_pasting: bool,

    /// パース結果
    pub parse_result: ParseResult,

    /// 型環境（収集された型制約）
    pub type_env: TypeEnv,

    /// 引数の型推論状態
    pub args_infer_status: InferStatus,

    /// 戻り値の型推論状態
    pub return_infer_status: InferStatus,
}

impl MacroInferInfo {
    /// 新しい MacroInferInfo を作成
    pub fn new(name: InternedStr) -> Self {
        Self {
            name,
            is_target: false,
            has_body: false,
            is_function: false,
            uses: HashSet::new(),
            used_by: HashSet::new(),
            is_thx_dependent: false,
            has_token_pasting: false,
            parse_result: ParseResult::Unparseable(None),
            type_env: TypeEnv::new(),
            args_infer_status: InferStatus::Pending,
            return_infer_status: InferStatus::Pending,
        }
    }

    /// 引数と戻り値の両方が確定しているか
    pub fn is_fully_confirmed(&self) -> bool {
        self.args_infer_status == InferStatus::TypeComplete
            && self.return_infer_status == InferStatus::TypeComplete
    }

    /// 使用するマクロを追加
    pub fn add_use(&mut self, used_macro: InternedStr) {
        self.uses.insert(used_macro);
    }

    /// 使用されるマクロを追加
    pub fn add_used_by(&mut self, user_macro: InternedStr) {
        self.used_by.insert(user_macro);
    }

    /// パース結果が式かどうか
    pub fn is_expression(&self) -> bool {
        matches!(self.parse_result, ParseResult::Expression(_))
    }

    /// パース結果が文かどうか
    pub fn is_statement(&self) -> bool {
        matches!(self.parse_result, ParseResult::Statement(_))
    }

    /// パース可能かどうか
    pub fn is_parseable(&self) -> bool {
        !matches!(self.parse_result, ParseResult::Unparseable(_))
    }

    /// マクロの戻り値型を取得
    ///
    /// 1. return_constraints があればそれを使用
    /// 2. 式マクロの場合、ルート式の型制約を使用
    pub fn get_return_type(&self) -> Option<&str> {
        // まず return_constraints を確認
        if let Some(ty) = self.type_env.get_return_type() {
            return Some(ty);
        }

        // 式マクロの場合、ルート式の型を取得
        if let ParseResult::Expression(ref expr) = self.parse_result {
            if let Some(constraints) = self.type_env.get_expr_constraints(expr.id) {
                // 最初の制約の型を返す
                if let Some(constraint) = constraints.first() {
                    return Some(&constraint.ty);
                }
            }
        }

        None
    }
}

/// マクロ型推論コンテキスト
///
/// 全マクロの型推論を管理する。
pub struct MacroInferContext {
    /// マクロ名 → 推論情報
    pub macros: HashMap<InternedStr, MacroInferInfo>,

    /// 型確定済みマクロ
    pub confirmed: HashSet<InternedStr>,

    /// 型未確定マクロ
    pub unconfirmed: HashSet<InternedStr>,

    /// 型推論不能マクロ
    pub unknown: HashSet<InternedStr>,
}

impl MacroInferContext {
    /// 新しいコンテキストを作成
    pub fn new() -> Self {
        Self {
            macros: HashMap::new(),
            confirmed: HashSet::new(),
            unconfirmed: HashSet::new(),
            unknown: HashSet::new(),
        }
    }

    /// マクロ情報を登録
    pub fn register(&mut self, info: MacroInferInfo) {
        let name = info.name;
        self.macros.insert(name, info);
    }

    /// マクロ情報を取得
    pub fn get(&self, name: InternedStr) -> Option<&MacroInferInfo> {
        self.macros.get(&name)
    }

    /// マクロ情報を可変で取得
    pub fn get_mut(&mut self, name: InternedStr) -> Option<&mut MacroInferInfo> {
        self.macros.get_mut(&name)
    }

    /// def-use 関係を構築
    ///
    /// 各マクロの uses 情報から used_by を逆引きで構築する。
    pub fn build_use_relations(&mut self) {
        // まず uses 情報を収集
        let use_pairs: Vec<(InternedStr, InternedStr)> = self
            .macros
            .iter()
            .flat_map(|(user, info)| {
                info.uses
                    .iter()
                    .map(move |used| (*user, *used))
            })
            .collect();

        // used_by を設定
        for (user, used) in use_pairs {
            if let Some(used_info) = self.macros.get_mut(&used) {
                used_info.add_used_by(user);
            }
        }
    }

    /// 初期分類を行う
    ///
    /// 各マクロの状態に基づいて confirmed/unconfirmed/unknown に分類する。
    pub fn classify_initial(&mut self) {
        for (name, info) in &self.macros {
            if info.is_fully_confirmed() {
                self.confirmed.insert(*name);
            } else if info.args_infer_status == InferStatus::TypeUnknown
                || info.return_infer_status == InferStatus::TypeUnknown
            {
                self.unknown.insert(*name);
            } else {
                self.unconfirmed.insert(*name);
            }
        }
    }

    /// 推論候補を取得
    ///
    /// 未確定マクロのうち、使用するマクロが全て確定済みのものを返す。
    /// 使用マクロ数の少ない順にソート。
    pub fn get_inference_candidates(&self) -> Vec<InternedStr> {
        let mut candidates: Vec<_> = self
            .unconfirmed
            .iter()
            .filter(|name| {
                if let Some(info) = self.macros.get(name) {
                    // 使用するマクロが全て confirmed に含まれているか
                    info.uses.iter().all(|used| {
                        self.confirmed.contains(used) || !self.macros.contains_key(used)
                    })
                } else {
                    false
                }
            })
            .copied()
            .collect();

        // 使用マクロ数でソート
        candidates.sort_by_key(|name| {
            self.macros
                .get(name)
                .map(|info| info.uses.len())
                .unwrap_or(0)
        });

        candidates
    }

    /// マクロを確定済みに移動
    pub fn mark_confirmed(&mut self, name: InternedStr) {
        self.unconfirmed.remove(&name);
        self.confirmed.insert(name);
        if let Some(info) = self.macros.get_mut(&name) {
            info.args_infer_status = InferStatus::TypeComplete;
            info.return_infer_status = InferStatus::TypeComplete;
        }
    }

    /// マクロを未知に移動（引数側）
    pub fn mark_args_unknown(&mut self, name: InternedStr) {
        if let Some(info) = self.macros.get_mut(&name) {
            info.args_infer_status = InferStatus::TypeUnknown;
        }
    }

    /// マクロを未知に移動（戻り値側）
    pub fn mark_return_unknown(&mut self, name: InternedStr) {
        if let Some(info) = self.macros.get_mut(&name) {
            info.return_infer_status = InferStatus::TypeUnknown;
        }
    }

    /// マクロを unknown 集合に移動
    pub fn move_to_unknown(&mut self, name: InternedStr) {
        self.unconfirmed.remove(&name);
        self.unknown.insert(name);
    }

    /// 統計情報を取得
    pub fn stats(&self) -> MacroInferStats {
        let mut args_unknown = 0;
        let mut return_unknown = 0;
        for info in self.macros.values() {
            if info.args_infer_status == InferStatus::TypeUnknown {
                args_unknown += 1;
            }
            if info.return_infer_status == InferStatus::TypeUnknown {
                return_unknown += 1;
            }
        }
        MacroInferStats {
            total: self.macros.len(),
            confirmed: self.confirmed.len(),
            unconfirmed: self.unconfirmed.len(),
            args_unknown,
            return_unknown,
        }
    }

    /// Phase 1: MacroInferInfo の初期構築（パースまで、型推論なし）
    ///
    /// 返り値: (info, has_pasting_direct, has_thx_direct)
    /// - has_pasting_direct: マクロ本体に直接 ## が含まれるか
    /// - has_thx_direct: マクロ本体に直接 aTHX/tTHX/my_perl が含まれるか
    pub fn build_macro_info(
        &self,
        def: &MacroDef,
        macro_table: &MacroTable,
        interner: &StringInterner,
        files: &FileRegistry,
        rust_decl_dict: Option<&RustDeclDict>,
        typedefs: &HashSet<InternedStr>,
        thx_symbols: (InternedStr, InternedStr, InternedStr),
    ) -> (MacroInferInfo, bool, bool) {
        let mut info = MacroInferInfo::new(def.name);
        info.is_target = def.is_target;
        info.has_body = !def.body.is_empty();
        info.is_function = matches!(def.kind, MacroKind::Function { .. });

        // 直接 ## を含むかチェック
        let has_pasting_direct = def.body.iter().any(|t| matches!(t.kind, TokenKind::HashHash));

        // 直接 THX トークンを含むかチェック
        let (sym_athx, sym_tthx, sym_my_perl) = thx_symbols;
        let has_thx_direct = def.body.iter().any(|t| {
            if let TokenKind::Ident(id) = t.kind {
                id == sym_athx || id == sym_tthx || id == sym_my_perl
            } else {
                false
            }
        });

        // 初期値を設定（後で propagate で上書きされる可能性あり）
        info.has_token_pasting = has_pasting_direct;
        info.is_thx_dependent = has_thx_direct;

        // マクロ本体を展開（TokenExpander を使用）
        let mut expander = TokenExpander::new(macro_table, interner, files);
        if let Some(dict) = rust_decl_dict {
            expander.set_bindings_consts(&dict.consts);
        }
        let expanded_tokens = expander.expand_with_calls(&def.body);

        // def-use 関係を収集（展開後のトークンから識別子を抽出）
        self.collect_uses(&expanded_tokens, macro_table, &mut info);

        // パースを試行
        info.parse_result = self.try_parse_tokens(&expanded_tokens, interner, files, typedefs);

        (info, has_pasting_direct, has_thx_direct)
    }

    /// Phase 2: 型推論の適用
    ///
    /// 既に登録済みの MacroInferInfo に対して型制約を収集する
    pub fn infer_macro_types<'a>(
        &mut self,
        name: InternedStr,
        params: &[InternedStr],
        interner: &'a StringInterner,
        files: &FileRegistry,
        apidoc: Option<&'a ApidocDict>,
        fields_dict: Option<&'a FieldsDict>,
        rust_decl_dict: Option<&'a RustDeclDict>,
        typedefs: &HashSet<InternedStr>,
    ) {
        // 先に確定済みマクロの戻り値型を収集（借用の競合を避けるため）
        let confirmed_return_types = self.collect_confirmed_return_types(interner);

        let info = match self.macros.get_mut(&name) {
            Some(info) => info,
            None => return,
        };

        // パース成功した場合、型制約を収集
        if let ParseResult::Expression(ref expr) = info.parse_result {
            let mut analyzer = SemanticAnalyzer::with_rust_decl_dict(
                interner,
                apidoc,
                fields_dict,
                rust_decl_dict,
            );

            // 確定済みマクロの戻り値型を登録
            for (macro_name, return_type) in &confirmed_return_types {
                analyzer.register_macro_return_type(macro_name, return_type);
            }

            // apidoc 型情報付きでパラメータをシンボルテーブルに登録
            analyzer.register_macro_params_from_apidoc(name, params, files, typedefs);

            // 全式の型制約を収集
            analyzer.collect_expr_constraints(expr, &mut info.type_env);

            // マクロ自体の戻り値型を制約として追加
            if let Some(apidoc_dict) = apidoc {
                let macro_name_str = interner.get(name);
                if let Some(entry) = apidoc_dict.get(macro_name_str) {
                    if let Some(ref return_type) = entry.return_type {
                        info.type_env.add_return_constraint(TypeConstraint::new(
                            expr.id,
                            return_type,
                            ConstraintSource::Apidoc,
                            format!("return type of macro {}", macro_name_str),
                        ));
                    }
                }
            }
        }

        // Statement の場合も型制約を収集
        if let ParseResult::Statement(ref block_items) = info.parse_result {
            let mut analyzer = SemanticAnalyzer::with_rust_decl_dict(
                interner,
                apidoc,
                fields_dict,
                rust_decl_dict,
            );

            // 確定済みマクロの戻り値型を登録
            for (macro_name, return_type) in &confirmed_return_types {
                analyzer.register_macro_return_type(macro_name, return_type);
            }

            // apidoc 型情報付きでパラメータをシンボルテーブルに登録
            analyzer.register_macro_params_from_apidoc(name, params, files, typedefs);

            // 各 BlockItem について型制約を収集
            for item in block_items {
                if let BlockItem::Stmt(stmt) = item {
                    analyzer.collect_stmt_constraints(stmt, &mut info.type_env);
                }
            }
        }
    }

    /// 確定済みマクロの戻り値型を収集
    fn collect_confirmed_return_types(&self, interner: &StringInterner) -> Vec<(String, String)> {
        let mut result = Vec::new();
        for confirmed_name in &self.confirmed {
            if let Some(confirmed_info) = self.macros.get(confirmed_name) {
                // MacroInferInfo::get_return_type() を使用（ルート式の型も考慮）
                if let Some(return_type) = confirmed_info.get_return_type() {
                    let name_str = interner.get(*confirmed_name);
                    result.push((name_str.to_string(), return_type.to_string()));
                }
            }
        }
        result
    }

    /// マクロを解析して MacroInferInfo を作成（従来のAPI - 互換性のため保持）
    ///
    /// 1. マクロ本体をパース（式 or 文）
    /// 2. def-use 関係を収集（使用するマクロ/関数）
    /// 3. 初期型制約を収集
    #[allow(dead_code)]
    pub fn analyze_macro<'a>(
        &mut self,
        def: &MacroDef,
        macro_table: &MacroTable,
        thx_macros: &HashSet<InternedStr>,
        pasting_macros: &HashSet<InternedStr>,
        interner: &'a StringInterner,
        files: &FileRegistry,
        apidoc: Option<&'a ApidocDict>,
        fields_dict: Option<&'a FieldsDict>,
        rust_decl_dict: Option<&'a RustDeclDict>,
        typedefs: &HashSet<InternedStr>,
    ) {
        let mut info = MacroInferInfo::new(def.name);
        info.is_target = def.is_target;
        info.has_body = !def.body.is_empty();
        info.is_function = matches!(def.kind, MacroKind::Function { .. });
        info.is_thx_dependent = thx_macros.contains(&def.name);
        info.has_token_pasting = pasting_macros.contains(&def.name);

        // 関数形式マクロの場合、パラメータを取得
        let params: Vec<InternedStr> = match &def.kind {
            MacroKind::Function { params, .. } => params.clone(),
            MacroKind::Object => vec![],
        };

        // マクロ本体を展開（TokenExpander を使用）
        // expand_with_calls() を使用して関数形式マクロも展開
        // （DEBUG_l 等の関数マクロが複合文を引数に取る場合に必要）
        let mut expander = TokenExpander::new(macro_table, interner, files);
        if let Some(dict) = rust_decl_dict {
            expander.set_bindings_consts(&dict.consts);
        }
        let expanded_tokens = expander.expand_with_calls(&def.body);

        // def-use 関係を収集（展開後のトークンから識別子を抽出）
        self.collect_uses(&expanded_tokens, macro_table, &mut info);

        // パースを試行
        info.parse_result = self.try_parse_tokens(&expanded_tokens, interner, files, typedefs);

        // パース成功した場合、型制約を収集
        if let ParseResult::Expression(ref expr) = info.parse_result {
            let mut analyzer = SemanticAnalyzer::with_rust_decl_dict(
                interner,
                apidoc,
                fields_dict,
                rust_decl_dict,
            );

            // apidoc 型情報付きでパラメータをシンボルテーブルに登録
            analyzer.register_macro_params_from_apidoc(def.name, &params, files, typedefs);

            // 全式の型制約を収集（collect_expr_constraints が全式の型を計算）
            analyzer.collect_expr_constraints(expr, &mut info.type_env);

            // マクロ自体の戻り値型を制約として追加
            if let Some(apidoc_dict) = apidoc {
                let macro_name_str = interner.get(def.name);
                if let Some(entry) = apidoc_dict.get(macro_name_str) {
                    if let Some(ref return_type) = entry.return_type {
                        info.type_env.add_return_constraint(TypeConstraint::new(
                            expr.id,
                            return_type,
                            ConstraintSource::Apidoc,
                            format!("return type of macro {}", macro_name_str),
                        ));
                    }
                }
            }
        }

        // Statement の場合も型制約を収集
        if let ParseResult::Statement(ref block_items) = info.parse_result {
            let mut analyzer = SemanticAnalyzer::with_rust_decl_dict(
                interner,
                apidoc,
                fields_dict,
                rust_decl_dict,
            );

            // apidoc 型情報付きでパラメータをシンボルテーブルに登録
            analyzer.register_macro_params_from_apidoc(def.name, &params, files, typedefs);

            // 各 BlockItem について型制約を収集
            for item in block_items {
                if let BlockItem::Stmt(stmt) = item {
                    analyzer.collect_stmt_constraints(stmt, &mut info.type_env);
                }
            }
        }

        self.register(info);
    }

    /// トークン列から使用するマクロ/関数を収集
    fn collect_uses(
        &self,
        tokens: &[crate::token::Token],
        macro_table: &MacroTable,
        info: &mut MacroInferInfo,
    ) {
        for token in tokens {
            if let TokenKind::Ident(id) = &token.kind {
                // マクロテーブルに存在する識別子は使用マクロ
                if macro_table.get(*id).is_some() && *id != info.name {
                    info.add_use(*id);
                }
            }
        }
    }

    /// トークン列を式または文としてパース試行
    fn try_parse_tokens(
        &self,
        tokens: &[crate::token::Token],
        interner: &StringInterner,
        files: &FileRegistry,
        typedefs: &HashSet<InternedStr>,
    ) -> ParseResult {
        if tokens.is_empty() {
            return ParseResult::Unparseable(Some("empty token sequence".to_string()));
        }

        // 空白・改行をスキップして最初の有効なトークンを探す
        let first_significant = tokens.iter().find(|t| {
            !matches!(t.kind, TokenKind::Space | TokenKind::Newline)
        });

        // 先頭トークンが KwDo または KwIf なら文としてパース試行
        let is_statement_start = first_significant
            .is_some_and(|t| matches!(t.kind, TokenKind::KwDo | TokenKind::KwIf));
        if is_statement_start {
            match parse_statement_from_tokens_ref(tokens.to_vec(), interner, files, typedefs) {
                Ok(stmt) => return ParseResult::Statement(vec![BlockItem::Stmt(stmt)]),
                Err(_) => {} // フォールスルーして式としてパース
            }
        }

        // 式としてパースを試行
        match parse_expression_from_tokens_ref(tokens.to_vec(), interner, files, typedefs) {
            Ok(expr) => ParseResult::Expression(Box::new(expr)),
            Err(err) => ParseResult::Unparseable(Some(err.format_with_files(files))),
        }
    }

    /// 全マクロから THX 依存関係を収集（定義順序に依存しない）
    ///
    /// 2パスで推移的閉包を計算:
    /// 1. 直接 aTHX, tTHX, my_perl を含むマクロを収集
    /// 2. THX マクロを使用するマクロも THX 依存として追加（収束まで繰り返し）
    ///
    /// Note: 現在は propagate_flag_via_used_by で代替
    #[allow(dead_code)]
    fn collect_thx_dependencies(
        &self,
        macro_table: &MacroTable,
        thx_symbols: (InternedStr, InternedStr, InternedStr),
    ) -> HashSet<InternedStr> {
        let (sym_athx, sym_tthx, sym_my_perl) = thx_symbols;

        // Phase 1: 直接 THX トークンを含むマクロを収集
        let mut thx_macros = HashSet::new();
        for (name, def) in macro_table.iter() {
            for token in &def.body {
                if let TokenKind::Ident(id) = token.kind {
                    if id == sym_athx || id == sym_tthx || id == sym_my_perl {
                        thx_macros.insert(*name);
                        break;
                    }
                }
            }
        }

        // Phase 2: 推移的閉包を計算（THX マクロを使用するマクロも THX 依存）
        loop {
            let mut added = false;
            for (name, def) in macro_table.iter() {
                if thx_macros.contains(name) {
                    continue;
                }
                for token in &def.body {
                    if let TokenKind::Ident(id) = token.kind {
                        if thx_macros.contains(&id) {
                            thx_macros.insert(*name);
                            added = true;
                            break;
                        }
                    }
                }
            }
            if !added {
                break;
            }
        }

        thx_macros
    }

    /// トークン連結 (##) 依存を収集（推移的閉包）
    ///
    /// Note: 現在は propagate_flag_via_used_by で代替
    #[allow(dead_code)]
    fn collect_pasting_dependencies(
        &self,
        macro_table: &MacroTable,
    ) -> HashSet<InternedStr> {
        // Phase 1: 直接 ## を含むマクロを収集
        let mut pasting_macros = HashSet::new();
        for (name, def) in macro_table.iter() {
            for token in &def.body {
                if matches!(token.kind, TokenKind::HashHash) {
                    pasting_macros.insert(*name);
                    break;
                }
            }
        }

        // Phase 2: 推移的閉包を計算（## マクロを使用するマクロも ## 依存）
        loop {
            let mut added = false;
            for (name, def) in macro_table.iter() {
                if pasting_macros.contains(name) {
                    continue;
                }
                for token in &def.body {
                    if let TokenKind::Ident(id) = token.kind {
                        if pasting_macros.contains(&id) {
                            pasting_macros.insert(*name);
                            added = true;
                            break;
                        }
                    }
                }
            }
            if !added {
                break;
            }
        }

        pasting_macros
    }

    /// 全ターゲットマクロを解析
    ///
    /// MacroTable 内の全ターゲットマクロに対して analyze_macro を実行し、
    /// def-use 関係を構築して初期分類を行う。
    pub fn analyze_all_macros<'a>(
        &mut self,
        macro_table: &MacroTable,
        interner: &'a StringInterner,
        files: &FileRegistry,
        apidoc: Option<&'a ApidocDict>,
        fields_dict: Option<&'a FieldsDict>,
        rust_decl_dict: Option<&'a RustDeclDict>,
        typedefs: &HashSet<InternedStr>,
        thx_symbols: (InternedStr, InternedStr, InternedStr),
    ) {
        // Step 1: 全マクロの初期構築（パースのみ、型推論なし）
        let mut thx_initial = HashSet::new();
        let mut pasting_initial = HashSet::new();

        for def in macro_table.iter_target_macros() {
            let (info, has_pasting, has_thx) = self.build_macro_info(
                def, macro_table, interner, files, rust_decl_dict, typedefs, thx_symbols
            );
            if has_pasting {
                pasting_initial.insert(def.name);
            }
            if has_thx {
                thx_initial.insert(def.name);
            }
            self.register(info);
        }

        // Step 2: used_by を構築
        self.build_use_relations();

        // Step 3: THX の推移閉包を計算（used_by 経由）
        self.propagate_flag_via_used_by(&thx_initial, true);

        // Step 4: ## の推移閉包を計算（used_by 経由）
        self.propagate_flag_via_used_by(&pasting_initial, false);

        // Step 5: 全マクロを unconfirmed に
        for name in self.macros.keys().copied().collect::<Vec<_>>() {
            self.unconfirmed.insert(name);
        }

        // Step 6: 依存順に型推論
        self.infer_types_in_dependency_order(
            macro_table, interner, files, apidoc, fields_dict, rust_decl_dict, typedefs
        );
    }

    /// used_by を辿ってフラグを推移的に伝播
    ///
    /// is_thx が true の場合は is_thx_dependent を、false の場合は has_token_pasting を設定
    fn propagate_flag_via_used_by(&mut self, initial_set: &HashSet<InternedStr>, is_thx: bool) {
        // 初期集合のフラグを設定
        for name in initial_set {
            if let Some(info) = self.macros.get_mut(name) {
                if is_thx {
                    info.is_thx_dependent = true;
                } else {
                    info.has_token_pasting = true;
                }
            }
        }

        // used_by を辿って伝播
        let mut to_propagate: Vec<InternedStr> = initial_set.iter().copied().collect();

        while let Some(name) = to_propagate.pop() {
            let used_by_list: Vec<InternedStr> = self.macros
                .get(&name)
                .map(|info| info.used_by.iter().copied().collect())
                .unwrap_or_default();

            for user in used_by_list {
                if let Some(user_info) = self.macros.get_mut(&user) {
                    let flag = if is_thx {
                        &mut user_info.is_thx_dependent
                    } else {
                        &mut user_info.has_token_pasting
                    };
                    if !*flag {
                        *flag = true;
                        to_propagate.push(user);
                    }
                }
            }
        }
    }

    /// 依存順に型推論を実行
    fn infer_types_in_dependency_order<'a>(
        &mut self,
        macro_table: &MacroTable,
        interner: &'a StringInterner,
        files: &FileRegistry,
        apidoc: Option<&'a ApidocDict>,
        fields_dict: Option<&'a FieldsDict>,
        rust_decl_dict: Option<&'a RustDeclDict>,
        typedefs: &HashSet<InternedStr>,
    ) {
        loop {
            let candidates = self.get_inference_candidates();
            if candidates.is_empty() {
                // 残りを全て unknown へ
                let remaining: Vec<_> = self.unconfirmed.iter().copied().collect();
                for name in remaining {
                    self.move_to_unknown(name);
                }
                break;
            }

            for name in candidates {
                // パラメータを取得
                let params: Vec<InternedStr> = macro_table
                    .get(name)
                    .map(|def| match &def.kind {
                        MacroKind::Function { params, .. } => params.clone(),
                        MacroKind::Object => vec![],
                    })
                    .unwrap_or_default();

                // 型推論を実行
                self.infer_macro_types(
                    name, &params, interner, files, apidoc, fields_dict, rust_decl_dict, typedefs
                );

                // 推論結果に基づいて分類
                let is_confirmed = self.macros.get(&name)
                    .map(|info| {
                        // 戻り値型が決まっていれば confirmed とする
                        // MacroInferInfo::get_return_type() を使用（ルート式の型も考慮）
                        info.get_return_type().is_some()
                    })
                    .unwrap_or(false);

                if is_confirmed {
                    self.mark_confirmed(name);
                } else {
                    self.move_to_unknown(name);
                }
            }
        }
    }

    /// 式から使用される関数/マクロを再帰的に収集
    pub fn collect_uses_from_expr(
        expr: &Expr,
        uses: &mut HashSet<InternedStr>,
    ) {
        match &expr.kind {
            ExprKind::Call { func, args } => {
                // 関数名を収集
                if let ExprKind::Ident(name) = &func.kind {
                    uses.insert(*name);
                }
                Self::collect_uses_from_expr(func, uses);
                for arg in args {
                    Self::collect_uses_from_expr(arg, uses);
                }
            }
            ExprKind::Ident(name) => {
                uses.insert(*name);
            }
            ExprKind::Binary { lhs, rhs, .. } => {
                Self::collect_uses_from_expr(lhs, uses);
                Self::collect_uses_from_expr(rhs, uses);
            }
            ExprKind::Cast { expr: inner, .. }
            | ExprKind::PreInc(inner)
            | ExprKind::PreDec(inner)
            | ExprKind::PostInc(inner)
            | ExprKind::PostDec(inner)
            | ExprKind::AddrOf(inner)
            | ExprKind::Deref(inner)
            | ExprKind::UnaryPlus(inner)
            | ExprKind::UnaryMinus(inner)
            | ExprKind::BitNot(inner)
            | ExprKind::LogNot(inner)
            | ExprKind::Sizeof(inner) => {
                Self::collect_uses_from_expr(inner, uses);
            }
            ExprKind::Index { expr: base, index } => {
                Self::collect_uses_from_expr(base, uses);
                Self::collect_uses_from_expr(index, uses);
            }
            ExprKind::Member { expr: base, .. } | ExprKind::PtrMember { expr: base, .. } => {
                Self::collect_uses_from_expr(base, uses);
            }
            ExprKind::Conditional { cond, then_expr, else_expr } => {
                Self::collect_uses_from_expr(cond, uses);
                Self::collect_uses_from_expr(then_expr, uses);
                Self::collect_uses_from_expr(else_expr, uses);
            }
            ExprKind::Assign { lhs, rhs, .. } => {
                Self::collect_uses_from_expr(lhs, uses);
                Self::collect_uses_from_expr(rhs, uses);
            }
            ExprKind::Comma { lhs, rhs } => {
                Self::collect_uses_from_expr(lhs, uses);
                Self::collect_uses_from_expr(rhs, uses);
            }
            _ => {}
        }
    }
}

impl Default for MacroInferContext {
    fn default() -> Self {
        Self::new()
    }
}

/// 推論統計
#[derive(Debug, Clone, Copy)]
pub struct MacroInferStats {
    pub total: usize,
    pub confirmed: usize,
    pub unconfirmed: usize,
    /// 引数の型が unknown のマクロ数
    pub args_unknown: usize,
    /// 戻り値の型が unknown のマクロ数
    pub return_unknown: usize,
}

impl std::fmt::Display for MacroInferStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "MacroInferStats {{ total: {}, confirmed: {}, unconfirmed: {}, args_unknown: {}, return_unknown: {} }}",
            self.total, self.confirmed, self.unconfirmed, self.args_unknown, self.return_unknown
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intern::StringInterner;

    #[test]
    fn test_macro_infer_info_new() {
        let mut interner = StringInterner::new();
        let name = interner.intern("MY_MACRO");

        let info = MacroInferInfo::new(name);

        assert_eq!(info.name, name);
        assert!(!info.is_target);
        assert!(!info.is_thx_dependent);
        assert!(!info.has_token_pasting);
        assert!(info.uses.is_empty());
        assert!(info.used_by.is_empty());
        assert!(!info.is_parseable());
        assert_eq!(info.args_infer_status, InferStatus::Pending);
        assert_eq!(info.return_infer_status, InferStatus::Pending);
    }

    #[test]
    fn test_macro_infer_context_register() {
        let mut interner = StringInterner::new();
        let name = interner.intern("FOO");

        let mut ctx = MacroInferContext::new();
        let info = MacroInferInfo::new(name);
        ctx.register(info);

        assert!(ctx.get(name).is_some());
        assert_eq!(ctx.macros.len(), 1);
    }

    #[test]
    fn test_build_use_relations() {
        let mut interner = StringInterner::new();
        let foo = interner.intern("FOO");
        let bar = interner.intern("BAR");
        let baz = interner.intern("BAZ");

        let mut ctx = MacroInferContext::new();

        // FOO uses BAR
        let mut foo_info = MacroInferInfo::new(foo);
        foo_info.add_use(bar);
        ctx.register(foo_info);

        // BAR uses BAZ
        let mut bar_info = MacroInferInfo::new(bar);
        bar_info.add_use(baz);
        ctx.register(bar_info);

        // BAZ is standalone
        let baz_info = MacroInferInfo::new(baz);
        ctx.register(baz_info);

        // Build relations
        ctx.build_use_relations();

        // BAR should be used_by FOO
        assert!(ctx.get(bar).unwrap().used_by.contains(&foo));
        // BAZ should be used_by BAR
        assert!(ctx.get(baz).unwrap().used_by.contains(&bar));
    }

    #[test]
    fn test_inference_candidates() {
        let mut interner = StringInterner::new();
        let foo = interner.intern("FOO");
        let bar = interner.intern("BAR");
        let baz = interner.intern("BAZ");

        let mut ctx = MacroInferContext::new();

        // FOO uses BAR
        let mut foo_info = MacroInferInfo::new(foo);
        foo_info.add_use(bar);
        ctx.register(foo_info);

        // BAR uses BAZ
        let mut bar_info = MacroInferInfo::new(bar);
        bar_info.add_use(baz);
        ctx.register(bar_info);

        // BAZ is standalone (confirmed)
        let mut baz_info = MacroInferInfo::new(baz);
        baz_info.args_infer_status = InferStatus::TypeComplete;
        baz_info.return_infer_status = InferStatus::TypeComplete;
        ctx.register(baz_info);

        ctx.classify_initial();

        // Initially, only BAZ is confirmed
        assert!(ctx.confirmed.contains(&baz));
        assert!(ctx.unconfirmed.contains(&foo));
        assert!(ctx.unconfirmed.contains(&bar));

        // Candidates: BAR (uses BAZ which is confirmed)
        let candidates = ctx.get_inference_candidates();
        assert_eq!(candidates, vec![bar]);

        // After confirming BAR
        ctx.mark_confirmed(bar);
        let candidates = ctx.get_inference_candidates();
        assert_eq!(candidates, vec![foo]);
    }
}
