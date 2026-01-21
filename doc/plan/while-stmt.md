# while 文の Rust 変換

## 問題

inline 関数のコード生成で `/* TODO: Discriminant(4) */` が出力される。
`Discriminant(4)` は `Stmt::While`（while 文）。

### 例: Perl_foldEQ

```c
// C の定義
while (len--) {
    if (*a != *b && *a != PL_fold[*b])
        return 0;
    a++,b++;
}
```

```rust
// 現在の出力（誤り）
// /* TODO: Discriminant(4) */
//     return 1;

// 期待する出力
while { let _t = len; len -= 1; _t } != 0 {
    if ((*a != *b) && (*a != PL_fold[(*b as usize)])) != 0 {
        return 0;
    }
    { a += 1; b += 1; };
}
return 1;
```

## 原因

`stmt_to_rust_inline` が `Stmt::While` を処理していない。

### stmt_to_rust_inline の現在の処理

```rust
fn stmt_to_rust_inline(&self, stmt: &Stmt, indent: &str) -> String {
    match stmt {
        Stmt::Expr(Some(expr), _) => { ... }
        Stmt::Expr(None, _) => { ... }
        Stmt::Return(Some(expr), _) => { ... }
        Stmt::Return(None, _) => { ... }
        Stmt::If { cond, then_stmt, else_stmt, .. } => { ... }
        Stmt::Compound(compound) => { ... }
        Stmt::DoWhile { body, cond, .. } => { ... }
        _ => format!("{}/* TODO: {:?} */", indent, std::mem::discriminant(stmt))
    }
}
```

## Stmt::While の構造

```rust
While {
    cond: Box<Expr>,
    body: Box<Stmt>,
    loc: SourceLocation,
},
```

## 解決策

### C の while 文

```c
while (cond) { body }
```

### Rust 変換

```rust
while cond != 0 { body }
```

### 後置デクリメントの処理

C の `while (len--)` パターンは `expr_to_rust_inline` で適切に変換される:

```rust
// len-- は以下に変換される
{ let _t = len; len -= 1; _t }

// したがって while (len--) は:
while { let _t = len; len -= 1; _t } != 0 {
    body
}
```

これは有効な Rust 構文であり、ブロック式が古い `len` の値を返し、
それがゼロでないかを比較する。

### 実装

```rust
Stmt::While { cond, body, .. } => {
    let cond_str = self.expr_to_rust_inline(cond);
    let mut result = format!("{}while {} != 0 {{\n", indent, cond_str);
    let nested_indent = format!("{}    ", indent);
    result.push_str(&self.stmt_to_rust_inline(body, &nested_indent));
    result.push_str("\n");
    result.push_str(&format!("{}}}", indent));
    result
}
```

## 変更対象ファイル

| ファイル | 変更内容 |
|----------|----------|
| `src/rust_codegen.rs` | `RustCodegen::stmt_to_rust_inline` に `While` 処理を追加 |
| `src/rust_codegen.rs` | `CodegenDriver::stmt_to_rust_inline` に `While` 処理を追加 |

## テスト

1. `cargo build` でビルド確認
2. `cargo test` で既存テストが通ることを確認
3. `--gen-rust` で以下を確認:
   - `Perl_foldEQ` の `/* TODO: Discriminant(4) */` が消える
   - `while (len--)` が `while { let _t = len; len -= 1; _t } != 0` として出力される

## 影響を受ける関数

以下の関数が while 文を含む:

- `Perl_foldEQ`
- `Perl_foldEQ_latin1`
- `Perl_foldEQ_locale`
- その他

## 将来の拡張

以下の文も同様に処理が必要:

| 文種別 | Rust 変換 | 状態 |
|--------|-----------|------|
| `While` | `while cond != 0 { body }` | 今回実装 |
| `For` | `for` または `loop` | 未実装 |
| `Switch` | `match` (複雑) | 未実装 |
| `Goto` / `Label` | サポート困難 | 未実装 |
| `Continue` / `Break` | そのまま | 未実装 |
