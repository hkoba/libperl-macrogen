# Plan: null ポインタ型とパラメータ型推論の修正 (カテゴリ B+C)

## 問題

2つの異なるカテゴリのエラー、合計 21 件。

---

## カテゴリ B: パラメータ型推論の失敗 (14件)

### 症状

```rust
// 正しくは a: *const refcounted_he だが *mut c_int に推論される
pub unsafe fn refcounted_he_chain_2hv(my_perl: ..., a: *mut c_int, b: U32) -> *mut HV {
    Perl_refcounted_he_chain_2hv(my_perl, a, b)
}
```

bindings.rs の実関数 `Perl_refcounted_he_chain_2hv` は
`c: *const refcounted_he` を受ける。マクロ本体が `Perl_refcounted_he_chain_2hv(aTHX_ a, b)` で
`a` をそのまま渡しているので、型は `*const refcounted_he` に推論されるべき。

### 対象関数 (14件)

| マクロ関数 | パラメータ | 正しい型 | 現在の推論型 |
|-----------|-----------|---------|------------|
| `refcounted_he_chain_2hv(a, b)` | a | `*const refcounted_he` | `*mut c_int` |
| `refcounted_he_fetch_pv(a, b, c, d)` | a | `*const refcounted_he` | `*mut c_int` |
| `refcounted_he_fetch_pvn(a, b, c, d, e)` | a | `*const refcounted_he` | `*mut c_int` |
| `refcounted_he_fetch_sv(a, b, c, d)` | a | `*const refcounted_he` | `*mut c_int` |
| `refcounted_he_free(a)` | a | `*mut refcounted_he` | `*mut c_int` |
| `refcounted_he_inc(a)` | a | `*mut refcounted_he` | `*mut c_int` |
| `refcounted_he_new_pv(a, b, c, d, e)` | a | `*const refcounted_he` | `*mut c_int` |
| `refcounted_he_new_pvn(a, b, c, d, e, f)` | a | `*const refcounted_he` | `*mut c_int` |
| `refcounted_he_new_sv(a, b, c, d, e)` | a | `*const refcounted_he` | `*mut c_int` |
| `init_tm(a)` | a | `*mut tm` | `*mut c_int` |
| `resume_compcv_and_save(buffer)` | buffer | `*mut suspended_compcv` | `*mut c_int` |
| 他3件 | | | |

### 原因

これらのマクロは embed.h で定義されたシンプルなラッパー:
```c
#define refcounted_he_chain_2hv(a,b)  Perl_refcounted_he_chain_2hv(aTHX_ a,b)
```

パラメータ名が `a`, `b` 等の汎用名で、`embed.fnc` の apidoc に
型情報がない場合、型推論は `collect_call_constraints()` を通じて
`Perl_refcounted_he_chain_2hv` の引数型から推論するはず。

しかし現在 `*mut c_int` に推論されている。これは以下のいずれかが原因:
1. `Perl_refcounted_he_chain_2hv` が bindings.rs に存在しない
2. THX パラメータの位置ずれ（aTHX_ が最初の引数として追加されるため）
3. 型が `refcounted_he` で、bindings.rs の型名がマッチしない

### 調査と修正方針

**Step 1: 原因の特定**
- bindings.rs に `Perl_refcounted_he_chain_2hv` が存在するか確認
- THX パラメータのオフセット処理が正しいか確認
- `collect_call_constraints()` でこの関数の型が正しく取得されるか確認

**Step 2: 修正**
原因に応じて以下のいずれかを実施:
- THX オフセット修正: `collect_call_constraints()` で aTHX_ 引数をスキップ
- 型名マッチング修正: `refcounted_he` の型名解決
- フォールバック改善: 推論失敗時に `*mut c_int` ではなく `/* unknown */` にする

---

## カテゴリ C: null ポインタの型が `*mut c_void` のまま (7件)

### 症状

```rust
// cur_text は *mut SV 型のフィールドだが、null が *mut c_void
(((((*cx).cx_u).cx_blk).blk_u).blku_eval).cur_text = (0 as *mut c_void);
// ↑ expected *mut SV, found *mut c_void
```

### 対象パターン

| コード | 代入先フィールド型 | null の型 |
|--------|-------------------|----------|
| `.cur_text = (0 as *mut c_void)` | `*mut SV` | `*mut c_void` |
| `.old_namesv = (0 as *mut c_void)` | `*mut SV` | `*mut c_void` |
| `.cv = (0 as *mut c_void)` | `*mut CV` | `*mut c_void` |
| `(if cond { expr } else { (0 as *mut c_void) })` | `*mut *mut SV` | `*mut c_void` |

### 原因

C では `NULL` (= `(void*)0`) は任意のポインタ型に暗黙変換される。
Rust では型が一致しないとエラーになる。

codegen の `expr_to_rust_inline` の `Cast` ハンドラが
`(void*)0` → `(0 as *mut c_void)` を生成するが、
**代入先のフィールド型を考慮していない**。

### 修正方針

代入文 (`Assign`) の codegen で、RHS が null リテラル
(`is_null_literal(rhs)` = true) の場合、LHS のフィールド型に合わせた
null ポインタ式を生成する。

```rust
// Stmt::Expr(Some(Assign { op: Assign, lhs, rhs }))
if is_null_literal(rhs) {
    // LHS のフィールド型を取得
    if let Some(field_ut) = self.infer_field_type_from_lhs(lhs) {
        if field_ut.is_pointer() {
            return format!("{}{} = {};", indent, l, null_ptr_expr(&field_ut));
        }
    }
}
```

同様に、条件式の else 節で null が使われるケース:
```rust
// (if cond { expr } else { NULL })
// else 節の型を then 節の型に合わせる
```
→ これは既存の条件式型統一ロジック（conditional unification）で
  then 節のポインタ型から else 節の null 型を推論可能。

### 変更箇所

| ファイル | 関数/箇所 | 変更 |
|----------|----------|------|
| `rust_codegen.rs` | inline の `stmt_to_rust` (Assign) | LHS フィールド型に合わせた null 生成 |
| `rust_codegen.rs` | macro の `expr_to_rust` (Assign) | 同上 |
| `rust_codegen.rs` | 条件式の else 節 | then 節の型に合わせた null 型推論 |

---

## 実装順序

1. **カテゴリ B の調査** — 原因特定が先（THX オフセット？型名不一致？）
2. **カテゴリ C の修正** — 代入文での null ポインタ型合わせ（独立して実装可能）
3. **カテゴリ B の修正** — 原因に基づく修正

## 期待効果

- カテゴリ B: 14件のエラー解消（パラメータ型が正しくなる）
- カテゴリ C: 7件のエラー解消（null ポインタが正しい型になる）
