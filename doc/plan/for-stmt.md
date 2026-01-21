# for 文の Rust 変換

## 問題

inline 関数のコード生成で `/* TODO: Discriminant(6) */` が出力される。
`Discriminant(6)` は `Stmt::For`（for 文）。

### 例: Perl_valid_utf8_to_uvchr

```c
// C の定義
for (++s; s < send; s++) {
    uv = UTF8_ACCUMULATE(uv, *s);
}
```

```rust
// 現在の出力（誤り）
// /* TODO: Discriminant(6) */

// 期待する出力
{
    { let _t = s; s += 1; _t };  // ++s (init)
    while (s < send) != 0 {
        uv = UTF8_ACCUMULATE(uv, *s);
        { let _t = s; s += 1; _t };  // s++ (step)
    }
}
```

## 原因

`stmt_to_rust_inline` が `Stmt::For` を処理していない。

## Stmt::For の構造

```rust
For {
    init: Option<ForInit>,
    cond: Option<Box<Expr>>,
    step: Option<Box<Expr>>,
    body: Box<Stmt>,
    loc: SourceLocation,
},

pub enum ForInit {
    Expr(Box<Expr>),
    Decl(Declaration),
}
```

## 解決策

### C の for 文

```c
for (init; cond; step) { body }
```

### Rust 変換

Rust には C スタイルの for 文がないため、`loop` または `while` に変換する。

```rust
{
    init;  // 初期化（式または宣言）
    while cond != 0 {
        body;
        step;
    }
}
```

### 特殊ケース

1. **cond がない場合**: 無限ループ `loop { body; step; }`
2. **init がない場合**: 初期化部分を省略
3. **step がない場合**: ステップ部分を省略

### 実装

```rust
Stmt::For { init, cond, step, body, .. } => {
    let mut result = format!("{}{{\n", indent);
    let nested_indent = format!("{}    ", indent);

    // 初期化部分
    if let Some(for_init) = init {
        match for_init {
            ForInit::Expr(expr) => {
                result.push_str(&format!("{}{};\n", nested_indent, self.expr_to_rust_inline(expr)));
            }
            ForInit::Decl(decl) => {
                result.push_str(&self.decl_to_rust_let(decl, &nested_indent));
            }
        }
    }

    // ループ部分
    if let Some(cond_expr) = cond {
        let cond_str = self.expr_to_rust_inline(cond_expr);
        result.push_str(&format!("{}while {} != 0 {{\n", nested_indent, cond_str));
    } else {
        result.push_str(&format!("{}loop {{\n", nested_indent));
    }

    let body_indent = format!("{}    ", nested_indent);

    // ループ本体
    result.push_str(&self.stmt_to_rust_inline(body, &body_indent));
    result.push_str("\n");

    // ステップ部分
    if let Some(step_expr) = step {
        result.push_str(&format!("{}{};\n", body_indent, self.expr_to_rust_inline(step_expr)));
    }

    result.push_str(&format!("{}}}\n", nested_indent));
    result.push_str(&format!("{}}}", indent));
    result
}
```

## 変更対象ファイル

| ファイル | 変更内容 |
|----------|----------|
| `src/rust_codegen.rs` | `RustCodegen::stmt_to_rust_inline` に `For` 処理を追加 |
| `src/rust_codegen.rs` | `CodegenDriver::stmt_to_rust_inline` に `For` 処理を追加 |

## テスト

1. `cargo build` でビルド確認
2. `cargo test` で既存テストが通ることを確認
3. `--gen-rust` で以下を確認:
   - `Perl_valid_utf8_to_uvchr` の `/* TODO: Discriminant(6) */` が消える
   - `S_perl_hash_siphash_*` 関数の for 文が正しく変換される

## 影響を受ける関数

- `Perl_valid_utf8_to_uvchr`
- `S_perl_hash_siphash_1_3_with_state`
- `S_perl_hash_siphash_1_3_with_state_64`
- `S_perl_hash_siphash_2_4_with_state`
- `S_perl_hash_siphash_2_4_with_state_64`
