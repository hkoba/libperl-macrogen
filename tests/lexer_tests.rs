//! Lexer integration tests

use std::path::PathBuf;
use tinycc_macro_bindgen::{FileRegistry, Lexer, StringInterner, Token, TokenKind};

/// Helper to tokenize a source string
fn tokenize(source: &[u8]) -> (Vec<Token>, StringInterner) {
    let mut files = FileRegistry::new();
    let file_id = files.register(PathBuf::from("test.c"));
    let mut interner = StringInterner::new();
    let mut lexer = Lexer::new(source, file_id, &mut interner);

    let mut tokens = Vec::new();
    loop {
        let token = lexer.next_token().unwrap();
        if matches!(token.kind, TokenKind::Eof) {
            break;
        }
        tokens.push(token);
    }
    (tokens, interner)
}

/// Helper to get identifier string from token
fn get_ident<'a>(token: &Token, interner: &'a StringInterner) -> Option<&'a str> {
    if let TokenKind::Ident(id) = token.kind {
        Some(interner.get(id))
    } else {
        None
    }
}

#[test]
fn test_simple_tokens() {
    let (tokens, _) = tokenize(b"+ - * / % = ;");
    assert_eq!(tokens.len(), 7);
    assert!(matches!(tokens[0].kind, TokenKind::Plus));
    assert!(matches!(tokens[1].kind, TokenKind::Minus));
    assert!(matches!(tokens[2].kind, TokenKind::Star));
    assert!(matches!(tokens[3].kind, TokenKind::Slash));
    assert!(matches!(tokens[4].kind, TokenKind::Percent));
    assert!(matches!(tokens[5].kind, TokenKind::Eq));
    assert!(matches!(tokens[6].kind, TokenKind::Semi));
}

#[test]
fn test_compound_operators() {
    let (tokens, _) = tokenize(b"++ -- << >> <= >= == != && || += -= *= /=");
    assert_eq!(tokens.len(), 14);
    assert!(matches!(tokens[0].kind, TokenKind::PlusPlus));
    assert!(matches!(tokens[1].kind, TokenKind::MinusMinus));
    assert!(matches!(tokens[2].kind, TokenKind::LtLt));
    assert!(matches!(tokens[3].kind, TokenKind::GtGt));
    assert!(matches!(tokens[4].kind, TokenKind::LtEq));
    assert!(matches!(tokens[5].kind, TokenKind::GtEq));
    assert!(matches!(tokens[6].kind, TokenKind::EqEq));
    assert!(matches!(tokens[7].kind, TokenKind::BangEq));
    assert!(matches!(tokens[8].kind, TokenKind::AmpAmp));
    assert!(matches!(tokens[9].kind, TokenKind::PipePipe));
    assert!(matches!(tokens[10].kind, TokenKind::PlusEq));
    assert!(matches!(tokens[11].kind, TokenKind::MinusEq));
    assert!(matches!(tokens[12].kind, TokenKind::StarEq));
    assert!(matches!(tokens[13].kind, TokenKind::SlashEq));
}

#[test]
fn test_brackets() {
    let (tokens, _) = tokenize(b"( ) [ ] { }");
    assert_eq!(tokens.len(), 6);
    assert!(matches!(tokens[0].kind, TokenKind::LParen));
    assert!(matches!(tokens[1].kind, TokenKind::RParen));
    assert!(matches!(tokens[2].kind, TokenKind::LBracket));
    assert!(matches!(tokens[3].kind, TokenKind::RBracket));
    assert!(matches!(tokens[4].kind, TokenKind::LBrace));
    assert!(matches!(tokens[5].kind, TokenKind::RBrace));
}

#[test]
fn test_integer_literals() {
    let (tokens, _) = tokenize(b"0 123 0x1F 0777 0b1010");
    assert_eq!(tokens.len(), 5);
    assert!(matches!(tokens[0].kind, TokenKind::IntLit(0)));
    assert!(matches!(tokens[1].kind, TokenKind::IntLit(123)));
    assert!(matches!(tokens[2].kind, TokenKind::IntLit(0x1F)));
    assert!(matches!(tokens[3].kind, TokenKind::IntLit(0o777)));
    assert!(matches!(tokens[4].kind, TokenKind::IntLit(0b1010)));
}

#[test]
fn test_integer_suffixes() {
    let (tokens, _) = tokenize(b"123L 456U 789UL 100LL 200ULL");
    assert_eq!(tokens.len(), 5);
    // Should be parsed as integer literals (signed or unsigned)
    for token in &tokens {
        assert!(matches!(token.kind, TokenKind::IntLit(_) | TokenKind::UIntLit(_)));
    }
}

#[test]
fn test_float_literals() {
    // Note: .5 may be tokenized as Dot + IntLit by this lexer
    let (tokens, _) = tokenize(b"1.0 3.14f 2.0L 1e10 1.5e-3");
    assert_eq!(tokens.len(), 5);
    for token in &tokens {
        assert!(matches!(token.kind, TokenKind::FloatLit(_)));
    }
}

#[test]
fn test_string_literals() {
    let (tokens, _) = tokenize(br#""hello" "world\n" "tab\t""#);
    assert_eq!(tokens.len(), 3);
    assert!(matches!(&tokens[0].kind, TokenKind::StringLit(s) if s == b"hello"));
    assert!(matches!(&tokens[1].kind, TokenKind::StringLit(s) if s == b"world\n"));
    assert!(matches!(&tokens[2].kind, TokenKind::StringLit(s) if s == b"tab\t"));
}

#[test]
fn test_char_literals() {
    let (tokens, _) = tokenize(b"'a' 'b' '\\n' '\\0'");
    assert_eq!(tokens.len(), 4);
    assert!(matches!(tokens[0].kind, TokenKind::CharLit(b'a')));
    assert!(matches!(tokens[1].kind, TokenKind::CharLit(b'b')));
    assert!(matches!(tokens[2].kind, TokenKind::CharLit(b'\n')));
    assert!(matches!(tokens[3].kind, TokenKind::CharLit(0)));
}

#[test]
fn test_identifiers() {
    let (tokens, interner) = tokenize(b"foo bar _baz __attr123");
    assert_eq!(tokens.len(), 4);
    assert_eq!(get_ident(&tokens[0], &interner), Some("foo"));
    assert_eq!(get_ident(&tokens[1], &interner), Some("bar"));
    assert_eq!(get_ident(&tokens[2], &interner), Some("_baz"));
    assert_eq!(get_ident(&tokens[3], &interner), Some("__attr123"));
}

#[test]
fn test_keywords() {
    // Keywords are returned as keyword tokens
    let (tokens, _interner) = tokenize(b"int void return if while for");
    assert_eq!(tokens.len(), 6);
    assert!(matches!(tokens[0].kind, TokenKind::KwInt));
    assert!(matches!(tokens[1].kind, TokenKind::KwVoid));
    assert!(matches!(tokens[2].kind, TokenKind::KwReturn));
    assert!(matches!(tokens[3].kind, TokenKind::KwIf));
    assert!(matches!(tokens[4].kind, TokenKind::KwWhile));
    assert!(matches!(tokens[5].kind, TokenKind::KwFor));
}

#[test]
fn test_gcc_extension_keywords() {
    let (tokens, _interner) = tokenize(b"inline __inline __inline__ __attribute__ typeof");
    assert_eq!(tokens.len(), 5);
    assert!(matches!(tokens[0].kind, TokenKind::KwInline));
    assert!(matches!(tokens[1].kind, TokenKind::KwInline2));
    assert!(matches!(tokens[2].kind, TokenKind::KwInline3));
    assert!(matches!(tokens[3].kind, TokenKind::KwAttribute2));
    assert!(matches!(tokens[4].kind, TokenKind::KwTypeof));
}

#[test]
fn test_line_comments() {
    let (tokens, _) = tokenize(b"// comment\nint");
    // Newline token with comment, then identifier
    assert!(tokens.len() >= 1);
    let newline = &tokens[0];
    assert!(matches!(newline.kind, TokenKind::Newline));
    assert_eq!(newline.leading_comments.len(), 1);
    assert!(newline.leading_comments[0].text.contains("comment"));
}

#[test]
fn test_block_comments() {
    let (tokens, _interner) = tokenize(b"/* block comment */ int");
    // Keyword with leading block comment
    assert!(tokens.len() >= 1);
    let kw = &tokens[0];
    assert!(matches!(kw.kind, TokenKind::KwInt));
    assert_eq!(kw.leading_comments.len(), 1);
    assert!(kw.leading_comments[0].text.contains("block comment"));
}

#[test]
fn test_ellipsis() {
    let (tokens, _) = tokenize(b"...");
    assert_eq!(tokens.len(), 1);
    assert!(matches!(tokens[0].kind, TokenKind::Ellipsis));
}

#[test]
fn test_arrow_and_dot() {
    let (tokens, _) = tokenize(b"-> .");
    assert_eq!(tokens.len(), 2);
    assert!(matches!(tokens[0].kind, TokenKind::Arrow));
    assert!(matches!(tokens[1].kind, TokenKind::Dot));
}

#[test]
fn test_source_location() {
    let (tokens, _) = tokenize(b"int\nmain");
    // First token should be at line 1
    assert_eq!(tokens[0].loc.line, 1);
    // After newline, next token should be at line 2
    // (tokens[1] is Newline, tokens[2] is "main")
    let main_token = tokens.iter().find(|t| matches!(t.kind, TokenKind::Ident(_)) && tokens.iter().position(|x| std::ptr::eq(x, *t)).unwrap() > 0);
    if let Some(t) = main_token {
        assert_eq!(t.loc.line, 2);
    }
}

#[test]
fn test_hex_escape_in_string() {
    let (tokens, _) = tokenize(br#""\x41\x42\x43""#);
    assert_eq!(tokens.len(), 1);
    assert!(matches!(&tokens[0].kind, TokenKind::StringLit(s) if s == b"ABC"));
}

#[test]
fn test_octal_escape_in_string() {
    let (tokens, _) = tokenize(br#""\101\102""#);
    assert_eq!(tokens.len(), 1);
    assert!(matches!(&tokens[0].kind, TokenKind::StringLit(s) if s == b"AB"));
}
