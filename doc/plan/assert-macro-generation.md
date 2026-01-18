# assert 系マクロの Rust assert! への変換

## 現状

### AST 構造

```rust
// src/ast.rs
pub enum ExprKind {
    // ...
    Assert {
        kind: AssertKind,
        condition: Box<Expr>,
    },
}

pub enum AssertKind {
    /// assert(condition)
    Assert,
    /// assert_(condition) - 末尾カンマ付き
    AssertUnderscore,
}
```

### 問題

現在の `RustCodegen` では `ExprKind::Assert` が処理されておらず、
`_ => self.todo_marker(...)` にフォールバックしている。

```rust
// 現在の出力例（TYPE_INCOMPLETE として扱われる）
// [CODEGEN_INCOMPLETE] some_macro - macro function
// ...
//       (assert ...)  // <- 処理されていない
```

## C と Rust の assert の違い

| 項目 | C の assert | Rust の assert! |
|------|-------------|-----------------|
| 条件 | 整数値（0 = false） | bool |
| 失敗時 | abort() | panic!() |
| 式として | assert_ はコンマ演算子 | {} ブロックで式化可能 |

## 変換ルール

### 1. assert(condition)

```c
// C
assert(ptr != NULL);
assert(x > 0);
```

```rust
// Rust
assert!((ptr != NULL) != 0);  // または assert!(ptr != NULL);
assert!((x > 0) != 0);        // または assert!(x > 0);
```

**方針**: 条件式の種類によって変換を最適化

- 比較演算子 (`==`, `!=`, `<`, `>`, `<=`, `>=`) の結果 → そのまま `assert!(cond)`
- 論理演算子 (`&&`, `||`, `!`) の結果 → そのまま `assert!(cond)`
- その他（ポインタ、整数値など） → `assert!((cond) != 0)`

### 2. assert_(condition)

```c
// C: assert_ はコンマ演算子として使用
#define __ASSERT_(statement)  assert(statement),
#define CvPADLIST(sv)  (*(assert_(!CvISXSUB((CV*)(sv))) &CvPADLIST_...))
```

```rust
// Rust: ブロック式で assert! を実行し、unit を返す
{ assert!(cond); }
```

**方針**: `assert_` は式として使用されるため、`{ assert!(cond); }` としてブロック式に

## 既存実装の活用

### convert_assert_calls について

`MacroInferContext::convert_assert_calls` と `convert_assert_calls_in_stmt` は、
マクロパース時に `Call { func: Ident("assert"), args }` を
`ExprKind::Assert { kind, condition }` に変換している。

```rust
// macro_infer.rs での変換（既に実装済み）
fn convert_assert_calls(expr: &mut Expr) {
    // Call { func: Ident("assert"), args } を
    // Assert { kind: AssertKind, condition } に変換
}
```

**結論**: AST レベルの変換は既に完了しており、
コード生成時に `ExprKind::Assert` を処理するだけでよい。

## 実装計画

### Phase 1: RustCodegen::expr_to_rust への Assert 処理追加

`RustCodegen::expr_to_rust` に `ExprKind::Assert` のケースを追加：

```rust
ExprKind::Assert { kind, condition } => {
    let cond = self.expr_to_rust(condition, info);
    let assert_expr = if self.is_boolean_expr(condition) {
        format!("assert!({})", cond)
    } else {
        format!("assert!(({}) != 0)", cond)
    };

    match kind {
        AssertKind::Assert => assert_expr,
        AssertKind::AssertUnderscore => format!("{{ {}; }}", assert_expr),
    }
}
```

### Phase 2: RustCodegen::expr_to_rust_inline への Assert 処理追加

`RustCodegen::expr_to_rust_inline` にも同様の処理を追加。

### Phase 3: is_boolean_expr ヘルパー追加（オプション）

比較演算子や論理演算子の結果かどうかを判定するヘルパー：

```rust
fn is_boolean_expr(&self, expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Binary { op, .. } => matches!(op,
            BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge |
            BinOp::Eq | BinOp::Ne | BinOp::LogAnd | BinOp::LogOr
        ),
        ExprKind::LogNot(_) => true,
        _ => false,
    }
}
```

**簡略化オプション**: 常に `assert!(({cond}) != 0)` とすることも可能。
Rust コンパイラが最適化するため、実行時のオーバーヘッドはない。

## 変更対象ファイル

- `src/rust_codegen.rs`:
  - `RustCodegen::expr_to_rust` に Assert ケース追加
  - `RustCodegen::expr_to_rust_inline` に Assert ケース追加
  - `CodegenDriver::expr_to_rust_inline` に Assert ケース追加
  - `is_boolean_expr_kind` フリー関数追加

## テスト計画

1. `assert(x > 0)` → `assert!((x > 0))`
2. `assert(ptr)` → `assert!((ptr) != 0)`
3. `assert_(condition)` → `{ assert!((condition) != 0); }`
4. 生成されたコードで CODEGEN_INCOMPLETE が減少することを確認

## 実装完了

以下の変更を実装:

1. **`is_boolean_expr_kind` フリー関数**:
   - 比較演算子 (`<`, `>`, `<=`, `>=`, `==`, `!=`) と論理演算子 (`&&`, `||`) の結果は bool
   - `LogNot` は含めない（現在の変換は int を返すため）

2. **`RustCodegen::expr_to_rust` への Assert ケース追加**:
   ```rust
   ExprKind::Assert { kind, condition } => {
       let cond = self.expr_to_rust(condition, info);
       let assert_expr = if self.is_boolean_expr(condition) {
           format!("assert!({})", cond)
       } else {
           format!("assert!(({}) != 0)", cond)
       };
       match kind {
           AssertKind::Assert => assert_expr,
           AssertKind::AssertUnderscore => format!("{{ {}; }}", assert_expr),
       }
   }
   ```

3. **同様のケースを以下にも追加**:
   - `RustCodegen::expr_to_rust_inline`
   - `CodegenDriver::expr_to_rust_inline`

## 生成例

```rust
// assert(my_perl)
assert!((my_perl) != 0)

// assert(e > s)
assert!((e > s))

// assert_(!CvISXSUB(cv))
{ assert!(((if CvISXSUB(cv) { 0 } else { 1 })) != 0); }
```
