# Plan: Phase 4〜6 — codegen 改善（Tier 4 除外）

## Context

Phase 3 完了後（commit `c56630c`）のエラー数: 1,813。
`doc/plan/build-error-analysis-phase3-done.md` の分析に基づき、
**Tier 4（名前未解決, E0425, ~190 エラー）を除外**して、残りの体系的エラーに対処する。

| Phase | 対象 Tier | 概要 | 推定エラー削減 |
|-------|-----------|------|---------------|
| 4 | Tier 5+6+7 | lvalue Call展開 + GvGP型推論 + as bool | ~94 |
| 5 | Tier 1 | ポインタ算術基盤（TypeHint） | ~449 |
| 6 | Tier 2+3C | 整数幅キャスト + bool引数変換 | ~231 |

---

## Phase 4: 単独修正可能な3件（~94 エラー）

### Step 4-A: lvalue Call 展開（~49 エラー, E0067+E0070 残余）

**ファイル**: `src/rust_codegen.rs`

**問題**: Phase 3 で `ExprKind::MacroCall` の LHS 展開は実装済みだが、
`ExprKind::Call` 形式で出力されるマクロ呼び出し（`GvFLAGS(gv)`, `CopLINE(c)` 等）が
代入 LHS に来るとき、関数呼び出しのまま出力されてエラーになる。

`ExprKind::Call` は `expanded` フィールドを持たないため、
codegen 時に `macro_ctx.macros` からマクロ本体を取得して再展開する必要がある。

**方針**: Assign/PreInc/PreDec/PostInc/PostDec ハンドラで LHS が `Call` かつ
`should_emit_as_macro_call(func_name)` が true の場合、
`macro_ctx` から `ParseResult::Expression` を取得し、
マクロパラメータを実引数に置換した展開式を codegen に渡す。

#### ヘルパーメソッド追加:

```rust
/// Call 式が lvalue マクロ呼び出しなら、展開済み lvalue 文字列を返す
fn try_expand_call_as_lvalue(&mut self, func: &Expr, args: &[Expr], info: &MacroInferInfo) -> Option<String> {
    if let ExprKind::Ident(name) = &func.kind {
        if self.should_emit_as_macro_call(*name) {
            if let Some(macro_info) = self.macro_ctx.macros.get(name) {
                if let ParseResult::Expression(body) = &macro_info.parse_result {
                    // マクロ本体を codegen する際、パラメータを実引数に置換
                    // パラメータ名 → 実引数文字列のマッピングを作成
                    let saved_params = std::mem::take(&mut self.param_substitutions);
                    for (i, param) in macro_info.params.iter().enumerate() {
                        if let Some(arg) = args.get(i) {
                            let arg_str = self.expr_to_rust(arg, info);
                            self.param_substitutions.insert(param.name, arg_str);
                        }
                    }
                    let result = self.expr_to_rust(body, info);
                    self.param_substitutions = saved_params;
                    return Some(result);
                }
            }
        }
    }
    None
}
```

**注**: `param_substitutions: HashMap<InternedStr, String>` を `RustCodegen` に追加し、
`expr_to_rust` の `ExprKind::Ident` ハンドラで置換テーブルを参照する。

#### Assign ハンドラ修正:

```rust
ExprKind::Assign { op, lhs, rhs } => {
    let l = if let ExprKind::MacroCall { expanded, .. } = &lhs.kind {
        self.expr_to_rust(expanded, info)
    } else if let ExprKind::Call { func, args } = &lhs.kind {
        self.try_expand_call_as_lvalue(func, args, info)
            .unwrap_or_else(|| self.expr_to_rust(lhs, info))
    } else {
        self.expr_to_rust(lhs, info)
    };
    // ... 以下同じ
}
```

PreInc/PreDec/PostInc/PostDec も同様のパターンで修正。
`expr_to_rust_inline` 側と `stmt_to_rust_inline` 側も同様。

### Step 4-B: GvGP 型推論修正（~27 エラー, E0614）

**ファイル**: `src/semantic.rs`

**問題**: `usual_arithmetic_conversion_str` のランク表でポインタ型（`*mut GP` 等）が
rank 0 にフォールスルーする。`0 + *mut GP` で int が選ばれ、
GvGP の戻り値型が `c_int` になる。

**解決**: `usual_arithmetic_conversion_str` でポインタ型を検出し、
ポインタ型が含まれる場合はポインタ型を優先して返す。

```rust
fn usual_arithmetic_conversion_str(&self, lhs: &str, rhs: &str) -> String {
    // ポインタ型が含まれる場合はポインタ型を返す
    let is_ptr = |ty: &str| ty.contains('*');
    if is_ptr(lhs) && !is_ptr(rhs) {
        return lhs.to_string();
    }
    if is_ptr(rhs) && !is_ptr(lhs) {
        return rhs.to_string();
    }
    // 既存のランク表による型選択
    // ...
}
```

### Step 4-C: `as bool` → `!= 0` 変換（~18 エラー, E0054）

**ファイル**: `src/rust_codegen.rs`

**問題**: Cast ハンドラ（L1013）が `(expr as bool)` を出力するが、
Rust では整数型を `as bool` でキャストできない。

**解決**: Cast ハンドラでターゲット型が `bool` の場合、`(expr != 0)` に変換。

```rust
ExprKind::Cast { type_name, expr: inner } => {
    let t = self.type_name_to_rust(type_name);
    if t == "bool" {
        let e = self.expr_to_rust(inner, info);
        return format!("(({}) != 0)", e);
    }
    // ... 既存のロジック
}
```

`expr_to_rust_inline` 側も同様。

---

## Phase 5: ポインタ算術基盤（~449 エラー）

### 概要

C のポインタ算術（`p + n`, `p - q`, `p += n`, `p != 0` 等）を
Rust の `.offset()`, `.offset_from()`, `.is_null()` 等に変換する。

`expr_to_rust` / `expr_to_rust_inline` の返り値に型ヒントを追加する
大規模リファクタが必要。

### Step 5-A: TypeHint 設計・基盤

`expr_to_rust` の返り値を `(String, TypeHint)` タプルにする代わりに、
**出力文字列はそのまま**で、式の型情報を補助的に取得するヘルパーを用いる。

```rust
/// 式の型ヒントを推定する（codegen 用の簡易版）
enum TypeHint {
    Pointer,   // *mut T / *const T
    Integer,   // i32, u32, usize, etc.
    Bool,
    Unknown,
}

fn infer_type_hint(&self, expr: &Expr, info: &MacroInferInfo) -> TypeHint {
    match &expr.kind {
        ExprKind::IntLit(_) => TypeHint::Integer,
        ExprKind::Cast { type_name, .. } => {
            let t = self.type_name_to_rust(type_name);
            if is_pointer_type_str(&t) { TypeHint::Pointer }
            else if t == "bool" { TypeHint::Bool }
            else { TypeHint::Integer }
        }
        ExprKind::Ident(name) => {
            // パラメータの型制約を参照
            if let Some(constraints) = info.type_env.param_constraints.get(name) {
                // ...型文字列からヒントを判定
            }
            TypeHint::Unknown
        }
        ExprKind::Call { func, .. } | ExprKind::MacroCall { name, .. } => {
            // macro_ctx / rust_decl_dict から戻り値型を参照
            TypeHint::Unknown
        }
        ExprKind::PtrMember { .. } | ExprKind::Deref(_) => TypeHint::Unknown,
        // ... 他のケース
        _ => TypeHint::Unknown,
    }
}
```

**利点**: `expr_to_rust` の返り値を変更しないため影響範囲が限定的。
**制限**: 100% の精度は期待できないが、主要パターンはカバー可能。

### Step 5-B: ptr ± int → `.offset()` 変換（~201 エラー, E0369+E0368）

Binary ハンドラで、一方がポインタ型・他方が整数型の場合：
- `ptr + int` → `ptr.offset(int as isize)`
- `ptr - int` → `ptr.offset(-(int as isize))`
- `ptr += int` → `ptr = ptr.offset(int as isize)`（Assign ハンドラ）

### Step 5-C: ptr == 0 → `.is_null()` 変換（~208 エラー, E0308 subset）

Binary ハンドラの比較演算で、一方がポインタ型・他方が `0` リテラルの場合：
- `ptr != 0` → `!ptr.is_null()`
- `ptr == 0` → `ptr.is_null()`

### Step 5-D: ptr - ptr → `.offset_from()` 変換（~22 エラー）

両方がポインタ型の減算：
- `ptr1 - ptr2` → `ptr1.offset_from(ptr2)`

### Step 5-E: `-usize` → wrapping 変換（~28 エラー, E0600）

`UnaryMinus` ハンドラで、内部式が unsigned 型と推定される場合：
- `-(x as usize)` → `(x as usize).wrapping_neg()`

---

## Phase 6: 整数幅キャスト + bool 引数変換（~231 エラー）

### Step 6-A: 整数幅不一致の `as` キャスト挿入（~120 エラー, E0277）

Phase 5 の TypeHint を利用し、Binary ハンドラで両方が整数型だが
幅が異なる場合にキャスト挿入。

リテラル整数の場合はサフィックスを付加（`1u32`, `0xff_u64` 等）。

### Step 6-B: bool/integer 引数変換（~111 エラー, E0308 subset）

Call/MacroCall ハンドラで、`rust_decl_dict` の関数シグネチャから
パラメータの型が `bool` であることが分かる場合：
- 整数リテラル `1` → `true`, `0` → `false`
- 整数式 `expr` → `(expr) != 0`

---

## 変更ファイル一覧

| Phase | ファイル | 変更内容 |
|-------|----------|----------|
| 4-A | `src/rust_codegen.rs` | `param_substitutions` フィールド、`try_expand_call_as_lvalue`、Assign/Inc/Dec 修正 |
| 4-B | `src/semantic.rs` | `usual_arithmetic_conversion_str` のポインタ型対応 |
| 4-C | `src/rust_codegen.rs` | Cast ハンドラの `bool` 特殊化 |
| 5 | `src/rust_codegen.rs` | `TypeHint`, `infer_type_hint`, Binary/Assign/UnaryMinus 修正 |
| 6 | `src/rust_codegen.rs` | 整数キャスト挿入、bool 引数変換 |

## 検証

各 Phase 完了後に：

1. `cargo build && cargo test`
2. `cargo test rust_codegen_regression`
3. 統合ビルド: `~/blob/libperl-rs/12-macrogen-2-build.zsh`
4. `tmp/build-error.log` のエラー数確認（`grep -c '^error' tmp/build-error.log`）

### 目標エラー数

| Phase | 目標 |
|-------|------|
| Phase 4 完了後 | ~1,719 (-94) |
| Phase 5 完了後 | ~1,270 (-449) |
| Phase 6 完了後 | ~1,039 (-231) |
