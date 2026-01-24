# 条件付き unsafe ブロック生成

## 概要

現在、すべての生成関数に `unsafe { }` ブロックを追加しているが、
関数呼び出しを含まないマクロ/インライン関数では `unnecessary unsafe block` 警告が発生する。

関数呼び出しの有無を解析し、必要な場合のみ `unsafe { }` を生成するように改良する。

## 問題のあるコード例

```rust
/// UNI_TO_NATIVE - macro function
#[inline]
pub unsafe fn UNI_TO_NATIVE(ch: UV) -> UV {
    unsafe {  // ← unnecessary unsafe block warning
        ((ch | 0) as UV)
    }
}
```

## 設計方針：パーサーベースのカウント

### 方針選択の理由

AST走査（2パス）ではなく、パーサーベースのカウント（1パス）を採用する。

| 観点 | パーサーベース | AST走査 |
|------|--------------|---------|
| パス数 | 1パス | 2パス |
| 実装の複雑さ | Parser修正 + API変更 | 新規メソッド追加のみ |
| 精度 | ◎ パース時に確実 | ◎ AST構造に基づく |
| 関心の分離 | △ パーサーに責務追加 | ◎ 別処理として独立 |

**採用理由:**
1. **効率的** - 1パスで完結
2. **マクロ展開との整合性** - 展開後トークンをパースするので自然に正しいカウントが得られる
3. **実装が明確** - ExprKind::Call 生成箇所が2箇所のみなので漏れにくい

### 1. Parser に関数呼び出しカウンターを追加

**ファイル:** `src/parser.rs`

```rust
pub struct Parser<S: Source> {
    // ... existing fields ...

    /// パース中に検出した関数呼び出しの数
    pub function_call_count: usize,
}
```

#### 1.1 ExprKind::Call 生成時にインクリメント

ExprKind::Call は以下の2箇所で生成される：

- **1717行目付近**: 後置呼び出し式 `foo(args)`
- **1874行目付近**: 同様のパターン

```rust
// ExprKind::Call 生成時
self.function_call_count += 1;
ExprKind::Call { func: Box::new(func_expr), args }
```

#### 1.2 パーサー初期化時にカウンターをリセット

```rust
impl<S: Source> Parser<S> {
    pub fn new(source: &mut S) -> Result<Self> {
        Ok(Self {
            // ... existing fields ...
            function_call_count: 0,
        })
    }
}
```

### 2. パース関数の戻り値を拡張

**ファイル:** `src/parser.rs`

#### 2.1 統計情報を含む結果型を定義

```rust
/// パース結果に付随する統計情報
#[derive(Debug, Clone, Default)]
pub struct ParseStats {
    /// 関数呼び出しの数
    pub function_call_count: usize,
}
```

#### 2.2 統計情報付きパース関数を追加

```rust
/// トークン列から式をパース（統計情報付き）
pub fn parse_expression_from_tokens_ref_with_stats(
    tokens: Vec<Token>,
    interner: &StringInterner,
    files: &FileRegistry,
    typedefs: &HashSet<InternedStr>,
) -> Result<(Expr, ParseStats)> {
    let mut source = TokenSliceRef::new(tokens, interner, files);
    let mut parser = Parser::from_source_with_typedefs(&mut source, typedefs.clone())?;
    let expr = parser.parse_expr_only()?;
    let stats = ParseStats {
        function_call_count: parser.function_call_count,
    };
    Ok((expr, stats))
}

/// トークン列を文としてパース（統計情報付き）
pub fn parse_statement_from_tokens_ref_with_stats(
    tokens: Vec<Token>,
    interner: &StringInterner,
    files: &FileRegistry,
    typedefs: &HashSet<InternedStr>,
) -> Result<(Stmt, ParseStats)> {
    let mut source = TokenSliceRef::new(tokens, interner, files);
    let mut parser = Parser::from_source_with_typedefs(&mut source, typedefs.clone())?;
    let stmt = parser.parse_statement()?;
    let stats = ParseStats {
        function_call_count: parser.function_call_count,
    };
    Ok((stmt, stats))
}
```

### 3. MacroInferInfo に関数呼び出し情報を追加

**ファイル:** `src/macro_infer.rs`

```rust
pub struct MacroInferInfo {
    // ... existing fields ...

    /// 関数呼び出しを含むかどうか（パース時に検出）
    pub has_function_calls: bool,
}
```

#### 3.1 try_parse_tokens を修正

```rust
/// トークン列を式または文としてパース試行
fn try_parse_tokens(
    &self,
    tokens: &[Token],
    interner: &StringInterner,
    files: &FileRegistry,
    typedefs: &HashSet<InternedStr>,
) -> (ParseResult, bool) {  // (結果, has_function_calls)
    // ...

    // 文としてパース試行
    if is_statement_start {
        match parse_statement_from_tokens_ref_with_stats(tokens.to_vec(), interner, files, typedefs) {
            Ok((stmt, stats)) => {
                return (
                    ParseResult::Statement(vec![BlockItem::Stmt(stmt)]),
                    stats.function_call_count > 0,
                );
            }
            Err(_) => {}
        }
    }

    // 式としてパース
    match parse_expression_from_tokens_ref_with_stats(tokens.to_vec(), interner, files, typedefs) {
        Ok((expr, stats)) => (
            ParseResult::Expression(Box::new(expr)),
            stats.function_call_count > 0,
        ),
        Err(err) => (
            ParseResult::Unparseable(Some(err.format_with_files(files))),
            false,
        ),
    }
}
```

### 4. FunctionDef に関数呼び出し情報を追加

**ファイル:** `src/inline_fn.rs`

```rust
pub struct FunctionDef {
    // ... existing fields ...

    /// 関数呼び出しの数（パース時に検出）
    pub function_call_count: usize,
}
```

#### 4.1 インライン関数収集時に統計を記録

インライン関数のパース時に Parser の `function_call_count` を取得し、
FunctionDef に格納する。

**注意:** インライン関数は宣言パース時に収集されるため、
Parser インスタンスから直接カウントを取得する方法を検討する。

代替案: インライン関数本体（CompoundStmt）のパース後に、
Parser の累積カウントを記録する。

### 5. コード生成の修正

**ファイル:** `src/rust_codegen.rs`

#### 5.1 マクロ関数の生成

```rust
pub fn generate_macro(mut self, info: &MacroInferInfo) -> GeneratedCode {
    // ...

    // 関数呼び出しがある場合のみ unsafe ブロックを生成
    if info.has_function_calls {
        self.writeln("    unsafe {");
        // body with 8-space indent
        self.writeln("    }");
    } else {
        // body with 4-space indent (no unsafe block)
    }

    // ...
}
```

#### 5.2 インライン関数の生成

```rust
pub fn generate_inline_fn(mut self, name: InternedStr, func_def: &FunctionDef) -> GeneratedCode {
    // ...

    // 関数呼び出しがある場合のみ unsafe ブロックを生成
    if func_def.function_call_count > 0 {
        self.writeln("    unsafe {");
        let body_str = self.compound_stmt_to_string(&func_def.body, "        ");
        self.buffer.push_str(&body_str);
        self.writeln("    }");
    } else {
        let body_str = self.compound_stmt_to_string(&func_def.body, "    ");
        self.buffer.push_str(&body_str);
    }

    // ...
}
```

## 実装順序

1. **Step 1:** Parser に `function_call_count` フィールドを追加
   - `src/parser.rs` を修正
   - 初期化時に 0 にリセット
   - ExprKind::Call 生成時（2箇所）にインクリメント

2. **Step 2:** ParseStats 構造体と統計付きパース関数を追加
   - `src/parser.rs` に `ParseStats` を定義
   - `parse_expression_from_tokens_ref_with_stats` を追加
   - `parse_statement_from_tokens_ref_with_stats` を追加

3. **Step 3:** MacroInferInfo に `has_function_calls` フィールドを追加
   - `src/macro_infer.rs` を修正
   - `try_parse_tokens` を修正して統計を返す
   - パース時にフィールドを設定

4. **Step 4:** FunctionDef に `function_call_count` フィールドを追加
   - `src/inline_fn.rs` を修正
   - インライン関数収集時にカウントを記録

5. **Step 5:** RustCodegen を修正
   - `generate_macro` で条件分岐
   - `generate_inline_fn` で条件分岐

6. **Step 6:** テストと検証
   - ビルドして warning が減少することを確認

## 変更対象ファイル

| ファイル | 変更内容 |
|----------|----------|
| `src/parser.rs` | `function_call_count` フィールド追加、`ParseStats` 定義、統計付きパース関数追加 |
| `src/macro_infer.rs` | `has_function_calls` フィールド追加、`try_parse_tokens` 修正 |
| `src/inline_fn.rs` | `function_call_count` フィールド追加 |
| `src/rust_codegen.rs` | 条件付き unsafe ブロック生成 |

## 備考

- ポインタのデリファレンス (`*ptr`) も unsafe 操作だが、`unsafe fn` 内では
  unsafe ブロックなしでも許可されるため、関数呼び出しのみを対象とする
- 将来的に `ptr.offset()` などの unsafe メソッド呼び出しも検出対象に
  追加することを検討
- マクロ展開との関係:
  - `TokenExpander` がトークンを展開した後にパースされる
  - 展開されたマクロ内の関数呼び出しは正しくカウントされる
  - `NoExpandSymbols`（assert, SvANY）は展開されずに関数呼び出しとしてカウント
