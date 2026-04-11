# Plan: 代入文の LHS/RHS 型不一致に対するキャスト自動挿入

## 現状の問題

代入文 `lhs = rhs` で LHS と RHS の型が不一致のケースが 13 件ある。

### フィールド代入 (9件)

| LHS 型 | RHS 型 | 例 |
|--------|--------|-----|
| `u8` | `u16` | `Iin_eval = blku_u16 & 63` |
| `isize` | `i32` | `blku_oldsp = sp.offset_from(...)` |
| `u16` | `*mut _` | `blku_u16 = std::ptr::null_mut()` |
| `u16` | `u32` | `blku_u16 = op_private & phlags` |
| `isize` | `*mut _` | `xav_fill = std::ptr::null_mut()` |
| `usize` | `*mut _` | `xpv_cur = std::ptr::null_mut()` |
| `usize` | `u64` | `*retlen = expectlen` |
| `u64` | `u8` | `uv = (uv as U8) & mask` |
| `usize` | `isize` | `*len = tmps.offset_from(...)` |

### let 宣言 (4件)

| 宣言型 | 初期化式型 | 例 |
|--------|-----------|-----|
| `UV` (u64) | `u8` | `let expectlen = *PL_utf8skip[...]` |
| `*mut U8` | `*const U8` | `let send = s.offset(...)` |
| (同上2件) | | |

## パターン分類

### パターン A: 整数幅不一致 (6件)

`u8 ← u16`, `isize ← i32`, `u16 ← u32`, `usize ← u64`, `u64 ← u8`, `usize ← isize`

**対処**: RHS に `as lhs_type` キャストを挿入。
C では整数型間の暗黙変換が可能。Rust では `as` キャストが必要。

### パターン B: null ポインタの整数フィールド代入 (3件)

`u16 ← *mut _`, `isize ← *mut _`, `usize ← *mut _`

LHS が整数型フィールドだが RHS が `std::ptr::null_mut()`。
C では `NULL` は 0 として使える（整数とポインタの暗黙変換）。

**対処**: LHS が整数型なら RHS の null を `0` に変換。

### パターン C: let 宣言の const/mut 不一致 (2件) + 整数幅 (2件)

既に `decl_to_rust_let` で一部対応済みだが、漏れがある。

## 設計

### 基本方針

代入文の codegen で、LHS の型を `field_type_map` または `infer_expr_type` で取得し、
RHS の推論型と比較して不一致があれば `as` キャストを挿入する。

const→mut キャストは安全でないため**行わない**。整数幅変換のみ。

### 変更箇所

**inline パス** (`stmt_to_rust_inline` の `Assign` ハンドラ):

```rust
AssignOp::Assign => {
    // LHS の型を推論
    let lhs_ut = self.infer_expr_type_inline(lhs);
    let rhs_ut = self.infer_expr_type_inline(rhs);

    let r = if is_null_literal(rhs) {
        // null リテラルの場合
        if let Some(ref lut) = lhs_ut {
            if lut.is_pointer() {
                null_ptr_expr(lut)               // ポインタ型 → null_mut()/null()
            } else {
                "0".to_string()                   // 整数型 → 0
            }
        } else {
            "std::ptr::null_mut()".to_string()    // 型不明 → フォールバック
        }
    } else {
        let r_str = self.expr_to_rust_inline_ctx(rhs, ExprContext::Top);
        // 整数型の幅不一致キャスト
        if let (Some(ref lut), Some(ref rut)) = (&lhs_ut, &rhs_ut) {
            let ls = lut.to_rust_string();
            let rs = rut.to_rust_string();
            if let (Some(nl), Some(nr)) = (normalize_integer_type(&ls), normalize_integer_type(&rs)) {
                if !integer_types_compatible(nl, nr) {
                    format!("{} as {}", strip_outer_parens(&r_str), nl)
                } else { strip_outer_parens(&r_str).to_string() }
            } else { strip_outer_parens(&r_str).to_string() }
        } else { strip_outer_parens(&r_str).to_string() }
    };
    format!("{}{} = {};", indent, l, r)
}
```

**macro パス** (`expr_to_rust_ctx` の `Assign` ハンドラ):

同様のロジックを `infer_expr_type(lhs, info)` / `infer_expr_type(rhs, info)` で実装。

### LHS 型の取得方法

1. **フィールドアクセス** (`Member { member }` / `PtrMember { member }`):
   `field_type_map.get(member_name)` で取得。bindings.rs の構造体フィールド型。

2. **Deref + フィールド** (`(*ptr).field`):
   `field_type_map.get(field_name)` で取得。

3. **ローカル変数** (`Ident(name)`):
   `current_param_types.get(name)` で取得。

4. **Deref (ポインタ先)** (`*ptr`):
   `infer_expr_type_inline(ptr)` → `inner_type()` で取得。

これらは全て `infer_expr_type_inline(lhs)` が返す。

### null リテラルの整数フィールド代入

C では `field = NULL` で整数フィールドに 0 を代入する慣習がある。
Perl ソースでは `xpv_cur = 0` の 0 が `NULL` マクロ経由で `(void*)0` に展開され、
codegen で `std::ptr::null_mut()` に変換されるケース。

**修正**: `is_null_literal(rhs)` の場合、LHS 型がポインタでなければ `0` を出力。

### let 宣言の整数幅不一致

`decl_to_rust_let` の既存ロジック (整数幅キャスト) で対応済みだが、
`PL_utf8skip[...]` のインデックスアクセスで型推論が `u8` を返せない場合がある。

**修正**: `infer_expr_type_inline` に `Index` + `Deref` + static 配列のケースを追加。

## 実装手順

1. **null→0 変換**: `Assign` で `is_null_literal(rhs)` かつ LHS が非ポインタ → `0` を出力
2. **整数幅キャスト**: `Assign` で LHS/RHS の推論型を比較し、幅不一致なら `as` 挿入
3. **let 宣言の改善**: `decl_to_rust_let` の整数幅キャスト対応を強化

## 期待効果

13 件中 9〜11 件のエラーを解消。残りは型推論が型を返せないケース。
