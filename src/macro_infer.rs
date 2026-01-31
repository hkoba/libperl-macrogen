//! マクロ型推論エンジン
//!
//! マクロ定義から型情報を推論するためのモジュール。
//! ExprId を活用し、複数ソースからの型制約を収集・管理する。

use std::collections::{HashMap, HashSet};

use crate::apidoc::ApidocDict;
use crate::ast::{AssertKind, BlockItem, Expr, ExprKind};
use crate::c_fn_decl::CFnDeclDict;
use crate::fields_dict::FieldsDict;
use crate::inline_fn::InlineFnDict;
use crate::intern::{InternedStr, StringInterner};
use crate::macro_def::{MacroDef, MacroKind, MacroTable};
use crate::parser::{
    parse_expression_from_tokens_ref_with_stats,
    parse_statement_from_tokens_ref_with_stats,
    ParseStats,
};
use crate::rust_decl::RustDeclDict;
use crate::semantic::SemanticAnalyzer;
use crate::source::FileRegistry;
use crate::token::TokenKind;
use crate::token_expander::TokenExpander;
use crate::type_env::{TypeConstraint, TypeEnv};
use crate::type_repr::TypeRepr;

// use std::io;
// use crate::SexpPrinter;

/// 展開を抑制するマクロシンボル
///
/// これらのマクロは展開せずに AST に関数呼び出しとして残す。
/// パターン検出（SvANY）や特殊処理（assert）に使用。
#[derive(Debug, Clone, Copy)]
pub struct NoExpandSymbols {
    /// assert マクロ
    pub assert: InternedStr,
    /// assert_ マクロ（Perl 独自）
    pub assert_: InternedStr,
}

impl NoExpandSymbols {
    /// 新しい NoExpandSymbols を作成
    pub fn new(interner: &mut StringInterner) -> Self {
        Self {
            assert: interner.intern("assert"),
            assert_: interner.intern("assert_"),
        }
    }

    /// 全シンボルをイテレート
    pub fn iter(&self) -> impl Iterator<Item = InternedStr> {
        [self.assert, self.assert_].into_iter()
    }
}

/// 明示的に展開するマクロのシンボル
///
/// `preserve_function_macros` モードで展開対象となるマクロ。
/// これらは単純なフィールドアクセスや `__builtin_expect` ラッパーなので、
/// インライン展開した方が効率的。
#[derive(Debug, Clone, Copy)]
pub struct ExplicitExpandSymbols {
    /// SvANY マクロ（sv->sv_any に展開）
    pub sv_any: InternedStr,
    /// SvFLAGS マクロ（sv->sv_flags に展開）
    pub sv_flags: InternedStr,
    /// EXPECT マクロ（__builtin_expect のラッパー）
    pub expect: InternedStr,
    /// LIKELY マクロ（__builtin_expect(cond, 1) のラッパー）
    pub likely: InternedStr,
    /// UNLIKELY マクロ（__builtin_expect(cond, 0) のラッパー）
    pub unlikely: InternedStr,
    /// cBOOL マクロ（条件を bool に変換）
    pub cbool: InternedStr,
    /// __ASSERT_ マクロ（DEBUGGING 時のアサーション）
    pub assert_underscore_: InternedStr,
    /// STR_WITH_LEN マクロ（文字列リテラルと長さのペア）
    pub str_with_len: InternedStr,
}

impl ExplicitExpandSymbols {
    /// 新しい ExplicitExpandSymbols を作成
    pub fn new(interner: &mut StringInterner) -> Self {
        Self {
            sv_any: interner.intern("SvANY"),
            sv_flags: interner.intern("SvFLAGS"),
            expect: interner.intern("EXPECT"),
            likely: interner.intern("LIKELY"),
            unlikely: interner.intern("UNLIKELY"),
            cbool: interner.intern("cBOOL"),
            assert_underscore_: interner.intern("__ASSERT_"),
            str_with_len: interner.intern("STR_WITH_LEN"),
        }
    }

    /// 全シンボルをイテレート
    pub fn iter(&self) -> impl Iterator<Item = InternedStr> {
        [
            self.sv_any,
            self.sv_flags,
            self.expect,
            self.likely,
            self.unlikely,
            self.cbool,
            self.assert_underscore_,
            self.str_with_len,
        ].into_iter()
    }
}

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

// ============================================================================
// MacroAst: マクロの AST 表現（パラメータ情報付き）
// ============================================================================

/// マクロパラメータの AST 表現
///
/// 各パラメータは `Expr` として表現され、固有の `ExprId` を持つ。
/// これにより、パラメータの型制約も `expr_constraints` に統一的に格納できる。
#[derive(Debug, Clone)]
pub struct MacroParam {
    /// パラメータ名
    pub name: InternedStr,
    /// パラメータを表す Expr（ExprKind::Ident を持つ）
    pub expr: Expr,
}

impl MacroParam {
    /// 新しい MacroParam を作成
    pub fn new(name: InternedStr, loc: crate::source::SourceLocation) -> Self {
        Self {
            name,
            expr: Expr::new(ExprKind::Ident(name), loc),
        }
    }

    /// パラメータの ExprId を取得
    pub fn expr_id(&self) -> crate::ast::ExprId {
        self.expr.id
    }
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

/// apidoc からジェネリック型パラメータを収集
///
/// `type` や `cast` キーワードを持つパラメータをジェネリック型として扱う。
fn collect_generic_params(entry: &crate::apidoc::ApidocEntry, info: &mut MacroInferInfo) {
    use crate::apidoc::ApidocEntry;

    const PARAM_NAMES: [char; 7] = ['T', 'U', 'V', 'W', 'X', 'Y', 'Z'];
    let mut param_idx = 0;

    // パラメータの type/cast を収集
    for (i, arg) in entry.args.iter().enumerate() {
        if ApidocEntry::is_type_param_keyword(&arg.ty) {
            if param_idx < PARAM_NAMES.len() {
                let name = PARAM_NAMES[param_idx].to_string();
                info.generic_type_params.insert(i as i32, name);
                param_idx += 1;
            }
        }
    }

    // 戻り値型の type/cast を収集
    if entry.returns_type_param() {
        // 最初のパラメータの type と同じ場合は同じ名前を使う
        // （NUM2PTR のように戻り値型とパラメータの type が同じ場合）
        let name = if let Some(first_name) = info.generic_type_params.get(&0) {
            first_name.clone()
        } else if param_idx < PARAM_NAMES.len() {
            PARAM_NAMES[param_idx].to_string()
        } else {
            "T".to_string()
        };
        info.generic_type_params.insert(-1, name); // -1 = return type
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

    /// パラメータリスト（各パラメータは ExprId を持つ）
    pub params: Vec<MacroParam>,

    /// パース結果
    pub parse_result: ParseResult,

    /// 型環境（収集された型制約）
    pub type_env: TypeEnv,

    /// 引数の型推論状態
    pub args_infer_status: InferStatus,

    /// 戻り値の型推論状態
    pub return_infer_status: InferStatus,

    /// ジェネリック型パラメータ情報
    ///
    /// apidoc で `type` や `cast` として宣言されたパラメータは、
    /// Rust のジェネリック型パラメータとして扱う。
    /// key: パラメータインデックス（-1 は戻り値型）
    /// value: 型パラメータ名 ("T", "U", etc.)
    pub generic_type_params: HashMap<i32, String>,

    /// 関数呼び出しの数（パース時に検出）
    pub function_call_count: usize,
    /// ポインタデリファレンスの数（パース時に検出）
    pub deref_count: usize,

    /// 呼び出される関数名の集合（マクロ以外の関数呼び出し）
    pub called_functions: HashSet<InternedStr>,
    /// 利用不可関数の呼び出しを含む（直接または推移的）
    pub calls_unavailable: bool,
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
            params: Vec::new(),
            parse_result: ParseResult::Unparseable(None),
            type_env: TypeEnv::new(),
            args_infer_status: InferStatus::Pending,
            return_infer_status: InferStatus::Pending,
            generic_type_params: HashMap::new(),
            function_call_count: 0,
            deref_count: 0,
            called_functions: HashSet::new(),
            calls_unavailable: false,
        }
    }

    /// unsafe 操作を含むか
    pub fn has_unsafe_ops(&self) -> bool {
        self.function_call_count > 0 || self.deref_count > 0
    }

    /// パラメータ名から対応する ExprId を検索
    pub fn find_param_expr_id(&self, name: InternedStr) -> Option<crate::ast::ExprId> {
        self.params.iter()
            .find(|p| p.name == name)
            .map(|p| p.expr_id())
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
    pub fn get_return_type(&self) -> Option<&crate::type_repr::TypeRepr> {
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
        no_expand: NoExpandSymbols,
        explicit_expand: ExplicitExpandSymbols,
    ) -> (MacroInferInfo, bool, bool) {
        let mut info = MacroInferInfo::new(def.name);
        info.is_target = def.is_target;
        info.has_body = !def.body.is_empty();
        info.is_function = matches!(def.kind, MacroKind::Function { .. });

        // パラメータの Expr を生成（各パラメータに ExprId を割り当て）
        if let MacroKind::Function { params, .. } = &def.kind {
            for &param_name in params {
                info.params.push(MacroParam::new(param_name, crate::source::SourceLocation::default()));
            }
        }

        // 直接 ## を含むかチェック
        let has_pasting_direct = def.body.iter().any(|t| matches!(t.kind, TokenKind::HashHash));

        // マクロ本体を展開（TokenExpander を使用）
        let mut expander = TokenExpander::new(macro_table, interner, files);
        if let Some(dict) = rust_decl_dict {
            expander.set_bindings_consts(&dict.consts);
        }
        // 特定マクロを展開しないよう登録（assert など特殊処理用）
        for sym in no_expand.iter() {
            expander.add_no_expand(sym);
        }
        // 明示的に展開するマクロを登録（SvANY, SvFLAGS など）
        expander.extend_explicit_expand(explicit_expand.iter());
        let expanded_tokens = expander.expand_with_calls(&def.body);

        // def-use 関係を収集（呼び出されたマクロの集合から、no_expand マクロを含む）
        self.collect_uses_from_called(expander.called_macros(), &mut info);

        // THX 判定: 展開されたマクロに aTHX, tTHX が含まれるか、
        // または展開後トークンに my_perl が含まれるかをチェック
        let (sym_athx, sym_tthx, sym_my_perl) = thx_symbols;
        let has_thx_from_uses = info.uses.contains(&sym_athx) || info.uses.contains(&sym_tthx);
        let has_my_perl = expanded_tokens.iter().any(|t| {
            matches!(t.kind, TokenKind::Ident(id) if id == sym_my_perl)
        });
        let has_thx = has_thx_from_uses || has_my_perl;

        // 初期値を設定（後で propagate で上書きされる可能性あり）
        info.has_token_pasting = has_pasting_direct;
        info.is_thx_dependent = has_thx;

        // パースを試行
        let (parse_result, stats) = self.try_parse_tokens(&expanded_tokens, interner, files, typedefs);
        info.parse_result = parse_result;
        info.function_call_count = stats.function_call_count;
        info.deref_count = stats.deref_count;

        // パース成功した場合、assert 呼び出しを Assert 式に変換
        match &mut info.parse_result {
            ParseResult::Expression(expr) => {
                convert_assert_calls(expr, interner);
            }
            ParseResult::Statement(items) => {
                for item in items {
                    if let BlockItem::Stmt(stmt) = item {
                        convert_assert_calls_in_stmt(stmt, interner);
                    }
                }
            }
            ParseResult::Unparseable(_) => {}
        }

        // パース成功した場合、関数呼び出しを収集
        match &info.parse_result {
            ParseResult::Expression(expr) => {
                Self::collect_function_calls_from_expr(expr, &mut info.called_functions);
            }
            ParseResult::Statement(block_items) => {
                Self::collect_function_calls_from_block_items(block_items, &mut info.called_functions);
            }
            ParseResult::Unparseable(_) => {}
        }

        (info, has_pasting_direct, has_thx)
    }

    /// Phase 2: 型推論の適用
    ///
    /// 既に登録済みの MacroInferInfo に対して型制約を収集する
    /// `return_types_cache` は確定済みマクロの戻り値型キャッシュ
    pub fn infer_macro_types<'a>(
        &mut self,
        name: InternedStr,
        params: &[InternedStr],
        interner: &'a StringInterner,
        files: &FileRegistry,
        apidoc: Option<&'a ApidocDict>,
        fields_dict: Option<&'a FieldsDict>,
        rust_decl_dict: Option<&'a RustDeclDict>,
        inline_fn_dict: Option<&'a InlineFnDict>,
        typedefs: &HashSet<InternedStr>,
        return_types_cache: &HashMap<String, String>,
    ) {
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
                inline_fn_dict,
            );

            // println!("expr macro {}: {:?}", interner.get(name), expr);
            // {
            //     print!("expr macro {}: ", interner.get(name));
            //     let stdout = io::stdout();
            //     let mut handler = stdout.lock();
            //     let mut printer = SexpPrinter::new(&mut handler, interner);
            //     let _ = printer.print_expr(expr);
            // }
            // println!("");

            // 確定済みマクロの戻り値型を設定（キャッシュへの参照を渡す）
            analyzer.set_macro_return_types(return_types_cache);

            // apidoc 型情報付きでパラメータをシンボルテーブルに登録
            analyzer.register_macro_params_from_apidoc(name, params, files, typedefs);

            // 全式の型制約を収集
            analyzer.collect_expr_constraints(expr, &mut info.type_env);

            // マクロ自体の戻り値型を制約として追加
            if let Some(apidoc_dict) = apidoc {
                let macro_name_str = interner.get(name);
                if let Some(entry) = apidoc_dict.get(macro_name_str) {
                    if let Some(ref return_type) = entry.return_type {
                        let type_repr = TypeRepr::from_apidoc_string(return_type, interner);
                        info.type_env.add_return_constraint(TypeConstraint::new(
                            expr.id,
                            type_repr,
                            format!("return type of macro {}", macro_name_str),
                        ));
                    }

                    // ジェネリック型パラメータを収集
                    collect_generic_params(entry, info);
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
                inline_fn_dict,
            );

            // 確定済みマクロの戻り値型を設定（キャッシュへの参照を渡す）
            analyzer.set_macro_return_types(return_types_cache);

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

    /// マクロの戻り値型を取得（キャッシュ更新用）
    pub fn get_macro_return_type(&self, name: InternedStr, interner: &StringInterner) -> Option<(String, String)> {
        self.macros.get(&name).and_then(|info| {
            info.get_return_type().map(|ty| {
                (interner.get(name).to_string(), ty.to_display_string(interner))
            })
        })
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

        // def-use 関係を収集（呼び出されたマクロの集合から、no_expand マクロを含む）
        self.collect_uses_from_called(expander.called_macros(), &mut info);

        // パースを試行
        let (parse_result, stats) = self.try_parse_tokens(&expanded_tokens, interner, files, typedefs);
        info.parse_result = parse_result;
        info.function_call_count = stats.function_call_count;
        info.deref_count = stats.deref_count;

        // パース成功した場合、関数呼び出しを収集
        match &info.parse_result {
            ParseResult::Expression(expr) => {
                Self::collect_function_calls_from_expr(expr, &mut info.called_functions);
            }
            ParseResult::Statement(block_items) => {
                Self::collect_function_calls_from_block_items(block_items, &mut info.called_functions);
            }
            ParseResult::Unparseable(_) => {}
        }

        // パース成功した場合、型制約を収集
        if let ParseResult::Expression(ref expr) = info.parse_result {
            let mut analyzer = SemanticAnalyzer::with_rust_decl_dict(
                interner,
                apidoc,
                fields_dict,
                rust_decl_dict,
                None, // inline_fn_dict (deprecated function doesn't use it)
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
                        let type_repr = TypeRepr::from_apidoc_string(return_type, interner);
                        info.type_env.add_return_constraint(TypeConstraint::new(
                            expr.id,
                            type_repr,
                            format!("return type of macro {}", macro_name_str),
                        ));
                    }

                    // ジェネリック型パラメータを収集
                    collect_generic_params(entry, &mut info);
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
                None, // inline_fn_dict (deprecated function doesn't use it)
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
    /// 呼び出されたマクロを uses に追加
    ///
    /// TokenExpander が呼び出したマクロの集合（no_expand を含む）から、自分自身を除いて uses に追加する。
    fn collect_uses_from_called(
        &self,
        called_macros: &HashSet<InternedStr>,
        info: &mut MacroInferInfo,
    ) {
        for &id in called_macros {
            if id != info.name {
                info.add_use(id);
            }
        }
    }

    /// トークン列を式または文としてパース試行
    ///
    /// # Returns
    /// (パース結果, 関数呼び出しを含むか)
    fn try_parse_tokens(
        &self,
        tokens: &[crate::token::Token],
        interner: &StringInterner,
        files: &FileRegistry,
        typedefs: &HashSet<InternedStr>,
    ) -> (ParseResult, ParseStats) {
        if tokens.is_empty() {
            return (ParseResult::Unparseable(Some("empty token sequence".to_string())), ParseStats::default());
        }

        // 空白・改行をスキップして最初の有効なトークンを探す
        let first_significant = tokens.iter().find(|t| {
            !matches!(t.kind, TokenKind::Space | TokenKind::Newline)
        });

        // 先頭トークンが KwDo または KwIf なら文としてパース試行
        let is_statement_start = first_significant
            .is_some_and(|t| matches!(t.kind, TokenKind::KwDo | TokenKind::KwIf));
        if is_statement_start {
            match parse_statement_from_tokens_ref_with_stats(tokens.to_vec(), interner, files, typedefs) {
                Ok((stmt, stats)) => {
                    return (
                        ParseResult::Statement(vec![BlockItem::Stmt(stmt)]),
                        stats,
                    );
                }
                Err(_) => {} // フォールスルーして式としてパース
            }
        }

        // 式としてパースを試行
        match parse_expression_from_tokens_ref_with_stats(tokens.to_vec(), interner, files, typedefs) {
            Ok((expr, stats)) => (
                ParseResult::Expression(Box::new(expr)),
                stats,
            ),
            Err(err) => (ParseResult::Unparseable(Some(err.format_with_files(files))), ParseStats::default()),
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
        inline_fn_dict: Option<&'a InlineFnDict>,
        c_fn_decl_dict: Option<&'a CFnDeclDict>,
        typedefs: &HashSet<InternedStr>,
        thx_symbols: (InternedStr, InternedStr, InternedStr),
        no_expand: NoExpandSymbols,
        explicit_expand: ExplicitExpandSymbols,
    ) {
        // Step 1: 全マクロの初期構築（パースのみ、型推論なし）
        let mut thx_initial = HashSet::new();
        let mut pasting_initial = HashSet::new();

        for def in macro_table.iter_target_macros() {
            let (info, has_pasting, has_thx) = self.build_macro_info(
                def, macro_table, interner, files, rust_decl_dict, typedefs, thx_symbols, no_expand, explicit_expand
            );
            if has_pasting {
                pasting_initial.insert(def.name);
            }
            if has_thx {
                thx_initial.insert(def.name);
            }
            self.register(info);
        }

        // Step 1.5: called_functions を CFnDeclDict と照合して THX 依存を追加検出
        if let Some(c_fn_dict) = c_fn_decl_dict {
            for (name, info) in &self.macros {
                // 呼び出す関数が THX 依存かチェック
                let has_thx_from_fn_calls = info.called_functions.iter().any(|fn_name| {
                    c_fn_dict.is_thx_dependent(*fn_name)
                });
                if has_thx_from_fn_calls && !thx_initial.contains(name) {
                    thx_initial.insert(*name);
                }
            }
        }

        // Step 2: used_by を構築
        self.build_use_relations();

        // Step 3: THX の推移閉包を計算（used_by 経由）
        self.propagate_flag_via_used_by(&thx_initial, true);

        // Step 4: ## の推移閉包を計算（used_by 経由）
        self.propagate_flag_via_used_by(&pasting_initial, false);

        // Step 4.5: 利用不可関数呼び出しのチェックと伝播
        self.check_function_availability(rust_decl_dict, inline_fn_dict, interner);
        self.propagate_unavailable_via_used_by();

        // Step 5: 全マクロを unconfirmed に
        for name in self.macros.keys().copied().collect::<Vec<_>>() {
            self.unconfirmed.insert(name);
        }

        // Step 6: 依存順に型推論
        self.infer_types_in_dependency_order(
            macro_table, interner, files, apidoc, fields_dict, rust_decl_dict, inline_fn_dict, typedefs
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

    /// 関数呼び出しの利用可能性をチェック
    ///
    /// 各マクロの `called_functions` を調べ、bindings.rs にもマクロにも
    /// 存在しない関数を呼び出している場合、`calls_unavailable = true` を設定
    fn check_function_availability(
        &mut self,
        rust_decl_dict: Option<&RustDeclDict>,
        inline_fn_dict: Option<&InlineFnDict>,
        interner: &StringInterner,
    ) {
        // bindings.rs の関数名を収集
        let bindings_fns: std::collections::HashSet<&str> = rust_decl_dict
            .map(|d| d.fns.keys().map(|s| s.as_str()).collect())
            .unwrap_or_default();

        // ビルトイン関数
        let builtin_fns: std::collections::HashSet<&str> = [
            "__builtin_expect",
            "__builtin_offsetof",
            "__builtin_types_compatible_p",
            "__builtin_constant_p",
            "__builtin_choose_expr",
            "__builtin_unreachable",
            "__builtin_trap",
            "__builtin_assume",
            "__builtin_bswap16",
            "__builtin_bswap32",
            "__builtin_bswap64",
            "__builtin_popcount",
            "__builtin_clz",
            "__builtin_ctz",
            "__errno_location",  // glibc
            "pthread_mutex_lock",
            "pthread_mutex_unlock",
            "pthread_rwlock_rdlock",
            "pthread_rwlock_wrlock",
            "pthread_rwlock_unlock",
            "memchr",
            "memcpy",
            "memmove",
            "memset",
            "strlen",
            "strcmp",
            "strncmp",
            "strcpy",
            "strncpy",
        ].into_iter().collect();

        // マクロ名の集合
        let macro_names: HashSet<InternedStr> = self.macros.keys().copied().collect();

        // 各マクロの関数呼び出しをチェック
        let macro_names_list: Vec<InternedStr> = self.macros.keys().copied().collect();
        for name in macro_names_list {
            let called_functions: Vec<InternedStr> = self.macros
                .get(&name)
                .map(|info| info.called_functions.iter().copied().collect())
                .unwrap_or_default();

            let mut has_unavailable = false;
            for called_fn in called_functions {
                let fn_name = interner.get(called_fn);

                // マクロとして存在する場合はOK
                if macro_names.contains(&called_fn) {
                    continue;
                }

                // bindings.rs に存在する場合はOK
                if bindings_fns.contains(fn_name) {
                    continue;
                }

                // インライン関数として存在する場合はOK
                if let Some(inline_fns) = inline_fn_dict {
                    if inline_fns.get(called_fn).is_some() {
                        continue;
                    }
                }

                // ビルトイン関数の場合はOK
                if builtin_fns.contains(fn_name) {
                    continue;
                }

                // それ以外は利用不可
                has_unavailable = true;
                break;
            }

            if has_unavailable {
                if let Some(info) = self.macros.get_mut(&name) {
                    info.calls_unavailable = true;
                }
            }
        }
    }

    /// calls_unavailable を used_by 経由で伝播
    fn propagate_unavailable_via_used_by(&mut self) {
        // 初期集合: 直接利用不可関数を呼び出すマクロ
        let initial_set: HashSet<InternedStr> = self.macros
            .iter()
            .filter(|(_, info)| info.calls_unavailable)
            .map(|(name, _)| *name)
            .collect();

        // used_by を辿って伝播
        let mut to_propagate: Vec<InternedStr> = initial_set.into_iter().collect();

        while let Some(name) = to_propagate.pop() {
            let used_by_list: Vec<InternedStr> = self.macros
                .get(&name)
                .map(|info| info.used_by.iter().copied().collect())
                .unwrap_or_default();

            for user in used_by_list {
                if let Some(user_info) = self.macros.get_mut(&user) {
                    if !user_info.calls_unavailable {
                        user_info.calls_unavailable = true;
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
        inline_fn_dict: Option<&'a InlineFnDict>,
        typedefs: &HashSet<InternedStr>,
    ) {
        // 確定済みマクロの戻り値型キャッシュ（O(N²) を避けるため）
        let mut return_types_cache: HashMap<String, String> = HashMap::new();

        loop {
            let candidates = self.get_inference_candidates();
            if candidates.is_empty() {
                // 残りの未確定マクロにも型推論を実行（apidoc 情報を適用するため）
                let remaining: Vec<_> = self.unconfirmed.iter().copied().collect();
                for name in remaining {
                    // パラメータを取得
                    let params: Vec<InternedStr> = macro_table
                        .get(name)
                        .map(|def| match &def.kind {
                            MacroKind::Function { params, .. } => params.clone(),
                            MacroKind::Object => vec![],
                        })
                        .unwrap_or_default();

                    // 型推論を実行（apidoc 型情報を適用）
                    self.infer_macro_types(
                        name, &params, interner, files, apidoc, fields_dict, rust_decl_dict, inline_fn_dict, typedefs,
                        &return_types_cache,
                    );

                    // apidoc から型が確定した場合は confirmed に
                    let is_confirmed = self.macros.get(&name)
                        .map(|info| info.get_return_type().is_some())
                        .unwrap_or(false);

                    if is_confirmed {
                        if let Some((macro_name, return_type)) = self.get_macro_return_type(name, interner) {
                            return_types_cache.insert(macro_name, return_type);
                        }
                        self.mark_confirmed(name);
                    } else {
                        self.move_to_unknown(name);
                    }
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

                // 型推論を実行（キャッシュを渡す）
                self.infer_macro_types(
                    name, &params, interner, files, apidoc, fields_dict, rust_decl_dict, inline_fn_dict, typedefs,
                    &return_types_cache,
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
                    // キャッシュに戻り値型を追加
                    if let Some((macro_name, return_type)) = self.get_macro_return_type(name, interner) {
                        return_types_cache.insert(macro_name, return_type);
                    }
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

    /// 式から関数呼び出しのみを再帰的に収集（識別子は含めない）
    pub fn collect_function_calls_from_expr(
        expr: &Expr,
        calls: &mut HashSet<InternedStr>,
    ) {
        match &expr.kind {
            ExprKind::Call { func, args } => {
                // 関数名を収集（直接呼び出しの場合のみ）
                if let ExprKind::Ident(name) = &func.kind {
                    calls.insert(*name);
                }
                Self::collect_function_calls_from_expr(func, calls);
                for arg in args {
                    Self::collect_function_calls_from_expr(arg, calls);
                }
            }
            ExprKind::Binary { lhs, rhs, .. } => {
                Self::collect_function_calls_from_expr(lhs, calls);
                Self::collect_function_calls_from_expr(rhs, calls);
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
                Self::collect_function_calls_from_expr(inner, calls);
            }
            ExprKind::Index { expr: base, index } => {
                Self::collect_function_calls_from_expr(base, calls);
                Self::collect_function_calls_from_expr(index, calls);
            }
            ExprKind::Member { expr: base, .. } | ExprKind::PtrMember { expr: base, .. } => {
                Self::collect_function_calls_from_expr(base, calls);
            }
            ExprKind::Conditional { cond, then_expr, else_expr } => {
                Self::collect_function_calls_from_expr(cond, calls);
                Self::collect_function_calls_from_expr(then_expr, calls);
                Self::collect_function_calls_from_expr(else_expr, calls);
            }
            ExprKind::Assign { lhs, rhs, .. } => {
                Self::collect_function_calls_from_expr(lhs, calls);
                Self::collect_function_calls_from_expr(rhs, calls);
            }
            ExprKind::Comma { lhs, rhs } => {
                Self::collect_function_calls_from_expr(lhs, calls);
                Self::collect_function_calls_from_expr(rhs, calls);
            }
            ExprKind::StmtExpr(compound) => {
                Self::collect_function_calls_from_block_items(&compound.items, calls);
            }
            _ => {}
        }
    }

    /// ブロックアイテムから関数呼び出しを収集
    fn collect_function_calls_from_block_items(
        items: &[BlockItem],
        calls: &mut HashSet<InternedStr>,
    ) {
        for item in items {
            match item {
                BlockItem::Stmt(stmt) => {
                    Self::collect_function_calls_from_stmt(stmt, calls);
                }
                BlockItem::Decl(_) => {
                    // 宣言内の初期化子も処理する必要があるが、今回はスキップ
                }
            }
        }
    }

    /// 文から関数呼び出しを収集
    fn collect_function_calls_from_stmt(
        stmt: &crate::ast::Stmt,
        calls: &mut HashSet<InternedStr>,
    ) {
        use crate::ast::{Stmt, ForInit};
        match stmt {
            Stmt::Expr(Some(expr), _) => {
                Self::collect_function_calls_from_expr(expr, calls);
            }
            Stmt::If { cond, then_stmt, else_stmt, .. } => {
                Self::collect_function_calls_from_expr(cond, calls);
                Self::collect_function_calls_from_stmt(then_stmt, calls);
                if let Some(else_s) = else_stmt {
                    Self::collect_function_calls_from_stmt(else_s, calls);
                }
            }
            Stmt::While { cond, body, .. } => {
                Self::collect_function_calls_from_expr(cond, calls);
                Self::collect_function_calls_from_stmt(body, calls);
            }
            Stmt::DoWhile { body, cond, .. } => {
                Self::collect_function_calls_from_stmt(body, calls);
                Self::collect_function_calls_from_expr(cond, calls);
            }
            Stmt::For { init, cond, step, body, .. } => {
                if let Some(for_init) = init {
                    match for_init {
                        ForInit::Expr(expr) => {
                            Self::collect_function_calls_from_expr(expr, calls);
                        }
                        ForInit::Decl(_) => {
                            // 宣言内の初期化子は今回はスキップ
                        }
                    }
                }
                if let Some(cond_expr) = cond {
                    Self::collect_function_calls_from_expr(cond_expr, calls);
                }
                if let Some(step_expr) = step {
                    Self::collect_function_calls_from_expr(step_expr, calls);
                }
                Self::collect_function_calls_from_stmt(body, calls);
            }
            Stmt::Compound(compound) => {
                Self::collect_function_calls_from_block_items(&compound.items, calls);
            }
            Stmt::Return(Some(expr), _) => {
                Self::collect_function_calls_from_expr(expr, calls);
            }
            Stmt::Switch { expr, body, .. } => {
                Self::collect_function_calls_from_expr(expr, calls);
                Self::collect_function_calls_from_stmt(body, calls);
            }
            Stmt::Label { stmt, .. } | Stmt::Case { stmt, .. } | Stmt::Default { stmt, .. } => {
                Self::collect_function_calls_from_stmt(stmt, calls);
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

/// マクロ名がアサーションマクロかどうかを判定
pub fn detect_assert_kind(name: &str) -> Option<AssertKind> {
    match name {
        "assert" => Some(AssertKind::Assert),
        "assert_" => Some(AssertKind::AssertUnderscore),
        _ => None,
    }
}

/// AST 内の assert/assert_ 呼び出しを Assert 式に変換
///
/// パース後に呼び出し、`Call { func: Ident("assert"), args }` を
/// `Assert { kind, condition }` に変換する。
pub fn convert_assert_calls(expr: &mut Expr, interner: &StringInterner) {
    match &mut expr.kind {
        ExprKind::Call { func, args } => {
            // 子を先に処理
            convert_assert_calls(func, interner);
            for arg in args.iter_mut() {
                convert_assert_calls(arg, interner);
            }

            // assert/assert_ 呼び出しを検出
            if let ExprKind::Ident(name) = &func.kind {
                let name_str = interner.get(*name);
                if let Some(kind) = detect_assert_kind(name_str) {
                    if let Some(condition) = args.pop() {
                        expr.kind = ExprKind::Assert {
                            kind,
                            condition: Box::new(condition),
                        };
                    }
                }
            }
        }
        ExprKind::Binary { lhs, rhs, .. } => {
            convert_assert_calls(lhs, interner);
            convert_assert_calls(rhs, interner);
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
            convert_assert_calls(inner, interner);
        }
        ExprKind::Index { expr: base, index } => {
            convert_assert_calls(base, interner);
            convert_assert_calls(index, interner);
        }
        ExprKind::Member { expr: base, .. } | ExprKind::PtrMember { expr: base, .. } => {
            convert_assert_calls(base, interner);
        }
        ExprKind::Conditional { cond, then_expr, else_expr } => {
            convert_assert_calls(cond, interner);
            convert_assert_calls(then_expr, interner);
            convert_assert_calls(else_expr, interner);
        }
        ExprKind::Assign { lhs, rhs, .. } => {
            convert_assert_calls(lhs, interner);
            convert_assert_calls(rhs, interner);
        }
        ExprKind::Comma { lhs, rhs } => {
            convert_assert_calls(lhs, interner);
            convert_assert_calls(rhs, interner);
        }
        ExprKind::Assert { condition, .. } => {
            convert_assert_calls(condition, interner);
        }
        ExprKind::CompoundLit { init, .. } => {
            for item in init {
                if let crate::ast::Initializer::Expr(e) = &mut item.init {
                    convert_assert_calls(e, interner);
                }
            }
        }
        ExprKind::StmtExpr(compound) => {
            for item in &mut compound.items {
                if let BlockItem::Stmt(stmt) = item {
                    convert_assert_calls_in_stmt(stmt, interner);
                }
            }
        }
        // リテラルや識別子など、再帰不要
        ExprKind::Ident(_)
        | ExprKind::IntLit(_)
        | ExprKind::UIntLit(_)
        | ExprKind::FloatLit(_)
        | ExprKind::CharLit(_)
        | ExprKind::StringLit(_)
        | ExprKind::SizeofType(_)
        | ExprKind::Alignof(_) => {}
    }
}

/// CompoundStmt 内の assert 呼び出しを変換
///
/// inline 関数の本体などに使用。
pub fn convert_assert_calls_in_compound_stmt(compound: &mut crate::ast::CompoundStmt, interner: &StringInterner) {
    use crate::ast::BlockItem;
    for item in &mut compound.items {
        if let BlockItem::Stmt(s) = item {
            convert_assert_calls_in_stmt(s, interner);
        }
    }
}

/// Statement 内の assert 呼び出しを変換
pub fn convert_assert_calls_in_stmt(stmt: &mut crate::ast::Stmt, interner: &StringInterner) {
    use crate::ast::Stmt;
    match stmt {
        Stmt::Expr(Some(expr), _) => convert_assert_calls(expr, interner),
        Stmt::If { cond, then_stmt, else_stmt, .. } => {
            convert_assert_calls(cond, interner);
            convert_assert_calls_in_stmt(then_stmt, interner);
            if let Some(else_s) = else_stmt {
                convert_assert_calls_in_stmt(else_s, interner);
            }
        }
        Stmt::While { cond, body, .. } => {
            convert_assert_calls(cond, interner);
            convert_assert_calls_in_stmt(body, interner);
        }
        Stmt::DoWhile { body, cond, .. } => {
            convert_assert_calls_in_stmt(body, interner);
            convert_assert_calls(cond, interner);
        }
        Stmt::For { init, cond, step, body, .. } => {
            if let Some(crate::ast::ForInit::Expr(e)) = init {
                convert_assert_calls(e, interner);
            }
            if let Some(c) = cond {
                convert_assert_calls(c, interner);
            }
            if let Some(s) = step {
                convert_assert_calls(s, interner);
            }
            convert_assert_calls_in_stmt(body, interner);
        }
        Stmt::Switch { expr, body, .. } => {
            convert_assert_calls(expr, interner);
            convert_assert_calls_in_stmt(body, interner);
        }
        Stmt::Return(Some(expr), _) => convert_assert_calls(expr, interner),
        Stmt::Compound(compound) => {
            for item in &mut compound.items {
                match item {
                    BlockItem::Stmt(s) => convert_assert_calls_in_stmt(s, interner),
                    BlockItem::Decl(_) => {}
                }
            }
        }
        Stmt::Label { stmt: s, .. }
        | Stmt::Case { stmt: s, .. }
        | Stmt::Default { stmt: s, .. } => {
            convert_assert_calls_in_stmt(s, interner);
        }
        _ => {}
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

    #[test]
    fn test_no_expand_symbols_new() {
        let mut interner = StringInterner::new();
        let symbols = NoExpandSymbols::new(&mut interner);

        assert_eq!(interner.get(symbols.assert), "assert");
        assert_eq!(interner.get(symbols.assert_), "assert_");
    }

    #[test]
    fn test_no_expand_symbols_iter() {
        let mut interner = StringInterner::new();
        let symbols = NoExpandSymbols::new(&mut interner);

        let syms: Vec<_> = symbols.iter().collect();
        assert_eq!(syms.len(), 2);
        assert!(syms.contains(&symbols.assert));
        assert!(syms.contains(&symbols.assert_));
    }

    #[test]
    fn test_explicit_expand_symbols_new() {
        let mut interner = StringInterner::new();
        let symbols = ExplicitExpandSymbols::new(&mut interner);

        assert_eq!(interner.get(symbols.sv_any), "SvANY");
        assert_eq!(interner.get(symbols.sv_flags), "SvFLAGS");
        assert_eq!(interner.get(symbols.expect), "EXPECT");
        assert_eq!(interner.get(symbols.likely), "LIKELY");
        assert_eq!(interner.get(symbols.unlikely), "UNLIKELY");
        assert_eq!(interner.get(symbols.cbool), "cBOOL");
        assert_eq!(interner.get(symbols.assert_underscore_), "__ASSERT_");
        assert_eq!(interner.get(symbols.str_with_len), "STR_WITH_LEN");
    }

    #[test]
    fn test_explicit_expand_symbols_iter() {
        let mut interner = StringInterner::new();
        let symbols = ExplicitExpandSymbols::new(&mut interner);

        let syms: Vec<_> = symbols.iter().collect();
        assert_eq!(syms.len(), 8);
        assert!(syms.contains(&symbols.sv_any));
        assert!(syms.contains(&symbols.sv_flags));
        assert!(syms.contains(&symbols.expect));
        assert!(syms.contains(&symbols.likely));
        assert!(syms.contains(&symbols.unlikely));
        assert!(syms.contains(&symbols.cbool));
        assert!(syms.contains(&symbols.assert_underscore_));
        assert!(syms.contains(&symbols.str_with_len));
    }
}
