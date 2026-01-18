//! inline 関数辞書
//!
//! is_target なヘッダーファイルに含まれる inline 関数を収集し、
//! 型推論と Rust コード生成に活用する。

use std::collections::HashMap;

use crate::ast::FunctionDef;
use crate::intern::{InternedStr, StringInterner};
use crate::macro_infer::convert_assert_calls_in_compound_stmt;

/// inline 関数辞書
///
/// FunctionDef をそのまま保持し、型情報は AST から直接取得する。
#[derive(Debug, Default)]
pub struct InlineFnDict {
    fns: HashMap<InternedStr, FunctionDef>,
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

    /// FunctionDef から inline 関数を収集
    ///
    /// assert/assert_ 呼び出しを Assert 式に変換してから保存する。
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
