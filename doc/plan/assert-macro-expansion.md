# assert マクロ内のマクロ展開対応

## 問題

`assert()` マクロの引数内にあるマクロ（`PL_markstack_ptr` など）が展開されない。

### 例

```rust
// 現在の出力（誤り）
assert!(((PL_markstack_ptr > PL_markstack) || ...));

// 期待する出力
assert!((((*my_perl).Imarkstack_ptr > (*my_perl).Imarkstack) || ...));
```

## 原因

### データフロー

1. プリプロセッサが `assert(PL_markstack_ptr > PL_markstack)` を処理
2. `collect_macro_args` で引数トークンを収集 → **展開前の生トークン**
3. `MacroInvocationKind::Function { args }` に格納（`src/token.rs:52-53`）
4. `wrap_with_markers` でマーカーと共に出力（`src/preprocessor.rs:2606-2612`）
5. パーサーが `parse_expr_from_tokens` で args をパース（`src/parser.rs:2093`）
6. `PL_markstack_ptr` は単なる識別子としてパースされる

### 根本原因

`MacroInvocationKind::Function { args }` の `args` は「展開前の生トークン」（コメント記載）。
wrapped マクロ（assert）の場合でも展開されていないため、内部のマクロが残る。

## 解決策

### 方法: プリプロセッサで args を展開

wrapped マクロの場合、`args` を格納する前にマクロ展開を適用する。

### 既存メソッドの活用

`expand_token_list` メソッド（line 2750-2796）が既に存在する。
引数 prescan 用に作られたが、wrapped マクロの args 展開にも使える。

```rust
/// トークンリストを展開（引数prescan用）
fn expand_token_list(&mut self, tokens: &[Token]) -> Result<Vec<Token>, CompileError>
```

### 実装

`src/preprocessor.rs` の `expand_one_macro` 関数を修正 (line 2606-2612):

```rust
// 既存コード
let wrapped = self.wrap_with_markers(
    marked,
    id,
    token,
    MacroInvocationKind::Function { args },
    &call_loc,
);

// 修正後
let kind = if self.wrapped_macros.contains(&id) {
    // wrapped マクロの場合、引数を展開してから格納
    let expanded_args: Result<Vec<_>, _> = args.into_iter()
        .map(|arg_tokens| {
            let expanded = self.expand_token_list(&arg_tokens)?;
            // 展開結果からマーカーを除去（入れ子 assert エラー防止）
            Ok(expanded.into_iter()
                .filter(|t| !matches!(t.kind, TokenKind::MacroBegin(_) | TokenKind::MacroEnd(_)))
                .collect())
        })
        .collect();
    MacroInvocationKind::Function { args: expanded_args? }
} else {
    MacroInvocationKind::Function { args }
};
let wrapped = self.wrap_with_markers(
    marked,
    id,
    token,
    kind,
    &call_loc,
);
```

### 追加対応: マーカーの除去

展開後のトークンに他の wrapped マクロのマーカー（`MacroBegin`/`MacroEnd`）が
含まれると、パーサーが「入れ子 assert」エラーを出す。
そのため、展開後にマーカーを除去する必要がある。

## 変更対象ファイル

| ファイル | 変更内容 |
|----------|----------|
| `src/preprocessor.rs` | wrapped マクロの args 展開 |

## 注意点

1. **再帰展開の制御**: `expand_token_list` は内部で `try_expand_macro` を呼び、NoExpandRegistry を適用
2. **既存 prescan との違い**: `prescan_args` は HashMap<param, tokens> 形式。ここでは Vec<Vec<Token>> を展開
3. **性能**: wrapped マクロは少数（assert 系のみ）なので影響は軽微

## テスト

1. `cargo build` でビルド確認
2. `cargo test` で既存テストが通ることを確認
3. `Perl_POPMARK` の assert が正しく展開されることを確認:
   - `PL_markstack_ptr` → `(*my_perl).Imarkstack_ptr`
   - `PL_markstack` → `(*my_perl).Imarkstack`
