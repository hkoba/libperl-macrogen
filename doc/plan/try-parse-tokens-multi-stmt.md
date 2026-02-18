# Plan: `try_parse_tokens` でセミコロン区切りの複数文をパース

## 問題

`PAD_SET_CUR(padlist, nth)` の展開後トークン列:
```
SAVECOMPPAD ( ) ; PAD_SET_CUR_NOSAVE ( padlist , nth ) ;
```

`try_parse_tokens` は先頭トークンが `KwDo`/`KwIf` の場合のみ文パースを試行する。
先頭が `Ident` の場合は式としてパースするため、`SAVECOMPPAD()` だけがパースされ、
`;` 以降の `PAD_SET_CUR_NOSAVE(padlist, nth)` は捨てられる。

結果、パラメータ `padlist`/`nth` が式中に出現せず、型推論が働かない。

## 設計

### アプローチ: セミコロンの存在で文パースを試行

トークン列にセミコロンが含まれる場合、式パースの前に複数文パースを試行する。

### 判定ロジック

現在:
```
1. 先頭が KwDo/KwIf → 文パース試行 → 失敗なら式パース
2. それ以外 → 式パース
```

変更後:
```
1. 先頭が KwDo/KwIf → 文パース試行（1文）→ 失敗なら式パース
2. トークン列にセミコロンあり → 複数文パース試行 → 失敗なら式パース
3. それ以外 → 式パース
```

セミコロンの検出条件: トップレベル（括弧の外側）にセミコロンがあること。
`(a; b)` のようなケースは statement expression であり、式パースで処理される。

### 複数文パーサー

`parser.rs` に新しい公開関数を追加。`parse_compound_stmt` と同じ要領で
`{` `}` なしの `BlockItem` リストをパースする。終了条件は EOF。

```rust
pub fn parse_block_items_from_tokens_ref_with_stats(
    tokens: Vec<Token>,
    interner: &StringInterner,
    files: &FileRegistry,
    typedefs: &HashSet<InternedStr>,
) -> Result<(Vec<BlockItem>, ParseStats)>
```

```rust
pub fn parse_block_items_from_tokens_ref_with_generic_params(
    tokens: Vec<Token>,
    interner: &StringInterner,
    files: &FileRegistry,
    typedefs: &HashSet<InternedStr>,
    generic_params: HashMap<InternedStr, usize>,
) -> Result<(Vec<BlockItem>, ParseStats, HashSet<InternedStr>)>
```

内部実装:
```rust
let mut items = Vec::new();
while !parser.check(&TokenKind::Eof) {
    items.push(parser.parse_block_item()?);
}
```

`parse_block_item` は既存の private メソッド。`pub(crate)` に変更するか、
内部で同等のロジックを書く。

### `try_parse_tokens` の修正

```rust
fn try_parse_tokens(...) -> (...) {
    // 1. 先頭が KwDo/KwIf → 既存の文パース試行（変更なし）
    if is_statement_start { ... }

    // 2. トップレベルにセミコロンがあれば複数文パースを試行
    if has_toplevel_semicolon(&tokens) {
        match parse_block_items_from_tokens_ref_with_stats(...) {
            Ok((items, stats)) => {
                return (ParseResult::Statement(items), stats, HashSet::new());
            }
            Err(_) => {} // フォールスルーして式パース
        }
    }

    // 3. 式パース（既存、変更なし）
    ...
}
```

### `has_toplevel_semicolon` ヘルパー

```rust
/// トークン列のトップレベル（括弧の外側）にセミコロンがあるか判定
fn has_toplevel_semicolon(tokens: &[Token]) -> bool {
    let mut depth = 0;
    for t in tokens {
        match t.kind {
            TokenKind::LParen | TokenKind::LBrace | TokenKind::LBracket => depth += 1,
            TokenKind::RParen | TokenKind::RBrace | TokenKind::RBracket => {
                if depth > 0 { depth -= 1; }
            }
            TokenKind::Semi if depth == 0 => return true,
            _ => {}
        }
    }
    false
}
```

## 実装

### ファイル: `src/parser.rs`

#### 1. `parse_block_item` の可視性を `pub(crate)` に変更

```rust
// Before
fn parse_block_item(&mut self) -> Result<BlockItem> {

// After
pub(crate) fn parse_block_item(&mut self) -> Result<BlockItem> {
```

#### 2. 複数文パース用の公開関数を追加

`parse_statement_from_tokens_ref_with_stats` の直後に配置:

```rust
pub fn parse_block_items_from_tokens_ref_with_stats(
    tokens: Vec<Token>,
    interner: &StringInterner,
    files: &FileRegistry,
    typedefs: &HashSet<InternedStr>,
) -> Result<(Vec<BlockItem>, ParseStats)> {
    let mut source = TokenSliceRef::new(tokens, interner, files);
    let mut parser = Parser::from_source_with_typedefs(&mut source, typedefs.clone())?;
    parser.allow_missing_semi = true;
    let mut items = Vec::new();
    while !parser.check(&TokenKind::Eof) {
        items.push(parser.parse_block_item()?);
    }
    let stats = ParseStats {
        function_call_count: parser.function_call_count,
        deref_count: parser.deref_count,
    };
    Ok((items, stats))
}

pub fn parse_block_items_from_tokens_ref_with_generic_params(
    tokens: Vec<Token>,
    interner: &StringInterner,
    files: &FileRegistry,
    typedefs: &HashSet<InternedStr>,
    generic_params: HashMap<InternedStr, usize>,
) -> Result<(Vec<BlockItem>, ParseStats, HashSet<InternedStr>)> {
    let mut source = TokenSliceRef::new(tokens, interner, files);
    let mut parser = Parser::from_source_with_typedefs(&mut source, typedefs.clone())?;
    parser.allow_missing_semi = true;
    parser.generic_params = generic_params;
    let mut items = Vec::new();
    while !parser.check(&TokenKind::Eof) {
        items.push(parser.parse_block_item()?);
    }
    let stats = ParseStats {
        function_call_count: parser.function_call_count,
        deref_count: parser.deref_count,
    };
    let detected = parser.detected_type_params;
    Ok((items, stats, detected))
}
```

### ファイル: `src/macro_infer.rs`

#### 3. `has_toplevel_semicolon` ヘルパーを追加

#### 4. `try_parse_tokens` の修正

既存の `is_statement_start` ブロックの後、式パースの前に
複数文パース試行を追加。

## 変更ファイル一覧

| ファイル | 変更内容 |
|----------|----------|
| `src/parser.rs` | `parse_block_item` を `pub(crate)` に、複数文パース関数2つ追加 |
| `src/macro_infer.rs` | `has_toplevel_semicolon` 追加、`try_parse_tokens` に複数文パース試行追加 |

## 検証

1. `cargo build` / `cargo test`

2. 出力確認:
   ```bash
   cargo run -- --auto --gen-rust samples/xs-wrapper.h --bindings samples/bindings.rs 2>/dev/null \
     | grep -B 1 -A 8 'fn PAD_SET_CUR\b'
   ```
   - `padlist: PADLIST, nth: I32` と型が付くこと
   - 関数本体に `SAVECOMPPAD` と `PAD_SET_CUR_NOSAVE` の両方が含まれること

3. 回帰テスト: `cargo test rust_codegen_regression`

## エッジケース

1. **セミコロンが括弧内にのみ存在**: `fn((a; b))` → `has_toplevel_semicolon` = false
   → 式パースされる（正しい）

2. **複数文パースに失敗**: フォールスルーして式パース → 既存の動作を維持

3. **末尾セミコロンのみ**: `expr;` → 1つの式文としてパース → `Statement([Stmt::Expr(expr)])`
   これは既存の `Expression(expr)` と意味的に同等。
   ただし `ParseResult::Statement` の場合、`get_return_type` が `"()"` を返す。
   末尾セミコロン1つだけの場合は式パースを優先するべきか？
   → `has_toplevel_semicolon` は最後のセミコロンの後に有効なトークンがある場合のみ
   true を返すようにすると安全。
   **ただし、`PAD_SET_CUR` の例では末尾にもセミコロンがあるため、
   この条件だと `SAVECOMPPAD(); PAD_SET_CUR_NOSAVE(padlist,nth);` で
   最後のセミコロンの後は EOF → 検出される。**
   実際には `expr;` 単独の場合も `Statement` としてパースされるが、
   その場合は既存の式パースでも成功するため、先に式パースを試行しても同じ結果になる。
   → フォールスルーにより問題なし。

4. **KwDo/KwIf で始まる複数文**: 既存のパスで1文としてパースされる。
   1文パースが成功すれば return されるため、複数文パスには到達しない。
   1文パースが失敗した場合のみフォールスルーで複数文パスに到達する。
   → 既存の動作を壊さない。
