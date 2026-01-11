# マクロ関数の文（Statement）判定の改良

## 目標

マクロの body が `KwDo` または `KwIf` で始まる場合、文としてパースできるようにする。

## 背景

### 現状の問題

現在、`ParseResult::Statement` バリアントは定義されているが使われていない：
- `try_parse_tokens` は式としてのパースのみ試行
- 文として判別されているマクロは 0 件

### 対象パターン

```c
#define CopFILE_copy(dst,src)       \
    STMT_START {                    \
        CopFILE_copy_x((dst),(src));\
        CopFILE_debug((dst),"...",0);\
    } STMT_END
```

`STMT_START` → `do`, `STMT_END` → `while(0)` に展開されるため、
最終的には `do { ... } while(0)` となる。

### 問題点

`do { ... } while(0)` は文法上、末尾に `;` が必要：
```c
do { ... } while(0);  // ← この ; がマクロ body には含まれない
```

## 実装計画

### Step 1: Parser 構造体にフラグ追加

**src/parser.rs:**

```rust
pub struct Parser<'a, S: TokenSource> {
    // ... 既存フィールド ...
    /// do-while 文の末尾セミコロンを省略可能にするフラグ
    allow_missing_semi: bool,
}
```

### Step 2: parse_do_while_stmt を修正

**src/parser.rs:**

```rust
fn parse_do_while_stmt(&mut self) -> Result<Stmt> {
    // ... 既存のコード ...
    self.expect(&TokenKind::RParen)?;

    // allow_missing_semi が true の場合、; は任意
    if self.allow_missing_semi {
        if self.check(&TokenKind::Semi) {
            self.advance()?;
        }
        // ; がなくても OK
    } else {
        self.expect(&TokenKind::Semi)?;
    }

    Ok(Stmt::DoWhile { body, cond, loc })
}
```

### Step 3: 文パース用の公開メソッドを追加

**src/parser.rs:**

```rust
/// 文をパース（末尾セミコロン省略可能）
pub fn parse_stmt_allow_missing_semi(&mut self) -> Result<Stmt> {
    self.allow_missing_semi = true;
    let result = self.parse_stmt();
    self.allow_missing_semi = false;
    result
}
```

### Step 4: 文パース用の公開関数を追加

**src/parser.rs:**

```rust
/// トークン列を文としてパース（参照ベース版）
/// do-while の末尾セミコロンは省略可能
pub fn parse_statement_from_tokens_ref(
    tokens: Vec<Token>,
    interner: &StringInterner,
    files: &FileRegistry,
    typedefs: &HashSet<InternedStr>,
) -> Result<Stmt> {
    let mut source = TokenSliceRef::new(tokens, interner, files);
    let mut parser = Parser::from_source_with_typedefs(&mut source, typedefs.clone())?;
    parser.parse_stmt_allow_missing_semi()
}
```

### Step 5: macro_infer.rs の try_parse_tokens を拡張

**src/macro_infer.rs:**

```rust
fn try_parse_tokens(&self, tokens: &[Token], ...) -> ParseResult {
    if tokens.is_empty() {
        return ParseResult::Unparseable(Some("empty token sequence".to_string()));
    }

    // 1. 先頭トークンが KwDo または KwIf なら文としてパース試行
    if matches!(tokens[0].kind, TokenKind::KwDo | TokenKind::KwIf) {
        match parse_statement_from_tokens_ref(
            tokens.to_vec(), interner, files, typedefs
        ) {
            Ok(stmt) => return ParseResult::Statement(vec![BlockItem::Stmt(stmt)]),
            Err(_) => {} // フォールスルーして式としてパース
        }
    }

    // 2. 式としてパースを試行（既存のコード）
    match parse_expression_from_tokens_ref(tokens.to_vec(), interner, files, typedefs) {
        Ok(expr) => ParseResult::Expression(Box::new(expr)),
        Err(err) => ParseResult::Unparseable(Some(err.format_with_files(files))),
    }
}
```

## 修正対象ファイル

1. **src/parser.rs**
   - `Parser` 構造体に `allow_missing_semi: bool` フィールド追加
   - `new` / `from_source_with_typedefs` で初期化
   - `parse_do_while_stmt` で条件付き `;` チェック
   - `parse_stmt_allow_missing_semi` メソッド追加
   - `parse_statement_from_tokens_ref` 公開関数追加

2. **src/macro_infer.rs**
   - `parse_statement_from_tokens_ref` をインポート
   - `try_parse_tokens` で先頭トークンをチェックして文パースを試行

3. **src/lib.rs**
   - `parse_statement_from_tokens_ref` をエクスポート

## 期待される結果

```
CopFILE_copy: statement (N constraints, M uses)
  (do-while
    (compound ...)
    (int-lit 0))
```

`KwDo` で始まるマクロが `ParseResult::Statement` として認識される。

## 注意点

1. `KwIf` の場合は末尾 `;` は不要なので、`allow_missing_semi` の影響は `do-while` のみ
2. 文パースが失敗した場合は、従来通り式としてパースを試行（フォールバック）
3. `allow_missing_semi` フラグはパース完了後にリセットする
