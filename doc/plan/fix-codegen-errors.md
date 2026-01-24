# 生成コードのコンパイルエラー修正計画

## 概要

libperl-macrogen が生成した Rust コードを libperl-sys でビルドした際に発生するエラーの修正計画。

エラー総数: 約 3,500 件

---

## 課題リスト（優先度順）

### 1. 型が見つからない (E0412) - 2,278件

**症状:**
```
error[E0412]: cannot find type `c_void` in this scope
error[E0412]: cannot find type `c_char` in this scope
error[E0412]: cannot find type `c_int` in this scope
error[E0412]: cannot find type `size_t` in this scope
```

**件数内訳:**
- `c_void`: 1,137件
- `c_char`: 614件
- `c_int`: 450件
- `size_t`: 77件
- `SSize_t`: 28件
- `ssize_t`: 27件

**原因:**
生成コードに use 文がない。

**修正方針:**
コード生成時に先頭に以下を追加:
```rust
use std::ffi::{c_void, c_char, c_int, c_uint, c_long, c_ulong};
// または bindgen 生成の型を参照
use super::{c_void, c_char, c_int, size_t, ...};
```

**修正箇所:** `src/rust_codegen.rs` - コード生成の先頭部分

---

### 2. `(0 as ())` パターン (E0605) - 266件 【難易度: 高】

**症状:**
```
error[E0605]: non-primitive cast: `{integer}` as `()`
  |
17 |     (0 as ());
```

**原因:**
C の `ASSUME(x)` 内の `assert(x)` が単なる if 文に変換され、
結果として `(0 as ())` という無効なコードになっている。

本来は `assert!(x)` に変換すべきケース。

**修正方針:**
- `assert(x)` パターンを検出して `assert!(x)` に変換
- `ASSUME`/`STATIC_ASSERT` マクロの特別処理が必要

**修正箇所:** `src/rust_codegen.rs` または `src/macro_infer.rs`

**備考:** 難易度が高いため後回し

---

### 3. `__builtin_expect` が見つからない (E0425) - 48件

**症状:**
```
error[E0425]: cannot find function `__builtin_expect` in this scope
```

**原因:**
GCC の組み込み関数。分岐予測のヒントを与えるが、Rust には存在しない。

**修正方針:**
`__builtin_expect(cond, expected)` → `cond` に置き換え（expected は無視）

**修正箇所:** `src/rust_codegen.rs` - 関数呼び出しの変換

---

### 4. 型の不一致 (E0308) - 709件

複数のサブパターンに分類される。

#### 4a. `else { 0 }` でポインタを返す

**症状:**
```rust
return (if cond { ptr } else { 0 });
//                             ^ expected `*mut T`, found integer
```

**原因:**
C の `NULL` (0) がそのまま `0` として出力されている。

**修正方針:**
戻り値型がポインタの場合、`0` → `std::ptr::null_mut()` に変換

---

#### 4b. `(bool_expr) != 0` の冗長比較

**症状:**
```rust
if (cond1 == cond2) != 0 { ... }
//  ^^^^^^^^^^^^^^ already bool, `!= 0` is redundant
```

**原因:**
C では `if (expr)` で任意の整数を条件にできるが、Rust では bool が必要。
しかし、既に bool の場合は `!= 0` が冗長。

**修正方針:**
式の型を追跡し、既に bool なら `!= 0` を付けない

---

#### 4c. `&&` / `||` の両辺が整数

**症状:**
```rust
if ((flags & MASK1) && (flags & MASK2)) != 0 { ... }
//  ^^^^^^^^^^^^^^^ expected `bool`, found `u32`
```

**原因:**
C では `(a && b)` で a, b が整数でも動作するが、Rust では bool が必要。

**修正方針:**
`&&` / `||` の各辺を `!= 0` でラップ:
```rust
if ((flags & MASK1) != 0 && (flags & MASK2) != 0) { ... }
```

---

#### 4d. 定数の型違い (`u32` vs `i32`)

**症状:**
```rust
Perl_sv_2iv_flags(my_perl, sv, SV_GMAGIC);
//                             ^^^^^^^^^ expected `i32`, found `u32`
```

**原因:**
定数の型が関数の期待する型と一致していない。

**修正方針:**
関数呼び出し時に引数の型を確認し、必要に応じて `as` キャストを追加

---

### 5. break outside loop (E0268) - 24件

**症状:**
```
error[E0268]: `break` outside of a loop or labeled block
```

**原因:**
C の `do { ... } while(0)` パターンが正しく変換されていない。

**修正方針:**
`do { ... } while(0)` → `'block: { ... }` (labeled block) に変換

**修正箇所:** `src/rust_codegen.rs` - do-while 文の変換

---

### 6. 未定義の関数/マクロ (E0425) - 多数

**症状:**
```
error[E0425]: cannot find function `SvANY` in this scope
error[E0425]: cannot find function `SvFLAGS` in this scope
error[E0425]: cannot find function `HvAUX` in this scope
error[E0425]: cannot find function `FITS_IN_8_BITS` in this scope
error[E0425]: cannot find value `sp` in this scope
```

**原因:**
- これらはマクロとして定義されており、別の関数として生成されるべき
- または展開されるべきだが展開されていない

**修正方針:**
- マクロ依存関係の解決順序を確認
- 必要なマクロが先に生成されるようにする

---

### 7. ポインタ演算 (E0369, E0277) - 74件

**症状:**
```
error[E0369]: cannot add `{integer}` to `*mut u8`
error[E0277]: cannot add `*mut gp` to `{integer}`
```

**原因:**
C のポインタ演算 `ptr + offset` が Rust でそのまま使えない。

**修正方針:**
`ptr + offset` → `ptr.offset(offset as isize)` または `ptr.add(offset)`

---

### 8. 引数の数の不一致 (E0061) - 64件

**症状:**
```
error[E0061]: this function takes 2 arguments but 1 argument was supplied
error[E0061]: this function takes 3 arguments but 2 arguments were supplied
```

**原因:**
THX マクロ (`aTHX_`, `pTHX_`) の処理が不正確。

**修正方針:**
THX パラメータの追加/削除ロジックを確認・修正

---

## 実装順序

1. **課題 1**: use 文追加（最も簡単、2,278件解消）
2. **課題 3**: `__builtin_expect` 置換（簡単、48件解消）
3. **課題 4a**: null ポインタ変換（中程度）
4. **課題 5**: break outside loop（中程度）
5. **課題 4b-4d**: 型不一致の残り（複雑）
6. **課題 6-8**: 残りの問題（調査必要）
7. **課題 2**: `(0 as ())` / assert 変換（難易度高、後回し）

---

## 進捗

| 優先度 | 課題 | 難易度 | ステータス | 解消件数 |
|--------|------|--------|------------|----------|
| 1 | use 文追加 | 簡単 | **完了** | 2,278 |
| 2 | `__builtin_expect` | 簡単 | **完了** | 48 |
| 3 | null ポインタ変換 | 中程度 | 未着手 | - |
| 4 | break outside loop | 中程度 | 未着手 | - |
| 5 | 型の不一致（残り） | 複雑 | 未着手 | - |
| 6 | 未定義関数 | 調査必要 | 未着手 | - |
| 7 | ポインタ演算 | 中程度 | 未着手 | - |
| 8 | 引数の数 | 調査必要 | 未着手 | - |
| 9 | `(0 as ())` / assert | 高 | 後回し | - |
