//! 部分トークン列用マクロ展開器
//!
//! マクロの body（トークン列）を展開するための軽量ユーティリティ。
//! Preprocessor とは異なり、MacroTable は readonly で新しいマクロ定義は追加しない。

use std::collections::{HashMap, HashSet};

use crate::intern::{InternedStr, StringInterner};
use crate::macro_def::{MacroDef, MacroKind, MacroTable};
use crate::source::{FileRegistry, SourceLocation};
use crate::token::{
    MacroBeginInfo, MacroEndInfo, MacroInvocationKind, Token, TokenId, TokenKind,
};

/// キーの存在チェックのみを抽象化する trait
///
/// HashMap<String, V> の値の型を隠蔽し、キー検索のみを公開する。
pub trait KeySet {
    fn contains(&self, key: &str) -> bool;
}

// HashMap<String, V> に対する汎用実装
impl<V> KeySet for HashMap<String, V> {
    fn contains(&self, key: &str) -> bool {
        self.contains_key(key)
    }
}

/// 部分トークン列のマクロ展開器
///
/// MacroTable は readonly で、新しいマクロ定義は追加しない。
pub struct TokenExpander<'a> {
    /// マクロテーブル（参照のみ）
    macro_table: &'a MacroTable,
    /// 文字列インターナー
    interner: &'a StringInterner,
    /// ファイルレジストリ
    files: &'a FileRegistry,
    /// 展開しないマクロ名（定数マクロ等）
    no_expand: HashSet<InternedStr>,
    /// マクロ展開マーカーを出力するか
    emit_markers: bool,
    /// bindings.rs 定数名のキーセット（値の型は隠蔽）
    bindings_consts: Option<&'a dyn KeySet>,
}

impl<'a> TokenExpander<'a> {
    /// 新しい TokenExpander を作成
    pub fn new(
        macro_table: &'a MacroTable,
        interner: &'a StringInterner,
        files: &'a FileRegistry,
    ) -> Self {
        Self {
            macro_table,
            interner,
            files,
            no_expand: HashSet::new(),
            emit_markers: false,
            bindings_consts: None,
        }
    }

    /// bindings.rs の定数名セットを設定
    pub fn set_bindings_consts(&mut self, consts: &'a dyn KeySet) {
        self.bindings_consts = Some(consts);
    }

    /// 展開しないマクロを追加
    pub fn add_no_expand(&mut self, name: InternedStr) {
        self.no_expand.insert(name);
    }

    /// 展開しないマクロを複数追加
    pub fn extend_no_expand(&mut self, names: impl IntoIterator<Item = InternedStr>) {
        self.no_expand.extend(names);
    }

    /// マクロ展開マーカー出力を有効化
    pub fn set_emit_markers(&mut self, emit: bool) {
        self.emit_markers = emit;
    }

    /// トークン列をマクロ展開する（オブジェクトマクロのみ）
    ///
    /// 関数マクロは識別子のまま残す。
    pub fn expand(&self, tokens: &[Token]) -> Vec<Token> {
        let mut visited = HashSet::new();
        self.expand_internal(tokens, &mut visited)
    }

    /// トークン列をマクロ展開する（関数マクロ呼び出しも含む）
    ///
    /// `FOO(a, b)` のような関数マクロ呼び出しも展開する。
    pub fn expand_with_calls(&self, tokens: &[Token]) -> Vec<Token> {
        let mut visited = HashSet::new();
        self.expand_with_calls_internal(tokens, &mut visited)
    }

    /// 内部展開ロジック（オブジェクトマクロのみ）
    fn expand_internal(&self, tokens: &[Token], visited: &mut HashSet<InternedStr>) -> Vec<Token> {
        let mut result = Vec::new();

        for token in tokens {
            match &token.kind {
                TokenKind::Ident(id) => {
                    // 展開禁止リストにあればそのまま
                    if self.no_expand.contains(id) {
                        result.push(token.clone());
                        continue;
                    }

                    // bindings.rs に定数として存在すればそのまま
                    if let Some(consts) = self.bindings_consts {
                        let name_str = self.interner.get(*id);
                        if consts.contains(name_str) {
                            result.push(token.clone());
                            continue;
                        }
                    }

                    // 再帰防止
                    if visited.contains(id) {
                        result.push(token.clone());
                        continue;
                    }

                    // マクロを検索
                    if let Some(def) = self.macro_table.get(*id) {
                        // オブジェクトマクロのみ展開
                        if !def.is_function() {
                            visited.insert(*id);
                            let expanded = self.expand_object_macro(def, token, visited);
                            result.extend(expanded);
                            visited.remove(id);
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

        result
    }

    /// 内部展開ロジック（関数マクロ呼び出しも含む）
    fn expand_with_calls_internal(
        &self,
        tokens: &[Token],
        visited: &mut HashSet<InternedStr>,
    ) -> Vec<Token> {
        let mut result = Vec::new();
        let mut i = 0;

        while i < tokens.len() {
            let token = &tokens[i];

            match &token.kind {
                TokenKind::Ident(id) => {
                    // 展開禁止リストにあればそのまま
                    if self.no_expand.contains(id) {
                        result.push(token.clone());
                        i += 1;
                        continue;
                    }

                    // bindings.rs に定数として存在すればそのまま
                    if let Some(consts) = self.bindings_consts {
                        let name_str = self.interner.get(*id);
                        if consts.contains(name_str) {
                            result.push(token.clone());
                            i += 1;
                            continue;
                        }
                    }

                    // 再帰防止
                    if visited.contains(id) {
                        result.push(token.clone());
                        i += 1;
                        continue;
                    }

                    // マクロを検索
                    if let Some(def) = self.macro_table.get(*id) {
                        if def.is_function() {
                            // 関数マクロ: 次のトークンが '(' かチェック
                            if let Some((args, end_idx)) = self.try_collect_args(&tokens[i + 1..]) {
                                visited.insert(*id);
                                let expanded =
                                    self.expand_function_macro(def, token, &args, visited);
                                result.extend(expanded);
                                visited.remove(id);
                                i += 1 + end_idx + 1; // id + args + closing paren
                                continue;
                            }
                        } else {
                            // オブジェクトマクロ
                            visited.insert(*id);
                            let expanded = self.expand_object_macro(def, token, visited);
                            result.extend(expanded);
                            visited.remove(id);
                            i += 1;
                            continue;
                        }
                    }
                    result.push(token.clone());
                    i += 1;
                }
                _ => {
                    result.push(token.clone());
                    i += 1;
                }
            }
        }

        result
    }

    /// オブジェクトマクロを展開
    fn expand_object_macro(
        &self,
        def: &MacroDef,
        trigger_token: &Token,
        visited: &mut HashSet<InternedStr>,
    ) -> Vec<Token> {
        // 再帰的に展開
        let expanded = self.expand_internal(&def.body, visited);

        // 呼び出し位置を設定
        let tokens_with_loc: Vec<Token> = expanded
            .into_iter()
            .map(|mut t| {
                t.loc = trigger_token.loc.clone();
                t
            })
            .collect();

        // マーカーで囲む
        self.wrap_with_markers(
            tokens_with_loc,
            def.name,
            trigger_token,
            MacroInvocationKind::Object,
        )
    }

    /// 関数マクロを展開
    fn expand_function_macro(
        &self,
        def: &MacroDef,
        trigger_token: &Token,
        args: &[Vec<Token>],
        visited: &mut HashSet<InternedStr>,
    ) -> Vec<Token> {
        let (params, is_variadic) = match &def.kind {
            MacroKind::Function { params, is_variadic } => (params, *is_variadic),
            _ => return vec![trigger_token.clone()],
        };

        // 引数マップを構築
        let arg_map = self.build_arg_map(params, args, is_variadic, &trigger_token.loc);

        // ボディを置換しながら展開
        let expanded = self.substitute_and_expand(&def.body, &arg_map, visited);

        // 呼び出し位置を設定
        let tokens_with_loc: Vec<Token> = expanded
            .into_iter()
            .map(|mut t| {
                t.loc = trigger_token.loc.clone();
                t
            })
            .collect();

        // マーカーで囲む
        self.wrap_with_markers(
            tokens_with_loc,
            def.name,
            trigger_token,
            MacroInvocationKind::Function {
                args: args.to_vec(),
            },
        )
    }

    /// 引数マップを構築
    fn build_arg_map(
        &self,
        params: &[InternedStr],
        args: &[Vec<Token>],
        is_variadic: bool,
        _loc: &SourceLocation,
    ) -> HashMap<InternedStr, Vec<Token>> {
        let mut arg_map = HashMap::new();

        if is_variadic && !params.is_empty() {
            // 可変長引数の処理
            let normal_param_count = params.len() - 1;
            for (i, param) in params.iter().take(normal_param_count).enumerate() {
                if i < args.len() {
                    arg_map.insert(*param, args[i].clone());
                } else {
                    arg_map.insert(*param, Vec::new());
                }
            }

            // 可変長部分
            let mut va = Vec::new();
            for (i, arg) in args.iter().enumerate().skip(normal_param_count) {
                if i > normal_param_count {
                    va.push(Token::new(TokenKind::Comma, SourceLocation::default()));
                }
                va.extend(arg.clone());
            }
            if let Some(last_param) = params.last() {
                arg_map.insert(*last_param, va);
            }
        } else {
            // 通常の引数
            for (i, param) in params.iter().enumerate() {
                if i < args.len() {
                    arg_map.insert(*param, args[i].clone());
                } else {
                    arg_map.insert(*param, Vec::new());
                }
            }
        }

        arg_map
    }

    /// パラメータを置換しながら展開
    fn substitute_and_expand(
        &self,
        body: &[Token],
        arg_map: &HashMap<InternedStr, Vec<Token>>,
        visited: &mut HashSet<InternedStr>,
    ) -> Vec<Token> {
        let mut result = Vec::new();

        for token in body {
            match &token.kind {
                TokenKind::Ident(id) => {
                    // パラメータなら置換
                    if let Some(arg_tokens) = arg_map.get(id) {
                        // 引数を展開して追加
                        let expanded = self.expand_with_calls_internal(arg_tokens, visited);
                        result.extend(expanded);
                    } else if self.no_expand.contains(id) || visited.contains(id) {
                        result.push(token.clone());
                    } else if let Some(consts) = self.bindings_consts {
                        // bindings.rs に定数として存在すればそのまま
                        let name_str = self.interner.get(*id);
                        if consts.contains(name_str) {
                            result.push(token.clone());
                        } else if let Some(def) = self.macro_table.get(*id) {
                            // マクロ呼び出し（関数マクロでなければ展開）
                            if !def.is_function() {
                                visited.insert(*id);
                                let expanded = self.expand_object_macro(def, token, visited);
                                result.extend(expanded);
                                visited.remove(id);
                            } else {
                                result.push(token.clone());
                            }
                        } else {
                            result.push(token.clone());
                        }
                    } else if let Some(def) = self.macro_table.get(*id) {
                        // マクロ呼び出し（関数マクロでなければ展開）
                        if !def.is_function() {
                            visited.insert(*id);
                            let expanded = self.expand_object_macro(def, token, visited);
                            result.extend(expanded);
                            visited.remove(id);
                        } else {
                            result.push(token.clone());
                        }
                    } else {
                        result.push(token.clone());
                    }
                }
                _ => {
                    result.push(token.clone());
                }
            }
        }

        result
    }

    /// トークン列から関数マクロの引数を収集する
    ///
    /// 成功した場合、(引数リスト, 消費したトークン数) を返す。
    /// '(' で始まっていなければ None を返す。
    fn try_collect_args(&self, tokens: &[Token]) -> Option<(Vec<Vec<Token>>, usize)> {
        if tokens.is_empty() {
            return None;
        }

        // 空白をスキップして '(' を探す
        let mut start = 0;
        while start < tokens.len() {
            match &tokens[start].kind {
                TokenKind::Space | TokenKind::Newline => start += 1,
                TokenKind::LParen => break,
                _ => return None,
            }
        }

        if start >= tokens.len() || !matches!(tokens[start].kind, TokenKind::LParen) {
            return None;
        }

        // 引数を収集
        let mut args: Vec<Vec<Token>> = Vec::new();
        let mut current_arg = Vec::new();
        let mut paren_depth = 0;
        let mut i = start + 1; // '(' の次から

        while i < tokens.len() {
            let token = &tokens[i];
            match &token.kind {
                TokenKind::LParen => {
                    paren_depth += 1;
                    current_arg.push(token.clone());
                }
                TokenKind::RParen => {
                    if paren_depth == 0 {
                        // 引数の終わり
                        if !current_arg.is_empty() || !args.is_empty() {
                            args.push(current_arg);
                        }
                        return Some((args, i));
                    }
                    paren_depth -= 1;
                    current_arg.push(token.clone());
                }
                TokenKind::Comma if paren_depth == 0 => {
                    args.push(current_arg);
                    current_arg = Vec::new();
                }
                _ => {
                    current_arg.push(token.clone());
                }
            }
            i += 1;
        }

        // 閉じ括弧が見つからなかった
        None
    }

    /// マクロ展開結果を MacroBegin/MacroEnd マーカーで囲む
    fn wrap_with_markers(
        &self,
        tokens: Vec<Token>,
        macro_name: InternedStr,
        trigger_token: &Token,
        kind: MacroInvocationKind,
    ) -> Vec<Token> {
        if !self.emit_markers {
            return tokens;
        }

        let marker_id = TokenId::next();
        let call_loc = trigger_token.loc.clone();

        // MacroBegin マーカー
        let begin_info = MacroBeginInfo {
            marker_id,
            trigger_token_id: trigger_token.id,
            macro_name,
            kind,
            call_loc: call_loc.clone(),
        };
        let begin_token = Token::new(
            TokenKind::MacroBegin(Box::new(begin_info)),
            call_loc.clone(),
        );

        // MacroEnd マーカー
        let end_info = MacroEndInfo {
            begin_marker_id: marker_id,
        };
        let end_token = Token::new(TokenKind::MacroEnd(end_info), call_loc);

        let mut result = Vec::with_capacity(tokens.len() + 2);
        result.push(begin_token);
        result.extend(tokens);
        result.push(end_token);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_interner_and_table() -> (StringInterner, MacroTable, FileRegistry) {
        (StringInterner::new(), MacroTable::new(), FileRegistry::new())
    }

    fn make_token(interner: &mut StringInterner, name: &str) -> Token {
        let id = interner.intern(name);
        Token::new(TokenKind::Ident(id), SourceLocation::default())
    }

    fn define_object_macro(
        interner: &mut StringInterner,
        table: &mut MacroTable,
        name: &str,
        body: Vec<Token>,
    ) {
        let macro_name = interner.intern(name);
        let has_token_pasting = body.iter()
            .any(|t| matches!(t.kind, TokenKind::HashHash));
        let def = MacroDef {
            name: macro_name,
            kind: MacroKind::Object,
            body,
            def_loc: SourceLocation::default(),
            leading_comments: vec![],
            is_builtin: false,
            is_target: true,
            has_token_pasting,
        };
        table.define(def, interner);
    }

    #[test]
    fn test_expand_object_macro() {
        let (mut interner, mut table, files) = make_interner_and_table();

        // FOO を 42 に展開するマクロを定義
        let value_token = Token::new(TokenKind::IntLit(42), SourceLocation::default());
        define_object_macro(&mut interner, &mut table, "FOO", vec![value_token]);

        // FOO を含むトークン列を展開
        let foo_token = make_token(&mut interner, "FOO");
        let expander = TokenExpander::new(&table, &interner, &files);
        let result = expander.expand(&[foo_token]);

        assert_eq!(result.len(), 1);
        assert!(matches!(result[0].kind, TokenKind::IntLit(42)));
    }

    #[test]
    fn test_expand_nested_macro() {
        let (mut interner, mut table, files) = make_interner_and_table();

        // BAR を 100 に展開
        let value_token = Token::new(TokenKind::IntLit(100), SourceLocation::default());
        define_object_macro(&mut interner, &mut table, "BAR", vec![value_token]);

        // FOO を BAR に展開
        let bar_token = make_token(&mut interner, "BAR");
        define_object_macro(&mut interner, &mut table, "FOO", vec![bar_token]);

        // FOO を展開すると最終的に 100 になる
        let foo_token = make_token(&mut interner, "FOO");
        let expander = TokenExpander::new(&table, &interner, &files);
        let result = expander.expand(&[foo_token]);

        assert_eq!(result.len(), 1);
        assert!(matches!(result[0].kind, TokenKind::IntLit(100)));
    }

    #[test]
    fn test_no_expand() {
        let (mut interner, mut table, files) = make_interner_and_table();

        // FOO を 42 に展開するマクロを定義
        let value_token = Token::new(TokenKind::IntLit(42), SourceLocation::default());
        define_object_macro(&mut interner, &mut table, "FOO", vec![value_token]);

        // FOO を展開禁止にする
        let foo_id = interner.intern("FOO");

        // FOO トークンを先に作成
        let foo_token = make_token(&mut interner, "FOO");

        let mut expander = TokenExpander::new(&table, &interner, &files);
        expander.add_no_expand(foo_id);

        // FOO は展開されない
        let result = expander.expand(&[foo_token]);

        assert_eq!(result.len(), 1);
        assert!(matches!(result[0].kind, TokenKind::Ident(_)));
    }

    #[test]
    fn test_self_referential_macro() {
        let (mut interner, mut table, files) = make_interner_and_table();

        // FOO を FOO + 1 に展開するマクロ（自己参照）
        let foo_token = make_token(&mut interner, "FOO");
        let plus_token = Token::new(TokenKind::Plus, SourceLocation::default());
        let one_token = Token::new(TokenKind::IntLit(1), SourceLocation::default());
        define_object_macro(
            &mut interner,
            &mut table,
            "FOO",
            vec![foo_token, plus_token, one_token],
        );

        // FOO を展開すると FOO + 1 になる（無限再帰しない）
        let foo_token = make_token(&mut interner, "FOO");
        let expander = TokenExpander::new(&table, &interner, &files);
        let result = expander.expand(&[foo_token]);

        assert_eq!(result.len(), 3);
        assert!(matches!(result[0].kind, TokenKind::Ident(_)));
        assert!(matches!(result[1].kind, TokenKind::Plus));
        assert!(matches!(result[2].kind, TokenKind::IntLit(1)));
    }

    #[test]
    fn test_bindings_consts_suppression() {
        let (mut interner, mut table, files) = make_interner_and_table();

        // BAR を 100 に展開するマクロを定義
        let value_token = Token::new(TokenKind::IntLit(100), SourceLocation::default());
        define_object_macro(&mut interner, &mut table, "BAR", vec![value_token]);

        // BAR を含むトークン列を作成
        let bar_token = make_token(&mut interner, "BAR");

        // bindings_consts に BAR を登録
        let mut bindings_consts: HashMap<String, String> = HashMap::new();
        bindings_consts.insert("BAR".to_string(), "u32".to_string());

        // bindings_consts を設定
        let mut expander = TokenExpander::new(&table, &interner, &files);
        expander.set_bindings_consts(&bindings_consts);

        // BAR は展開されない（bindings_consts に存在するため）
        let result = expander.expand(&[bar_token]);

        assert_eq!(result.len(), 1);
        // 識別子のまま残る
        assert!(matches!(result[0].kind, TokenKind::Ident(_)));
    }

    #[test]
    fn test_bindings_consts_not_in_list() {
        let (mut interner, mut table, files) = make_interner_and_table();

        // BAR を 100 に展開するマクロを定義
        let value_token = Token::new(TokenKind::IntLit(100), SourceLocation::default());
        define_object_macro(&mut interner, &mut table, "BAR", vec![value_token]);

        // BAR を含むトークン列を作成
        let bar_token = make_token(&mut interner, "BAR");

        // bindings_consts に BAR は含まない
        let mut bindings_consts: HashMap<String, String> = HashMap::new();
        bindings_consts.insert("FOO".to_string(), "u32".to_string());

        // bindings_consts を設定
        let mut expander = TokenExpander::new(&table, &interner, &files);
        expander.set_bindings_consts(&bindings_consts);

        // BAR は展開される（bindings_consts に存在しないため）
        let result = expander.expand(&[bar_token]);

        assert_eq!(result.len(), 1);
        // 100 に展開される
        assert!(matches!(result[0].kind, TokenKind::IntLit(100)));
    }
}
