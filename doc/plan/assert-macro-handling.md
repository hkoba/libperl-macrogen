# assert マクロの特別処理

## 目標

`assert(what)` と `assert_(what)` マクロを特別扱いし、DEBUGGING が undef でマクロが空に展開される場合でも、条件式を AST に保持して Rust の `debug_assert!` を生成できるようにする。

## 背景

### Perl の assert マクロ定義

```c
// DEBUGGING が定義されている場合:
#define assert_(what)  assert(what),

// DEBUGGING が未定義の場合:
#define assert_(what)   // empty
```

`assert_` は式の中で使われることが多い（例: `(assert_(cond), actual_expr)`）。
DEBUGGING が undef だと展開結果が空になり、条件式が失われる。

## 実装計画

### アプローチ: TokenExpander の no_expand 機能を活用

`assert` と `assert_` を `no_expand` に登録することで、マクロ展開を抑制し、
パーサーが `Call { func: Ident("assert"), args }` としてパース。
その後、Call 式を `Assert` 式に変換する。

### Step 1: AssertKind enum を追加 (src/ast.rs)

```rust
/// アサーションマクロの種類
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssertKind {
    /// assert(condition)
    Assert,
    /// assert_(condition) - 末尾カンマ付き
    AssertUnderscore,
}
```

### Step 2: ExprKind に Assert バリアントを追加 (src/ast.rs)

`StmtExpr` の後に追加:

```rust
pub enum ExprKind {
    // ... 既存バリアント ...

    StmtExpr(CompoundStmt),

    /// アサーション式（マクロが空に展開されても保持）
    Assert {
        kind: AssertKind,
        condition: Box<Expr>,
    },
}
```

### Step 3: assert 検出ヘルパーを追加 (src/macro_infer.rs)

```rust
/// マクロ名がアサーションマクロかどうかを判定
fn detect_assert_kind(name: &str) -> Option<AssertKind> {
    match name {
        "assert" => Some(AssertKind::Assert),
        "assert_" => Some(AssertKind::AssertUnderscore),
        _ => None,
    }
}
```

### Step 4: assert_symbols を thx_symbols と同様に渡す

`thx_symbols` と同様に、事前に intern したシンボルを引数で渡す。

**analyze_all_macros() のシグネチャ変更 (src/macro_infer.rs)**:

```rust
pub fn analyze_all_macros<'a>(
    &mut self,
    macro_table: &MacroTable,
    interner: &'a StringInterner,
    files: &FileRegistry,
    apidoc: Option<&'a ApidocDict>,
    fields_dict: Option<&'a FieldsDict>,
    rust_decl_dict: Option<&'a RustDeclDict>,
    inline_fn_dict: Option<&'a InlineFnDict>,
    typedefs: &HashSet<InternedStr>,
    thx_symbols: (InternedStr, InternedStr, InternedStr),
    assert_symbols: (InternedStr, InternedStr),  // 追加
)
```

**build_macro_info() のシグネチャ変更**:

```rust
pub fn build_macro_info(
    &self,
    def: &MacroDef,
    macro_table: &MacroTable,
    interner: &StringInterner,
    files: &FileRegistry,
    rust_decl_dict: Option<&RustDeclDict>,
    typedefs: &HashSet<InternedStr>,
    thx_symbols: (InternedStr, InternedStr, InternedStr),
    assert_symbols: (InternedStr, InternedStr),  // 追加
) -> (MacroInferInfo, bool, bool)
```

**main.rs での呼び出し修正**:

```rust
let sym_assert = pp.interner_mut().intern("assert");
let sym_assert_ = pp.interner_mut().intern("assert_");
let assert_symbols = (sym_assert, sym_assert_);

infer_ctx.analyze_all_macros(
    pp.macros(), interner, files,
    Some(&apidoc), Some(&fields_dict),
    rust_decl_dict.as_ref(), Some(&inline_fn_dict),
    &typedefs, thx_symbols, assert_symbols,  // assert_symbols 追加
);
```

### Step 5: TokenExpander に assert を no_expand 登録 (src/macro_infer.rs)

`build_macro_info()` 内で `assert_symbols` を使用:

```rust
let (sym_assert, sym_assert_) = assert_symbols;

let mut expander = TokenExpander::new(macro_table, interner, files);
if let Some(dict) = rust_decl_dict {
    expander.set_bindings_consts(&dict.consts);
}

// assert マクロを展開しないよう登録
expander.add_no_expand(sym_assert);
expander.add_no_expand(sym_assert_);

let expanded_tokens = expander.expand_with_calls(&def.body);
```

### Step 6: パース後に Call を Assert に変換 (src/macro_infer.rs)

`try_parse_tokens()` の後で、パース結果の式を走査し、
`assert`/`assert_` への Call を Assert に変換:

```rust
fn convert_assert_calls(expr: &mut Expr, interner: &StringInterner) {
    // 再帰的に子式を処理
    match &mut expr.kind {
        ExprKind::Call { func, args } => {
            // 子を先に処理
            convert_assert_calls(func, interner);
            for arg in args.iter_mut() {
                convert_assert_calls(arg, interner);
            }

            // assert/assert_ 呼び出しを検出
            if let ExprKind::Ident(name) = &func.kind {
                let name_str = interner.get(*name);
                if let Some(kind) = detect_assert_kind(name_str) {
                    if let Some(condition) = args.pop() {
                        expr.kind = ExprKind::Assert {
                            kind,
                            condition: Box::new(condition),
                        };
                    }
                }
            }
        }
        // 他の式も再帰処理
        ExprKind::Binary { lhs, rhs, .. } => {
            convert_assert_calls(lhs, interner);
            convert_assert_calls(rhs, interner);
        }
        // ... 他のバリアント ...
        _ => {}
    }
}
```

### Step 7: build_macro_info() でパース後に変換を適用

```rust
// パースを試行
info.parse_result = self.try_parse_tokens(&expanded_tokens, interner, files, typedefs);

// パース成功した場合、assert 呼び出しを Assert 式に変換
if let ParseResult::Expression(ref mut expr) = info.parse_result {
    convert_assert_calls(expr, interner);
}
```

### Step 8: collect_expr_constraints() を更新 (src/semantic.rs)

```rust
// collect_expr_constraints() の match に追加:
ExprKind::Assert { condition, .. } => {
    self.collect_expr_constraints(condition, type_env);
    type_env.add_constraint(TypeEnvConstraint::new(
        expr.id, "void", ConstraintSource::Inferred, "assertion"
    ));
}
```

### Step 9: S式出力を更新 (src/sexp.rs)

```rust
// SexpPrinter::print_expr() に追加:
ExprKind::Assert { kind, condition } => {
    let kind_str = match kind {
        AssertKind::Assert => "assert",
        AssertKind::AssertUnderscore => "assert_",
    };
    self.write_open(kind_str)?;
    self.print_expr(condition)?;
    self.write_close()
}
```

### Step 10: Rust コード生成を更新 (src/rust_codegen.rs)

```rust
// expr_to_rust() に追加:
ExprKind::Assert { kind, condition } => {
    let cond_frag = self.expr_to_rust(condition);
    CodeFragment::ok(format!("debug_assert!({})", cond_frag.code))
}
```

## 修正対象ファイル

1. **src/ast.rs**
   - `AssertKind` enum 追加
   - `ExprKind::Assert` バリアント追加

2. **src/macro_infer.rs**
   - `detect_assert_kind()` ヘルパー追加
   - `convert_assert_calls()` 関数追加
   - `build_macro_info()` 修正（no_expand 登録 + 変換呼び出し）

3. **src/semantic.rs**
   - `collect_expr_constraints()` に `Assert` case 追加

4. **src/sexp.rs**
   - `print_expr()` に `Assert` case 追加

5. **src/rust_codegen.rs**
   - `expr_to_rust()` に `Assert` case 追加

6. **src/lib.rs**
   - `AssertKind` の再エクスポート追加

## 実装順序

1. AST 変更 (ast.rs) - AssertKind と ExprKind::Assert
2. 検出・変換ロジック (macro_infer.rs) - detect_assert_kind, convert_assert_calls
3. build_macro_info 修正 (macro_infer.rs) - no_expand 登録 + 変換適用
4. 型推論 (semantic.rs)
5. 出力 (sexp.rs, rust_codegen.rs)
6. ビルド・テスト
