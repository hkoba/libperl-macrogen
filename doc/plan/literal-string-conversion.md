# Plan: `&str` パラメータの適切な変換式生成

## 目標

`&str` 型パラメータを関数本体内で使用する際に、文脈に応じた変換コードを生成する。

## 背景

### 問題

apidoc の `"..."` 引数を `&str` 型にマッピングした結果、
関数本体でそのパラメータが C 関数に渡される箇所で型不一致エラーが発生する。

```rust
// 現在の出力（コンパイルエラー）
pub unsafe fn newSVpvs(my_perl: *mut PerlInterpreter, str: &str) -> *mut SV {
    unsafe {
        Perl_newSVpvn(my_perl, str, (std::mem::size_of_val(&str) - 1))
    }                      //   ^^^                         ^^^^
}                          // ① *const c_char 期待    ② fat pointer のサイズ
```

2つの問題がある:

1. **ポインタ変換**: `str` が `*const c_char` を期待する関数引数に渡される
   → `str.as_ptr()` に変換すべき

2. **サイズ計算**: `sizeof(str)` が `std::mem::size_of_val(&str)` に変換される
   → `&str` に対する `size_of_val` は fat pointer のサイズ（16バイト）を返す
   → `str.len()` に変換すべき
   → `sizeof(str) - 1` は `str.len()` に変換すべき（C では null 終端を含むため）

### 期待される出力

```rust
pub unsafe fn newSVpvs(my_perl: *mut PerlInterpreter, str: &str) -> *mut SV {
    unsafe {
        Perl_newSVpvn(my_perl, str.as_ptr(), str.len())
    }
}
```

### 影響パターン

| C コード | 現在の出力 | 期待される出力 |
|----------|------------|----------------|
| `fn(str)` | `fn(str)` | `fn(str.as_ptr())` |
| `sizeof(str)` | `size_of_val(&str)` | `str.len() + 1` |
| `sizeof(str) - 1` | `size_of_val(&str) - 1` | `str.len()` |

## 設計

### アプローチ

`current_type_param_map` と同じパターンで `current_literal_string_params` を
`RustCodegen` に追加し、`expr_to_rust` / `expr_to_rust_inline` の
各ハンドラで文脈に応じた変換を行う。

### 変換対象の文脈

#### 文脈1: 関数呼び出し引数

`ExprKind::Call` のハンドラで、引数が literal_string_param の Ident の場合、
`.as_ptr()` を付与する。

```
ExprKind::Call { func: Ident("Perl_newSVpvn"), args: [..., Ident("str"), ...] }
  → Perl_newSVpvn(my_perl, str.as_ptr(), ...)
```

ただし、呼び出し先が自身も `literal_string_params` を持つマクロ関数の場合は
`.as_ptr()` を付けない（`&str` をそのまま渡す）。

#### 文脈2: sizeof 式

`ExprKind::Sizeof(Ident("str"))` で、内部の式が literal_string_param の場合:
- `sizeof(str)` → `str.len() + 1`（null 終端分を含む C の sizeof と同等）

さらに、`sizeof(str) - 1` パターンを検出して `str.len()` に簡略化する。
これは `ExprKind::BinOp(Sub, Sizeof(Ident("str")), IntLit(1))` として検出可能。

#### 文脈3: 単独使用

`ExprKind::Ident("str")` が他の文脈で使われる場合は `.as_ptr()` を付ける。
（`*const c_char` として使われるケースが多い）

### 判定方法

literal_string_param かどうかの判定は、`current_literal_string_params: HashSet<InternedStr>`
（パラメータ名の集合）を使う。

## 実装

### Phase 1: `RustCodegen` に `current_literal_string_params` を追加

**ファイル**: `src/rust_codegen.rs`

```rust
pub struct RustCodegen<'a> {
    // ... 既存フィールド ...
    current_type_param_map: HashMap<InternedStr, String>,
    /// 現在生成中のマクロのリテラル文字列パラメータ名の集合
    current_literal_string_params: HashSet<InternedStr>,
}
```

`generate_macro()` で設定:

```rust
self.current_literal_string_params = info.literal_string_params.iter()
    .filter_map(|&idx| info.params.get(idx).map(|p| p.name))
    .collect();
```

### Phase 2: `sizeof(str) - 1` → `str.len()` パターンの検出

**ファイル**: `src/rust_codegen.rs`

`expr_to_rust` の `ExprKind::BinOp` ハンドラで、
`Sizeof(Ident(param)) - IntLit(1)` パターンを検出:

```rust
ExprKind::BinOp { op: BinOp::Sub, lhs, rhs } => {
    // sizeof(literal_string_param) - 1 → param.len()
    if let ExprKind::Sizeof(inner) = &lhs.kind {
        if let ExprKind::Ident(name) = &inner.kind {
            if self.current_literal_string_params.contains(name) {
                if let ExprKind::IntLit(1) = &rhs.kind {
                    let param = self.interner.get(*name);
                    return format!("{}.len()", escape_rust_keyword(param));
                }
            }
        }
    }
    // 通常の処理...
}
```

### Phase 3: `sizeof(str)` → `str.len() + 1`

`ExprKind::Sizeof` ハンドラで、内部が literal_string_param の場合:

```rust
ExprKind::Sizeof(inner) => {
    if let ExprKind::Ident(name) = &inner.kind {
        if self.current_literal_string_params.contains(name) {
            let param = self.interner.get(*name);
            return format!("({}.len() + 1)", escape_rust_keyword(param));
        }
    }
    // 通常の処理...
}
```

### Phase 4: 関数引数での `.as_ptr()` 付与

`ExprKind::Call` のハンドラ内、引数の式を生成する際に:

```rust
// 引数生成時に literal_string_param の Ident なら .as_ptr() を付与
fn expr_to_rust_with_str_conversion(&mut self, expr: &Expr, info: &MacroInferInfo) -> String {
    if let ExprKind::Ident(name) = &expr.kind {
        if self.current_literal_string_params.contains(name) {
            let param = self.interner.get(*name);
            return format!("{}.as_ptr()", escape_rust_keyword(param));
        }
    }
    self.expr_to_rust(expr, info)
}
```

ただし、呼び出し先が `literal_string_params` を持つマクロの場合は
変換しない（`&str` をそのまま渡す）。

代替案: `ExprKind::Ident` ハンドラで常に `.as_ptr()` を付けるのは
`sizeof` や他の文脈で問題が生じるため避ける。

### Phase 5: `expr_to_rust_inline` にも同様の処理

`expr_to_rust_inline` の各ハンドラにも同様の変換を追加。

## 変更ファイル一覧

| ファイル | 変更内容 |
|----------|----------|
| `src/rust_codegen.rs` | `current_literal_string_params` 追加、`BinOp`/`Sizeof`/`Call` ハンドラ修正 |

## 検証

1. `cargo build` / `cargo test`

2. 出力確認:
   ```bash
   cargo run -- --auto --gen-rust samples/xs-wrapper.h --bindings samples/bindings.rs 2>/dev/null \
     | grep -A 5 -E 'fn (newSVpvs|sv_catpvs|memCHRs|hv_fetchs)\b'
   ```
   - `str.as_ptr()` が関数引数に出力されること
   - `str.len()` が `sizeof(str) - 1` の代わりに出力されること

3. 回帰テスト: `cargo test rust_codegen_regression`

## エッジケース

1. **`sizeof(str) - 1` 以外のパターン**: `sizeof(str)` 単独で使われる場合は
   `str.len() + 1` を生成（C の sizeof は null 終端を含む）。

2. **マクロ呼び出しの連鎖**: `sv_catpvs(sv, str)` → `sv_catpvs_flags(sv, str, flags)`
   のように、`&str` パラメータが別の `&str` パラメータを持つマクロに渡される場合は
   `.as_ptr()` を付けず `&str` のまま渡す。

3. **`ASSERT_IS_LITERAL(str)`**: 既に identity builtin として処理済み。
   展開結果の `str` に対して文脈に応じた変換が適用される。

4. **`&str` の参照**: `&str` が `ExprKind::UnaryOp(AddrOf, Ident("str"))` として
   出現する場合は `&str.as_ptr()` ではなく、文脈に応じた変換が必要。
   `sizeof` の場合は Phase 2/3 で対処済み。
