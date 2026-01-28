# calls_unavailable マクロのコメントアウト出力

## 概要

`calls_unavailable` により出力を抑制されたマクロ関数についても、
コメントアウトされた形でエントリーを出力する。
これにより、生成された関数と同じ並び順の中に抑制されたものも含まれ、
何がスキップされたか明確になる。

## 現在の動作

```rust
// should_include_macro で calls_unavailable をチェック
fn should_include_macro(&self, info: &MacroInferInfo) -> bool {
    // ...
    if info.calls_unavailable {
        return false;  // 完全にスキップ
    }
    true
}
```

## 期待される出力

```rust
// =============================================================================
// MACRO FUNCTIONS
// =============================================================================

/// SvFLAGS - macro function
#[inline]
pub unsafe fn SvFLAGS(sv: *mut SV) -> U32 {
    unsafe { (*sv).sv_flags }
}

// [CALLS_UNAVAILABLE] MEM_WRAP_CHECK - calls unavailable function(s)
// Unavailable: Perl_croak_memory_wrap

/// SvREFCNT - macro function
#[inline]
pub unsafe fn SvREFCNT(sv: *mut SV) -> U32 {
    // ...
}
```

## 実装方針

### 方針A: should_include_macro の変更なし + 別途出力

1. `should_include_macro` は現状のまま（`calls_unavailable` で false を返す）
2. マクロ出力ループで、抑制されたマクロも含めてソート
3. 各マクロについて、`calls_unavailable` なら コメントアウト出力

### 方針B: GenerateStatus に新状態を追加

1. `GenerateStatus` に `CallsUnavailable` を追加
2. `get_macro_status` で `calls_unavailable` をチェック
3. `generate_macro_function` で `CallsUnavailable` の場合はコメント出力

**推奨: 方針A**（シンプルで変更箇所が少ない）

## 実装設計

### 出力フォーマット

```rust
// [CALLS_UNAVAILABLE] マクロ名 - calls unavailable function(s)
// Unavailable: 関数名1, 関数名2, ...
```

### コード変更

`generate_macro_functions` のループを変更：

```rust
fn generate_macro_functions(&mut self, result: &InferResult) -> io::Result<()> {
    // 全ターゲットマクロを取得（calls_unavailable を含む）
    let mut macros: Vec<_> = result.macro_ctx.macros.iter()
        .filter(|(_, info)| info.is_target && info.has_body && info.is_function)
        .collect();

    // 名前でソート
    macros.sort_by_key(|(name, _)| self.interner.get(**name));

    for (name, info) in macros {
        if info.calls_unavailable {
            // コメントアウト出力
            self.generate_unavailable_comment(name, info)?;
        } else {
            // 通常の関数出力
            self.generate_single_macro_function(name, info)?;
        }
    }
    Ok(())
}
```

### 新メソッド

```rust
fn generate_unavailable_comment(
    &mut self,
    name: &InternedStr,
    info: &MacroInferInfo,
) -> io::Result<()> {
    let macro_name = self.interner.get(*name);

    // 呼び出している利用不可関数を収集
    let unavailable_fns: Vec<_> = info.called_functions.iter()
        .filter(|&fn_id| {
            let fn_name = self.interner.get(*fn_id);
            // bindings.rs にもマクロにも存在しない関数
            !self.is_available_function(fn_name, fn_id)
        })
        .map(|fn_id| self.interner.get(*fn_id))
        .collect();

    writeln!(self.writer, "// [CALLS_UNAVAILABLE] {} - calls unavailable function(s)", macro_name)?;
    if !unavailable_fns.is_empty() {
        writeln!(self.writer, "// Unavailable: {}", unavailable_fns.join(", "))?;
    }
    writeln!(self.writer)?;

    Ok(())
}
```

## 実装手順

| Step | 内容 |
|------|------|
| 1 | `generate_macro_functions` のフィルタリングロジックを変更 |
| 2 | `generate_unavailable_comment` メソッドを追加 |
| 3 | 利用不可関数の判定ヘルパーを追加（必要なら） |
| 4 | テストと検証 |

## 変更対象ファイル

| ファイル | 変更内容 |
|----------|----------|
| `src/rust_codegen.rs` | `generate_macro_functions` の変更、`generate_unavailable_comment` 追加 |

## テスト方法

```bash
# 生成結果を確認
cargo run --bin libperl-macrogen -- samples/xs-wrapper.h --auto --gen-rust \
  --bindings samples/bindings.rs --apidoc samples/embed.fnc 2>/dev/null | \
  grep "CALLS_UNAVAILABLE" | head -10

# 結合テスト
~/blob/libperl-rs/12-macrogen-2-build.zsh
```

## 備考

- コメントアウトされたエントリーはコンパイルに影響しない
- 何がスキップされたか一目でわかるようになる
- 将来的に bindings.rs に追加された場合、自動的に生成されるようになる
