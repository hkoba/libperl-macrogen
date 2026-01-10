//! マクロ型推論エンジン
//!
//! マクロ定義から型情報を推論するためのモジュール。
//! ExprId を活用し、複数ソースからの型制約を収集・管理する。

use std::collections::{HashMap, HashSet};

use crate::ast::{BlockItem, Expr};
use crate::intern::InternedStr;
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
