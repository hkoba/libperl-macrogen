# Preprocessor: キーワードトークンをマクロ名として受理する（TinyCC 風）

## Context

CI（GHA Ubuntu, gcc 13）で `<stdbool.h>` 36 行目の `#define bool _Bool` を
プリプロセスしようとすると `InvalidDirective("expected macro name")` で落ちる。

直接の原因:
- lexer (`src/token.rs:380`) は `bool` を **キーワードトークン `TokenKind::KwBool2`** に
  分類する。これは識別子 `TokenKind::Ident(InternedStr)` とは別バリアント。
- `#define` の名前読み取り (`src/preprocessor.rs:1684-1693`) は
  **`TokenKind::Ident` のみを受理** し、それ以外は "expected macro name" エラー。

ローカル Fedora（gcc 15）で再現しないのは、`<stdbool.h>` が
`__STDC_VERSION__ > 201710L`（C23）のときに該当ブロックを skip するガードが
入っているため。CI 側の Ubuntu/gcc 13 ではガードが通り、`#define` ブロックを
実際に処理する経路に入って踏む。**環境依存で再現していなかっただけで、
根本的にはこちら側の preprocessor のバグ**。

同じ問題は `bool` 以外でも起き得る（互換ヘッダで `#define inline __inline`
等を行うパターン）。`#undef` / `#ifdef` / `#ifndef` も同じ「Ident 専用」判定で、
キーワード相当の名前を **エラーなく無視** する silent-bug を抱えている
（マクロが未定義扱いになって誤分岐）。

### TinyCC のアプローチ（CLAUDE.md TinyCC Reference Rule）

`tinycc/tccpp.c` の `parse_define` (1616) は `v < TOK_IDENT` のみで弾く。
TinyCC は **キーワードと識別子を統一トークン namespace** に置き、TOK_IDENT 以上の
ID なら全て名前として受理する。`#ifdef` / `#undef` / `defined()` も全て同じ
パターン（`tccpp.c:1933, 1913, 1549`）。本リポジトリの lexer は
キーワードを別バリアントに分類してしまうので、preprocessor 側で
「キーワードでもあれば識別子文字列を取り出して再 intern する」逃げ道を
明示する必要がある。

### 既に部分的に直っている前例

`src/pp_expr.rs:362-394` (`parse_defined`) は **既に同じ方針で修正済み**:

```rust
let name = match self.current_kind() {
    Some(TokenKind::Ident(id)) => Some(*id),
    Some(kind) if kind.is_keyword() => {
        let kw_name = kind.format(self.interner);
        self.interner.lookup(&kw_name)
    }
    _ => return Err(...),
};
```

つまり `#if defined(bool)` は既に通る。一方 `#define bool ...` / `#undef bool` /
`#ifdef bool` は通らない、という非対称な状態。今回の修正でこれを揃える。

## 修正対象（4 箇所）

| ファイル:行 | 関数 | 現状の挙動 | 直後の挙動 |
|------------|------|-----------|-----------|
| `src/preprocessor.rs:1684-1693` | `process_define` | キーワードでエラー | キーワード文字列を intern してマクロ名に |
| `src/preprocessor.rs:1823-1831` | `process_undef` | キーワードを silent ignore | 同上 |
| `src/preprocessor.rs:2048-2053` | `process_ifdef`/`process_ifndef` 共用 | キーワードを未定義扱い | 同上、is_defined 判定 |
| `src/preprocessor.rs:1761-1820` | `parse_macro_params` | キーワード仮引数でエラー | キーワード文字列を intern して仮引数名に |

`#if defined(NAME)` / `defined NAME` の評価は `pp_expr.rs:parse_defined` で
**既に対応済み**なので追加修正不要。

`collect_if_condition` (`preprocessor.rs:2180`) は token を素通しするだけで
名前判定をしていないので修正不要。

## 設計

### 新ヘルパ `TokenKind::keyword_str()`

`src/token.rs` の `TokenKind` に追加。`pp_expr.rs` の既存パターンは
`is_keyword() + format(interner)` の二段で `String` を中継するが、Kw\* バリアントは
全て静的文字列なので、新規に `&'static str` を返すヘルパを定義した方が
意図が明確で zero-alloc。

```rust
impl TokenKind {
    /// キーワードトークン (Kw*) ならその表記文字列を返す。それ以外は None。
    /// プリプロセッサがキーワードをマクロ名として受理するための共通ヘルパ。
    pub fn keyword_str(&self) -> Option<&'static str> {
        match self {
            TokenKind::KwAuto => Some("auto"),
            TokenKind::KwBreak => Some("break"),
            // ... 全 63 Kw* バリアント。
            // 値は src/token.rs:506 の to_string() match と同じ文字列を使う。
            TokenKind::KwBool => Some("_Bool"),
            TokenKind::KwBool2 => Some("bool"),
            TokenKind::KwInline => Some("inline"),
            TokenKind::KwInline2 => Some("__inline"),
            TokenKind::KwInline3 => Some("__inline__"),
            // ... 以下省略
            _ => None,
        }
    }
}
```

実装時は `to_string()` (`src/token.rs:526` 付近) の Kw\* match から
文字列リテラルを抜き出して移植する。重複は避けたいので、可能なら
`to_string()` の Kw\* 部分を `self.keyword_str().map(|s| s.to_string())` で
置き換える形にすると将来追加するときに 1 箇所で済む。

### preprocessor 側の使用パターン

4 箇所すべて同じ形に揃える:

```rust
let name = match name_token.kind {
    TokenKind::Ident(id) => id,
    ref kind if let Some(s) = kind.keyword_str() => self.interner.intern(s),
    _ => return Err(CompileError::Preprocess {
        loc,
        kind: PPError::InvalidDirective("expected macro name".to_string()),
    }),
};
```

`process_undef` / `process_ifdef` は元々エラーを出していなかったが、
**今後はキーワードも黙って無視せず受理する**（silent ignore のままだと
`#undef bool` の効きが期待と違う）。Ident でもキーワードでもない場合に
silent ignore するか warn するかは現状維持（silent）で良い。

`parse_macro_params` の TokenKind::Ident アームを上記パターンに置換する。
仮引数名はマクロのスコープ内ローカルなので、Kw\* 由来の InternedStr で
登録しても他の Ident 由来 `bool` と同一の InternedStr に解決され、
`#define FOO(bool) (bool+1)` のような呼び出しでも正しく一致する。

### 識別子と再 intern の扱い

`Ident(InternedStr)` の側は既に interner 経由で `bool` を登録するので、
キーワード経由で `interner.intern("bool")` した場合と **同じ InternedStr**
が得られる（intern は同名なら同 ID を返す: `src/intern.rs:31`）。
従ってマクロテーブル上で `bool` という名前は一意な ID として扱われる。

### 副次的な懸念：`defined` を `#define` で上書きする件

TinyCC は `parse_define` で `v == TOK_DEFINED` を排除している
(`tccpp.c:1616`)。本リポジトリの `defined` は **キーワードではなく単なる Ident** で
扱われている (`src/preprocessor.rs:2184` で都度 intern)。従って `#define defined ...`
は現状でも素通りしてしまい、`#if defined(X)` が壊れる潜在バグがある。

**今回のスコープ外**。別 issue としてメモ程度に残す（fix するなら process_define
で `defined` という InternedStr と一致したらエラーを返す追加チェックが要る）。

## テスト

`tests/preprocessor_tests.rs` に追加する。既存のテストヘルパ（`preprocess(source)`
+ `collect_tokens` 風: `src/preprocessor_tests.rs:49-57` 参照）を再利用。

- `test_define_keyword_as_name`: `#define bool int\nbool x;` → 展開後 `int x;`
- `test_define_inline_alias`: `#define inline __inline\ninline void f();` →
  `__inline void f();`（ただし `__inline` も Kw\* なのでさらに展開ループに
  入らないことを確認する負例として有用）
- `test_undef_keyword`: `#define bool int\n#undef bool\nbool x;` → `bool x;`（未展開）
- `test_ifdef_keyword`: `#ifdef bool\nyes\n#endif` を `#define bool` ありなしで
  分岐確認
- `test_macro_param_keyword`: `#define FOO(bool) bool+1\nFOO(42)` → `42+1`
- 回帰: `test_defined_keyword_still_works`: `#define bool _Bool\n#if defined(bool)\nyes\n#endif`
  が "yes" を出す（`pp_expr` 側の既存修正が壊れていないことの確認）

CI で当初踏んだ直接ケースは libperl-sys 側の統合ビルドで自動的に検証される。
ローカルでは `cargo test --tests preprocessor_tests` で十分。

## 実装手順

1. **`src/token.rs`**: `keyword_str()` を追加（63 entries）。`to_string()` 既存 match
   との整合性を保つため、文字列リテラルは to_string() の Kw\* match からコピー
2. **`src/preprocessor.rs`**: 4 箇所を新パターンに置換
3. **`tests/preprocessor_tests.rs`**: 上記 6 ケースを追加
4. **`cargo test --all`** で全テスト通過を確認（現在 351 通過のラインを死守）
5. ローカルで `tmp/` に再現用ヘッダ（`#define bool int` を含むもの）を置いて
   `cargo run -- ...` で end-to-end 確認

## クリティカルファイル

- `src/token.rs`（`TokenKind`, `is_keyword()`, `to_string()` の match）
- `src/preprocessor.rs:1684, 1761, 1823, 2048`（4 修正点）
- `src/pp_expr.rs:362-394`（既に正しく実装済みのリファレンス）
- `src/intern.rs:31`（`intern(&str)` API）
- `tinycc/tccpp.c:1616, 1913, 1933, 1549`（TinyCC のリファレンス実装）
- `tests/preprocessor_tests.rs`（既存テストパターン）

## 検証

### ユニット
```
cargo test --tests preprocessor_tests
cargo test --all
```

### CI 相当の回帰
1. `tmp/test-stdbool.h` に `#include <stdbool.h>` 1 行のみのファイルを作る
2. `cargo run -- tmp/test-stdbool.h --gen-rust 2>&1 | head` で
   "expected macro name" エラーが消えていることを確認
3. もしくは Fedora 上で `__STDC_VERSION__` を意図的に古く設定して再現:
   ```
   cargo run -- samples/xs-wrapper.h --gen-rust \
       --define '__STDC_VERSION__=201112L' 2>&1 | grep "expected macro name"
   ```
   が空であること

### 下流統合
libperl-sys 側で `cargo update -p libperl-macrogen` 後、GHA をトリガーして
緑になることを最終確認。

## 非スコープ

- lexer のディレクティブ文脈モード化（B 案）。今回の `keyword_str()` 経由で
  実用上は十分と判断
- `#define defined ...` を弾くチェック追加（既存問題、別途）
- `_STDBOOL_H` の事前定義などのヘッダ単位 workaround（C 案）。根本解決により不要
