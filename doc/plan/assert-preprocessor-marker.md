# Assert マクロの Preprocessor マーカー方式

## 背景・問題

### 現状
inline 関数内の `assert()` 呼び出しは、プリプロセス時点で `((void)0)` に展開されてしまう：

```c
// NDEBUG が定義されている場合（Perl のデフォルト）
assert(sv);           // → ((void)0)
assert(x > 0);        // → ((void)0)
```

そのため、パーサーには `Cast { type: void, expr: 0 }` として届き、
元の condition が失われる。

### マクロとの違い
- **マクロ**: マクロ本体内の assert は展開されずに保存される → `convert_assert_calls` で変換可能
- **Inline 関数**: プリプロセス済みコードとして読み込まれる → assert は既に展開済み

## 提案: wrap_with_markers の活用

### 既存インフラ

```rust
// preprocessor.rs
fn wrap_with_markers(
    &self,
    tokens: Vec<Token>,           // 展開後のトークン
    macro_name: InternedStr,
    trigger_token: &Token,
    kind: MacroInvocationKind,    // ← args が保存されている！
    call_loc: &SourceLocation,
) -> Vec<Token> {
    if !self.config.emit_markers {
        return tokens;  // 現状: マーカーなしで返す
    }
    // MacroBegin/MacroEnd で囲む
}
```

重要な発見: `MacroInvocationKind::Function { args }` に**元の引数トークン**が保存されている。

### 提案の仕組み

1. **Assert マクロ辞書**: `assert`, `assert_`, `__ASSERT_` などを登録
2. **選択的マーカー**: 辞書に登録されたマクロのみマーカーで囲む
3. **パーサー変換**: `MacroBegin(assert)...MacroEnd` を `Assert` AST ノードに変換

### 処理フロー

```
プリプロセッサ:
  assert(condition)
    ↓ マクロ展開
  ((void)0)
    ↓ assert辞書にマッチ → マーカーで囲む
  MacroBegin(assert, args=[condition_tokens]) ((void)0) MacroEnd

パーサー:
  MacroBegin(assert, args=[condition_tokens]) を検出
    ↓ args からconditionをパース
  Assert { kind: Assert, condition: Expr }
    ↓ MacroEnd まで展開結果をスキップ
```

## 考察

### 利点

1. **元の condition を保持**: `MacroInvocationKind::Function { args }` から復元可能
2. **既存インフラ活用**: `wrap_with_markers` は既に実装済み
3. **選択的適用**: assert 系マクロのみにマーカーを適用
4. **マクロ展開に影響なし**: 通常の展開処理は変わらない

### 課題・検討事項

1. **パーサーの変更**:
   - `MacroBegin` トークンの認識
   - args からの式パース
   - 展開結果（`((void)0)`）のスキップ

2. **マーカー情報の拡張**:
   - 現在の `MacroBeginInfo` に `is_assert` フラグを追加するか
   - または assert 専用のマーカー型を新設するか

3. **入れ子の扱い**:
   - `assert(assert_(...))` のような入れ子ケース
   - 展開結果内に別の assert が含まれる場合

4. **emit_markers 設定との関係**:
   - 現在は `emit_markers: true` でのみマーカー出力
   - assert 用に別フラグを設けるか、辞書マッチで自動有効化するか

### 代替案との比較

| 方式 | 長所 | 短所 |
|------|------|------|
| **マーカー方式（提案）** | 既存インフラ活用、選択的 | パーサー変更必要 |
| DEBUGGING 定義 | シンプル | 他の副作用あり |
| `((void)0)` パターン検出 | パーサー変更不要 | 脆弱、condition 復元不可 |
| assert マクロ再定義 | 標準 assert をオーバーライド | 影響範囲が広い |

## 実装方針

### Phase 1: データ構造の追加

**`src/preprocessor.rs`**:
```rust
pub struct Preprocessor {
    // ...
    /// マーカーで囲むマクロの辞書
    wrapped_macros: HashSet<InternedStr>,
}

impl Preprocessor {
    pub fn add_wrapped_macro(&mut self, macro_name: &str) {
        let id = self.interner.intern(macro_name);
        self.wrapped_macros.insert(id);
    }
}
```

**`src/token.rs`** (`MacroBeginInfo` 拡張):
```rust
pub struct MacroBeginInfo {
    // 既存フィールド...
    /// wrap 対象マクロの場合 true
    pub is_wrapped: bool,
}
```

### Phase 2: プリプロセッサの拡張

`wrap_with_markers` を拡張:

```rust
fn wrap_with_markers(...) -> Vec<Token> {
    let is_wrapped = self.wrapped_macros.contains(&macro_name);

    // emit_markers が off でも、wrapped_macros に含まれていればマーカー出力
    if !self.config.emit_markers && !is_wrapped {
        return tokens;
    }

    let begin_info = MacroBeginInfo {
        // ...
        is_wrapped,
    };
    // ...
}
```

### Phase 3: パーサーの拡張

**式パース時に `MacroBegin` を処理**:

```rust
fn parse_primary_expr(&mut self) -> Result<Expr, ParseError> {
    match &self.current().kind {
        TokenKind::MacroBegin(info) if info.is_wrapped => {
            self.parse_wrapped_macro_expr(info)
        }
        // 他のケース...
    }
}

fn parse_wrapped_macro_expr(&mut self, info: &MacroBeginInfo) -> Result<Expr, ParseError> {
    let marker_id = info.marker_id;
    let macro_name = info.macro_name;

    // 1. args から condition をパース
    let args = match &info.kind {
        MacroInvocationKind::Function { args } => args,
        _ => return Err(...),
    };
    if args.len() != 1 {
        return Err(...);
    }
    let condition = self.parse_expr_from_tokens(&args[0])?;

    // 2. 入れ子チェック（condition 内に別の wrapped マクロがあればエラー）
    // → parse_expr_from_tokens 内でチェック

    // 3. Assert ノードを作成
    let kind = detect_assert_kind(&self.interner.get(macro_name));
    let assert_expr = Expr::new(ExprKind::Assert {
        kind: kind.unwrap_or(AssertKind::Assert),
        condition: Box::new(condition),
    });

    // 4. MacroEnd までスキップ
    self.advance(); // MacroBegin を消費
    self.skip_to_macro_end(marker_id)?;

    Ok(assert_expr)
}
```

### Phase 4: 呼び出し側での登録

**`src/infer_api.rs`** または使用箇所:
```rust
let mut pp = Preprocessor::new(...);
pp.add_wrapped_macro("assert");
pp.add_wrapped_macro("assert_");
pp.add_wrapped_macro("__ASSERT_");
```

## 設計決定事項

### 1. assert マクロ辞書の管理

`add_wrapped_macro(macro_name: String)` メソッドで動的に追加可能にする：

```rust
impl Preprocessor {
    /// マーカーで囲むマクロを登録
    pub fn add_wrapped_macro(&mut self, macro_name: &str) {
        let id = self.interner.intern(macro_name);
        self.wrapped_macros.insert(id);
    }
}
```

呼び出し側で必要なマクロを登録：
```rust
pp.add_wrapped_macro("assert");
pp.add_wrapped_macro("assert_");
pp.add_wrapped_macro("__ASSERT_");
```

### 2. 末尾カンマ形式の対応

`assert_` や `__ASSERT_` は末尾カンマを生成する形式。
マーカー情報に `AssertKind` 相当の情報を含めて保持し、
Rust コード生成時に `{ assert!(...); }` 形式で出力：

- `assert(cond)` → `assert!(cond)`
- `assert_(cond)` → `{ assert!(cond); }`
- `__ASSERT_(cond)` → `{ assert!(cond); }`

### 3. 入れ子の検出とエラー

assert マーカー処理中に別の assert マーカーを検出した場合はエラー：

```rust
// パーサーでの検出
if parsing_assert_marker && matches!(token.kind, TokenKind::MacroBegin(info) if info.is_assert) {
    return Err(ParseError::NestedAssertNotSupported { loc: token.loc.clone() });
}
```

## 変更対象ファイル

| ファイル | 変更内容 |
|----------|----------|
| `src/preprocessor.rs` | `wrapped_macros` フィールド追加、`add_wrapped_macro()` メソッド、`wrap_with_markers` 拡張 |
| `src/token.rs` | `MacroBeginInfo` に `is_wrapped` フィールド追加 |
| `src/parser.rs` | `parse_primary_expr` で `MacroBegin` 処理、`parse_wrapped_macro_expr` 新規、`skip_to_macro_end` 新規 |
| `src/error.rs` | `NestedAssertNotSupported` エラー追加 |
| `src/infer_api.rs` | `add_wrapped_macro` 呼び出し追加 |
| `src/lib.rs` | 必要に応じて再エクスポート |
