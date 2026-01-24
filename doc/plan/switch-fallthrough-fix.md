# Switch 文の fall-through と式文の構文エラー修正

## 問題

`--strict-rustfmt` で生成コードに構文エラーが発生している。

### エラーメッセージ

```
error: expected one of `=>`, `if`, or `|`, found `;`
    --> <stdin>:2653:14
     |
2653 |         break;
     |              ^ expected one of `=>`, `if`, or `|`

error: expected pattern, found `{`
    --> <stdin>:3293:13
     |
3293 |         { v1 -= ...
     |         ^ expected pattern
```

## 問題1: Switch の fall-through

### 原因

C の switch 文で fall-through パターンがあると、case の間に文が出力されてしまう。

#### C のコード例

```c
switch (len) {
    case 12: v2 += (U32)key[11] << 24;  /* FALLTHROUGH */
    case 11: v2 += (U32)key[10] << 16;  /* FALLTHROUGH */
    case 10: v2 += (U32)U8TO16_LE(key+8);
             v1 -= U8TO32_LE(key+4);   // ← case 10 に属する
             v0 += U8TO32_LE(key+0);   // ← case 10 に属する
             goto zaphod32_finalize;
    case 9: ...
}
```

#### AST 構造

```
Stmt::Case {
    expr: 10,
    stmt: "v2 += ..."  // 最初の文のみ
}
// 以下は Case の外に存在
"v1 -= ..."
"v0 += ..."
"goto ..."
Stmt::Case {
    expr: 9,
    stmt: ...
}
```

#### 現在の出力（エラー）

```rust
match len {
    10 => {
        { v2 += ...; v2 };
    }
    { v1 -= ...; v1 };  // ← match 内で不正
    { v0 += ...; v0 };  // ← match 内で不正
    break 'zaphod32_finalize; // goto
    9 => {
        ...
    }
}
```

### 解決策

`collect_switch_cases` を変更して、Case/Default 以外の文を直前の case のボディに追加する。

## 問題2: 代入式の冗長なブロック

### 原因

`ExprKind::Assign` の処理で、代入式を常に値を返す形式に変換している：

```rust
// rust_codegen.rs:1289-1295
ExprKind::Assign { op, lhs, rhs } => {
    match op {
        AssignOp::Assign => format!("{{ {} = {}; {} }}", l, r, l),
        _ => format!("{{ {} {} {}; {} }}", l, assign_op_to_rust(*op), r, l),
    }
}
```

これは C の式としての代入（`int x = (v2 += 5);`）を再現するため。

しかし、**文として使われる場合**は値を返す必要がない：

```c
v2 += 5;  // 値は使われない
```

#### 現在の出力

```rust
{ v2 += 5; v2 };  // 冗長
```

#### 期待する出力

```rust
v2 += 5;  // シンプル
```

### 解決策

`stmt_to_rust_inline` で代入式を特別扱いし、値を返さない形式で出力する。

## 実装計画

### Step 1: 式文での代入式の特別扱い

`stmt_to_rust_inline` の `Stmt::Expr(Some(expr), _)` 処理を修正：

```rust
Stmt::Expr(Some(expr), _) => {
    // 代入式は値を返さない形式で出力
    if let ExprKind::Assign { op, lhs, rhs } = &expr.kind {
        let l = self.expr_to_rust_inline(lhs);
        let r = self.expr_to_rust_inline(rhs);
        match op {
            AssignOp::Assign => format!("{}{} = {};", indent, l, r),
            _ => format!("{}{} {} {};", indent, l, assign_op_to_rust(*op), r),
        }
    } else {
        format!("{}{};", indent, self.expr_to_rust_inline(expr))
    }
}
```

### Step 2: collect_switch_cases の修正

Case/Default 以外の文を直前の case に含める：

```rust
struct SwitchCase {
    case_expr: Option<String>,  // None = default
    body_stmts: Vec<String>,
}

fn collect_switch_cases_v2(&mut self, stmt: &Stmt, indent: &str) -> Vec<SwitchCase> {
    let mut cases: Vec<SwitchCase> = Vec::new();

    // Compound の中身をフラット化して処理
    for item in flatten_compound(stmt) {
        match item {
            Stmt::Case { expr, stmt: case_stmt, .. } => {
                cases.push(SwitchCase {
                    case_expr: Some(self.expr_to_rust_inline(expr)),
                    body_stmts: vec![self.stmt_to_rust_inline(case_stmt, indent)],
                });
            }
            Stmt::Default { stmt: default_stmt, .. } => {
                cases.push(SwitchCase {
                    case_expr: None,
                    body_stmts: vec![self.stmt_to_rust_inline(default_stmt, indent)],
                });
            }
            other => {
                // 直前の case に追加
                if let Some(last) = cases.last_mut() {
                    last.body_stmts.push(self.stmt_to_rust_inline(other, indent));
                }
            }
        }
    }

    cases
}
```

### Step 3: match アームの生成

収集した cases から match アームを生成：

```rust
fn generate_match_arms(&self, cases: &[SwitchCase], indent: &str) -> String {
    let mut result = String::new();
    let body_indent = format!("{}    ", indent);

    for case in cases {
        let pattern = case.case_expr.as_deref().unwrap_or("_");
        result.push_str(&format!("{}{} => {{\n", indent, pattern));
        for stmt in &case.body_stmts {
            result.push_str(&format!("{}{}\n", body_indent, stmt));
        }
        result.push_str(&format!("{}}}\n", indent));
    }

    result
}
```

## 修正後の出力例

```rust
match len {
    10 => {
        v2 += ...;          // シンプルな代入文
        v1 -= ...;          // case 10 に含まれる
        v0 += ...;          // case 10 に含まれる
        break 'zaphod32_finalize;
    }
    9 => {
        v2 += ...;
    }
}
```

## 変更対象

| ファイル | 変更内容 |
|----------|----------|
| `src/rust_codegen.rs` | `stmt_to_rust_inline` で代入式を特別扱い（RustCodegen, InlineFnCodegen 両方） |
| `src/rust_codegen.rs` | `collect_switch_cases` を2パス方式に変更（RustCodegen, InlineFnCodegen 両方） |

## テスト

1. `cargo build` でビルド確認
2. `cargo test` で既存テストが通ることを確認
3. `cargo run -- --auto --gen-rust samples/wrapper.h --bindings samples/bindings.rs --strict-rustfmt` でエラーが出ないことを確認
