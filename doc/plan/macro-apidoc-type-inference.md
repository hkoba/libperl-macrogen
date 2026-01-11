# マクロの apidoc 型情報をパラメータ・戻り値型推論に適用

## 問題

`boolSV` のようなマクロで、apidoc 型情報があるにもかかわらず `:type <unknown>` と表示される。

```c
/*
=for apidoc Am|SV *|boolSV|bool b
*/
#define boolSV(b) ((b) ? &PL_sv_yes : &PL_sv_no)
```

出力:
```
boolSV: expression (0 constraints, 0 uses) [THX]
  (?
    (ident b) :type <unknown>    ← bool であるべき
    ...
  ) :type <unknown>*             ← SV * であるべき
```

## 原因分析

### 1. パラメータ型が Unknown になる理由

- `set_macro_params(&params)` はパラメータ名のみを `macro_params` HashSet に保存
- `infer_expr_type(Ident(b))` は `lookup_symbol(b)` を呼ぶ
- `b` はシンボルテーブルに登録されていないため `Type::Unknown` を返す

### 2. 戻り値型が Unknown になる理由

- `Conditional` 式の型は `then_ty` と `else_ty` から推論
- 両方が `Unknown*` のため、結果も `Unknown*`

### 3. 根本原因

- マクロ自体の apidoc 情報がパラメータの型登録に使われていない
- 関数呼び出し (`ExprKind::Call`) でのみ apidoc が使われる

### 4. 現在のアーキテクチャの問題

- `analyze_macro` で `SemanticAnalyzer` を使って制約収集
- `TypedSexpPrinter` で別の `SemanticAnalyzer` を作成して型推論をやり直し
- `type_env` に収集した情報が型表示に使われていない

## 解決策

### 設計方針

1. **型計算は `analyze_macro` で1回だけ実行**
2. **全ての式の型を `type_env` に保存**
3. **`TypedSexpPrinter` は `type_env` から型を取得**（SemanticAnalyzer を除去）

### Step 1: Lexer をジェネリック化して ReadOnly モードを追加

apidoc の型文字列に含まれる識別子は既に intern 済みのため、新規 intern は不要。
トレイトベースのアプローチで、コード重複なく MutableLexer と ReadOnlyLexer を実装。

**src/lexer.rs:**

```rust
/// 識別子解決トレイト
pub trait IdentResolver {
    /// 識別子文字列を InternedStr に解決
    /// 通常モード: intern して常に成功
    /// 読み取り専用モード: lookup のみ、見つからなければ None
    fn resolve_ident(&mut self, s: &str) -> Option<InternedStr>;
}

/// 通常の intern を行うラッパー
pub struct Interning<'a>(pub &'a mut StringInterner);

impl IdentResolver for Interning<'_> {
    fn resolve_ident(&mut self, s: &str) -> Option<InternedStr> {
        Some(self.0.intern(s))  // 常に成功
    }
}

/// lookup のみ行うラッパー（読み取り専用）
pub struct LookupOnly<'a>(pub &'a StringInterner);

impl IdentResolver for LookupOnly<'_> {
    fn resolve_ident(&mut self, s: &str) -> Option<InternedStr> {
        self.0.lookup(s)  // 見つからなければ None
    }
}

/// ジェネリックな Lexer
pub struct Lexer<'a, R: IdentResolver> {
    source: &'a [u8],
    pos: usize,
    file_id: FileId,
    resolver: R,
    // ... 他のフィールド
}

impl<'a, R: IdentResolver> Lexer<'a, R> {
    // 共通のメソッド群
    // ...

    fn scan_identifier(&mut self) -> Result<TokenKind> {
        // ... 識別子を読み取り
        let text = /* ... */;

        if let Some(kw) = TokenKind::from_keyword(text) {
            Ok(kw)
        } else {
            match self.resolver.resolve_ident(text) {
                Some(id) => Ok(TokenKind::Ident(id)),
                None => Err(LexError::UnknownIdentifier(text.to_string(), loc)),
            }
        }
    }
}

// 型エイリアス
pub type MutableLexer<'a> = Lexer<'a, Interning<'a>>;
pub type ReadOnlyLexer<'a> = Lexer<'a, LookupOnly<'a>>;

// コンストラクタ
impl<'a> Lexer<'a, Interning<'a>> {
    pub fn new(source: &'a [u8], file_id: FileId, interner: &'a mut StringInterner) -> Self {
        Lexer {
            source,
            pos: 0,
            file_id,
            resolver: Interning(interner),
            // ...
        }
    }
}

impl<'a> Lexer<'a, LookupOnly<'a>> {
    pub fn new_readonly(source: &'a [u8], file_id: FileId, interner: &'a StringInterner) -> Self {
        Lexer {
            source,
            pos: 0,
            file_id,
            resolver: LookupOnly(interner),
            // ...
        }
    }
}
```

**利点:**
- コード重複なし: 全てのスキャンロジックは共通
- 型安全: コンパイル時に mutable/readonly が区別される
- ゼロコスト抽象化: トレイトはモノモーフィズムで展開される

### Step 1b: parser.rs に型文字列パース関数を追加

ReadOnlyLexer を使って型文字列をパース。

**src/parser.rs:**

```rust
/// 型文字列から TypeName をパース
///
/// apidoc 等の型文字列（例: "SV *", "const char *"）をパースして
/// TypeName AST を返す。ReadOnlyLexer を使用するため、
/// 型文字列内の識別子は既に intern 済みである必要がある。
pub fn parse_type_from_string(
    type_str: &str,
    interner: &StringInterner,  // &mut 不要
    files: &FileRegistry,
    typedefs: &HashSet<InternedStr>,
) -> Result<TypeName> {
    // 仮想ファイルとして登録
    let file_id = files.register_virtual("<type-string>");

    // ReadOnlyLexer でトークン化（新規 intern なし）
    let mut lexer = Lexer::new_readonly(type_str.as_bytes(), file_id, interner);

    let mut tokens = Vec::new();
    loop {
        let token = lexer.next_token()?;
        if matches!(token.kind, TokenKind::Eof) {
            break;
        }
        tokens.push(token);
    }

    // パース
    let mut source = TokenSliceRef::new(tokens, interner, files);
    let mut parser = Parser::from_source_with_typedefs(&mut source, typedefs.clone())?;
    parser.parse_type_name()
}
```

**注意**: `parse_type_name` は現在 private なので public に変更が必要。

### Step 2: SemanticAnalyzer にマクロパラメータ登録メソッドを追加

apidoc から型情報を取得し、パラメータをシンボルテーブルに登録。

**src/semantic.rs:**

```rust
/// マクロパラメータを apidoc 型情報付きでシンボルテーブルに登録
///
/// # Arguments
/// * `macro_name` - マクロ名
/// * `params` - パラメータ名のリスト
/// * `interner` - 文字列インターナー
/// * `files` - ファイルレジストリ
/// * `typedefs` - typedef 名セット
pub fn register_macro_params_from_apidoc(
    &mut self,
    macro_name: InternedStr,
    params: &[InternedStr],
    files: &FileRegistry,
    typedefs: &HashSet<InternedStr>,
) {
    // macro_params に名前を登録（既存の動作を維持）
    self.macro_params.clear();
    for &param in params {
        self.macro_params.insert(param);
    }

    // apidoc からマクロ情報を取得
    if let Some(apidoc) = self.apidoc {
        let macro_name_str = self.interner.get(macro_name);
        if let Some(entry) = apidoc.get(macro_name_str) {
            // パラメータをシンボルとして登録
            for (i, &param_name) in params.iter().enumerate() {
                if let Some(apidoc_arg) = entry.args.get(i) {
                    // parser で型文字列をパース
                    if let Ok(type_name) = parse_type_from_string(
                        &apidoc_arg.ty,
                        self.interner,
                        files,
                        typedefs,
                    ) {
                        let ty = self.resolve_type_name(&type_name);
                        self.define_symbol(Symbol {
                            name: param_name,
                            ty,
                            loc: SourceLocation::default(),
                            kind: SymbolKind::Variable,
                        });
                    }
                }
            }
        }
    }
}
```

### Step 3: 全式の型を計算して type_env に保存

`analyze_macro` で全式を再帰的に走査し、型を計算して `type_env` に保存。

**src/macro_infer.rs:**

```rust
/// 式の型を計算して type_env に保存（再帰的に全子式を処理）
fn compute_and_store_expr_types(
    expr: &Expr,
    analyzer: &mut SemanticAnalyzer,
    type_env: &mut TypeEnv,
) {
    // この式の型を計算
    let ty = analyzer.infer_expr_type(expr);
    let ty_str = ty.to_string();

    // type_env に保存
    type_env.add_constraint(TypeConstraint::new(
        expr.id,
        ty_str,
        ConstraintSource::Inferred,
        "computed type",
    ));

    // 子式を再帰的に処理
    match &expr.kind {
        ExprKind::Call { func, args } => {
            compute_and_store_expr_types(func, analyzer, type_env);
            for arg in args {
                compute_and_store_expr_types(arg, analyzer, type_env);
            }
        }
        ExprKind::Binary { lhs, rhs, .. } => {
            compute_and_store_expr_types(lhs, analyzer, type_env);
            compute_and_store_expr_types(rhs, analyzer, type_env);
        }
        ExprKind::Conditional { cond, then_expr, else_expr } => {
            compute_and_store_expr_types(cond, analyzer, type_env);
            compute_and_store_expr_types(then_expr, analyzer, type_env);
            compute_and_store_expr_types(else_expr, analyzer, type_env);
        }
        ExprKind::Cast { expr: inner, .. }
        | ExprKind::PreInc(inner)
        | ExprKind::PreDec(inner)
        | ExprKind::PostInc(inner)
        | ExprKind::PostDec(inner)
        | ExprKind::AddrOf(inner)
        | ExprKind::Deref(inner)
        | ExprKind::UnaryPlus(inner)
        | ExprKind::UnaryMinus(inner)
        | ExprKind::BitNot(inner)
        | ExprKind::LogNot(inner)
        | ExprKind::Sizeof(inner) => {
            compute_and_store_expr_types(inner, analyzer, type_env);
        }
        ExprKind::Index { expr: base, index } => {
            compute_and_store_expr_types(base, analyzer, type_env);
            compute_and_store_expr_types(index, analyzer, type_env);
        }
        ExprKind::Member { expr: base, .. }
        | ExprKind::PtrMember { expr: base, .. } => {
            compute_and_store_expr_types(base, analyzer, type_env);
        }
        ExprKind::Assign { lhs, rhs, .. } => {
            compute_and_store_expr_types(lhs, analyzer, type_env);
            compute_and_store_expr_types(rhs, analyzer, type_env);
        }
        ExprKind::Comma { lhs, rhs } => {
            compute_and_store_expr_types(lhs, analyzer, type_env);
            compute_and_store_expr_types(rhs, analyzer, type_env);
        }
        ExprKind::StmtExpr(compound) => {
            // 複合文内の式も処理
            for item in &compound.items {
                if let BlockItem::Stmt(Stmt::Expr(Some(e), _)) = item {
                    compute_and_store_expr_types(e, analyzer, type_env);
                }
            }
        }
        // リテラル・識別子・型操作は子式なし
        ExprKind::Ident(_)
        | ExprKind::IntLit(_)
        | ExprKind::UIntLit(_)
        | ExprKind::FloatLit(_)
        | ExprKind::CharLit(_)
        | ExprKind::StringLit(_)
        | ExprKind::SizeofType(_)
        | ExprKind::Alignof(_)
        | ExprKind::CompoundLit { .. } => {}
    }
}
```

### Step 4: analyze_macro の修正

**src/macro_infer.rs:**

```rust
pub fn analyze_macro<'a>(
    &mut self,
    def: &MacroDef,
    macro_table: &MacroTable,
    thx_macros: &HashSet<InternedStr>,
    interner: &'a StringInterner,
    files: &FileRegistry,
    apidoc: Option<&'a ApidocDict>,
    fields_dict: Option<&'a FieldsDict>,
    rust_decl_dict: Option<&'a RustDeclDict>,
    typedefs: &HashSet<InternedStr>,
) {
    let mut info = MacroInferInfo::new(def.name);
    info.is_target = def.is_target;
    info.is_thx_dependent = thx_macros.contains(&def.name);

    // 関数形式マクロの場合、パラメータを取得
    let params: Vec<InternedStr> = match &def.kind {
        MacroKind::Function { params, .. } => params.clone(),
        MacroKind::Object => vec![],
    };

    // マクロ本体を展開
    let expander = TokenExpander::new(macro_table, interner, files);
    let expanded_tokens = expander.expand(&def.body);

    // def-use 関係を収集
    self.collect_uses(&expanded_tokens, macro_table, &mut info);

    // パースを試行
    info.parse_result = self.try_parse_tokens(&expanded_tokens, interner, files, typedefs);

    // パース成功した場合、型を計算して type_env に保存
    if let ParseResult::Expression(ref expr) = info.parse_result {
        let mut analyzer = SemanticAnalyzer::with_rust_decl_dict(
            interner,
            apidoc,
            fields_dict,
            rust_decl_dict,
        );

        // apidoc 型情報付きでパラメータをシンボルテーブルに登録
        analyzer.register_macro_params_from_apidoc(def.name, &params, files, typedefs);

        // 従来の制約収集（関数呼び出しからの制約）
        analyzer.collect_expr_constraints(expr, &mut info.type_env);

        // 全式の型を計算して type_env に保存
        compute_and_store_expr_types(expr, &mut analyzer, &mut info.type_env);

        // マクロ自体の戻り値型を制約として追加
        if let Some(apidoc_dict) = apidoc {
            let macro_name_str = interner.get(def.name);
            if let Some(entry) = apidoc_dict.get(macro_name_str) {
                if let Some(ref return_type) = entry.return_type {
                    info.type_env.add_return_constraint(TypeConstraint::new(
                        expr.id,
                        return_type,
                        ConstraintSource::Apidoc,
                        format!("return type of macro {}", macro_name_str),
                    ));
                }
            }
        }
    }

    self.register(info);
}
```

### Step 5: TypedSexpPrinter を type_env ベースに変更

**src/sexp.rs:**

```rust
/// 型注釈付きS-expression出力プリンター
pub struct TypedSexpPrinter<'a, W: Write> {
    writer: W,
    interner: &'a StringInterner,
    type_env: Option<&'a TypeEnv>,  // SemanticAnalyzer の代わりに type_env
    indent: usize,
    pretty: bool,
    skip_first_newline: bool,
}

impl<'a, W: Write> TypedSexpPrinter<'a, W> {
    /// 新しいプリンターを作成
    pub fn new(
        writer: W,
        interner: &'a StringInterner,
    ) -> Self {
        Self {
            writer,
            interner,
            type_env: None,
            indent: 0,
            pretty: false,
            skip_first_newline: false,
        }
    }

    /// type_env を設定
    pub fn set_type_env(&mut self, type_env: &'a TypeEnv) {
        self.type_env = Some(type_env);
    }

    /// ExprId から型文字列を取得
    fn get_type_str(&self, expr_id: ExprId) -> String {
        if let Some(type_env) = self.type_env {
            // expr_constraints から型を取得
            if let Some(constraints) = type_env.expr_constraints.get(&expr_id) {
                // 複数制約がある場合は最初のものを使用
                // TODO: 優先度付きで選択する場合はここを変更
                if let Some(constraint) = constraints.first() {
                    return constraint.ty.clone();
                }
            }
        }
        "<unknown>".to_string()
    }

    // print_expr 等で get_type_str を使用して型を出力
    // ...
}
```

### Step 6: main.rs の出力部分を修正

**src/main.rs:**

```rust
match &info.parse_result {
    ParseResult::Expression(expr) => {
        let stdout = io::stdout();
        let mut handle = stdout.lock();

        // type_env ベースの printer を作成
        let mut printer = TypedSexpPrinter::new(&mut handle, interner);
        printer.set_type_env(&info.type_env);
        printer.set_pretty(true);
        printer.set_indent(1);
        printer.set_skip_first_newline(true);

        let _ = printer.print_expr(expr);
        let _ = writeln!(handle);
    }
    ParseResult::Unparseable(Some(err_msg)) => {
        println!("  error: {}", err_msg);
    }
    _ => {}
}
```

## 修正対象ファイル

1. **src/lexer.rs**
   - `IdentResolver` トレイトを追加
   - `Interning`, `LookupOnly` ラッパー型を追加
   - `Lexer` をジェネリック化 `Lexer<'a, R: IdentResolver>`
   - `MutableLexer`, `ReadOnlyLexer` 型エイリアスを追加
   - `new_readonly` コンストラクタを追加
   - `LexError::UnknownIdentifier` バリアントを追加

2. **src/parser.rs**
   - `parse_type_from_string` 関数を追加（ReadOnlyLexer を使用）
   - `parse_type_name` を public に変更

3. **src/semantic.rs**
   - `register_macro_params_from_apidoc` メソッドを追加

4. **src/macro_infer.rs**
   - `compute_and_store_expr_types` 関数を追加
   - `analyze_macro` で全式の型計算と apidoc 戻り値型の追加

5. **src/sexp.rs**
   - `TypedSexpPrinter` から `SemanticAnalyzer` を削除
   - `type_env` ベースの型取得に変更

6. **src/main.rs**
   - `TypedSexpPrinter` の生成を変更（apidoc, fields_dict 不要に）
   - `set_type_env` で type_env を設定

## 期待される結果

```
boolSV: expression (2 constraints, 0 uses) [THX]
  (?
    (ident b) :type bool
    (addr-of
      (ptr-member
        (ident my_perl) :type PerlInterpreter* Isv_yes) :type SV) :type SV*
    (addr-of
      (ptr-member
        (ident my_perl) :type PerlInterpreter* Isv_no) :type SV) :type SV*) :type SV*
```

## 設計上の利点

1. **型計算は1回だけ**: `analyze_macro` で全式の型を計算・保存
2. **重複排除**: printer は保存済みの型を参照するだけ
3. **parser と semantic の責務分離**: 型文字列のパースは parser が担当
4. **拡張性**: type_env に複数の制約を保存できるため、将来的に制約の優先度付き選択も可能

## 補足: my_perl の型について

`my_perl` は THX (aTHX/tTHX/my_perl) マクロであり、型は `PerlInterpreter *`。
現在は Unknown だが、以下の方法で対応可能：

1. THX シンボルを特別扱いして型を設定
2. または apidoc/ヘッダーから THX の型情報を収集

これは本改善とは別の課題として扱う。
