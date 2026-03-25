# Plan: `consider using` help エラーの修正

## Context

`~/blob/libperl-rs/12-macrogen-2-build.zsh` の統合テストで 594 エラーが発生。
`./categorize-help-diags.tcl tmp/build-error.log` でカテゴリ分けした結果、
以下のエラーパターンが判明した（`remove these parentheses` 警告 999 件を除く）。

| カテゴリ | 件数 | 根本原因 |
|----------|------|----------|
| null pointer (null_mut) | 31 | 関数引数の `0` が pointer 型に変換されない |
| null pointer (null) | 3 | 同上 (const pointer) |
| u32→i32 conversion | 33 | 返り値/引数の整数幅不一致 |
| usize→i32 conversion | 9 | 同上 |
| u64→u32 conversion | 8 | ビット演算結果の幅不一致 |
| i8→i32 conversion | 6 | 文字リテラルキャスト |
| u32→u8 conversion | 4 | ビット演算結果の幅不一致 |
| consider making mutable | 9 | inline 関数パラメータに mut 未付与 |
| pointer + integer (wrapping_add) | 4 | pointer 演算が未検出 |
| pointer - pointer (offset_from) | 1 | 同上 |
| BitAnd/BitOr/BitAndAssign/BitOrAssign | 8 | 異なる幅の整数間のビット演算 |
| trait 制約不足 (Add/Sub/Rem/PartialEq/Clone) | 12 | ジェネリック関数の trait bound 不足 |
| float literal | 1 | float vs int リテラル比較 |
| did you mean / provide argument | 4 | マクロ展開の引数順序/個数エラー |
| その他 (u8→u64, usize→isize 等) | ~15 | 整数幅不一致の各バリエーション |

**目標**: 高コスト・高影響度の修正から優先して実施し、エラー数を大幅削減する。

## 修正ファイル

| ファイル | 変更内容 |
|---------|---------|
| `src/rust_codegen.rs` | 主な修正すべて (null pointer, mut params, 返り値キャスト, ビット演算幅) |
| `src/rust_decl.rs` | union フィールドの追加 (pointer 演算検出に必要) |

## Phase 1: 関数引数の null pointer 変換 (34 件削減見込み)

**問題**: `0` が関数引数として渡されるとき、呼び出し先のパラメータ型が
ポインタ型の場合に `std::ptr::null_mut()` / `std::ptr::null()` に変換されない。

**例**:
```rust
// 現在の出力:
sv_2pv(my_perl, sv, 0)  // 0 は *mut usize 型のはず
// 期待する出力:
sv_2pv(my_perl, sv, std::ptr::null_mut())
```

**既存コード**: `expr_to_rust_arg()` (L1665-1699) は bool/整数キャスト処理があるが、
null pointer 処理がない。inline 側の Call 処理 (L3784-3802) も同様。

### 変更 1: `expr_to_rust_arg()` に null pointer 検出を追加

```rust
// L1676 付近 (bool 変換の前に挿入)
// null pointer パラメータへの 0 リテラル変換
if let Some(callee_name) = callee {
    let func_name = self.interner.get(callee_name);
    if let Some(expected_ut) = self.get_callee_param_type(func_name, arg_index) {
        if expected_ut.is_pointer() && is_null_literal(expr) {
            return null_ptr_expr(expected_ut);
        }
    }
}
```

### 変更 2: inline Call 処理 (L3784-3802) に同様の null pointer 検出を追加

```rust
// L3792 付近 (bool 変換の後に挿入)
if let Some(expected_ut) = self.get_callee_param_type(&f, param_idx) {
    if expected_ut.is_pointer() && is_null_literal(arg) {
        return null_ptr_expr(expected_ut);
    }
}
```

**検証**: `cargo test` + 統合テスト

## Phase 2: inline 関数パラメータの `mut` 付与 (9 件削減見込み)

**問題**: inline 関数の `param_decl_to_rust()` (L3065-3082) はパラメータを
常に immutable で宣言する。マクロ側には `collect_mut_params()` (L451-578) が
あるが、inline 関数側では未使用。

**例**:
```rust
// 現在の出力:
pub unsafe fn Perl_utf8_hop(s: *mut U8, off: ssize_t) -> *mut U8 {
    { let _t = s; s = s.wrapping_add(1); _t };  // ERROR: cannot assign to immutable
// 期待する出力:
pub unsafe fn Perl_utf8_hop(mut s: *mut U8, mut off: ssize_t) -> *mut U8 {
```

### 変更: `generate_inline_fn()` (L2950) で mutation 検出を追加

1. 関数本体の AST (`body`) からパラメータ名を集める
2. `collect_mut_params_from_stmt()` を使って mutable なパラメータを検出
3. `build_fn_param_list()` (L3025) に `mut_params: &HashSet<InternedStr>` 引数を追加
4. `param_decl_to_rust()` に `is_mut: bool` 引数を追加し、`mut ` プレフィックスを生成

具体的な変更:

```rust
// generate_inline_fn() 内、パラメータリスト構築前:
let param_names: HashSet<InternedStr> = /* 関数パラメータ名を収集 */;
let mut_params = {
    let mut result = HashSet::new();
    collect_mut_params_from_stmt(body, &param_names, &mut result);
    result
};
let param_list = self.build_fn_param_list_with_mut(derived, &mut_params);
```

`build_fn_param_list` / `param_decl_to_rust` を変更して `mut_params` を受け取り、
該当パラメータに `mut ` プレフィックスを付ける。

**検証**: `cargo test` + 統合テスト

## Phase 3: 返り値の整数型キャスト (33+ 件削減見込み)

**問題**: 関数の返り値型が `i32` だが式の推論型が `u32` の場合、
`as i32` キャストが挿入されない。

**3 つのサブパターン**:

### 3a. Expression 形式のマクロ関数の返り値

**場所**: マクロ codegen の `ParseResult::Expression` 処理 (L1852-1866)

現在: 式をそのまま出力（void/bool チェックのみ）
修正: 式の推論型と `current_return_type` を比較し、整数幅が不一致なら `as` キャスト挿入

```rust
// L1864 の else ブロック内:
} else {
    // 返り値型と推論型が異なる整数型なら as キャストを挿入
    if let Some(ref ret_ut) = self.current_return_type {
        if let Some(expr_ut) = self.infer_expr_type(expr, info) {
            let ret_s = ret_ut.to_rust_string();
            let expr_s = expr_ut.to_rust_string();
            let nr = normalize_integer_type(&ret_s);
            let ne = normalize_integer_type(&expr_s);
            if let (Some(r), Some(e)) = (nr, ne) {
                if r != e {
                    self.writeln(&format!("{}({} as {})", body_indent, rust_expr, r));
                    // continue to next
                    ...
                }
            }
        }
    }
    self.writeln(&format!("{}{}", body_indent, rust_expr));
}
```

### 3b. `return expr;` 文のマクロ/inline 関数

**場所**: `stmt_to_rust()` (L2713-2732), `stmt_to_rust_inline()` (L3187-3207)

現在: null pointer と bool のみ特殊処理。整数型不一致は処理なし。
修正: 既存の null/bool 処理の後に整数キャスト処理を追加。

```rust
// L2730 付近（bool 処理の後）:
// 整数型の返り値キャスト
let e = self.expr_to_rust(expr, info);
if let Some(expr_ut) = self.infer_expr_type(expr, info) {
    let ret_s = rt.to_rust_string();
    let expr_s = expr_ut.to_rust_string();
    let nr = normalize_integer_type(&ret_s);
    let ne = normalize_integer_type(&expr_s);
    if let (Some(r), Some(e_norm)) = (nr, ne) {
        if r != e_norm {
            return format!("return ({} as {});", e, r);
        }
    }
}
return format!("return {};", e);
```

同等のロジックを `stmt_to_rust_inline()` にも適用。

### 3c. 暗黙の返り値（if-else の最後の式など）

マクロの Expression 形式ではなく Statement 形式で、最後の式が暗黙に返り値に
なるケースも同じパターンで対応する。ただし、これは Phase 3a で Expression パスが
カバーする範囲が広いため、残りのケース数は少ない見込み。

**検証**: `cargo test` + 統合テスト

## Phase 4: 関数引数の整数型キャスト強化 (10+ 件削減見込み)

**問題**: `cast_integer_arg_if_needed()` は既に存在するが、以下のケースで機能しない:
- 呼び出し先が自前生成マクロの場合、`get_callee_param_type()` が `None` を返す
- `normalize_integer_type()` が認識しない型名の場合

### 変更 1: マクロ呼び出しでも型情報を取得

`expr_to_rust_arg()` で `get_callee_param_type()` が `None` の場合、
`macro_ctx` からマクロのパラメータ型を取得するフォールバックを追加。

### 変更 2: `normalize_integer_type()` の対応型を拡充

現在未対応の可能性がある型 (`ssize_t`, `STRLEN` 等) を追加。

**検証**: `cargo test` + 統合テスト

## Phase 5: pointer 演算の検出改善 (5 件削減見込み)

**問題**: `svu_pv + xpv_cur` のような pointer + integer が `wrapping_add()` に
変換されない。また `array_ptr - alloc_ptr` が `offset_from()` に変換されない。

**根本原因**: `RustDeclDict::process_item()` (`src/rust_decl.rs` L114) が
`Item::Union` を処理していないため、union フィールド (`svu_pv`, `svu_iv` 等) が
`field_type_map` に含まれない。`is_pointer_expr_inline()` / `infer_type_hint()` の
`Member`/`PtrMember` arm は `field_type_map` を参照するため、pointer 検出に失敗。

**例**:
```rust
// 現在の出力 (pointer + integer が素の + のまま):
(*(((*sv).sv_u).svu_pv + (*((*sv).sv_any as *mut XPV)).xpv_cur))
// 期待する出力:
(*(((*sv).sv_u).svu_pv.wrapping_add((*((*sv).sv_any as *mut XPV)).xpv_cur)))
```

### 変更: `src/rust_decl.rs` の `process_item()` に `Item::Union` 処理を追加

```rust
// L132-138 の Item::Struct と同様:
Item::Union(item_union) => {
    if Self::is_pub(&item_union.vis) {
        let name = item_union.ident.to_string();
        let fields = Self::extract_fields(&item_union.fields);
        self.structs.insert(name.clone(), RustStruct { name, fields });
    }
}
```

union フィールドを `structs` に追加することで、`build_field_type_map()` が
union フィールドも含むようになり、pointer 検出が正しく動作する。

**注意**: union フィールド名の衝突も `build_field_type_map()` の conflict 検出で
正しく処理される（同名・同型なら統合、異型ならスキップ）。

**検証**: `cargo test` + 統合テスト

## Phase 6: ビット演算の型推論改善 (8+ 件削減見込み)

**問題**: `BitAnd`/`BitOr`/`BitAndAssign`/`BitOrAssign` でのキャストロジックは
既に実装済み (L2238-2254, L2541-2554, L3165-3178, L3682-3698) だが、
型推論が複雑な式チェーンで失敗するため、キャストが挿入されない。

**エラー例**:
```rust
// u8 &= u32: LHS が *(HEK_KEY(hek) as *mut c_uchar).offset(...).offset(...)
// → Deref(メソッドチェーン) の型推論が None を返す
*(ptr as *mut c_uchar).offset(n).offset(m) &= (!HVhek_UTF8);

// u64 | u8: RHS の cast 結果が u8 で、LHS が u64
uv = ((uv << 6) | (((*s) as U8) & mask));
// → BitOr の wider_integer_type で u64 が選ばれるが、`(expr as U8)` の型推論で
//   U8 → normalize → u8 で、wider_integer_type("u64", "u8") → "u64"
//   → RHS にキャスト挿入されるはず。`infer_expr_type` が Binary(BitAnd) の型を
//   正しく返せていない可能性。
```

**根本原因**: `infer_expr_type` / `infer_expr_type_inline` の `Call` arm (L1432-1443)
が **メソッドチェーン** (`.offset()` 等) の戻り値型を推論できない。

### 変更: `infer_expr_type` の Call arm にメソッド呼び出しの型推論を追加

`.offset()` / `.wrapping_add()` / `.wrapping_sub()` メソッドはレシーバと同じ型を返す。
この知識を `infer_expr_type` に追加:

```rust
ExprKind::Call { func, args } => {
    // メソッド呼び出し: receiver.method(arg)
    // offset/wrapping_add/wrapping_sub はレシーバと同じ型を返す
    if let ExprKind::Member { expr: receiver, member } = &func.kind {
        let method_name = self.interner.get(*member);
        if matches!(method_name, "offset" | "wrapping_add" | "wrapping_sub") {
            return self.infer_expr_type(receiver, info);
        }
    }
    // 既存の関数呼び出し型推論...
}
```

同等の変更を `infer_expr_type_inline` にも適用。

**注意**: Phase 5 で union フィールドが `field_type_map` に追加されることで、
一部のエラーは Phase 5 だけで解消される可能性がある。

**検証**: `cargo test` + 統合テスト

## Phase 7 (後日): 残りのエラー修正

以下は件数が少なく、修正の複雑さが高いため後日対応：

- **ジェネリック関数の trait bound** (12 件): `<T: Copy + PartialEq<i32> + Add<u32>>` など
  - `build_generic_clause()` で型パラメータの使用パターンから必要な trait を推定
  - 複雑度が高く、影響する関数数も限られる

- **`did you mean` / `provide argument`** (4 件): マクロ展開の引数順序/個数エラー
  - 個別のマクロ展開バグ（hv_common, Perl_custom_op_get_field）

- **float literal** (1 件): 既存処理の適用漏れ（LHS が float でも変数の場合に未適用）

## 検証コマンド (各 Phase 共通)

```bash
cargo test
~/blob/libperl-rs/12-macrogen-2-build.zsh
grep -c '^error' tmp/build-error.log
./categorize-help-diags.tcl tmp/build-error.log 2>&1 | grep -v 'remove these parentheses'
```

## 削減見込み

| Phase | 削減見込み | 累計残り |
|-------|-----------|---------|
| 現状 | - | 594 |
| Phase 1 (null pointer) | ~34 | ~560 |
| Phase 2 (mut params) | ~9 | ~551 |
| Phase 3 (返り値キャスト) | ~33 | ~518 |
| Phase 4 (引数キャスト強化) | ~10 | ~508 |
| Phase 5 (pointer 演算) | ~5 | ~503 |
| Phase 6 (ビット代入演算) | ~8 | ~495 |
