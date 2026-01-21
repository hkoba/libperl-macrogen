# do-while 文の Rust 変換

## 問題

inline 関数のコード生成で `/* TODO: Discriminant(5) */` が出力される。
`Discriminant(5)` は `Stmt::DoWhile`（do-while 文）。

### 例: Perl_cx_popsub_args

```c
// C の定義 (CX_POP_SAVEARRAY マクロ展開後)
do {
    AV *cx_pop_savearray_av = GvAV(PL_defgv);
    GvAV(PL_defgv) = cx->blk_sub.savearray;
    cx->blk_sub.savearray = NULL;
    SvREFCNT_dec(cx_pop_savearray_av);
} while (0);
```

```rust
// 現在の出力（誤り）
// /* TODO: Discriminant(5) */
//     { av = ((*(*my_perl).Icurpad.offset(0 as isize)) as *mut AV); av };

// 期待する出力
{
    let cx_pop_savearray_av: *mut AV = ...;
    ...
    Perl_SvREFCNT_dec(my_perl, ...);
}
{ av = ((*(*my_perl).Icurpad.offset(0 as isize)) as *mut AV); av };
```

## 原因

`stmt_to_rust_inline` が `Stmt::DoWhile` を処理していない。

### stmt_to_rust_inline の現在の処理 (line 1556-1597)

```rust
fn stmt_to_rust_inline(&self, stmt: &Stmt, indent: &str) -> String {
    match stmt {
        Stmt::Expr(Some(expr), _) => { ... }
        Stmt::Expr(None, _) => { ... }
        Stmt::Return(Some(expr), _) => { ... }
        Stmt::Return(None, _) => { ... }
        Stmt::If { cond, then_stmt, else_stmt, .. } => { ... }
        Stmt::Compound(compound) => { ... }
        _ => format!("{}/* TODO: {:?} */", indent, std::mem::discriminant(stmt))
    }
}
```

## Stmt enum の構造

```rust
pub enum Stmt {
    Compound(CompoundStmt),     // 0
    Expr(...),                  // 1
    If { ... },                 // 2
    Switch { ... },             // 3
    While { ... },              // 4
    DoWhile { ... },            // 5 ← これが未処理
    For { ... },                // 6
    Goto(...),                  // 7
    Continue(...),              // 8
    Break(...),                 // 9
    Return(...),                // 10
    Label { ... },              // 11
    Case { ... },               // 12
    Default { ... },            // 13
}
```

## 解決策

### `do { ... } while (0)` パターン

C マクロでよく使われる `do { ... } while (0)` パターンは、
ループが1回だけ実行されるため、Rust では単純なブロック `{ ... }` に変換できる。

### 一般的な do-while 文

`do { body } while (cond)` は Rust では `loop` を使って表現:

```rust
loop {
    body;
    if cond == 0 { break; }
}
```

### 実装

```rust
Stmt::DoWhile { body, cond, .. } => {
    // do { ... } while (0) パターンの検出
    if self.is_zero_constant(cond) {
        // 単純なブロックとして出力
        return self.stmt_to_rust_inline(body, indent);
    }

    // 一般的な do-while 文
    let mut result = format!("{}loop {{\n", indent);
    let nested_indent = format!("{}    ", indent);
    result.push_str(&self.stmt_to_rust_inline(body, &nested_indent));
    result.push_str("\n");
    let cond_str = self.expr_to_rust_inline(cond);
    result.push_str(&format!("{}    if {} == 0 {{ break; }}\n", indent, cond_str));
    result.push_str(&format!("{}}}", indent));
    result
}
```

### ヘルパー関数

```rust
/// 式がゼロ定数かどうかを判定
fn is_zero_constant(&self, expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::IntLit(0) => true,
        ExprKind::UIntLit(0) => true,
        _ => false,
    }
}
```

## 変更対象ファイル

| ファイル | 変更内容 |
|----------|----------|
| `src/rust_codegen.rs` | `RustCodegen::stmt_to_rust_inline` に `DoWhile` 処理を追加 |
| `src/rust_codegen.rs` | `CodegenDriver::stmt_to_rust_inline` に `DoWhile` 処理を追加 |
| `src/rust_codegen.rs` | `is_zero_constant` ヘルパー関数を追加 |

## 将来の拡張

以下の文も同様に処理が必要：

| 文種別 | Rust 変換 |
|--------|-----------|
| `While` | `while cond != 0 { body }` |
| `For` | `for` または `loop` |
| `Switch` | `match` (複雑) |
| `Goto` / `Label` | サポート困難 |
| `Continue` / `Break` | そのまま |

## テスト

1. `cargo build` でビルド確認
2. `cargo test` で既存テストが通ることを確認
3. `--gen-rust` で以下を確認:
   - `Perl_cx_popsub_args` の `/* TODO: Discriminant(5) */` が消える
   - `do { ... } while (0)` が単純なブロックとして出力される
