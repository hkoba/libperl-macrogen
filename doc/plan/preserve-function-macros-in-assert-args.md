# Preprocessor と TokenExpander のマクロ展開動作を統一する計画

## 背景: コード生成のゴール（CLAUDE.md より）

> C インライン関数と C マクロ関数の両方に対して、コード生成が一貫した挙動をすること。
> 関数呼び出しの引数に対しても、同じ一貫した挙動をすること。

### マクロ処理ルール

| マクロ種別 | 条件 | 処理 |
|-----------|------|------|
| オブジェクトマクロ（定数） | Rust 側に対応する定数定義あり | Rust の定数として出力 |
| オブジェクトマクロ（定数） | Rust 側に対応なし | 展開 |
| 関数マクロ | 特殊辞書に**未登録** | **関数呼び出しの形を維持** |
| 関数マクロ | `ExplicitExpandSymbols` に登録 | 展開 |
| assert 系 | `wrapped_macros` 登録 | 引数を処理して `assert!` として生成 |

**ポイント**: デフォルト動作は「関数マクロを保存」

## 問題

現在、2つの展開エンジンで動作が異なる：

| 処理エンジン | 対象 | 関数マクロのデフォルト |
|-------------|------|----------------------|
| `TokenExpander` | マクロ | 保存 ✓ |
| `Preprocessor` | Inline 関数 | **展開** ✗ |

### 症状

元の C コード:
```c
PERL_STATIC_INLINE I32 *
Perl_CvDEPTH(const CV * const sv)
{
    assert(SvTYPE(sv) == SVt_PVCV || SvTYPE(sv) == SVt_PVFM);
    return &((XPVCV*)SvANY(sv))->xcv_depth;
}
```

現在の生成コード（問題あり）:
```rust
// SvTYPE が展開されてしまっている
assert!((((((*sv).sv_flags & SVTYPEMASK) as svtype) == SVt_PVCV) || ...));
```

期待される生成コード:
```rust
// SvTYPE が関数呼び出しとして残る
assert!((SvTYPE(sv) == SVt_PVCV) || (SvTYPE(sv) == SVt_PVFM));
```

## 原因

`Preprocessor` の `wrapped_macros` 処理 (`src/preprocessor.rs:2621-2634`):

```rust
let kind = if self.wrapped_macros.contains(&id) {
    let expanded_args = args.into_iter()
        .map(|arg_tokens| {
            let expanded = self.expand_token_list(&arg_tokens)?;  // ← 全展開
            // ...
        })
        .collect();
    MacroInvocationKind::Function { args: expanded_args? }
} else {
    MacroInvocationKind::Function { args }
};
```

`expand_token_list` は **全てのマクロ**を展開する。
`TokenExpander` のような `preserve_function_macros` 機構がない。

## 解決策

`Preprocessor` に `TokenExpander` と同等の動作モードを追加する。

### 設計方針

1. **既存の `bindings_consts` パターンを参考にする**
   - `TokenExpander` は `bindings_consts: Option<&dyn KeySet>` を持つ
   - 同様のパターンで `explicit_expand` を渡せるようにする

2. **`expand_token_list` に preserve モードを追加**
   - 関数マクロはデフォルトで保存
   - `explicit_expand` に登録されたもののみ展開
   - オブジェクトマクロは通常通り展開（bindings_consts は除く）

### 実装計画

#### Step 1: `Preprocessor` に `explicit_expand_macros` フィールドを追加

```rust
// src/preprocessor.rs
pub struct Preprocessor<'a> {
    // ... 既存フィールド ...

    /// 明示的に展開するマクロ名（preserve_function_macros モードで使用）
    explicit_expand_macros: HashSet<InternedStr>,
}

impl<'a> Preprocessor<'a> {
    /// 明示展開マクロを追加
    pub fn add_explicit_expand_macro(&mut self, id: InternedStr) {
        self.explicit_expand_macros.insert(id);
    }
}
```

#### Step 2: `expand_token_list` に preserve モードを追加

```rust
fn expand_token_list(&mut self, tokens: &[Token]) -> Result<Vec<Token>, CompileError> {
    self.expand_token_list_internal(tokens, false)
}

fn expand_token_list_preserve_fn(&mut self, tokens: &[Token]) -> Result<Vec<Token>, CompileError> {
    self.expand_token_list_internal(tokens, true)
}

fn expand_token_list_internal(
    &mut self,
    tokens: &[Token],
    preserve_function_macros: bool,
) -> Result<Vec<Token>, CompileError> {
    // ... 既存のセットアップ ...

    while let Some(token) = self.lookahead.pop() {
        if matches!(token.kind, TokenKind::Eof) { break; }
        if matches!(token.kind, TokenKind::Newline) { continue; }

        if let TokenKind::Ident(id) = token.kind {
            // preserve モードでの展開判定
            if let Some(expanded) = self.try_expand_macro_internal(
                id, &token, preserve_function_macros
            )? {
                for t in expanded.into_iter().rev() {
                    self.lookahead.push(t);
                }
                continue;
            }
        }
        result.push(token);
    }
    // ...
}
```

#### Step 3: `try_expand_macro` に preserve モードを追加

```rust
fn try_expand_macro(
    &mut self,
    id: InternedStr,
    token: &Token,
) -> Result<Option<Vec<Token>>, CompileError> {
    self.try_expand_macro_internal(id, token, false)
}

fn try_expand_macro_internal(
    &mut self,
    id: InternedStr,
    token: &Token,
    preserve_function_macros: bool,
) -> Result<Option<Vec<Token>>, CompileError> {
    // skip_expand_macros チェック
    if self.skip_expand_macros.contains(&id) {
        return Ok(None);
    }

    if let Some(def) = self.macro_table.get(id).cloned() {
        match &def.kind {
            MacroKind::Object => {
                // オブジェクトマクロは常に展開
                // ... 既存の処理 ...
            }
            MacroKind::Function { .. } => {
                if preserve_function_macros {
                    // preserve モード: explicit_expand に含まれていなければ保存
                    if !self.explicit_expand_macros.contains(&id) {
                        return Ok(None);
                    }
                }
                // 展開処理
                // ... 既存の処理 ...
            }
        }
    }
    Ok(None)
}
```

#### Step 4: `wrapped_macros` の引数展開を変更

```rust
// src/preprocessor.rs:2621-2634
let kind = if self.wrapped_macros.contains(&id) {
    let expanded_args: Result<Vec<_>, _> = args.into_iter()
        .map(|arg_tokens| {
            // 関数マクロを保存するモードで展開
            let expanded = self.expand_token_list_preserve_fn(&arg_tokens)?;
            Ok(expanded.into_iter()
                .filter(|t| !matches!(t.kind, TokenKind::MacroBegin(_) | TokenKind::MacroEnd(_)))
                .collect())
        })
        .collect();
    MacroInvocationKind::Function { args: expanded_args? }
} else {
    MacroInvocationKind::Function { args }
};
```

#### Step 5: `explicit_expand_macros` の設定

`infer_api.rs` または `pipeline.rs` で設定:

```rust
// ExplicitExpandSymbols のマクロを Preprocessor にも登録
let explicit_expand = ExplicitExpandSymbols::new(pp.interner_mut());
for sym in explicit_expand.iter() {
    pp.add_explicit_expand_macro(sym);
}
```

## 動作確認

### 期待される動作

| マクロ | 種別 | 条件 | 結果 |
|--------|------|------|------|
| `SvTYPE(sv)` | 関数 | `explicit_expand` 未登録 | 保存 `SvTYPE(sv)` |
| `SvANY(sv)` | 関数 | `explicit_expand` 登録済み | 展開 `(*sv).sv_any` |
| `SVt_PVCV` | オブジェクト | bindings.rs に定数あり | **保存** `SVt_PVCV` |
| `SVt_PVCV` | オブジェクト | bindings.rs に定数なし | 展開 |

### テスト

```bash
# Perl_CvDEPTH の出力を確認
cargo run -- --auto --gen-rust samples/wrapper.h 2>&1 | grep -A10 "Perl_CvDEPTH"
```

期待される出力（bindings.rs 指定時）:
```rust
pub unsafe fn Perl_CvDEPTH(sv: *const CV) -> *mut I32 {
    unsafe {
        // SvTYPE: 関数マクロ → 保存
        // SVt_PVCV, SVt_PVFM: bindings.rs に定数あり → Rust 定数として保存
        assert!((SvTYPE(sv) == SVt_PVCV) || (SvTYPE(sv) == SVt_PVFM));
        return (&mut (*((*sv).sv_any as *mut XPVCV)).xcv_depth);
    }
}
```

## 実装順序

1. [x] `Preprocessor` に `explicit_expand_macros` フィールドを追加
2. [x] `add_explicit_expand_macro` メソッドを追加
3. [x] `try_expand_macro_internal` を追加（preserve モード対応）
4. [x] `expand_token_list_internal` を追加（preserve モード対応）
5. [x] `wrapped_macros` の引数展開を `expand_token_list_preserve_fn` に変更
6. [x] `infer_api.rs` で `ExplicitExpandSymbols` を Preprocessor に設定
7. [x] テスト

## 影響範囲

### 変更されるファイル

| ファイル | 変更内容 |
|----------|----------|
| `src/preprocessor.rs` | `explicit_expand_macros`, preserve モード追加 |
| `src/infer_api.rs` | Preprocessor への explicit_expand 設定 |

### 影響を受ける処理

- `wrapped_macros` の引数展開（`assert`, `assert_`）
- Inline 関数内の assert 引数

### 影響を受けない処理

- 通常の Preprocessor 展開（preserve モード未使用）
- `TokenExpander` によるマクロ処理（既に正しく動作）
