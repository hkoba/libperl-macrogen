# TinyCC から Rust への移植ガイド

## 1. アーキテクチャ改善提案

### 1.1 現状の問題点

1. **グローバル状態の多用**: `tok`, `tokc`, `vtop`, `file` など多数のグローバル変数
2. **巨大な共用体**: `CValue`, `Sym` の複雑な共用体
3. **密結合**: プリプロセッサとパーサーが直接グローバル変数を共有
4. **エラー処理**: `setjmp`/`longjmp` による非構造化エラー処理

### 1.2 Rust での改善設計

#### コンテキスト構造体による状態管理

```rust
// プリプロセッサコンテキスト
pub struct PreprocessorContext {
    file_stack: Vec<SourceFile>,
    macro_stack: Vec<MacroExpansion>,
    defines: HashMap<InternedStr, MacroDef>,
    ifdef_stack: Vec<IfDefState>,
    include_paths: Vec<PathBuf>,
    cached_includes: HashSet<PathBuf>,
}

// パーサーコンテキスト
pub struct ParserContext {
    token: Token,
    prev_token: Option<Token>,
    value_stack: Vec<Value>,
    scope_stack: Vec<Scope>,
}

// 統合コンパイラ状態
pub struct CompilerState {
    preprocessor: PreprocessorContext,
    parser: ParserContext,
    symbols: SymbolTable,
    types: TypeRegistry,
    files: FileRegistry,
    diagnostics: DiagnosticEngine,
}
```

#### 共用体の enum 化

```rust
// CValue の代替
#[derive(Debug, Clone)]
pub enum ConstValue {
    Int(i64),
    UInt(u64),
    Float(f32),
    Double(f64),
    LongDouble(f80),  // または f64 で代用
    String(Vec<u8>),
    WideString(Vec<u32>),
}

// Sym の代替
#[derive(Debug)]
pub enum SymbolKind {
    Variable {
        offset: i32,
        storage: StorageClass,
        is_extern: bool,
    },
    Function {
        params: Vec<SymbolId>,
        return_type: TypeId,
        body: Option<FunctionBody>,
        is_inline: bool,
        is_static: bool,
    },
    Macro {
        kind: MacroKind,
        tokens: Vec<Token>,
        params: Option<Vec<InternedStr>>,
    },
    Struct {
        fields: Vec<FieldId>,
        layout: StructLayout,
        is_union: bool,
    },
    Enum {
        values: Vec<(InternedStr, i64)>,
        underlying_type: TypeId,
    },
    Typedef {
        target: TypeId,
    },
    Label {
        state: LabelState,
        address: Option<u32>,
    },
}

pub struct Symbol {
    pub id: SymbolId,
    pub name: InternedStr,
    pub kind: SymbolKind,
    pub defined_at: SourceLocation,
    pub attributes: SymbolAttributes,
}
```

---

## 2. モジュール分割

### 推奨ディレクトリ構造

```
src/
├── lib.rs                  # メインAPI
├── common/
│   ├── mod.rs
│   ├── intern.rs           # String Interner
│   ├── source.rs           # SourceLocation, FileRegistry
│   └── diagnostics.rs      # エラー/警告処理
│
├── lexer/
│   ├── mod.rs              # Lexer trait/struct
│   ├── token.rs            # Token enum
│   ├── scanner.rs          # 文字スキャン
│   └── keywords.rs         # キーワードテーブル
│
├── preprocessor/
│   ├── mod.rs              # Preprocessor メイン
│   ├── macro_def.rs        # MacroDef 構造体
│   ├── macro_expand.rs     # マクロ展開ロジック
│   ├── include.rs          # #include 処理
│   ├── conditional.rs      # #if/#ifdef 処理
│   └── expr.rs             # プリプロセッサ式評価
│
├── parser/
│   ├── mod.rs              # Parser メイン
│   ├── decl.rs             # 宣言解析
│   ├── expr.rs             # 式解析
│   ├── stmt.rs             # 文解析
│   └── types.rs            # 型宣言解析
│
├── semantic/
│   ├── mod.rs              # 意味解析メイン
│   ├── symbol.rs           # シンボルテーブル
│   ├── scope.rs            # スコープ管理
│   ├── type_check.rs       # 型チェック
│   └── type_system.rs      # 型システム
│
├── ast/
│   ├── mod.rs              # AST 定義
│   ├── types.rs            # 型 AST
│   ├── expr.rs             # 式 AST
│   ├── stmt.rs             # 文 AST
│   └── decl.rs             # 宣言 AST
│
└── output/                 # 最終出力（Rust コード生成等）
    ├── mod.rs
    ├── macro_output.rs     # マクロ出力
    └── func_output.rs      # inline static 関数出力
```

---

## 3. Rust 標準データ構造への置き換え

| TinyCC 構造 | 現実装 | Rust 代替 | 備考 |
|-------------|--------|-----------|------|
| `TokenSym` ハッシュテーブル | 手製ハッシュ | `HashMap<String, TokenId>` | String Interner パターン推奨 |
| `CString` | 手製動的文字列 | `String` / `Vec<u8>` | |
| `dynarray` | 手製動的配列 | `Vec<T>` | |
| `define_stack` | リンクリスト | `Vec<MacroDef>` + スコープID | 連続メモリで高速 |
| `global_stack` / `local_stack` | リンクリスト | `Vec<Symbol>` + スコープ管理 | |
| `include_stack` | 固定配列 | `Vec<SourceFile>` | |
| `ifdef_stack` | 固定配列 | `Vec<IfDefState>` | |
| `vstack` | 固定配列 | `Vec<Value>` | 動的サイズ対応 |
| `cached_includes` | 手製ハッシュ | `HashSet<PathBuf>` | |
| `section` リスト | リンクリスト | `Vec<Section>` + ID | |

---

## 4. String Interner パターン

識別子の効率的な管理のための推奨実装：

```rust
use std::collections::HashMap;

/// インターン済み文字列の識別子
#[derive(Clone, Copy, Eq, PartialEq, Hash, Debug)]
pub struct InternedStr(u32);

/// 文字列インターナー
pub struct StringInterner {
    strings: Vec<String>,
    map: HashMap<String, InternedStr>,
}

impl StringInterner {
    pub fn new() -> Self {
        Self {
            strings: Vec::new(),
            map: HashMap::new(),
        }
    }

    /// 文字列をインターンし、IDを返す
    pub fn intern(&mut self, s: &str) -> InternedStr {
        if let Some(&id) = self.map.get(s) {
            return id;
        }
        let id = InternedStr(self.strings.len() as u32);
        self.strings.push(s.to_owned());
        self.map.insert(s.to_owned(), id);
        id
    }

    /// IDから文字列を取得
    pub fn get(&self, id: InternedStr) -> &str {
        &self.strings[id.0 as usize]
    }
}
```

---

## 5. エラーハンドリング

### 現状 (C)
```c
// setjmp/longjmp によるエラー処理
if (setjmp(s1->error_jmp_buf) == 0) {
    // コンパイル処理
} else {
    // エラー発生
}
```

### Rust での改善
```rust
/// ソース位置情報
#[derive(Debug, Clone)]
pub struct SourceLocation {
    pub file_id: FileId,
    pub line: u32,
    pub column: u32,
}

/// プリプロセッサエラー種別
#[derive(Debug)]
pub enum PPErrorKind {
    UnterminatedComment,
    UnterminatedString,
    InvalidDirective(String),
    MacroRedefinition(String),
    IncludeNotFound(PathBuf),
    CircularInclude(PathBuf),
    UnmatchedEndif,
    MissingEndif,
}

/// パースエラー種別
#[derive(Debug)]
pub enum ParseErrorKind {
    UnexpectedToken { expected: String, found: Token },
    InvalidType,
    InvalidDeclaration,
    UndeclaredIdentifier(String),
    Redeclaration(String),
}

/// 型エラー種別
#[derive(Debug)]
pub enum TypeErrorKind {
    TypeMismatch { expected: Type, found: Type },
    InvalidCast { from: Type, to: Type },
    IncompatibleTypes { op: String, left: Type, right: Type },
}

/// コンパイルエラー
#[derive(Debug)]
pub enum CompileError {
    Lexer { loc: SourceLocation, msg: String },
    Preprocessor { loc: SourceLocation, kind: PPErrorKind },
    Parse { loc: SourceLocation, kind: ParseErrorKind },
    Type { loc: SourceLocation, kind: TypeErrorKind },
    Semantic { loc: SourceLocation, msg: String },
}

pub type Result<T> = std::result::Result<T, CompileError>;

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompileError::Parse { loc, kind } => {
                write!(f, "{}:{}:{}: parse error: {:?}",
                    loc.file_id.0, loc.line, loc.column, kind)
            }
            // ... 他のバリアント
            _ => write!(f, "{:?}", self),
        }
    }
}

impl std::error::Error for CompileError {}
```

---

## 6. ファイル定義位置の記録

プロジェクト要件: inline static 関数とマクロがどのファイルで定義されたかを記録

### データ構造

```rust
/// ファイル識別子
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub struct FileId(u32);

/// ファイルレジストリ
pub struct FileRegistry {
    paths: Vec<PathBuf>,
    path_to_id: HashMap<PathBuf, FileId>,
}

impl FileRegistry {
    pub fn new() -> Self {
        Self {
            paths: Vec::new(),
            path_to_id: HashMap::new(),
        }
    }

    pub fn register(&mut self, path: PathBuf) -> FileId {
        if let Some(&id) = self.path_to_id.get(&path) {
            return id;
        }
        let id = FileId(self.paths.len() as u32);
        self.path_to_id.insert(path.clone(), id);
        self.paths.push(path);
        id
    }

    pub fn get_path(&self, id: FileId) -> &Path {
        &self.paths[id.0 as usize]
    }
}

/// ソース位置
#[derive(Debug, Clone)]
pub struct SourceLocation {
    pub file_id: FileId,
    pub line: u32,
    pub column: u32,
}

/// マクロ定義
pub struct MacroDef {
    pub name: InternedStr,
    pub kind: MacroKind,
    pub params: Option<Vec<InternedStr>>,
    pub body: Vec<Token>,
    pub defined_at: SourceLocation,  // 定義位置
    pub is_function_like: bool,
}

/// 関数定義
pub struct FunctionDef {
    pub name: InternedStr,
    pub return_type: TypeId,
    pub params: Vec<Parameter>,
    pub body: Option<Block>,
    pub defined_at: SourceLocation,  // 定義位置
    pub is_inline: bool,
    pub is_static: bool,
}
```

### 記録タイミング

```rust
impl Preprocessor {
    fn parse_define(&mut self) -> Result<()> {
        let start_loc = self.current_location();

        // マクロ名解析
        let name = self.expect_identifier()?;

        // パラメータ解析（関数マクロの場合）
        let params = self.parse_macro_params()?;

        // 本体解析
        let body = self.parse_macro_body()?;

        let macro_def = MacroDef {
            name,
            kind: if params.is_some() { MacroKind::Function } else { MacroKind::Object },
            params,
            body,
            defined_at: start_loc,  // 定義位置を記録
            is_function_like: params.is_some(),
        };

        self.defines.insert(name, macro_def);
        Ok(())
    }
}

impl Parser {
    fn parse_function_definition(&mut self, decl: Declaration) -> Result<FunctionDef> {
        let start_loc = decl.location.clone();

        // パラメータ解析
        let params = self.parse_params(&decl.ty)?;

        // 関数本体解析
        let body = self.parse_block()?;

        Ok(FunctionDef {
            name: decl.name,
            return_type: decl.ty.return_type(),
            params,
            body: Some(body),
            defined_at: start_loc,  // 定義位置を記録
            is_inline: decl.is_inline,
            is_static: decl.is_static,
        })
    }
}
```

### 出力フィルタリング

```rust
/// 出力フィルタ設定
pub struct OutputFilter {
    /// 対象ディレクトリ一覧
    pub target_dirs: Vec<PathBuf>,
}

impl OutputFilter {
    /// 指定された位置が出力対象かどうかを判定
    pub fn should_output(&self, loc: &SourceLocation, files: &FileRegistry) -> bool {
        let path = files.get_path(loc.file_id);
        self.target_dirs.iter().any(|dir| path.starts_with(dir))
    }
}

/// 定義収集
pub fn collect_definitions(
    compiler: &Compiler,
    filter: &OutputFilter,
) -> (Vec<MacroDef>, Vec<FunctionDef>) {
    // マクロ収集
    let macros: Vec<_> = compiler.defines.values()
        .filter(|m| filter.should_output(&m.defined_at, &compiler.files))
        .cloned()
        .collect();

    // inline static 関数収集
    let functions: Vec<_> = compiler.functions.iter()
        .filter(|f| f.is_inline && f.is_static)
        .filter(|f| filter.should_output(&f.defined_at, &compiler.files))
        .cloned()
        .collect();

    (macros, functions)
}
```

---

## 7. 実装優先順位

### Phase 1: 基盤構築
1. String Interner / FileRegistry
2. Token 定義 / SourceLocation
3. 基本的な Lexer（コメント、文字列、識別子、数値）
4. エラー型定義

### Phase 2: プリプロセッサ
1. #include 処理（ファイル位置記録含む）
2. #define / #undef（マクロ定義位置記録含む）
3. マクロ展開（オブジェクトマクロ → 関数マクロ）
4. 条件コンパイル（#if, #ifdef, #ifndef, #else, #endif）
5. トークン連結 (##) と文字列化 (#)

### Phase 3: パーサー
1. 型宣言解析（基本型 → ポインタ → 配列 → 関数）
2. struct / union / enum 解析
3. 式解析（単項 → 二項演算子優先順位）
4. 文解析（if, while, for, switch, return）
5. 関数定義解析

### Phase 4: 意味解析
1. シンボルテーブル / スコープ管理
2. 型チェック
3. inline static 関数の抽出（定義位置記録含む）

### Phase 5: 出力生成
1. マクロの Rust 変換
2. inline static 関数の Rust 変換
3. 型定義の Rust 変換

---

## 8. テスト戦略

### 単体テスト

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lexer_identifier() {
        let mut lexer = Lexer::new("foo bar_123");
        assert_eq!(lexer.next_token(), Token::Ident("foo".into()));
        assert_eq!(lexer.next_token(), Token::Ident("bar_123".into()));
    }

    #[test]
    fn test_preprocessor_define() {
        let mut pp = Preprocessor::new();
        pp.process("#define FOO 42").unwrap();
        assert!(pp.is_defined("FOO"));
    }

    #[test]
    fn test_macro_expansion() {
        let mut pp = Preprocessor::new();
        pp.process("#define ADD(a,b) ((a)+(b))").unwrap();
        let tokens = pp.expand("ADD(1, 2)").unwrap();
        // 結果検証
    }
}
```

### 統合テスト

TinyCC の `tests/pp/` ディレクトリにあるプリプロセッサテストを活用：

```rust
#[test]
fn test_pp_suite() {
    for entry in std::fs::read_dir("../tinycc/tests/pp").unwrap() {
        let path = entry.unwrap().path();
        if path.extension() == Some("c".as_ref()) {
            // テスト実行
        }
    }
}
```
