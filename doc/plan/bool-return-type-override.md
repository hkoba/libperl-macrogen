# Plan: bool 式を返すマクロ関数の戻り値型を bool に変更

## 目標

マクロ関数の本体が比較式・論理式（bool を返す式）の場合、
戻り値型を型推論結果（`c_int`, `U32` 等）ではなく `bool` に変更する。

### 現状

```rust
// c_int だが本体は == 比較（bool）
pub unsafe fn SvIOK_nog(sv: *const SV) -> c_int {
    (((*sv).sv_flags & (SVf_IOK | SVs_GMG)) == SVf_IOK)  // → bool
}
```

### 目標

```rust
pub unsafe fn SvIOK_nog(sv: *const SV) -> bool {
    (((*sv).sv_flags & (SVf_IOK | SVs_GMG)) == SVf_IOK)
}
```

### 影響規模

- 40 関数、54 エラー（51 i32 + 2 u32 + 1 isize）

---

## 設計

### 基本方針

`get_return_type()` で戻り値型を決定する際、
`ParseResult::Expression(expr)` の `expr` が bool 式ならば、
型推論結果を無視して `"bool"` を返す。

### 判定関数

既存の `is_boolean_expr()` を活用する。
この関数は以下のパターンを検出済み：

| パターン | 例 |
|----------|-----|
| 比較演算 | `a == b`, `a != b`, `a < b` |
| 論理演算 | `a && b`, `a \|\| b` |
| 論理否定 | `!a` |
| bool キャスト | `(bool)a` |

ただし、`MacroCall` や括弧でラップされたケースにも対応する必要がある。
例: `(SvTYPE(sv) == SVt_PVGV)` は `Binary(Eq)` なのでそのまま検出可能。

### 変更箇所

**`src/rust_codegen.rs`** — `get_return_type()` 内

```rust
fn get_return_type(&mut self, info: &MacroInferInfo) -> String {
    // ジェネリック戻り値型チェック（既存）
    if let Some(generic_name) = info.generic_type_params.get(&-1) {
        return generic_name.clone();
    }

    match &info.parse_result {
        ParseResult::Expression(expr) => {
            // ★ 新規: 本体式が bool を返すなら戻り値型は bool
            if is_boolean_expr(expr) {
                return "bool".to_string();
            }
            // 既存ロジック
            if let Some(ty) = info.get_return_type() {
                return self.type_repr_to_rust(ty);
            }
            self.unknown_marker().to_string()
        }
        ...
    }
}
```

### 呼び出し元への影響

bool を返すマクロ関数を呼ぶ側のマクロでは、
以下のパターンで影響が出る可能性がある：

1. **`if (isGV(sv))` → `if isGV(sv)`**:
   既に `wrap_as_bool_condition` が `!= 0` を挿入しているが、
   `isGV` が `bool` を返すなら `!= 0` は不要。
   → 既存の `is_boolean_expr_recursive` が Call の結果型を見るため、
     呼び出し先が `bool` に変われば自動的に対応される。

2. **`isGV(sv) | flags`**: bool と整数のビット演算。
   → これは C のイディオムだが Rust ではエラーになる。
   ただしこのパターンは少なく、主に assert 条件内で使われる。

3. **戻り値型キャッシュへの影響**:
   `return_types_cache` に `"bool"` が入ることで、
   呼び出し元マクロの型推論に `bool` が伝播する。
   → const/mut 推論と同様に依存順序で伝播するため問題なし。

### 副作用の管理

- `current_return_type` が `bool` に設定されることで、
  `cast_return_expr_if_needed` が戻り値式に `as bool` キャストを
  挿入しようとする可能性 → bool 式なら不要なので影響なし
- `expr_with_type_hint` で `type_hint = "bool"` が渡された場合、
  `IntLit(0)` → `false`, `IntLit(1)` → `true` 変換が適用される → 好ましい

### return_types_cache の更新

`return_types_cache` は `macro_infer.rs` の `infer_types_in_dependency_order()` で
構築される。ここでは `get_return_type()` (MacroInferInfo の方) を使用しており、
codegen の `get_return_type()` とは別の関数。

キャッシュに `bool` を反映するには2つの選択肢がある：

**選択肢 A**: codegen 側のみで対応（シンプル）
- `get_return_type()` (codegen) で bool チェックを追加
- キャッシュには従来の型が入り、依存マクロには `c_int` 等が伝播
- 呼び出し元では `isGV(sv)` が `c_int` を返すと思って生成される
- → 実際は `bool` なので `!= 0` の不要な比較が残る可能性があるが、エラーにはならない

**選択肢 B**: macro_infer 側でもキャッシュを更新
- `infer_types_in_dependency_order()` で bool 判定を追加
- `return_types_cache` に `"bool"` を格納
- 依存マクロに正しく伝播
- → より正確だがマクロ推論側の変更が必要

**推奨**: まず **選択肢 A** で実装し、必要に応じて B に拡張。
選択肢 A でも54件のエラーは解消される（戻り値型が `bool` になるため）。

---

## 実装手順

### Step 1: `get_return_type()` に bool チェックを追加

`src/rust_codegen.rs` の `get_return_type()` 冒頭で、
`ParseResult::Expression(expr)` の場合に `is_boolean_expr(expr)` をチェック。

### Step 2: regression test の更新

`isGV_with_GP` は既に `bool` を返しているため影響なし。
他の expected ファイルに該当するものがあれば更新。

### Step 3: テスト

- `cargo test` — 既存テスト通過
- `~/blob/libperl-rs/12-macrogen-2-build.zsh` — エラー数 54 件減の確認
- `grep 'fn SvIOK_nog' tmp/macro_bindings.rs` → `-> bool` になること

---

## リスク

- **低リスク**: bool を返す関数の呼び出し元で `!= 0` 比較が冗長になるだけで、
  コンパイルエラーにはならない
- **中リスク**: bool を整数として使うパターン（`isGV(sv) | flag`）が
  あればエラーになる → 発生時に `as c_int` キャストを追加で対応
