# Plan: 不要な括弧の除去 (1114件の warning)

## 概要

生成コードに 1114 件の `unnecessary parentheses` warning がある。
これは人間にとってノイズとなるため除去する。

## warning の分類

| カテゴリ | 件数 | 例 |
|----------|------|-----|
| assigned value | 363 | `let x = (expr as ty);` |
| block return value | 344 | `{ ((*sv).field) }` |
| function argument | 274 | `assert!((expr))` / `f((x))` |
| if condition | 96 | `if ((cond)) { }` |
| return value | 25 | `return (expr);` |
| while condition | 11 | `while ((cond)) { }` |
| method argument | 1 | `.offset((x))` |

## 原因

C のマクロ定義は防御的に括弧を多用する:
```c
#define SvFLAGS(sv) ((sv)->sv_flags)
#define SvTYPE(sv)  ((SvFLAGS(sv) & SVTYPEMASK))
```

codegen は C の AST をそのまま Rust に変換するため、
C の防御的括弧が Rust コードにも残る。

具体的には `expr_to_rust` / `expr_to_rust_inline` の以下のパターン:

1. **Binary 式**: `format!("({} {} {})", l, op, r)` — 常に `(lhs op rhs)` に括弧
2. **Cast 式**: `format!("({} as {})", expr, ty)` — 常に `(expr as ty)` に括弧
3. **Conditional 式**: `format!("(if {} {{ {} }} else {{ {} }})")` — 常に括弧

## 修正方針

### アプローチ: 文字列後処理で外側の不要な括弧を除去

生成された Rust コード文字列に対して、特定のコンテキストで
外側の括弧を除去する後処理を行う。

```rust
/// 式文字列の外側の不要な括弧を除去する
fn strip_outer_parens(s: &str) -> &str {
    // "(expr)" → "expr" （対応する閉じ括弧がある場合のみ）
    let s = s.trim();
    if s.starts_with('(') && s.ends_with(')') {
        // 対応チェック: 最初の '(' と最後の ')' が対応するか
        if matching_paren(s) {
            return &s[1..s.len()-1];
        }
    }
    s
}
```

適用箇所:

| コンテキスト | 適用箇所 | 変換例 |
|-------------|---------|--------|
| let 初期化子 | `decl_to_rust_let` の init_expr | `let x = (y as T)` → `let x = y as T` |
| block return | `expr_to_rust` の Expression return | `{ (expr) }` → `{ expr }` |
| 関数引数 | `expr_to_rust_arg` の結果 | `f((x))` → `f(x)` |
| if 条件 | `wrap_as_bool_condition` の結果 | `if ((x) != 0)` → `if (x) != 0` |
| return | `stmt_to_rust` の Return | `return (expr)` → `return expr` |
| while 条件 | while の cond | `while ((x)) { }` → `while (x) { }` |
| assert 引数 | assert ハンドラ | `assert!((expr))` → `assert!(expr)` |

### 注意点

- **安全性**: 括弧の除去は出力の意味を変えてはならない。
  `(a + b) * c` の外側括弧は除去不可。
  しかし **最外レベル** の括弧は常に安全に除去できる
  （代入の RHS、return の値、関数引数、if/while の条件）。

- **対応チェック**: `(a + b) * (c + d)` のような文字列で
  先頭の `(` と末尾の `)` が**非対応**の場合は除去しない。

- **Cast の括弧**: `(expr as ty)` は Rust の `as` が低優先なため
  括弧が必要な場合がある。ただし最外レベルでは不要。

### 段階的実装

**Phase 1: `strip_outer_parens` ヘルパー関数を追加**

**Phase 2: 各コンテキストで適用**
- `decl_to_rust_let`: init_expr に適用
- `stmt_to_rust`: return 値に適用
- `stmt_to_rust_inline`: 同上
- `generate_macro` の Expression body: 最外式に適用
- assert ハンドラ: 条件式に適用
- if/while 条件: `wrap_as_bool_condition` 結果に適用

**Phase 3: 内側の不要括弧の除去（オプション）**
- Binary 式の `format!("({} {} {})") ` で、
  最外レベルでない場合のみ括弧を付ける制御
  → 影響が大きいため段階的に

## 期待効果

1114 件の warning の大部分（80-90%）を除去。
