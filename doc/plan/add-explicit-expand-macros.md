# 展開対象マクロの追加計画

## 目的

以下のマクロを `ExplicitExpandSymbols`（明示展開リスト）に追加する：
- `EXPECT(expr, val)` - `__builtin_expect` のラッパー
- `LIKELY(cond)` - `__builtin_expect(cond, 1)` のラッパー
- `UNLIKELY(cond)` - `__builtin_expect(cond, 0)` のラッパー
- `cBOOL(cond)` - 条件を bool に変換

これらは最終的に `__builtin_expect(expr, val)` に展開され、コード生成時に `(expr)` に簡略化される。

## 前提：__builtin_expect の処理

**既に実装済み**（`src/rust_codegen.rs:607-608`）：

```rust
ExprKind::Call { func, args } => {
    // __builtin_expect(cond, expected) -> cond
    // GCC の分岐予測ヒントは Rust では無視
    if let ExprKind::Ident(name) = &func.kind {
        let func_name = self.interner.get(*name);
        if func_name == "__builtin_expect" && args.len() >= 1 {
            return self.expr_to_rust(&args[0], info);
        }
    }
    // ...
}
```

3箇所で同じ処理が実装されている（lines 607-608, 1490-1491, 2461-2462）。

## 変更対象ファイル

| ファイル | 変更内容 |
|----------|----------|
| `src/macro_infer.rs` | ExplicitExpandSymbols に4つのフィールドを追加 |

## 実装計画

### ExplicitExpandSymbols の拡張

**場所**: `src/macro_infer.rs:57-82`

```rust
// 変更前
#[derive(Debug, Clone, Copy)]
pub struct ExplicitExpandSymbols {
    pub sv_any: InternedStr,
    pub sv_flags: InternedStr,
}

impl ExplicitExpandSymbols {
    pub fn new(interner: &mut StringInterner) -> Self {
        Self {
            sv_any: interner.intern("SvANY"),
            sv_flags: interner.intern("SvFLAGS"),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = InternedStr> {
        [self.sv_any, self.sv_flags].into_iter()
    }
}

// 変更後
#[derive(Debug, Clone, Copy)]
pub struct ExplicitExpandSymbols {
    pub sv_any: InternedStr,
    pub sv_flags: InternedStr,
    pub expect: InternedStr,
    pub likely: InternedStr,
    pub unlikely: InternedStr,
    pub cbool: InternedStr,
}

impl ExplicitExpandSymbols {
    pub fn new(interner: &mut StringInterner) -> Self {
        Self {
            sv_any: interner.intern("SvANY"),
            sv_flags: interner.intern("SvFLAGS"),
            expect: interner.intern("EXPECT"),
            likely: interner.intern("LIKELY"),
            unlikely: interner.intern("UNLIKELY"),
            cbool: interner.intern("cBOOL"),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = InternedStr> {
        [
            self.sv_any,
            self.sv_flags,
            self.expect,
            self.likely,
            self.unlikely,
            self.cbool,
        ].into_iter()
    }
}
```

## 展開の流れ

```
LIKELY(cond)
    ↓ explicit_expand により展開
__builtin_expect((cond) != 0, 1)
    ↓ コード生成時に簡略化（既存処理）
((cond) != 0)
```

## テスト

1. `cargo test` で既存テストがパスすることを確認
2. `cargo run -- --auto --gen-rust --bindings samples/bindings.rs samples/wrapper.h` で出力確認
3. `LIKELY`, `UNLIKELY` を使用するマクロの出力に `__builtin_expect` が含まれないことを確認

## 実装順序

1. [ ] `ExplicitExpandSymbols` に4つのフィールドを追加
2. [ ] `new()` メソッドを更新
3. [ ] `iter()` メソッドを更新
4. [ ] テスト用コード `test_explicit_expand_symbols_iter` を更新
5. [ ] テスト実行と出力確認
