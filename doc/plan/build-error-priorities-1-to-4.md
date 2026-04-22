# 優先 1-4 実装計画（残エラー削減 第二弾）

**起点ドキュメント**: [build-error-next-considerations.md](build-error-next-considerations.md)
**現状**: errors 56 / warnings 55
**目標**: 1-4 を順次適用して 35-40 件前後まで削減（-16〜-20 見込み）

本計画は 4 つの独立したサブタスクを **依存順** に並べる。各タスクは
- 既存コードの対象箇所
- 追加/変更するヘルパ
- 期待する削減件数
- 回帰テストへの影響
- コミット単位
を明示する。**1 タスク = 1 コミット** を原則とする。

---

## タスク 1: ポインタ型エイリアス同一視（B, G.7, G.9）

### ゴール
`*mut c_char` と `*mut i8`、`*const c_int` と `*const i32`、`*mut U8` 同士の
const 違いを「内側同一・const 違いなら cast」「内側のみ違いでも正規化後同一
なら cast 不要」と一貫判定する。

### 背景（既存コード調査）
- `normalize_integer_type` (src/rust_codegen.rs:714) は
  `c_char → i8`, `c_int → i32`, `c_ulong → u64` などを既に吸収している。
- `UnifiedType::to_rust_string` は `Char/Int` を `c_char/c_int/c_ulong` 形
  で返す。bindings.rs 側は `i8/i32/u64` 直接名を返すケースがある。
- 現在の `cast_arg_syn_if_needed`/`cast_return_syn_expr_if_needed`/
  `build_assign_stmt` は、ポインタ型文字列を **to_rust_string で取得した
  後に単純比較**しており、別名だけの違いで cast を挿入してしまう
  （逆に cast すべきケースと混ざる）。

### 変更
1. **新ヘルパ** `pointer_inner_compatible(a_ut: &UnifiedType, b_ut: &UnifiedType) -> bool`
   を `src/unified_type.rs` の末尾に追加。
   - 両方 Pointer の時、const/mut は不問、inner を `to_rust_string` → `normalize_integer_type` で正規化して一致するか判定
   - 非整数 inner は通常比較（Named は名前一致）
2. **新ヘルパ** `pointer_const_differs(a_ut, b_ut) -> bool`
   - 両方 Pointer で inner が compatible、かつ is_const が違うなら true
3. **適用箇所**:
   - `cast_arg_syn_if_needed` (rust_codegen.rs:4264): const/mut 違いブランチ追加（**タスク 2 と合流**）
   - `cast_return_syn_expr_if_needed` (rust_codegen.rs:4683): 現在は integer 型のみ cast。ポインタでも `pointer_inner_compatible && !pointer_const_differs` なら cast 不要、compatible なのに文字列が違うなら cast を入れない／必要なら LHS 型に as 変換を入れる
   - `build_assign_stmt` (rust_codegen.rs:4750): const 違いキャスト判定を `pointer_const_differs` に置き換え

### 期待削減
- カテゴリ B 全 5 件 (`*mut c_char` vs `*mut i8`)
- G.7 (`*mut U8` vs `*const U8`) 1-2 件
- G.9 (`*const c_char` vs `*const c_void`) の c_char→i8 解消分 1 件

**合計 6-8 件**

### リスク
既存テスト OP_CLASS.rs など、`as u32` や `as *mut U32` を期待している
fixture で型一致判定が変わる可能性あり。
`cargo test --test rust_codegen_regression` で確認する。

---

## タスク 2: 引数位置の const/mut 一般化（A）

**タスク 1 の `pointer_const_differs` を使用する**ため、タスク 1 完了後に
着手。

### ゴール
`*mut *const COP` を受ける wrapper から `*const *const COP` を期待する
callee へ渡すケースで as-cast を自動挿入。

### 背景
- `cast_arg_syn_if_needed` (rust_codegen.rs:4283-4306) は
  「actual != expected_ty」ブランチで SV サブタイプと void pointer のみ cast。
- const/mut 違いだけの場合は cast されず Rust compiler が `mutability
  differs` エラー。

### 変更
1. `cast_arg_syn_if_needed` の `actual_ut.is_pointer() && expected_ut.is_pointer()` ブランチを再構成:
   - `is_sv_subtype_cast` → 既存
   - `expected_ut.is_void_pointer()` → 既存
   - **追加**: `pointer_inner_compatible(&actual_ut, &expected_ut) && pointer_const_differs(&actual_ut, &expected_ut)` → `cast_syn_expr(arg_expr, expected_ty)`
   - さらに二重ポインタ `**const T` vs `**mut T` は再帰比較で同一視。ヘルパ
     `pointer_inner_compatible` を再帰実装にしておくと自動的に対応。
2. actual が *mut *const T で expected が *const *const T のように
   **外側** const 違いのケースは既存の `actual.contains("*const")` 判定が
   to_rust_string レベルでは正確なので、そこは改変不要。

### 期待削減
- カテゴリ A 17 件のうち、const 1 段違いの典型 10 件
- 残る 7 件は戻り値 `*const` vs `*mut` など別経路（タスク 1 の return cast で吸収される予定）

**合計 10 件**

### リスク
過剰 cast で可読性が落ちる。`*mut T` → `*const T` は常に安全だが、
本当は wrapper 側の宣言が誤っているケースもある。応急 cast で警告が出る
可能性は低いが、回帰テストで確認。

---

## タスク 3: Option\<fn> 判定統一（G.3）

### ゴール
`(*(*mg).mg_virtual).svt_get != 0` のようにフィールド型が
`Option<unsafe fn(...)>` の時、`.is_some()` / `.is_none()` に置換。

### 背景
- bindgen 出力では function pointer フィールドは
  `Option<unsafe extern "C" fn(...)>` となる。
- 現状 Binary の Ne/Eq arm は integer/pointer の 0 比較を
  `x.is_null()` / `!x.is_null()` には変換するが、Option 判定はしていない。
- `try_build_common_macro_fn_call` (rust_codegen.rs:4341) で
  `type_str_is_fn_pointer` ヘルパが既に存在（`fn(` 含みを検出）。

### 変更
1. **共通ヘルパ** `is_option_fn_pointer(ut: &UnifiedType, rust_decl_dict: Option<&RustDeclDict>) -> bool`
   - to_rust_string 結果が `Option<` で始まり `fn(` を含むなら true
   - または Named(alias) で dict 経由で展開した型が同様
2. `src/rust_codegen.rs` の Binary Eq/Ne arm で、lhs 側の型が
   `is_option_fn_pointer` と判定されたら、rhs が `IntLit(0)` または
   null リテラルなら:
   - `==` → `lhs.is_none()`
   - `!=` → `lhs.is_some()`
3. 真偽文脈 (`wrap_as_bool_condition` / bool 化) でも同様に展開。

### 期待削減
- G.3 の `svt_get != 0` 系 1-2 件
- 類似パターン（bindgen の各 magic vtable）1 件

**合計 1-3 件**

### リスク
整数 0 との比較を Option 判定に誤変換する可能性。判定は **型が明確に
Option\<fn(...)>** の場合のみ。

---

## タスク 4: assign/compare における明示 cast（G.5, G.6, G.7 残）

### ゴール
- `*len = tmps.offset_from(base)` → `*len = tmps.offset_from(base) as usize;`
- `*retlen = expectlen` で u64/usize 違い → as cast
- ポインタ比較 `while x < e` で `*mut U8` vs `*const U8` → cast

### 背景
- 現在 `integer_types_compatible` (rust_codegen.rs:735) は `u64↔usize`,
  `i64↔isize` を compat とする。代入文でこれを参照しているため、
  コンパイラ要件（`usize` と `u64` は distinct 型）を見逃している。
- `offset_from` の戻り値は `isize`（prim 型）、STRLEN は usize。

### 変更

#### 4a. assign で isize/usize/u64 distinctness を強制
`build_assign_stmt` (rust_codegen.rs:4736) の integer 代入キャスト判定:

```rust
} else if let (Some(nl), Some(nr)) = (normalize_integer_type(&ls), normalize_integer_type(&rs)) {
    // 代入文では isize/usize と i64/u64 を明示的に区別する
    // (integer_types_compatible は演算用の緩い判定)
    if nl != nr {
        r_syn = cast_syn_expr(r_syn, nl);
    }
}
```

これだけで G.5/G.6 のほとんどを吸収。`integer_types_compatible` は
binary 演算側で引き続き使用。

#### 4b. ポインタ比較の const 違い cast
`src/rust_codegen.rs` の Binary 比較 arm (`<`, `<=`, `>`, `>=`, `==`, `!=`)
で、両オペランドがポインタかつ `pointer_inner_compatible &&
pointer_const_differs` なら、RHS を LHS 型に cast する分岐を追加。

### 期待削減
- G.5 (`offset_from` → usize) 1 件
- G.6 (u64 → usize assign) 1-2 件
- G.7 (pointer const 違い比較) 1 件

**合計 2-3 件**（タスク 1 で吸収される 1 件と重複する可能性あり）

### リスク
`integer_types_compatible` の用途を assign と演算で分ける必要。既存で
assign 側がこの関数に依存するテストが少ないので影響小。

---

## 実装スケジュール

| ステップ | 作業 | 完了条件 |
|---------|------|---------|
| 1. | タスク 1: `pointer_inner_compatible`/`pointer_const_differs` 追加 | 単体テスト通過 |
| 2. | タスク 1: cast_return/assign での利用 | 回帰 OK、統合ビルド errors 減 |
| 3. | タスク 2: cast_arg_syn_if_needed の const/mut 分岐 | 回帰 OK、errors さらに減 |
| 4. | タスク 3: is_option_fn_pointer + Binary Eq/Ne | 回帰 OK |
| 5. | タスク 4a: assign の integer 厳密判定 | 回帰 OK |
| 6. | タスク 4b: Binary 比較のポインタ const cast | 回帰 OK |
| 7. | 統合ビルド計測 → 次フェーズ（5-8）の準備 | error 件数 <= 40 |

**コミット戦略**: 各ステップ後に `cargo test` を通してから commit。
step 2-3 は合わせて 1 commit も可（タスク 1 のヘルパは 2 で使うため）。

---

## 測定基盤

### ビルド前後で測るコマンド
```zsh
~/blob/libperl-rs/12-macrogen-2-build.zsh 2>&1 | tee tmp/build-error.log
grep -c "^error" tmp/build-error.log
grep -c "^warning" tmp/build-error.log
```

### 回帰テスト
```zsh
cargo test --test rust_codegen_regression
cargo test
```

各ステップの commit 前に必須。

---

## 期待総削減

| # | サブタスク | 削減 |
|---|-----------|------|
| 1 | ポインタ型エイリアス同一視 | 6-8 |
| 2 | 引数 const/mut 一般化 | 10 |
| 3 | Option\<fn> 判定 | 1-3 |
| 4 | assign/compare 明示 cast | 2-3 |
| **合計** | | **19-24** |

重複 cast 除去で 4-5 件のオーバーラップを仮定すると、**実効 -16〜-20** 程度。
errors 56 → **35-40 前後** に着地見込み。

残余は以下に委ねる:
- タスク 5: type_hint の arg/ternary 伝搬 → 別計画
- タスク 6: Member-expr からの struct 型推論 → `reverse-type-inference-from-field-access.md` と統合
- タスク 7: Lvalue macro 展開（macro inference 修正）→ 大型計画

---

## 失敗時のロールバック方針

各タスクは独立 commit にするため、`git revert <sha>` で個別に戻せる。
特にタスク 2（引数 cast 一般化）と 4b（compare cast）は誤 cast が発生
しやすいので、**commit 前に `cargo run` で整合性を確認**する運用。

---

## オープン・クエスチョン

- Q1. `pointer_inner_compatible` は再帰でどこまで対応するか？
  → 二重ポインタ `**const T` まで。三重以上は現状未出現のため後回し。

- Q2. タスク 3 で `Option<fn>` の truthy 化は bool 化パスと assign パス、
  どちらを優先する？
  → 現状の bool 判定パスが Phase 3 にあるため、Phase 3 内で対処。
  将来 Phase 2 に移す際に合流させる。

- Q3. タスク 4a で `integer_types_compatible` を assign 側から削るか？
  → 削らず、assign 専用の **厳密判定**をインラインで入れる。既存の
  binary 演算の緩さは維持。
