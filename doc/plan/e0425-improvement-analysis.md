# E0425 未解決シンボル — 残存エラー分析と改善策

## 進捗サマリー

| 時点 | 総エラー | E0425 | 正常生成 | 主な施策 |
|------|---------|-------|---------|---------|
| commit `0741033` | 1,328 | 147 | 2,512 | UNRESOLVED_NAMES 検出 |
| commit `86228c5` | 1,325 | 141 | — | `use libc::{...}` 動的生成 |
| commit `1d44339` | 1,212 | 69 | 1,949 (1820 macro + 129 inline) | 依存順コード生成 + マクロ間カスケード検出 |
| commit `89f053d` | 999 | 15 | 1,875 (1768 macro + 107 inline) | 統合依存性追跡 + assert 修正 |
| commit `85b9730` | 998 | 12 | 1,841 (1738 macro + 103 inline) | assert 内カスケード + inline→macro 検出 |
| (Phase 1) | **981** | **5** | **1,832** (1729 macro + 103 inline) | KnownSymbols 厳格化 + ジェネリック誤検出対策 |

### 実施済み施策

- **A-1 (libc 関数)**: `LIBC_FUNCTIONS` 定数を定義し `use libc::{...}` を動的生成。
  `strcmp`, `strlen` 等の E0425 を解消。✅ 完了
- **B (カスケード依存 — マクロ間)**: `called_functions` の依存グラフから
  トポロジカルソートで生成順を決定。`successfully_generated` 集合を追跡し、
  生成失敗マクロの呼び出し元を `[CASCADE_UNAVAILABLE]` で自動コメントアウト。
  434 マクロを検出し E0425 を 72 件削減。✅ 完了
- **F-1 (クロスドメインカスケード)**: inline 関数の生成成功を追跡し、
  マクロ→inline 関数依存と inline→inline 関数依存の両方をカスケード検出。
  inline 関数は 2 パス方式 + fixpoint 伝播で処理。✅ 完了
- **F-2 (統合依存性追跡)**: `InlineFnDict` に `called_functions` と
  `calls_unavailable` を追加。`analyze_all_macros` Step 4.6〜4.7 で
  macro↔inline の 4 方向推移閉包を事前計算。宣言の初期化子からの
  関数呼び出し収集漏れも修正。✅ 完了
- **F-3 (assert 内カスケード + inline→macro)**: `ExprKind::Assert` の
  `collect_uses_from_expr` / `collect_function_calls_from_expr` ハンドリング漏れを修正。
  `precompute_macro_generability()` で trial codegen による inline→macro
  カスケード検出を追加。E0425 15→12。✅ 完了
- **C'-1 (KnownSymbols 厳格化 + ジェネリック誤検出対策)**: 3 つの変更で E0425 12→5。✅ 完了
  1. `KnownSymbols::new()` でオブジェクトマクロ（非関数マクロ）を登録しないよう変更
  2. `__errno_location` をビルトインリスト（4 箇所）から削除
  3. `generate_macro()` で型パラメータになったパラメータを `current_local_names` から
     除外し、値コンテキストでの使用を unresolved として検出

---

## 現状 (commit `85b9730`)

| 指標 | 値 |
|------|-----|
| 総ビルドエラー | 998 |
| E0425 エラー | 12 |
| CASCADE_UNAVAILABLE | 526 マクロ + 42 inline 関数 |
| UNRESOLVED_NAMES | 36 関数 (22 macro + 14 inline) |
| CODEGEN_INCOMPLETE | 424 関数 (422 macro + 2 inline) |
| CONTAINS_GOTO | 6 inline 関数 |
| 正常生成 | 1,841 関数 (1,738 macro + 103 inline) |

---

## 残存 E0425 エラーの分類 (12 件)

### C'. ローカル変数参照 (6 エラー)

| シンボル | E0425 数 | 関数名 | 元マクロ定義 |
|----------|---------|--------|-------------|
| `n` | 1 | `isPOWER_OF_2<T>()` | `#define isPOWER_OF_2(n) ((n) && ((n) & ((n)-1)) == 0)` |
| `t` | 1 | `SSNEWt<T>()` | `#define SSNEWt(n,t) SSNEW((n)*sizeof(t))` |
| `c` | 2 | `XDIGIT_VALUE<T>()` | `#define XDIGIT_VALUE(c) ...` |
| `s` | 2 | `READ_XDIGIT<T>()` | `#define READ_XDIGIT(s) ((s)++, XDIGIT_VALUE(*((s) - 1)))` |

**根本原因**: マクロのパラメータ（`n`, `t`, `c`, `s`）がジェネリック型パラメータとして
誤検出され、通常の関数パラメータとして生成されない。

- `isPOWER_OF_2(n)`: パーサーが `(n)` をキャスト式パターン `(TYPE)expr` と誤認し、
  `n` を型パラメータ `T` に変換 → `isPOWER_OF_2<T>()` で `n` への参照が残る
- `SSNEWt(n,t)`: `sizeof(t)` の `t` が型パラメータと認識され、`n` も巻き込まれる
- `XDIGIT_VALUE(c)`, `READ_XDIGIT(s)`: 同様のジェネリック誤検出

これらは全て `UNRESOLVED_NAMES` で検出されるべきだが、`KnownSymbols` に
オブジェクトマクロ名（1文字マクロ: `n`, `s`, `c`, `t`）として登録されているため
既知扱いになっている。

**改善策C'-1: KnownSymbols マクロ登録条件の厳格化**

`KnownSymbols::new()` (L84-91) でマクロ名を登録する条件を変更:

```rust
// 変更前
if info.has_body {
    names.insert(name_str.to_string());
}

// 変更後: 関数マクロのみ登録（オブジェクトマクロは除外）
if info.has_body && info.is_function {
    names.insert(name_str.to_string());
}
```

オブジェクトマクロ（`#define n 42` のような定数マクロ）は関数呼び出しとして
コード中に現れることがないため、`KnownSymbols` に登録する必要がない。
（定数として使用される場合は `bindings.rs` 側で登録済み。）

**期待効果**: 6 E0425 → 0 E0425（これらの関数が UNRESOLVED_NAMES として検出される）

**注意**: ジェネリック型パラメータの誤検出自体は別の問題（パーサーの
`looks_like_generic_cast()` ヒューリスティクスの限界）であり、根本的な修正は
より大きな変更が必要。C'-1 は「誤った関数を出力しない」という防御策。

---

### D'. 型名の未解決 (5 エラー)

| シンボル | E0425 数 | 関数名 | 原因 |
|----------|---------|--------|------|
| `body_details` | 2 | `new_NOARENA()`, `new_NOARENAZ()` | パラメータ型 `*mut body_details` |
| `PerlIO_funcs` | 2 | `PERLIO_FUNCS_CAST()` | 戻り値型とキャスト先の型 |
| `caddr_t` | 1 | `SSNEWa()` | キャスト式 `as caddr_t` |

**根本原因**: C の型名で `bindings.rs` に含まれていない。
`ExprKind::Cast` や関数パラメータの型名は `ExprKind::Ident` を経由しないため
UNRESOLVED_NAMES で検出されない。

**改善策D'-1: 型名の未解決チェック追加**

`RustCodegen` の型名出力箇所で `KnownSymbols` との照合を追加:

1. `decl_specs_to_rust()` — パラメータ・戻り値の型指定子
2. `type_name_to_rust()` — キャスト先の型名

未知の型名が見つかった場合は `unresolved_names` に記録する。

```rust
// decl_specs_to_rust() / type_name_to_rust() 内
let type_name = ...;
if !self.known_symbols.contains(&type_name) {
    self.unresolved_names.push(type_name.clone());
}
```

**期待効果**: 5 E0425 → 0 E0425（これらの関数が UNRESOLVED_NAMES として検出される）

**注意点**: 型名チェックは false positive のリスクがある。
`c_int`, `c_char` など Rust プリミティブ型名や libc 型名は除外が必要。
既に `KnownSymbols` に `bindings.rs` の型名は登録されている（L66-68）ので、
主に bindings.rs に含まれない C 固有の型（`caddr_t`, `body_details` 等）が対象。

---

### E'. __errno_location (1 エラー)

| シンボル | E0425 数 | 関数名 |
|----------|---------|--------|
| `__errno_location` | 1 | `get_extended_os_errno()` |

**根本原因**: `__errno_location` は glibc の内部関数で、`errno` マクロの展開結果。
`KnownSymbols` のビルトインリスト (L116) に登録されているため既知扱いになっている。

```rust
// src/rust_codegen.rs L116
"__errno_location",  // ← これが原因
```

**改善策E'-1: __errno_location をビルトインリストから除外**

```rust
// 変更前
let builtins = [
    ...
    "__errno_location",
    ...
];

// 変更後: __errno_location を削除
let builtins = [
    ...
    // "__errno_location" を削除
    ...
];
```

**期待効果**: 1 E0425 → 0 E0425（`get_extended_os_errno` が UNRESOLVED_NAMES として検出される）

---

## 改善策の優先度と期待効果

| 優先度 | 改善策 | 対象 | 期待 E0425 削減 | 実装コスト |
|--------|--------|------|----------------|-----------|
| ~~1~~ | ~~F-1〜F-3: カスケード検出~~ | ~~依存性追跡~~ | ~~-135~~ | ✅ 完了 |
| 1 | C'-1: マクロ登録条件厳格化 | ローカル変数 | -6 | 極小（1行変更） |
| 1 | E'-1: __errno_location 除外 | errno | -1 | 極小（1行削除） |
| 2 | D'-1: 型名チェック | 型名 | -5 | 中 |

### 推奨実装順序

**Phase 1 (即実行可能)**: C'-1 + E'-1 — `KnownSymbols` の修正。
合計 -7 E0425。2 箇所の変更のみ。

**Phase 2**: D'-1 — 型名の未解決検出。-5 E0425。
`decl_specs_to_rust()` と `type_name_to_rust()` に型名チェックを追加。
false positive を避けるための除外リスト設計が必要。

### Phase 1 の変更箇所

| ファイル | 行 | 変更内容 |
|----------|-----|---------|
| `src/rust_codegen.rs` | L88 | `if info.has_body {` → `if info.has_body && info.is_function {` |
| `src/rust_codegen.rs` | L116 | `"__errno_location",` を削除 |

### Phase 2 の変更箇所

| ファイル | 変更内容 |
|----------|---------|
| `src/rust_codegen.rs` | `decl_specs_to_rust()` で型名チェック追加 |
| `src/rust_codegen.rs` | `type_name_to_rust()` で型名チェック追加（キャスト式用） |
| `src/rust_codegen.rs` | `build_fn_param_list()` でパラメータ型チェック追加 |

---

## 全体目標の達成状況

当初目標: E0425 147 → 30 以下

**現在: 5** ✅ 目標大幅達成

| 時点 | E0425 | 備考 |
|------|-------|------|
| 開始 | 147 | |
| UNRESOLVED_NAMES | 147 | 検出インフラ |
| `use libc` | 141 | -6 |
| マクロ間カスケード | 69 | -72 |
| 統合依存性追跡 | 15 | -54 |
| assert 内 + inline→macro | 12 | -3 |
| Phase 1 実施 | **5** | **-7** |
| Phase 2 実施後 (見込み) | **0** | -5 |
| 目標 | ≤30 | ✅ |

残り 5 件の内訳:
- D': 型名未解決 (5): `PerlIO_funcs`(2), `body_details`(2), `caddr_t`(1)
