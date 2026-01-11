# 文（Statement）マクロの型付S式出力

## 目標

`ParseResult::Statement` として判定されたマクロについて、型付S式を出力する。

## 背景

### 現状の問題

現在、main.rs の出力処理で `ParseResult::Statement` は無視されている：

```rust
match &info.parse_result {
    ParseResult::Expression(expr) => {
        // 型付S式を出力
    }
    ParseResult::Unparseable(Some(err_msg)) => {
        println!("  error: {}", err_msg);
    }
    _ => {}  // ← Statement がここで無視される
}
```

181 件の文マクロが検出されているが、S式が出力されない。

### 設計上のポイント

1. **文自体には型がない** - 文はExprIdを持たず、型制約はない
2. **文に含まれる式には型がある** - 条件式、式文などは型注釈を付けられる
3. TypedSexpPrinter の `print_stmt()` は存在するが、内部で非型付き `print_expr()` を呼ぶ可能性がある

## 実装計画

### Step 1: sexp.rs の確認（変更不要）

`print_stmt()` は内部で `self.print_expr()` を呼んでおり、
既に型注釈付きで出力される。変更不要。

### Step 2: macro_infer.rs に Statement の型制約収集を追加

現在 `analyze_macro` (L302-330) は Expression のみ処理している。
Statement についても、含まれる式の型制約を収集する必要がある。

**src/macro_infer.rs:**

```rust
// パース成功した場合、型制約を収集
if let ParseResult::Expression(ref expr) = info.parse_result {
    // ... 既存のコード ...
}

// Statement の場合も型制約を収集
if let ParseResult::Statement(ref block_items) = info.parse_result {
    let mut analyzer = SemanticAnalyzer::with_rust_decl_dict(
        interner,
        apidoc,
        fields_dict,
        rust_decl_dict,
    );

    // apidoc 型情報付きでパラメータをシンボルテーブルに登録
    analyzer.register_macro_params_from_apidoc(def.name, &params, files, typedefs);

    // 各 BlockItem について型制約を収集
    for item in block_items {
        if let BlockItem::Stmt(stmt) = item {
            analyzer.collect_stmt_constraints(stmt, &mut info.type_env);
        }
    }
}
```

### Step 3: semantic.rs に collect_stmt_constraints を追加

文から式を抽出して `collect_expr_constraints` を呼び出すメソッドを追加。

**src/semantic.rs:**

```rust
/// 文から式の型制約を収集
pub fn collect_stmt_constraints(&mut self, stmt: &Stmt, type_env: &mut TypeEnv) {
    match stmt {
        Stmt::Compound(compound) => {
            for item in &compound.items {
                if let BlockItem::Stmt(s) = item {
                    self.collect_stmt_constraints(s, type_env);
                }
            }
        }
        Stmt::Expr(Some(expr), _) => {
            self.collect_expr_constraints(expr, type_env);
        }
        Stmt::If { cond, then_stmt, else_stmt, .. } => {
            self.collect_expr_constraints(cond, type_env);
            self.collect_stmt_constraints(then_stmt, type_env);
            if let Some(else_s) = else_stmt {
                self.collect_stmt_constraints(else_s, type_env);
            }
        }
        Stmt::While { cond, body, .. } => {
            self.collect_expr_constraints(cond, type_env);
            self.collect_stmt_constraints(body, type_env);
        }
        Stmt::DoWhile { body, cond, .. } => {
            self.collect_stmt_constraints(body, type_env);
            self.collect_expr_constraints(cond, type_env);
        }
        Stmt::For { init, cond, step, body, .. } => {
            if let Some(ForInit::Expr(e)) = init {
                self.collect_expr_constraints(e, type_env);
            }
            if let Some(c) = cond {
                self.collect_expr_constraints(c, type_env);
            }
            if let Some(s) = step {
                self.collect_expr_constraints(s, type_env);
            }
            self.collect_stmt_constraints(body, type_env);
        }
        Stmt::Return(Some(expr), _) => {
            self.collect_expr_constraints(expr, type_env);
        }
        Stmt::Switch { expr, body, .. } => {
            self.collect_expr_constraints(expr, type_env);
            self.collect_stmt_constraints(body, type_env);
        }
        Stmt::Case { expr, stmt, .. } => {
            self.collect_expr_constraints(expr, type_env);
            self.collect_stmt_constraints(stmt, type_env);
        }
        Stmt::Default { stmt, .. } | Stmt::Label { stmt, .. } => {
            self.collect_stmt_constraints(stmt, type_env);
        }
        _ => {} // Break, Continue, Goto, Asm, Expr(None)
    }
}
```

### Step 4: main.rs に Statement の出力処理を追加

**src/main.rs (L552-569):**

```rust
match &info.parse_result {
    ParseResult::Expression(expr) => {
        // 既存の処理
    }
    ParseResult::Statement(block_items) => {
        let stdout = io::stdout();
        let mut handle = stdout.lock();
        let mut printer = TypedSexpPrinter::new(&mut handle, interner);
        printer.set_type_env(&info.type_env);
        printer.set_pretty(true);
        printer.set_indent(1);
        printer.set_skip_first_newline(true);
        for item in block_items {
            if let BlockItem::Stmt(stmt) = item {
                let _ = printer.print_stmt(stmt);
            }
        }
        let _ = writeln!(handle);
    }
    ParseResult::Unparseable(Some(err_msg)) => {
        println!("  error: {}", err_msg);
    }
    _ => {}
}
```

## 修正対象ファイル

1. **src/semantic.rs**
   - `collect_stmt_constraints` メソッド追加

2. **src/macro_infer.rs**
   - `analyze_macro` に Statement の型制約収集を追加

3. **src/main.rs**
   - `ParseResult::Statement` の出力処理を追加

## 期待される結果

```
CopFILE_copy: statement (N constraints, 2 uses) [THX]
  (do-while
    (compound
      (expr-stmt
        (call (ident CopFILE_copy_x) :type <unknown> ...) :type <unknown>)
      ...)
    (int-lit 0) :type int)
```

文マクロでも型付S式が出力される。

## 注意点

1. 文自体には `:type` 注釈は付かない（ExprId がないため）
2. 文に含まれる式のみ `:type` 注釈が付く
3. BlockItem::Decl の場合は宣言として出力（型注釈なし）
