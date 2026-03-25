# Plan: 残存ポインタ演算エラー修正 (57件, 5 Cat)

## Context

前回実装した Phase 1-3 でポインタ演算対応を追加したが、57 件が残存。
原因は 2 つ:
1. `infer_type_hint` の MacroCall/Call ハンドラが不完全
2. PreInc/PostInc/PreDec/PostDec がポインタ型を考慮していない

```
Cat  件数  エラーメッセージ
  9    21  (error) consider using `wrapping_add` or `add` for pointer + {integer}
  5    18  (error) consider using `add` or `wrapping_add` to do pointer arithmetic
  3    11  (error) consider using `sub` or `wrapping_sub` to do pointer arithmetic
  7     4  (error) consider using `offset_from` for pointer - pointer
 15     3  (error) consider using `wrapping_sub` or `sub` for pointer - {integer}
```

## 根本原因

### 1. `infer_type_hint(MacroCall)` が `expanded` を見ない (Cat 9, 3, 7)

`infer_expr_type_str` (L1353) は MacroCall で return_constraints が無ければ
`expanded` にフォールバックするが、`infer_type_hint` (L978) はしない。

例: `PL_markstack` マクロ → expanded は `(*my_perl).Imarkstack` (PtrMember)
→ `field_type_map["Imarkstack"]` = `*mut I32` → Pointer なのに Unknown を返す。

### 2. `infer_type_hint(Call)` が関数戻り値型を見ない (Cat 15)

例: `r#in.offset(inlen as isize) - left` で LHS が `.offset()` の戻り値。
`infer_type_hint(Call)` は macro_ctx のみ参照し `rust_decl_dict` の戻り値型を見ない。
`is_pointer_expr_inline(Call)` は既に `get_callee_return_type` を見るが、
macro パスの `infer_type_hint(Call)` には無い。

### 3. PreInc/PostInc/PreDec/PostDec のポインタ未対応 (Cat 5, 3)

`PreInc` → `{{ e += 1; e }}`、`PostDec` → `{{ let _t = e; e -= 1; _t }}` と
固定テンプレートで生成。`e` がポインタの場合 `+=`/`-=` はコンパイルエラー。

## 実装計画

### Fix A: `infer_type_hint` MacroCall — expanded フォールバック

**ファイル**: `src/rust_codegen.rs` L978-986

```rust
// 変更前
ExprKind::MacroCall { name, .. } => {
    ...
    TypeHint::Unknown
}

// 変更後
ExprKind::MacroCall { name, expanded, .. } => {
    if let Some(callee) = self.macro_ctx.macros.get(name) {
        for c in &callee.type_env.return_constraints {
            if is_type_repr_pointer(&c.ty) {
                return TypeHint::Pointer;
            }
        }
    }
    self.infer_type_hint(expanded, info)  // フォールバック追加
}
```

### Fix B: `infer_type_hint` Call — 関数戻り値型チェック

**ファイル**: `src/rust_codegen.rs` L966-976

既存のマクロ return_constraints チェックの後に `get_callee_return_type` を追加:

```rust
ExprKind::Call { func, .. } => {
    if let ExprKind::Ident(name) = &func.kind {
        // 既存: マクロの戻り値型
        if let Some(callee) = self.macro_ctx.macros.get(name) { ... }
        // 追加: bindings.rs の関数戻り値型
        if let Some(ret_ty) = self.get_callee_return_type(self.interner.get(*name)) {
            if is_pointer_type_str(ret_ty) {
                return TypeHint::Pointer;
            }
        }
    }
    TypeHint::Unknown
}
```

### Fix C: PreInc/PostInc/PreDec/PostDec ポインタ対応

**ファイル**: `src/rust_codegen.rs`
- macro パス: 4箇所 (PreInc L2296, PreDec L2308, PostInc L2319, PostDec L2330)
- inline パス: 4箇所 (PreInc L3719, PreDec L3732, PostInc L3743, PostDec L3754)

ポインタ判定を追加し、ポインタなら wrapping_add/wrapping_sub を使用:

```rust
// macro パス PreInc 例
ExprKind::PreInc(inner) => {
    let e = /* 既存の MacroCall/Call lvalue 展開 */;
    if self.infer_type_hint(inner, info) == TypeHint::Pointer {
        format!("{{ {} = {}.wrapping_add(1); {} }}", e, e, e)
    } else {
        format!("{{ {} += 1; {} }}", e, e)
    }
}
// PreDec: wrapping_sub(1)
// PostInc: {{ let _t = e; e = e.wrapping_add(1); _t }}
// PostDec: {{ let _t = e; e = e.wrapping_sub(1); _t }}
```

inline パスは `is_pointer_expr_inline(inner)` でチェック。

## 修正箇所まとめ

| Fix | 箇所 | 対象 Cat | 期待削減 |
|-----|------|---------|---------|
| A | 1 | 9, 3, 7 | ~36 |
| B | 1 | 15 | ~3 |
| C | 8 | 5, 3 | ~29 |

※ Cat 3 は Fix A + Fix C の両方で解消される（型検出改善 + Inc/Dec 対応）

## 検証

```bash
cargo test
~/blob/libperl-rs/12-macrogen-2-build.zsh
for d in 3 5 7 9 15; do
  count=$(ls tmp/help/$d/*.txt 2>/dev/null | grep -v __help__ | wc -l)
  printf "Cat %2d: %d\n" "$d" "$count"
done
grep -c '^error' tmp/build-error.log
# 期待: Cat 3,5,7,9,15 が 0 または大幅減、total 640→~583
```
