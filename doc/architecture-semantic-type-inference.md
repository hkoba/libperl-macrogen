# 意味解析と型推論のアーキテクチャ

このドキュメントでは、libperl-macrogen における意味解析と型推論の仕組みを解説する。

## 概要

型推論システムは、C マクロおよびインライン関数から Rust コードを生成する際に、適切な型情報を導出するために使用される。

```
┌─────────────────────────────────────────────────────────────────────┐
│                        入力ソース                                    │
├─────────────────────────────────────────────────────────────────────┤
│  wrapper.h    bindings.rs    embed.fnc (apidoc)                     │
│  (C header)   (Rust types)   (API documentation)                    │
└─────────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────────┐
│                     infer_api.rs                                     │
│  ・パイプライン統合                                                  │
│  ・Preprocessor, Parser, MacroInferContext の連携                   │
└─────────────────────────────────────────────────────────────────────┘
                              │
          ┌───────────────────┼───────────────────┐
          ▼                   ▼                   ▼
┌─────────────────┐ ┌─────────────────┐ ┌─────────────────┐
│  macro_infer.rs │ │  semantic.rs    │ │  type_env.rs    │
│  ・マクロ解析    │ │  ・型推論エンジン │ │  ・制約管理      │
│  ・def-use 関係 │ │  ・スコープ管理   │ │  ・TypeConstraint│
└─────────────────┘ └─────────────────┘ └─────────────────┘
          │                   │                   │
          └───────────────────┼───────────────────┘
                              ▼
┌─────────────────────────────────────────────────────────────────────┐
│               型表現レイヤー                                          │
├──────────────────────────┬──────────────────────────────────────────┤
│  type_repr.rs            │  unified_type.rs                         │
│  ・TypeRepr (制約用)      │  ・UnifiedType (正規化済み型)              │
│  ・出所情報付き           │  ・C/Rust 型の相互変換                     │
└──────────────────────────┴──────────────────────────────────────────┘
```

## 主要コンポーネントの役割分担

### 1. macro_infer.rs - マクロ解析エンジン

**責務**: マクロ定義から型情報を推論する

#### MacroInferContext

マクロ解析の中心となる構造体。

```rust
pub struct MacroInferContext {
    /// マクロ名 → 推論情報
    pub macros: HashMap<InternedStr, MacroInferInfo>,
    /// 型確定済みマクロ
    pub confirmed: HashSet<InternedStr>,
    /// 型未確定マクロ
    pub unconfirmed: HashSet<InternedStr>,
    /// 型推論不能マクロ
    pub unknown: HashSet<InternedStr>,
}
```

主要メソッド:
- `analyze_all_macros()` - 全マクロの解析エントリポイント
- `build_macro_info()` - Phase 1: パースと基本情報収集
- `infer_macro_types()` - Phase 2: 型制約の収集

#### MacroInferInfo

個々のマクロの推論情報を保持。

```rust
pub struct MacroInferInfo {
    pub name: InternedStr,
    pub is_target: bool,            // ターゲットマクロか
    pub has_body: bool,             // マクロ本体にトークンがあるか
    pub is_function: bool,          // 関数形式マクロか
    pub uses: HashSet<InternedStr>, // 使用するマクロ (def-use)
    pub used_by: HashSet<InternedStr>, // 使用されるマクロ (use-def)
    pub is_thx_dependent: bool,     // THX 依存か
    pub has_token_pasting: bool,    // ## を含むか
    pub params: Vec<MacroParam>,    // パラメータリスト
    pub parse_result: ParseResult,  // パース結果 (Expression/Statement/Unparseable)
    pub type_env: TypeEnv,          // 型制約環境
    pub args_infer_status: InferStatus,   // 引数の型推論状態
    pub return_infer_status: InferStatus, // 戻り値の型推論状態
    pub generic_type_params: HashMap<i32, String>, // ジェネリック型パラメータ
    pub literal_string_params: HashSet<usize>,     // &str 型パラメータのインデックス
    pub function_call_count: usize,     // 関数呼び出しの数
    pub deref_count: usize,             // ポインタデリファレンスの数
    pub called_functions: HashSet<InternedStr>,    // 呼び出す関数
    pub calls_unavailable: bool,        // 利用不可関数の呼び出しを含む（推移的）
}
```

**ジェネリック型パラメータ** (`generic_type_params`):
- apidoc で `type` や `cast` として宣言されたパラメータ、またはパーサーが自動検出した型パラメータ
- key: パラメータインデックス（-1 は戻り値型）、value: 型パラメータ名 ("T", "U", etc.)
- コード生成時に turbofish 構文 `fn::<T>(...)` として出力

**リテラル文字列パラメータ** (`literal_string_params`):
- apidoc で `"..."` 形式の引数として宣言されたパラメータ
- Rust では `&str` 型として出力し、`.as_ptr()` / `.len()` 変換を自動適用

**利用不可フラグ** (`calls_unavailable`):
- このマクロが直接または推移的に利用不可関数を呼び出す場合 true
- カスケード依存検出に使用（依存先が利用不可なら呼び出し元もコメントアウト）

#### 展開制御シンボル

```rust
/// 展開を抑制するマクロ (assert など)
pub struct NoExpandSymbols {
    pub assert: InternedStr,
    pub assert_: InternedStr,
}

/// 明示的に展開するマクロ
pub struct ExplicitExpandSymbols {
    pub sv_any: InternedStr,         // SvANY（sv->sv_any に展開）
    pub sv_flags: InternedStr,       // SvFLAGS（sv->sv_flags に展開）
    pub cv_flags: InternedStr,       // CvFLAGS（CV 用）
    pub hek_flags: InternedStr,      // HEK_FLAGS
    pub expect: InternedStr,         // EXPECT（__builtin_expect ラッパー）
    pub likely: InternedStr,         // LIKELY
    pub unlikely: InternedStr,       // UNLIKELY
    pub cbool: InternedStr,          // cBOOL（条件を bool に変換）
    pub assert_underscore_: InternedStr, // __ASSERT_
    pub str_with_len: InternedStr,   // STR_WITH_LEN
    pub int2ptr: InternedStr,        // INT2PTR（整数→ポインタキャスト）
    pub assert_not_rok: InternedStr, // assert_not_ROK
    pub assert_not_glob: InternedStr,// assert_not_glob
    pub mutable_ptr: InternedStr,    // MUTABLE_PTR（identity キャスト）
}
```

### 2. semantic.rs - 意味解析エンジン

**責務**: スコープ管理と式の型推論

#### SemanticAnalyzer

```rust
pub struct SemanticAnalyzer<'a> {
    interner: &'a StringInterner,
    scopes: Vec<Scope>,              // スコープスタック
    current_scope: ScopeId,          // 現在のスコープ
    struct_defs: HashMap<InternedStr, Vec<(InternedStr, Type)>>,
    union_defs: HashMap<InternedStr, Vec<(InternedStr, Type)>>,
    typedef_defs: HashMap<InternedStr, Type>,
    apidoc: Option<&'a ApidocDict>,  // API ドキュメント
    fields_dict: Option<&'a FieldsDict>, // フィールド型情報
    rust_decl_dict: Option<&'a RustDeclDict>, // Rust バインディング
    inline_fn_dict: Option<&'a InlineFnDict>, // インライン関数
    macro_params: HashSet<InternedStr>, // マクロパラメータ
    macro_return_types: Option<&'a HashMap<String, String>>, // 確定済みマクロの戻り値型
    macro_param_types: Option<&'a HashMap<String, Vec<(String, String)>>>, // 確定済みマクロのパラメータ型
    files: Option<&'a FileRegistry>,        // 型文字列パース用
    parser_typedefs: Option<&'a HashSet<InternedStr>>, // 型文字列パース用
}
```

`files` と `parser_typedefs` は `register_macro_params_from_apidoc()` で設定され、
`parse_type_string()` ヘルパーメソッドで完全な C パーサーによる型文字列解析に使用される。

`macro_return_types` と `macro_param_types` はネストしたマクロ呼び出しからの型伝播に使用される。
依存順で型推論を行う際、既に型が確定したマクロの型情報をキャッシュし、
後続のマクロの型推論で参照する。

#### 主要機能

1. **スコープ管理**
   - `push_scope()` / `pop_scope()` - スコープの開始/終了
   - `define_symbol()` - シンボル登録
   - `lookup_symbol()` - シンボル検索（スコープチェーン）

2. **型解決**
   - `resolve_decl_specs()` - DeclSpecs から Type を構築
   - `apply_declarator()` - Declarator でポインタ等を適用
   - `resolve_type_name()` - TypeName の解決

3. **型制約収集**
   - `collect_expr_constraints()` - 式から型制約を収集
   - `collect_stmt_constraints()` - 文から型制約を収集
   - `register_macro_params_from_apidoc()` - apidoc 型情報でパラメータを登録

4. **ネストしたマクロ呼び出しからの型伝播**
   - `set_macro_return_types()` - 確定済みマクロの戻り値型キャッシュを設定
   - `set_macro_param_types()` - 確定済みマクロのパラメータ型キャッシュを設定
   - `get_macro_return_type()` - マクロの戻り値型を取得
   - `get_macro_param_types()` - マクロのパラメータ型を取得

#### Type 列挙型

C 言語の型を表現:

```rust
pub enum Type {
    Void, Char, SignedChar, UnsignedChar,
    Short, Int, Long, LongLong, // 符号付き整数
    UnsignedShort, UnsignedInt, UnsignedLong, UnsignedLongLong, // 符号なし
    Float, Double, LongDouble,  // 浮動小数点
    Bool, Int128, UnsignedInt128,
    Pointer(Box<Type>, TypeQualifiers),
    Array(Box<Type>, Option<usize>),
    Function { return_type: Box<Type>, params: Vec<Type>, variadic: bool },
    Struct { name: Option<InternedStr>, members: Option<Vec<(InternedStr, Type)>> },
    Union { ... },
    Enum { name: Option<InternedStr> },
    TypedefName(InternedStr),
    Unknown,
}
```

### 3. type_env.rs - 型制約管理

**責務**: 型制約の収集と管理

#### TypeEnv

```rust
pub struct TypeEnv {
    /// パラメータ名 → 型制約リスト
    pub param_constraints: HashMap<InternedStr, Vec<TypeConstraint>>,
    /// ExprId → 型制約リスト
    pub expr_constraints: HashMap<ExprId, Vec<TypeConstraint>>,
    /// 戻り値の型制約
    pub return_constraints: Vec<TypeConstraint>,
    /// ExprId → パラメータ名のリンク
    pub expr_to_param: Vec<ParamLink>,
    /// パラメータ名 → ExprId リスト（逆引き）
    pub param_to_exprs: HashMap<InternedStr, Vec<ExprId>>,
}
```

#### TypeConstraint

```rust
pub struct TypeConstraint {
    pub expr_id: ExprId,     // 対象となる式の ID
    pub ty: TypeRepr,        // 構造化された型表現
    pub context: String,     // デバッグ用コンテキスト
}
```

### 4. type_repr.rs - 型表現

**責務**: 型情報の構造化表現（出所情報付き）

#### TypeRepr

```rust
pub enum TypeRepr {
    /// C 言語の型
    CType {
        specs: CTypeSpecs,           // 型指定子
        derived: Vec<CDerivedType>,  // ポインタ、配列など
        source: CTypeSource,         // 出所
    },
    /// Rust バインディングからの型
    RustType {
        repr: RustTypeRepr,          // 型表現
        source: RustTypeSource,      // 出所
    },
    /// 推論で導出
    Inferred(InferredType),
}
```

#### 主要メソッド

| メソッド | 役割 |
|----------|------|
| `from_apidoc_string()` | 簡易パーサーで型文字列を解析（フォールバック用） |
| `from_c_type_string()` | 完全な C パーサー（parser.rs）で型文字列を解析 |
| `from_type_name()` | パーサー出力の TypeName から TypeRepr を作成 |
| `is_void()` | void 型かどうかを判定（ポインタなしの純粋な void のみ true） |
| `pointee_name()` | ポインタ型の指示先の構造体/typedef 名を抽出（`*mut SV` → `SV`） |
| `type_name()` | 構造体/typedef/enum 名を抽出（`union _xhvnameu` → `_xhvnameu`） |

`from_c_type_string()` は `"COP* const"` のような複雑なパターンも正しく解析できる。
`from_apidoc_string()` は簡易パーサーを使用するため、一部のパターンで失敗する可能性がある。

#### 出所情報

```rust
pub enum CTypeSource {
    Header,                          // C ヘッダー
    Apidoc { raw: String },          // embed.fnc
    InlineFn { func_name: InternedStr }, // インライン関数
    Parser,                          // parser.rs の parse_type_from_string を使用
    FieldInference { field_name: InternedStr }, // フィールドアクセスからの逆推論
    Cast,                            // キャスト式の型名（AST から直接変換）
    SvFamilyCast,                    // SV ファミリーキャストからの型推論
}

pub enum RustTypeSource {
    FnParam { func_name: String, param_index: usize },
    FnReturn { func_name: String },
    Const { const_name: String },
    Parsed { raw: String },
}
```

#### InferredType

推論の根拠を保持:

```rust
pub enum InferredType {
    IntLiteral, UIntLiteral, FloatLiteral, CharLiteral, StringLiteral,
    SymbolLookup { name: InternedStr, resolved_type: Box<TypeRepr> },
    BinaryOp { op: BinOp, result_type: Box<TypeRepr> },
    MemberAccess { base_type: String, member: InternedStr, field_type: Option<Box<TypeRepr>> },
    PtrMemberAccess { ... },
    Cast { target_type: Box<TypeRepr> },
    // ...
}
```

### 5. unified_type.rs - 統一型表現

**責務**: C 型と Rust 型の相互変換

#### UnifiedType

```rust
pub enum UnifiedType {
    Void, Bool,
    Char { signed: Option<bool> },
    Int { signed: bool, size: IntSize },
    Float, Double, LongDouble,
    Pointer { inner: Box<UnifiedType>, is_const: bool },
    Array { inner: Box<UnifiedType>, size: Option<usize> },
    Named(String),
    Unknown,
}
```

主要機能:
- `from_c_str()` - C 型文字列からパース
- `from_rust_str()` - Rust 型文字列からパース
- `to_rust_string()` - Rust 型文字列に変換
- `equals_ignoring_const()` - const を無視した比較

## 型推論のフロー

### Phase 1: マクロ情報の収集 (build_macro_info)

```
1. マクロ定義を取得
2. Preprocessor.expand_macro_body_for_inference() でマクロ本体を展開
   - ExplicitExpandSymbols は展開する (SvANY, SvFLAGS 等)
   - その他の関数マクロは保存（関数呼び出しとして残す）
   - MacroBegin/MacroEnd マーカー付きで preserve_call フラグを設定
3. 展開結果をパース (式 or 文)
   - preserve_call=true のマーカーは MacroCall AST ノードに変換
   - wrapped マクロ (assert 等) は Assert AST ノードに変換
4. def-use 関係を収集（MacroCall の expanded から uses を収集）
5. THX 依存・トークン連結の検出
6. 関数呼び出しの収集
```

### Phase 2: 型制約の収集 (infer_macro_types)

```
1. SemanticAnalyzer を作成
2. 確定済みマクロの型キャッシュを設定（ネスト呼び出し型伝播用）
   - macro_return_types: マクロ名 → 戻り値型
   - macro_param_types: マクロ名 → [(パラメータ名, 型)]
3. apidoc からパラメータ型を登録
4. collect_expr_constraints() で式を走査
   - 各式に TypeConstraint を追加
   - パラメータ参照をリンク
   - ネストしたマクロ呼び出しの引数型を伝播
5. 戻り値型の制約を追加
```

### Phase 3: 依存順での型推論 (infer_types_in_dependency_order)

```
1. 全マクロを unconfirmed に分類
2. macro_return_type_cache, macro_param_type_cache を初期化
3. ループ:
   a. 候補を取得 (依存マクロが全て confirmed)
   b. 各候補に型推論を適用（キャッシュを参照）
   c. 型が確定したら confirmed に移動し、キャッシュに型情報を追加
   d. 確定できなければ unknown に移動
4. 残りの未確定マクロにも apidoc 情報を適用
```

### Phase 4: 関数可用性チェック (check_function_availability)

```
1. 各マクロの called_functions を確認
2. bindings.rs、inline 関数辞書、ビルトイン関数リストと照合
3. 利用不可関数がある場合 calls_unavailable = true
4. カスケード依存：calls_unavailable なマクロを呼ぶマクロも推移的に不可
```

## semantic.rs と macro_infer.rs の役割分担

| 観点 | semantic.rs | macro_infer.rs |
|------|-------------|----------------|
| 主な対象 | 式・文の型推論 | マクロ全体の解析 |
| スコープ | 管理する | 使わない |
| 依存関係 | 考慮しない | def-use 関係を管理 |
| 型情報源 | シンボルテーブル、辞書類 | apidoc、bindings.rs |
| 出力 | Type、TypeConstraint | MacroInferInfo |

### 連携の流れ

```
MacroInferContext.infer_macro_types()
    │
    ├─ SemanticAnalyzer を作成
    │
    ├─ analyzer.set_macro_return_types(cache)    ← 確定済みマクロ型キャッシュ
    ├─ analyzer.set_macro_param_types(cache)     ← パラメータ型キャッシュ
    │
    ├─ analyzer.register_macro_params_from_apidoc()
    │   └─ パラメータをシンボルテーブルに登録
    │
    ├─ analyzer.collect_expr_constraints(expr, type_env)
    │   ├─ 式を走査して型制約を MacroInferInfo.type_env に追加
    │   └─ ネストしたマクロ呼び出しの型をキャッシュから伝播
    │
    └─ info.get_return_type()
        └─ 戻り値型を取得
```

## 型情報の優先順位

1. **bindings.rs** (RustDeclDict) - 最も信頼性が高い
2. **apidoc** (embed.fnc) - Perl 公式ドキュメント
3. **inline 関数の AST** - C ヘッダーからパース
4. **推論** - 式の構造から導出

## 関連ファイル

| ファイル | 役割 |
|----------|------|
| `src/macro_infer.rs` | マクロ解析エンジン |
| `src/semantic.rs` | 意味解析・型推論 |
| `src/type_env.rs` | 型制約管理 |
| `src/type_repr.rs` | 型表現（出所情報付き） |
| `src/unified_type.rs` | C/Rust 統一型 |
| `src/infer_api.rs` | 推論 API・パイプライン |
| `src/apidoc.rs` | embed.fnc パーサー |
| `src/rust_decl.rs` | bindings.rs パーサー |
| `src/fields_dict.rs` | フィールド型辞書 |
| `src/inline_fn.rs` | インライン関数辞書 |
