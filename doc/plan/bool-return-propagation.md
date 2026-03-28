# Plan: 依存順序による bool 戻り値型の伝播

## 背景

前コミット `ce40644` で `is_boolean_expr()` を使って本体が比較式・論理式のマクロの
戻り値型を `bool` に変更した。しかし以下の3関数は本体が関数呼び出しであるため
検出されない:

| 関数 | 本体 | 呼び出し先の戻り値 |
|------|------|---------------------|
| `CxTRYBLOCK(c)` | `CxEVALBLOCK(c)` | `bool` (今回変更済み) |
| `RX_OFFS_VALID(rx_sv, n)` | `RXp_OFFS_VALID(...)` | `bool` (今回変更済み) |
| `SvBoolFlagsOK(sv)` | `SvIandPOK(sv)` | `bool` (今回変更済み) |

## 設計

### アプローチ: const/mut 推論と同じ「依存順パス + 集合の蓄積」

`generate_macros()` 内の既存の const/mut 解析パスと同じ位置に、
bool 戻り値型の判定パスを追加する。

```
既存: sorted_names のループ
  → const/mut 解析 (callee_const_params を蓄積)
新規: 同じループ内で
  → bool 戻り値判定 (bool_return_macros を蓄積)
```

### データ構造

```rust
/// bool を返すと判定されたマクロの集合
bool_return_macros: HashSet<InternedStr>
```

### 判定ロジック

マクロの `ParseResult::Expression(expr)` に対して:

1. `is_boolean_expr(expr)` → true → bool
2. `expr` が `Call { func: Ident(name), .. }` で `name` が:
   - `bool_return_macros` に含まれる → bool
   - bindings.rs の関数で戻り値が `bool` → bool
   - inline 関数で戻り値が `bool` → bool
3. `expr` が `MacroCall { name, .. }` で `name` が `bool_return_macros` に含まれる → bool

この判定を `is_boolean_expr_with_context()` として実装する:

```rust
fn is_boolean_expr_with_context(
    expr: &Expr,
    bool_return_macros: &HashSet<InternedStr>,
    bool_return_externals: &HashSet<InternedStr>,
) -> bool
```

### 外部関数の bool 戻り値情報の事前収集

const/mut の `seed_callee_const_from_externals` と同様に、
bindings.rs と inline 関数辞書から bool を返す関数名を事前収集する。

```rust
fn seed_bool_return_externals(
    &self,
    result: &InferResult,
) -> HashSet<InternedStr>
```

- bindings.rs: `func.ret_ty == Some("bool")` の関数
- inline 関数: 戻り値の TypeSpec が `Bool` の関数

### 処理の統合

const/mut パスと bool パスを同じ依存順ループ内で実行する:

```rust
// 依存順ループ（既存 + 新規）
for &name in &sorted_names {
    if let Some(info) = result.infer_ctx.macros.get(&name) {
        if !info.is_parseable() || info.calls_unavailable { continue; }

        // ── const/mut 解析（既存）──
        ...

        // ── bool 戻り値判定（新規）──
        if let ParseResult::Expression(expr) = &info.parse_result {
            if is_boolean_expr_with_context(expr, &bool_return_macros, &bool_return_externals) {
                bool_return_macros.insert(name);
            }
        }
    }
}
```

### codegen への受け渡し

`CodegenDriver` に `bool_return_macros: HashSet<InternedStr>` を保存し、
`RustCodegen` に `is_bool_return: bool` フラグを渡す。

`get_return_type()` で:
```rust
if self.is_bool_return {
    return "bool".to_string();
}
```

これにより `is_boolean_expr()` の直接チェックも不要になり、
すべて依存順パスの結果を使う統一的な仕組みになる。

## 影響範囲

- `src/rust_codegen.rs`:
  - `generate_macros()`: bool パス追加（依存順ループ内）
  - `seed_bool_return_externals()`: 新規メソッド
  - `is_boolean_expr_with_context()`: 新規フリー関数
  - `CodegenDriver` に `bool_return_macros` フィールド追加
  - `RustCodegen` に `is_bool_return` フィールド追加
  - `get_return_type()`: `is_bool_return` チェック追加

## 期待効果

- 残り3件の int←bool エラー解消
- 今後 bool マクロを呼ぶラッパーマクロが増えても自動対応
