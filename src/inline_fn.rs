//! inline 関数辞書
//!
//! is_target なヘッダーファイルに含まれる inline 関数を収集し、
//! 型推論と Rust コード生成に活用する。

use std::collections::{HashMap, HashSet};

use crate::ast::FunctionDef;
use crate::intern::{InternedStr, StringInterner};
use crate::macro_infer::{convert_assert_calls_in_compound_stmt, MacroInferContext};

/// inline 関数辞書
///
/// FunctionDef をそのまま保持し、型情報は AST から直接取得する。
/// 各 inline 関数の呼び出し先（called_functions）と利用可能性も追跡する。
#[derive(Debug, Default)]
pub struct InlineFnDict {
    fns: HashMap<InternedStr, FunctionDef>,
    /// 各 inline 関数の呼び出し先
    called_functions: HashMap<InternedStr, HashSet<InternedStr>>,
    /// 利用不可関数の呼び出しを含む inline 関数の集合
    calls_unavailable: HashSet<InternedStr>,
}

impl InlineFnDict {
    /// 新しい辞書を作成
    pub fn new() -> Self {
        Self::default()
    }

    /// inline 関数を登録
    pub fn insert(&mut self, name: InternedStr, func_def: FunctionDef) {
        self.fns.insert(name, func_def);
    }

    /// inline 関数を取得
    pub fn get(&self, name: InternedStr) -> Option<&FunctionDef> {
        self.fns.get(&name)
    }

    /// 全ての inline 関数を走査
    pub fn iter(&self) -> impl Iterator<Item = (&InternedStr, &FunctionDef)> {
        self.fns.iter()
    }

    /// inline 関数の数
    pub fn len(&self) -> usize {
        self.fns.len()
    }

    /// 辞書が空かどうか
    pub fn is_empty(&self) -> bool {
        self.fns.is_empty()
    }

    /// inline 関数の呼び出し先を取得
    pub fn get_called_functions(&self, name: InternedStr) -> Option<&HashSet<InternedStr>> {
        self.called_functions.get(&name)
    }

    /// 利用不可関数を呼び出すかどうか
    pub fn is_calls_unavailable(&self, name: InternedStr) -> bool {
        self.calls_unavailable.contains(&name)
    }

    /// 利用不可フラグを設定
    pub fn set_calls_unavailable(&mut self, name: InternedStr) {
        self.calls_unavailable.insert(name);
    }

    /// called_functions の全エントリを走査
    pub fn called_functions_iter(&self) -> impl Iterator<Item = (&InternedStr, &HashSet<InternedStr>)> {
        self.called_functions.iter()
    }

    /// FunctionDef から inline 関数を収集
    ///
    /// assert/assert_ 呼び出しを Assert 式に変換してから保存する。
    /// 関数呼び出し先（called_functions）も同時に収集する。
    pub fn collect_from_function_def(&mut self, func_def: &FunctionDef, interner: &StringInterner) {
        if !func_def.specs.is_inline {
            return;
        }

        let name = match func_def.declarator.name {
            Some(n) => n,
            None => return,
        };

        // クローンして assert 呼び出しを変換
        let mut func_def = func_def.clone();
        convert_assert_calls_in_compound_stmt(&mut func_def.body, interner);

        // 関数呼び出し先を収集
        let mut calls = HashSet::new();
        MacroInferContext::collect_function_calls_from_block_items(
            &func_def.body.items,
            &mut calls,
        );
        self.called_functions.insert(name, calls);

        self.insert(name, func_def);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inline_fn_dict_new() {
        let dict = InlineFnDict::new();
        assert!(dict.is_empty());
        assert_eq!(dict.len(), 0);
    }
}
