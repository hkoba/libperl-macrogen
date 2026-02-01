# Inline 関数内のマクロ呼び出し保存計画

## 目的

inline 関数内で使用されるマクロ関数（`SvTYPE`, `isREGEXP` 等）を、展開せずに関数呼び出しとして保存する。

**関連**: CLAUDE.md の「Current Implementation Gap」セクション

## 現状分析

### assert の保存機構（既存・動作中）

```
assert(cond)
    ↓ Preprocessor (wrapped_macros)
MacroBegin { name: "assert", args: [cond] }
((void)0)
MacroEnd
    ↓ Parser
Assert { kind: AssertKind::Assert, condition: cond }
    ↓ RustCodegen
assert!((cond) != 0)
```

### SvTYPE の現状（問題）

```
SvTYPE(re)
    ↓ Preprocessor (展開される)
((*re).sv_flags & SVTYPEMASK)
    ↓ Parser
Binary { op: BitAnd, ... }
    ↓ RustCodegen
((*re).sv_flags & SVTYPEMASK)
```

### 目標とする動作

```
SvTYPE(re)
    ↓ Preprocessor (wrapped_macros)
MacroBegin { name: "SvTYPE", args: [re] }
((*re).sv_flags & SVTYPEMASK)
MacroEnd
    ↓ Parser
Call { func: "SvTYPE", args: [re] }
    ↓ RustCodegen
SvTYPE(re)
```

---

## 設計選択肢

### Option A: wrapped_macros の拡張（最小変更・推奨）

`wrapped_macros` に保存対象マクロを追加し、Parser で `Call` ノードとして再構築。

**メリット**:
- 既存の `MacroBegin`/`MacroEnd` 機構を再利用
- AST への変更が不要
- Parser の変更が局所的

**デメリット**:
- 展開結果は無視される（Rust 側で同等の関数を呼ぶため問題なし）

### Option B: Preprocessor に preserve_function_macros を適用

inline 関数本体のパース時に `preserve_function_macros = true` を使用。

**メリット**: より一般的な解決策
**デメリット**: Preprocessor の大幅な変更が必要

---

## 採用案: Option A（wrapped_macros 拡張）

---

## 実装計画

### Step 1: 保存対象マクロのリスト定義

**場所**: `src/pipeline.rs` または新規モジュール

```rust
/// 関数呼び出しとして保存するマクロのリスト
pub fn preserved_macro_calls() -> Vec<&'static str> {
    vec![
        // 型取得マクロ
        "SvTYPE",
        "SvFLAGS",
        // 型チェックマクロ
        "isREGEXP",
        "SvROK",
        "SvIOK",
        "SvNOK",
        "SvPOK",
        // アクセサマクロ（SvANY は ExplicitExpandSymbols なので除外）
        "SvPVX",
        "SvIVX",
        "SvNVX",
        "SvCUR",
        "SvLEN",
    ]
}
```

### Step 2: with_codegen_defaults() の拡張

**場所**: `src/pipeline.rs`

```rust
pub fn with_codegen_defaults(mut self) -> Self {
    // 既存: assert 系
    self.preprocess.wrapped_macros = vec![
        "assert".to_string(),
        "assert_".to_string(),
    ];

    // 新規: 関数呼び出しとして保存するマクロを追加
    for name in preserved_macro_calls() {
        self.preprocess.wrapped_macros.push(name.to_string());
    }

    self
}
```

### Step 3: Parser での MacroBegin 処理拡張

**場所**: `src/parser.rs`

現状の処理:
```rust
// MacroBegin { name: "assert", args } → Assert ノード
```

変更後:
```rust
fn handle_macro_begin(&mut self, name: &str, args: Vec<Expr>) -> ParseResult<Expr> {
    if name == "assert" || name == "assert_" {
        // 既存: Assert ノード生成
        let kind = detect_assert_kind(name).unwrap();
        Ok(Expr::new(ExprKind::Assert { kind, condition: Box::new(args[0].clone()) }))
    } else {
        // 新規: それ以外の wrapped_macros は Call として再構築
        let func_id = self.interner.intern(name);
        Ok(Expr::new(ExprKind::Call {
            func: Box::new(Expr::new(ExprKind::Ident(func_id))),
            args,
        }))
    }
}
```

### Step 4: MacroBegin の引数パース確認

`wrapped_macros` のマーカーが正しく引数を保存しているか確認。
現状の assert 処理で動作しているため、同じ機構が使えるはず。

---

## 影響範囲

| ファイル | 変更内容 |
|----------|----------|
| `src/pipeline.rs` | `preserved_macro_calls()` 追加、`with_codegen_defaults()` 拡張 |
| `src/parser.rs` | `MacroBegin` 検出時の分岐追加 |

---

## テスト計画

1. `Perl_ReANY` の生成結果確認
   ```rust
   // 期待される出力
   return (if (SvTYPE(re) == SVt_PVLV) { ... } else { ... });

   // 現状の出力（問題）
   return (if ((((*re).sv_flags & SVTYPEMASK) as svtype) == SVt_PVLV) { ... } else { ... });
   ```

2. `isREGEXP(re)` が assert 内で保存されていることを確認（既存動作）

3. 保存対象外のマクロは従来通り展開されることを確認

---

## 確認事項

実装前に以下を確認:

1. [ ] `wrapped_macros` の引数が複数の場合の動作
2. [ ] ネストしたマクロ呼び出しの場合の動作
3. [ ] Parser での `MacroBegin` 検出箇所の特定

---

## 実装順序

1. [ ] Parser での `MacroBegin` 処理箇所を特定
2. [ ] `preserved_macro_calls()` リストを定義
3. [ ] `with_codegen_defaults()` を拡張
4. [ ] Parser の `MacroBegin` 処理を拡張
5. [ ] テスト実行と出力確認
6. [ ] アーキテクチャ文書の更新
