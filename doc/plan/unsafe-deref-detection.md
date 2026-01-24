# ポインタデリファレンスの unsafe ブロック検出

## 概要

Rust 2024 edition では、`unsafe fn` 内であっても生ポインタのデリファレンスには
`unsafe { }` ブロックが必要。現在の実装は関数呼び出しのみを検出しているため、
ポインタデリファレンスも検出対象に追加する。

## 問題のあるコード例

```rust
/// cxstack_max - macro function
#[inline]
pub unsafe fn cxstack_max(my_perl: *mut PerlInterpreter) -> I32 {
    // warning: dereference of raw pointer is unsafe and requires unsafe block
    (*(*my_perl).Icurstackinfo).si_cxmax
}
```

## 設計方針

### パース時にカウントする理由

1. **効率的** - AST再スキャンが不要
2. **一貫性** - マクロもインライン関数も同じ仕組みでカウント
3. **拡張性** - 各カウントを別々に保持することで将来の解析に活用可能

### カウント対象

1. **ExprKind::Call** - 関数呼び出し → `function_call_count`
2. **ExprKind::Deref** - `*ptr` 形式のデリファレンス → `deref_count`
3. **ExprKind::PtrMember** - `ptr->member` 形式（暗黙のデリファレンス）→ `deref_count`

### フィールド設計

`function_call_count` と `deref_count` は別々のフィールドとして保持する。
これにより、将来の解析（例：複雑度計測、最適化ヒント）にも活用できる。

## 実装計画

### Step 1: Parser に deref_count を追加

**ファイル:** `src/parser.rs`

```rust
pub struct Parser<'a, S: TokenSource> {
    // ... existing fields ...
    pub function_call_count: usize,  // 既存
    pub deref_count: usize,          // 新規追加
}
```

ExprKind::Deref と ExprKind::PtrMember 生成時にインクリメント。

### Step 2: ParseStats に deref_count を追加

**ファイル:** `src/parser.rs`

```rust
#[derive(Debug, Clone, Default)]
pub struct ParseStats {
    pub function_call_count: usize,
    pub deref_count: usize,
}

impl ParseStats {
    /// unsafe 操作を含むか
    pub fn has_unsafe_ops(&self) -> bool {
        self.function_call_count > 0 || self.deref_count > 0
    }
}
```

### Step 3: FunctionDef にカウントフィールドを追加

**ファイル:** `src/ast.rs`

```rust
pub struct FunctionDef {
    pub specs: DeclSpecs,
    pub declarator: Declarator,
    pub body: CompoundStmt,
    pub info: NodeInfo,
    pub comments: Vec<Comment>,
    pub is_target: bool,

    /// 関数本体に含まれる関数呼び出しの数（パース時に検出）
    pub function_call_count: usize,
    /// 関数本体に含まれるポインタデリファレンスの数（パース時に検出）
    pub deref_count: usize,
}
```

### Step 4: Parser で FunctionDef 生成時にカウント記録

**ファイル:** `src/parser.rs`

```rust
if self.check(&TokenKind::LBrace) {
    // 本体パース前のカウントを記録
    let call_count_before = self.function_call_count;
    let deref_count_before = self.deref_count;

    let body = self.parse_compound_stmt()?;

    // 差分が関数本体のカウント
    let function_call_count = self.function_call_count - call_count_before;
    let deref_count = self.deref_count - deref_count_before;

    return Ok(ExternalDecl::FunctionDef(FunctionDef {
        specs,
        declarator,
        body,
        info: NodeInfo::new(loc),
        comments,
        is_target,
        function_call_count,
        deref_count,
    }));
}
```

### Step 5: MacroInferInfo のフィールドを拡張

**ファイル:** `src/macro_infer.rs`

```rust
pub struct MacroInferInfo {
    // ... existing fields ...

    // 変更前: pub has_function_calls: bool,
    // 変更後:
    /// 関数呼び出しの数（パース時に検出）
    pub function_call_count: usize,
    /// ポインタデリファレンスの数（パース時に検出）
    pub deref_count: usize,
}

impl MacroInferInfo {
    /// unsafe 操作を含むか
    pub fn has_unsafe_ops(&self) -> bool {
        self.function_call_count > 0 || self.deref_count > 0
    }
}
```

### Step 6: macro_infer.rs の統計収集を更新

**ファイル:** `src/macro_infer.rs`

```rust
let (parse_result, stats) = self.try_parse_tokens(...);
info.parse_result = parse_result;
info.function_call_count = stats.function_call_count;
info.deref_count = stats.deref_count;
```

### Step 7: rust_codegen.rs の条件を更新

**ファイル:** `src/rust_codegen.rs`

#### マクロ生成

```rust
// 変更前
let needs_unsafe = info.has_function_calls;

// 変更後
let needs_unsafe = info.has_unsafe_ops();
```

#### インライン関数生成

```rust
// 変更前
let needs_unsafe = count_function_calls_in_compound_stmt(&func_def.body) > 0;

// 変更後
let needs_unsafe = func_def.function_call_count > 0 || func_def.deref_count > 0;
```

### Step 8: ast.rs の AST走査関数を削除

**ファイル:** `src/ast.rs`

以下の関数を削除：
- `count_function_calls_in_expr`
- `count_function_calls_in_initializer`
- `count_function_calls_in_stmt`
- `count_function_calls_in_decl`
- `count_function_calls_in_compound_stmt`

## 実装順序

1. **Step 1-2:** Parser に deref_count 追加、ParseStats 拡張
2. **Step 3-4:** FunctionDef にカウント追加、パース時に記録
3. **Step 5-6:** MacroInferInfo 拡張、統計収集更新
4. **Step 7:** rust_codegen.rs の条件更新
5. **Step 8:** ast.rs の不要関数削除
6. **Step 9:** テストと検証

## 変更対象ファイル

| ファイル | 変更内容 |
|----------|----------|
| `src/parser.rs` | `deref_count` 追加、Deref/PtrMember 生成時インクリメント、FunctionDef 生成時カウント記録 |
| `src/ast.rs` | FunctionDef にカウントフィールド追加、AST走査関数削除 |
| `src/macro_infer.rs` | `function_call_count`/`deref_count` フィールド追加、`has_unsafe_ops()` メソッド追加 |
| `src/rust_codegen.rs` | 条件を `has_unsafe_ops()` またはフィールド直接参照に変更 |

## 備考

- `ExprKind::Member` (`.` 演算子) はデリファレンスを含まないのでカウント不要
- 将来的に `ptr.offset()` などの unsafe メソッド呼び出しも検出対象に
  追加することを検討
- 各カウントを分離することで、将来的なコード品質メトリクスにも活用可能
