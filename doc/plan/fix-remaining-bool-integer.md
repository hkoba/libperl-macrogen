# Plan: 残り24件の `expected bool, found integer` エラーの修正

## パターン分類

| パターン | 件数 | 例 | 原因 |
|----------|------|-----|------|
| A: bool 関数 + `!= 0` | 5 | `isREGEXP(...) != 0` | `is_bool_expr_with_dict` が bool を認識しない |
| B: bool フィールド + `!= 0` | 5 | `Itainting != 0` | フィールド bool 判定が効かない |
| C: bool パラメータ + `!= 0` | 1 | `cBOOL(cbool: bool)` → `cbool != 0` | パラメータ bool 判定が効かない |
| D: bool 引数に整数リテラル | 10 | `gv_efullname4(..., 1)` | `0`/`1` → `false`/`true` 変換の漏れ |
| E: その他 | 3 | `PL_valid_types_PVX[i] != 0` | 配列要素の型推論 |

## 原因分析

### パターン A+B+C: `!= 0` が不要に付加される

**根本原因**: 2つのパスが混在。

1. **`wrap_as_bool_condition_macro/inline`** — if/assert の条件式で呼ばれる。
   前回の修正で `is_bool_expr_with_dict`, パラメータ bool, フィールド bool を追加済み。

2. **`expr_to_rust` / `expr_to_rust_inline` 内での `!= 0` 直接生成** —
   `wrap_as_bool_condition` を経由せずに `!= 0` を出力する箇所がある。
   特に assert 内や inline 関数の if 条件。

**パターン A の追加原因**: inline 関数の `RustCodegen` に `bool_return_macros` が渡されていない。
`generate_inline_fns()` で作る `RustCodegen` に `.with_bool_return(false, self.bool_return_macros.clone())`
を追加する必要がある。

### パターン D: `0`/`1` → `false`/`true` 変換漏れ

**原因**: 引数が `bool` パラメータの場合に整数リテラルを `true`/`false` に変換するロジックは
`expr_to_rust_arg` (macro) と inline Call ハンドラの `callee_param_is_bool` チェックに存在する。
しかし以下のケースが漏れている:

- 呼び出し先が **自家生成マクロ関数** で、パラメータ型推論で `bool` になったケース
- inline 関数内の Call で、callee が自家生成マクロ関数

`callee_param_is_bool` が自家生成マクロの bool パラメータを認識できるかの確認が必要。

---

## 修正計画

### 修正 1: inline 関数に `bool_return_macros` を渡す

**場所**: `generate_inline_fns()` の `RustCodegen::new()` 呼び出し

```rust
let codegen = RustCodegen::new(...)
    .with_dump_ast_for(...)
    .with_bool_return(false, self.bool_return_macros.clone());
```

ただし `bool_return_macros` は `generate_macros()` 内で構築されるため、
inline 関数生成時にはまだ存在しない。
→ **解決策**: bool 解析パスを `generate()` レベルに引き上げ、
  inline/macro 両方の生成の前に実行する。
  または inline 関数生成にも bool 解析結果を渡す。

**実際の対応**: `generate()` の順序:
1. inline 関数セクション（先に生成）
2. マクロセクション（後に生成）

inline が先なので、macro の bool 情報は使えない。しかし inline 関数内で呼ばれる
マクロは「まだ生成されていない」ので、マクロの bool 判定を事前計算する必要がある。

→ **方針**: `generate()` の冒頭で **全マクロ** に対する bool 解析パスを実行し、
  その結果を `CodegenDriver` に保存。inline/macro 両方で使う。

### 修正 2: `!= 0` 生成箇所の統一

`expr_to_rust` / `expr_to_rust_inline` で `!= 0` を直接出力する全箇所を
`wrap_as_bool_condition` 経由に統一するのが理想。ただし影響が大きいため、
代わりに `wrap_as_bool_condition` が呼ばれるパスを確認し、
漏れている箇所を個別に修正する。

主な `!= 0` 生成箇所:
- `wrap_as_bool_condition_macro/inline` — 修正済み
- assert ハンドラ内: `format!("assert!(({}) != 0)", cond)` — ここでも
  `wrap_as_bool_condition` が使われるべき
- if 条件: `wrap_as_bool_condition` が使われる
- `while`/`do-while`/`for` 条件: 同上

### 修正 3: 整数リテラル → bool 変換の拡張

`expr_to_rust_arg` と inline Call ハンドラの bool 引数変換を
自家生成マクロの bool パラメータにも対応させる。
`callee_param_is_bool` が `bool_return_macros` ベースで
パラメータ型を判定できるようにする。

---

## 実装順序

1. **修正 1**: `generate()` 冒頭で全マクロの bool 解析パスを実行し、
   inline/macro 両方に `bool_return_macros` を渡す
2. **修正 3**: 整数リテラル → bool 変換を自家生成マクロにも拡張
   (`callee_param_is_bool` の改善 + `0`/`1` → `false`/`true`)
3. **修正 2**: 残りの `!= 0` 漏れ箇所を個別修正

## 期待効果

24件中 18〜22件の解消を見込む。残りはジェネリクスや配列型関連の特殊ケース。
