# Plan: libperl-sys 統合ビルドの Blocker 一覧と対応方針

## 現状サマリー

| 指標 | 数値 |
|------|------|
| 生成された関数（アクティブ） | 2,130 |
| コメントアウトされた関数 | 1,515 (734 CALLS_UNAVAILABLE + 407 CODEGEN_INCOMPLETE + 320 TYPE_INCOMPLETE + 54 PARSE_FAILED) |
| アクティブ関数のうちエラーあり | ~1,032 |
| アクティブ関数のうちエラーなし | ~1,098 |
| コンパイルエラー総数 | 1,929 |
| 警告数 | 1,769 |

## エラー分類（影響大→小の順）

### Tier 1: C→Rust の型システム変換（~1,600 エラー / ~800 関数）

これらは C と Rust の型システムの根本的な違いに起因する。個別の ad-hoc 修正ではなく、
コード生成器（`rust_codegen.rs`）での体系的な変換ルールが必要。

#### 1-A. NULL リテラル問題（~265 エラー）
**症状**: `expected *mut sv, found usize` — C の `0` / `NULL` がそのまま `0` として出力される
**原因**: C では `0` がポインタとして暗黙変換されるが、Rust では不可
**例**:
```rust
// 現状
return (if (cond) { value } else { 0 });  // 0 はポインタではない
(0 as *mut c_void)  // 一部は変換されているが不十分

// 期待
return (if (cond) { value } else { std::ptr::null_mut() });
```
**根本解決**: コード生成時に、ポインタ型が期待される文脈で整数リテラル `0` を
`std::ptr::null_mut()` または `std::ptr::null()` に変換する。
三項演算子の分岐、return 文、代入文、関数引数で発生する。
→ **判断ポイント**: semantic analyzer で型情報を持っている場合は型情報に基づく変換、
持っていない場合はヒューリスティック（`if-else` の対ブランチの型から推論）のどちらが適切か？

#### 1-B. 整数→ bool 変換（~150 エラー）
**症状**: `expected bool, found integer` / `cannot cast u32 as bool`
**原因**: C は整数をそのまま bool として使える。Rust では明示的変換が必要
**例**:
```rust
// 現状
if (((*sv).sv_flags & SVs_GMG) as bool) { ... }  // E0054
return 0;  // where return type is bool              // E0308

// 期待
if ((*sv).sv_flags & SVs_GMG) != 0 { ... }
return false;
```
**根本解決**: 2パターンある
- `as bool` キャスト → `!= 0` に変換
- bool 型返り値関数での `return 0` / `return 1` → `return false` / `return true`

**実装方針**: codegen の `expr_to_rust` で、bool 文脈（`if` 条件、return（返り値 bool の場合）、
`&&` / `||` のオペランド）での整数リテラルおよび `as bool` キャストを変換する。

#### 1-C. 返り値の型不一致（~246 エラー）
**症状**: `expected () because of return type` / `expected i32 because of return type`
**原因**: 複合的
- void 関数なのに最後の式が値を返す（`{ flags &= X; flags }` パターン）
- 返り値の型と `return 0` / `return 1` の不一致（1-A, 1-B と重複）
**例**:
```rust
// void 関数の本体で値式が最後にある
pub unsafe fn SvAMAGIC_off(sv: *mut SV) -> () {
    { (*(*stash).sv_flags &= (!SVf_AMAGIC); ... }
    // ↑ ブロックの最後の式が U32 を返すが、関数は () を返す
}
```
**根本解決**: void 関数の本体で、最後の式が値を持つ場合にセミコロンを追加して
式文にする（値を捨てる）。これは `{ assign; value }` パターンの C comma expression
変換と密接に関連する。

#### 1-D. ポインタ算術（~230 エラー）
**症状**: `cannot subtract &mut sv from *mut sv` / `cannot add i32 to *mut i32`
**原因**: C のポインタ算術（`ptr + n`, `ptr - ptr`, `ptr += n`）は Rust では直接使えない
**例**:
```rust
// 現状
sv - (&mut (*my_perl).Isv_yes)   // ポインタ同士の減算
ptr + n                           // ポインタ+整数

// 期待
(sv as usize - &mut (*my_perl).Isv_yes as *mut _ as usize)  // or .offset()
ptr.offset(n as isize)                                        // or .add(n)
```
**根本解決**: 意味解析で型情報を保持し、codegen で `p + n` → `p.offset(n as isize)`,
`p - q` → `(p as usize).wrapping_sub(q as usize)` のように変換する。
→ **判断ポイント**: ポインタ算術の安全な Rust 変換パターンをどう設計するか？
`.offset()` vs `.add()` vs `.wrapping_offset()` の選択基準は？

#### 1-E. 整数幅の不一致（~143 エラー）
**症状**: `expected usize, found u32` / `expected u32, found u64`
**原因**: C では整数型間の暗黙変換が行われるが、Rust では明示的な `as` キャストが必要
**例**:
```rust
// 現状
array.offset(index)  // index: U32, 期待: isize

// 期待
array.offset(index as isize)
```
**根本解決**: 意味解析の型情報に基づき、代入・引数渡し・比較で型幅が異なる場合に
`as` キャストを挿入する。
→ **判断ポイント**: 全ての暗黙キャストを自動挿入するか、
安全でないキャスト（truncation）は警告を出すか？

### Tier 2: C 固有の構文・セマンティクスの変換（~280 エラー / ~150 関数）

#### 2-A. 未定義シンボル参照（223 エラー）
**内訳**:
| シンボル | 件数 | 原因 |
|----------|------|------|
| `c_uchar` 型 | 27 | `std::ffi::c_uchar` 未 import |
| `__VA_ARGS__` | 18 | 可変長マクロ引数の未展開 |
| `__errno_location` | 14 | C ライブラリ関数 |
| `strlen`, `strcmp`, `memset` 等 | 20 | C 標準ライブラリ関数 |
| `__builtin_unreachable` 等 | 10 | GCC 組み込み関数 |
| `Perl_croak_memory_wrap` 等 | 9 | bindings.rs に未定義の Perl 関数 |
| `inRANGE_helper_`, `generic_isCC_` | 11 | 内部ヘルパーマクロ |
| `stash`, `sp`, `mg`, `uv` | 14 | マクロ内のローカル変数宣言漏れ |
| `bodies_by_type`, `body_details` | 7 | C static 変数 |
| その他 | ~93 | 各種 |

**根本解決（サブカテゴリ別）**:
- **`c_uchar`**: use 宣言に追加（簡単）
- **`__VA_ARGS__`**: 可変長マクロの展開が不完全。展開ロジックの修正が必要
- **C 標準ライブラリ関数**: extern 宣言を生成するか、対応する Rust 関数に置き換える
- **GCC builtins**: `__builtin_unreachable` → `std::hint::unreachable_unchecked()`,
  `__builtin_ctz` → `.trailing_zeros()` 等の変換ルール追加
- **ローカル変数宣言漏れ**: マクロ本体で宣言されたローカル変数が展開時に失われている。
  `let` 宣言の生成を確認する必要がある
- **内部ヘルパーマクロ**: `ExplicitExpandSymbols` に追加するか、生成対象から除外する

#### 2-B. Enum キャスト（~109 エラー）
**症状**: `non-primitive cast: u32 as perl_core::svtype`
**原因**: C では `(svtype)x` でキャスト可能だが、Rust の enum への直接キャストは不可
**例**:
```rust
// 現状
((*sv).sv_flags & SVTYPEMASK) as svtype

// 期待（選択肢）
// A. std::mem::transmute((*sv).sv_flags & SVTYPEMASK)
// B. svtype::from_u32((*sv).sv_flags & SVTYPEMASK)  // if impl exists
// C. 比較元を整数にキャスト: SvTYPE(sv) == (SVt_PVCV as u32)
```
**根本解決**: bindgen が生成する enum の特性を調べ、最も安全な変換方法を決める。
→ **判断ポイント**: `#[repr(u32)]` enum なら `transmute` でよいか、
それとも安全な変換関数を提供するか？

#### 2-C. 関数呼び出しの左辺値使用（34 + 12 = 46 エラー, E0067 + E0070）
**症状**: `invalid left-hand side of assignment` — `CopLINE(c) += 1`
**原因**: C マクロの戻り値が左辺値（lvalue）として使えるが、Rust の関数は rvalue のみ
**例**:
```rust
// 現状（不正）
CopLINE(c) += 1;          // 関数呼び出し結果への代入
CvFILE(sv) = savepv(...);  // 関数呼び出し結果への代入

// 期待
// → これらのマクロは getter/setter パターンに分離する必要がある
```
**根本解決**: C のマクロが左辺値を返す場合（`*ptr` のようなもの）、
呼び出し側での lvalue 使用を検出し、ポインタの dereference に変換する。
例: `CopLINE(c)` がマクロ展開で `(*c).cop_line` ならば、呼び出し側で
`(*c).cop_line += 1` と展開すべき。
→ **判断ポイント**: lvalue マクロを inline 展開するか、
setter 関数を別途生成するか？

#### 2-D. ビットフィールドアクセサ（22 エラー, E0615）
**症状**: `attempted to take value of method op_type on type perl_core::op`
**原因**: bindgen はビットフィールドに対してメソッド（getter/setter）を生成するが、
codegen はフィールドアクセスとして出力する
**例**:
```rust
// 現状（不正）
(*op).op_type                  // フィールドアクセス

// 期待
(*op).op_type()                // メソッド呼び出し
```
**根本解決**: bindings.rs のビットフィールドメソッド一覧を解析し、
codegen でフィールドアクセスをメソッド呼び出しに変換する。

#### 2-E. goto のラベル変換（11 エラー, E0426）
**症状**: `use of undeclared label 'zaphod32_read8`
**原因**: C の `goto label` を `break 'label` に変換しているが、対応する labeled block がない
**例**: ハッシュ関数内の `goto zaphod32_finalize` → `break 'zaphod32_finalize`
**根本解決**: goto → break 変換は labeled block (`'label: { ... }`) が存在する
場合にのみ有効。switch-case 内の goto は異なるアプローチが必要。
→ **判断ポイント**: ハッシュ関数など goto を多用する関数は生成対象から除外するか、
より高度な goto→structured control flow 変換を実装するか？

#### 2-F. `offset()` on array（52 エラー, E0599）
**症状**: `no method named offset found for array [u8; 256]`
**原因**: C の配列はポインタに decay するので `array + n` ができるが、
Rust の配列には `.offset()` メソッドがない
**例**:
```rust
// 現状
PL_utf8skip.offset(s as isize)

// 期待
PL_utf8skip.as_ptr().offset(s as isize)
// または
*PL_utf8skip.get_unchecked(s as usize)
```
**根本解決**: ポインタ算術変換（1-D）と同時に対応可能。
配列型に対する `.offset()` を `.as_ptr().offset()` に変換する。

### Tier 3: コード品質・慣用性（警告 1,769 件）

現時点では blocker ではないが、最終的には対応すべきもの。

#### 3-A. 不要な unsafe ブロック（大量の警告）
**原因**: `pub unsafe fn` の内部に `unsafe { }` ブロックがある
**解決**: 関数自体が unsafe なので内部の unsafe ブロックは不要。除去する。
(Rust edition 2024 では unsafe fn の本体は暗黙 unsafe ではないため、
内部ブロックが必要。ただし二重ネストは不要。)

#### 3-B. `0 as *mut c_void` → `std::ptr::null_mut()`（110 箇所）
1-A と重複。NULL 変換の一部として対応。

#### 3-C. `MUTABLE_PTR` の展開（85 箇所）
**原因**: `MUTABLE_PTR(x)` はキャスト目的の identity マクロだが、関数呼び出しとして保持されている
**解決**: `ExplicitExpandSymbols` に追加する（簡単）

## 対応の優先順位と戦略

### Phase 1: 低コストで大量のエラーを解消

| タスク | 対象エラー | 推定エラー削減 | 難易度 |
|--------|-----------|---------------|--------|
| `c_uchar` を use 宣言に追加 | E0425 | ~27 | 容易 |
| GCC builtins 変換 | E0425 | ~10 | 容易 |
| ビットフィールド getter 検出 | E0615 | ~22 | 中 |
| `MUTABLE_PTR` 等を展開対象に | E0599 一部 | ~85（警告） | 容易 |

### Phase 2: 型システム変換の根本改修（最も影響大）

| タスク | 対象エラー | 推定エラー削減 | 難易度 |
|--------|-----------|---------------|--------|
| NULL リテラル→ null_mut()/null() | E0308 | ~265 | 中〜高 |
| 整数↔bool 変換 | E0308, E0054 | ~150 | 中 |
| 返り値型の整合性 | E0308 | ~246 | 中 |
| Enum キャスト | E0605 | ~109 | 中 |
| ポインタ算術変換 | E0369, E0368, E0277 | ~230 | 高 |
| 整数幅の暗黙キャスト | E0308, E0277 | ~143 | 高 |

### Phase 3: 構文変換の高度な改修

| タスク | 対象エラー | 推定エラー削減 | 難易度 |
|--------|-----------|---------------|--------|
| lvalue マクロの展開/setter 化 | E0067, E0070 | ~46 | 高 |
| 配列→ポインタ decay | E0599 | ~52 | 中 |
| ローカル変数宣言の修正 | E0425 一部 | ~14 | 中 |

### Phase 4: 生成対象の拡大（CALLS_UNAVAILABLE 等の解消）

| タスク | 推定効果 |
|--------|---------|
| C 標準ライブラリ関数の extern 宣言生成 | CALLS_UNAVAILABLE 削減 |
| `__VA_ARGS__` 展開の修正 | PARSE_FAILED/CODEGEN_INCOMPLETE 削減 |

## 設計判断（決定済み）

### Q1: NULL 変換の実装レイヤー → **A. codegen レイヤー**
`expr_to_rust` で、型コンテキストがポインタの場合に `0` → `null_mut()` 変換。

### Q2: ポインタ算術の変換方針 → **A. `.offset()` ベース**
`p + n` → `p.offset(n as isize)`（C と同じ unsafe セマンティクス）。

### Q3: Enum キャストの方針 → **A. `std::mem::transmute`**
最もシンプル。unsafe fn 内で使用するため問題なし。

### Q4: lvalue マクロの対応 → **A. inline 展開**
`CopLINE(c) += 1` → `(*c).cop_line += 1` のように、
lvalue 使用箇所でマクロを inline 展開する。

### Q5: goto を使う関数の扱い → **A. 生成対象から除外**
goto を使う関数（ハッシュ関数等）は生成対象から除外する。
