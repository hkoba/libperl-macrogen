# インライン関数を利用可能性チェックに含める

## 概要

`CvDEPTH(sv)` マクロが `Perl_CvDEPTH` を呼び出すが、`Perl_CvDEPTH` は
インライン関数として定義されている。現在の実装ではインライン関数を
チェックしていないため、`CALLS_UNAVAILABLE` と誤判定されている。

## 現在の動作

```
// [CALLS_UNAVAILABLE] CvDEPTH(sv) - calls unavailable function(s)
// Unavailable: Perl_CvDEPTH
```

しかし `Perl_CvDEPTH` はインライン関数として存在するため、
`CvDEPTH` は正常に生成されるべき。

## 変更箇所

### 1. macro_infer.rs: check_function_availability

```rust
fn check_function_availability(
    &mut self,
    rust_decl_dict: Option<&RustDeclDict>,
    inline_fn_dict: Option<&InlineFnDict>,  // 追加
    interner: &StringInterner,
) {
    // ...

    // インライン関数として存在する場合はOK（追加）
    if let Some(inline_fns) = inline_fn_dict {
        if inline_fns.get(called_fn).is_some() {
            continue;
        }
    }

    // ...
}
```

### 2. macro_infer.rs: analyze_all_macros での呼び出し

```rust
// Step 4.5: 利用不可関数呼び出しのチェックと伝播
self.check_function_availability(rust_decl_dict, inline_fn_dict, interner);
```

### 3. rust_codegen.rs: is_function_available

```rust
fn is_function_available(&self, fn_id: crate::InternedStr, fn_name: &str, result: &InferResult) -> bool {
    // ...

    // インライン関数として存在する場合はOK（追加）
    if let Some(inline_fn_dict) = &result.inline_fn_dict {
        if inline_fn_dict.get(fn_id).is_some() {
            return true;
        }
    }

    // ...
}
```

## 実装手順

| Step | 内容 |
|------|------|
| 1 | `check_function_availability` に `inline_fn_dict` パラメータを追加 |
| 2 | インライン関数の存在チェックを追加 |
| 3 | `analyze_all_macros` での呼び出しを更新 |
| 4 | `is_function_available` にインライン関数チェックを追加 |
| 5 | テストと検証 |

## 変更対象ファイル

| ファイル | 変更内容 |
|----------|----------|
| `src/macro_infer.rs` | `check_function_availability` の修正 |
| `src/rust_codegen.rs` | `is_function_available` の修正 |

## テスト方法

```bash
# CvDEPTH が正常に生成されることを確認
cargo run --bin libperl-macrogen -- samples/xs-wrapper.h --auto --gen-rust \
  --bindings samples/bindings.rs --apidoc samples/embed.fnc 2>/dev/null | \
  grep -A5 "pub unsafe fn CvDEPTH"

# CALLS_UNAVAILABLE から CvDEPTH が消えていることを確認
cargo run --bin libperl-macrogen -- samples/xs-wrapper.h --auto --gen-rust \
  --bindings samples/bindings.rs --apidoc samples/embed.fnc 2>/dev/null | \
  grep "CALLS_UNAVAILABLE.*CvDEPTH"
# → 出力がないこと

# 結合テスト
~/blob/libperl-rs/12-macrogen-2-build.zsh
```

## 期待される効果

- インライン関数を呼び出すマクロが正しく生成される
- `CALLS_UNAVAILABLE` の誤検出が減少
