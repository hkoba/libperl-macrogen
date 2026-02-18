# Plan: `assert_(cond) expr` パターンのパース対応

## 問題

`SvOK_off(sv)` が PARSE_FAILED になる。

### マクロ定義チェーン

```c
// perl.h
#define assert_(what)   assert(what),    // DEBUGGING 有効時
#define assert_(what)                     // DEBUGGING 無効時

// sv.h
#define assert_not_ROK(sv)  assert_(!SvROK(sv) || !SvRV(sv))
#define assert_not_glob(sv) assert_(!isGV_with_GP(sv))

#define SvOK_off(sv)  (assert_not_ROK(sv) assert_not_glob(sv) \
                        SvFLAGS(sv) &= ~(SVf_OK|SVf_IVisUV|SVf_UTF8), \
                        SvOOK_off(sv))
```

### 展開後のトークン列

`assert_not_ROK` と `assert_not_glob` は通常マクロとして展開される。
`assert_` は `NoExpandSymbols` のためトークンとして残る。結果:

```
( assert_ ( cond1 ) assert_ ( cond2 ) SvFLAGS(sv) &= ~(...) , SvOOK_off(sv) )
```

### パースエラーの原因

パーサーが `assert_(cond1)` を関数呼び出し式としてパースした後、
`)` または二項演算子を期待するが、次のトークンは `assert_`（Ident）。
→ `expected RParen, found Ident`

## 背景: `assert_` の C での役割

`assert_(what)` は DEBUGGING 有効時に `assert(what),`（末尾カンマ付き）に展開される。
つまり `assert_(cond) expr` は実質的に `assert(cond), expr` = カンマ式。

C では式の中に文を書けないため、`assert` を式の「前置修飾子」として使う苦肉の策。
Rust では `{ assert!(cond); expr }` とブロック式で自然に表現できる。

## 同様のパターン

`__ASSERT_` も同じ構造:
```c
// utf8.h
#define NATIVE_TO_LATIN1(ch)  (__ASSERT_(FITS_IN_8_BITS(ch)) ((U8) (ch)))
```

`__ASSERT_` は `ExplicitExpandSymbols` に登録されているため展開されるが、
展開結果は `assert_(cond)` であり、同じ問題が発生しうる。

## 設計

### アプローチ: 展開済みトークンへのカンマ注入

`expand_macro_body_for_inference` で展開された後、パーサーに渡す前に、
`assert_(...)` パターンの直後にカンマトークンを注入する。

```
// Before (パースエラー)
( assert_(cond1) assert_(cond2) SvFLAGS(sv) &= ~(...) , SvOOK_off(sv) )

// After (有効なカンマ式)
( assert_(cond1) , assert_(cond2) , SvFLAGS(sv) &= ~(...) , SvOOK_off(sv) )
```

これにより:
1. パーサーは通常のカンマ式として正常にパースする
2. 既存の `convert_assert_calls` が `Call("assert_", [cond])` → `Assert(AssertUnderscore, cond)` に変換
3. 最終 AST: `Comma(Assert(c1), Comma(Assert(c2), Comma(BinOp(&=, ...), Call(SvOOK_off, ...))))`

### Rust コード生成

既存の codegen が正しく処理する:
- `Comma { lhs, rhs }` → `{ lhs; rhs }`
- `Assert { kind: AssertUnderscore, condition }` → `{ assert!(cond); }`

結果:
```rust
{ { assert!(!SvROK(sv) || !SvRV(sv)); }; { { assert!(!isGV_with_GP(sv)); }; { SvFLAGS(sv) &= !(SVf_OK | SVf_IVisUV | SVf_UTF8); SvOOK_off(sv) } } }
```

冗長だが意味的に正しい。将来的にネストしたブロック式の平坦化で改善可能。

### なぜこのアプローチか

1. **C の意味論に忠実**: `assert_(cond)` は `assert(cond),` と定義されている。
   カンマを注入するのはその意味論を再現している。
2. **既存の仕組みを最大限活用**: `convert_assert_calls` + `Assert` AST + `Comma` codegen
   がすべてそのまま使える。
3. **変更箇所が最小**: トークン前処理の追加のみ。パーサーと codegen は変更不要。

## 実装

### ファイル: `src/macro_infer.rs`

#### カンマ注入関数の追加

```rust
/// assert_ 呼び出しの後にカンマトークンを注入
///
/// `assert_(cond) expr` パターンを `assert_(cond), expr` に変換する。
/// C の `assert_(what)` は DEBUGGING 時に `assert(what),` に展開されるため、
/// この変換は元の意味論を再現する。
fn inject_comma_after_assert_underscore(
    tokens: &[Token],
    no_expand: &NoExpandSymbols,
) -> Vec<Token> {
    // assert_ の名前を取得
    let assert_underscore = no_expand.assert_;

    let mut result = Vec::with_capacity(tokens.len());
    let mut i = 0;

    while i < tokens.len() {
        // assert_ ( ... ) パターンを検出
        if matches!(tokens[i].kind, TokenKind::Ident(name) if name == assert_underscore) {
            // assert_ トークンを追加
            result.push(tokens[i].clone());
            i += 1;

            // ( ... ) を括弧の深さを追跡しながらコピー
            // 空白をスキップ
            while i < tokens.len() && matches!(tokens[i].kind, TokenKind::Space | TokenKind::Newline) {
                result.push(tokens[i].clone());
                i += 1;
            }

            if i < tokens.len() && matches!(tokens[i].kind, TokenKind::LParen) {
                let mut depth = 0;
                while i < tokens.len() {
                    result.push(tokens[i].clone());
                    match tokens[i].kind {
                        TokenKind::LParen => depth += 1,
                        TokenKind::RParen => {
                            depth -= 1;
                            if depth == 0 {
                                i += 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                    i += 1;
                }

                // RParen の後にカンマを注入（後続トークンがある場合のみ）
                // ただし、既にカンマがある場合や、閉じ括弧の場合はスキップ
                let next_significant = tokens[i..].iter()
                    .find(|t| !matches!(t.kind, TokenKind::Space | TokenKind::Newline));
                let needs_comma = next_significant
                    .is_some_and(|t| !matches!(t.kind, TokenKind::Comma | TokenKind::RParen));
                if needs_comma {
                    // カンマトークンを生成（位置は直前のトークンと同じ）
                    let loc = result.last().map(|t| t.loc.clone())
                        .unwrap_or_default();
                    result.push(Token { kind: TokenKind::Comma, loc, .. });
                }
            }
        } else {
            result.push(tokens[i].clone());
            i += 1;
        }
    }

    result
}
```

#### `analyze_single_macro` での呼び出し

`expand_macro_body_for_inference` の結果に対して、パース前にカンマ注入を適用:

```rust
let (expanded_tokens, called_macros) = match pp.expand_macro_body_for_inference(...) {
    Ok(result) => result,
    Err(_) => (def.body.clone(), HashSet::new()),
};

// assert_(cond) の後にカンマを注入
let expanded_tokens = inject_comma_after_assert_underscore(
    &expanded_tokens,
    &self.no_expand,
);
```

## 変更ファイル一覧

| ファイル | 変更内容 |
|----------|----------|
| `src/macro_infer.rs` | `inject_comma_after_assert_underscore()` 追加、`analyze_single_macro` で呼び出し |

## 検証

1. `cargo build` / `cargo test`

2. 出力確認:
   ```bash
   cargo run -- --auto --gen-rust samples/xs-wrapper.h --bindings samples/bindings.rs 2>/dev/null \
     | grep -B 1 -A 8 'fn SvOK_off\b'
   ```
   - PARSE_FAILED ではなく関数が生成されること
   - `assert!` 呼び出しが含まれること

3. 同様のパターンのマクロも確認:
   ```bash
   cargo run -- --auto --gen-rust samples/xs-wrapper.h --bindings samples/bindings.rs 2>/dev/null \
     | grep -B 1 -A 8 'fn SvOK_off_exc_UV\b'
   ```

4. 回帰テスト: `cargo test rust_codegen_regression`

## エッジケース

1. **既にカンマがある場合**: `assert_(cond), expr` → カンマ重複を避ける。
   次の有効トークンが `Comma` ならスキップ。

2. **閉じ括弧が続く場合**: `(assert_(cond))` → カンマ不要。
   次の有効トークンが `RParen` ならスキップ。

3. **連続する assert_**: `assert_(c1) assert_(c2) expr` →
   `assert_(c1), assert_(c2), expr`。各 `assert_` の後にカンマが注入される。

4. **assert_ が式の最後**: `(... , assert_(cond))` → カンマ不要。
   後続がないのでスキップ。

5. **__ASSERT_ 展開後**: `__ASSERT_` は `ExplicitExpandSymbols` で展開され、
   結果に `assert_(cond)` が含まれる。この `assert_` にもカンマ注入が適用される。
