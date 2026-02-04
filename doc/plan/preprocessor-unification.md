# Preprocessor 一本化 実装計画

## 概要

TokenExpander を廃止し、マクロ推論でも Preprocessor を使用するように変更する。

## 実装ステップ

### Step 1: Preprocessor に「マクロ本体展開 API」を追加

Preprocessor に、マクロ定義の本体トークン列を展開する公開メソッドを追加する。

**新規メソッド**:
```rust
impl Preprocessor {
    /// マクロ本体を展開する（MacroInferContext 用）
    ///
    /// - `body`: マクロ本体のトークン列
    /// - `params`: パラメータ名のリスト（関数マクロの場合）
    /// - `args`: 各パラメータに対応する引数トークン列
    ///
    /// Returns: (展開結果, 呼び出されたマクロ集合)
    pub fn expand_macro_body(
        &mut self,
        body: &[Token],
        params: &[InternedStr],
        args: &[Vec<Token>],
    ) -> (Vec<Token>, HashSet<InternedStr>);
}
```

**内部実装**:
- 既存の `expand_function_macro_body()` をベースに
- `called_macros` を収集して返す
- `explicit_expand_macros` / `preserve_function_macros` の制御を適用

### Step 2: MacroInferContext の修正

`build_macro_info()` で TokenExpander の代わりに Preprocessor を使用する。

**変更前**:
```rust
fn build_macro_info(&self, def: &MacroDef, ...) {
    let mut expander = TokenExpander::new(macro_table, interner, files);
    // ...
    let expanded = expander.expand_with_calls(&def.body);
}
```

**変更後**:
```rust
fn build_macro_info(&self, def: &MacroDef, pp: &mut Preprocessor, ...) {
    let (expanded, called_macros) = pp.expand_macro_body(
        &def.body,
        &params,
        &[], // オブジェクトマクロの場合は空
    );
}
```

### Step 3: analyze_all_macros() のシグネチャ変更

Preprocessor への参照を受け取るように変更。

**変更前**:
```rust
pub fn analyze_all_macros(
    &mut self,
    macro_table: &MacroTable,
    interner: &StringInterner,
    files: &FileRegistry,
    // ...
)
```

**変更後**:
```rust
pub fn analyze_all_macros(
    &mut self,
    pp: &mut Preprocessor,
    // macro_table, interner, files は pp から取得
    // ...
)
```

### Step 4: infer_api.rs の修正

`run_inference_with_preprocessor()` で Preprocessor を `analyze_all_macros()` に渡す。

### Step 5: TokenExpander の削除

- `src/token_expander.rs` を削除
- `lib.rs` から `pub mod token_expander;` を削除
- 関連テストの削除または移行

### Step 6: テストの修正

- TokenExpander のテストを Preprocessor のテストに移行
- 新しい API のテストを追加

## ファイル変更一覧

| ファイル | 変更内容 |
|----------|----------|
| `src/preprocessor.rs` | `expand_macro_body()` メソッド追加 |
| `src/macro_infer.rs` | TokenExpander → Preprocessor に変更 |
| `src/infer_api.rs` | Preprocessor の渡し方を変更 |
| `src/token_expander.rs` | 削除 |
| `src/lib.rs` | `token_expander` モジュールを削除 |
| `tests/` | テストの修正 |

## 注意点

1. **Preprocessor の状態管理**
   - マクロ本体展開時、ファイル状態は使用しない
   - `MacroTable` と `StringInterner` のみ使用

2. **展開制御の移植**
   - `explicit_expand_macros` は既に Preprocessor にある
   - `preserve_function_macros` フラグを追加（または既存のものを使用）
   - `no_expand` の仕組みも確認

3. **called_macros の収集**
   - TokenExpander の `called_macros()` 相当の機能を実装
   - def-use 関係構築に必要

## 実装順序

1. Step 1: `expand_macro_body()` を追加（既存コードに影響なし）
2. Step 2-4: MacroInferContext を段階的に移行
3. Step 5-6: TokenExpander を削除、テスト修正
