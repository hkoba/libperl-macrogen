# SvPV_flags_const_nolen の引数 sv に型がつかない問題

## 症状

`SvPV_flags_const_nolen` の引数 `sv` が `/* unknown */` になる。

```rust
// [CODEGEN_INCOMPLETE] SvPV_flags_const_nolen [THX] - macro function
// pub unsafe fn SvPV_flags_const_nolen(my_perl: *mut PerlInterpreter, sv: /* unknown */, flags: U32) -> *mut c_char {
```

`sv` は `Perl_SvPV_helper` の第2引数として渡されているため、`Perl_SvPV_helper` のパラメータ型 `*const SV` が伝播するはずだが、伝播していない。

## 原因

`parse_c_type_string` が `"SV * const"` 形式の型文字列をパースできない。

### 処理の流れ

1. `lookup_inline_fn_param_type` が `Perl_SvPV_helper` の arg 1 の型を取得
2. `Type::Pointer(TypedefName(SV), quals={const})` が返される
3. `Type::display()` が `"SV * const"` を出力
4. `parse_c_type_for_inline_fn` → `TypeRepr::from_apidoc_string("SV * const")` → `parse_c_type_string` を呼び出し

### `parse_c_type_string("SV * const")` の処理

```
入力: "SV * const"

Step 1: 末尾の `*` をチェック
  → "SV * const" は `*` で終わらない → ptr_count = 0

Step 2: `const` をチェック
  → ends_with(" const") → true
  → is_const = true, base = "SV *"

Step 3: parse_c_base_type("SV *")
  → 既知の型名にマッチしない
  → interner.lookup("SV *") → None
  → フォールバック: CTypeSpecs::Void  ← ★ここで void になる
```

### 根本原因

`parse_c_type_string` は以下の処理順で設計されている:
1. 末尾の `*` を全て除去
2. `const` を除去
3. 残りをベース型としてパース

しかし `"SV * const"` のように `const` が `*` の **後** にある場合、Step 1 で `*` が検出されず、Step 2 で `const` だけ除去された結果、ベース型に `*` が残ってしまう。

## 影響範囲

`const` 修飾付きポインタパラメータを持つ全てのインライン関数で同じ問題が発生する。

`Perl_SvPV_helper` の場合:

| arg | C型 | display() 出力 | パース結果 |
|-----|-----|----------------|-----------|
| 0 | `PerlInterpreter *my_perl` | `PerlInterpreter *` | ✓ `PerlInterpreter *` |
| 1 | `SV *const sv` | `SV * const` | ✗ `void` |
| 2 | `STRLEN *const lp` | `STRLEN * const` | ✗ `void` |
| 3 | `const U32 flags` | `U32` | ✓ `U32` |
| 4 | `const PL_SvPVtype type` | `PL_SvPVtype` | ✓ `PL_SvPVtype` |
| 5 | `char *(*non_trivial)(...)` | `(function ...)` | ✗ `void` |
| 6 | `const bool or_null` | `_Bool` | ✓ `_Bool` |
| 7 | `const U32 return_flags` | `U32` | ✓ `U32` |

## 修正案

`parse_c_type_string` で `*` と修飾子（`const`, `volatile`）の除去をループで行う:

```rust
// 末尾の *, const, volatile をループで除去
loop {
    let prev = base;
    if base.ends_with('*') {
        ptr_count += 1;
        base = base[..base.len() - 1].trim();
    }
    if base.ends_with(" const") {
        is_const = true;
        base = base[..base.len() - 6].trim();
    }
    if base.ends_with(" volatile") {
        base = base[..base.len() - 9].trim();
    }
    if base == prev {
        break;
    }
}
```

この修正により `"SV * const"` は以下のようにパースされる:
1. ループ1回目: `" const"` 除去 → `"SV *"`
2. ループ2回目: `"*"` 除去 → `"SV"`, `ptr_count = 1`
3. ループ3回目: 変化なし → break
4. `parse_c_base_type("SV")` → `CTypeSpecs::TypedefName(SV)` ✓

## 関連ファイル

- `src/type_repr.rs` - `parse_c_type_string()`, `parse_c_base_type()`
- `src/semantic.rs` - `lookup_inline_fn_param_type()`, `parse_c_type_for_inline_fn()`
