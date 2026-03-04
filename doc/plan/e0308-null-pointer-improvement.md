# E0308 Null Pointer — 改善計画

## 現状サマリー

| エラー | 件数 | 割合 |
|--------|------|------|
| E0308 総数 | 687 | (全エラー 981 件の 70%) |
| ├ null pointer (help あり) | 62 | |
| ├ integer conversion (help あり) | 78 | |
| ├ float literal (help あり) | 1 | |
| ├ char → i32 (help あり) | 5 | |
| ├ did you mean / その他 (help あり) | 4 | |
| └ help なし | 537 | |

### help なし E0308 の主な内訳

| パターン | 件数 | 説明 |
|----------|------|------|
| integer → bool | 86 | C の暗黙的 int↔bool 変換 |
| bool → i32 | 51 | bool を整数として使用 |
| `*const T` → `*mut T` | ~100 | const/mut ポインタの不一致 |
| u32 ↔ usize / u8 / i32 | ~60 | 整数幅の不一致 |
| `*mut TypeA` → `*mut sv` | ~50 | 派生型ポインタの不一致 |

---

## 本計画のスコープ: null pointer パターン (62 エラー)

### エラーの分類

| サブパターン | 件数 | 例 |
|-------------|------|-----|
| 条件式: `(ptr) != 0` | 31 | `if ((file) != 0)`, `while ((mg) != 0)` |
| 式中: `(expr) != 0` | 16 | `((PadnameTYPE(pn)) != 0)`, `(((*o).op_next) != 0)` |
| assert: `assert!((ptr) != 0)` | 10 | `assert!((a) != 0)` |
| 関数引数: リテラル 0 | 5 | `sv_2pv(my_perl, sv, 0)` |

### 根本原因

codegen にはすでに null pointer → `.is_null()` 変換のロジックがあるが、
型ヒント（`TypeHint::Pointer`）の検出精度が不足しているため多くのケースで
フォールバックの `(expr) != 0` が出力される。

| 検出対象 | マクロ codegen (`infer_type_hint`) | inline codegen (`is_pointer_expr_inline`) |
|----------|----------------------------------|----------------------------------------|
| パラメータ | ✓ (`param_constraints`) | ✓ (`current_param_types`) |
| ローカル変数 | ✗ | ✗ |
| 構造体フィールド | ✗ (`PtrMember` → `Unknown`) | ✗ (`PtrMember` → `false`) |
| 関数呼び出し戻り値 | △ (macro_ctx のみ) | △ (macro_ctx のみ) |
| `Eq/Ne` 比較での is_null 変換 | ✓ (L1316-1334) | **✗ (未実装)** |

特に **inline 関数の `expr_to_rust_inline` に `Eq/Ne` → `.is_null()` 変換が
存在しない**のが大きな欠落。

---

## 改善策

### Phase 1: ローカル変数の型追跡 (期待効果: ~20 エラー削減)

`current_param_types` を拡張して、ローカル変数宣言の型も追跡する。

**変更箇所:**

1. `collect_decl_names()` (L959-965) を拡張して型も収集:

```rust
fn collect_decl_names(&mut self, decl: &Declaration) {
    for init_decl in &decl.declarators {
        if let Some(name) = init_decl.declarator.name {
            self.current_local_names.insert(name);
            // 追加: 型情報も収集
            let ty = self.decl_type_string(decl, init_decl);
            self.current_param_types.insert(name, ty);
        }
    }
}
```

2. `infer_type_hint()` の `Ident` 分岐 (L677) にローカル変数型の参照を追加:

```rust
ExprKind::Ident(name) => {
    // 追加: current_param_types（パラメータ + ローカル変数）を参照
    if let Some(ty) = self.current_param_types.get(name) {
        if is_pointer_type_str(ty) {
            return TypeHint::Pointer;
        }
    }
    // 既存: param_constraints を参照
    ...
}
```

**効果:** `(file) != 0`, `(gv) != 0`, `(prev) != 0`, `(mg) != 0` 等の
ローカル変数ポインタ比較が `.is_null()` に変換される。

### Phase 2: inline 関数の `Eq/Ne` → `.is_null()` 変換 (期待効果: ~15 エラー削減)

`expr_to_rust_inline` の `Binary` 処理に、マクロ codegen と同等の
pointer-null 比較変換を追加。

**変更箇所:**

`expr_to_rust_inline()` の `ExprKind::Binary` 分岐 (L2593-2604):

```rust
ExprKind::Binary { op, lhs, rhs } => {
    // 追加: ポインタ == 0 / != 0 → .is_null()
    if matches!(op, BinOp::Eq | BinOp::Ne) {
        if self.is_pointer_expr_inline(lhs) && is_null_literal(rhs) {
            let l = self.expr_to_rust_inline(lhs);
            return if *op == BinOp::Eq {
                format!("{}.is_null()", l)
            } else {
                format!("!{}.is_null()", l)
            };
        }
        if self.is_pointer_expr_inline(rhs) && is_null_literal(lhs) {
            let r = self.expr_to_rust_inline(rhs);
            return if *op == BinOp::Eq {
                format!("{}.is_null()", r)
            } else {
                format!("!{}.is_null()", r)
            };
        }
    }
    // 既存コード ...
}
```

### Phase 3: 構造体フィールドアクセスの型追跡 (期待効果: ~10 エラー削減)

`PtrMember` / `Member` の型ヒントを FieldsDict から解決する。

**変更箇所:**

1. `infer_type_hint()` に `PtrMember` / `Member` のハンドリングを追加:

```rust
ExprKind::PtrMember { expr, member } | ExprKind::Member { expr, member } => {
    // FieldsDict から一貫した型があればそれを使用
    let member_str = self.interner.get(*member);
    if let Some(ty) = self.fields_dict.get_consistent_type(member_str) {
        if is_type_repr_pointer(&ty) {
            return TypeHint::Pointer;
        }
    }
    TypeHint::Unknown
}
```

2. `is_pointer_expr_inline()` にも同等の処理を追加。

**注意点:** `FieldsDict` が codegen に渡されているか確認が必要。

### Phase 4: 関数引数のリテラル 0 → `null_mut()` (期待効果: 5 エラー削減)

関数呼び出しの引数で、パラメータ型がポインタなのにリテラル `0` が渡されている
場合、`std::ptr::null_mut()` に変換する。

**変更箇所:**

`expr_to_rust()` / `expr_to_rust_inline()` の `Call` ハンドリング:

```rust
ExprKind::Call { func, args } => {
    // 引数の型ヒントを取得（bindings_info または macro_ctx から）
    let param_types = self.get_callee_param_types(func);
    let rendered_args: Vec<String> = args.iter().enumerate().map(|(i, arg)| {
        if let Some(ty) = param_types.get(i) {
            self.expr_with_type_hint(arg, info, Some(ty))
        } else {
            self.expr_to_rust(arg, info)
        }
    }).collect();
    // ...
}
```

**注意点:** `bindings_info` から関数のパラメータ型を取得する仕組みが必要。
現在は `expr_with_type_hint` が戻り値型でのみ使われているので、
引数型でも同じ仕組みを使う。

---

## 実装優先度

| Phase | 対象 | 期待削減 | 実装コスト | 依存 |
|-------|------|---------|-----------|------|
| 1 | ローカル変数の型追跡 | ~20 | 小〜中 | なし |
| 2 | inline Eq/Ne → is_null | ~15 | 小 | Phase 1 と並行可能 |
| 3 | 構造体フィールド型追跡 | ~10 | 中 | FieldsDict 参照の確認 |
| 4 | 関数引数 0 → null_mut | 5 | 中 | callee 型情報の取得 |
| **合計** | | **~50** | | |

### 推奨実装順序

Phase 1 + Phase 2 を先に実装（相互に独立、効果大）。
Phase 3 は FieldsDict の利用可能性を確認してから。
Phase 4 は callee の型情報取得方法の設計が必要。

---

## 将来の拡張（本計画のスコープ外）

| カテゴリ | 件数 | 説明 |
|---------|------|------|
| integer ↔ bool 変換 | 137 | C の暗黙的 int/bool 変換 |
| 整数幅の不一致 | ~78 | u32 ↔ usize / u8 / i32 等 |
| const/mut ポインタ不一致 | ~100 | `*const T` → `*mut T` |
| 派生型ポインタ不一致 | ~50 | `*mut HV` → `*mut SV` 等 |

これらは別の改善計画で対応する。
