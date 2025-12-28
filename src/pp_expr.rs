//! プリプロセッサ条件式の評価
//!
//! #if / #elif ディレクティブの条件式を評価する。

use crate::error::{CompileError, PPError};
use crate::intern::{InternedStr, StringInterner};
use crate::macro_def::MacroTable;
use crate::source::SourceLocation;
use crate::token::{Token, TokenKind};

/// プリプロセッサ式評価器
pub struct PPExprEvaluator<'a> {
    tokens: &'a [Token],
    pos: usize,
    interner: &'a StringInterner,
    macros: &'a MacroTable,
    loc: SourceLocation,
    /// "defined" キーワードのインターン済み文字列
    defined_id: Option<InternedStr>,
}

impl<'a> PPExprEvaluator<'a> {
    /// 新しい評価器を作成
    pub fn new(
        tokens: &'a [Token],
        interner: &'a StringInterner,
        macros: &'a MacroTable,
        loc: SourceLocation,
    ) -> Self {
        // "defined" を検索
        let defined_id = interner.lookup("defined");

        Self {
            tokens,
            pos: 0,
            interner,
            macros,
            loc,
            defined_id,
        }
    }

    /// 条件式を評価
    pub fn evaluate(&mut self) -> Result<i64, CompileError> {
        let result = self.expr()?;
        Ok(result)
    }

    /// 現在のトークンを取得
    fn current(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    /// 現在のトークン種別を取得
    fn current_kind(&self) -> Option<&TokenKind> {
        self.current().map(|t| &t.kind)
    }

    /// 次へ進む
    fn advance(&mut self) {
        if self.pos < self.tokens.len() {
            self.pos += 1;
        }
    }

    /// エラーを生成
    fn error(&self, msg: &str) -> CompileError {
        CompileError::Preprocess {
            loc: self.loc.clone(),
            kind: PPError::InvalidCondition(msg.to_string()),
        }
    }

    /// 条件式 (ternary)
    fn expr(&mut self) -> Result<i64, CompileError> {
        let cond = self.logical_or()?;

        if matches!(self.current_kind(), Some(TokenKind::Question)) {
            self.advance();
            let then_val = self.expr()?;
            if !matches!(self.current_kind(), Some(TokenKind::Colon)) {
                return Err(self.error("expected ':' in ternary expression"));
            }
            self.advance();
            let else_val = self.expr()?;
            Ok(if cond != 0 { then_val } else { else_val })
        } else {
            Ok(cond)
        }
    }

    /// 論理OR
    fn logical_or(&mut self) -> Result<i64, CompileError> {
        let mut left = self.logical_and()?;

        while matches!(self.current_kind(), Some(TokenKind::PipePipe)) {
            self.advance();
            let right = self.logical_and()?;
            left = if left != 0 || right != 0 { 1 } else { 0 };
        }

        Ok(left)
    }

    /// 論理AND
    fn logical_and(&mut self) -> Result<i64, CompileError> {
        let mut left = self.bitwise_or()?;

        while matches!(self.current_kind(), Some(TokenKind::AmpAmp)) {
            self.advance();
            let right = self.bitwise_or()?;
            left = if left != 0 && right != 0 { 1 } else { 0 };
        }

        Ok(left)
    }

    /// ビットOR
    fn bitwise_or(&mut self) -> Result<i64, CompileError> {
        let mut left = self.bitwise_xor()?;

        while matches!(self.current_kind(), Some(TokenKind::Pipe)) {
            self.advance();
            let right = self.bitwise_xor()?;
            left |= right;
        }

        Ok(left)
    }

    /// ビットXOR
    fn bitwise_xor(&mut self) -> Result<i64, CompileError> {
        let mut left = self.bitwise_and()?;

        while matches!(self.current_kind(), Some(TokenKind::Caret)) {
            self.advance();
            let right = self.bitwise_and()?;
            left ^= right;
        }

        Ok(left)
    }

    /// ビットAND
    fn bitwise_and(&mut self) -> Result<i64, CompileError> {
        let mut left = self.equality()?;

        while matches!(self.current_kind(), Some(TokenKind::Amp)) {
            self.advance();
            let right = self.equality()?;
            left &= right;
        }

        Ok(left)
    }

    /// 等価比較
    fn equality(&mut self) -> Result<i64, CompileError> {
        let mut left = self.relational()?;

        loop {
            match self.current_kind() {
                Some(TokenKind::EqEq) => {
                    self.advance();
                    let right = self.relational()?;
                    left = if left == right { 1 } else { 0 };
                }
                Some(TokenKind::BangEq) => {
                    self.advance();
                    let right = self.relational()?;
                    left = if left != right { 1 } else { 0 };
                }
                _ => break,
            }
        }

        Ok(left)
    }

    /// 関係比較
    fn relational(&mut self) -> Result<i64, CompileError> {
        let mut left = self.shift()?;

        loop {
            match self.current_kind() {
                Some(TokenKind::Lt) => {
                    self.advance();
                    let right = self.shift()?;
                    left = if left < right { 1 } else { 0 };
                }
                Some(TokenKind::Gt) => {
                    self.advance();
                    let right = self.shift()?;
                    left = if left > right { 1 } else { 0 };
                }
                Some(TokenKind::LtEq) => {
                    self.advance();
                    let right = self.shift()?;
                    left = if left <= right { 1 } else { 0 };
                }
                Some(TokenKind::GtEq) => {
                    self.advance();
                    let right = self.shift()?;
                    left = if left >= right { 1 } else { 0 };
                }
                _ => break,
            }
        }

        Ok(left)
    }

    /// シフト演算
    fn shift(&mut self) -> Result<i64, CompileError> {
        let mut left = self.additive()?;

        loop {
            match self.current_kind() {
                Some(TokenKind::LtLt) => {
                    self.advance();
                    let right = self.additive()?;
                    left <<= right;
                }
                Some(TokenKind::GtGt) => {
                    self.advance();
                    let right = self.additive()?;
                    left >>= right;
                }
                _ => break,
            }
        }

        Ok(left)
    }

    /// 加減算
    fn additive(&mut self) -> Result<i64, CompileError> {
        let mut left = self.multiplicative()?;

        loop {
            match self.current_kind() {
                Some(TokenKind::Plus) => {
                    self.advance();
                    let right = self.multiplicative()?;
                    left = left.wrapping_add(right);
                }
                Some(TokenKind::Minus) => {
                    self.advance();
                    let right = self.multiplicative()?;
                    left = left.wrapping_sub(right);
                }
                _ => break,
            }
        }

        Ok(left)
    }

    /// 乗除算
    fn multiplicative(&mut self) -> Result<i64, CompileError> {
        let mut left = self.unary()?;

        loop {
            match self.current_kind() {
                Some(TokenKind::Star) => {
                    self.advance();
                    let right = self.unary()?;
                    left = left.wrapping_mul(right);
                }
                Some(TokenKind::Slash) => {
                    self.advance();
                    let right = self.unary()?;
                    if right == 0 {
                        return Err(self.error("division by zero"));
                    }
                    left /= right;
                }
                Some(TokenKind::Percent) => {
                    self.advance();
                    let right = self.unary()?;
                    if right == 0 {
                        return Err(self.error("modulo by zero"));
                    }
                    left %= right;
                }
                _ => break,
            }
        }

        Ok(left)
    }

    /// 単項演算
    fn unary(&mut self) -> Result<i64, CompileError> {
        match self.current_kind() {
            Some(TokenKind::Plus) => {
                self.advance();
                self.unary()
            }
            Some(TokenKind::Minus) => {
                self.advance();
                Ok(-self.unary()?)
            }
            Some(TokenKind::Bang) => {
                self.advance();
                let val = self.unary()?;
                Ok(if val == 0 { 1 } else { 0 })
            }
            Some(TokenKind::Tilde) => {
                self.advance();
                Ok(!self.unary()?)
            }
            _ => self.primary(),
        }
    }

    /// 一次式
    fn primary(&mut self) -> Result<i64, CompileError> {
        match self.current_kind().cloned() {
            Some(TokenKind::IntLit(n)) => {
                self.advance();
                Ok(n)
            }
            Some(TokenKind::UIntLit(n)) => {
                self.advance();
                Ok(n as i64)
            }
            Some(TokenKind::CharLit(c)) => {
                self.advance();
                Ok(c as i64)
            }
            Some(TokenKind::WideCharLit(c)) => {
                self.advance();
                Ok(c as i64)
            }
            Some(TokenKind::LParen) => {
                self.advance();
                let val = self.expr()?;
                if !matches!(self.current_kind(), Some(TokenKind::RParen)) {
                    return Err(self.error("expected ')'"));
                }
                self.advance();
                Ok(val)
            }
            Some(TokenKind::Ident(id)) => {
                // defined演算子のチェック
                if Some(id) == self.defined_id {
                    self.advance();
                    return self.parse_defined();
                }

                // 未定義の識別子は0として扱う（C標準）
                self.advance();
                Ok(0)
            }
            Some(_) => Err(self.error("unexpected token in preprocessor expression")),
            None => Err(self.error("unexpected end of expression")),
        }
    }

    /// defined演算子をパース
    fn parse_defined(&mut self) -> Result<i64, CompileError> {
        let has_paren = matches!(self.current_kind(), Some(TokenKind::LParen));
        if has_paren {
            self.advance();
        }

        // 識別子またはキーワードを受け入れる
        // キーワードも #define で定義される可能性があるため
        let name = match self.current_kind() {
            Some(TokenKind::Ident(id)) => Some(*id),
            Some(kind) if kind.is_keyword() => {
                // キーワードの名前を取得して検索
                // インターンされていなければマクロとして定義されていない
                let kw_name = kind.format(self.interner);
                self.interner.lookup(&kw_name)
            }
            _ => return Err(self.error("expected identifier after 'defined'")),
        };
        self.advance();

        if has_paren {
            if !matches!(self.current_kind(), Some(TokenKind::RParen)) {
                return Err(self.error("expected ')' after identifier in 'defined'"));
            }
            self.advance();
        }

        // nameがNoneの場合、マクロは定義されていない
        Ok(match name {
            Some(n) if self.macros.is_defined(n) => 1,
            _ => 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::macro_def::MacroDef;
    use crate::source::FileId;

    fn make_token(kind: TokenKind) -> Token {
        Token::new(kind, SourceLocation::default())
    }

    fn eval_tokens(tokens: &[Token], interner: &StringInterner, macros: &MacroTable) -> i64 {
        let loc = SourceLocation::new(FileId::default(), 1, 1);
        let mut eval = PPExprEvaluator::new(tokens, interner, macros, loc);
        eval.evaluate().unwrap()
    }

    #[test]
    fn test_simple_number() {
        let interner = StringInterner::new();
        let macros = MacroTable::new();
        let tokens = vec![make_token(TokenKind::IntLit(42))];

        assert_eq!(eval_tokens(&tokens, &interner, &macros), 42);
    }

    #[test]
    fn test_arithmetic() {
        let interner = StringInterner::new();
        let macros = MacroTable::new();

        // 2 + 3
        let tokens = vec![
            make_token(TokenKind::IntLit(2)),
            make_token(TokenKind::Plus),
            make_token(TokenKind::IntLit(3)),
        ];
        assert_eq!(eval_tokens(&tokens, &interner, &macros), 5);

        // 10 - 4 * 2
        let tokens = vec![
            make_token(TokenKind::IntLit(10)),
            make_token(TokenKind::Minus),
            make_token(TokenKind::IntLit(4)),
            make_token(TokenKind::Star),
            make_token(TokenKind::IntLit(2)),
        ];
        assert_eq!(eval_tokens(&tokens, &interner, &macros), 2);
    }

    #[test]
    fn test_comparison() {
        let interner = StringInterner::new();
        let macros = MacroTable::new();

        // 5 > 3
        let tokens = vec![
            make_token(TokenKind::IntLit(5)),
            make_token(TokenKind::Gt),
            make_token(TokenKind::IntLit(3)),
        ];
        assert_eq!(eval_tokens(&tokens, &interner, &macros), 1);

        // 2 == 3
        let tokens = vec![
            make_token(TokenKind::IntLit(2)),
            make_token(TokenKind::EqEq),
            make_token(TokenKind::IntLit(3)),
        ];
        assert_eq!(eval_tokens(&tokens, &interner, &macros), 0);
    }

    #[test]
    fn test_logical() {
        let interner = StringInterner::new();
        let macros = MacroTable::new();

        // 1 && 0
        let tokens = vec![
            make_token(TokenKind::IntLit(1)),
            make_token(TokenKind::AmpAmp),
            make_token(TokenKind::IntLit(0)),
        ];
        assert_eq!(eval_tokens(&tokens, &interner, &macros), 0);

        // 1 || 0
        let tokens = vec![
            make_token(TokenKind::IntLit(1)),
            make_token(TokenKind::PipePipe),
            make_token(TokenKind::IntLit(0)),
        ];
        assert_eq!(eval_tokens(&tokens, &interner, &macros), 1);
    }

    #[test]
    fn test_ternary() {
        let interner = StringInterner::new();
        let macros = MacroTable::new();

        // 1 ? 10 : 20
        let tokens = vec![
            make_token(TokenKind::IntLit(1)),
            make_token(TokenKind::Question),
            make_token(TokenKind::IntLit(10)),
            make_token(TokenKind::Colon),
            make_token(TokenKind::IntLit(20)),
        ];
        assert_eq!(eval_tokens(&tokens, &interner, &macros), 10);

        // 0 ? 10 : 20
        let tokens = vec![
            make_token(TokenKind::IntLit(0)),
            make_token(TokenKind::Question),
            make_token(TokenKind::IntLit(10)),
            make_token(TokenKind::Colon),
            make_token(TokenKind::IntLit(20)),
        ];
        assert_eq!(eval_tokens(&tokens, &interner, &macros), 20);
    }

    #[test]
    fn test_defined() {
        let mut interner = StringInterner::new();
        let mut macros = MacroTable::new();

        let foo = interner.intern("FOO");
        let defined = interner.intern("defined");
        let _ = defined; // 登録だけ

        // FOO を定義
        macros.define(MacroDef::object(foo, vec![], SourceLocation::default()), &interner);

        // defined(FOO)
        let tokens = vec![
            make_token(TokenKind::Ident(interner.lookup("defined").unwrap())),
            make_token(TokenKind::LParen),
            make_token(TokenKind::Ident(foo)),
            make_token(TokenKind::RParen),
        ];
        assert_eq!(eval_tokens(&tokens, &interner, &macros), 1);

        // defined(BAR) - 未定義
        let bar = interner.intern("BAR");
        let tokens = vec![
            make_token(TokenKind::Ident(interner.lookup("defined").unwrap())),
            make_token(TokenKind::LParen),
            make_token(TokenKind::Ident(bar)),
            make_token(TokenKind::RParen),
        ];
        assert_eq!(eval_tokens(&tokens, &interner, &macros), 0);
    }

    #[test]
    fn test_unary() {
        let interner = StringInterner::new();
        let macros = MacroTable::new();

        // -5
        let tokens = vec![
            make_token(TokenKind::Minus),
            make_token(TokenKind::IntLit(5)),
        ];
        assert_eq!(eval_tokens(&tokens, &interner, &macros), -5);

        // !0
        let tokens = vec![
            make_token(TokenKind::Bang),
            make_token(TokenKind::IntLit(0)),
        ];
        assert_eq!(eval_tokens(&tokens, &interner, &macros), 1);

        // !1
        let tokens = vec![
            make_token(TokenKind::Bang),
            make_token(TokenKind::IntLit(1)),
        ];
        assert_eq!(eval_tokens(&tokens, &interner, &macros), 0);
    }

    #[test]
    fn test_parentheses() {
        let interner = StringInterner::new();
        let macros = MacroTable::new();

        // (2 + 3) * 4
        let tokens = vec![
            make_token(TokenKind::LParen),
            make_token(TokenKind::IntLit(2)),
            make_token(TokenKind::Plus),
            make_token(TokenKind::IntLit(3)),
            make_token(TokenKind::RParen),
            make_token(TokenKind::Star),
            make_token(TokenKind::IntLit(4)),
        ];
        assert_eq!(eval_tokens(&tokens, &interner, &macros), 20);
    }
}
