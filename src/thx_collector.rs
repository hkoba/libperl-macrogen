//! THX マクロ収集モジュール
//!
//! Preprocessor のコールバックとして動作し、マクロ定義時に
//! THX 依存（aTHX, tTHX, my_perl を含む）マクロを収集する。

use std::collections::HashSet;

use crate::intern::{InternedStr, StringInterner};
use crate::macro_def::MacroDef;
use crate::preprocessor::MacroDefCallback;
use crate::token::TokenKind;

/// THX マクロを収集するコールバック実装
///
/// マクロ定義時に呼ばれ、そのマクロが THX 依存かどうかを判定して記録する。
///
/// THX 依存マクロの判定条件:
/// 1. マクロの body に `aTHX`, `tTHX`, `my_perl` トークンが出現する
/// 2. マクロの body に既に `thx_macros` に登録済みのトークンが出現する
pub struct ThxCollector {
    /// THX 依存マクロ名の集合
    pub thx_macros: HashSet<InternedStr>,

    // 事前に intern したシンボル（文字列比較を避けるため）
    sym_athx: InternedStr,
    sym_tthx: InternedStr,
    sym_my_perl: InternedStr,
}

impl ThxCollector {
    /// 新しい ThxCollector を作成
    ///
    /// `aTHX`, `tTHX`, `my_perl` のシンボルを事前に intern しておく。
    pub fn new(interner: &mut StringInterner) -> Self {
        Self {
            thx_macros: HashSet::new(),
            sym_athx: interner.intern("aTHX"),
            sym_tthx: interner.intern("tTHX"),
            sym_my_perl: interner.intern("my_perl"),
        }
    }

    /// トークンが THX 関連かどうか判定
    #[inline]
    fn is_thx_token(&self, id: InternedStr) -> bool {
        id == self.sym_athx || id == self.sym_tthx || id == self.sym_my_perl
    }

    /// 指定されたマクロが THX 依存かどうか
    pub fn is_thx_dependent(&self, name: InternedStr) -> bool {
        self.thx_macros.contains(&name)
    }

    /// THX 依存マクロの数を取得
    pub fn len(&self) -> usize {
        self.thx_macros.len()
    }

    /// THX 依存マクロが空かどうか
    pub fn is_empty(&self) -> bool {
        self.thx_macros.is_empty()
    }
}

impl MacroDefCallback for ThxCollector {
    fn on_macro_defined(&mut self, def: &MacroDef) {
        // def.body をスキャンして以下の条件をチェック：
        // 1. トークンが aTHX, tTHX, my_perl のいずれか
        // 2. 既に thx_macros に登録済みのトークン
        for token in &def.body {
            if let TokenKind::Ident(id) = &token.kind {
                if self.is_thx_token(*id) || self.thx_macros.contains(id) {
                    self.thx_macros.insert(def.name);
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::macro_def::MacroKind;
    use crate::source::SourceLocation;
    use crate::token::Token;

    fn make_ident_token(interner: &mut StringInterner, name: &str) -> Token {
        let id = interner.intern(name);
        Token::new(TokenKind::Ident(id), SourceLocation::default())
    }

    #[test]
    fn test_thx_collector_athx() {
        let mut interner = StringInterner::new();
        let mut collector = ThxCollector::new(&mut interner);

        // aTHX を含むマクロ
        let athx_token = make_ident_token(&mut interner, "aTHX");
        let macro_name = interner.intern("MY_MACRO");
        let def = MacroDef {
            name: macro_name,
            kind: MacroKind::Object,
            body: vec![athx_token],
            def_loc: SourceLocation::default(),
            leading_comments: vec![],
            is_builtin: false,
            is_target: true,
        };

        collector.on_macro_defined(&def);
        assert!(collector.is_thx_dependent(macro_name));
    }

    #[test]
    fn test_thx_collector_tthx() {
        let mut interner = StringInterner::new();
        let mut collector = ThxCollector::new(&mut interner);

        // tTHX を含むマクロ
        let tthx_token = make_ident_token(&mut interner, "tTHX");
        let macro_name = interner.intern("MY_MACRO");
        let def = MacroDef {
            name: macro_name,
            kind: MacroKind::Object,
            body: vec![tthx_token],
            def_loc: SourceLocation::default(),
            leading_comments: vec![],
            is_builtin: false,
            is_target: true,
        };

        collector.on_macro_defined(&def);
        assert!(collector.is_thx_dependent(macro_name));
    }

    #[test]
    fn test_thx_collector_my_perl() {
        let mut interner = StringInterner::new();
        let mut collector = ThxCollector::new(&mut interner);

        // my_perl を含むマクロ
        let my_perl_token = make_ident_token(&mut interner, "my_perl");
        let macro_name = interner.intern("MY_MACRO");
        let def = MacroDef {
            name: macro_name,
            kind: MacroKind::Object,
            body: vec![my_perl_token],
            def_loc: SourceLocation::default(),
            leading_comments: vec![],
            is_builtin: false,
            is_target: true,
        };

        collector.on_macro_defined(&def);
        assert!(collector.is_thx_dependent(macro_name));
    }

    #[test]
    fn test_thx_collector_transitive() {
        let mut interner = StringInterner::new();
        let mut collector = ThxCollector::new(&mut interner);

        // 最初に aTHX を含むマクロを定義
        let athx_token = make_ident_token(&mut interner, "aTHX");
        let base_macro = interner.intern("BASE_MACRO");
        let base_def = MacroDef {
            name: base_macro,
            kind: MacroKind::Object,
            body: vec![athx_token],
            def_loc: SourceLocation::default(),
            leading_comments: vec![],
            is_builtin: false,
            is_target: true,
        };
        collector.on_macro_defined(&base_def);

        // 次に BASE_MACRO を使うマクロを定義
        let base_token = make_ident_token(&mut interner, "BASE_MACRO");
        let derived_macro = interner.intern("DERIVED_MACRO");
        let derived_def = MacroDef {
            name: derived_macro,
            kind: MacroKind::Object,
            body: vec![base_token],
            def_loc: SourceLocation::default(),
            leading_comments: vec![],
            is_builtin: false,
            is_target: true,
        };
        collector.on_macro_defined(&derived_def);

        // 両方とも THX 依存として登録されているはず
        assert!(collector.is_thx_dependent(base_macro));
        assert!(collector.is_thx_dependent(derived_macro));
    }

    #[test]
    fn test_thx_collector_non_thx() {
        let mut interner = StringInterner::new();
        let mut collector = ThxCollector::new(&mut interner);

        // THX に関係ないマクロ
        let other_token = make_ident_token(&mut interner, "some_value");
        let macro_name = interner.intern("NORMAL_MACRO");
        let def = MacroDef {
            name: macro_name,
            kind: MacroKind::Object,
            body: vec![other_token],
            def_loc: SourceLocation::default(),
            leading_comments: vec![],
            is_builtin: false,
            is_target: true,
        };

        collector.on_macro_defined(&def);
        assert!(!collector.is_thx_dependent(macro_name));
    }
}
