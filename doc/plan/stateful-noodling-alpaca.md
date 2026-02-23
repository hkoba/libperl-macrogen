# Plan: Phase 2 — 型システム変換の根本改修

## Context

`libperl-macrogen` の生成コードを `libperl-sys` に統合するにあたり、
C と Rust の型システムの違いに起因するコンパイルエラーが ~1,143 件発生している。
Phase 1 で低コスト修正（c_uchar, GCC builtins, MUTABLE_PTR, goto 除外）を完了し、
1,929→1,895 エラーまで削減した。

Phase 2 では codegen レイヤー（`rust_codegen.rs`）に体系的な型変換ルールを追加し、
最大 ~900 エラーの削減を目指す。

## 設計方針

### 戻り値型コンテキストの伝搬

現在 `expr_to_rust` / `expr_to_rust_inline` は型コンテキストを受け取らないため、
`0` を `null_mut()` や `false` に変換できない。

**アプローチ**: `RustCodegen` 構造体に `current_return_type: Option<String>` フィールドを追加。
関数生成開始時にセットし、`return` 文・条件式・トップレベル式で参照する。

シグネチャ変更なし → 既存の全呼び出しサイトに影響しない。

## 実装ステップ

### Step 1: 基盤 — `current_return_type` フィールド追加

**ファイル**: `src/rust_codegen.rs`

`RustCodegen` 構造体（L326-341）に追加:
```rust
current_return_type: Option<String>,
```

セットする箇所:
- `generate_macro` (L502): `self.current_return_type = Some(return_type.clone())`
- `generate_inline_fn` (L1305): `self.current_return_type = Some(return_type.clone())`

ヘルパー関数（モジュールレベル）:
```rust
fn is_pointer_type_str(ty: &str) -> bool {
    ty.starts_with("*mut ") || ty.starts_with("*const ") || ty == "*mut c_void"
}
fn is_const_pointer_type_str(ty: &str) -> bool {
    ty.starts_with("*const ")
}
```

### Step 2: Enum キャスト → transmute (~109 エラー, E0605)

**ファイル**: `src/enum_dict.rs`, `src/rust_codegen.rs`

`EnumDict` に追加:
```rust
pub fn is_target_enum(&self, name: InternedStr) -> bool {
    self.target_enums.contains(&name)
}
```

`ExprKind::Cast` ハンドラ（`expr_to_rust` L911, `expr_to_rust_inline` L1934）で:
```rust
// TypeSpec::TypedefName が enum_dict の target_enum なら transmute
if self.is_enum_cast_target(type_name) {
    return format!("std::mem::transmute::<_, {}>({})", t, e);
}
// TypeSpec::Enum なら transmute
if self.is_explicit_enum_cast(type_name) {
    return format!("std::mem::transmute::<_, {}>({})", t, e);
}
```

ヘルパーメソッド:
```rust
fn is_enum_cast_target(&self, type_name: &TypeName) -> bool {
    for spec in &type_name.specs.type_specs {
        match spec {
            TypeSpec::TypedefName(name) => return self.enum_dict.is_target_enum(*name),
            TypeSpec::Enum(_) => return true,
            _ => {}
        }
    }
    false
}
```

### Step 3: `as bool` → `!= 0` (~17 エラー, E0054)

**ファイル**: `src/rust_codegen.rs`

同じ `ExprKind::Cast` ハンドラで、`t == "bool"` の場合:
```rust
if t == "bool" {
    return format!("(({}) != 0)", e);
}
```

これを enum チェックの後、既存の `as` キャスト生成の前に挿入。

### Step 4: NULL リテラル → null_mut()/null() (~265 エラー, E0308 の主要部分)

**ファイル**: `src/rust_codegen.rs`

NULL 判定ヘルパー:
```rust
fn is_null_literal(expr: &Expr) -> bool {
    matches!(expr.kind, ExprKind::IntLit(0))
}
```

変換ヘルパー:
```rust
fn null_ptr_expr(return_type: &str) -> String {
    if is_const_pointer_type_str(return_type) {
        "std::ptr::null()".to_string()
    } else {
        "std::ptr::null_mut()".to_string()
    }
}
```

適用箇所 (4 箇所 × 2 コードパス = 最大 8 箇所):

**A. `Stmt::Return`** (`stmt_to_rust` L1086, `stmt_to_rust_inline` L1485):
```rust
Stmt::Return(Some(expr), _) => {
    if let Some(ref rt) = self.current_return_type {
        if is_pointer_type_str(rt) && is_null_literal(expr) {
            return format!("return {};", null_ptr_expr(rt));
        }
    }
    format!("return {};", self.expr_to_rust(expr, info))
}
```

**B. `ExprKind::Conditional`** (`expr_to_rust` L978, `expr_to_rust_inline` L1991):
```rust
ExprKind::Conditional { cond, then_expr, else_expr } => {
    let c = self.expr_to_rust(cond, info);
    let cond_str = wrap_as_bool_condition(cond, &c);
    let type_hint = self.current_return_type.clone();
    let t = self.expr_with_null_hint(then_expr, info, type_hint.as_deref());
    let e = self.expr_with_null_hint(else_expr, info, type_hint.as_deref());
    format!("(if {} {{ {} }} else {{ {} }})", cond_str, t, e)
}
```

ヘルパー:
```rust
fn expr_with_null_hint(&mut self, expr: &Expr, info: &MacroInferInfo, type_hint: Option<&str>) -> String {
    if let Some(ty) = type_hint {
        if is_pointer_type_str(ty) && is_null_literal(expr) {
            return null_ptr_expr(ty);
        }
    }
    self.expr_to_rust(expr, info)
}
// expr_to_rust_inline 用も同様
fn expr_with_null_hint_inline(&mut self, expr: &Expr, type_hint: Option<&str>) -> String {
    if let Some(ty) = type_hint {
        if is_pointer_type_str(ty) && is_null_literal(expr) {
            return null_ptr_expr(ty);
        }
    }
    self.expr_to_rust_inline(expr)
}
```

**C. マクロのトップレベル式** (`generate_macro` L562):
```rust
ParseResult::Expression(expr) => {
    if let Some(ref rt) = self.current_return_type {
        if is_pointer_type_str(rt) && is_null_literal(expr) {
            self.writeln(&format!("{}{}", body_indent, null_ptr_expr(rt)));
        } else {
            let rust_expr = self.expr_to_rust(expr, info);
            self.writeln(&format!("{}{}", body_indent, rust_expr));
        }
    } else {
        let rust_expr = self.expr_to_rust(expr, info);
        self.writeln(&format!("{}{}", body_indent, rust_expr));
    }
}
```

### Step 5: Bool リテラル in return context (~50 エラー, E0308)

**ファイル**: `src/rust_codegen.rs`

Step 4 と同じ箇所に追加:

**A. `Stmt::Return`**:
```rust
if rt == "bool" {
    match &expr.kind {
        ExprKind::IntLit(0) => return format!("return false;"),
        ExprKind::IntLit(1) => return format!("return true;"),
        _ => {}
    }
}
```

**B. `ExprKind::Conditional`**: 同様の type_hint 伝搬で
`IntLit(0)` → `false`, `IntLit(1)` → `true`

**C. マクロのトップレベル式**: 同様

### Step 6: void 関数の末尾式処理 (~100 エラー, E0308)

**ファイル**: `src/rust_codegen.rs`

`generate_macro` (L561-566) で、`ParseResult::Expression` かつ return_type が `()` の場合:
```rust
ParseResult::Expression(expr) => {
    let rust_expr = self.expr_to_rust(expr, info);
    if self.current_return_type.as_deref() == Some("()") {
        // void 関数: 式の結果を捨てる
        self.writeln(&format!("{}{}; // void", body_indent, rust_expr));
    } else {
        self.writeln(&format!("{}{}", body_indent, rust_expr));
    }
}
```

## 変更ファイル一覧

| ファイル | 変更内容 |
|----------|----------|
| `src/rust_codegen.rs` | `current_return_type` フィールド、enum/bool/NULL/void 変換ロジック |
| `src/enum_dict.rs` | `is_target_enum()` メソッド追加 |

## 検証

1. `cargo build` / `cargo test`
2. 回帰テスト: `cargo test rust_codegen_regression`
3. 統合ビルド:
   ```bash
   ~/blob/libperl-rs/12-macrogen-2-build.zsh
   ```
   エラー数が 1,895 から大幅に減少すること（目標: ~1,000 以下）
4. 出力サンプル確認:
   ```bash
   cargo run -- --auto --gen-rust samples/xs-wrapper.h --bindings samples/bindings.rs 2>/dev/null \
     | grep -E 'null_mut|null\(\)|transmute|!= 0|false|true'
   ```

## Phase 2 でスコープ外とするもの

- **ポインタ算術** (2-5): 式の中間型追跡が必要。Phase 3 以降
- **整数幅の暗黙キャスト** (2-6): 同上
- **ビットフィールド getter** (2-D): bindings.rs 解析が必要。Phase 3 以降
