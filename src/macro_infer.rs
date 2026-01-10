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
use crate::parser::parse_expression_from_tokens_ref;
use crate::rust_decl::RustDeclDict;
use crate::semantic::SemanticAnalyzer;
use crate::source::FileRegistry;
use crate::thx_collector::ThxCollector;
use crate::token::TokenKind;
use crate::token_expander::TokenExpander;
use crate::type_env::TypeEnv;

/// マクロのパース結果
#[derive(Debug, Clone)]
pub enum ParseResult {
    /// 式としてパース成功
    Expression(Box<Expr>),
    /// 文としてパース成功
    Statement(Vec<BlockItem>),
    /// パース不能
    Unparseable,
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

    /// このマクロが使用する他のマクロ（def-use 関係）
    pub uses: HashSet<InternedStr>,
    /// このマクロを使用するマクロ（use-def 関係）
    pub used_by: HashSet<InternedStr>,

    /// THX 依存（aTHX, tTHX, my_perl を含む）
    pub is_thx_dependent: bool,

    /// パース結果
    pub parse_result: ParseResult,

    /// 型環境（収集された型制約）
    pub type_env: TypeEnv,

    /// 推論状態
    pub infer_status: InferStatus,
}

impl MacroInferInfo {
    /// 新しい MacroInferInfo を作成
    pub fn new(name: InternedStr) -> Self {
        Self {
            name,
            is_target: false,
            uses: HashSet::new(),
            used_by: HashSet::new(),
            is_thx_dependent: false,
            parse_result: ParseResult::Unparseable,
            type_env: TypeEnv::new(),
            infer_status: InferStatus::Pending,
        }
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
        !matches!(self.parse_result, ParseResult::Unparseable)
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
            match info.infer_status {
                InferStatus::TypeComplete => {
                    self.confirmed.insert(*name);
                }
                InferStatus::TypeIncomplete | InferStatus::Pending => {
                    self.unconfirmed.insert(*name);
                }
                InferStatus::TypeUnknown => {
                    self.unknown.insert(*name);
                }
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
            info.infer_status = InferStatus::TypeComplete;
        }
    }

    /// マクロを未知に移動
    pub fn mark_unknown(&mut self, name: InternedStr) {
        self.unconfirmed.remove(&name);
        self.unknown.insert(name);
        if let Some(info) = self.macros.get_mut(&name) {
            info.infer_status = InferStatus::TypeUnknown;
        }
    }

    /// 統計情報を取得
    pub fn stats(&self) -> MacroInferStats {
        MacroInferStats {
            total: self.macros.len(),
            confirmed: self.confirmed.len(),
            unconfirmed: self.unconfirmed.len(),
            unknown: self.unknown.len(),
        }
    }

    /// マクロを解析して MacroInferInfo を作成
    ///
    /// 1. マクロ本体をパース（式 or 文）
    /// 2. def-use 関係を収集（使用するマクロ/関数）
    /// 3. 初期型制約を収集
    pub fn analyze_macro<'a>(
        &mut self,
        def: &MacroDef,
        macro_table: &MacroTable,
        thx_collector: &ThxCollector,
        interner: &'a StringInterner,
        files: &FileRegistry,
        apidoc: Option<&'a ApidocDict>,
        fields_dict: Option<&'a FieldsDict>,
        rust_decl_dict: Option<&'a RustDeclDict>,
    ) {
        let mut info = MacroInferInfo::new(def.name);
        info.is_target = def.is_target;
        info.is_thx_dependent = thx_collector.is_thx_dependent(def.name);

        // 関数形式マクロの場合、パラメータを取得
        let params: Vec<InternedStr> = match &def.kind {
            MacroKind::Function { params, .. } => params.clone(),
            MacroKind::Object => vec![],
        };

        // マクロ本体を展開（TokenExpander を使用）
        let expander = TokenExpander::new(macro_table, interner, files);
        let expanded_tokens = expander.expand(&def.body);

        // def-use 関係を収集（展開後のトークンから識別子を抽出）
        self.collect_uses(&expanded_tokens, macro_table, &mut info);

        // パースを試行
        info.parse_result = self.try_parse_tokens(&expanded_tokens, interner, files);

        // パース成功した場合、型制約を収集
        if let ParseResult::Expression(ref expr) = info.parse_result {
            let mut analyzer = SemanticAnalyzer::with_rust_decl_dict(
                interner,
                apidoc,
                fields_dict,
                rust_decl_dict,
            );
            analyzer.set_macro_params(&params);
            analyzer.collect_expr_constraints(expr, &mut info.type_env);
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

    /// トークン列を式としてパース試行
    fn try_parse_tokens(
        &self,
        tokens: &[crate::token::Token],
        interner: &StringInterner,
        files: &FileRegistry,
    ) -> ParseResult {
        if tokens.is_empty() {
            return ParseResult::Unparseable;
        }

        // 空の typedef セット（マクロ解析時は typedef 情報なし）
        let typedefs = HashSet::new();

        // 式としてパースを試行
        match parse_expression_from_tokens_ref(tokens.to_vec(), interner, files, &typedefs) {
            Ok(expr) => ParseResult::Expression(Box::new(expr)),
            Err(_) => ParseResult::Unparseable,
        }
    }

    /// 全ターゲットマクロを解析
    ///
    /// MacroTable 内の全ターゲットマクロに対して analyze_macro を実行し、
    /// def-use 関係を構築して初期分類を行う。
    pub fn analyze_all_macros<'a>(
        &mut self,
        macro_table: &MacroTable,
        thx_collector: &ThxCollector,
        interner: &'a StringInterner,
        files: &FileRegistry,
        apidoc: Option<&'a ApidocDict>,
        fields_dict: Option<&'a FieldsDict>,
        rust_decl_dict: Option<&'a RustDeclDict>,
    ) {
        // ターゲットマクロのみを解析
        for def in macro_table.iter_target_macros() {
            self.analyze_macro(
                def,
                macro_table,
                thx_collector,
                interner,
                files,
                apidoc,
                fields_dict,
                rust_decl_dict,
            );
        }

        // def-use 関係を構築
        self.build_use_relations();

        // 初期分類
        self.classify_initial();
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
    pub unknown: usize,
}

impl std::fmt::Display for MacroInferStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "MacroInferStats {{ total: {}, confirmed: {}, unconfirmed: {}, unknown: {} }}",
            self.total, self.confirmed, self.unconfirmed, self.unknown
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
        assert!(info.uses.is_empty());
        assert!(info.used_by.is_empty());
        assert!(!info.is_parseable());
        assert_eq!(info.infer_status, InferStatus::Pending);
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
        baz_info.infer_status = InferStatus::TypeComplete;
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
