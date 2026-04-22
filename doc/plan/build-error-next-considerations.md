# 残 56 件のエラーと改良のための考察

**現状**: errors 56 / warnings 55（セッション開始時 errors 160 / warnings 825 から
-65% / -93%）。残りは単発対応では引けない、より深い設計判断を伴う項目が中心。

## エラー分類サマリ

| 件数 | 代表パターン | 根本原因の層 |
|------|-------------|-------------|
| 17 | `types differ in mutability` | wrapper 引数/戻り値の const 伝搬 |
| 5 | `RXp_EXTFLAGS(...) &= mask` (E0067) | macro body の param 名と template 不整合 |
| 5 | `*mut c_char` vs `*mut i8` 戻り値 | `c_char` ≡ `i8` のエイリアス判定 |
| 3 | `block . oldcomppad` 上の E0610 | マクロパラメータ型の推論不足 |
| 3 | `if { u32 } else { null_mut() }` | 三項型ヒントが引数型まで伝搬しない |
| 2 | `*const sv` vs `*mut gv` (GvGP) | call-form 維持で arg cast が効かない |
| 単発 | transmute/bool+=usize/HEK_LEN= 等 | 個別イディオム |

---

## カテゴリ A: 関数引数の const/mut ミスマッチ（10 件）

### 具体例

```rust
pub unsafe fn Perl_caller_cx(my_perl: *mut PerlInterpreter, a: c_int, b: *mut *const COP) -> ... {
    Perl_caller_cx(my_perl, a, b)   // callee: b は *const *const COP
}
```

Wrapper の param 宣言が `*mut *const COP`、FFI callee 側は `*const *const COP`。

### 考察

- **wrapper の param 型がどこから来るか**: `param_type_only` が
  `apply_simple_derived_with_specs_const` 経由でソースの const を
  反映するように修正したが、**すべての Perl API は `*mut` で
  受けるケース**が多い（wrapper を呼び出すユーザが mutable に使う可能性が
  あるから）。呼出側で `as *const T` キャストが必要。
- **対策候補**:
  1. **arg cast に const/mut 不一致を一般化**: 現在 `cast_arg_syn_if_needed`
     は SV サブタイプと void 互換のみ as-cast する。純粋な const/mut 違い
     (例: `*mut *const COP` → `*const *const COP`) では cast されない。
     同じ `inner_type()` で const 違いならキャストを入れる分岐が必要。
  2. **wrapper の param 宣言を callee 準拠に寄せる**: bindings.rs の
     callee が `*const T` なら wrapper も `*const T`。ただしこれは
     既存マクロ API の互換を壊す可能性（呼出側コードが `*mut` を渡す）。

案 1 が安全で副作用が小さい。**影響範囲は 17 件中の半分以上**。

---

## カテゴリ B: 戻り値型 `*mut c_char` vs `*mut i8`（5 件）

### 具体例

```rust
pub unsafe fn RX_WRAPPED_const(rx_sv: *const SV) -> *mut c_char {
    body  // 実際の型は *mut i8
}
```

### 考察

- Rust では `c_char` と `i8` は **同じ型**だが、型推論上は別名。
- `normalize_integer_type` は `c_char` と `i8` を同一視する（どちらも `"i8"`）が、
  **ポインタ内側の型を比較する際には inner.to_rust_string() が "c_char" と "i8" で
  異なる文字列を返す**ため、wrapper 戻り値の cast 判定が失敗している。
- **対策**: `cast_return_syn_expr_if_needed` / `cast_arg_syn_if_needed` に
  「ポインタ内の型名を normalize してから比較」する分岐を追加する。
  `*mut c_char == *mut i8` / `*const c_int == *const i32` 等を一斉に吸収。

同時に `c_int`/`i32`, `c_uint`/`u32`, `c_ulong`/`u64` など他の C エイリアスも
同様のパス。単純な case 1 個で複数解消する見込み。

---

## カテゴリ C: 関数型マクロを lvalue に置く（5 件 E0067）

### 具体例

```rust
RXp_EXTFLAGS(ReANY(rx_sv as *const SV) as *const regexp) &= !(1u64 << ...) as u32;
```

C の原型:
```c
#define RXp_EXTFLAGS(rx)  ((rx)->extflags)
RXp_EXTFLAGS(x) &= mask;   // 展開後は ((x)->extflags) &= mask; で valid
```

### 考察

- `try_expand_call_as_lvalue_syn` は param 置換で body を inline 展開する
  仕組みがあるが、**macro_info.params[0].name = "rx" なのに body は
  `Ident("prog")` を使う**という整合性破綻。
- 原因: マクロ body の推論が、`RXp_ISTAINTED(prog)` を処理する過程で
  `RXp_EXTFLAGS(prog)` を展開して body を「prog 版」として cache し、
  `RXp_EXTFLAGS` の body に保存したため。**template ではなく特定呼出の
  expansion**を保持している。
- **対策 A (深い)**: macro inference が body を template（元 param 名を
  保持）として格納するよう変更。既存の推論ロジックに影響大。
- **対策 B (軽量)**: body が params を参照していないケースのみ、
  free identifier を positional に置換するフォールバック。
  前回試みて他の macro で誤発火したため、フォールバック条件を厳しく
  する必要（例: body が single ident ひとつだけ使う、かつその
  ident が known_symbols に無い）。
- **対策 C (削減)**: lvalue で使われる 5 件を特定し、`apidoc` の
  `skip_codegen` パッチで回避（応急）。

---

## カテゴリ D: macro パラメータの型誤推論（3 件 E0610）

### 具体例

```rust
pub unsafe fn CX_CURPAD_SAVE(my_perl: *mut PerlInterpreter, block: c_int) -> () {
    block.oldcomppad = (*my_perl).Icomppad;  // block は struct のはず
}
```

### 考察

- `block` は C では struct 型（`PERL_CONTEXT` 等）のはずだが、
  マクロパラメータの型推論で `c_int` と判定された。
- `block . oldcomppad` のような **Member アクセスがある時点で、
  base の型は struct でなければならない**という強い制約があるが、
  Phase 2 の型推論ではこれを活用できていない可能性。
- **対策**: `collect_expr_constraints` で Member 式の base 型に
  「フィールドを持つ型」制約を付け、Tier を高める。具体的には
  fields_dict に `oldcomppad` を持つ struct を検索し、それを
  Named 制約として加える。`reverse-type-inference-from-field-access.md`
  の既存プランと関連。

---

## カテゴリ E: 三項式の型ヒントが引数型まで伝搬しない（3 件）

### 具体例

```rust
newSVpvn_flags(my_perl, s, len, if u != 0 { SVf_UTF8 } else { std::ptr::null_mut() })
// 第 4 引数は U32 だが、Conditional の else 枝が null_mut で *mut _ 型
```

### 考察

- 現在 `build_syn_expr` の Conditional arm は `self.current_return_type`
  を type_hint として使う。これは関数の戻り値型。
- 引数位置の Conditional では **expected 型は callee の param 型**だが、
  この情報を build 時に渡していない。
- **対策**: `build_arg_string_unified` で callee の expected type を
  取得した後、arg が Conditional なら `build_syn_expr` に expected type を
  push-down する仕組みを導入。現在 `build_syn_expr_with_type_hint` という
  API が存在（generate_macro 冒頭で使用）— これを arg 位置でも使えるよう
  拡張。

---

## カテゴリ F: `GvGP(Idefgv)` 呼出で arg cast が効かない（2 件）

### 具体例

```rust
{ SvREFCNT_dec(my_perl, GvSV((*my_perl).Idefgv as *const SV));
  { (*GvGP((*my_perl).Idefgv)).gp_sv = ... } }
//       ^^^^ Idefgv は *mut gv だが GvGP は *const sv を期待
```

### 考察

- `GvSV(... as *const SV)` は cast が入っているのに、`GvGP(...)` は cast なし。
- `GvGP` がマクロで lvalue 文脈でも使われるため、その経路では
  `try_expand_call_as_lvalue_syn` → macro body inline 展開に切替わる。
  展開時に arg の cast が落ちる。
- **対策**: `try_expand_call_as_lvalue_syn` で arg を syn にする前に
  `cast_arg_syn_if_needed` を適用する。または inline 後の再 cast。

カテゴリ C と同じ経路の延長線上。

---

## カテゴリ G: 単発イディオム（個別対処）

### G.1 transmute size mismatch
```rust
std::mem::transmute::<_, regex_charset>((flags as u64 & ...) >> ...)
```
`u64` から 32bit enum への transmute は invalid。
**対策**: `transmute` を避け、`as i32 as enum_type` や
`unsafe { core::mem::transmute::<i32, regex_charset>(val as i32) }` に。

### G.2 `count += !(bool_expr)` 
```rust
count += !((...) < (...))   // C で count += (condition ? 1 : 0) のイディオム
```
**対策**: `+= bool` パターンを検出し、`+= (bool as usize)` に変換。
または `if expr { 1 } else { 0 }` 形式に展開。

### G.3 `svt_get != 0` (Option<fn>)
```rust
(*(*mg).mg_virtual).svt_get != 0   // Option<unsafe fn(...)>
```
**対策**: Binary `Ne/Eq` で lhs 型が `Option<...>` (fn pointer alias) なら
`.is_some()` / `.is_none()` に変換。

### G.4 `HEK_LEN(HeKEY_hek(he)) = -2` (E0070)
関数呼び出し型マクロの代入。**対策**: カテゴリ C と同じ経路で、
`HEK_LEN` も lvalue 展開対応。

### G.5 `*len = tmps.offset_from(...)` isize → usize
`*len` は `*mut STRLEN = *mut usize`。`offset_from` は isize。
**対策**: Assign で RHS が offset_from call なら as-cast を LHS 型に。

### G.6 `*retlen = expectlen` u64 → usize
似たケース。Assign の integer cast が u64 と usize を同等視してしまう。
`integer_types_compatible` は両者を true にするため cast 省略。
**対策**: assign で LHS と RHS の文字列が `"usize"` と `"u64"` のような
Rust 的に distinct なら、compatibility を緩く判定せず cast を入れる。

### G.7 `while x < e` / `offset(...) <= e`
`x: *mut U8` vs `e: *const U8`。両ポインタ比較は const/mut 不問と思いきや、
Rust では「内側の型が同じで const/mut 違い」だと比較できない。
**対策**: Binary 比較で const/mut 違いを検出し、どちらかを cast。

### G.8 `(*sv_).xnv_u.xnv_nv = 0` （float assign に integer）
前回 fix を試みたが build_assign_stmt 経由ではなかった。実態は
Assign 式の Block expression 内部。
**対策**: `build_syn_expr` の Assign arm でも float LHS + IntLit → float lit
変換を適用。

### G.9 `memchr(...)` arg `*const i8` 期待 `*const c_void`
libc_fn_param_type は `*const c_void` を返すが、文字列リテラルの
`.as_ptr()` が `*const i8`。**対策**: c_char* → c_void* cast 挿入
(ポインタ種の自動 cast 拡張)。

### G.10 `cophh_fetch_pvs(... key.as_ptr() as *const c_char ...)`
wrapper の `cophh_fetch_pvs` は 3rd arg を `&str` に変換している
(literal_string_param 扱い)。callee が wrapper なので expected=&str だが
呼出側は `.as_ptr() as *const c_char` を渡してしまう。
**対策**: cast_arg_syn_if_needed で expected が `&str` なら生の
ident（literal string param）を渡す経路を保持。

---

## 設計レベルで着手すべき共通基盤

### 1. 型推論の expected-type 伝搬

現在の `build_syn_expr` は「text-up」で型を決める。一方、Rust の型検査は
「上から下」にも流れる（expected type from context）。

**提案**: `build_syn_expr_with_type_hint` を generate_macro 以外の
ポイント（引数位置、代入 RHS、三項枝、return）でも活用。
`self.current_expected_type: Option<String>` を push/pop 方式で管理。

影響範囲: カテゴリ E 全件、G.5/G.6、null pointer 変換の残ケース。

### 2. ポインタ型エイリアス同一視

`c_char` / `i8`、`c_uchar` / `u8`, `c_int` / `i32`, `c_long` / `i64`,
`c_ulong` / `u64`, `size_t` / `usize` を型比較レベルで同一視する
ヘルパを導入（`pointer_inner_compatible`）。

影響範囲: カテゴリ B 全件、G.7 と G.9 の一部。

### 3. Lvalue 展開経路の整備

カテゴリ C の根本修復。`try_expand_call_as_lvalue_syn` を正しく
template ベースに戻すには、以下のどちらかが必要:

- (3a) macro inference の `body` 格納を template 化。
  `macro_defuse-inference-ordering.md` や
  `concurrent-leaping-petal.md` で扱った constraint ルーティングの
  延長として検討。
- (3b) `try_expand_call_as_lvalue_syn` 内で、arg 型推論と body の
  ident を positional マッピングするフォールバック。前回失敗したので、
  `params.len() == body_free_idents.len() && body_is_single_place_expr`
  のような強い条件で限定的に有効化する。

影響範囲: カテゴリ C（5件）、F（2件）、G.4（1件）。

### 4. Option<fn> 判定の統一

`is_pointer_expr_unified`, `infer_expr_type_unified`, bool 変換パスが
現在、function pointer (Option 型) を pointer として扱ったり扱わなかったり
混乱している。共通ヘルパ `is_function_pointer_type` を導入し、
- 比較 `== 0` / `!= 0` → `.is_none()` / `.is_some()`
- 真偽文脈の wrap → `.is_some()`

で一貫化。

### 5. Phase 2 の Member 制約を強化

カテゴリ D の対策として、`collect_expr_constraints` が Member/PtrMember を
見たら base 式に「その member を持つ struct 型」制約を付ける。

`reverse-type-inference-from-field-access.md` に既存プランがあり、
整合を取る。

---

## 優先順位

| # | 対象 | 種別 | 予想工数 | 削減件数 |
|---|------|------|----------|----------|
| 1 | ポインタ型エイリアス同一視 (B, G.7, G.9) | 共通基盤 | 小 | 6-8 |
| 2 | 引数位置の const/mut cast (A) | arg cast 拡張 | 小 | 10 |
| 3 | Option<fn> 判定統一 (G.3) | 共通基盤 | 小 | 1-3 |
| 4 | offset_from / u64=usize 明示 cast (G.5, G.6, G.7) | assign cast | 小 | 2-3 |
| 5 | type_hint の arg/ternary 伝搬 (E) | 設計拡張 | 中 | 3-5 |
| 6 | Member 式による struct 型推論 (D) | Phase 2 拡張 | 中 | 3 |
| 7 | Lvalue macro 展開 (C, F, G.4) | macro inference 修正 | 大 | 6-8 |
| 8 | transmute size mismatch (G.1) | 個別 | 小 | 1 |

**1-4 は比較的浅い修正で合計 20 件程度**を取れる見込み。
**5-7 は設計変更を伴うが、これをクリアすると残りは単発のみ**になる。
最終的には 56 → 10-15 件程度まで削減可能と推定。

---

## 既存プランとの関連

- `doc/plan/e0308-null-pointer-improvement.md` — カテゴリ E の一部
- `doc/plan/fix-const-mut-mismatch.md` — カテゴリ A 既存
- `doc/plan/reverse-type-inference-from-field-access.md` — カテゴリ D
- `doc/plan/concurrent-leaping-petal.md` — Phase 2 の constraint 優先
  (既知課題)
- `doc/plan/macro-defuse-inference-ordering.md` — カテゴリ C/F の深部

新たな計画ではなく、これらを統合した方針として進めるのが効率的。
