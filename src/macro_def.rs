//! マクロ定義と管理
//!
//! Cプリプロセッサのマクロ定義を表現し、マクロテーブルで管理する。

use std::collections::HashMap;

use crate::intern::InternedStr;
use crate::source::SourceLocation;
use crate::token::{Comment, Token, TokenKind};

/// マクロ定義の種類
#[derive(Debug, Clone, PartialEq)]
pub enum MacroKind {
    /// オブジェクトマクロ: #define FOO value
    Object,
    /// 関数マクロ: #define FOO(a, b) ...
    Function {
        params: Vec<InternedStr>,
        is_variadic: bool,
    },
}

/// マクロ定義
#[derive(Debug, Clone)]
pub struct MacroDef {
    /// マクロ名
    pub name: InternedStr,
    /// マクロの種類
    pub kind: MacroKind,
    /// 置換トークン列
    pub body: Vec<Token>,
    /// 定義された位置（ファイル追跡用）
    pub def_loc: SourceLocation,
    /// 定義前のコメント（将来のドキュメント生成用）
    pub leading_comments: Vec<Comment>,
    /// ビルトインマクロかどうか
    pub is_builtin: bool,
    /// ターゲットディレクトリで定義されたマクロかどうか
    pub is_target: bool,
    /// マクロ本体にトークン連結 (##) を含むか
    pub has_token_pasting: bool,
}

impl MacroDef {
    /// 新しいオブジェクトマクロを作成
    pub fn object(
        name: InternedStr,
        body: Vec<Token>,
        def_loc: SourceLocation,
    ) -> Self {
        let has_token_pasting = body.iter()
            .any(|t| matches!(t.kind, TokenKind::HashHash));
        Self {
            name,
            kind: MacroKind::Object,
            body,
            def_loc,
            leading_comments: Vec::new(),
            is_builtin: false,
            is_target: false,
            has_token_pasting,
        }
    }

    /// 新しい関数マクロを作成
    pub fn function(
        name: InternedStr,
        params: Vec<InternedStr>,
        is_variadic: bool,
        body: Vec<Token>,
        def_loc: SourceLocation,
    ) -> Self {
        let has_token_pasting = body.iter()
            .any(|t| matches!(t.kind, TokenKind::HashHash));
        Self {
            name,
            kind: MacroKind::Function { params, is_variadic },
            body,
            def_loc,
            leading_comments: Vec::new(),
            is_builtin: false,
            is_target: false,
            has_token_pasting,
        }
    }

    /// ターゲットディレクトリのマクロとして設定
    pub fn with_target(mut self, is_target: bool) -> Self {
        self.is_target = is_target;
        self
    }

    /// コメント付きで作成
    pub fn with_comments(mut self, comments: Vec<Comment>) -> Self {
        self.leading_comments = comments;
        self
    }

    /// ビルトインとしてマーク
    pub fn as_builtin(mut self) -> Self {
        self.is_builtin = true;
        self
    }

    /// 関数マクロかどうか
    pub fn is_function(&self) -> bool {
        matches!(self.kind, MacroKind::Function { .. })
    }

    /// パラメータ数を取得（オブジェクトマクロなら0）
    pub fn param_count(&self) -> usize {
        match &self.kind {
            MacroKind::Object => 0,
            MacroKind::Function { params, .. } => params.len(),
        }
    }

    /// 可変引数マクロかどうか
    pub fn is_variadic(&self) -> bool {
        matches!(self.kind, MacroKind::Function { is_variadic: true, .. })
    }
}

/// マクロテーブル
#[derive(Debug, Default)]
pub struct MacroTable {
    macros: HashMap<InternedStr, MacroDef>,
}

impl MacroTable {
    /// 新しいマクロテーブルを作成
    pub fn new() -> Self {
        Self {
            macros: HashMap::new(),
        }
    }

    /// マクロを定義（既存の定義があれば返す）
    /// `__` で始まる builtin マクロは上書きされない
    pub fn define(&mut self, def: MacroDef, interner: &crate::intern::StringInterner) -> Option<MacroDef> {
        // 既存のマクロがbuiltinで、名前が __ で始まる場合は上書きしない
        if let Some(existing) = self.macros.get(&def.name) {
            if existing.is_builtin {
                let name_str = interner.get(def.name);
                if name_str.starts_with("__") {
                    return None;
                }
            }
        }
        self.macros.insert(def.name, def)
    }

    /// マクロを削除（削除された定義があれば返す）
    pub fn undefine(&mut self, name: InternedStr) -> Option<MacroDef> {
        self.macros.remove(&name)
    }

    /// マクロ定義を取得
    pub fn get(&self, name: InternedStr) -> Option<&MacroDef> {
        self.macros.get(&name)
    }

    /// マクロが定義されているかどうか
    pub fn is_defined(&self, name: InternedStr) -> bool {
        self.macros.contains_key(&name)
    }

    /// 全マクロをイテレート
    pub fn iter(&self) -> impl Iterator<Item = (&InternedStr, &MacroDef)> {
        self.macros.iter()
    }

    /// ターゲットマクロのみをイテレート
    pub fn iter_target_macros(&self) -> impl Iterator<Item = &MacroDef> {
        self.macros.values().filter(|def| def.is_target)
    }

    /// マクロ数を返す
    pub fn len(&self) -> usize {
        self.macros.len()
    }

    /// テーブルが空かどうか
    pub fn is_empty(&self) -> bool {
        self.macros.is_empty()
    }

    /// 非ビルトインマクロのみをイテレート
    pub fn user_defined(&self) -> impl Iterator<Item = (&InternedStr, &MacroDef)> {
        self.macros.iter().filter(|(_, def)| !def.is_builtin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intern::StringInterner;
    use crate::source::FileId;

    #[test]
    fn test_object_macro() {
        let mut interner = StringInterner::new();
        let name = interner.intern("FOO");
        let loc = SourceLocation::new(FileId::default(), 1, 1);

        let def = MacroDef::object(name, vec![], loc);
        assert!(!def.is_function());
        assert_eq!(def.param_count(), 0);
        assert!(!def.is_variadic());
    }

    #[test]
    fn test_function_macro() {
        let mut interner = StringInterner::new();
        let name = interner.intern("MAX");
        let a = interner.intern("a");
        let b = interner.intern("b");
        let loc = SourceLocation::new(FileId::default(), 1, 1);

        let def = MacroDef::function(name, vec![a, b], false, vec![], loc);
        assert!(def.is_function());
        assert_eq!(def.param_count(), 2);
        assert!(!def.is_variadic());
    }

    #[test]
    fn test_variadic_macro() {
        let mut interner = StringInterner::new();
        let name = interner.intern("PRINTF");
        let fmt = interner.intern("fmt");
        let loc = SourceLocation::new(FileId::default(), 1, 1);

        let def = MacroDef::function(name, vec![fmt], true, vec![], loc);
        assert!(def.is_function());
        assert!(def.is_variadic());
    }

    #[test]
    fn test_macro_table() {
        let mut interner = StringInterner::new();
        let mut table = MacroTable::new();

        let foo = interner.intern("FOO");
        let bar = interner.intern("BAR");
        let loc = SourceLocation::new(FileId::default(), 1, 1);

        // 定義
        assert!(table.define(MacroDef::object(foo, vec![], loc.clone()), &interner).is_none());
        assert!(table.define(MacroDef::object(bar, vec![], loc.clone()), &interner).is_none());
        assert_eq!(table.len(), 2);

        // 検索
        assert!(table.is_defined(foo));
        assert!(table.get(foo).is_some());

        // 再定義
        let old = table.define(MacroDef::object(foo, vec![], loc), &interner);
        assert!(old.is_some());
        assert_eq!(table.len(), 2);

        // 削除
        assert!(table.undefine(foo).is_some());
        assert!(!table.is_defined(foo));
        assert_eq!(table.len(), 1);
    }
}
