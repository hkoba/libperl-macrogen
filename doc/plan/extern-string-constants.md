# Plan: extern 文字列定数 (PL_Yes 等) のポインタ変換

## 問題

bindings.rs で `extern static PL_Yes: [c_char; 0]` として宣言された
C の文字列定数が、ポインタ比較のコードで型不一致エラーを起こす。

```rust
// 生成コード
((*sv).sv_u).svu_pv as *const c_char) == PL_Yes
// エラー: expected *const i8, found [i8; 0]
```

`PL_Yes` は `[c_char; 0]` 型で、`*const c_char` に暗黙変換されない。
`.as_ptr()` を呼んでポインタに変換する必要がある。

## 既存インフラ

`RustDeclDict` に `static_arrays: HashSet<String>` が既にある。
`PL_Yes`, `PL_No`, `PL_Zero`, `PL_utf8skip` 等の extern static 配列名が格納済み。

`KnownSymbols` にもこれらの名前が含まれている。

## 設計

### アプローチ: codegen で static 配列の Ident に `.as_ptr()` を付加

`expr_to_rust` / `expr_to_rust_inline` の `Ident` ケースで、
名前が `static_arrays` に含まれる場合、`.as_ptr()` を自動付加する。

```rust
ExprKind::Ident(name) => {
    let name_str = self.interner.get(*name);
    // extern static 配列はポインタとして使われるため .as_ptr() を付加
    if self.is_static_array(name_str) {
        return format!("{}.as_ptr()", escape_rust_keyword(name_str));
    }
    // ... 既存ロジック ...
}
```

### 判定方法

`RustDeclDict` の `static_arrays` を `BindingsInfo` 経由で `RustCodegen` に渡す。
既に `BindingsInfo` は `static_arrays: HashSet<String>` を持っている:

```rust
pub struct BindingsInfo {
    pub static_arrays: HashSet<String>,
    pub bitfield_methods: HashSet<String>,
}
```

`RustCodegen` で `self.bindings_info.static_arrays.contains(name_str)` でチェック。

### 影響範囲

- `PL_Yes`, `PL_No`, `PL_Zero` — ポインタ比較 (2 エラー)
- `PL_utf8skip` — 配列インデックス (`PL_utf8skip[i]` → `*PL_utf8skip.as_ptr().offset(i)`)
  ※ これは既に `is_static_array_expr` で対応済み
- `PL_valid_types_PVX` 等 — 同上
- `PL_charclass` — 同上

### 既存の `is_static_array_expr` との関係

codegen には既に `is_static_array_expr` がある:

```rust
fn is_static_array_expr(&self, expr: &Expr) -> bool {
    if let ExprKind::Ident(name) = &expr.kind {
        let name_str = self.interner.get(*name);
        self.bindings_info.static_arrays.contains(name_str)
    } else { false }
}
```

これは `Index` 式で `array[i]` → `*array.as_ptr().offset(i)` の変換に使われている。
今回は `Ident` 単体で使われる場合（ポインタ比較）にも `.as_ptr()` を付ける。

### 注意点

- `Index` 式の中で既に `as_ptr()` が呼ばれるパスがある → 二重に付かないよう注意
- `sizeof_val(&PL_Yes)` のようなケースでは `.as_ptr()` 不要 → しかし現実には発生しない

## 実装手順

1. `expr_to_rust` / `expr_to_rust_inline` の `Ident` ケースに
   static_arrays チェックを追加
2. ポインタコンテキストでのみ `.as_ptr()` を付加
   （代入 RHS、比較、関数引数等）

## 期待効果

`PL_Yes` / `PL_No` 関連の E0308 エラー 2 件が解消。
