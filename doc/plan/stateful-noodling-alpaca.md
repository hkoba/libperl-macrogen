# Plan: C99 style variadic macro (`#define FOO(...)`) の __VA_ARGS__ 展開修正

## Context

`cargo run -- --auto -E samples/wrapper.h` で `perlvars.h` の
`PERLVARI(G, csighandlerp, Sighandler_t, Perl_csighandler)` が

```c
Sighandler_t PL_csighandlerp = __VA_ARGS__ ;
```

と展開される。正しくは `= Perl_csighandler` であるべき。

原因: `INTERN.h` で `INIT` マクロが C99 style variadic として定義されている:

```c
#define INIT(...) = __VA_ARGS__
```

PERLVARI の展開結果 `INIT(Perl_csighandler)` のさらなる展開時に、
`INIT` の body 内の `__VA_ARGS__` トークンが引数に置換されない。

## 根本原因

**ファイル**: `src/preprocessor.rs`

### `parse_macro_params()` (L1796-1806)

C99 style `#define INIT(...)` のパース時:
- `TokenKind::Ellipsis` で `is_variadic = true` を設定し break
- `params` は空のまま返される → `params = [], is_variadic = true`

### `try_expand_macro_internal()` (L2589)

展開時の条件分岐:
```rust
if *is_variadic && !params.is_empty() {
    // ← C99 style は params が空なのでここに入らない
    ...
} else {
    // ← こちらに入る（非可変長と同じ処理）
    // params は空なので何もマップされない
    // __VA_ARGS__ トークンは未置換のまま残る
}
```

### TinyCC の実装 (`tccpp.c` L1645-1646)

```c
if (varg == TOK_DOTS) {
    varg = TOK___VA_ARGS__;  // ... を __VA_ARGS__ パラメータに置換
    is_vaargs = 1;
}
```

TinyCC は C99 `...` を `__VA_ARGS__` という名前のパラメータとして登録する。
これにより、パラメータリストに必ずエントリが存在し、展開時に通常の
パラメータ置換で `__VA_ARGS__` がマッチする。

## 修正方針

TinyCC と同じアプローチ: `parse_macro_params()` で C99 style `...` を検出した際、
`__VA_ARGS__` を interned した ID を params に追加する。

### 変更箇所

**ファイル**: `src/preprocessor.rs`

#### `parse_macro_params()` (L1796-1806)

```rust
TokenKind::Ellipsis => {
    // 標準 C99: ... のみ（__VA_ARGS__ として扱う）
    is_variadic = true;
    // __VA_ARGS__ をパラメータ名として登録（TinyCC と同じ方式）
    let va_args_id = self.interner.intern("__VA_ARGS__");
    params.push(va_args_id);
    let next = self.next_raw_token()?;
    if !matches!(next.kind, TokenKind::RParen) {
        return Err(CompileError::Preprocess {
            loc: token.loc,
            kind: PPError::InvalidMacroArgs("expected ')' after '...'".to_string()),
        });
    }
    break;
}
```

この変更により:
- `#define INIT(...)` → `params = [__VA_ARGS__], is_variadic = true`
- 展開時の `is_gnu_style` 判定: `last_param == va_args_id` → `is_gnu_style = false`
- `normal_param_count = params.len()` (= 1)
- `args[0]` が `__VA_ARGS__` にマップされる
- body 内の `__VA_ARGS__` トークンが正しく置換される

#### `try_expand_macro_internal()` (L2589)

条件 `!params.is_empty()` が自然に true になるため、変更不要。
既存の展開ロジックがそのまま動作する。

ただし一点確認が必要: `collect_macro_args()` (L2766) への `param_count` 引数。
`params.len()` が 1 になるので、`INIT(a, b, c)` のような呼び出し時に
`args.len() >= param_count` (= `args.len() >= 1`) で正しくコンマが
可変長引数に含まれる。これは正しい動作。

### 変更不要の箇所

- `collect_macro_args()`: `param_count` が正しく渡されるため変更不要
- `expand_tokens()`: `__VA_ARGS__` は通常の Ident 置換で処理されるため変更不要
- `try_expand_macro_internal()`: 条件分岐が自然に正しいパスを通るため変更不要
- `macro_def.rs`: `MacroKind::Function` の構造に変更不要

## 検証

### 1. 単体テスト用ケース

```c
/* C99 standard variadic: ... only */
#define INIT(...) = __VA_ARGS__
INIT(hello)                    /* → = hello */
INIT(a, b, c)                  /* → = a, b, c */

/* GNU extension: NAME... */
#define INIT2(args...) = args
INIT2(hello)                   /* → = hello */

/* Mixed: normal + ... */
#define FOO(a, ...) a(__VA_ARGS__)
FOO(f, 1, 2)                  /* → f(1, 2) */

/* Empty variadic */
#define BAR(...) {__VA_ARGS__}
BAR()                          /* → {} */

/* ## __VA_ARGS__ (GCC comma elision) */
#define LOG(fmt, ...) printf(fmt, ##__VA_ARGS__)
LOG("hello")                   /* → printf("hello") */
```

### 2. 統合テスト

```bash
# PERLVARI の正しい展開を確認
cargo run -- --auto -E samples/wrapper.h 2>/dev/null | grep PL_csighandlerp
# 期待: Sighandler_t PL_csighandlerp = Perl_csighandler ;
```

### 3. 既存テスト

```bash
cargo test
# 全テスト通過を確認
```

### 4. xs-wrapper.h との互換

```bash
# gen-rust の stats が悪化しないこと
cargo run -- --auto --gen-rust samples/xs-wrapper.h --bindings samples/bindings.rs 2>&1 | tail -3
```
