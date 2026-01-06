# マクロ展開追跡システム - 統合設計 v3

## 概要

3つの機能を統合して実装する:

1. **TokenID ベースの展開禁止追跡**: `no_expand` フィールドを Token から分離
2. **マクロ展開マーカー**: 展開結果を `MacroBegin`/`MacroEnd` で囲む
3. **全ASTノードへのマクロ情報埋め込み**: `NodeInfo` 構造体による統一的なメタデータ管理

## Part 1: Token レイヤーの変更

### 1.1 TokenId

```rust
/// トークンID（一意の通し番号）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct TokenId(pub u64);

impl TokenId {
    pub fn next() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(1);  // 0 は無効値として予約
        Self(COUNTER.fetch_add(1, Ordering::Relaxed))
    }

    pub const INVALID: Self = Self(0);

    pub fn is_valid(&self) -> bool {
        self.0 != 0
    }
}
```

### 1.2 マーカートークン

```rust
/// マクロ展開の開始マーカー情報
#[derive(Debug, Clone)]
pub struct MacroBeginInfo {
    /// このマーカーのID（MacroEnd との対応付け用）
    pub marker_id: TokenId,
    /// 展開を引き起こしたトークンのID
    pub trigger_token_id: TokenId,
    /// マクロ名
    pub macro_name: InternedStr,
    /// マクロの種類と引数
    pub kind: MacroInvocationKind,
    /// 展開が発生した位置
    pub call_loc: SourceLocation,
}

/// マクロ呼び出しの種類
#[derive(Debug, Clone)]
pub enum MacroInvocationKind {
    /// オブジェクトマクロ
    Object,
    /// 関数マクロ
    Function {
        /// 引数のトークン列（展開前）
        args: Vec<Vec<Token>>,
    },
}

/// マクロ展開の終了マーカー情報
#[derive(Debug, Clone)]
pub struct MacroEndInfo {
    /// 対応する MacroBegin のマーカーID
    pub begin_marker_id: TokenId,
}

/// TokenKind への追加
pub enum TokenKind {
    // ... 既存バリアント ...

    /// マクロ展開開始マーカー
    MacroBegin(Box<MacroBeginInfo>),
    /// マクロ展開終了マーカー
    MacroEnd(MacroEndInfo),
}
```

### 1.3 Token 構造体

```rust
pub struct Token {
    pub id: TokenId,
    pub kind: TokenKind,
    pub loc: SourceLocation,
    pub leading_comments: Vec<Comment>,
    // no_expand は削除
}

impl Token {
    pub fn new(kind: TokenKind, loc: SourceLocation) -> Self {
        Self {
            id: TokenId::next(),
            kind,
            loc,
            leading_comments: Vec::new(),
        }
    }

    pub fn clone_with_new_id(&self) -> Self {
        Self {
            id: TokenId::next(),
            kind: self.kind.clone(),
            loc: self.loc.clone(),
            leading_comments: self.leading_comments.clone(),
        }
    }
}
```

### 1.4 展開禁止レジストリ

```rust
/// 展開禁止情報の管理
pub struct NoExpandRegistry {
    map: HashMap<TokenId, HashSet<InternedStr>>,
}

impl NoExpandRegistry {
    pub fn new() -> Self { Self { map: HashMap::new() } }

    pub fn add(&mut self, token_id: TokenId, macro_id: InternedStr) {
        self.map.entry(token_id).or_default().insert(macro_id);
    }

    pub fn extend(&mut self, token_id: TokenId, macros: impl IntoIterator<Item = InternedStr>) {
        self.map.entry(token_id).or_default().extend(macros);
    }

    pub fn is_blocked(&self, token_id: TokenId, macro_id: InternedStr) -> bool {
        self.map.get(&token_id).map_or(false, |s| s.contains(&macro_id))
    }

    pub fn inherit(&mut self, from: TokenId, to: TokenId) {
        if let Some(set) = self.map.get(&from).cloned() {
            self.map.entry(to).or_default().extend(set);
        }
    }
}
```

## Part 2: AST レイヤーの変更

### 2.1 NodeInfo - 全ASTノード共通のメタデータ

```rust
/// ASTノードの共通メタデータ
#[derive(Debug, Clone, Default)]
pub struct NodeInfo {
    /// ソース位置
    pub loc: SourceLocation,
    /// マクロ展開情報（マクロ展開由来の場合のみ Some）
    pub macro_expansion: Option<Box<MacroExpansionInfo>>,
}

impl NodeInfo {
    pub fn new(loc: SourceLocation) -> Self {
        Self {
            loc,
            macro_expansion: None,
        }
    }

    pub fn with_macro_info(loc: SourceLocation, macro_info: MacroExpansionInfo) -> Self {
        Self {
            loc,
            macro_expansion: if macro_info.is_empty() {
                None
            } else {
                Some(Box::new(macro_info))
            },
        }
    }
}

/// マクロ展開の履歴情報
#[derive(Debug, Clone, Default)]
pub struct MacroExpansionInfo {
    /// マクロ展開のチェーン（外側から内側へ）
    /// 例: A が B を含み、B が C を含む場合: [A, B, C]
    pub chain: Vec<MacroInvocation>,
}

impl MacroExpansionInfo {
    pub fn is_empty(&self) -> bool {
        self.chain.is_empty()
    }

    pub fn push(&mut self, invocation: MacroInvocation) {
        self.chain.push(invocation);
    }

    /// 最も内側のマクロ呼び出し
    pub fn innermost(&self) -> Option<&MacroInvocation> {
        self.chain.last()
    }

    /// 最も外側のマクロ呼び出し
    pub fn outermost(&self) -> Option<&MacroInvocation> {
        self.chain.first()
    }
}

/// 単一のマクロ呼び出し情報
#[derive(Debug, Clone)]
pub struct MacroInvocation {
    /// マクロ名
    pub name: InternedStr,
    /// 呼び出し位置
    pub call_loc: SourceLocation,
    /// 関数マクロの場合、引数のテキスト表現
    pub args: Option<Vec<String>>,
}
```

### 2.2 AST ノードの更新パターン

既存の `SourceLocation` フィールドを `NodeInfo` に置き換える:

```rust
// === 変更前 ===
pub struct FunctionDef {
    pub specs: DeclSpecs,
    pub declarator: Declarator,
    pub body: CompoundStmt,
    pub loc: SourceLocation,           // ← これを
    pub comments: Vec<Comment>,
    pub is_target: bool,
}

// === 変更後 ===
pub struct FunctionDef {
    pub specs: DeclSpecs,
    pub declarator: Declarator,
    pub body: CompoundStmt,
    pub info: NodeInfo,                // ← こうする
    pub comments: Vec<Comment>,
    pub is_target: bool,
}

impl FunctionDef {
    /// 後方互換性のための loc アクセサ
    pub fn loc(&self) -> &SourceLocation {
        &self.info.loc
    }
}
```

### 2.3 Expr enum の更新

```rust
/// 式
#[derive(Debug, Clone)]
pub enum Expr {
    // 一次式（タプル形式からstruct形式に変更）
    Ident { name: InternedStr, info: NodeInfo },
    IntLit { value: i64, info: NodeInfo },
    UIntLit { value: u64, info: NodeInfo },
    FloatLit { value: f64, info: NodeInfo },
    CharLit { value: u8, info: NodeInfo },
    StringLit { value: Vec<u8>, info: NodeInfo },

    // 後置式
    Index {
        expr: Box<Expr>,
        index: Box<Expr>,
        info: NodeInfo,
    },
    Call {
        func: Box<Expr>,
        args: Vec<Expr>,
        info: NodeInfo,
    },
    Member {
        expr: Box<Expr>,
        member: InternedStr,
        info: NodeInfo,
    },
    // ... 他のバリアントも同様に info: NodeInfo を持つ
}

impl Expr {
    /// NodeInfo を取得
    pub fn info(&self) -> &NodeInfo {
        match self {
            Expr::Ident { info, .. } => info,
            Expr::IntLit { info, .. } => info,
            // ... 全バリアント
        }
    }

    /// SourceLocation を取得（後方互換性）
    pub fn loc(&self) -> &SourceLocation {
        &self.info().loc
    }

    /// マクロ展開情報を取得
    pub fn macro_info(&self) -> Option<&MacroExpansionInfo> {
        self.info().macro_expansion.as_deref()
    }
}
```

### 2.4 Stmt enum の更新

```rust
/// 文
#[derive(Debug, Clone)]
pub enum Stmt {
    Labeled {
        label: Label,
        stmt: Box<Stmt>,
        info: NodeInfo,
    },
    Compound(CompoundStmt),  // CompoundStmt が info を持つ
    Expr(Option<Box<Expr>>, NodeInfo),
    If {
        cond: Box<Expr>,
        then_stmt: Box<Stmt>,
        else_stmt: Option<Box<Stmt>>,
        info: NodeInfo,
    },
    Switch {
        expr: Box<Expr>,
        body: Box<Stmt>,
        info: NodeInfo,
    },
    While {
        cond: Box<Expr>,
        body: Box<Stmt>,
        info: NodeInfo,
    },
    DoWhile {
        body: Box<Stmt>,
        cond: Box<Expr>,
        info: NodeInfo,
    },
    For {
        init: ForInit,
        cond: Option<Box<Expr>>,
        update: Option<Box<Expr>>,
        body: Box<Stmt>,
        info: NodeInfo,
    },
    Goto(InternedStr, NodeInfo),
    Continue(NodeInfo),
    Break(NodeInfo),
    Return(Option<Box<Expr>>, NodeInfo),
}

impl Stmt {
    pub fn info(&self) -> &NodeInfo {
        match self {
            Stmt::Labeled { info, .. } => info,
            Stmt::Compound(c) => &c.info,
            Stmt::Expr(_, info) => info,
            // ... 全バリアント
        }
    }
}
```

### 2.5 その他のAST構造体

```rust
pub struct CompoundStmt {
    pub items: Vec<BlockItem>,
    pub info: NodeInfo,
}

pub struct Declaration {
    pub specs: DeclSpecs,
    pub declarators: Vec<InitDeclarator>,
    pub info: NodeInfo,
    pub comments: Vec<Comment>,
    pub is_target: bool,
}

pub struct TypeName {
    pub specs: DeclSpecs,
    pub declarator: Option<AbstractDeclarator>,
    pub info: NodeInfo,
}

// ... 他の構造体も同様
```

## Part 3: Parser の変更

### 3.1 マクロコンテキスト管理

```rust
/// パース中のマクロ展開追跡
#[derive(Debug, Clone)]
struct MacroContext {
    /// 現在アクティブなマクロ展開のスタック
    stack: Vec<ActiveMacroExpansion>,
}

#[derive(Debug, Clone)]
struct ActiveMacroExpansion {
    /// MacroBegin の情報
    begin_info: MacroBeginInfo,
}

impl MacroContext {
    fn new() -> Self {
        Self { stack: Vec::new() }
    }

    fn push(&mut self, info: MacroBeginInfo) {
        self.stack.push(ActiveMacroExpansion { begin_info: info });
    }

    fn pop(&mut self, end_marker_id: TokenId) -> Option<ActiveMacroExpansion> {
        // end_marker_id と一致する begin を探してポップ
        if let Some(pos) = self.stack.iter().rposition(|e| e.begin_info.marker_id == end_marker_id) {
            Some(self.stack.remove(pos))
        } else {
            None
        }
    }

    /// 現在のマクロ展開情報を MacroExpansionInfo に変換
    fn to_expansion_info(&self, interner: &StringInterner) -> MacroExpansionInfo {
        MacroExpansionInfo {
            chain: self.stack.iter().map(|e| {
                MacroInvocation {
                    name: e.begin_info.macro_name,
                    call_loc: e.begin_info.call_loc.clone(),
                    args: match &e.begin_info.kind {
                        MacroInvocationKind::Object => None,
                        MacroInvocationKind::Function { args } => {
                            Some(args.iter().map(|a| tokens_to_string(a, interner)).collect())
                        }
                    },
                }
            }).collect(),
        }
    }
}
```

### 3.2 Parser 構造体の拡張

```rust
pub struct Parser<S: TokenSource> {
    // ... 既存フィールド ...

    /// マクロ展開コンテキスト
    macro_ctx: MacroContext,

    /// マクロマーカーを処理するか
    handle_macro_markers: bool,
}
```

### 3.3 トークン取得時のマーカー処理

```rust
impl<S: TokenSource> Parser<S> {
    /// 次のトークンを取得（マーカーを透過的に処理）
    fn next_token(&mut self) -> Result<Token> {
        loop {
            let token = self.source.next_token()?;

            if !self.handle_macro_markers {
                return Ok(token);
            }

            match &token.kind {
                TokenKind::MacroBegin(info) => {
                    self.macro_ctx.push((**info).clone());
                    continue;  // マーカーはスキップ
                }
                TokenKind::MacroEnd(info) => {
                    self.macro_ctx.pop(info.begin_marker_id);
                    continue;  // マーカーはスキップ
                }
                _ => return Ok(token),
            }
        }
    }

    /// 現在位置の NodeInfo を作成
    fn make_node_info(&self, loc: SourceLocation) -> NodeInfo {
        let macro_info = self.macro_ctx.to_expansion_info(&self.interner);
        NodeInfo::with_macro_info(loc, macro_info)
    }

    /// 現在のマクロコンテキストでNodeInfoを作成（locは後で設定）
    fn capture_macro_context(&self) -> MacroExpansionInfo {
        self.macro_ctx.to_expansion_info(&self.interner)
    }
}
```

### 3.4 パース関数の更新例

```rust
impl<S: TokenSource> Parser<S> {
    fn parse_expr_primary(&mut self) -> Result<Expr> {
        let token = self.next_token()?;
        let info = self.make_node_info(token.loc.clone());

        match token.kind {
            TokenKind::Ident(name) => Ok(Expr::Ident { name, info }),
            TokenKind::IntLit(value) => Ok(Expr::IntLit { value, info }),
            // ...
        }
    }

    fn parse_compound_stmt(&mut self) -> Result<CompoundStmt> {
        let lbrace = self.expect(TokenKind::LBrace)?;
        let info = self.make_node_info(lbrace.loc.clone());

        let mut items = Vec::new();
        while !self.check(TokenKind::RBrace) {
            items.push(self.parse_block_item()?);
        }
        self.expect(TokenKind::RBrace)?;

        Ok(CompoundStmt { items, info })
    }
}
```

## Part 4: Rust コード生成での利用

### 4.1 マクロ情報のコメント出力

```rust
impl RustCodeGen {
    /// 式をRustコードに変換
    fn expr_to_rust(&self, expr: &Expr) -> CodeFragment {
        let code = self.expr_kind_to_rust(expr);

        // マクロ情報があればコメントを付与
        if let Some(info) = expr.macro_info() {
            let comment = self.format_macro_comment(info);
            CodeFragment::new(&format!("{}{}", code.code, comment))
        } else {
            code
        }
    }

    fn format_macro_comment(&self, info: &MacroExpansionInfo) -> String {
        if info.is_empty() {
            return String::new();
        }

        let chain: Vec<String> = info.chain.iter().map(|inv| {
            let name = self.interner.resolve(inv.name);
            match &inv.args {
                Some(args) if !args.is_empty() => {
                    format!("{}({})", name, args.join(", "))
                }
                _ => name.to_string(),
            }
        }).collect();

        format!(" /* {} */", chain.join(" → "))
    }
}
```

### 4.2 出力例

```rust
// 入力: SvANY(sv) マクロ（SvANY → MUTABLE_PTR と展開）
pub unsafe fn example(sv: *mut SV) -> *mut c_void {
    ((*sv).sv_any) /* SvANY(sv) → MUTABLE_PTR((*sv).sv_any) */
}

// ネストした展開の例
pub unsafe fn get_flags(sv: *mut SV) -> U32 {
    ((*((*sv).sv_any as *mut XPVHV)).xhv_flags) /* HvFLAGS(hv) → SvFLAGS(hv) */
}
```

### 4.3 デバッグモード

```rust
/// コード生成オプション
pub struct CodeGenOptions {
    /// マクロ展開コメントを出力するか
    pub emit_macro_comments: bool,

    /// 詳細なマクロ情報（位置情報含む）を出力するか
    pub verbose_macro_info: bool,
}

impl RustCodeGen {
    fn format_macro_comment_verbose(&self, info: &MacroExpansionInfo) -> String {
        let details: Vec<String> = info.chain.iter().map(|inv| {
            let name = self.interner.resolve(inv.name);
            let loc = &inv.call_loc;
            format!("{}@{}:{}", name, loc.file_id.0, loc.line)
        }).collect();

        format!(" /* macro: {} */", details.join(" → "))
    }
}
```

## 実装フェーズ

### Phase 1: TokenId の導入 (0.5日)

**変更ファイル**: `src/token.rs`

1. `TokenId` 構造体を追加
2. `Token` に `id: TokenId` フィールドを追加
3. `Token::new()` で自動ID付与
4. 既存テストが通ることを確認

### Phase 2: マーカートークンの追加 (0.5日)

**変更ファイル**: `src/token.rs`

1. `MacroBeginInfo`, `MacroEndInfo`, `MacroInvocationKind` を追加
2. `TokenKind::MacroBegin`, `TokenKind::MacroEnd` を追加
3. Debug/Display 実装を更新

### Phase 3: NoExpandRegistry (0.5日)

**変更ファイル**: `src/preprocessor.rs`

1. `NoExpandRegistry` 構造体を追加
2. 単体テストを作成

### Phase 4: NodeInfo と MacroExpansionInfo (1日)

**変更ファイル**: `src/ast.rs`

1. `NodeInfo` 構造体を追加
2. `MacroExpansionInfo`, `MacroInvocation` を追加
3. `Expr::info()`, `Stmt::info()` などのアクセサを追加（既存構造と並存）

### Phase 5: AST の NodeInfo 移行 (2日)

**変更ファイル**: `src/ast.rs`, `src/parser.rs`, `src/sexp.rs`

1. 主要なAST構造体の `loc: SourceLocation` を `info: NodeInfo` に変更
2. 後方互換の `loc()` メソッドを追加
3. パーサーを更新
4. S式出力を更新
5. 段階的にテスト

### Phase 6: 再帰的マクロ展開 (2日)

**変更ファイル**: `src/preprocessor.rs`

1. `ExpansionContext` 構造体を追加
2. `macro_subst` 再帰関数を実装
3. `wrap_with_markers` を実装
4. `emit_markers` 設定オプションを追加
5. テスト

### Phase 7: 旧実装からの移行 (1日)

**変更ファイル**: `src/preprocessor.rs`, `src/token.rs`

1. `next_token` を新実装に切り替え
2. 旧 `try_expand_macro` を削除
3. `Token::no_expand` フィールドを削除
4. 統合テスト

### Phase 8: Parser のマーカー対応 (1日)

**変更ファイル**: `src/parser.rs`

1. `MacroContext` を追加
2. `handle_macro_markers` オプションを追加
3. `next_token` でマーカーを処理
4. `make_node_info` でマクロ情報を付与
5. テスト

### Phase 9: Rust コード生成 (1日)

**変更ファイル**: `src/rust_codegen.rs`

1. `CodeGenOptions` にマクロコメントオプションを追加
2. `format_macro_comment` を実装
3. 各コード生成関数でマクロ情報を出力
4. テスト

### Phase 10: CLI とドキュメント (0.5日)

**変更ファイル**: `src/main.rs`, `README.md`

1. `--emit-macro-markers` オプションを追加
2. `--macro-comments` オプションを追加
3. ドキュメント更新

## ファイル変更一覧

| ファイル | 変更内容 |
|---------|---------|
| `src/token.rs` | `TokenId`, マーカートークン, `no_expand` 削除 |
| `src/ast.rs` | `NodeInfo`, `MacroExpansionInfo`, 全ノードの更新 |
| `src/preprocessor.rs` | `NoExpandRegistry`, `ExpansionContext`, 再帰展開 |
| `src/parser.rs` | `MacroContext`, マーカー処理, `make_node_info` |
| `src/sexp.rs` | NodeInfo 対応 |
| `src/rust_codegen.rs` | マクロコメント出力 |
| `src/main.rs` | CLI オプション追加 |

## テスト計画

### 単体テスト

```rust
#[test]
fn test_node_info_creation() {
    let info = NodeInfo::new(SourceLocation::default());
    assert!(info.macro_expansion.is_none());
}

#[test]
fn test_macro_expansion_info() {
    let mut info = MacroExpansionInfo::default();
    info.push(MacroInvocation { ... });
    assert_eq!(info.chain.len(), 1);
}
```

### 統合テスト

```c
// test_macros.h
#define FOO(x) ((x) + 1)
#define BAR FOO(10)

int test = BAR;
// 期待: BAR → FOO(10) のマクロチェーン情報がASTに付与される
```

### 出力確認

```bash
# マーカーなし（既存動作と同じ）
cargo run --bin libperl-macrogen -- --auto -E samples/wrapper.h

# マーカーあり（デバッグ用）
cargo run --bin libperl-macrogen -- --auto -E --emit-macro-markers samples/wrapper.h

# Rust生成時にマクロコメント
cargo run --bin libperl-macrogen -- --auto --gen-rust-fns --bindings samples/bindings.rs --macro-comments samples/wrapper.h
```
