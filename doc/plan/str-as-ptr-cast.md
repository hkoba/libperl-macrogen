# Plan: `str.as_ptr()` に `as *const c_char` キャストを追加

## 問題

`&str::as_ptr()` は `*const u8` を返すが、C 関数は `*const c_char`（`*const i8`）を期待する。

```rust
// 現在の出力（型不一致エラー）
Perl_newSVpvn(my_perl, str.as_ptr(), str.len())
//                      ^^^^^^^^^^^^
//                      *const u8 だが *const c_char が必要
```

## 対応

`expr_to_rust_arg()` で `.as_ptr()` を生成する箇所を
`.as_ptr() as *const c_char` に変更する。

## 実装

**ファイル**: `src/rust_codegen.rs`

`expr_to_rust_arg()` 内の1行を変更:

```rust
// Before
return format!("{}.as_ptr()", param);

// After
return format!("{}.as_ptr() as *const c_char", param);
```

## 期待される出力

```rust
Perl_newSVpvn(my_perl, str.as_ptr() as *const c_char, str.len())
```
