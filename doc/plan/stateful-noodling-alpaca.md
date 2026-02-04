# Plan: Generalized Macro Call Preservation in AST

## Goal

マクロを常に展開しながら、元のマクロ呼び出し情報を AST に保持する仕組みを一般化する。
これにより：
1. 全ての uses 関係を正確に把握できる
2. コード生成時にオリジナルに近い式を出力できる
3. assert のような特殊変換にも対応できる

## Background

### 現在の問題

`CopFILEAV(c)` のマクロ本体 `GvAV(gv_fetchfile(CopFILE(c)))` において：
- `GvAV` が「保存」（非展開）されると、引数内の `gv_fetchfile` が uses に記録されない
- THX 依存が伝播せず、生成コードでパラメータ不足が発生

### 現在の部分的実装（assert のみ）

```
assert(x) → Preprocessor → MacroBegin(is_wrapped=true) + ((void)0) + MacroEnd
         → Parser → ExprKind::Assert { kind, condition: x }
         → Codegen → assert!((x) != 0)
```

### 理想の設計

- **全ての関数マクロを常に展開**（uses 関係を完全に把握）
- **展開結果と元のマクロ呼び出し情報を両方 AST に保持**
- **コード生成時に選択可能**（マクロ呼び出し形式 or 展開形式）

## Design

### 1. AST に ExprKind::MacroCall を追加

**File**: `src/ast.rs`

```rust
/// マクロ呼び出し（元の呼び出し情報と展開結果を両方保持）
MacroCall {
    /// マクロ名
    name: InternedStr,
    /// 元の引数（パース済み式）
    args: Vec<Expr>,
    /// 展開後の式（型推論・意味解析用）
    expanded: Box<Expr>,
    /// マクロ呼び出し位置
    call_loc: SourceLocation,
},
```

**利点**:
- `expanded` で型推論を実行
- `name` と `args` でコード生成時にマクロ呼び出しを再構築
- ネストされたマクロは `args` 内に MacroCall として自然に表現

### 2. MacroBeginInfo に preserve_call フラグを追加

**File**: `src/token.rs`

```rust
pub struct MacroBeginInfo {
    pub marker_id: TokenId,
    pub trigger_token_id: TokenId,
    pub macro_name: InternedStr,
    pub kind: MacroInvocationKind,  // Function { args } に生トークンが入る
    pub call_loc: SourceLocation,
    pub is_wrapped: bool,           // 既存（assert 用の特殊処理）
    pub preserve_call: bool,        // NEW: コード生成でマクロ呼び出しを保持するか
}
```

### 3. Preprocessor の変更

**File**: `src/preprocessor.rs`

#### 3.1 preserve_function_macros モードの廃止

`expand_tokens_for_inference()` で：
- 全ての関数マクロを**常に展開**する
- MacroBegin/MacroEnd マーカーを**常に出力**する

#### 3.2 preserve_call フラグの設定ロジック

```rust
// トークンペースト（##）を含むマクロは展開形式のみ
let preserve_call = !has_token_pasting && !is_explicit_expand_only(id);
```

- `SvANY`, `SvFLAGS` などの explicit_expand マクロ → `preserve_call = false`
- `##` を含むマクロ → `preserve_call = false`
- その他の関数マクロ → `preserve_call = true`

### 4. Parser の変更

**File**: `src/parser.rs`

#### 4.1 MacroBegin(preserve_call=true) の処理

```rust
fn parse_macro_call_expr(&mut self, begin_info: &MacroBeginInfo) -> Result<Expr> {
    // 1. MacroEnd までの展開トークンをパースして expanded を得る
    let expanded = self.parse_expression()?;

    // 2. begin_info.kind から元の引数トークン列を取得
    let raw_args = match &begin_info.kind {
        MacroInvocationKind::Function { args } => args,
        _ => unreachable!(),
    };

    // 3. 各引数トークン列を式としてパース
    let parsed_args = raw_args.iter()
        .map(|tokens| self.parse_tokens_as_expr(tokens))
        .collect::<Result<Vec<_>>>()?;

    // 4. MacroCall ノードを作成
    Ok(Expr::new(ExprKind::MacroCall {
        name: begin_info.macro_name,
        args: parsed_args,
        expanded: Box::new(expanded),
        call_loc: begin_info.call_loc.clone(),
    }, begin_info.call_loc.clone()))
}
```

#### 4.2 assert の特殊処理との統合

`is_wrapped = true` の場合は既存の `parse_wrapped_macro_expr()` を使用。
`preserve_call = true` かつ `is_wrapped = false` の場合は上記の一般処理。

### 5. Rust Codegen の変更

**File**: `src/rust_codegen.rs`

```rust
fn expr_to_rust(&mut self, expr: &Expr, info: &MacroInferInfo) -> String {
    match &expr.kind {
        ExprKind::MacroCall { name, args, expanded, call_loc } => {
            let name_str = self.interner.get(*name);

            // マクロ呼び出し形式で出力するか判定
            if self.should_emit_as_macro_call(*name) {
                let args_str: Vec<String> = args.iter()
                    .map(|arg| self.expr_to_rust(arg, info))
                    .collect();
                format!("{}({})", escape_rust_keyword(name_str), args_str.join(", "))
            } else {
                // 展開形式で出力
                self.expr_to_rust(expanded, info)
            }
        }
        // ... 既存の処理
    }
}

fn should_emit_as_macro_call(&self, name: InternedStr) -> bool {
    // 以下の場合はマクロ呼び出し形式で出力：
    // - MacroInferContext にマクロ情報があり、生成対象である
    // - または bindings.rs に対応する関数がある
    // - または inline 関数として存在する
    self.is_function_available(name)
}
```

### 6. Macro Inference の変更

**File**: `src/macro_infer.rs`

#### 6.1 uses 収集の修正

`expand_macro_body_for_inference()` が全てのマクロを展開するようになるため：
- `called_macros` が完全な uses 関係を反映
- THX 依存の伝播も正確になる

#### 6.2 MacroCall ノードからの uses 収集

パース後の AST から uses を収集する場合も、MacroCall ノードの `args` と `expanded` 両方を走査。

## Implementation Steps

### Step 1: AST に MacroCall を追加
- `src/ast.rs` に `ExprKind::MacroCall` を追加
- 関連する SexpPrinter 等も更新

### Step 2: MacroBeginInfo を拡張
- `src/token.rs` に `preserve_call` フラグを追加
- デフォルトは `false`（後方互換性）

### Step 3: Preprocessor を修正
- `expand_tokens_for_inference()` で全関数マクロを展開
- `preserve_call` フラグの設定ロジックを追加

### Step 4: Parser を修正
- `preserve_call = true` のマーカー処理を追加
- `parse_macro_call_expr()` を実装

### Step 5: Rust Codegen を修正
- `ExprKind::MacroCall` の処理を追加
- `should_emit_as_macro_call()` の判定ロジックを実装

### Step 6: テストと検証
- 回帰テスト（既存の 5 関数）が通ることを確認
- CopFILEAV, CopFILESV が `[THX]` マーカー付きで生成されることを確認

## Files to Modify

| File | Changes |
|------|---------|
| `src/ast.rs` | `ExprKind::MacroCall` variant 追加 |
| `src/token.rs` | `MacroBeginInfo` に `preserve_call` フラグ追加 |
| `src/preprocessor.rs` | 全関数マクロを展開、マーカー出力、preserve_call 設定 |
| `src/parser.rs` | `parse_macro_call_expr()` 追加、MacroBegin 処理分岐 |
| `src/rust_codegen.rs` | `MacroCall` の expr_to_rust 実装 |
| `src/macro_infer.rs` | uses 収集の MacroCall 対応 |
| `src/sexp.rs` | MacroCall の S 式出力（デバッグ用） |

## Verification

1. `cargo build` - ビルド成功
2. `cargo test` - 全テスト通過
3. 回帰テスト:
   ```bash
   cargo test rust_codegen_regression
   ```
4. CopFILEAV の THX 検出:
   ```bash
   cargo run -- --auto --gen-rust samples/xs-wrapper.h --bindings samples/bindings.rs 2>/dev/null | grep -B2 'CopFILEAV'
   ```
   期待出力: `/// CopFILEAV [THX] - macro function`

## Edge Cases

1. **ネストされたマクロ呼び出し**: `FOO(BAR(x))` → MacroCall の args に MacroCall が入る
2. **トークンペースト**: `CONCAT(a, b)` → `preserve_call = false`、展開形式のみ
3. **explicit_expand マクロ**: `SvANY`, `SvFLAGS` → `preserve_call = false`、インライン展開
4. **assert 系**: 既存の `is_wrapped` 処理を維持（後方互換）

## Notes

- この変更により `preserve_function_macros` モードは不要になる
- `explicit_expand_macros` の役割は「インライン展開すべきマクロ」の指定に変わる
- 段階的な移行が可能（まず preserve_call=false をデフォルトにして動作確認）
