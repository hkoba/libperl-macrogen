# Plan: E0308 null pointer — Phase 1 ローカル変数型追跡 + inline Eq/Ne 変換

## Context

E0308 (mismatched types) エラー 687 件のうち 62 件が null pointer 関連。
codegen にはすでに `ptr != 0` → `!ptr.is_null()` 変換のロジックがあるが、
以下の 2 つの欠落により多くのケースで `(expr) != 0` がそのまま出力される:

1. **ローカル変数の型が追跡されない** — `collect_decl_names()` は名前だけ収集し、
   型情報を `current_param_types` に登録しない
2. **inline 関数に `Eq/Ne` → `.is_null()` 変換がない** — `expr_to_rust_inline()` の
   Binary ハンドラに `BinOp::Eq/Ne` のポインタ判定がなく、マクロ codegen
   (`expr_to_rust()` L1316-1334) にある変換と非対称

## 変更内容

### 変更 1: `collect_decl_names()` で型情報も収集

`src/rust_codegen.rs` L959-966

```rust
// 変更前
fn collect_decl_names(&mut self, decl: &Declaration) {
    for init_decl in &decl.declarators {
        if let Some(name) = init_decl.declarator.name {
            self.current_local_names.insert(name);
        }
    }
}

// 変更後
fn collect_decl_names(&mut self, decl: &Declaration) {
    let base_type = self.decl_specs_to_rust(&decl.specs);
    for init_decl in &decl.declarators {
        if let Some(name) = init_decl.declarator.name {
            self.current_local_names.insert(name);
            // ローカル変数の型も追跡（ポインタ検出用）
            let ty = self.apply_derived_to_type(&base_type, &init_decl.declarator.derived);
            self.current_param_types.insert(name, ty);
        }
    }
}
```

`current_param_types` にはすでにパラメータの型が格納されている (L2037-2038)。
ローカル変数の型も同じ HashMap に追加することで、
`is_pointer_expr_inline()` が自動的にローカル変数のポインタ型も検出する。

**効果**: `is_pointer_expr_inline()` の `Ident` 分岐 (L770-773) は
`current_param_types.get(name)` を参照するので、追加の変更は不要。
`wrap_as_bool_condition_inline()` 経由の LogAnd/LogOr でのポインタ検出が
ローカル変数にも効くようになる。

### 変更 2: `expr_to_rust_inline()` に `Eq/Ne` → `.is_null()` 変換を追加

`src/rust_codegen.rs` L2593-2604

```rust
// 変更前
ExprKind::Binary { op, lhs, rhs } => {
    let l = self.expr_to_rust_inline(lhs);
    let r = self.expr_to_rust_inline(rhs);
    match op {
        BinOp::LogAnd | BinOp::LogOr => { ... }
        _ => format!("({} {} {})", l, bin_op_to_rust(*op), r)
    }
}

// 変更後: Eq/Ne のポインタ判定を追加（マクロ codegen L1316-1334 と対称）
ExprKind::Binary { op, lhs, rhs } => {
    // ポインタ == 0 / != 0 → .is_null()
    if matches!(op, BinOp::Eq | BinOp::Ne) {
        if self.is_pointer_expr_inline(lhs) && is_null_literal(rhs) {
            let l = self.expr_to_rust_inline(lhs);
            return if *op == BinOp::Eq {
                format!("{}.is_null()", l)
            } else {
                format!("!{}.is_null()", l)
            };
        }
        if self.is_pointer_expr_inline(rhs) && is_null_literal(lhs) {
            let r = self.expr_to_rust_inline(rhs);
            return if *op == BinOp::Eq {
                format!("{}.is_null()", r)
            } else {
                format!("!{}.is_null()", r)
            };
        }
    }
    let l = self.expr_to_rust_inline(lhs);
    let r = self.expr_to_rust_inline(rhs);
    match op {
        BinOp::LogAnd | BinOp::LogOr => { ... 既存のまま ... }
        _ => format!("({} {} {})", l, bin_op_to_rust(*op), r)
    }
}
```

### 変更の相乗効果

| ケース | 変更 1 のみ | 変更 2 のみ | 両方 |
|--------|-----------|-----------|------|
| inline: `if (local_ptr != 0)` | LogAnd/LogOr 内のみ | パラメータのみ | ✓ |
| inline: `assert!(local_ptr != 0)` | assert のみ | パラメータのみ | ✓ |
| inline: `return (local_ptr != 0)` の比較 | ✗ | パラメータのみ | ✓ |
| macro: `(ptr) != 0` の比較 | ✗ (マクロはローカル変数なし) | ✗ (マクロ codegen は別パス) | ✗ |

マクロ codegen の null pointer エラーは主に構造体フィールドアクセス
(`PtrMember → TypeHint::Unknown`) に起因し、Phase 3 (FieldsDict) の対象。

## 変更ファイル

| ファイル | 変更箇所 |
|----------|----------|
| `src/rust_codegen.rs` | `collect_decl_names()` L959-966: 型情報収集の追加 |
| `src/rust_codegen.rs` | `expr_to_rust_inline()` L2593-2604: Eq/Ne → .is_null() 変換追加 |

## 検証

```bash
# 1. 全テスト通過
cargo test

# 2. gen-rust stats
cargo run -- samples/xs-wrapper.h --auto --gen-rust --bindings samples/bindings.rs 2>&1 | tail -5

# 3. 統合ビルドテスト
~/blob/libperl-rs/12-macrogen-2-build.zsh
grep -c 'error\[E0308\]' tmp/build-error.log
# 期待: E0308 が 687 から減少

# 4. null pointer 関連の確認
grep -A10 'error\[E0308\]' tmp/build-error.log | grep -c 'null pointer'
# 期待: 62 から減少
```
