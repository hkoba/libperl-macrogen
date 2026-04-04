# Plan: `expected bool, found integer` 残り22件の修正 (Phase 2 移行後)

## 残りエラーの分類

### パターン 1: bool フィールドに `!= 0` (5件)

```rust
// L1604: assert 条件内の || 式の中
assert!(((((*my_perl).Itainting) != 0) || (!(((*my_perl).Itainted) != 0))));
// L1605: if 条件
if (((*my_perl).Itainted) != 0) {
// L1608: if 条件
if (((*my_perl).Itainting) != 0) {
```

`Itainting`, `Itainted` は bindings.rs で `bool` 型。
`wrap_as_bool_condition` は最外の式に対して呼ばれるが、
内側の部分式 `((*my_perl).Itainted)` に対しては呼ばれない。

**原因**: `!= 0` は `expr_to_rust_inline` の中の `Binary(Ne)` ハンドラで
そのまま `!= 0` として出力される。外側から `wrap_as_bool_condition` が呼ばれても、
内側の `Binary(Ne, PtrMember(Itainted), IntLit(0))` は既に展開済み。

問題は「`x != 0` の `x` が bool 型なら、`x != 0` ではなく `x` 単体で出力すべき」
ということ。これは `Binary(Ne, expr, IntLit(0))` パターンの検出。

### パターン 2: bool パラメータに `!= 0` (1件)

```rust
// cBOOL(cbool: bool) → ((cbool) != 0)
pub unsafe fn cBOOL(cbool: bool) -> bool {
    ((cbool) != 0)
}
```

`cBOOL` のパラメータ `cbool: bool` に対し `!= 0` が付く。
パターン 1 と同じ原因で `Binary(Ne, Ident(cbool), IntLit(0))`。

### パターン 3: bool 引数に整数リテラル `0`/`1` (10件)

```rust
// gv_efullname4 の d: bool に 1 を渡す
gv_efullname4(my_perl, sv, gv, prefix, 1);
// utf16_to_utf8_base の e: bool/f: bool に 0/1 を渡す
utf16_to_utf8_base(my_perl, p, d, bytelen, newlen, false, 1)
// _toLOWER_utf8_flags の f: bool に 0 を渡す
_toLOWER_utf8_flags(my_perl, p, e, s, l, 0)
// SvTRUE_common の第3引数が bool で 1 を渡す
SvTRUE_common(my_perl, sv, 1)
```

`callee_param_is_bool` は自家生成マクロの bool パラメータを認識するが、
`expr_to_rust_arg` / inline Call ハンドラで `0`/`1` → `false`/`true` 変換が
自家生成マクロに対しても動作しているか確認が必要。

呼び出し先が自家生成マクロで `is_bool_return = true` のケースでは、
パラメータ型推論で `bool` が伝播しているかが問題。

### パターン 4: bool フィールドへの代入で `(s) != 0` (3件)

```rust
// TAINTING_set(s: bool)
{ (*my_perl).Itainting = ((s) != 0); (*my_perl).Itainting };
```

`s` は `bool` パラメータだが `(s) != 0` となっている。
`Itainting` は `bool` フィールドなので、`s` をそのまま代入すべき。
→ パターン 1+2 の複合。`s != 0` の `s` が bool なら不要。

### パターン 5: その他 (3件)

- `PL_valid_types_PVX[...] != 0` — 配列要素が bool 型
- `Gv_AMupdate(..., 0) != 0` — `Gv_AMupdate` が `bool` を返すか不明
- `MAYBE_DEREF_GV_flags<T>` — ジェネリクス関連

---

## 修正方針

### 修正 A: `Binary(Ne, expr, IntLit(0))` の bool 検出 (パターン 1, 2, 4: 9件)

**Phase 3 の `expr_to_rust` / `expr_to_rust_inline`** の `Binary(Ne)` ハンドラで、
`expr != 0` の `expr` が bool 型なら `!= 0` を省略して `expr` のみを出力する。

具体的には `Binary { op: BinOp::Ne, lhs, rhs: IntLit(0) }` パターンで
LHS が bool（パラメータ、フィールド、関数戻り値）なら `lhs` のみを出力。

```rust
BinOp::Ne if matches!(rhs.kind, ExprKind::IntLit(0)) => {
    // LHS が bool なら != 0 は不要
    if self.is_bool_expr_with_dict(lhs)
        || self.current_param_types.get(...).is_some_and(|ut| ut.is_bool())
        || self.field_type_is_bool(lhs) {
        return l;  // != 0 なしで LHS のみ
    }
}
```

同様に `Binary { op: BinOp::Eq, lhs, rhs: IntLit(0) }` は `!lhs` に変換可能。

### 修正 B: 整数リテラル → bool 変換の拡張 (パターン 3: 10件)

**Phase 2** で確定した `is_bool_return` を持つマクロのパラメータが `bool` なら、
呼び出し時の `0`/`1` を `false`/`true` に変換する。

`callee_param_is_bool` が Phase 2 の `resolved_param_types` を参照するか、
`MacroInferInfo` のパラメータ型制約から `bool` を判定できるようにする。

inline Call ハンドラでも同様。

---

## 実装順序

1. **修正 A** — `Binary(Ne/Eq, bool_expr, IntLit(0))` パターンの検出と省略 (9件)
2. **修正 B** — 自家生成マクロの bool 引数に 0/1 → false/true 変換 (10件)

## 期待効果

22件中 19件の解消を見込む。残り3件はジェネリクスや特殊ケース。
