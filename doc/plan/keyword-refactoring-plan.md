# キーワード処理のリファクタリング計画

## 概要

現在 `parser.rs` の `Keywords` 構造体で管理しているキーワードを `token::TokenKind` に移行し、
`if id == self.kw.kw_*` パターンを Rust の `match` 式に置き換える。

## 背景

### 現状の問題点

1. **冗長なコード**: キーワードチェックが `if-else` の連鎖で書かれている
   ```rust
   if id == self.kw.kw_inline || id == self.kw.kw_inline2 || id == self.kw.kw_inline3 {
       // ...
   }
   ```

2. **二重定義**: `token.rs` と `parser.rs` の両方でキーワードが定義されている

3. **型安全性の欠如**: `InternedStr` の比較は実行時エラーの可能性がある

### TinyCC の方式（参考）

TinyCC では:
- キーワードはトークン値（`TOK_INT`, `TOK_CONST1` など）として定義
- `switch(tok)` で直接マッチ
- typedef 名はシンボルテーブル検索で処理

```c
switch(tok) {
case TOK_CHAR: ...
case TOK_INT: ...
case TOK_CONST1:
case TOK_CONST2:
case TOK_CONST3: ...
default:
    // typedef 名はシンボルテーブルで検索
    s = sym_find(tok);
    if (!s || !(s->type.t & VT_TYPEDEF))
        goto the_end;
}
```

## リファクタリング後の設計

### 1. TokenKind の拡張

```rust
pub enum TokenKind {
    // 既存のキーワード
    KwInt,
    KwChar,
    KwInline,
    // ...

    // 追加するGCC拡張キーワード
    KwInline2,      // __inline
    KwInline3,      // __inline__
    KwRestrict2,    // __restrict
    KwRestrict3,    // __restrict__
    KwSigned2,      // __signed__
    KwBool2,        // bool (C23)
    KwConst2,       // __const
    KwConst3,       // __const__
    KwVolatile2,    // __volatile
    KwVolatile3,    // __volatile__
    KwAttribute,    // __attribute__
    KwAttribute2,   // __attribute
    KwAsm,          // asm
    KwAsm2,         // __asm
    KwAsm3,         // __asm__
    KwAlignof2,     // __alignof
    KwAlignof3,     // __alignof__
    KwTypeof,       // typeof (C23)
    KwTypeof2,      // __typeof
    KwTypeof3,      // __typeof__
    KwInt128,       // __int128
    KwThread,       // __thread
    KwExtension,    // __extension__
    // ...
}
```

### 2. レキサーの変更

```rust
fn scan_identifier(&mut self) -> Result<TokenKind> {
    let text = std::str::from_utf8(&self.source[start..self.pos]).unwrap();

    // キーワードなら変換、そうでなければ識別子
    if let Some(kw) = TokenKind::from_keyword(text) {
        Ok(kw)
    } else {
        Ok(TokenKind::Ident(self.interner.intern(text)))
    }
}
```

### 3. パーサーの変更

```rust
// Before
if id == self.kw.kw_inline || id == self.kw.kw_inline2 || id == self.kw.kw_inline3 {
    specs.is_inline = true;
}

// After
match &self.current.kind {
    TokenKind::KwInline | TokenKind::KwInline2 | TokenKind::KwInline3 => {
        specs.is_inline = true;
    }
    // ...
}
```

### 4. typedef 名の処理

typedef 名は引き続き `HashSet<InternedStr>` で管理:

```rust
match &self.current.kind {
    // キーワード
    TokenKind::KwInt | TokenKind::KwChar | ... => { /* 型指定子 */ }

    // typedef 名
    TokenKind::Ident(id) if self.typedefs.contains(id) => { /* typedef */ }

    // 通常の識別子
    TokenKind::Ident(id) => { /* 識別子 */ }

    _ => { /* その他 */ }
}
```

### 5. `__builtin_va_list` について

現在は `typedefs.insert()` で事前登録しているが、これは暫定的な対応。
将来的には TinyCC のように C ヘッダー経由で定義する方が正しい:

```c
// tccdefs.h 相当のヘッダー
typedef char *__builtin_va_list;
```

## 変更対象ファイル

| ファイル | 変更内容 | 変更量 |
|---------|---------|--------|
| `src/token.rs` | キーワードバリアント追加、`from_keyword()` 更新 | +100行 |
| `src/lexer.rs` | 識別子スキャン時にキーワード変換 | +10行 |
| `src/parser.rs` | `Keywords` 削除、`match` 式への書き換え | -80行/+50行 |

## 実装手順

### Phase 1: token.rs の拡張
1. 新しいキーワードバリアントを追加
2. `from_keyword()` を更新（全キーワード文字列 → TokenKind のマッピング）
3. `format()` を更新（TokenKind → 文字列のマッピング）
4. 既存テストが通ることを確認

### Phase 2: lexer.rs の変更
1. `scan_identifier()` でキーワード変換を追加
2. レキサーテストを更新・追加
3. テストが通ることを確認

### Phase 3: parser.rs のリファクタリング
1. `Keywords` 構造体を削除
2. `parse_declaration_specifiers()` を `match` 式に書き換え
3. その他のキーワードチェック箇所を順次書き換え
4. パーサーテストが通ることを確認

### Phase 4: 統合テスト
1. 全テストが通ることを確認
2. `samples/wrapper.h` のパースが成功することを確認

## 注意事項

1. **プリプロセッサとの互換性**
   - マクロ名としてキーワードを使う場合（例: `#define int MY_INT`）
   - 現在のプリプロセッサ実装では問題にならない（マクロ展開後にパーサーが処理）

2. **GCC拡張の別名**
   - `inline`, `__inline`, `__inline__` は別バリアントとして保持
   - `match` パターンで `|` を使って同等に扱う

3. **テストの更新**
   - レキサーテストでキーワードが `Ident` ではなく `Kw*` として返ることを反映

## 関連ファイル

- `tinycc/tcctok.h` - TinyCC のキーワード定義（参考）
- `tinycc/tccgen.c` - TinyCC の `parse_btype()` 実装（参考）
