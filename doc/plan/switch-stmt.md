# switch 文の Rust 変換

## 問題

inline 関数のコード生成で `/* TODO: Discriminant(3) */` が出力される。
`Discriminant(3)` は `Stmt::Switch`（switch 文）。

### 例: zaphod32_hash_with_state

```c
switch (len) {
    default: goto zaphod32_read8;
    case 12: v2 += (U32)key[11] << 24;  /* FALLTHROUGH */
    case 11: v2 += (U32)key[10] << 16;  /* FALLTHROUGH */
    // ...
    case 1: v0 += (U32)key[0];
            break;
    case 0: v2 ^= 0xFF;
            break;
}
```

## C switch 文の複雑さ

C の switch 文は以下の特徴があり、Rust の match への変換が困難：

1. **Fall-through**: break がないと次の case に流れる
2. **Goto**: case 内から goto でジャンプ可能
3. **Default 位置**: default は任意の位置に配置可能
4. **複数 case ラベル**: 同じコードに複数の case を付けられる

## Stmt::Switch の構造

```rust
Switch {
    expr: Box<Expr>,
    body: Box<Stmt>,  // 通常は Compound
    loc: SourceLocation,
}

Case {
    expr: Box<Expr>,
    stmt: Box<Stmt>,
    loc: SourceLocation,
}

Default {
    stmt: Box<Stmt>,
    loc: SourceLocation,
}
```

## 解決策

### 基本方針

1. Switch body から Case/Default を抽出
2. Rust の match 式として出力
3. Fall-through は明示的にコメントで警告
4. Goto は Rust の unsafe ブロック内で対処（または TODO コメント）

### 実装

```rust
Stmt::Switch { expr, body, .. } => {
    let expr_str = self.expr_to_rust_inline(expr);
    let mut result = format!("{}match {} {{\n", indent, expr_str);
    let nested_indent = format!("{}    ", indent);

    // body から Case/Default を収集
    if let Stmt::Compound(compound) = body.as_ref() {
        for item in &compound.items {
            if let BlockItem::Stmt(stmt) = item {
                match stmt {
                    Stmt::Case { expr: case_expr, stmt: case_stmt, .. } => {
                        let case_val = self.expr_to_rust_inline(case_expr);
                        result.push_str(&format!("{}{} => {{\n", nested_indent, case_val));
                        let body_indent = format!("{}    ", nested_indent);
                        result.push_str(&self.stmt_to_rust_inline(case_stmt, &body_indent));
                        result.push_str("\n");
                        result.push_str(&format!("{}}}\n", nested_indent));
                    }
                    Stmt::Default { stmt: default_stmt, .. } => {
                        result.push_str(&format!("{}_ => {{\n", nested_indent));
                        let body_indent = format!("{}    ", nested_indent);
                        result.push_str(&self.stmt_to_rust_inline(default_stmt, &body_indent));
                        result.push_str("\n");
                        result.push_str(&format!("{}}}\n", nested_indent));
                    }
                    _ => {
                        // Case/Default 以外の文は出力
                        result.push_str(&self.stmt_to_rust_inline(stmt, &nested_indent));
                        result.push_str("\n");
                    }
                }
            }
        }
    }

    result.push_str(&format!("{}}}", indent));
    result
}
```

## 制限事項

1. **Fall-through**: Rust では非サポート。各 case は独立したアームとして出力
2. **Goto**: `goto label;` は別途 Label 対応が必要
3. **複数 case ラベル**: `case 1: case 2: ...` は `1 | 2 =>` に変換すべきだが、現状は個別出力

## 変更対象ファイル

| ファイル | 変更内容 |
|----------|----------|
| `src/rust_codegen.rs` | `RustCodegen::stmt_to_rust_inline` に `Switch` 処理を追加 |
| `src/rust_codegen.rs` | `CodegenDriver::stmt_to_rust_inline` に `Switch` 処理を追加 |

## テスト

1. `cargo build` でビルド確認
2. `cargo test` で既存テストが通ることを確認
3. `--gen-rust` で `/* TODO: Discriminant(3) */` が消えることを確認

## 将来の改善

- Fall-through パターンの検出と適切な変換
- 複数 case ラベルの `|` パターンへの変換
- Goto/Label のループラベル変換
