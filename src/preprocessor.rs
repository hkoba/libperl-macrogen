//! Cプリプロセッサ
//!
//! tinycc の tccpp.c に相当する機能を提供する。
//! next_token() がメインのインターフェースで、マクロ展開済みのトークンを返す。

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{CompileError, PPError};
use crate::intern::{InternedStr, StringInterner};
use crate::lexer::Lexer;
use crate::macro_def::{MacroDef, MacroKind, MacroTable};
use crate::pp_expr::PPExprEvaluator;
use crate::source::{FileId, FileRegistry, SourceLocation};
use crate::token::{Comment, Token, TokenKind};

/// インクルードパスの種類
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncludeKind {
    /// <...> システムヘッダ
    System,
    /// "..." ローカルヘッダ
    Local,
}

/// プリプロセッサ設定
#[derive(Debug, Default, Clone)]
pub struct PPConfig {
    /// システムインクルードパス (-I)
    pub include_paths: Vec<PathBuf>,
    /// 事前定義マクロ (-D)
    pub predefined: Vec<(String, Option<String>)>,
}

/// 条件コンパイル状態
#[derive(Debug, Clone)]
struct CondState {
    /// 現在のブランチが有効か
    active: bool,
    /// いずれかのブランチが有効だったか
    seen_active: bool,
    /// #else を見たか
    seen_else: bool,
    /// ディレクティブの位置
    loc: SourceLocation,
}

/// 入力ソース（ファイルまたはマクロ展開）
struct InputSource {
    /// ソースバイト列
    source: Vec<u8>,
    /// 現在位置
    pos: usize,
    /// 行番号
    line: u32,
    /// 列番号
    column: u32,
    /// ファイルID
    file_id: FileId,
    /// 行頭フラグ（ディレクティブ検出用）
    at_line_start: bool,
    /// トークンバッファ（マクロ展開の場合）
    tokens: Option<Vec<Token>>,
    /// トークンバッファの位置
    token_pos: usize,
}

impl InputSource {
    /// ファイルから作成
    fn from_file(source: Vec<u8>, file_id: FileId) -> Self {
        Self {
            source,
            pos: 0,
            line: 1,
            column: 1,
            file_id,
            at_line_start: true,
            tokens: None,
            token_pos: 0,
        }
    }

    /// トークン列から作成（マクロ展開用）
    fn from_tokens(tokens: Vec<Token>, loc: SourceLocation) -> Self {
        Self {
            source: Vec::new(),
            pos: 0,
            line: loc.line,
            column: loc.column,
            file_id: loc.file_id,
            at_line_start: false,
            tokens: Some(tokens),
            token_pos: 0,
        }
    }

    /// トークンソースかどうか
    fn is_token_source(&self) -> bool {
        self.tokens.is_some()
    }

    /// 次のトークンを取得（トークンソースの場合）
    fn next_buffered_token(&mut self) -> Option<Token> {
        if let Some(ref tokens) = self.tokens {
            if self.token_pos < tokens.len() {
                let token = tokens[self.token_pos].clone();
                self.token_pos += 1;
                return Some(token);
            }
        }
        None
    }

    /// 行頭かどうか
    fn is_at_line_start(&self) -> bool {
        self.at_line_start
    }

    /// 現在位置を取得
    fn current_location(&self) -> SourceLocation {
        SourceLocation::new(self.file_id, self.line, self.column)
    }

    /// 行継続をスキップした実際の位置を取得
    fn skip_line_continuations(&self, start_pos: usize) -> usize {
        let mut pos = start_pos;
        loop {
            // \ の後に改行があれば行継続
            if self.source.get(pos) == Some(&b'\\') {
                let next = self.source.get(pos + 1);
                if next == Some(&b'\n') {
                    pos += 2;
                    continue;
                } else if next == Some(&b'\r') && self.source.get(pos + 2) == Some(&b'\n') {
                    // Windows形式の改行 (\r\n)
                    pos += 3;
                    continue;
                }
            }
            break;
        }
        pos
    }

    /// 現在の文字をピーク（行継続を処理）
    fn peek(&self) -> Option<u8> {
        let pos = self.skip_line_continuations(self.pos);
        self.source.get(pos).copied()
    }

    /// n文字先をピーク（行継続を処理）
    fn peek_n(&self, n: usize) -> Option<u8> {
        let mut pos = self.pos;
        for i in 0..=n {
            pos = self.skip_line_continuations(pos);
            if pos >= self.source.len() {
                return None;
            }
            if i < n {
                pos += 1;
            }
        }
        self.source.get(pos).copied()
    }

    /// 1文字進める（行継続を処理）
    fn advance(&mut self) -> Option<u8> {
        // 行継続をスキップ
        let old_pos = self.pos;
        self.pos = self.skip_line_continuations(self.pos);

        // スキップした行継続の分だけ行番号を更新
        for i in old_pos..self.pos {
            if self.source.get(i) == Some(&b'\n') {
                self.line += 1;
            }
        }

        let c = self.source.get(self.pos).copied()?;
        self.pos += 1;

        if c == b'\n' {
            self.line += 1;
            self.column = 1;
            self.at_line_start = true;
        } else {
            self.column += 1;
            if c != b' ' && c != b'\t' && c != b'\r' {
                self.at_line_start = false;
            }
        }
        Some(c)
    }

    /// 空白をスキップ（改行は含まない）
    fn skip_whitespace(&mut self) {
        while let Some(c) = self.peek() {
            // space, tab, carriage return, form feed (^L), vertical tab
            if c == b' ' || c == b'\t' || c == b'\r' || c == 0x0C || c == 0x0B {
                self.advance();
            } else {
                break;
            }
        }
    }
}

/// プリプロセッサ
pub struct Preprocessor {
    /// ファイルレジストリ
    files: FileRegistry,
    /// 文字列インターナー
    interner: StringInterner,
    /// マクロテーブル
    macros: MacroTable,
    /// 設定
    config: PPConfig,
    /// 入力ソーススタック
    sources: Vec<InputSource>,
    /// 条件コンパイルスタック
    cond_stack: Vec<CondState>,
    /// 現在展開中のマクロ（再帰検出用）
    expanding: HashSet<InternedStr>,
    /// 先読みトークンバッファ
    lookahead: Vec<Token>,
    /// 収集中のコメント
    pending_comments: Vec<Comment>,
    /// 現在の条件が有効かどうかのキャッシュ
    cond_active: bool,
    /// スペースをトークンとして返すかどうか（TinyCC の PARSE_FLAG_SPACES 相当）
    return_spaces: bool,
}

impl Preprocessor {
    /// 新しいプリプロセッサを作成
    pub fn new(config: PPConfig) -> Self {
        let mut pp = Self {
            files: FileRegistry::new(),
            interner: StringInterner::new(),
            macros: MacroTable::new(),
            config,
            sources: Vec::new(),
            cond_stack: Vec::new(),
            expanding: HashSet::new(),
            lookahead: Vec::new(),
            pending_comments: Vec::new(),
            cond_active: true,
            return_spaces: false,
        };

        // 事前定義マクロを登録
        pp.define_predefined_macros();

        pp
    }

    /// 事前定義マクロを登録
    fn define_predefined_macros(&mut self) {
        // 設定から事前定義マクロを登録
        for (name, value) in self.config.predefined.clone() {
            let name_id = self.interner.intern(&name);
            let body = if let Some(val) = value {
                // 値を字句解析
                self.tokenize_string(&val)
            } else {
                // 値なしは 1 として扱う
                vec![Token::new(TokenKind::IntLit(1), SourceLocation::default())]
            };
            let def = MacroDef::object(name_id, body, SourceLocation::default()).as_builtin();
            self.macros.define(def);
        }
    }

    /// 文字列をトークン列に変換
    fn tokenize_string(&mut self, s: &str) -> Vec<Token> {
        let bytes = s.as_bytes();
        let file_id = FileId::default();
        let mut lexer = Lexer::new(bytes, file_id, &mut self.interner);

        let mut tokens = Vec::new();
        loop {
            match lexer.next_token() {
                Ok(token) => {
                    if matches!(token.kind, TokenKind::Eof) {
                        break;
                    }
                    if !matches!(token.kind, TokenKind::Newline) {
                        tokens.push(token);
                    }
                }
                Err(_) => break,
            }
        }
        tokens
    }

    /// ファイルを処理開始
    pub fn process_file(&mut self, path: &Path) -> Result<(), CompileError> {
        let source = fs::read(path).map_err(|e| {
            CompileError::Preprocess {
                loc: SourceLocation::default(),
                kind: PPError::IoError(path.to_path_buf(), e.to_string()),
            }
        })?;

        let file_id = self.files.register(path.to_path_buf());
        let input = InputSource::from_file(source, file_id);
        self.sources.push(input);

        Ok(())
    }

    /// 現在のソースからレキサー経由でトークンを取得
    fn lex_token_from_source(&mut self) -> Result<Option<Token>, CompileError> {
        // トークンソースかどうかを先にチェック
        {
            let Some(source) = self.sources.last_mut() else {
                return Ok(None);
            };

            if source.is_token_source() {
                return Ok(source.next_buffered_token());
            }

            // return_spaces モードの場合、空白をトークンとして返す
            if self.return_spaces {
                if let Some(c) = source.peek() {
                    // space, tab, form feed, vertical tab
                    if c == b' ' || c == b'\t' || c == 0x0C || c == 0x0B {
                        let loc = source.current_location();
                        source.advance();
                        // 連続する空白は1つのSpaceトークンにまとめる
                        while let Some(c) = source.peek() {
                            if c == b' ' || c == b'\t' || c == 0x0C || c == 0x0B {
                                source.advance();
                            } else {
                                break;
                            }
                        }
                        return Ok(Some(Token::new(TokenKind::Space, loc)));
                    }
                }
            } else {
                source.skip_whitespace();
            }
        }

        // コメントを処理
        let mut leading_comments = Vec::new();
        loop {
            {
                let Some(source) = self.sources.last_mut() else {
                    return Ok(None);
                };
                if !self.return_spaces {
                    source.skip_whitespace();
                }
            }

            let (is_line_comment, is_block_comment) = {
                let Some(source) = self.sources.last() else {
                    return Ok(None);
                };
                (
                    source.peek() == Some(b'/') && source.peek_n(1) == Some(b'/'),
                    source.peek() == Some(b'/') && source.peek_n(1) == Some(b'*'),
                )
            };

            if is_line_comment {
                let comment = self.scan_line_comment();
                leading_comments.push(comment);
            } else if is_block_comment {
                let comment = self.scan_block_comment()?;
                leading_comments.push(comment);
            } else {
                break;
            }
        }

        let loc = {
            let Some(source) = self.sources.last() else {
                return Ok(None);
            };
            source.current_location()
        };

        let kind = self.scan_token_kind()?;

        let mut token = Token::new(kind, loc);
        token.leading_comments = leading_comments;
        Ok(Some(token))
    }

    /// 行コメントをスキャン
    fn scan_line_comment(&mut self) -> Comment {
        let source = self.sources.last_mut().unwrap();
        let loc = source.current_location();
        source.advance(); // /
        source.advance(); // /

        let start = source.pos;
        while source.peek().is_some_and(|c| c != b'\n') {
            source.advance();
        }
        let text = String::from_utf8_lossy(&source.source[start..source.pos]).to_string();

        Comment::new(crate::token::CommentKind::Line, text, loc)
    }

    /// ブロックコメントをスキャン
    fn scan_block_comment(&mut self) -> Result<Comment, CompileError> {
        let source = self.sources.last_mut().unwrap();
        let loc = source.current_location();
        source.advance(); // /
        source.advance(); // *

        let start = source.pos;
        loop {
            match (source.peek(), source.peek_n(1)) {
                (Some(b'*'), Some(b'/')) => {
                    let end = source.pos;
                    source.advance(); // *
                    source.advance(); // /
                    let text = String::from_utf8_lossy(&source.source[start..end]).to_string();
                    return Ok(Comment::new(crate::token::CommentKind::Block, text, loc));
                }
                (Some(_), _) => {
                    source.advance();
                }
                (None, _) => {
                    return Err(CompileError::Lex {
                        loc,
                        kind: crate::error::LexError::UnterminatedComment,
                    });
                }
            }
        }
    }

    /// トークン種別をスキャン
    fn scan_token_kind(&mut self) -> Result<TokenKind, CompileError> {
        let source = self.sources.last_mut().unwrap();
        let Some(c) = source.peek() else {
            return Ok(TokenKind::Eof);
        };

        match c {
            b'\n' => {
                source.advance();
                Ok(TokenKind::Newline)
            }

            // ワイド文字列/文字リテラル
            b'L' if matches!(source.peek_n(1), Some(b'"') | Some(b'\'')) => {
                source.advance(); // L
                if source.peek() == Some(b'"') {
                    self.scan_wide_string()
                } else {
                    self.scan_wide_char()
                }
            }

            // 識別子またはキーワード
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => self.scan_identifier(),

            // 数値リテラル
            b'0'..=b'9' => self.scan_number(),

            // 文字列リテラル
            b'"' => self.scan_string(),

            // 文字リテラル
            b'\'' => self.scan_char(),

            // 演算子・区切り記号
            b'+' => self.scan_operator(b'+', &[(b'+', TokenKind::PlusPlus), (b'=', TokenKind::PlusEq)], TokenKind::Plus),
            b'-' => self.scan_operator(b'-', &[(b'-', TokenKind::MinusMinus), (b'=', TokenKind::MinusEq), (b'>', TokenKind::Arrow)], TokenKind::Minus),
            b'*' => self.scan_operator(b'*', &[(b'=', TokenKind::StarEq)], TokenKind::Star),
            b'/' => self.scan_operator(b'/', &[(b'=', TokenKind::SlashEq)], TokenKind::Slash),
            b'%' => self.scan_operator(b'%', &[(b'=', TokenKind::PercentEq)], TokenKind::Percent),
            b'&' => self.scan_operator(b'&', &[(b'&', TokenKind::AmpAmp), (b'=', TokenKind::AmpEq)], TokenKind::Amp),
            b'|' => self.scan_operator(b'|', &[(b'|', TokenKind::PipePipe), (b'=', TokenKind::PipeEq)], TokenKind::Pipe),
            b'^' => self.scan_operator(b'^', &[(b'=', TokenKind::CaretEq)], TokenKind::Caret),
            b'~' => {
                source.advance();
                Ok(TokenKind::Tilde)
            }
            b'!' => self.scan_operator(b'!', &[(b'=', TokenKind::BangEq)], TokenKind::Bang),
            b'<' => self.scan_lt(),
            b'>' => self.scan_gt(),
            b'=' => self.scan_operator(b'=', &[(b'=', TokenKind::EqEq)], TokenKind::Eq),
            b'?' => {
                source.advance();
                Ok(TokenKind::Question)
            }
            b':' => {
                source.advance();
                Ok(TokenKind::Colon)
            }
            b'.' => self.scan_dot(),
            b',' => {
                source.advance();
                Ok(TokenKind::Comma)
            }
            b';' => {
                source.advance();
                Ok(TokenKind::Semi)
            }
            b'(' => {
                source.advance();
                Ok(TokenKind::LParen)
            }
            b')' => {
                source.advance();
                Ok(TokenKind::RParen)
            }
            b'[' => {
                source.advance();
                Ok(TokenKind::LBracket)
            }
            b']' => {
                source.advance();
                Ok(TokenKind::RBracket)
            }
            b'{' => {
                source.advance();
                Ok(TokenKind::LBrace)
            }
            b'}' => {
                source.advance();
                Ok(TokenKind::RBrace)
            }
            b'#' => {
                source.advance();
                if source.peek() == Some(b'#') {
                    source.advance();
                    Ok(TokenKind::HashHash)
                } else {
                    Ok(TokenKind::Hash)
                }
            }

            _ => {
                let loc = source.current_location();
                source.advance();
                Err(CompileError::Lex {
                    loc,
                    kind: crate::error::LexError::InvalidChar(c as char),
                })
            }
        }
    }

    /// 汎用演算子スキャン
    fn scan_operator(&mut self, _first: u8, continuations: &[(u8, TokenKind)], default: TokenKind) -> Result<TokenKind, CompileError> {
        let source = self.sources.last_mut().unwrap();
        source.advance();
        for (next, kind) in continuations {
            if source.peek() == Some(*next) {
                source.advance();
                return Ok(kind.clone());
            }
        }
        Ok(default)
    }

    /// < 演算子のスキャン
    fn scan_lt(&mut self) -> Result<TokenKind, CompileError> {
        let source = self.sources.last_mut().unwrap();
        source.advance();
        match source.peek() {
            Some(b'<') => {
                source.advance();
                if source.peek() == Some(b'=') {
                    source.advance();
                    Ok(TokenKind::LtLtEq)
                } else {
                    Ok(TokenKind::LtLt)
                }
            }
            Some(b'=') => {
                source.advance();
                Ok(TokenKind::LtEq)
            }
            _ => Ok(TokenKind::Lt),
        }
    }

    /// > 演算子のスキャン
    fn scan_gt(&mut self) -> Result<TokenKind, CompileError> {
        let source = self.sources.last_mut().unwrap();
        source.advance();
        match source.peek() {
            Some(b'>') => {
                source.advance();
                if source.peek() == Some(b'=') {
                    source.advance();
                    Ok(TokenKind::GtGtEq)
                } else {
                    Ok(TokenKind::GtGt)
                }
            }
            Some(b'=') => {
                source.advance();
                Ok(TokenKind::GtEq)
            }
            _ => Ok(TokenKind::Gt),
        }
    }

    /// . 演算子のスキャン
    fn scan_dot(&mut self) -> Result<TokenKind, CompileError> {
        let source = self.sources.last_mut().unwrap();
        source.advance();
        if source.peek() == Some(b'.') && source.peek_n(1) == Some(b'.') {
            source.advance();
            source.advance();
            Ok(TokenKind::Ellipsis)
        } else {
            Ok(TokenKind::Dot)
        }
    }

    /// 識別子をスキャン（tinycc方式：キーワード変換しない）
    fn scan_identifier(&mut self) -> Result<TokenKind, CompileError> {
        let source = self.sources.last_mut().unwrap();
        let start = source.pos;
        while let Some(c) = source.peek() {
            if c.is_ascii_alphanumeric() || c == b'_' {
                source.advance();
            } else {
                break;
            }
        }

        let text = std::str::from_utf8(&source.source[start..source.pos]).unwrap();

        // すべて識別子として返す（キーワード判定は後段で行う）
        let interned = self.interner.intern(text);
        Ok(TokenKind::Ident(interned))
    }

    /// 数値リテラルをスキャン
    fn scan_number(&mut self) -> Result<TokenKind, CompileError> {
        let source = self.sources.last_mut().unwrap();
        let loc = source.current_location();
        let start = source.pos;

        // 16進数、8進数、2進数の判定
        if source.peek() == Some(b'0') {
            source.advance();
            match source.peek() {
                Some(b'x') | Some(b'X') => {
                    source.advance();
                    while source.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
                        source.advance();
                    }
                }
                Some(b'b') | Some(b'B') => {
                    source.advance();
                    while matches!(source.peek(), Some(b'0') | Some(b'1')) {
                        source.advance();
                    }
                }
                Some(b'0'..=b'7') => {
                    while source.peek().is_some_and(|c| matches!(c, b'0'..=b'7')) {
                        source.advance();
                    }
                }
                Some(b'.') | Some(b'e') | Some(b'E') => {
                    return self.scan_float_from(start, loc);
                }
                _ => {}
            }
        } else {
            while source.peek().is_some_and(|c| c.is_ascii_digit()) {
                source.advance();
            }
            if matches!(source.peek(), Some(b'.') | Some(b'e') | Some(b'E')) {
                return self.scan_float_from(start, loc);
            }
        }

        self.finish_integer(start, loc)
    }

    /// 浮動小数点数をスキャン
    fn scan_float_from(&mut self, start: usize, loc: SourceLocation) -> Result<TokenKind, CompileError> {
        let source = self.sources.last_mut().unwrap();

        if source.peek() == Some(b'.') {
            source.advance();
            while source.peek().is_some_and(|c| c.is_ascii_digit()) {
                source.advance();
            }
        }

        if matches!(source.peek(), Some(b'e') | Some(b'E')) {
            source.advance();
            if matches!(source.peek(), Some(b'+') | Some(b'-')) {
                source.advance();
            }
            while source.peek().is_some_and(|c| c.is_ascii_digit()) {
                source.advance();
            }
        }

        if matches!(source.peek(), Some(b'f') | Some(b'F') | Some(b'l') | Some(b'L')) {
            source.advance();
        }

        let text = std::str::from_utf8(&source.source[start..source.pos]).unwrap();
        let value: f64 = text
            .trim_end_matches(|c| c == 'f' || c == 'F' || c == 'l' || c == 'L')
            .parse()
            .map_err(|_| CompileError::Lex {
                loc: loc.clone(),
                kind: crate::error::LexError::InvalidNumber(text.to_string()),
            })?;

        Ok(TokenKind::FloatLit(value))
    }

    /// 整数リテラルの仕上げ
    fn finish_integer(&mut self, start: usize, loc: SourceLocation) -> Result<TokenKind, CompileError> {
        let source = self.sources.last_mut().unwrap();

        // サフィックス
        let mut is_unsigned = false;
        let mut is_long = false;
        let mut is_longlong = false;

        loop {
            match source.peek() {
                Some(b'u') | Some(b'U') => {
                    is_unsigned = true;
                    source.advance();
                }
                Some(b'l') | Some(b'L') => {
                    if is_long {
                        is_longlong = true;
                    }
                    is_long = true;
                    source.advance();
                }
                _ => break,
            }
        }

        let text = std::str::from_utf8(&source.source[start..source.pos]).unwrap();

        // プレフィックスと基数を判定（チェイン trim ではなく排他的に処理）
        let (num_text, radix) = if text.starts_with("0x") || text.starts_with("0X") {
            (&text[2..], 16)
        } else if text.starts_with("0b") || text.starts_with("0B") {
            (&text[2..], 2)
        } else if text.starts_with('0') && text.len() > 1 {
            // 8進数（ただしサフィックスのみの場合は10進数の0として扱う）
            let without_suffix = text.trim_end_matches(|c: char| c == 'u' || c == 'U' || c == 'l' || c == 'L');
            if without_suffix.len() > 1 {
                (without_suffix, 8)
            } else {
                (without_suffix, 10)
            }
        } else {
            (text, 10)
        };

        // サフィックスを除去
        let num_text = num_text.trim_end_matches(|c: char| c == 'u' || c == 'U' || c == 'l' || c == 'L');

        if is_unsigned || is_longlong {
            let value = u64::from_str_radix(num_text, radix).map_err(|_| CompileError::Lex {
                loc: loc.clone(),
                kind: crate::error::LexError::InvalidNumber(text.to_string()),
            })?;
            Ok(TokenKind::UIntLit(value))
        } else {
            // まずi64でパースを試み、失敗したらu64でリトライ
            // (サフィックスなしでも大きな数値に対応)
            match i64::from_str_radix(num_text, radix) {
                Ok(value) => Ok(TokenKind::IntLit(value)),
                Err(_) => {
                    let value = u64::from_str_radix(num_text, radix).map_err(|_| CompileError::Lex {
                        loc: loc.clone(),
                        kind: crate::error::LexError::InvalidNumber(text.to_string()),
                    })?;
                    Ok(TokenKind::UIntLit(value))
                }
            }
        }
    }

    /// 文字列リテラルをスキャン
    fn scan_string(&mut self) -> Result<TokenKind, CompileError> {
        let loc = {
            let source = self.sources.last_mut().unwrap();
            let loc = source.current_location();
            source.advance(); // "
            loc
        };

        let mut bytes = Vec::new();
        loop {
            let c = {
                let source = self.sources.last_mut().unwrap();
                source.peek()
            };

            match c {
                Some(b'"') => {
                    let source = self.sources.last_mut().unwrap();
                    source.advance();
                    return Ok(TokenKind::StringLit(bytes));
                }
                Some(b'\\') => {
                    {
                        let source = self.sources.last_mut().unwrap();
                        source.advance();
                    }
                    let escaped = self.scan_escape_sequence(&loc)?;
                    bytes.push(escaped);
                }
                Some(b'\n') | None => {
                    return Err(CompileError::Lex {
                        loc,
                        kind: crate::error::LexError::UnterminatedString,
                    });
                }
                Some(c) => {
                    let source = self.sources.last_mut().unwrap();
                    source.advance();
                    bytes.push(c);
                }
            }
        }
    }

    /// ワイド文字列をスキャン
    fn scan_wide_string(&mut self) -> Result<TokenKind, CompileError> {
        let loc = {
            let source = self.sources.last_mut().unwrap();
            let loc = source.current_location();
            source.advance(); // "
            loc
        };

        let mut chars = Vec::new();
        loop {
            let c = {
                let source = self.sources.last_mut().unwrap();
                source.peek()
            };

            match c {
                Some(b'"') => {
                    let source = self.sources.last_mut().unwrap();
                    source.advance();
                    return Ok(TokenKind::WideStringLit(chars));
                }
                Some(b'\\') => {
                    {
                        let source = self.sources.last_mut().unwrap();
                        source.advance();
                    }
                    let escaped = self.scan_escape_sequence(&loc)?;
                    chars.push(escaped as u32);
                }
                Some(b'\n') | None => {
                    return Err(CompileError::Lex {
                        loc,
                        kind: crate::error::LexError::UnterminatedString,
                    });
                }
                Some(c) => {
                    let source = self.sources.last_mut().unwrap();
                    source.advance();
                    chars.push(c as u32);
                }
            }
        }
    }

    /// 文字リテラルをスキャン
    fn scan_char(&mut self) -> Result<TokenKind, CompileError> {
        let loc = {
            let source = self.sources.last_mut().unwrap();
            let loc = source.current_location();
            source.advance(); // '
            loc
        };

        let first_char = {
            let source = self.sources.last().unwrap();
            source.peek()
        };

        let value = match first_char {
            Some(b'\'') => {
                return Err(CompileError::Lex {
                    loc,
                    kind: crate::error::LexError::EmptyCharLit,
                });
            }
            Some(b'\\') => {
                {
                    let source = self.sources.last_mut().unwrap();
                    source.advance();
                }
                self.scan_escape_sequence(&loc)?
            }
            Some(c) => {
                let source = self.sources.last_mut().unwrap();
                source.advance();
                c
            }
            None => {
                return Err(CompileError::Lex {
                    loc,
                    kind: crate::error::LexError::UnterminatedChar,
                });
            }
        };

        let source = self.sources.last_mut().unwrap();
        if source.peek() != Some(b'\'') {
            return Err(CompileError::Lex {
                loc,
                kind: crate::error::LexError::UnterminatedChar,
            });
        }
        source.advance();

        Ok(TokenKind::CharLit(value))
    }

    /// ワイド文字をスキャン
    fn scan_wide_char(&mut self) -> Result<TokenKind, CompileError> {
        let loc = {
            let source = self.sources.last_mut().unwrap();
            let loc = source.current_location();
            source.advance(); // '
            loc
        };

        let first_char = {
            let source = self.sources.last().unwrap();
            source.peek()
        };

        let value = match first_char {
            Some(b'\'') => {
                return Err(CompileError::Lex {
                    loc,
                    kind: crate::error::LexError::EmptyCharLit,
                });
            }
            Some(b'\\') => {
                {
                    let source = self.sources.last_mut().unwrap();
                    source.advance();
                }
                self.scan_escape_sequence(&loc)? as u32
            }
            Some(c) => {
                let source = self.sources.last_mut().unwrap();
                source.advance();
                c as u32
            }
            None => {
                return Err(CompileError::Lex {
                    loc,
                    kind: crate::error::LexError::UnterminatedChar,
                });
            }
        };

        let source = self.sources.last_mut().unwrap();
        if source.peek() != Some(b'\'') {
            return Err(CompileError::Lex {
                loc,
                kind: crate::error::LexError::UnterminatedChar,
            });
        }
        source.advance();

        Ok(TokenKind::WideCharLit(value))
    }

    /// エスケープシーケンスをスキャン
    fn scan_escape_sequence(&mut self, loc: &SourceLocation) -> Result<u8, CompileError> {
        let source = self.sources.last_mut().unwrap();
        match source.peek() {
            Some(b'n') => { source.advance(); Ok(b'\n') }
            Some(b't') => { source.advance(); Ok(b'\t') }
            Some(b'r') => { source.advance(); Ok(b'\r') }
            Some(b'\\') => { source.advance(); Ok(b'\\') }
            Some(b'\'') => { source.advance(); Ok(b'\'') }
            Some(b'"') => { source.advance(); Ok(b'"') }
            Some(b'0') => { source.advance(); Ok(0) }
            Some(b'a') => { source.advance(); Ok(0x07) }
            Some(b'b') => { source.advance(); Ok(0x08) }
            Some(b'f') => { source.advance(); Ok(0x0C) }
            Some(b'v') => { source.advance(); Ok(0x0B) }
            Some(b'x') => {
                source.advance();
                let mut value = 0u8;
                let mut count = 0;
                while let Some(c) = source.peek() {
                    if let Some(digit) = (c as char).to_digit(16) {
                        value = value.wrapping_mul(16).wrapping_add(digit as u8);
                        source.advance();
                        count += 1;
                        if count >= 2 { break; }
                    } else {
                        break;
                    }
                }
                if count == 0 {
                    return Err(CompileError::Lex {
                        loc: loc.clone(),
                        kind: crate::error::LexError::InvalidEscape('x'),
                    });
                }
                Ok(value)
            }
            Some(c @ b'0'..=b'7') => {
                let mut value = (c - b'0') as u8;
                source.advance();
                for _ in 0..2 {
                    if let Some(c @ b'0'..=b'7') = source.peek() {
                        value = value * 8 + (c - b'0');
                        source.advance();
                    } else {
                        break;
                    }
                }
                Ok(value)
            }
            Some(c) => Err(CompileError::Lex {
                loc: loc.clone(),
                kind: crate::error::LexError::InvalidEscape(c as char),
            }),
            None => Err(CompileError::Lex {
                loc: loc.clone(),
                kind: crate::error::LexError::UnterminatedString,
            }),
        }
    }

    /// 次のトークンを取得（メインインターフェース）
    pub fn next_token(&mut self) -> Result<Token, CompileError> {
        loop {
            // 先読みバッファから取得
            if let Some(token) = self.lookahead.pop() {
                return Ok(token);
            }

            // 現在のソースからトークンを取得
            let token = match self.lex_token_from_source()? {
                Some(t) => t,
                None => {
                    // ソースが空 - ポップして続行
                    if self.sources.len() > 1 {
                        self.sources.pop();
                        continue;
                    }
                    Token::new(TokenKind::Eof, SourceLocation::default())
                }
            };

            // コメントを収集
            if !token.leading_comments.is_empty() {
                self.pending_comments.extend(token.leading_comments.iter().cloned());
            }

            match &token.kind {
                TokenKind::Eof => {
                    // 現在のソースが終了
                    if self.sources.len() > 1 {
                        self.sources.pop();
                        continue;
                    }

                    // 条件コンパイルスタックのチェック
                    if !self.cond_stack.is_empty() {
                        let state = &self.cond_stack[0];
                        return Err(CompileError::Preprocess {
                            loc: state.loc.clone(),
                            kind: PPError::MissingEndif,
                        });
                    }

                    return Ok(token);
                }

                TokenKind::Newline => {
                    // 改行は通常スキップ
                    continue;
                }

                TokenKind::Hash => {
                    // プリプロセッサディレクティブ
                    let at_line_start = self.sources.last().map(|s| s.is_at_line_start()).unwrap_or(false);
                    if at_line_start || self.sources.last().map(|s| s.is_token_source()).unwrap_or(false) {
                        // ファイルソースで行頭、またはトークンソース（#が先頭にある場合）
                    }
                    self.process_directive(token.loc.clone())?;
                    continue;
                }

                TokenKind::Ident(id) if self.cond_active => {
                    // マクロ展開を試みる
                    let id = *id;
                    if let Some(expanded) = self.try_expand_macro(id, &token)? {
                        // 展開結果を先読みバッファに追加（逆順）
                        for t in expanded.into_iter().rev() {
                            self.lookahead.push(t);
                        }
                        continue;
                    }
                    return Ok(self.attach_comments(token));
                }

                _ if !self.cond_active => {
                    // 条件コンパイルで無効なブランチ
                    continue;
                }

                _ => {
                    return Ok(self.attach_comments(token));
                }
            }
        }
    }

    /// 生のトークンを取得（マクロ展開なし）
    fn next_raw_token(&mut self) -> Result<Token, CompileError> {
        loop {
            // 先読みバッファから取得
            if let Some(token) = self.lookahead.pop() {
                return Ok(token);
            }

            match self.lex_token_from_source()? {
                Some(token) => {
                    if !token.leading_comments.is_empty() {
                        self.pending_comments.extend(token.leading_comments.iter().cloned());
                    }
                    return Ok(token);
                }
                None => {
                    if self.sources.len() > 1 {
                        self.sources.pop();
                        continue;
                    }
                    return Ok(Token::new(TokenKind::Eof, SourceLocation::default()));
                }
            }
        }
    }

    /// 蓄積したコメントをトークンに付与
    fn attach_comments(&mut self, mut token: Token) -> Token {
        if !self.pending_comments.is_empty() {
            token.leading_comments = std::mem::take(&mut self.pending_comments);
        }
        token
    }

    /// プリプロセッサディレクティブを処理
    fn process_directive(&mut self, loc: SourceLocation) -> Result<(), CompileError> {
        // ディレクティブ名を取得
        let directive_token = self.next_raw_token()?;

        match &directive_token.kind {
            TokenKind::Newline | TokenKind::Eof => {
                // 空のディレクティブ（許可）
                return Ok(());
            }
            TokenKind::Ident(id) => {
                let name = self.interner.get(*id).to_string();
                self.process_directive_by_name(&name, loc)?;
            }
            TokenKind::IntLit(_) => {
                // #line または # 123 "file" 形式
                self.skip_to_eol()?;
            }
            _ => {
                return Err(CompileError::Preprocess {
                    loc,
                    kind: PPError::InvalidDirective(format!("{:?}", directive_token.kind)),
                });
            }
        }

        Ok(())
    }

    /// ディレクティブ名に基づいて処理
    fn process_directive_by_name(&mut self, name: &str, loc: SourceLocation) -> Result<(), CompileError> {
        match name {
            "define" => {
                if self.cond_active {
                    self.process_define(loc)?;
                } else {
                    self.skip_to_eol()?;
                }
            }
            "undef" => {
                if self.cond_active {
                    self.process_undef()?;
                } else {
                    self.skip_to_eol()?;
                }
            }
            "include" => {
                if self.cond_active {
                    self.process_include(loc)?;
                } else {
                    self.skip_to_eol()?;
                }
            }
            "if" => self.process_if(loc)?,
            "ifdef" => self.process_ifdef(loc, false)?,
            "ifndef" => self.process_ifdef(loc, true)?,
            "elif" => self.process_elif(loc)?,
            "else" => self.process_else(loc)?,
            "endif" => self.process_endif()?,
            "error" => {
                if self.cond_active {
                    self.process_error(loc)?;
                } else {
                    self.skip_to_eol()?;
                }
            }
            "warning" | "pragma" | "line" => {
                self.skip_to_eol()?;
            }
            _ => {
                if self.cond_active {
                    return Err(CompileError::Preprocess {
                        loc,
                        kind: PPError::InvalidDirective(name.to_string()),
                    });
                } else {
                    self.skip_to_eol()?;
                }
            }
        }

        Ok(())
    }

    /// #define を処理
    fn process_define(&mut self, loc: SourceLocation) -> Result<(), CompileError> {
        let name_token = self.next_raw_token()?;
        let name = match name_token.kind {
            TokenKind::Ident(id) => id,
            _ => {
                return Err(CompileError::Preprocess {
                    loc,
                    kind: PPError::InvalidDirective("expected macro name".to_string()),
                });
            }
        };

        // TinyCC方式: スペースモードを有効にして次のトークンを取得
        // '(' がマクロ名の直後にある場合のみ関数マクロとして扱う
        self.return_spaces = true;
        let next = self.next_raw_token()?;
        self.return_spaces = false;

        let (kind, body_start) = if matches!(next.kind, TokenKind::LParen) {
            // マクロ名の直後に '(' があるので関数マクロ
            let (params, is_variadic) = self.parse_macro_params()?;
            (MacroKind::Function { params, is_variadic }, None)
        } else if matches!(next.kind, TokenKind::Space) {
            // スペースがあった場合、次のトークンを読んでオブジェクトマクロのボディとする
            let body_first = self.next_raw_token()?;
            (MacroKind::Object, Some(body_first))
        } else {
            // その他（改行など）はそのままオブジェクトマクロ
            (MacroKind::Object, Some(next))
        };

        let mut body = Vec::new();
        let mut need_more = true;
        if let Some(first) = body_start {
            if matches!(first.kind, TokenKind::Newline | TokenKind::Eof) {
                // 値なしマクロ：これ以上読む必要なし
                need_more = false;
            } else {
                body.push(first);
            }
        }

        if need_more {
            loop {
                let token = self.next_raw_token()?;
                match token.kind {
                    TokenKind::Newline | TokenKind::Eof => break,
                    _ => body.push(token),
                }
            }
        }

        let def = MacroDef {
            name,
            kind,
            body,
            def_loc: loc,
            leading_comments: std::mem::take(&mut self.pending_comments),
            is_builtin: false,
        };

        self.macros.define(def);
        Ok(())
    }

    /// 関数マクロのパラメータをパース
    /// GNU拡張: NAME... 形式の可変長引数もサポート
    fn parse_macro_params(&mut self) -> Result<(Vec<InternedStr>, bool), CompileError> {
        let mut params = Vec::new();
        let mut is_variadic = false;

        loop {
            let token = self.next_raw_token()?;
            match token.kind {
                TokenKind::RParen => break,
                TokenKind::Ident(id) => {
                    params.push(id);
                    let next = self.next_raw_token()?;
                    match next.kind {
                        TokenKind::Comma => continue,
                        TokenKind::RParen => break,
                        TokenKind::Ellipsis => {
                            // GNU拡張: NAME... 形式
                            // パラメータ名はそのまま保持し、variadic フラグをセット
                            is_variadic = true;
                            let rparen = self.next_raw_token()?;
                            if !matches!(rparen.kind, TokenKind::RParen) {
                                return Err(CompileError::Preprocess {
                                    loc: token.loc,
                                    kind: PPError::InvalidMacroArgs("expected ')' after '...'".to_string()),
                                });
                            }
                            break;
                        }
                        _ => {
                            return Err(CompileError::Preprocess {
                                loc: token.loc,
                                kind: PPError::InvalidMacroArgs("expected ',' or ')'".to_string()),
                            });
                        }
                    }
                }
                TokenKind::Ellipsis => {
                    // 標準 C99: ... のみ（__VA_ARGS__ として扱う）
                    is_variadic = true;
                    let next = self.next_raw_token()?;
                    if !matches!(next.kind, TokenKind::RParen) {
                        return Err(CompileError::Preprocess {
                            loc: token.loc,
                            kind: PPError::InvalidMacroArgs("expected ')' after '...'".to_string()),
                        });
                    }
                    break;
                }
                _ => {
                    return Err(CompileError::Preprocess {
                        loc: token.loc,
                        kind: PPError::InvalidMacroArgs("expected parameter name".to_string()),
                    });
                }
            }
        }

        Ok((params, is_variadic))
    }

    /// #undef を処理
    fn process_undef(&mut self) -> Result<(), CompileError> {
        let token = self.next_raw_token()?;
        if let TokenKind::Ident(id) = token.kind {
            self.macros.undefine(id);
        }
        self.skip_to_eol()?;
        Ok(())
    }

    /// #include を処理
    fn process_include(&mut self, loc: SourceLocation) -> Result<(), CompileError> {
        let token = self.next_raw_token()?;

        let (path, kind) = match &token.kind {
            TokenKind::StringLit(bytes) => {
                let path = String::from_utf8_lossy(bytes).to_string();
                (path, IncludeKind::Local)
            }
            TokenKind::Lt => {
                // TinyCC方式: トークナイザを使わず文字レベルで直接読み取る
                // ファイル名に含まれる "64.h" などが FloatLit として誤解析されるのを防ぐ
                let path = self.scan_include_path('>')?;
                (path, IncludeKind::System)
            }
            _ => {
                return Err(CompileError::Preprocess {
                    loc,
                    kind: PPError::InvalidDirective("expected include path".to_string()),
                });
            }
        };

        self.skip_to_eol()?;

        let resolved = self.resolve_include(&path, kind, &loc)?;

        let source = fs::read(&resolved).map_err(|e| {
            CompileError::Preprocess {
                loc: loc.clone(),
                kind: PPError::IoError(resolved.clone(), e.to_string()),
            }
        })?;

        let file_id = self.files.register(resolved);
        let input = InputSource::from_file(source, file_id);
        self.sources.push(input);

        Ok(())
    }

    /// インクルードパスを解決
    fn resolve_include(&self, path: &str, kind: IncludeKind, loc: &SourceLocation) -> Result<PathBuf, CompileError> {
        let path = Path::new(path);

        if kind == IncludeKind::Local {
            if let Some(source) = self.sources.last() {
                if !source.is_token_source() {
                    let current_path = self.files.get_path(source.file_id);
                    if let Some(parent) = current_path.parent() {
                        let candidate = parent.join(path);
                        if candidate.exists() {
                            return Ok(candidate);
                        }
                    }
                }
            }
        }

        for dir in &self.config.include_paths {
            let candidate = dir.join(path);
            if candidate.exists() {
                return Ok(candidate);
            }
        }

        Err(CompileError::Preprocess {
            loc: loc.clone(),
            kind: PPError::IncludeNotFound(path.to_path_buf()),
        })
    }

    /// #if を処理
    fn process_if(&mut self, loc: SourceLocation) -> Result<(), CompileError> {
        // 親が無効な場合は文字レベルでスキップ
        if !self.cond_active {
            self.cond_stack.push(CondState {
                active: false,
                seen_active: false,
                seen_else: false,
                loc: loc.clone(),
            });
            self.skip_false_branch(loc)?;
            return Ok(());
        }

        // マクロ展開付きでトークンを収集
        let tokens = self.collect_if_condition()?;

        let mut eval = PPExprEvaluator::new(&tokens, &self.interner, &self.macros, loc.clone());
        let active = eval.evaluate()? != 0;

        self.cond_stack.push(CondState {
            active,
            seen_active: active,
            seen_else: false,
            loc: loc.clone(),
        });

        self.update_cond_active();

        // 条件が偽の場合、TinyCC方式でスキップ
        if !active {
            self.skip_false_branch(loc)?;
        }

        Ok(())
    }

    /// 偽ブランチをスキップし、#else/#elif/#endif を処理
    fn skip_false_branch(&mut self, loc: SourceLocation) -> Result<(), CompileError> {
        loop {
            let directive = self.preprocess_skip()?;
            match directive.as_str() {
                "endif" => {
                    // #endif: スタックからポップして終了
                    self.cond_stack.pop();
                    self.update_cond_active();
                    return Ok(());
                }
                "else" => {
                    // #else: 今までどのブランチも有効でなければこのブランチを有効化
                    if let Some(state) = self.cond_stack.last_mut() {
                        if state.seen_else {
                            return Err(CompileError::Preprocess {
                                loc,
                                kind: PPError::UnmatchedElse,
                            });
                        }
                        state.seen_else = true;
                        if !state.seen_active {
                            state.active = true;
                            state.seen_active = true;
                            self.update_cond_active();
                            return Ok(());
                        }
                        // seen_active が true なら、このelseブランチも偽なので続けてスキップ
                    }
                }
                "elif" => {
                    // #elif: 条件を評価
                    if let Some(state) = self.cond_stack.last() {
                        if state.seen_else {
                            return Err(CompileError::Preprocess {
                                loc,
                                kind: PPError::ElifAfterElse,
                            });
                        }
                        if state.seen_active {
                            // 既に有効なブランチがあったので、この elif もスキップ
                            self.skip_to_eol()?;
                            continue;
                        }
                    }
                    // 条件を評価
                    let tokens = self.collect_if_condition()?;
                    let new_active = {
                        let mut eval = PPExprEvaluator::new(&tokens, &self.interner, &self.macros, loc.clone());
                        eval.evaluate()? != 0
                    };
                    if let Some(state) = self.cond_stack.last_mut() {
                        if new_active {
                            state.active = true;
                            state.seen_active = true;
                            self.update_cond_active();
                            return Ok(());
                        }
                        // 条件が偽なので続けてスキップ
                    }
                }
                _ => unreachable!(),
            }
        }
    }

    /// #ifdef / #ifndef を処理
    fn process_ifdef(&mut self, loc: SourceLocation, negate: bool) -> Result<(), CompileError> {
        // 親が無効な場合は文字レベルでスキップ
        if !self.cond_active {
            self.cond_stack.push(CondState {
                active: false,
                seen_active: false,
                seen_else: false,
                loc: loc.clone(),
            });
            self.skip_false_branch(loc)?;
            return Ok(());
        }

        let token = self.next_raw_token()?;
        let defined = if let TokenKind::Ident(id) = token.kind {
            self.macros.is_defined(id)
        } else {
            false
        };

        self.skip_to_eol()?;

        let active = if negate { !defined } else { defined };

        self.cond_stack.push(CondState {
            active,
            seen_active: active,
            seen_else: false,
            loc: loc.clone(),
        });

        self.update_cond_active();

        // 条件が偽の場合、TinyCC方式でスキップ
        if !active {
            self.skip_false_branch(loc)?;
        }

        Ok(())
    }

    /// #elif を処理
    /// 注: これは有効なブランチから呼ばれる（そのブランチは終了し、残りをスキップする必要がある）
    fn process_elif(&mut self, loc: SourceLocation) -> Result<(), CompileError> {
        if self.cond_stack.is_empty() {
            return Err(CompileError::Preprocess {
                loc,
                kind: PPError::UnmatchedEndif,
            });
        }

        let seen_else = self.cond_stack.last().unwrap().seen_else;
        if seen_else {
            return Err(CompileError::Preprocess {
                loc,
                kind: PPError::ElifAfterElse,
            });
        }

        // 有効なブランチを見た後なので、#endif までスキップ
        // (seen_active = true を維持したまま)
        self.skip_to_eol()?;
        self.skip_false_branch(loc)?;

        Ok(())
    }

    /// #else を処理
    /// 注: これは有効なブランチから呼ばれる（そのブランチは終了し、#else 以降をスキップする必要がある）
    fn process_else(&mut self, loc: SourceLocation) -> Result<(), CompileError> {
        if self.cond_stack.is_empty() {
            return Err(CompileError::Preprocess {
                loc,
                kind: PPError::UnmatchedElse,
            });
        }

        let seen_else = self.cond_stack.last().unwrap().seen_else;
        if seen_else {
            return Err(CompileError::Preprocess {
                loc,
                kind: PPError::UnmatchedElse,
            });
        }

        // seen_else をマーク
        if let Some(state) = self.cond_stack.last_mut() {
            state.seen_else = true;
        }

        // 有効なブランチを見た後なので、#endif までスキップ
        self.skip_to_eol()?;
        self.skip_false_branch(loc)?;

        Ok(())
    }

    /// #endif を処理
    fn process_endif(&mut self) -> Result<(), CompileError> {
        if self.cond_stack.is_empty() {
            return Err(CompileError::Preprocess {
                loc: SourceLocation::default(),
                kind: PPError::UnmatchedEndif,
            });
        }

        self.cond_stack.pop();
        self.skip_to_eol()?;
        self.update_cond_active();
        Ok(())
    }

    /// #error を処理
    fn process_error(&mut self, loc: SourceLocation) -> Result<(), CompileError> {
        let mut message = String::new();
        loop {
            let token = self.next_raw_token()?;
            match token.kind {
                TokenKind::Newline | TokenKind::Eof => break,
                TokenKind::Ident(id) => {
                    if !message.is_empty() { message.push(' '); }
                    message.push_str(self.interner.get(id));
                }
                TokenKind::StringLit(bytes) => {
                    if !message.is_empty() { message.push(' '); }
                    message.push_str(&String::from_utf8_lossy(&bytes));
                }
                _ => {
                    if !message.is_empty() { message.push(' '); }
                    message.push_str(&format!("{:?}", token.kind));
                }
            }
        }

        Err(CompileError::Preprocess {
            loc,
            kind: PPError::InvalidDirective(format!("#error {}", message)),
        })
    }

    /// 条件アクティブ状態を更新
    fn update_cond_active(&mut self) {
        self.cond_active = self.cond_stack.iter().all(|s| s.active);
    }

    /// 行末までトークンを収集（マクロ展開なし）
    fn collect_to_eol(&mut self) -> Result<Vec<Token>, CompileError> {
        let mut tokens = Vec::new();
        loop {
            let token = self.next_raw_token()?;
            match token.kind {
                TokenKind::Newline | TokenKind::Eof => break,
                _ => tokens.push(token),
            }
        }
        Ok(tokens)
    }

    /// #if条件用：マクロ展開付きでトークン収集
    /// TinyCC方式: マクロは展開するが、defined の引数は展開しない
    fn collect_if_condition(&mut self) -> Result<Vec<Token>, CompileError> {
        let mut tokens = Vec::new();
        let defined_id = self.interner.intern("defined");

        loop {
            // 生トークンを読む（先読みバッファから、またはソースから）
            let token = self.next_raw_token()?;

            match &token.kind {
                TokenKind::Newline | TokenKind::Eof => break,
                TokenKind::Ident(id) if *id == defined_id => {
                    // defined演算子の場合、引数は展開しない
                    tokens.push(token);

                    // 次のトークン（パーレンまたは識別子）を収集
                    let next = self.next_raw_token()?;
                    if matches!(next.kind, TokenKind::LParen) {
                        tokens.push(next);
                        // ( 内の識別子を収集（展開しない）
                        let ident = self.next_raw_token()?;
                        tokens.push(ident);
                        let rparen = self.next_raw_token()?;
                        tokens.push(rparen);
                    } else {
                        // defined IDENT 形式（parenthesisなし）
                        tokens.push(next);
                    }
                }
                TokenKind::Ident(id) => {
                    let id = *id;
                    // マクロ展開を試みる
                    if let Some(expanded) = self.try_expand_macro(id, &token)? {
                        // 展開されたトークンを先読みバッファに入れて再処理
                        for t in expanded.into_iter().rev() {
                            self.lookahead.push(t);
                        }
                    } else {
                        // 展開できなかった（未定義の識別子、または展開中のマクロ）
                        tokens.push(token);
                    }
                }
                _ => {
                    tokens.push(token);
                }
            }
        }

        Ok(tokens)
    }

    /// #include <...> のパスを文字レベルで読み取る（TinyCC方式）
    fn scan_include_path(&mut self, terminator: char) -> Result<String, CompileError> {
        let source = self.sources.last_mut().ok_or_else(|| {
            CompileError::Preprocess {
                loc: SourceLocation::default(),
                kind: PPError::InvalidDirective("no source".to_string()),
            }
        })?;

        let loc = source.current_location();
        let mut path = String::new();

        loop {
            match source.peek() {
                Some(c) if c == terminator as u8 => {
                    source.advance();
                    break;
                }
                Some(b'\n') | None => {
                    return Err(CompileError::Preprocess {
                        loc,
                        kind: PPError::InvalidDirective("unterminated include path".to_string()),
                    });
                }
                Some(c) => {
                    source.advance();
                    path.push(c as char);
                }
            }
        }

        Ok(path)
    }

    /// 行末までスキップ
    fn skip_to_eol(&mut self) -> Result<(), CompileError> {
        loop {
            let token = self.next_raw_token()?;
            if matches!(token.kind, TokenKind::Newline | TokenKind::Eof) {
                break;
            }
        }
        Ok(())
    }

    /// TinyCC方式: 条件が偽のブロックをスキップ
    /// トークナイザを使わず文字レベルでスキャンし、#else/#elif/#endif を見つけるまでスキップ
    /// 戻り値: 見つかったディレクティブ名 ("else", "elif", "endif")
    fn preprocess_skip(&mut self) -> Result<String, CompileError> {
        let mut depth = 0i32;  // #if のネスト深度

        loop {
            let source = match self.sources.last_mut() {
                Some(s) => s,
                None => {
                    return Err(CompileError::Preprocess {
                        loc: SourceLocation::default(),
                        kind: PPError::MissingEndif,
                    });
                }
            };

            // 行頭フラグをリセット
            let mut at_line_start = source.is_at_line_start();

            loop {
                let c = match source.peek() {
                    Some(c) => c,
                    None => break, // このソースは終了、外側ループで次のソースへ
                };

                match c {
                    // 空白はスキップ
                    b' ' | b'\t' | b'\r' | 0x0C | 0x0B => {
                        source.advance();
                    }
                    // 改行
                    b'\n' => {
                        source.advance();
                        at_line_start = true;
                    }
                    // 行継続
                    b'\\' => {
                        source.advance();
                        if source.peek() == Some(b'\n') {
                            source.advance();
                        } else if source.peek() == Some(b'\r') {
                            source.advance();
                            if source.peek() == Some(b'\n') {
                                source.advance();
                            }
                        }
                    }
                    // 文字列リテラル（スキップ）
                    b'"' | b'\'' => {
                        let quote = c;
                        source.advance();
                        loop {
                            match source.peek() {
                                Some(c) if c == quote => {
                                    source.advance();
                                    break;
                                }
                                Some(b'\\') => {
                                    source.advance();
                                    source.advance(); // エスケープ文字をスキップ
                                }
                                Some(b'\n') | None => break,
                                Some(_) => {
                                    source.advance();
                                }
                            }
                        }
                        at_line_start = false;
                    }
                    // コメント
                    b'/' => {
                        source.advance();
                        match source.peek() {
                            Some(b'/') => {
                                // 行コメント
                                while source.peek().is_some_and(|c| c != b'\n') {
                                    source.advance();
                                }
                            }
                            Some(b'*') => {
                                // ブロックコメント
                                source.advance();
                                loop {
                                    match (source.peek(), source.peek_n(1)) {
                                        (Some(b'*'), Some(b'/')) => {
                                            source.advance();
                                            source.advance();
                                            break;
                                        }
                                        (Some(_), _) => {
                                            source.advance();
                                        }
                                        (None, _) => break,
                                    }
                                }
                            }
                            _ => {}
                        }
                        at_line_start = false;
                    }
                    // プリプロセッサディレクティブ
                    b'#' if at_line_start => {
                        source.advance();
                        // 空白をスキップ
                        while matches!(source.peek(), Some(b' ') | Some(b'\t')) {
                            source.advance();
                        }
                        // ディレクティブ名を読む
                        let mut directive = String::new();
                        while let Some(c) = source.peek() {
                            if c.is_ascii_alphabetic() || c == b'_' {
                                directive.push(c as char);
                                source.advance();
                            } else {
                                break;
                            }
                        }

                        match directive.as_str() {
                            "if" | "ifdef" | "ifndef" => {
                                depth += 1;
                                // 行末までスキップ
                                while source.peek().is_some_and(|c| c != b'\n') {
                                    source.advance();
                                }
                            }
                            "endif" => {
                                if depth == 0 {
                                    // 行末までスキップしてから戻る
                                    while source.peek().is_some_and(|c| c != b'\n') {
                                        source.advance();
                                    }
                                    return Ok("endif".to_string());
                                }
                                depth -= 1;
                                while source.peek().is_some_and(|c| c != b'\n') {
                                    source.advance();
                                }
                            }
                            "else" if depth == 0 => {
                                while source.peek().is_some_and(|c| c != b'\n') {
                                    source.advance();
                                }
                                return Ok("else".to_string());
                            }
                            "elif" if depth == 0 => {
                                // elifの場合は条件式を読む必要があるので、行末までスキップせずに戻る
                                return Ok("elif".to_string());
                            }
                            _ => {
                                // その他のディレクティブは行末までスキップ
                                while source.peek().is_some_and(|c| c != b'\n') {
                                    source.advance();
                                }
                            }
                        }
                        at_line_start = false;
                    }
                    // その他の文字
                    _ => {
                        source.advance();
                        at_line_start = false;
                    }
                }
            }

            // このソースが終了したら次のソースへ
            if self.sources.len() > 1 {
                self.sources.pop();
            } else {
                return Err(CompileError::Preprocess {
                    loc: SourceLocation::default(),
                    kind: PPError::MissingEndif,
                });
            }
        }
    }

    /// マクロ展開を試みる
    fn try_expand_macro(&mut self, id: InternedStr, token: &Token) -> Result<Option<Vec<Token>>, CompileError> {
        if self.expanding.contains(&id) {
            return Ok(None);
        }

        let def = match self.macros.get(id) {
            Some(def) => def.clone(),
            None => return Ok(None),
        };

        match &def.kind {
            MacroKind::Object => {
                self.expanding.insert(id);
                let expanded = self.expand_tokens(&def.body, &HashMap::new())?;
                self.expanding.remove(&id);
                Ok(Some(expanded))
            }
            MacroKind::Function { params, is_variadic } => {
                let next = self.next_raw_token()?;
                if !matches!(next.kind, TokenKind::LParen) {
                    self.lookahead.push(next);
                    return Ok(None);
                }

                let args = self.collect_macro_args(params.len(), *is_variadic)?;

                let mut arg_map = HashMap::new();

                if *is_variadic && !params.is_empty() {
                    // GNU拡張: NAME... 形式の場合、最後のパラメータが可変長引数を受け取る
                    // 標準形式: ... のみの場合、params に可変長用パラメータは含まれない
                    let va_args_id = self.interner.intern("__VA_ARGS__");
                    let last_param = *params.last().unwrap();
                    let is_gnu_style = last_param != va_args_id;

                    // 通常のパラメータをマップ（最後のパラメータを除く場合がある）
                    let normal_param_count = if is_gnu_style { params.len() - 1 } else { params.len() };
                    for (i, param) in params.iter().take(normal_param_count).enumerate() {
                        if i < args.len() {
                            arg_map.insert(*param, args[i].clone());
                        } else {
                            arg_map.insert(*param, Vec::new());
                        }
                    }

                    // 可変長引数を構築
                    let mut va = Vec::new();
                    let va_start = normal_param_count;
                    for (i, arg) in args.iter().enumerate().skip(va_start) {
                        if i > va_start {
                            va.push(Token::new(TokenKind::Comma, token.loc.clone()));
                        }
                        va.extend(arg.clone());
                    }

                    if is_gnu_style {
                        // GNU拡張: 名前付きパラメータに格納
                        arg_map.insert(last_param, va.clone());
                        // __VA_ARGS__ もエイリアスとして登録（互換性のため）
                        arg_map.insert(va_args_id, va);
                    } else {
                        // 標準形式: __VA_ARGS__ に格納
                        arg_map.insert(va_args_id, va);
                    }
                } else {
                    // 非可変長マクロ
                    for (i, param) in params.iter().enumerate() {
                        if i < args.len() {
                            arg_map.insert(*param, args[i].clone());
                        } else {
                            arg_map.insert(*param, Vec::new());
                        }
                    }
                }

                self.expanding.insert(id);
                let expanded = self.expand_tokens(&def.body, &arg_map)?;
                self.expanding.remove(&id);
                Ok(Some(expanded))
            }
        }
    }

    /// マクロ引数を収集
    fn collect_macro_args(&mut self, param_count: usize, is_variadic: bool) -> Result<Vec<Vec<Token>>, CompileError> {
        let mut args = Vec::new();
        let mut current_arg = Vec::new();
        let mut paren_depth = 0;

        loop {
            let token = self.next_raw_token()?;
            match token.kind {
                TokenKind::LParen => {
                    paren_depth += 1;
                    current_arg.push(token);
                }
                TokenKind::RParen => {
                    if paren_depth == 0 {
                        if !current_arg.is_empty() || !args.is_empty() {
                            args.push(current_arg);
                        }
                        break;
                    }
                    paren_depth -= 1;
                    current_arg.push(token);
                }
                TokenKind::Comma if paren_depth == 0 => {
                    if is_variadic && args.len() >= param_count {
                        current_arg.push(token);
                    } else {
                        args.push(current_arg);
                        current_arg = Vec::new();
                    }
                }
                TokenKind::Eof => {
                    return Err(CompileError::Preprocess {
                        loc: token.loc,
                        kind: PPError::InvalidMacroArgs("unterminated macro arguments".to_string()),
                    });
                }
                TokenKind::Newline => continue,
                _ => current_arg.push(token),
            }
        }

        Ok(args)
    }

    /// トークン列を展開
    fn expand_tokens(&mut self, tokens: &[Token], args: &HashMap<InternedStr, Vec<Token>>) -> Result<Vec<Token>, CompileError> {
        let mut result = Vec::new();
        let mut i = 0;

        while i < tokens.len() {
            let token = &tokens[i];

            match &token.kind {
                TokenKind::Hash if i + 1 < tokens.len() => {
                    if let TokenKind::Ident(param_id) = tokens[i + 1].kind {
                        if let Some(arg_tokens) = args.get(&param_id) {
                            let stringified = self.stringify_tokens(arg_tokens);
                            result.push(Token::new(
                                TokenKind::StringLit(stringified.into_bytes()),
                                token.loc.clone(),
                            ));
                            i += 2;
                            continue;
                        }
                    }
                    return Err(CompileError::Preprocess {
                        loc: token.loc.clone(),
                        kind: PPError::InvalidStringize,
                    });
                }
                TokenKind::HashHash => {
                    if result.is_empty() || i + 1 >= tokens.len() {
                        return Err(CompileError::Preprocess {
                            loc: token.loc.clone(),
                            kind: PPError::InvalidTokenPaste,
                        });
                    }

                    // 左辺のトークンを取得
                    let left = result.pop().unwrap();

                    // 右辺のトークンを取得（パラメータの場合は展開）
                    i += 1;
                    let right_token = &tokens[i];
                    let right_tokens = if let TokenKind::Ident(id) = right_token.kind {
                        if let Some(arg_tokens) = args.get(&id) {
                            arg_tokens.clone()
                        } else {
                            vec![right_token.clone()]
                        }
                    } else {
                        vec![right_token.clone()]
                    };

                    // トークン連結を実行
                    let pasted = self.paste_tokens(&left, &right_tokens, &token.loc)?;
                    result.extend(pasted);
                    i += 1;
                    continue;
                }
                TokenKind::Ident(id) => {
                    if let Some(arg_tokens) = args.get(id) {
                        result.extend(arg_tokens.iter().cloned());
                    } else {
                        result.push(token.clone());
                    }
                }
                _ => result.push(token.clone()),
            }

            i += 1;
        }

        Ok(result)
    }

    /// トークン連結 (##)
    fn paste_tokens(&mut self, left: &Token, right: &[Token], loc: &SourceLocation) -> Result<Vec<Token>, CompileError> {
        // 左辺と右辺の文字列表現を取得
        let left_str = self.token_to_string(left);

        // 右辺が空の場合は左辺のみ返す
        if right.is_empty() {
            return Ok(vec![left.clone()]);
        }

        // 右辺の最初のトークンと連結
        let right_first_str = self.token_to_string(&right[0]);
        let pasted_str = format!("{}{}", left_str, right_first_str);

        // 連結結果を再トークン化
        let pasted_tokens = self.tokenize_string(&pasted_str);

        // 右辺の残りのトークンを追加
        let mut result = pasted_tokens;
        result.extend(right.iter().skip(1).cloned());

        // 位置情報を更新
        for t in &mut result {
            t.loc = loc.clone();
        }

        Ok(result)
    }

    /// トークンを文字列表現に変換
    fn token_to_string(&self, token: &Token) -> String {
        match &token.kind {
            TokenKind::Ident(id) => self.interner.get(*id).to_string(),
            TokenKind::IntLit(n) => n.to_string(),
            TokenKind::UIntLit(n) => n.to_string(),
            TokenKind::FloatLit(f) => f.to_string(),
            TokenKind::StringLit(s) => format!("\"{}\"", String::from_utf8_lossy(s)),
            TokenKind::CharLit(c) => format!("'{}'", *c as char),
            TokenKind::WideCharLit(c) => format!("L'{}'", char::from_u32(*c).unwrap_or('?')),
            TokenKind::Plus => "+".to_string(),
            TokenKind::Minus => "-".to_string(),
            TokenKind::Star => "*".to_string(),
            TokenKind::Slash => "/".to_string(),
            TokenKind::Percent => "%".to_string(),
            TokenKind::Amp => "&".to_string(),
            TokenKind::Pipe => "|".to_string(),
            TokenKind::Caret => "^".to_string(),
            TokenKind::Tilde => "~".to_string(),
            TokenKind::Bang => "!".to_string(),
            TokenKind::Lt => "<".to_string(),
            TokenKind::Gt => ">".to_string(),
            TokenKind::Eq => "=".to_string(),
            TokenKind::Question => "?".to_string(),
            TokenKind::Colon => ":".to_string(),
            TokenKind::Dot => ".".to_string(),
            TokenKind::Comma => ",".to_string(),
            TokenKind::Semi => ";".to_string(),
            TokenKind::LParen => "(".to_string(),
            TokenKind::RParen => ")".to_string(),
            TokenKind::LBracket => "[".to_string(),
            TokenKind::RBracket => "]".to_string(),
            TokenKind::LBrace => "{".to_string(),
            TokenKind::RBrace => "}".to_string(),
            TokenKind::Arrow => "->".to_string(),
            TokenKind::PlusPlus => "++".to_string(),
            TokenKind::MinusMinus => "--".to_string(),
            TokenKind::LtLt => "<<".to_string(),
            TokenKind::GtGt => ">>".to_string(),
            TokenKind::LtEq => "<=".to_string(),
            TokenKind::GtEq => ">=".to_string(),
            TokenKind::EqEq => "==".to_string(),
            TokenKind::BangEq => "!=".to_string(),
            TokenKind::AmpAmp => "&&".to_string(),
            TokenKind::PipePipe => "||".to_string(),
            TokenKind::PlusEq => "+=".to_string(),
            TokenKind::MinusEq => "-=".to_string(),
            TokenKind::StarEq => "*=".to_string(),
            TokenKind::SlashEq => "/=".to_string(),
            TokenKind::PercentEq => "%=".to_string(),
            TokenKind::AmpEq => "&=".to_string(),
            TokenKind::PipeEq => "|=".to_string(),
            TokenKind::CaretEq => "^=".to_string(),
            TokenKind::LtLtEq => "<<=".to_string(),
            TokenKind::GtGtEq => ">>=".to_string(),
            TokenKind::Ellipsis => "...".to_string(),
            TokenKind::Hash => "#".to_string(),
            TokenKind::HashHash => "##".to_string(),
            _ => String::new(),
        }
    }

    /// トークン列を文字列化
    fn stringify_tokens(&self, tokens: &[Token]) -> String {
        let mut result = String::new();
        for (i, token) in tokens.iter().enumerate() {
            if i > 0 { result.push(' '); }
            match &token.kind {
                TokenKind::Ident(id) => result.push_str(self.interner.get(*id)),
                TokenKind::IntLit(n) => result.push_str(&n.to_string()),
                TokenKind::UIntLit(n) => result.push_str(&format!("{}u", n)),
                TokenKind::FloatLit(f) => result.push_str(&f.to_string()),
                TokenKind::StringLit(s) => {
                    result.push('"');
                    result.push_str(&String::from_utf8_lossy(s));
                    result.push('"');
                }
                TokenKind::CharLit(c) => {
                    result.push('\'');
                    result.push(*c as char);
                    result.push('\'');
                }
                _ => result.push_str(&format!("{:?}", token.kind)),
            }
        }
        result
    }

    /// ファイルレジストリへの参照
    pub fn files(&self) -> &FileRegistry {
        &self.files
    }

    /// 文字列インターナーへの参照
    pub fn interner(&self) -> &StringInterner {
        &self.interner
    }

    /// 文字列インターナーへの可変参照
    pub fn interner_mut(&mut self) -> &mut StringInterner {
        &mut self.interner
    }

    /// マクロテーブルへの参照
    pub fn macros(&self) -> &MacroTable {
        &self.macros
    }

    /// 全トークンを収集
    pub fn collect_tokens(&mut self) -> Result<Vec<Token>, CompileError> {
        let mut tokens = Vec::new();
        loop {
            let token = self.next_token()?;
            if matches!(token.kind, TokenKind::Eof) {
                break;
            }
            tokens.push(token);
        }
        Ok(tokens)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn create_temp_file(content: &str) -> NamedTempFile {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(content.as_bytes()).unwrap();
        file
    }

    /// 識別子文字列がトークン列に含まれるかチェック
    fn has_ident(pp: &Preprocessor, tokens: &[Token], name: &str) -> bool {
        tokens.iter().any(|t| {
            if let TokenKind::Ident(id) = t.kind {
                pp.interner().get(id) == name
            } else {
                false
            }
        })
    }

    #[test]
    fn test_simple_tokens() {
        let file = create_temp_file("int x;");
        let mut pp = Preprocessor::new(PPConfig::default());
        pp.process_file(file.path()).unwrap();

        let tokens = pp.collect_tokens().unwrap();
        // int, x, ; の3トークン
        assert_eq!(tokens.len(), 3);
        // tinycc方式: キーワードも識別子として返される
        assert!(has_ident(&pp, &tokens, "int"));
        assert!(has_ident(&pp, &tokens, "x"));
    }

    #[test]
    fn test_object_macro() {
        let file = create_temp_file("#define VALUE 42\nint x = VALUE;");
        let mut pp = Preprocessor::new(PPConfig::default());
        pp.process_file(file.path()).unwrap();

        let tokens = pp.collect_tokens().unwrap();
        assert!(tokens.iter().any(|t| matches!(t.kind, TokenKind::IntLit(42))));
    }

    #[test]
    fn test_function_macro() {
        let file = create_temp_file("#define ADD(a, b) a + b\nint x = ADD(1, 2);");
        let mut pp = Preprocessor::new(PPConfig::default());
        pp.process_file(file.path()).unwrap();

        let tokens = pp.collect_tokens().unwrap();
        assert!(tokens.iter().any(|t| matches!(t.kind, TokenKind::Plus)));
    }

    #[test]
    fn test_ifdef() {
        let file = create_temp_file("#define FOO\n#ifdef FOO\nint x;\n#endif");
        let mut pp = Preprocessor::new(PPConfig::default());
        pp.process_file(file.path()).unwrap();

        let tokens = pp.collect_tokens().unwrap();
        assert!(has_ident(&pp, &tokens, "int"));
    }

    #[test]
    fn test_ifndef() {
        let file = create_temp_file("#ifndef BAR\nint x;\n#endif");
        let mut pp = Preprocessor::new(PPConfig::default());
        pp.process_file(file.path()).unwrap();

        let tokens = pp.collect_tokens().unwrap();
        assert!(has_ident(&pp, &tokens, "int"));
    }

    #[test]
    fn test_ifdef_else() {
        let file = create_temp_file("#ifdef UNDEFINED\nint x;\n#else\nfloat y;\n#endif");
        let mut pp = Preprocessor::new(PPConfig::default());
        pp.process_file(file.path()).unwrap();

        let tokens = pp.collect_tokens().unwrap();
        // UNDEFINED は定義されていないので、int x は出力されない
        assert!(!has_ident(&pp, &tokens, "x"));
        // float y は出力される
        assert!(has_ident(&pp, &tokens, "float"));
        assert!(has_ident(&pp, &tokens, "y"));
    }

    #[test]
    fn test_if_expression() {
        let file = create_temp_file("#if 1 + 1 == 2\nint x;\n#endif");
        let mut pp = Preprocessor::new(PPConfig::default());
        pp.process_file(file.path()).unwrap();

        let tokens = pp.collect_tokens().unwrap();
        assert!(has_ident(&pp, &tokens, "int"));
    }

    #[test]
    fn test_predefined_macro() {
        let config = PPConfig {
            predefined: vec![("VERSION".to_string(), Some("100".to_string()))],
            ..Default::default()
        };
        let file = create_temp_file("int v = VERSION;");
        let mut pp = Preprocessor::new(config);
        pp.process_file(file.path()).unwrap();

        let tokens = pp.collect_tokens().unwrap();
        assert!(tokens.iter().any(|t| matches!(t.kind, TokenKind::IntLit(100))));
    }

    #[test]
    fn test_undef() {
        let file = create_temp_file("#define FOO 1\n#undef FOO\n#ifdef FOO\nint x;\n#endif");
        let mut pp = Preprocessor::new(PPConfig::default());
        pp.process_file(file.path()).unwrap();

        let tokens = pp.collect_tokens().unwrap();
        // FOO は #undef されているので、int x は出力されない
        assert!(!has_ident(&pp, &tokens, "x"));
    }

    #[test]
    fn test_nested_ifdef() {
        let file = create_temp_file(
            "#define A\n#ifdef A\n#ifdef B\nint x;\n#else\nfloat y;\n#endif\n#endif"
        );
        let mut pp = Preprocessor::new(PPConfig::default());
        pp.process_file(file.path()).unwrap();

        let tokens = pp.collect_tokens().unwrap();
        // A は定義されているが B は定義されていないので、float y が出力される
        assert!(!has_ident(&pp, &tokens, "x"));
        assert!(has_ident(&pp, &tokens, "float"));
        assert!(has_ident(&pp, &tokens, "y"));
    }
}
