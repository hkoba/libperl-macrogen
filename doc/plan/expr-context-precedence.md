# Plan: ExprContext の演算子優先順位ベース拡張

## Rust の演算子優先順位（高い順）

| 優先順位 | 演算子 |
|----------|--------|
| 最高 | パス、メソッド呼び出し、フィールド、関数呼び出し、インデックス |
| 高 | `?` |
| 高 | 単項: `-` `*` `!` `&` |
| **中高** | **`as`** |
| 中 | `*` `/` `%` |
| 中 | `+` `-` |
| 中低 | `<<` `>>` |
| 低 | `&` |
| 低 | `^` |
| 低 | `\|` |
| 低 | `==` `!=` `<` `>` `<=` `>=` |
| 最低 | `&&` |
| 最低 | `\|\|` |
| 最低 | `=` `+=` 等 |

**重要**: `as` は**単項より低く、全ての二項演算子より高い**。

## 括弧が不要なケース

### Cast 式 `(expr as T)`

| コンテキスト | 必要性 | 理由 |
|-------------|--------|------|
| 関数引数 `f(expr as T)` | 不要 | `,` で区切り |
| let RHS `let x = expr as T;` | 不要 | `;` で終端 |
| return `return expr as T;` | 不要 | `;` で終端 |
| 代入 RHS `x = expr as T;` | 不要 | `;` で終端 |
| Binary の RHS `a + expr as T` | 不要 | `as` > `+` |
| Binary の LHS `expr as T + a` | **不要** | `as` > `+`, `(expr as T) + a` と同じ |
| Deref `*(expr as T)` | **必要** | `*` > `as`, `*expr as T` は `(*expr) as T` |
| メソッド `(expr as T).method()` | **必要** | `.` > `as` |
| Cast 内 `(expr as T1) as T2` | 不要 | `as` は左結合 |

### Binary 式 `(a op b)`

| コンテキスト | 必要性 | 理由 |
|-------------|--------|------|
| 関数引数 `f(a + b)` | 不要 | `,` で区切り |
| let RHS `let x = a + b;` | 不要 | `;` で終端 |
| return `return a + b;` | 不要 | `;` で終端 |
| Cast 内 `(a + b) as T` | **必要** | `as` > すべての二項 |
| 高優先 Binary の中 `(a + b) * c` | **必要** | `*` > `+` |
| 低優先 Binary の中 `a * b + c` → `a * b` 部分 | 不要 | `*` > `+` |

## 残り 120 件 warning の分類と対処

### function argument (20件)

**原因**: `cast_integer_arg_if_needed` が `format!("({} as {})")` で常に括弧。

**対処**: `cast_integer_arg_if_needed` は引数コンテキスト（Top）で呼ばれるため、
括弧なしの `format!("{} as {}")` で良い。
ただし内部の `arg_str` 自体が Binary 式の場合は括弧が必要（`as` > 全二項演算子）。

→ `arg_str` が既に `(...)` で括弧付きなら OK。括弧なしの Binary 結果が来たら危険。
しかし現状 Binary は常に括弧付きなので問題ない。

**修正**: `cast_integer_arg_if_needed` の `format!("({} as {})")` を
`format!("{} as {}")` に変更（引数コンテキストのみ）。

### assigned value (45件)

**原因**: 代入 RHS の内側にある Binary/Cast の括弧。
`strip_outer_parens` で最外は除去されるが、内部の括弧は残る。

例: `(*dest) = (((byte as U8) >> 6) | ((!(255 >> 2)) as u8))`
- 外側は strip 済み
- `(byte as U8)` — Cast、Binary `>>` のオペランドなので括弧必要（`as` > `>>`）
- `((!(255 >> 2)) as u8)` — Cast の外側括弧は不要（代入 RHS は Top に準ずる）

**対処**: Assign の RHS 内の Binary/Cast オペランドにも Top 伝播。
しかし `strip_outer_parens` は文字列ベースで内部構造を知らない。
**ExprContext を Binary/Cast の中まで伝播**する必要がある。

### block return value (49件)

**原因**: Conditional の then/else 分岐内の式。
`(OPpLVAL_INTRO | OPpENTERSUB_INARGS)` — Binary が括弧付き。

**対処**: Conditional の分岐は Top コンテキスト（`expr_with_type_hint` が Top で呼ぶ）
のはず。Binary が Top でも括弧を維持しているのは前回の断念によるもの。

**Binary の Top 括弧除去が安全なケース**: Binary の結果が Cast の内部以外で使われる場合。
Cast 内部では `as` > 全二項なので常に括弧必要。

## 外側コンテキスト参照の検討

### chain が有用なケース

`f((a | b) as u32)` の生成:
1. `f(...)` → 引数は Top
2. `(a | b) as u32` → Cast で Top → 外側括弧なし → `a | b as u32` **WRONG**

問題: Cast が Top だが、Cast の**内部**の Binary は `as` の中なので括弧必要。
Cast の ctx を Top にすると外側括弧は除去されるが、
内部に Binary がある場合は「外側が Cast」という情報が必要。

chain アプローチ:
```rust
// Cast の内部: 自分は Top だが、親が Cast
ExprContext { kind: Top, parent: Some(&ExprContext { kind: AsCast, .. }) }
```

しかしこれは不要。Cast の内部は常に Default（Binary に括弧を維持させる）で良い。
Cast の外側のみ Top で括弧を除去。これは現在の実装で既に正しい。

### chain が不要な理由

括弧の要否は**直接の親の演算子優先順位**のみで決まる:
- 子が Binary、親が Cast 内部 → 括弧必要（`as` > 全二項）
- 子が Binary、親が Top → 括弧不要
- 子が Cast、親が Deref → 括弧必要（`*` > `as`）
- 子が Cast、親が Top → 括弧不要

祖父母以上の情報は不要。**chain は現時点では不要**。

## 修正計画

### ExprContext の拡張

```rust
#[derive(Clone, Copy, PartialEq, Eq)]
enum ExprContext {
    /// 括弧不要のトップレベル位置
    Top,
    /// Cast (as) の内側: 全ての Binary に括弧必要
    CastInner,
    /// Deref/AddrOf の内側: Cast に括弧必要
    UnaryInner,
    /// Binary の内側: 子の優先順位次第
    BinaryInner { parent_prec: u8 },
}
```

各演算子の優先順位:
```rust
fn binop_precedence(op: BinOp) -> u8 {
    match op {
        BinOp::Mul | BinOp::Div | BinOp::Mod => 10,
        BinOp::Add | BinOp::Sub => 9,
        BinOp::Shl | BinOp::Shr => 8,
        BinOp::BitAnd => 7,
        BinOp::BitXor => 6,
        BinOp::BitOr => 5,
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => 4,
        BinOp::LogAnd => 3,
        BinOp::LogOr => 2,
    }
}
```

### 括弧判定ロジック

```rust
fn needs_parens_for_binary(op: BinOp, ctx: ExprContext) -> bool {
    match ctx {
        ExprContext::Top => false,
        ExprContext::CastInner => true,  // as > all binary
        ExprContext::UnaryInner => true,  // unary > all binary
        ExprContext::BinaryInner { parent_prec } => {
            binop_precedence(op) < parent_prec
        }
    }
}

fn needs_parens_for_cast(ctx: ExprContext) -> bool {
    match ctx {
        ExprContext::Top => false,
        ExprContext::CastInner => false,  // as is left-assoc
        ExprContext::UnaryInner => true,  // * > as
        ExprContext::BinaryInner { .. } => false,  // as > all binary
    }
}
```

### 実装手順

1. ExprContext を 4 値に拡張
2. `binop_precedence` 関数を追加
3. Cast 式: 外側括弧を `needs_parens_for_cast(ctx)` で判定、内部は `CastInner`
4. Binary 式: 外側括弧を `needs_parens_for_binary(op, ctx)` で判定、
   内部は `BinaryInner { parent_prec: binop_precedence(op) }`
5. Deref/AddrOf: 内部を `UnaryInner` で呼ぶ
6. `cast_integer_arg_if_needed` のキャストも括弧なしに（引数は Top）
7. `strip_outer_parens` の大部分を不要にする

### 期待効果

残り 120 件の warning の大部分（100件以上）を安全に除去。
