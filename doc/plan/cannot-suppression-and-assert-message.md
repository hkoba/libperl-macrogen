# Plan: _CANNOT マクロ抑制 と assert メッセージ抽出

## 概要

2つの独立した改良を行う。

1. **`_CANNOT` を含むマクロ関数の生成抑制**
2. **`assert(expr || !"message")` パターンから assert メッセージを抽出**

---

## Task 1: `_CANNOT` マクロ関数の生成抑制

### 背景

`fakesdio.h` / `nostdio.h` で `#define _CANNOT "CANNOT"` が定義され、
以下のような関数マクロが生成される：

```c
#define fread(b,s,c,f)   _CANNOT fread
#define fwrite(b,s,c,f)  _CANNOT fwrite
```

これらは意図的に「この関数は使えない」ことを示すマクロで、
Rust 関数として生成する意味がない。現状はマクロ展開後に本体が
`c"CANNOT"` になり、`[CODEGEN_INCOMPLETE]` としてコメントアウトされている。

### 現状

`tmp/macro_bindings.rs` に 37 箇所。すべて既に `[CODEGEN_INCOMPLETE]` として
コメントアウト済み（パース不可 or 型推論不可で除外されている）。

### 方針

マクロ推論フェーズ（`src/macro_infer.rs`）で、展開済みトークン列に
`_CANNOT`（定数名）が含まれる場合、そのマクロを「生成不要」とマークする。

### 変更箇所

**`src/macro_infer.rs`** — `infer_single_macro()` 内（展開済みトークン列取得後）

```rust
// _CANNOT を含むマクロは生成抑制
let has_cannot = expanded_tokens.iter().any(|t| {
    matches!(&t.kind, TokenKind::StringLit(s) if s == "CANNOT")
        || matches!(&t.kind, TokenKind::Ident(id) if interner.get(*id) == "_CANNOT")
});
if has_cannot {
    info.calls_unavailable = true;  // 既存フラグを流用
    return Ok(info);
}
```

あるいは、`_CANNOT` がオブジェクトマクロとして展開された後は
`StringLit("CANNOT")` トークンになるため、文字列リテラル `"CANNOT"` の
存在チェックだけで十分かもしれない。

### 影響範囲

- 出力は現状と同等（既にコメントアウト済み）
- ただし `[CODEGEN_INCOMPLETE]` ではなく、統計上 `calls_unavailable` に
  分類されるようになる（より正確な分類）

---

## Task 2: `assert(expr || !"message")` パターンの改善

### 背景

C のイディオム:

```c
assert((PL_markstack_ptr > PL_markstack) || !"MARK underflow");
```

`!"string"` は常に `0`（false）に評価されるため、条件が false の場合のみ
assert が発火する。文字列はデバッグ時のメッセージ目的。

### 現状の出力

```rust
assert!((((*my_perl).Imarkstack_ptr > (*my_perl).Imarkstack) || (!((c"MARK underflow") != 0))));
```

`|| (!((c"MARK underflow") != 0))` の部分が不自然。
また `c"MARK underflow"` を `!= 0` と比較しているため、`PartialEq` エラーの
原因にもなっている（`tmp/help/2/1.txt` のエラー）。

### 目標の出力

```rust
assert!((*my_perl).Imarkstack_ptr > (*my_perl).Imarkstack, "MARK underflow");
```

### 出現箇所

Perl ヘッダ全体で1箇所のみ（`inline.h` の `Perl_POPMARK`）。

### 方針

Assert 条件式の codegen 時に、`condition` が `Binary(LogOr, real_cond, Not(StringLit(msg)))`
のパターンにマッチするか検出し、`assert!(real_cond, "msg")` 形式で出力する。

### 変更箇所

**`src/rust_codegen.rs`** — `ExprKind::Assert` ハンドラ（2箇所：macro path L2893, inline path L4511）

パターンマッチのロジック:

```rust
ExprKind::Assert { kind, condition } => {
    // assert(expr || !"message") パターンの検出
    if let ExprKind::Binary { op: BinOp::LogOr, lhs, rhs } = &condition.kind {
        if let Some(msg) = extract_assert_message(rhs) {
            // real_cond = lhs, message = msg
            let cond = self.expr_to_rust_inline(lhs);  // or expr_to_rust(lhs, info)
            let cond_str = self.wrap_as_bool_condition_inline(lhs, &cond);
            let assert_expr = format!("assert!({}, \"{}\")", cond_str, msg);
            return match kind {
                AssertKind::Assert => assert_expr,
                AssertKind::AssertUnderscore => format!("{{ {}; }}", assert_expr),
            };
        }
    }
    // ... 既存のロジック ...
}
```

ヘルパー関数:

```rust
/// assert(expr || !"message") の RHS から文字列を抽出
fn extract_assert_message(expr: &Expr) -> Option<&str> {
    // !("string") or !(string != 0) パターン
    match &expr.kind {
        ExprKind::LogNot(inner) => {
            match &inner.kind {
                ExprKind::StringLit(s) => Some(s.as_str()),
                _ => None,
            }
        }
        _ => None,
    }
}
```

### AST 上の表現

C の `!"MARK underflow"` は AST 上で以下のいずれか:
- `LogNot(StringLit("MARK underflow"))` — パーサが `!` を論理否定として解釈
- `Not(StringLit(...))` — ビット否定として解釈（可能性低い）

実際の AST ノード名は `ExprKind::LogNot` か `ExprKind::Not` かを確認する必要がある。

### 影響

- `Perl_POPMARK` の assert が正しい Rust コードになる
- `PartialEq<{integer}>` is not implemented for `&CStr` エラー（1件）が解消される

---

## 実装順序

1. **Task 2（assert メッセージ）を先に実装** — エラー数を1件削減でき、変更箇所が小さい
2. **Task 1（_CANNOT 抑制）を後で実装** — 既に実質的に除外されているため、分類改善のみ

## テスト

- `cargo test` — 既存テスト通過
- `~/blob/libperl-rs/12-macrogen-2-build.zsh` — エラー数確認
- `grep 'MARK underflow' tmp/macro_bindings.rs` — 出力確認
- `grep 'CANNOT' tmp/macro_bindings.rs | wc -l` — _CANNOT マクロのカウント確認
