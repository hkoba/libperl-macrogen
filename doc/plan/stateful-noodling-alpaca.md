# Plan: help: 付きエラーの段階的修正

## Context

E0277 ビット演算修正完了後、total errors: 735。
`categorize-help-diags.tcl` で `tmp/build-error.log` を分析した結果、
`help:` 付きエラーが 38 カテゴリに分類された。

根本原因別にグルーピングし、影響度順にコード生成器を修正する。

## エラー分類（根本原因別）

### グループ A: ポインタ型検出改善（前提作業）

`infer_type_hint()` と `is_pointer_expr_inline()` が
PtrMember/Member のポインタ型を検出できない。
既存の `field_type_map` を活用して改善する。

### グループ B: ポインタ二項演算 — 100 件 (Cat 10: 88, 11: 3, 8: 9)

```
(*my_perl).Istack_base + offset   → E0369 (ptr + int)
cx - 1                            → E0369 (ptr - int)
ptr1 - ptr2                       → E0369 (ptr - ptr)
```
- **macro パス**: `infer_type_hint()` 改善で既存の `.offset()` 変換が効くようになる
- **inline パス**: ポインタ演算処理が完全に未実装 → 追加必要

### グループ C: ポインタ複合代入 — 32 件 (Cat 6: 21, 3: 11)

```
(*my_perl).Imarkstack_ptr -= 1;   → E0368 (ptr -=)
(*dest) += 1;                     → E0368 (ptr +=)
```
- macro/inline 両パスで未実装 → `wrapping_add`/`wrapping_sub` 変換を追加

### グループ D: Null ポインタ比較 — 49 件 (Cat 9: 45, 21: 4)

```
if ((gv) != 0)                    → E0308 (ptr vs usize)
```
- 既存の `.is_null()` 変換は実装済みだが、ポインタ型検出が不十分
- グループ A の改善で大半が解決する見込み

### グループ E: 関数引数の整数型変換 — 90 件

| Cat | 件数 | パターン |
|-----|------|----------|
| 4 | 38 | `u32` → `i32` (定数: SV_GMAGIC 等) |
| 31 | 8 | `u64` → `u32` |
| 13 | 9 | `usize` → `i32` (sizeof) |
| 15 | 6 | `i8` → `i32` (char literal) |
| 34 | 6 | `i32` → `u32` (packWARN) |
| 他 | 23 | 各種幅変換 |

関数呼び出し時に `rust_decl_dict.fns` のシグネチャと比較して `as` キャスト挿入。

### グループ F: 小規模個別修正 — 20 件

| Cat | 件数 | 概要 |
|-----|------|------|
| 14 | 4 | `svtype` 比較 → enum を整数にキャスト |
| 39 | 3 | 引数に `mut` 不足 |
| 32 | 1 | float literal `0` vs `0.0` |
| 他 | 12 | ジェネリック型、CStr 比較等 |

## 実装フェーズ

### Phase 1: ポインタ型検出改善

**ファイル**: `src/rust_codegen.rs`

#### 1a. `infer_type_hint()` に Member/PtrMember 対応追加

現在 `TypeHint::Unknown` を返す箇所を改善:

```rust
ExprKind::Member { member, .. } | ExprKind::PtrMember { member, .. } => {
    let member_str = self.interner.get(*member);
    if let Some(ty) = self.field_type_map.get(member_str) {
        if is_pointer_type_str(ty) { TypeHint::Pointer }
        else if ty == "bool" { TypeHint::Bool }
        else { TypeHint::Integer }
    } else { TypeHint::Unknown }
}
```

#### 1b. `is_pointer_expr_inline()` に Member/PtrMember/Deref 追加

```rust
ExprKind::Member { member, .. } | ExprKind::PtrMember { member, .. } => {
    let member_str = self.interner.get(*member);
    self.field_type_map.get(member_str).is_some_and(|ty| is_pointer_type_str(ty))
}
ExprKind::Deref(inner) => {
    // ポインタ to ポインタの deref
    if let Some(ty) = self.infer_expr_type_str_inline(inner) {
        if let Some(derefed) = deref_type(&ty) {
            return is_pointer_type_str(derefed);
        }
    }
    false
}
```

### Phase 2: inline パスにポインタ演算追加

**ファイル**: `src/rust_codegen.rs` — `expr_to_rust_inline()` の Binary ハンドラ

macro パス (L1875-1897) と対称的な処理を追加:

```rust
if matches!(op, BinOp::Add | BinOp::Sub) {
    if self.is_pointer_expr_inline(lhs) && !self.is_pointer_expr_inline(rhs) {
        let l = self.expr_to_rust_inline(lhs);
        let r = self.expr_to_rust_inline(rhs);
        return if *op == BinOp::Add {
            format!("{}.offset({} as isize)", l, r)
        } else {
            format!("{}.offset(-({} as isize))", l, r)
        };
    }
    // ptr - ptr → .offset_from()
    if self.is_pointer_expr_inline(lhs) && self.is_pointer_expr_inline(rhs) && *op == BinOp::Sub {
        let l = self.expr_to_rust_inline(lhs);
        let r = self.expr_to_rust_inline(rhs);
        return format!("{}.offset_from({})", l, r);
    }
}
```

### Phase 3: ポインタ複合代入

**ファイル**: `src/rust_codegen.rs` — 3 箇所の Assign ハンドラ

`ptr += n` → `ptr = ptr.wrapping_add(n as usize)` に変換。

macro パス (`expr_to_rust`):
```rust
AssignOp::AddAssign | AssignOp::SubAssign => {
    let lh = self.infer_type_hint(lhs, info);
    if lh == TypeHint::Pointer {
        let method = if *op == AssignOp::AddAssign { "wrapping_add" } else { "wrapping_sub" };
        return format!("{{ {} = {}.{}({} as usize); {} }}", l, l, method, r, l);
    }
    // 既存の fallthrough
}
```

inline パス (`stmt_to_rust_inline`, `expr_to_rust_inline`): 同パターン。

### Phase 4: 関数引数の型キャスト挿入

**ファイル**: `src/rust_codegen.rs` — Call ハンドラ

関数呼び出し時に `rust_decl_dict.fns[name].params[i].ty` と
`infer_expr_type_str*()` の結果を比較し、不一致なら `as` キャスト挿入。

```rust
fn cast_arg_if_needed(&self, arg_str: &str, arg_expr: &Expr,
                       expected_ty: &str, ...) -> String {
    if let Some(actual) = self.infer_expr_type_str*(arg_expr, ...) {
        let na = normalize_integer_type(&actual);
        let ne = normalize_integer_type(expected_ty);
        if na.is_some() && ne.is_some() && na != ne {
            return format!("({} as {})", arg_str, ne.unwrap());
        }
    }
    arg_str.to_string()
}
```

### Phase 5: 小規模個別修正

- **Cat 39** (3件): `mut` パラメータ検出
- **Cat 32** (1件): float vs int 0 → `0.0`
- **Cat 14** (4件): svtype 比較 → `as u32` キャスト

## 検証

各 Phase 後に:
```bash
# 1. テスト通過
cargo test

# 2. 統合ビルド + 分類スクリプト実行
~/blob/libperl-rs/12-macrogen-2-build.zsh
./categorize-help-diags.tcl tmp/build-error.log

# 3. 全体エラー数の確認
grep -c '^error' tmp/build-error.log

# 4. tmp/help/ の error カテゴリ件数一覧で、対象カテゴリの減少を確認
for d in $(ls -v tmp/help/); do
  help=$(cat tmp/help/$d/__help__.txt)
  case "$help" in *"(error)"*)
    count=$(ls tmp/help/$d/*.txt 2>/dev/null | grep -v __help__ | wc -l)
    printf "%3d  %4d  %s\n" "$d" "$count" "$help"
    ;; esac
done
# 期待: Phase で対象とした Cat の件数が 0 または大幅減少
```

## 期待効果

| Phase | 対象エラー | 期待削減 |
|-------|-----------|---------|
| 1+2 | ポインタ演算 (Cat 10,11,8) + Null比較 (Cat 9,21) | ~149 件 |
| 3 | ポインタ複合代入 (Cat 3,6) | ~32 件 |
| 4 | 関数引数型変換 (グループ E) | ~90 件 |
| 5 | 小規模修正 (グループ F) | ~8 件 |
| **合計** | | **~279 件** (735 → ~456) |
