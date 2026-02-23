# libperl-sys 統合ビルド エラー分析

**時点**: Phase 3 完了後 (commit `c56630c`)
**総エラー数**: 1,813
**前回 (Phase 2)**: 1,829 → -16

## エラーサマリ

| エラーコード | 件数 | カテゴリ | 概要 |
|-------------|------|---------|------|
| E0308 | 1,054 | 型不一致 | expected X, found Y |
| E0369 | 202 | 二項演算不可 | ポインタ算術 |
| E0425 | 190 | 名前未解決 | シンボルが見つからない |
| E0277 | 120 | トレイト未実装 | 整数幅の不一致演算 |
| E0368 | 45 | 複合代入不可 | ポインタへの += -= |
| E0067 | 34 | 代入LHS不正 | 関数呼び出し結果への代入 |
| E0605 | 30 | キャスト不正 | non-primitive cast |
| E0600 | 28 | 単項マイナス | unsigned への `-` |
| E0614 | 27 | deref 不可 | 整数型への `*` |
| E0599 | 27 | メソッド不在 | 配列への `.offset()` 等 |
| E0054 | 18 | bool キャスト | `as bool` 不可 |
| E0070 | 15 | 代入LHS不正 | rvalue への代入 |
| その他 | 23 | 個別問題 | E0435(5), E0606(4), E0610(3), E0596(3), E0507(2), etc. |

---

## Tier 1: ポインタ算術 (~449 エラー)

**関連**: E0308(~247), E0369(~178), E0368(45), E0600(28)

C ではポインタに整数を加減算できるが、Rust では `.offset()` / `.add()` / `.sub()` を使う必要がある。
現状の codegen はポインタ算術式をそのまま `p + n` と出力しているため、大量のエラーが発生。

### サブカテゴリ

| パターン | C の式 | 現在の出力 | 正しい出力 | 推定件数 |
|---------|--------|-----------|-----------|---------|
| ptr + int | `p + n` | `p + n` | `p.offset(n as isize)` | ~156 (E0369) |
| ptr - ptr | `p - q` | `p - q` | `p.offset_from(q)` | ~22 (E0369) |
| ptr += int | `p += n` | `p += n` | `p = p.offset(n as isize)` | ~45 (E0368) |
| ptr の式が usize に | ptr 比較/代入 | `sv != 0` | `!sv.is_null()` | ~247 (E0308) |
| -usize | `-(unsigned)` | `-(x as usize)` | wrapping_neg 等 | ~28 (E0600) |

### 実装方針

**式の型追跡 (Expression Type Tracking)** が必要。

`expr_to_rust` / `expr_to_rust_inline` が `String` だけでなく型情報も返すように拡張する。
ポインタ型のオペランドが `+` `-` `+=` に渡された場合に `.offset()` 等に変換。

```
// 案: expr_to_rust の返り値を (String, Option<TypeHint>) にする
enum TypeHint {
    Pointer(String),  // "*mut SV" etc.
    Integer,          // i32, u32, usize, etc.
    Bool,
    Unknown,
}
```

**難易度**: 高（全 expr_to_rust の返り値変更、影響範囲大）
**依存**: なし
**効果**: ~449 エラー削減の可能性

---

## Tier 2: 整数幅の暗黙キャスト (~120 エラー)

**関連**: E0277(120)

C では `u32 & u64`、`usize |= u32` 等の異なる整数幅の演算が暗黙に許可される。
Rust では同一型でないと演算できない。

### 主なパターン

| パターン | 件数 | 例 |
|---------|------|-----|
| `u8 & u32` | 22 | `PL_charclass` のビットマスク操作 |
| `u32 & u64` / `u32 \|= u64` | 24 | フラグ操作 |
| `usize &= u32` / `usize \|= u32` | 18 | SvFLAGS 等のビット演算 |
| `i32 & u32` | 5 | 混合符号 |
| ptr + int 型 | ~10 | `{integer} + *mut gp` 等 |

### 実装方針

Tier 1 の型追跡と組み合わせて、二項演算のオペランド間で `as` キャストを挿入。
片方がリテラル整数の場合は型サフィックスを付加する方がシンプルかもしれない。

**難易度**: 中（Tier 1 の型追跡が前提）
**依存**: Tier 1
**効果**: ~120 エラー

---

## Tier 3: ポインタ/整数間の型不一致 (~247 エラー)

**関連**: E0308 のサブセット

### サブカテゴリ A: ポインタの NULL 比較 (~208)

```rust
// 現在の出力
assert!((sv) != 0);
if ((gv) != 0) { ... }

// 正しい出力
assert!(!(sv).is_null());
if (!(gv).is_null()) { ... }
```

`0` と比較される式がポインタ型の場合、`!= 0` → `.is_null()` / `!.is_null()` に変換。
これも Tier 1 の型追跡が必要。

### サブカテゴリ B: ポインタ型間のキャスト欠落 (~39)

```rust
// 現在: expected *mut sv, found *mut av
GvAV(gv)   // returns *mut AV, but caller expects *mut SV

// 必要: as *mut SV キャスト
```

bindgen の型定義で `AV = sv`, `HV = sv` 等の型エイリアスがある場合、
Rust は構造的に異なる型として扱う。`as` キャストが必要。

### サブカテゴリ C: bool / 整数変換 (~111)

```rust
// E0308: expected bool, found integer
if ((hasargs) != 0) { ... }    // hasargs: c_int → OK (既に != 0 あり)
Perl_SvTRUE_common(my_perl, sv, 1);  // 引数に bool 期待、1 渡し
```

関数引数で `bool` を期待するが整数リテラルが渡されるケース。
`1` → `true`, `0` → `false` の変換が必要（引数位置で型が分かる場合）。

**難易度**: 中〜高
**依存**: Tier 1 の型追跡
**効果**: ~247 エラー（Tier 1 と重複あり）

---

## Tier 4: 名前未解決 (~190 エラー)

**関連**: E0425

### サブカテゴリ

| パターン | 件数 | 原因 | 対策 |
|---------|------|------|------|
| `__VA_ARGS__` | 18 | 可変引数マクロの展開不完全 | マクロ引数の正しい展開 |
| C ライブラリ関数 | 49 | `strlen`, `strcmp`, `memcpy` 等 | extern "C" 宣言 or libc crate |
| Perl 内部マクロ/関数 | 43 | `inRANGE`, `PadnameFLAGS` 等 | 生成対象への追加 or inline 展開 |
| ローカル変数 | 25 | `sp`, `stash`, `bodies_by_type` | マクロの前提変数が未宣言 |
| 型名 | 6 | `body_details`, `PerlIO_funcs` | use 文の追加 |

### 対策案

1. **C ライブラリ関数 (49)**: `use libc::*;` を追加、または extern 宣言を生成
2. **`__VA_ARGS__` (18)**: 可変引数マクロの生成を抑制、または展開ロジック改善
3. **ローカル変数 (25)**: マクロが暗黙に参照する変数（`sp` 等）は、
   その function scope に含まれない場合は生成対象外にする
4. **Perl 内部マクロ (43)**: 利用可能性チェックの改善

**難易度**: 中（個別対応が多い）
**依存**: なし
**効果**: ~190 エラー（ただし一部は生成抑制による）

---

## Tier 5: lvalue マクロ呼び出し (~49 エラー)

**関連**: E0067(34), E0070(15)

### パターン

```rust
// E0067: compound assign on function call
{ GvFLAGS(gv) &= (!GVf_ASSUMECV); GvFLAGS(gv) }
{ CopLINE(c) -= 1; CopLINE(c) }
{ SvREFCNT(sv) += 1; SvREFCNT(sv) }
{ RX_EXTFLAGS(rx_sv) |= ...; RX_EXTFLAGS(rx_sv) }
```

Phase 3 で `ExprKind::MacroCall` の LHS 展開は実装済みだが、
これらは `ExprKind::Call`（通常の関数呼び出し形式）として出力されている。
Token Expander が MacroCall ではなく Call として保持するケース。

### 対策案

**案 A**: `Call` の LHS でも、関数名が既知の lvalue マクロ（`ExplicitExpandSymbols` に登録済み）
なら展開形式に変換する。codegen 時に `macro_ctx` を参照して `expanded` 相当の式を再構築。

**案 B**: Token Expander で lvalue 位置の Call を MacroCall に変換する前処理を追加。

案 A が影響範囲が小さい。`ExplicitExpandSymbols` に含まれるマクロ名のリスト
（`GvFLAGS`, `SvREFCNT`, `CopLINE`, `RX_EXTFLAGS`, `RXp_EXTFLAGS`, `ST` 等）を
codegen 側で参照し、`Call` → インライン展開する。

**難易度**: 中
**依存**: なし（単独で実装可能）
**効果**: ~49 エラー

---

## Tier 6: GvGP 型推論の誤り (~27 エラー)

**関連**: E0614(27)

### 原因

`GvGP` マクロの戻り値型が `c_int` と推論されているが、正しくは `*mut GP`。
式 `(0 + ((*gv).sv_u).svu_gp)` で `0 +` が整数加算と解釈され、
戻り値型が `c_int` になっている。

```rust
// 現在の出力
pub unsafe fn GvGP(gv: *mut SV) -> c_int {  // ← 間違い: should be *mut GP
    (0 + ((*gv).sv_u).svu_gp)
}
```

`*GvGP(gv)` が `*c_int` → E0614 (i32 cannot be dereferenced)。

### 対策案

型推論で `0 + ptr` パターンを認識し、結果型をポインタに保つ。
または、`MUTABLE_PTR` マクロ展開の型ヒントを apidoc から取得。

**難易度**: 中
**依存**: なし
**効果**: ~27 エラー（連鎖的に他のエラーも解消する可能性）

---

## Tier 7: as bool キャスト (~18 エラー)

**関連**: E0054

```rust
// 現在
(x as bool)  // E0054: cannot cast u32 as bool

// 正しい
(x != 0)     // or: x as u32 != 0
```

Phase 2 で一度実装を試みたが revert 済み。
文脈依存の変換が必要（`as bool` のキャスト先が bool の場合のみ変換）。

**難易度**: 低〜中
**依存**: なし
**効果**: ~18 エラー

---

## Tier 8: その他の個別問題 (~53 エラー)

| エラー | 件数 | 内容 | 対策 |
|--------|------|------|------|
| E0605 | 30 | non-primitive cast (`[i8;1] as *mut u8` 等) | `.as_ptr()` / `.as_mut_ptr()` |
| E0599 | 27 | 配列 `.offset()` (struct member), union field 等 | struct member 配列の型追跡 |
| E0435 | 5 | 非定数値を定数文脈で使用 | const 式の判定改善 |
| E0606 | 4 | キャスト不正 | 個別対応 |
| E0610 | 3 | `{integer}` has no fields | `0 + ` パターン (Tier 6 関連) |
| E0596 | 3 | immutable borrow | `let` → `let mut` |
| E0507 | 2 | cannot move out of raw ptr | Copy トレイト境界 |
| E0747 | 1 | const を type 位置に | ジェネリクス推論 |
| E0618 | 1 | Option<fn> を直接呼び出し | `.unwrap()()` |
| E0609 | 1 | union field なし | 型推論 |
| E0384 | 1 | immutable 再代入 | `let mut` |
| E0381 | 1 | 未初期化変数使用 | 初期化 |
| E0061 | 1 | 引数の数不一致 | THX 処理 |

---

## 推奨実装順序

### Phase 4: lvalue + GvGP + as bool (~94 エラー, 単独実装可能)

| Step | 内容 | 効果 |
|------|------|------|
| 4-A | lvalue Call 展開 (Tier 5) | ~49 |
| 4-B | GvGP 型推論修正 (Tier 6) | ~27 |
| 4-C | `as bool` → `!= 0` 変換 (Tier 7) | ~18 |

### Phase 5: ポインタ算術基盤 (~449 エラー, 大規模リファクタ)

| Step | 内容 | 効果 |
|------|------|------|
| 5-A | TypeHint 返り値の設計・基盤 | 0 (基盤) |
| 5-B | ptr ± int → `.offset()` 変換 | ~201 (E0369+E0368) |
| 5-C | ptr == 0 → `.is_null()` 変換 | ~208 (E0308 subset) |
| 5-D | ptr - ptr → `.offset_from()` | ~22 |
| 5-E | `-usize` → wrapping 変換 | ~28 |

### Phase 6: 整数幅キャスト + 名前解決 (~310 エラー)

| Step | 内容 | 効果 |
|------|------|------|
| 6-A | 整数幅不一致の `as` 挿入 (Tier 2) | ~120 |
| 6-B | C ライブラリ関数宣言 (Tier 4 部分) | ~49 |
| 6-C | `__VA_ARGS__` 改善 | ~18 |
| 6-D | 生成対象外の判定改善 | ~25 |
| 6-E | bool/integer 引数変換 (Tier 3C) | ~111 |

---

## 参考: エラー推移

| Phase | Total | 主な削減内容 |
|-------|-------|------------|
| Phase 0 (初期) | ~1,895 | - |
| Phase 1 | ~1,895→1,826 (-69) | c_uchar, GCC builtins, goto 除外 |
| Phase 2 | 1,826→1,829 (+3) | enum transmute, NULL/bool (bindings.rs 更新による変動) |
| Phase 3 | 1,829→1,813 (-16) | bitfield getter, array decay, lvalue |
| Phase 4 (計画) | ~1,813→~1,719 | lvalue Call, GvGP, as bool |
| Phase 5 (計画) | ~1,719→~1,270 | ポインタ算術基盤 |
| Phase 6 (計画) | ~1,270→~960 | 整数幅, 名前解決, bool 引数 |
