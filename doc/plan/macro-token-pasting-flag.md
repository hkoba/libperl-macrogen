# MacroDef へのトークン連結フラグ追加

## 目標

`MacroDef` にフィールドとして `has_token_pasting: bool` を持たせ、
Preprocessor でマクロ定義時に設定する。

## 背景

### 問題

`CALL_BLOCK_HOOKS` のようなマクロは `STMT_START { ... } STMT_END` 形式だが、
内部で `BhkENTRY(hk, which)` を呼び出しており、このマクロは以下のように定義されている:

```c
#define BhkENTRY(hk, which) \
    ((BhkFLAGS(hk) & BHKf_ ## which) ? ((hk)->which) : NULL)
```

トークン連結 (`##`) を含むマクロは Rust 関数に変換できないため、
そのようなマクロを識別してマーキングする必要がある。

### 設計決定

- **方法 B（フィールド）を採用**: `MacroDef` にフラグを持たせる
- **理由**: 後段で必ず必要になる情報なので、遅延評価のメリットがない
- Preprocessor でマクロ定義時に一度だけ計算し、フィールドに格納

## 実装手順

### Step 1: MacroDef にフィールド追加

**src/macro_def.rs**:

```rust
pub struct MacroDef {
    pub name: InternedStr,
    pub kind: MacroKind,
    pub body: Vec<Token>,
    pub def_loc: SourceLocation,
    pub leading_comments: Vec<String>,
    pub is_builtin: bool,
    pub is_target: bool,
    /// マクロ本体にトークン連結 (##) を含むか
    pub has_token_pasting: bool,
}
```

### Step 2: Preprocessor でフラグ設定

**src/preprocessor.rs** のマクロ定義処理部分:

```rust
let has_token_pasting = body.iter()
    .any(|t| matches!(t.kind, TokenKind::HashHash));

let def = MacroDef {
    name,
    kind,
    body,
    def_loc,
    leading_comments,
    is_builtin: false,
    is_target,
    has_token_pasting,
};
```

### Step 3: main.rs で出力時にマーク表示

**src/main.rs** の `--infer-macro-types` 出力部分:

```rust
let has_pasting = macro_table.get(*name)
    .is_some_and(|def| def.has_token_pasting);

// [THX] と同様に [##] を表示
let markers = format!("{}{}",
    if info.is_thx_dependent { " [THX]" } else { "" },
    if has_pasting { " [##]" } else { "" },
);
```

### Step 4: テストコードの更新

`MacroDef` を直接作成しているテストコードに `has_token_pasting` フィールドを追加。

## 修正対象ファイル

1. **src/macro_def.rs**
   - `MacroDef` に `has_token_pasting: bool` フィールド追加

2. **src/preprocessor.rs**
   - マクロ定義作成時にフラグを計算・設定

3. **src/main.rs**
   - 出力時に `[##]` マーク表示

4. **テストコード** (`src/token_expander.rs` 等)
   - `MacroDef` を直接作成している箇所でフィールド追加

## 期待される結果

変更前:
```
CALL_BLOCK_HOOKS: unparseable (0 constraints, 4 uses) [THX]
```

変更後:
```
CALL_BLOCK_HOOKS: unparseable (0 constraints, 4 uses) [THX] [##]
```

## 注意点

- `##` は展開後ではなく、`MacroDef.body`（元のトークン列）をチェック
- builtin マクロも含めて一律でチェック
