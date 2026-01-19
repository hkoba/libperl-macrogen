# inline 関数の StmtExpr 対応

## 問題

inline 関数のコード生成で `/* TODO: Discriminant(29) */` が出力される。
`Discriminant(29)` は `ExprKind::StmtExpr`（GCC ステートメント式）。

### 例: CvNAME_HEK

```c
// C の定義
PERL_STATIC_INLINE HEK *
CvNAME_HEK(CV *sv)
{
    return CvNAMED(sv)
        ? ((XPVCV*)MUTABLE_PTR(SvANY(sv)))->xcv_gv_u.xcv_hek
        : 0;
}

// MUTABLE_PTR の展開
#define MUTABLE_PTR(p) ({ void *p_ = (p); p_; })
```

### 現在の出力

```rust
// [CODEGEN_INCOMPLETE] CvNAME_HEK - inline function
// pub unsafe fn CvNAME_HEK(sv: *mut CV) -> HEK {
//     return (if ((*(/* TODO: Discriminant(29) */ as *mut XPVCV)).xcv_flags & 32768) != 0 { ... } else { 0 });
// }
```

## 原因

`RustCodegen::expr_to_rust_inline` メソッド（line 936-1066）に
`ExprKind::StmtExpr` のケースがない。

`expr_to_rust` メソッドには既に対応があり、MUTABLE_PTR パターンの検出も行っている。

## 解決策

`RustCodegen::expr_to_rust_inline` に `ExprKind::StmtExpr` のケースを追加。

### 実装

`expr_to_rust` の実装を参考に、以下を追加:

```rust
ExprKind::StmtExpr(compound) => {
    // MUTABLE_PTR パターンを検出: ({ void *p_ = (expr); p_; }) => expr
    if let Some(init_expr) = self.detect_mutable_ptr_pattern(compound) {
        return self.expr_to_rust_inline(init_expr);
    }

    // 通常の statement expression: Rust のブロック式として出力
    let mut parts = Vec::new();
    for item in &compound.items {
        match item {
            BlockItem::Stmt(Stmt::Expr(Some(e), _)) => {
                parts.push(self.expr_to_rust_inline(e));
            }
            BlockItem::Stmt(stmt) => {
                parts.push(self.stmt_to_rust_inline(stmt, ""));
            }
            BlockItem::Decl(_) => {
                // 宣言はスキップ（TODO: ローカル変数対応が必要な場合は追加）
            }
        }
    }
    if parts.is_empty() {
        "{ }".to_string()
    } else if parts.len() == 1 {
        parts.pop().unwrap()
    } else {
        let last = parts.pop().unwrap();
        let stmts = parts.join("; ");
        format!("{{ {}; {} }}", stmts, last)
    }
}
```

## 変更対象ファイル

| ファイル | 変更内容 |
|----------|----------|
| `src/rust_codegen.rs` | `RustCodegen::expr_to_rust_inline` に `ExprKind::StmtExpr` ケースを追加 |

## 期待する出力

```rust
/// CvNAME_HEK - inline function
#[inline]
pub unsafe fn CvNAME_HEK(sv: *mut CV) -> HEK {
    return (if (((*(SvANY(sv) as *mut XPVCV)).xcv_flags) & 32768) != 0 {
        ((*(SvANY(sv) as *mut XPVCV)).xcv_gv_u).xcv_hek
    } else {
        0
    });
}
```

MUTABLE_PTR パターンが検出され、`({ void *p_ = (expr); p_; })` が `expr` に簡約される。

## テスト

1. `cargo build` でビルド確認
2. `cargo test` で既存テストが通ることを確認
3. `--gen-rust` で CvNAME_HEK が正しく生成されることを確認
