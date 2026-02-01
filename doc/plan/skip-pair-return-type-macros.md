# pair 戻り値型マクロのインライン展開

## 概要

apidoc で戻り値型が `pair` となっているマクロ（例: `STR_WITH_LEN`）は、
カンマ式で複数の値を返すため、単一の戻り値を持つ Rust 関数として生成できない。

これらのマクロは：
1. 関数を生成しない
2. **呼び出し側でインライン展開する**（重要）

## 対象

```c
// handy.h
=for apidoc Amu|pair|STR_WITH_LEN|"literal string"
#define STR_WITH_LEN(s)  ASSERT_IS_LITERAL(s), (sizeof(s)-1)
```

`STR_WITH_LEN("foo")` は `"foo", 3` に展開される。

## 現状の問題

### 問題1: STR_WITH_LEN 自体

```
// [PARSE_FAILED] STR_WITH_LEN(s)
```

### 問題2: STR_WITH_LEN を呼び出すマクロ

```rust
// cop_hints_exists_pvs の現在の出力
pub unsafe fn cop_hints_exists_pvs(..., key: c_int, ...) -> bool {
    cBOOL(Perl_refcounted_he_fetch_pvn(..., STR_WITH_LEN(key), 0, ...))
    //                                      ^^^^^^^^^^^^^^^^^ 関数呼び出し形式のまま
}
```

## 期待される出力

```rust
pub unsafe fn cop_hints_exists_pvs(..., key: *const c_char, len: STRLEN, ...) -> bool {
    cBOOL(Perl_refcounted_he_fetch_pvn(..., key, len, 0, ...))
    //                                      ^^^  ^^^ 2つの引数に展開
}
```

## 実装方針

### Phase 1: pair マクロの識別と登録

1. apidoc から `return_type == "pair"` のマクロを収集
2. `MacroInferContext` に `pair_macros: HashSet<InternedStr>` を追加
3. これらのマクロは関数生成をスキップ

### Phase 2: 呼び出し側でのインライン展開

コード生成時に `ExprKind::Call` を処理する際：

```rust
ExprKind::Call { func, args } => {
    if let ExprKind::Ident(name) = &func.kind {
        // pair マクロの呼び出しをインライン展開
        if self.is_pair_macro(*name) {
            return self.expand_pair_macro_call(*name, args, info);
        }
    }
    // 通常の関数呼び出し処理
}
```

### Phase 3: STR_WITH_LEN の展開ロジック

```rust
fn expand_pair_macro_call(&self, name: InternedStr, args: &[Expr], info: &MacroInferInfo) -> String {
    let name_str = self.interner.get(name);
    match name_str {
        "STR_WITH_LEN" => {
            // STR_WITH_LEN(arg) → arg, (arg.len())
            // 実際には引数をそのまま2つの引数として展開
            let arg = self.expr_to_rust(&args[0], info);
            format!("{}, ({}.len())", arg, arg)
        }
        _ => {
            // 他の pair マクロは将来対応
            format!("/* unsupported pair macro: {} */", name_str)
        }
    }
}
```

### Phase 4: パラメータの伝播

`STR_WITH_LEN(key)` を呼び出すマクロでは、`key` パラメータが
実際には「文字列 + 長さ」のペアを表す。

**オプション A**: パラメータを2つに分割
```rust
// key を key_ptr と key_len に分割
fn cop_hints_exists_pvs(..., key_ptr: *const c_char, key_len: STRLEN, ...) {
    ...(..., key_ptr, key_len, ...)
}
```

**オプション B**: 呼び出し側でリテラル展開を期待
```rust
// key をそのまま使用し、呼び出し側で展開
fn cop_hints_exists_pvs(..., key: &CStr, ...) {
    ...(..., key.as_ptr(), key.to_bytes().len(), ...)
}
```

**推奨**: オプション A（シンプルで明示的）

## 実装手順

| Step | 内容 |
|------|------|
| 1 | `MacroInferInfo` に `is_pair_macro: bool` を追加 |
| 2 | apidoc パース時に pair マクロを識別 |
| 3 | pair マクロは `GenerateStatus::InlineOnly` として処理 |
| 4 | `expr_to_rust` で pair マクロ呼び出しを検出・展開 |
| 5 | pair マクロを呼び出すマクロのパラメータを調整 |
| 6 | テストと検証 |

## 変更対象ファイル

| ファイル | 変更内容 |
|----------|----------|
| `src/macro_infer.rs` | `is_pair_macro` フラグ、pair マクロ検出 |
| `src/rust_codegen.rs` | インライン展開ロジック、パラメータ調整 |
| `src/apidoc.rs` | pair 戻り値型の判定ヘルパー |

## テスト方法

```bash
# STR_WITH_LEN が展開されることを確認
cargo run -- samples/xs-wrapper.h --auto --gen-rust --bindings samples/bindings.rs 2>&1 | \
  grep -A5 "cop_hints_exists_pvs"

# STR_WITH_LEN が関数呼び出しとして残っていないことを確認
cargo run -- samples/xs-wrapper.h --auto --gen-rust --bindings samples/bindings.rs 2>&1 | \
  grep -v "^//" | grep "STR_WITH_LEN"
# → 出力がないこと

# 結合テスト
~/blob/libperl-rs/12-macrogen-2-build.zsh
```

## 複雑性の考慮

この機能は以下の点で複雑：

1. **パラメータ数の変化**: `STR_WITH_LEN(key)` を使うマクロは、
   元のパラメータ `key` が2つのパラメータ（ptr + len）に変わる

2. **型推論への影響**: pair マクロのパラメータは特殊な型推論が必要

3. **ネストした呼び出し**: `foo(STR_WITH_LEN(bar(x)))` のようなケース

## 段階的実装の提案

1. **Step 1**: pair マクロ自体の関数生成をスキップ（コメント出力）
2. **Step 2**: pair マクロを呼び出すマクロを `CALLS_PAIR_MACRO` としてマーク
3. **Step 3**: インライン展開の実装（将来）

Step 1-2 で問題のあるマクロを明示的にし、Step 3 で本格的な展開を実装する。
