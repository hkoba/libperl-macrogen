# Plan: Expand Object Macros Within Preserved Function Macro Arguments

## Goal

関数マクロを保存（展開しない）モードで処理する際、引数内のオブジェクトマクロを展開する。

例: `amagic_call(sv, &PL_sv_undef, meth, flags)` において、
`amagic_call` は保存しつつ `PL_sv_undef` は展開する。

## Background

### 問題

`expand_tokens_for_inference` において、`explicit_expand_macros` に含まれない関数マクロは
「保存」される。しかし、引数トークンが生のままコピーされるため、引数内のオブジェクトマクロが
展開されない。

**現象の例**:
```c
// 入力: AMG_CALLunary マクロを展開すると
amagic_call(sv, &PL_sv_undef, method, flags)

// 期待: PL_sv_undef が展開される
amagic_call(sv, &(PL_interpvar_table->Isv_undef), method, flags)

// 現状: 生トークンがそのまま残る
amagic_call(sv, &PL_sv_undef, method, flags)  // ← 未展開
```

### 原因箇所

`src/preprocessor.rs:3208-3214`:

```rust
// preserve_function_macros モード: explicit_expand に含まれない場合は保存
if !self.explicit_expand_macros.contains(&id) {
    // 展開しない: 識別子と引数をそのまま残す
    result.push(token.clone());
    result.extend(tokens[i + 1..i + 1 + consumed].iter().cloned());  // ← 生トークン
    i += 1 + consumed;
    continue;
}
```

### 既存の正しいパターン

`expand_macro_body_for_inference` では引数を展開してから使用している:

```rust
let (expanded_arg, arg_called) = self.expand_tokens_for_inference(
    arg_tokens,
    in_progress,
)?;
```

## Design

### 修正方針

`try_collect_args_from_tokens` で収集した引数 `args` に対して、各引数を
`expand_tokens_for_inference` で展開してから保存する。

### 実装

**File**: `src/preprocessor.rs`

**修正箇所**: `expand_tokens_for_inference` 関数内、関数マクロ保存の分岐

```rust
// preserve_function_macros モード: explicit_expand に含まれない場合は保存
if !self.explicit_expand_macros.contains(&id) {
    // 関数名は保存
    result.push(token.clone());

    // 引数を展開してから保存
    // 開き括弧
    result.push(Token::new(TokenKind::LParen, token.loc.clone()));

    for (arg_idx, arg_tokens) in args.iter().enumerate() {
        if arg_idx > 0 {
            result.push(Token::new(TokenKind::Comma, token.loc.clone()));
        }
        // 引数内のオブジェクトマクロを展開
        let (expanded_arg, arg_called) = self.expand_tokens_for_inference(
            arg_tokens,
            in_progress,
        )?;
        called_macros.extend(arg_called);
        result.extend(expanded_arg);
    }

    // 閉じ括弧
    result.push(Token::new(TokenKind::RParen, token.loc.clone()));

    i += 1 + consumed;
    continue;
}
```

## Implementation Steps

### Step 1: 修正の実装

`src/preprocessor.rs` の `expand_tokens_for_inference` 関数内で、
関数マクロを保存する分岐を修正する。

### Step 2: テスト

既存のテストが通ることを確認し、新しい動作を検証。

## Files to Modify

| File | Changes |
|------|---------|
| `src/preprocessor.rs` | `expand_tokens_for_inference` の関数マクロ保存分岐を修正 |

## Verification

1. **ビルド確認**:
   ```bash
   cargo build
   ```

2. **テスト**:
   ```bash
   cargo test
   ```

3. **出力確認**:
   ```bash
   cargo run -- --auto --gen-rust samples/xs-wrapper.h --bindings samples/bindings.rs 2>/dev/null | grep -E 'PL_sv_undef|PL_defgv'
   ```

   期待: `PL_sv_undef`, `PL_defgv` が展開された形式で出力される

4. **回帰テスト**:
   ```bash
   cargo test rust_codegen_regression
   ```

## Edge Cases

1. **ネストした関数マクロ**: `foo(bar(PL_sv_undef))`
   - `bar` も保存対象の場合、`bar` の引数も再帰的に展開される

2. **空の引数**: `foo()`
   - 空の引数リストでもエラーにならないこと

3. **可変引数**: `foo(a, b, ...)`
   - 全ての引数が展開されること

## Notes

- この変更は `expand_tokens_for_inference` のみに影響
- 通常のマクロ展開（`expand_token_list`）には影響しない
- `wrapped_macros` の処理では既に `expand_token_list_preserve_fn` で引数を展開しているため、
  同様のパターンに従う
