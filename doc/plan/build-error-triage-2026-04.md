# Plan: 統合ビルドエラー 160件 + 不要括弧 88件の triage と改良計画

## Context

`tmp/build-error.log` (2026-04-19 時点、`libperl-sys` の `macro_bindings.rs` ビルド結果):

- **errors**: 160 (全て rustc エラーで `cargo build` 失敗)
  - うち 1 件はエラーコードのない `error: cast cannot be followed by a method call`
- **warnings**: 825
  - うち **682 件** は Rust 2024 edition の `E0133 unsafe-op-in-unsafe-fn`
    (`#[warn]` だが将来的に error 化)
  - **unnecessary parentheses 系 103 件** (block return 56 + if cond 32 +
    assigned value 14 + while cond 1)

本プランは **エラーをカテゴリ別に根本対処** し、波及で warning も減らす
順序と方針を示す。TinyCC 参照ルール (`CLAUDE.md`) と Phase 2/3 分離ルール
に沿って書く。

## ゴール

1. **エラーを 160 → 30 以下**（残りはアーキテクチャ改変が必要な残課題に
   限定）
2. **コード無しエラー** `cast cannot be followed by a method call` を解消
3. **不要括弧警告 103 件 → 20 件以下**（`static_array_emitter` の
   正規化で大半が消える見込み）
4. **unsafe-op-in-unsafe-fn 682 件** を 1 件の方針決定で一掃（`unsafe`
   ブロックで本体を包む or crate 属性で allow）

## エラー分類サマリ

エラーコード別の内訳（既に `sort | uniq -c` で集計済み）:

| # | Code | Count | 概要 |
|---|------|-------|------|
| 1 | E0308 `mismatched types` | 81 | 型一致。内訳多数 (下記サブカテゴリ) |
| 2 | E0600 `cannot apply unary '-' to usize` | 14 | `-1 as size_t` パターン |
| 3 | E0308 `if and else have incompatible types` | 9 | 三項/条件の型不一致 |
| 4 | E0308 `arguments to this function are incorrect` | 7 | 引数型不一致 (memcpy, memset 等) |
| 5 | E0061 `takes 1 argument but 0 supplied` | 7 | `Perl_croak_memory_wrap()` 0引数呼出しに `(_: ())` 引数の食い違い |
| 6 | E0067/E0070 `invalid left-hand side of assignment` | 9 | bit field setter `(* o) . op_moresib () = 0` |
| 7 | E0610 `primitive type doesn't have fields` | 3 | `block . oldcomppad`：`block` が `i32` 扱い |
| 8 | E0609 `no field 'arena' on body_details` | 3 | bitfield 名直接アクセス（bindgen は accessor メソッド） |
| 9 | E0599 `no method 'offset' found for array` | 5 | 配列型フィールドを pointer として `.offset()` |
| 10 | E0600 `cannot apply '!' to *mut u8` / `*const u8` / `*mut c_void` | 4 | `!ptr` パターン |
| 11 | E0277 `u64 & usize`, `u64 * usize`, `usize - u64`, `usize * u64`, `u8 * usize`, `u8 + u64` | 5+ | size_t/UV/STRLEN の型不統一 |
| 12 | E0308 `can't compare &CStr with {integer}` | 1 | `c"..." != 0` |
| 13 | E0369 `cannot subtract *mut u8 from *mut u8` | 1 | pointer - pointer |
| 14 | E0369 `cannot add svtype to [body_details; 17]` | 1 | `bodies_by_type + r#type` |
| 15 | E0369 `regex_charset << {integer}` | 1 | enum に `<<` |
| 16 | E0277 `u32 |= svtype` | 1 | enum を u32 に OR-assign |
| 17 | E0308 `expected *const i8, found &CStr` | 複数 | C 文字列リテラルを FFI に渡す |
| 18 | E0512 `transmute` size mismatch | 1 | まだ詳細未確認 |
| 19 | コード無し `cast cannot be followed by a method call` | 1 | `!memchr(...) as *const c_char.is_null()` |

### E0308 `mismatched types` (81件) のサブカテゴリ

| サブ | 代表例 | 推定件数 |
|------|--------|----------|
| A. `isize` vs `i64`/`i32`/`Stack_off_t` | `offset_from`、`xav_fill` など | 15-20 |
| B. `*mut T` vs `*const T` の `let` 束縛 | `let s: *mut U8 = s0;` | 10-15 |
| C. `u64` vs `usize` / `u8` vs `usize` / `u8` vs `u64` | `size_t` 混在 | 15-20 |
| D. `*mut c_void` vs `*mut i8` | `new_body = (new_body as *mut c_char).offset(...)` | 5-8 |
| E. `*const i8` vs `&CStr` (引数) | `Perl_croak_nocontext(c"...", ...)` | 3-5 |
| F. `expected u64, found u8` (配列要素) | `PL_utf8skip.as_ptr().offset(...)` | 3-5 |
| G. `usize` vs `isize` on return / assign | `return s.offset_from(s0);` | 3-5 |
| H. `Option<fn>` vs `{integer}` | `(*mg_virtual).svt_get != 0` | 2-3 |
| I. `break;` vs expected type | `unreachable_unchecked(); break;` 型不整合 | 3 |

## 改良計画（優先順）

### P0: 土台のクリーンアップ

#### P0.1 Rust 2024 `unsafe-op-in-unsafe-fn` (~682 warning 一掃)

全てのマクロ・inline fn 生成は `pub unsafe fn` だが、本体内の
`*ptr`, `ptr.offset(...)` に個別 `unsafe {}` が無い。2024 edition の
lint。対処案:

1. **案A**: `macro_bindings.rs` の先頭に `#![allow(unsafe_op_in_unsafe_fn)]`
   をファイルレベルで付ける（一行で 682 件消える）
2. **案B**: 全関数の body を丸ごと `unsafe { ... }` で包む
3. **案C**: AST 走査でポインタ操作ノードごとに `unsafe {}` を挿入

短期は **A が最もコスパが良い**。既存コードの一部は既に `unsafe { ... }`
ブロックで包まれており、それらが逆に `unnecessary unsafe block` 警告に
なっているものもある (10件)。まず allow で一掃し、長期的に警告を
クリーンに保つ方針は別途検討。

**変更箇所**: `src/rust_codegen.rs` の先頭出力（ヘッダ）に
`#![allow(unsafe_op_in_unsafe_fn)]` を追加。

#### P0.2 `Perl_croak_memory_wrap` 等 void 引数 inline fn の修正 (7件)

`void Perl_croak_memory_wrap(void)` → 生成 `fn Perl_croak_memory_wrap(_: ())`
になり、呼出側は `Perl_croak_memory_wrap()` で 0 引数になるため E0061。

**根本対処**: `src/inline_fn.rs` または `rust_codegen.rs` の引数処理で、
C の `(void)` 単独パラメータ（=引数なし）を Rust の `()` 引数に変換
しないようにする。TinyCC 同様「`(void)` はゼロ引数と同義」と解釈。

**TinyCC 参照**: `tccgen.c` で `declaration_type_start` / `parse_btype`
が `void` を特殊扱いしているので確認。

#### P0.3 不要括弧警告 (56 + 32 + 14 + 1 = 103件)

ほぼ全てが `static_array_emitter.rs` の `bodies_by_type[]` 初期化子
から発生。エミッタが `translate_const_expr` で `({} {} {})` や
`(if (...) ...)` など naive に括弧を付けるが、`normalize_parens`
(`src/syn_codegen.rs:307`) を通していない。

**修正**: `static_array_emitter::emit_one_array` の最終 struct literal
文字列を組み立てる直前、各フィールド値 `val_rust` を
`crate::syn_codegen::normalize_parens(&val_rust)` に通す。同様に
`_bitfield_{N}: (a) | (b) | ...` も正規化。

これで 56+32+14 ≈ **~100件が一度に解消**する見込み。

### P1: 高頻度の型問題を潰す（直球の codegen 修正）

#### P1.1 `- 1 as size_t` → `usize::MAX` (E0600 ×14)

C の典型イディオム `((size_t)-1)` は「最大値」を意味する。これを
そのまま Rust `- 1 as size_t` に訳すと `- 1usize` で失敗。

**対処**: AST レベルで `Cast { expr: UnaryMinus(IntLit(1)), type: unsigned }`
パターンを検出して `usize::MAX` / `u64::MAX` 等に変換。TypeRepr で
unsigned 判定し、対応する `MAX` 定数を出す。

場所: `src/rust_codegen.rs` の Cast 式出力、もしくは式生成時の専用
シムで上位ノード `UnaryMinus(IntLit)` + 外側 Cast をまとめて検出。

#### P1.2 `!ptr` → `ptr.is_null()` (E0600 ×4)

文脈: `if !s < e_ { break; }`, `if !s > start && ...`,
`if !x.offset(N) <= e`, `!memchr(...) as *const c_char.is_null()`
(cast-method-call問題とも関連)。

C の `!ptr` は「ptr == NULL」の意味。Rust 移植では

- 単独 `!ptr` → `ptr.is_null()`
- `!ptr && rest` / `!ptr || rest` → `ptr.is_null() && rest`
- `!ptr < e` 等の比較混在は元々の C 意図が曖昧だが、ほとんどの実
  Perl コードは `!(ptr < e)` のつもりで書かれている可能性が高い。
  **該当関数を手動検査し、括弧の取り違いかどうか確認** した上で
  対処を決める。

**対処案**: Phase 2 (`semantic.rs`) で `UnaryNot` の被演算子型が
ポインタかを判定し、Phase 3 で `is_null()` を emit。

`!memchr(...) as *const c_char.is_null()` の 1件は **cast が method
呼出より先** で、パース自体できていない。cast を emit する際に
メソッド呼出が続くなら括弧を付けるロジックが必要:

```
(expr as T).method()  ← 正しい
expr as T.method()    ← 誤り（method が T に対して解釈される）
```

`build_syn_expr` で Cast の外が MethodCall なら cast を Paren で包む。

#### P1.3 size_t / STRLEN / U64 の型統一 (E0277 複数、E0308 type C)

`std::mem::size_of::<T>()` は Rust では常に `usize` を返す。
Perl の `size_t` / `STRLEN` / `U64` は `u64` として bindings.rs に
出ている（`size_t = u64` alias）ため、`size_of::<T>() * (x as U64)`
で型ミスマッチが発生。

**対処**: codegen 内で以下のどちらかを採る:

- a. `size_of::<T>()` の返り値に自動で `as size_t` を付ける
- b. `size_t` / `STRLEN` を `usize` として扱い、関数境界のみ `u64` 変換

(a) が侵襲度低。Phase 2 で expr の型を推論する際、
`ExprKind::SizeofType` / `ExprKind::Sizeof` の結果型を `usize` として
明示し、算術演算子で `size_t` 側と混ざる場合に Phase 3 で cast を挿入。

#### P1.4 `offset_from` 戻り値型合わせ (E0308 サブ A, G / 約 10件)

`ptr.offset_from(other)` は `isize`。ターゲット型が `i64`/`i32`
(例: `Stack_off_t`) や `usize` の場合、`as i64` / `as usize` を
自動付加。

**対処**: Phase 2 の `infer_expr_type` で `offset_from` / 返り値が
`isize` の API 呼出しを特定し、代入・引数の target type と異なる
場合に Cast を自動挿入する設計を導入。

### P2: ポインタ / 配列 / bit-field の構造的誤り

#### P2.1 `*mut` vs `*const` 束縛の不一致 (E0308 サブ B, ~10件)

`let s: *mut U8 = s0;` で `s0` が `*const U8`。
C 側では `const` 外しが暗黙だが Rust では明示 cast が必要。

**対処**: Phase 2 で `let` の初期化子型と宣言型を突き合わせ、
ポインタ const/mut のみ違う場合に `as *mut T` / `as *const T`
cast を挿入。既に `fix-const-mut-mismatch.md` がある（進行中）。

#### P2.2 配列フィールドへの `.offset()` (E0599 ×5 / E0369 ×1)

```rust
(*my_perl).Ifold_locale.offset(*b as isize)    // [u8; 256] に .offset()
(*my_perl).Ibody_roots.offset(sv_type as isize) // [*mut c_void; 17]
bodies_by_type . offset (sv_type as isize)      // [body_details; 17]
bodies_by_type + r#type                          // same
```

C の `pointer + index` / `pointer[index]` を Rust の `.offset()` に
翻訳するロジックが、対象が配列でもポインタでも同じになっている。

**対処**: Phase 2 の型推論で「配列型」を識別し、Phase 3 で
`arr.as_ptr().offset(i)` または `arr[i as usize]` に切り替える。

- Rust `RustDeclDict` (bindings.rs パース) から配列型情報を取れる
- 自家生成の `bodies_by_type` は `static_array_emitter` が出して
  いるので、codegen の型表にも登録する

#### P2.3 `a - b` (pointer - pointer) → `offset_from` (E0369 ×1)

`Perl_utf8_hop(hopped, *lenp as isize) - hopped` を
`Perl_utf8_hop(...).offset_from(hopped)` に変換。

**対処**: Phase 3 で `Binary { op: Sub, lhs: ptr, rhs: ptr }` を
判定し `offset_from` に書き換え。既存の cast パスに近い。

#### P2.4 bit-field `.field` 参照 / 代入 (E0609 ×3 / E0070 ×4 / E0067 ×5)

- 参照: `(*type_details).arena` → `(*type_details).arena()`
- 代入: `(*o).op_moresib() = 0;` → `(*o).set_op_moresib(0);`

bindgen が bit-field にアクセサを生成していることを `RustDeclDict` の
struct 情報から検出 (既に `fields_dict` が bit-field を認識)。

**対処**: Phase 2 の `fields_dict` で struct member の `BitField`
フラグを参照できるようにし、Phase 3 で:
- 参照 `a.bf` → `a.bf()`
- 代入 `a.bf = v` → `a.set_bf(v)`

これは比較的局所的な修正で 12件解消。

#### P2.5 `block . oldcomppad` / `i32 に field` (E0610 ×3)

`{ block . oldcomppad = (* my_perl) . Icomppad ; block . oldcomppad }`
の `block` が `i32` と推論される。`block` は predeclared なマクロ
パラメータ名（`CX_CUR()` 内の `block` かも）。

調査必要: `block` という名前のマクロパラメータ / 識別子がどこから
`i32` 型として入ってきているか。Phase 2 の型推論が誤っている。
対象マクロを `--dump-types-for` で検査する。

### P3: 条件式 / 特殊キャスト

#### P3.1 三項の型不一致 (E0308 `if and else`, 9件)

パターン A: `if !GvEGV(gv).is_null() { GvEGV(gv) } else { gv } as *const SV`
- `GvEGV` 戻り値 `*const SV`、`gv` は `*mut gv`
- **対処**: else 分岐にも `as *const SV` cast を挿入、または外側 cast を
  両分岐の前に分配

パターン B: `if HeKUTF8(he) != 0 { SVf_UTF8 } else { std::ptr::null_mut() }`
- then: `u32` (SVf_UTF8)、else: `*mut _` (null)
- **対処**: C の `?: 0` では then が整数なら else の `NULL` も `0`。
  Rust では `if cond { SVf_UTF8 } else { 0 }` に書き換える必要がある。
  すでに `e0308-null-pointer-improvement.md` プランがある。

パターン C: `xav_fill + 1 as usize` vs `i64 path`
- 数値昇格の不整合。Phase 2 で統一化する必要。

#### P3.2 CStr vs `*const i8` (E0308 E, 複数)

`Perl_croak(my_perl, c"panic: ...", ...)` で引数が `*const c_char` を
期待しているのに `&CStr`。Phase 3 で FFI 呼出しの引数が
文字列リテラル (`c"..."`) なら `.as_ptr()` を付ける。

**対処**: `get_callee_param_type_extended` の結果型が `*const c_char`
で実引数が `ExprKind::StringLit` なら `.as_ptr()` 付加。既に
`literal-string-conversion.md` プランあり（未完了）。

#### P3.3 `c"UNREACHABLE" != 0` (E0277)

`assert(cstr != 0)` パターン。C の「非NULL チェック」だが文字列
リテラルは常に非 NULL。**対処**: Phase 3 の assert 展開で、
「文字列リテラル != 0」を `true` または削除に変換（コード無くして良い）。

#### P3.4 enum への `<<` / `|=` (E0369 / E0277)

- `cs << 0 + 7` (`cs: regex_charset`) → `(cs as i32) << (0 + 7)`
- `(*sv).sv_flags |= r#type` (`type: svtype`) → `|= r#type as u32`

Phase 2 で enum 被演算子を検出し Phase 3 で cast 挿入。

### P4: その他の小さな穴

- **P4.1** `memset(new_body as *mut c_char, ...)` → `memset(new_body as *mut c_void, ...)`。libc の memset/memcpy の引数型は `*mut c_void` / `*const c_void`。既存の cast パスが「既に cast があると再 cast しない」ポリシーなら、FFI 側が *c_void を要求するなら `as *mut c_void` に訂正する優先度を上げる。
- **P4.2** `Option<unsafe extern "C" fn(...)>` != 0 → `.is_some()` に変換（マクロ展開後の `(*mg_virtual).svt_get != 0`）。
- **P4.3** `unreachable_unchecked(); break;` の後続 break が type mismatch。`break` が return型 `usize` に合わない。assert / unreachable の後の dead code を除去するか、stmt を分離する。
- **P4.4** `new_body: *mut c_void = ... as *mut c_char` — assign 型と初期型のズレ。Phase 2 で target type に合わせ最外側 cast を追加。

## 実施順序

| Step | タスク | 期待改善 | 工数目安 |
|------|--------|----------|----------|
| 1 | P0.1 `#![allow(unsafe_op_in_unsafe_fn)]` 追加 | warning -682 | 30分 |
| 2 | P0.3 `static_array_emitter` に `normalize_parens` 適用 | warning -90前後 | 1h |
| 3 | P0.2 `void` 引数 inline fn 生成修正 | error -7 | 1h |
| 4 | P1.1 `- 1 as size_t` → `usize::MAX` | error -14 | 2h |
| 5 | P1.2 `!ptr` → `is_null()` / cast-before-method paren | error -5 | 3h |
| 6 | P2.4 bit-field accessor/setter | error -12 | 3h |
| 7 | P1.3 size_t 型統一（簡易版） | error -15前後 | 4h |
| 8 | P1.4 `offset_from` 戻り値 cast | error -10前後 | 3h |
| 9 | P2.1 `*mut`/`*const` 束縛 cast | error -10 | 3h |
| 10 | P2.2 配列フィールド offset | error -6 | 2h |
| 11 | P3.2 CStr `.as_ptr()` 補完 | error -5 | 2h |
| 12 | P3.1 三項型不一致（パターン別） | error -9 | 4h |
| 13 | P3.4 enum cast | error -3 | 1h |
| 14 | 残り (P2.3, P2.5, P3.3, P4系) | error -10 | 4h |

ステップ 1-6 までで「**大カテゴリの簡単なもの**」を一掃し、
error 約 40件 / warning 約 780件 を除去する想定。7 以降は Phase 2 への
ロジック追加が必要で慎重に進める。

## 検証方法

各ステップ後:

```bash
# unit tests (299 + 11 ignored)
cargo test

# 生成
cargo run -- samples/xs-wrapper.h --auto --gen-rust \
    --bindings samples/bindings.rs 2>/dev/null > /tmp/gen.rs

# 統合ビルド
~/blob/libperl-rs/12-macrogen-2-build.zsh

# エラー/警告カウント
grep -cE '^error' tmp/build-error.log
grep -cE '^warning' tmp/build-error.log
```

## アーキテクチャメモ (CLAUDE.md 準拠)

**Phase 2 (`semantic.rs` / `macro_infer.rs`) に配置すべき分析**:

- P1.1 の `- 1 as unsigned` 検出（AST 正規化）
- P1.2 の `!ptr` のポインタ判定 & `is_null()` 変換マーク
- P1.3 の size_t 統一型推論
- P1.4 の `isize` → target 型 cast 必要性判定
- P2.1 の const/mut 不一致 cast 判定
- P2.2 の 配列型 vs ポインタ型判定
- P2.4 の bit-field フラグ伝搬
- P3.4 の enum 被演算子検出

**Phase 3 (`rust_codegen.rs`) はそれらを受けて emit するだけ**。
現状の technical debt (`collect_must_mut_pointer_params` 等) と
同様に、Phase 2 に寄せる方向で実装する。

## リスク

1. **Phase 2 への分析追加は他マクロに波及**する可能性がある。各ステップ
   でユニットテスト + 生成差分 diff でリグレッション監視。
2. **`#![allow(unsafe_op_in_unsafe_fn)]` は技術的負債**を作る。長期的には
   Phase 3 でポインタ操作ごとに `unsafe {}` を挟む対応が望ましいが、
   本 PR では先送り。
3. **`!ptr` の意図曖昧ケース** (`!s < e`) は元 C コードが `(!s) < e` の
   つもりか `!(s < e)` のつもりか文脈判断が必要。人手確認の上で
   `samples/xs-wrapper.h` の該当行を注視する。
