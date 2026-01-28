# C言語キーワードの特別処理

## 概要

C言語の特定のキーワード/マクロを Rust コード生成時に適切に変換する。

## 対象

| C | 現状 | 期待する出力 |
|---|------|-------------|
| `true` | `r#true` | `true` |
| `false` | `r#false` | `false` |
| `__FILE__` | `__FILE__` | `file!()` |
| `__LINE__` | `__LINE__` | `line!()` |

## 現在の動作

### true/false の問題

`RUST_KEYWORDS` に `true`, `false` が含まれているため、
`escape_rust_keyword` が `r#true`, `r#false` に変換している。

しかし Rust では `true`/`false` はキーワードではなくリテラルなので、
エスケープ不要。

### __FILE__/__LINE__ の問題

プリプロセッサマクロ `__FILE__`/`__LINE__` が識別子としてそのまま出力されている。
Rust には同等の `file!()`/`line!()` マクロがある。

## 実装方針

### 方針A: escape_rust_keyword を修正

1. `RUST_KEYWORDS` から `true`, `false` を除外
2. `escape_rust_keyword` に `__FILE__`, `__LINE__` の変換を追加

```rust
fn escape_rust_keyword(name: &str) -> String {
    match name {
        "true" | "false" => name.to_string(),  // リテラルはそのまま
        "__FILE__" => "file!()".to_string(),
        "__LINE__" => "line!()".to_string(),
        _ if RUST_KEYWORDS.contains(&name) => format!("r#{}", name),
        _ => name.to_string(),
    }
}
```

### 方針B: 識別子の変換専用関数を追加

```rust
fn ident_to_rust(name: &str) -> String {
    match name {
        "true" | "false" => name.to_string(),
        "__FILE__" => "file!()".to_string(),
        "__LINE__" => "line!()".to_string(),
        _ => escape_rust_keyword(name),
    }
}
```

**推奨: 方針A**（変更箇所が少なく、既存の関数を修正するだけ）

## 実装手順

| Step | 内容 |
|------|------|
| 1 | `RUST_KEYWORDS` から `true`, `false` を除外 |
| 2 | `escape_rust_keyword` に特殊ケースの処理を追加 |
| 3 | テストと検証 |

## 変更対象ファイル

| ファイル | 変更内容 |
|----------|----------|
| `src/rust_codegen.rs` | `RUST_KEYWORDS` と `escape_rust_keyword` の修正 |

## テスト方法

```bash
# true/false の出力確認
cargo run --bin libperl-macrogen -- samples/xs-wrapper.h --auto --gen-rust \
  --bindings samples/bindings.rs --apidoc samples/embed.fnc 2>&1 | \
  grep -E "r#true|r#false"
# → 出力がないこと

# __FILE__/__LINE__ の変換確認
cargo run --bin libperl-macrogen -- samples/xs-wrapper.h --auto --gen-rust \
  --bindings samples/bindings.rs --apidoc samples/embed.fnc 2>&1 | \
  grep -E "file!\(\)|line!\(\)" | head -5

# 結合テスト
~/blob/libperl-rs/12-macrogen-2-build.zsh
```

## 期待される効果

- `true`/`false` が正しい Rust リテラルとして出力される
- `__FILE__`/`__LINE__` が Rust マクロに変換される
- コンパイルエラーの削減
