# マクロ関数のRust関数化 実装計画

## 概要

Perlヘッダーで定義されたCマクロ関数をRust関数に変換する。
そのために、マクロ本体をCの式としてパースし、型情報を付与した上で
Rustコードを生成する。

## 最終目標

```c
#define SvANY(sv)       (sv)->sv_any
```

から

```rust
#[inline]
pub fn SvANY(sv: *mut SV) -> *mut c_void {
    unsafe { (*sv).sv_any }
}
```

を生成する。

## 現状の課題

`Parser` は `Preprocessor` と密結合している:

```rust
pub struct Parser<'a> {
    pp: &'a mut Preprocessor,  // プリプロセッサへの可変参照
    current: Token,
    typedefs: HashSet<InternedStr>,
}
```

マクロ本体は `Vec<Token>` として保存されているが、
これを既存のパーサーでパースするには、トークンソースの抽象化が必要。

## 実装方針: 方法B（トークンソース抽象化）

### Phase 1: トークンソースの抽象化

#### 1.1 TokenSource trait の定義 (~50行)

新規ファイル `src/token_source.rs`:

```rust
use crate::error::Result;
use crate::intern::StringInterner;
use crate::token::Token;

/// トークンを供給するソースの抽象化
pub trait TokenSource {
    /// 次のトークンを取得
    fn next_token(&mut self) -> Result<Token>;

    /// StringInterner への参照を取得
    fn interner(&self) -> &StringInterner;

    /// StringInterner への可変参照を取得
    fn interner_mut(&mut self) -> &mut StringInterner;
}
```

#### 1.2 Preprocessor に TokenSource を実装 (~30行)

`src/preprocessor.rs` に追加:

```rust
impl TokenSource for Preprocessor {
    fn next_token(&mut self) -> Result<Token> {
        self.next_token()  // 既存メソッドを呼ぶ
    }

    fn interner(&self) -> &StringInterner {
        &self.interner
    }

    fn interner_mut(&mut self) -> &mut StringInterner {
        &mut self.interner
    }
}
```

#### 1.3 TokenSlice の実装 (~80行)

新規構造体 `TokenSlice`:

```rust
/// トークン列からトークンを供給
pub struct TokenSlice {
    tokens: Vec<Token>,
    pos: usize,
    interner: StringInterner,
}

impl TokenSlice {
    pub fn new(tokens: Vec<Token>, interner: StringInterner) -> Self {
        Self { tokens, pos: 0, interner }
    }
}

impl TokenSource for TokenSlice {
    fn next_token(&mut self) -> Result<Token> {
        if self.pos < self.tokens.len() {
            let token = self.tokens[self.pos].clone();
            self.pos += 1;
            Ok(token)
        } else {
            // EOF トークンを返す
            Ok(Token::new(TokenKind::Eof, SourceLocation::default()))
        }
    }

    fn interner(&self) -> &StringInterner {
        &self.interner
    }

    fn interner_mut(&mut self) -> &mut StringInterner {
        &mut self.interner
    }
}
```

### Phase 2: Parser の改修

#### 2.1 Parser の汎用化 (~100行の変更)

`src/parser.rs` の変更:

```rust
// Before
pub struct Parser<'a> {
    pp: &'a mut Preprocessor,
    current: Token,
    typedefs: HashSet<InternedStr>,
}

// After
pub struct Parser<'a, S: TokenSource> {
    source: &'a mut S,
    current: Token,
    typedefs: HashSet<InternedStr>,
}
```

既存の `Parser::new(pp: &mut Preprocessor)` はそのまま維持し、
新たに `Parser::from_source(source: &mut S)` を追加。

#### 2.2 式パース用エントリポイント (~30行)

```rust
impl<'a, S: TokenSource> Parser<'a, S> {
    /// トークン列から式をパース
    pub fn parse_expr_only(&mut self) -> Result<Expr> {
        self.parse_expr()
    }
}

/// ヘルパー関数: トークン列から式をパース
pub fn parse_expression_from_tokens(
    tokens: Vec<Token>,
    interner: StringInterner,
) -> Result<Expr> {
    let mut source = TokenSlice::new(tokens, interner);
    let mut parser = Parser::from_source(&mut source)?;
    parser.parse_expr_only()
}
```

### Phase 3: マクロ展開とパースの統合

#### 3.1 MacroAnalyzer の拡張 (~100行)

```rust
impl MacroAnalyzer {
    /// マクロ本体をパースして AST を取得
    pub fn parse_macro_body(&self, def: &MacroDef) -> Result<Expr> {
        // 1. マクロ展開（再帰的に他マクロを展開）
        let expanded = self.expand_macro_body(def, macros, &mut HashSet::new());

        // 2. トークン列をパース
        let interner = self.interner.clone();  // 注: 所有権の問題があれば要調整
        parse_expression_from_tokens(expanded, interner)
    }

    /// パース結果を型注釈付きS式で出力
    pub fn dump_parsed_sexp(&self, macros: &MacroTable) -> String {
        // Expression マクロをパースしてS式出力
    }
}
```

### Phase 4: Rust コード生成

#### 4.1 RustCodeGen モジュール (~200行)

新規ファイル `src/rust_codegen.rs`:

```rust
pub struct RustCodeGen<'a> {
    interner: &'a StringInterner,
    fields_dict: &'a FieldsDict,
}

impl RustCodeGen {
    /// 式をRustコードに変換
    pub fn expr_to_rust(&self, expr: &Expr, info: &MacroInfo) -> String {
        match expr {
            Expr::Binary { op, left, right, .. } => {
                format!("{} {} {}",
                    self.expr_to_rust(left, info),
                    self.op_to_rust(op),
                    self.expr_to_rust(right, info))
            }
            Expr::Member { expr, arrow, member, .. } => {
                if *arrow {
                    format!("(*{}).{}", self.expr_to_rust(expr, info), member)
                } else {
                    format!("{}.{}", self.expr_to_rust(expr, info), member)
                }
            }
            // ...
        }
    }

    /// マクロをRust関数に変換
    pub fn macro_to_rust_fn(&self, def: &MacroDef, info: &MacroInfo, expr: &Expr) -> String {
        let name = self.interner.get(def.name);
        let params = self.format_params(def, info);
        let ret_ty = info.return_type.as_deref().unwrap_or("()");
        let body = self.expr_to_rust(expr, info);

        format!(
            "#[inline]\npub unsafe fn {}({}) -> {} {{\n    {}\n}}\n",
            name, params, ret_ty, body
        )
    }
}
```

## 実装順序

1. **Phase 1.1-1.3**: TokenSource trait とその実装（1日目）
2. **Phase 2.1-2.2**: Parser の汎用化（1-2日目）
3. **Phase 3.1**: MacroAnalyzer 拡張（2日目）
4. **Phase 4.1**: Rust コード生成（3日目）

## 見積もり行数

| Phase | 新規 | 変更 | 計 |
|-------|------|------|-----|
| 1.1 TokenSource trait | 50 | 0 | 50 |
| 1.2 Preprocessor impl | 30 | 0 | 30 |
| 1.3 TokenSlice | 80 | 0 | 80 |
| 2.1-2.2 Parser改修 | 30 | 100 | 130 |
| 3.1 MacroAnalyzer拡張 | 100 | 50 | 150 |
| 4.1 RustCodeGen | 200 | 0 | 200 |
| **合計** | **490** | **150** | **640** |

## リスク・課題

1. **StringInterner の所有権**: TokenSlice が独自のインターナーを持つと、
   既存のインターン済み文字列と互換性がなくなる。参照カウント or 共有が必要かも。

2. **型パラメータの伝播**: `Parser<'a, S>` の型パラメータが既存コードに
   影響する可能性。型エイリアス `type PreprocessorParser<'a> = Parser<'a, Preprocessor>`
   で軽減可能。

3. **マクロパラメータの扱い**: マクロパラメータは展開時に置換される。
   パース時に「これはパラメータ」という情報を保持する仕組みが必要。

## 次のステップ

1. このドキュメントをレビュー
2. Phase 1 から順に実装開始
3. 各Phase完了時にテストを追加
