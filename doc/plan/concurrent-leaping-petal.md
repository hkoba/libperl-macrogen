# Plan: マクロ間呼び出し境界での SV→CV キャスト挿入の修正

## Context

直前の作業で「共通フィールドマクロ (`_XPVCV_COMMON` 等) からの SV
ファミリー型逆推論」を導入し、`CvHASGV` などの xpvcv 専用マクロが
`*const CV` と正しく推論されるようになった
(`src/semantic.rs` の `try_infer_sv_family_from_member`,
`CTypeSource::CommonMacroFieldInference` Tier 3)。

しかし統合ビルドでは **エラー数が 113 → 126 (+13)** と退行した。原因は
**マクロ→マクロ呼び出しの境界で、必要な SV-subtype キャストが挿入されない**
ことにある。

### 症状例

`tmp/build-error.log` より:

```
19649 | if ! CopFILE (c) . is_null () { GvAV (gv_fetchfile (my_perl , CopFILE (c))) } ...
      | ----  ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ expected `*const sv`, found `*mut gv`
      |       arguments to this function are incorrect
note: function defined here
14852 | pub unsafe fn GvAV(gv: *const SV) -> *mut AV {
```

`GvAV` は宣言上 `gv: *const SV` だが、`gv_fetchfile()` の戻り値は
`*mut gv`。`is_sv_subtype_cast` (src/rust_codegen.rs:457) が
`*mut gv → *const SV` を許す対象になっているはずだが、**キャストが挿入
されない**。

### 真因 (root cause)

`build_arg_string_unified` (`src/rust_codegen.rs:3452`) は引数式の
キャスト要否を判定するため `get_callee_param_type_extended` で
**callee が期待する型** を取得する。callee がマクロ関数の場合
(method 3, `src/rust_codegen.rs:1920-1953`) 以下の挙動になっている:

```rust
// src/rust_codegen.rs:1932-1943 (method 1)
if let Some(expr_ids) = macro_info.type_env.param_to_exprs.get(&param.name) {
    for expr_id in expr_ids {
        if let Some(constraints) = macro_info.type_env.expr_constraints.get(expr_id) {
            for c in constraints {
                if !c.ty.is_void() {
                    let ty = c.ty.to_rust_string(self.interner);
                    return Some(UnifiedType::from_rust_str(&ty));   // ←最初の非voidを採用
                }
            }
        }
    }
}
```

これは「**最初に見つかった非 void 制約**」を返す。一方、宣言型を決める
`get_param_type` (`src/rust_codegen.rs:2395-2446`) は **Tier-best** 選択:

```rust
// src/rust_codegen.rs:2422-2432 (best-tier)
for c in constraints {
    if c.ty.is_void() { continue; }
    let tier = c.ty.confidence_tier();
    if best.is_none() || tier < best.unwrap().1 {
        best = Some((&c.ty, tier));
    }
}
```

結果として、同じパラメータに対する callee の見え方が二系統に分裂する:

| 用途 | 関数 | 選択方式 | 例: `Cv*` 系の `cv` |
|------|------|---------|-------------------|
| 宣言生成 | `get_param_type` | Tier-best | `*const CV` (Tier 3, CommonMacroFieldInference) |
| 呼出側のキャスト判定 | `get_callee_param_type_extended` | First-non-void | `*const SV` (Tier 4, SvFamilyCast) ← 古い制約 |

callee 宣言は `*const CV` で固定されているのに、呼出側では `*const SV`
を期待していると見做すため、引数 `*mut gv` を `*const SV` にキャスト
する判断が出される — しかし実体は `*const CV` を期待。
あるいは「`*const SV` と一致するから cast 不要」と判断され、何も挿入
されないまま `*mut gv → *const CV` のずれが残る。

### ゴール

`get_callee_param_type_extended` の **method 3 (自家生成マクロパス)** を
`get_param_type` と同じ Tier-best 選択に揃え、宣言と呼出時の callee
パラメータ型観測を一貫させる。これにより:

- callee が `*const CV` 宣言 → 呼出側でも「`*const CV` 期待」と判定
- 引数 actual が `*const SV` または `*mut sv` 等 → `is_sv_subtype_cast`
  発火 → `*const CV` への as-cast が挿入される
- `+13` 退行が解消し、できれば 113 エラー以下になる

## アプローチ概要

「**全 ExprId に対する全制約を Tier 順走査して非 void の最良を採用**」
する共通ヘルパを `rust_codegen.rs` に追加し、`get_param_type` と
`get_callee_param_type_extended` (method 3) の双方で使用する。ヘルパは
TypeRepr を返し、呼出側で `to_rust_string` して必要なフォーマット (UnifiedType
or String) に変換する。

副次的効果として `callee_param_is_bool` (`src/rust_codegen.rs:1959-`)
の判定方式も best-tier に揃えると、bool 引数判定の安定性も向上する
(別タスクとして optional)。

## 設計

### 1. 共通ヘルパ追加

`src/rust_codegen.rs` の private 関連関数として:

```rust
/// 自家生成マクロの param に対する全制約のうち、Tier が最も高い
/// (=数値が小さい) 非 void TypeRepr のクローンを返す。
/// param.expr_id() および param_to_exprs から得た全 ExprId を走査する。
fn best_constraint_for_macro_param(
    info: &MacroInferInfo,
    param: &MacroParam,
) -> Option<crate::type_repr::TypeRepr> {
    let mut best: Option<(&crate::type_repr::TypeRepr, u8)> = None;

    let mut all_expr_ids: Vec<ExprId> = info
        .type_env
        .param_to_exprs
        .get(&param.name)
        .map(|ids| ids.iter().cloned().collect())
        .unwrap_or_default();
    all_expr_ids.push(param.expr_id());

    for expr_id in &all_expr_ids {
        if let Some(constraints) = info.type_env.expr_constraints.get(expr_id) {
            for c in constraints {
                if c.ty.is_void() { continue; }
                let tier = c.ty.confidence_tier();
                if best.is_none() || tier < best.unwrap().1 {
                    best = Some((&c.ty, tier));
                }
            }
        }
    }
    best.map(|(t, _)| t.clone())
}
```

`MacroInferInfo` / `MacroParam` / `TypeRepr` / `ExprId` のインポートは
既存ファイル内で利用可能。`self` 不要 (free function でも可)。

### 2. `get_callee_param_type_extended` method 3 を best-tier に置き換え

`src/rust_codegen.rs:1920-1953` の方法1 + 方法2 を以下に置換:

```rust
// 3. 自家生成マクロ関数の type_env から best-tier 制約を取得
if let Some(macro_info) = self.macro_ctx.macros.get(&interned) {
    let macro_param_idx = if macro_info.is_thx_dependent {
        if arg_index == 0 {
            return Some(UnifiedType::from_rust_str("*mut PerlInterpreter"));
        }
        arg_index - 1
    } else {
        arg_index
    };
    if let Some(param) = macro_info.params.get(macro_param_idx) {
        if let Some(ty) = best_constraint_for_macro_param(macro_info, param) {
            let rust_ty = ty.to_rust_string(self.interner);
            return Some(UnifiedType::from_rust_str(&rust_ty));
        }
    }
}
```

これで宣言側 (`get_param_type`) と完全に同じ「Tier-best」観測になる。

### 3. `get_param_type` をヘルパで置き換え (任意・推奨)

`src/rust_codegen.rs:2410-2443` のループも `best_constraint_for_macro_param`
で書き換えると重複が解消する:

```rust
let best = best_constraint_for_macro_param(info, param);
if let Some(mut ty) = best {
    if should_be_const {
        ty.make_outer_pointer_const();
    } else if ty.has_outer_pointer() {
        ty.make_outer_pointer_mut();
    }
    return self.type_repr_to_rust(&ty);
}
self.unknown_marker().to_string()
```

リテラル文字列パラメータ判定や generic 判定はその前段で行うため変化なし。

### 4. `callee_param_is_bool` も同手法で揃える (任意)

`src/rust_codegen.rs:1959-` の bool 判定パスも、全制約走査ではなく
「best-tier の制約が bool か」に揃えると一貫性が高まる。ただしこの修正は
別 PR としても良い。最初は触らず、先に method 3 と get_param_type だけ
揃えて整合性を確認する。

## 影響範囲と検証

### 単体テスト

- `cargo test`: 既存 350+ tests が通ることを確認

### 統合ビルド

```bash
~/blob/libperl-rs/12-macrogen-2-build.zsh
```

期待される変化:
- エラー数: 126 → **113 以下** (退行解消、できれば改善)
- `GvAV (gv_fetchfile (...))` のような呼出に `*const CV` 等への
  as-cast が挿入される
- `Cv*` 系で `*const SV` を期待していた呼出が `*const CV` 期待に
  変わる (callee の真の宣言と一致)

### スポットチェック

```bash
# 改善前後で diff
grep -n "GvAV\|CvGV\|CvSTASH" tmp/macro_bindings.rs | head
```

`gv as *const CV` 等の cast 挿入を確認。

### 想定される連鎖改善

- `MUTABLE_SV(...)`, `MUTABLE_HV(...)`, `MUTABLE_AV(...)` 系統の
  type-coercion マクロでも一致が改善する可能性あり
- 過去にコメントアウトしていた SV-family 関連の error 群が消える
- `Cv*` macro 同士のチェーン呼出
  (`CvISXSUB(cv) ? ... : CvSTART(cv)` 等) で型が揃う

## リスク

1. **bool 判定波及**: `get_callee_param_type_extended` の戻り値を bool
   として再解釈する箇所はない (bool 判定は別経路 `callee_param_is_bool`
   が担う) ため、bool 関連の波及は理論上ない。念のため `bool` が含まれた
   テストも回す。

2. **inline 関数 callee**: method 2 (`inline_fn_dict`) と method 1
   (`bindings.rs`) は今回触らない。これらはもともと宣言ベースのため
   一貫している。method 3 のみが「内部 type_env を peek する」性質を
   持つので不一致が起きていた。

3. **`*const` vs `*mut` の取り違え**: `get_param_type` では
   `should_be_const` (`const_pointer_positions`) によって最終的な
   const/mut を決めている。`get_callee_param_type_extended` の戻り値も
   厳密にはこの後処理が必要かもしれないが、現行コードがすでに後処理を
   行っていない (best-tier 直前で `c.ty` をそのまま返す) ため、一旦は
   ヘルパの戻り値をそのまま返す。const/mut の問題は別途対処する余地を
   残すが、SV-subtype キャストの判定 (`is_sv_subtype_cast`) は const/mut
   差を許容するロジックなので、本タスクの解決には十分。

4. **constraint 走査の順序非決定性**: `expr_constraints` は HashMap だが、
   Tier-best は順序非依存 (純粋に最小値選択) なので、結果は決定的。
   first-non-void から best-tier への変更でテスト出力差分は出るが、
   それは想定通り。

## 実施順序

| Step | 内容 | 主な変更ファイル |
|------|------|-----------------|
| 1 | `best_constraint_for_macro_param` ヘルパを `rust_codegen.rs` に追加 | `src/rust_codegen.rs` |
| 2 | `get_callee_param_type_extended` method 3 を best-tier 採用に置換 | `src/rust_codegen.rs` |
| 3 | `cargo test` で全 unit test pass を確認 | (検証のみ) |
| 4 | 統合ビルドで `+13` 退行が解消することを確認 | (検証のみ) |
| 5 | (任意) `get_param_type` 内ループもヘルパに置換し重複削減 | `src/rust_codegen.rs` |
| 6 | (任意・別タスク) `callee_param_is_bool` も同様に best-tier 化 | `src/rust_codegen.rs` |

Step 1-4 で本来の目的 (退行解消) は達成。Step 5-6 はリファクタ要素のため
別 commit に分けると安全。
