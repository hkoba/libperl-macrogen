# Plan: const/mut 不一致エラーの修正 (80件)

## 問題の内訳

80 件の `types differ in mutability` エラー。2つの主パターン。

### パターン A: let 宣言の型不一致 (28件)

```rust
// inline 関数内のローカル変数宣言
let a: *mut U8 = (s1 as *const U8);  // *mut <- *const
```

**原因**: inline 関数の C コードで `U8 *a = (const U8*)s1` のように
const キャストされた値を非 const ポインタ変数に代入。
C では合法だが Rust ではエラー。

**発生箇所**: inline 関数の `decl_to_rust_let` で生成される let 宣言。
変数の型は C 宣言から取得（`*mut U8`）、初期化式は Cast から取得（`*const U8`）。

**修正方針**: `decl_to_rust_let` で、初期化式がポインタ型で
宣言型とconst/mutが異なる場合、`as *mut U8` キャストを追加。

### パターン B: 関数引数の mutability 不一致 (52件)

```rust
// caller は *const SV パラメータを持つが、callee は *mut SV を要求
sv_copypv_flags(my_perl, a: *mut SV, b: *mut SV, c: I32)
// 呼び出し元で:  sv_copypv_flags(my_perl, sv, ...)  // sv: *const SV
```

| 呼び出し先 | 件数 |
|-----------|------|
| `sv_dup` / `sv_dup_inc` | 9 |
| `sv_*_flags` 系 | 12 |
| `save_pushptr` | 2 |
| その他 | 29 |

**原因**: 2つのケースがある:

1. **自家生成マクロ間**: 呼び出し元の const/mut 推論で `*const` にしたパラメータを、
   `*mut` パラメータを持つ自家生成マクロに渡す。
   → const/mut 推論の must-mut 検出が不完全。

2. **自家生成マクロ → bindings.rs 関数**: 自家生成マクロ経由で
   bindings.rs の `*mut` パラメータ関数に渡すが、
   間のマクロは単なるラッパーで `*const` に推論された。
   → ラッパーマクロの const 推論が transitive に効いていない。

**修正方針**: 2段階で対応。

**Step 1 (根本修正)**: const/mut 推論の must-mut 検出を改善。
呼び出し先マクロのパラメータが `*mut` なら、呼び出し元のパラメータも `*mut` にする。
→ 既存の `callee_const_params` チェックで対応できるはずだが、
  自家生成マクロ間の伝播が完全でない可能性。

**Step 2 (フォールバック)**: 引数渡し時に const→mut の自動キャストを挿入。
`cast_integer_arg_if_needed` を拡張して、引数の const→mut 変換にも対応。

---

## 修正計画

### 修正 1: let 宣言の const→mut キャスト (28件)

**場所**: `src/rust_codegen.rs` — `decl_to_rust_let` (inline 関数パス)

```rust
// 既存: let 宣言の整数型キャスト処理
let init_expr = if let Some(expr_ut) = self.infer_expr_type_inline(expr) {
    // 整数型キャスト
    ...
};

// 追加: ポインタ const→mut キャスト
let init_expr = if decl_ty.contains("*mut") && init_expr.contains("*const") {
    // 初期化式がポインタで const/mut 不一致
    format!("({} as {})", init_expr, decl_ty)
} else {
    init_expr
};
```

より正確には `infer_expr_type_inline` で初期化式の型を取得し、
宣言型と const/mut を比較する。

### 修正 2: const/mut 推論の自家生成マクロ間伝播改善 (52件の一部)

**場所**: `src/rust_codegen.rs` — `collect_must_mut_from_expr` の `Call` ケース

現在のロジック:
```rust
let const_arg_positions = callee_const.get(func_name);
let is_const_at_callee = const_arg_positions
    .map_or(false, |positions| positions.contains(&i));
if !is_const_at_callee {
    result.insert(*arg_name);  // must-mut
}
```

問題: `callee_const` にキーが存在しない場合、`map_or(false, ...)` で
`is_const_at_callee = false` → must-mut。これは正しい。

しかし **自家生成マクロが `callee_const` にキーとして存在する場合**
（= そのマクロの一部パラメータが const）、
**他のパラメータ**（const でないもの）が正しく mut として伝播しないケースがある。

→ 確認して必要なら修正。

### 修正 3: 引数渡し時の const→mut キャスト (残り)

**場所**: `src/rust_codegen.rs` — `cast_integer_arg_if_needed`

既存の SV subtype cast に加えて、const→mut の変換も追加:

```rust
// ポインタの const→mut 変換
if actual.contains("*const") && expected_ty.contains("*mut") {
    let actual_base = actual.replace("*const", "*mut");
    if actual_base == expected_ty
        || is_sv_subtype_cast(&UnifiedType::from_rust_str(&actual_base),
                               &UnifiedType::from_rust_str(expected_ty)) {
        return format!("({} as {})", arg_str, expected_ty);
    }
}
```

---

## 実装順序

1. **修正 1** (let 宣言) — 独立して実装可能、28件解消見込み
2. **修正 2** (const/mut 推論改善) — 原因調査→修正。根本対応。
3. **修正 3** (引数キャスト) — フォールバック。修正 2 で対応しきれない分を吸収。

## 期待効果

80件中 60〜70件の解消を見込む。
