# Plan: E0308 Phase 4 — 自家生成関数の bool 型追跡

## Context

Phase 3 (完了) で E0308 を 652→629 に削減（total: 944→921）。
Phase 3 では `bindings.rs` の関数シグネチャを参照して bool 変換を行ったが、
**自家生成関数**（inline/macro）の型情報は参照できていない。

残り 629 件の E0308 のうち `expected bool, found integer` が 60 件で依然最大カテゴリ。
その内訳:

| サブパターン | 件数 | 原因 |
|-------------|------|------|
| `(generated_fn(x)) != 0` | ~35 | 自家生成関数が bool を返すのに `!= 0` が残る |
| 関数内部の `(flags & MASK)` を bool で返す | ~11 | `HvHasAUX` 等の内部エラー |
| 関数引数に整数リテラル `1`/`0` | ~8 | `Perl_SvTRUE_common(..., 1)` 等 |
| その他 | ~6 | `char` リテラル、変数 |

### 根本原因

Phase 3 の `is_bool_expr_with_dict()` は `rust_decl_dict`（`bindings.rs`）のみを
参照するが、生成関数は `bindings.rs` に含まれない。`RustCodegen` は以下を
持っているが bool 判定に使っていない:

1. **`macro_ctx: &MacroInferContext`** — マクロの推論結果（戻り値型あり）
2. **`current_return_type`** — 現在生成中の関数の戻り値型（内部エラー修正に使用可能）

### 追加で対処すべきサブカテゴリ

Phase 4 では bool 以外の上位サブカテゴリにも着手する:

| サブカテゴリ | 件数 | 内容 |
|-------------|------|------|
| `i8 ← char` | 8 | C の char リテラル `'0'` が Rust では `char` 型になる |
| `() ← u32` | 5 | 戻り値型 `()` の関数で値を返す |

## 変更内容

### 変更 A: `is_bool_expr_with_dict()` に自家生成関数の戻り値型を追加

`MacroInferContext` からマクロ関数の戻り値型が `bool` かを判定する。

```rust
fn is_bool_expr_with_dict(&self, expr: &Expr) -> bool {
    if is_boolean_expr_recursive(expr, self.interner) {
        return true;
    }
    if let ExprKind::Call { func, .. } = &expr.kind {
        if let ExprKind::Ident(name) = &func.kind {
            let func_name = self.interner.get(*name);
            // 1. bindings.rs の関数
            if let Some(ret_ty) = self.get_callee_return_type(func_name) {
                return ret_ty == "bool";
            }
            // 2. 自家生成マクロ関数の戻り値型
            if let Some(macro_info) = self.macro_ctx.macros.get(name) {
                if let Some(ty) = macro_info.get_return_type() {
                    let rust_ty = ty.to_rust_string(self.interner);
                    return rust_ty == "bool";
                }
            }
        }
    }
    false
}
```

**期待効果**: `(HvHasAUX(hv)) != 0` → `HvHasAUX(hv)` など、~35 件の解消。

### 変更 B: `expr_to_rust_arg()` / inline Call に自家生成関数の引数型も追加

`get_callee_param_type()` を拡張して、`macro_ctx` のマクロパラメータ型も参照する。
MacroInferInfo の `type_env` から各パラメータの型を取得可能。

```rust
fn get_callee_param_type(&self, func_name: &str, arg_index: usize) -> Option<&str> {
    // 1. bindings.rs
    if let Some(ty) = self.rust_decl_dict
        .and_then(|d| d.fns.get(func_name))
        .and_then(|f| f.params.get(arg_index))
        .map(|p| p.ty.as_str())
    {
        return Some(ty);
    }
    // 2. 自家生成マクロ（macro_ctx からパラメータ型を取得）
    // MacroInferInfo の type_env にパラメータ型が格納されている
    // → to_rust_string() で型文字列に変換して "bool" を判定
    None // 具体的な型取得方法は実装時に確認
}
```

**期待効果**: `Perl_SvTRUE_common(my_perl, sv, 1)` → `...true` など、~8 件の解消。

### 変更 C: 関数戻り値型 bool での `(flags & MASK)` 自動変換

`current_return_type` が `"bool"` の場合、return 文の式が `(flags & MASK)` パターンなら
`(flags & MASK) != 0` に自動変換する。

具体的には `generate_macro()` / `generate_inline_fn()` の return 文処理で:

```rust
// 戻り値型が bool で、式が bool を返さない場合
if self.current_return_type.as_deref() == Some("bool") {
    if !self.is_bool_expr_with_dict(expr) {
        let e = self.expr_to_rust(expr, info);
        return format!("return (({}) != 0);", e);
    }
}
```

実際には return 文だけでなく、暗黙の最終式（ブロック末尾）にも適用が必要。
既存の return 文処理箇所（macro: L1889, inline: L2331）に追加する。

**期待効果**: `HvHasAUX` の関数内部エラー ~11 件の解消。

### 変更 D: C char リテラル `'x'` → `b'x'` 変換

C の `char` 型は `i8`/`u8` だが、Rust の `'x'` は `char` 型（4バイト Unicode）。
C コードで char リテラルが使われる場合、`b'x'` に変換する必要がある。

`expr_to_rust()` / `expr_to_rust_inline()` の `ExprKind::CharLit` ハンドラを修正:

```rust
ExprKind::CharLit(c) => {
    // ASCII 文字は b'x' (u8) として出力
    if c.is_ascii() {
        format!("b'{}'", escape_char(*c))
    } else {
        format!("'{}'", escape_char(*c))
    }
}
```

ただし比較先の型が `i8` の場合は `b'x' as i8` が必要。
これは比較演算子のハンドラで型を見て調整するか、
常に `b'x' as i8` を出力するかを検討する。

**期待効果**: `i8 ← char` エラー 8 件 + `i32 ← char` 5 件 = ~13 件の解消。

## 変更ファイル

| ファイル | 変更箇所 |
|----------|----------|
| `src/rust_codegen.rs` | `is_bool_expr_with_dict()`: macro_ctx の戻り値型参照追加 |
| `src/rust_codegen.rs` | `get_callee_param_type()`: macro_ctx のパラメータ型参照追加 |
| `src/rust_codegen.rs` | return 文の bool 自動変換（macro + inline） |
| `src/rust_codegen.rs` | `CharLit` ハンドラ: `'x'` → `b'x' as i8` 変換 |

## 検証

```bash
# 1. 全テスト通過
cargo test

# 2. gen-rust stats が悪化しないこと
cargo run -- samples/xs-wrapper.h --auto --gen-rust --bindings samples/bindings.rs 2>&1 | tail -5

# 3. 統合ビルドテスト
~/blob/libperl-rs/12-macrogen-2-build.zsh
grep -c 'error\[E0308\]' tmp/build-error.log
# 期待: 629 → ~562 (約 67 件減少)

# 4. bool/integer の確認
grep -c 'expected.*bool.*found.*integer' tmp/build-error.log
# 期待: 60 → ~6

# 5. i8/char の確認
grep -c 'expected `i8`, found `char`' tmp/build-error.log
# 期待: 8 → 0
```

## 実装順序

1. **変更 A** (最大効果: ~35 件) — `is_bool_expr_with_dict` の macro_ctx 拡張
2. **変更 C** (~11 件) — return 文の bool 自動変換
3. **変更 B** (~8 件) — 引数型の macro_ctx 拡張
4. **変更 D** (~13 件) — char リテラル変換

変更 A と D は独立しており並行実装可能。
変更 C は A に依存（`is_bool_expr_with_dict` の改善後に正確に動作する）。

## 残存見込み

Phase 4 後の E0308 見込み: 629 - 67 = ~562 件
残存する主要カテゴリ:

| カテゴリ | 件数 | 備考 |
|---------|------|------|
| i32 ← u32 | 36 | 整数幅の不一致（`as` キャスト必要） |
| usize ← u32 | 28 | 同上 |
| u32 ← u64 | 25 | 1u64 リテラルが u32 フィールドに代入 |
| u8 ← u32 | 21 | 同上 |
| *mut SV ← *mut GV/HV | 30 | Perl の SV サブタイプキャスト |
| Other | ~360 | 多様なパターン |
