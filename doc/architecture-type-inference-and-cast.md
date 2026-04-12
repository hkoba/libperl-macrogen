# 型推論とキャスト生成のアーキテクチャ

マクロ関数の引数・戻り値の型推論と、関数本体でのキャスト生成に関するロジックを解説する。

## 全体フロー

```
Phase 2a: 型制約の収集 (依存順)
  infer_types_in_dependency_order()
  ├─ マクロ本体を AST にパース
  ├─ SemanticAnalyzer で式の型制約を収集
  │   └─ collect_call_constraints(): 呼び出し先の引数型を制約として追加
  └─ 確定したマクロの型をキャッシュに保存

Phase 2b: const/mut・bool の確定 (依存順)
  resolve_param_and_return_types()
  ├─ 外部関数 (bindings.rs, inline) の const/bool 情報を収集
  ├─ 各マクロのパラメータが *mut 必須か判定
  └─ 結果を MacroInferInfo に格納

Phase 3: コード生成
  get_param_type()    → Tier ベースで最適な型を選択 + const/mut 調整
  get_return_type()   → bool override + void フォールバック
  expr_to_rust_ctx()  → 式のコード生成 + キャスト挿入
```

## Phase 2a: 型制約の収集

### 依存順処理の仕組み

`infer_types_in_dependency_order()` (macro_infer.rs) は、
マクロ間の def-use 関係に基づくトポロジカル順序で型推論を実行する。

```
SvFLAGS(sv) → 他のマクロに依存しないリーフマクロ → 最初に処理
SvTYPE(sv) → SvFLAGS を使う → SvFLAGS 確定後に処理
isGV(sv) → SvTYPE を使う → SvTYPE 確定後に処理
```

各マクロについて:
1. `infer_macro_types()` で `SemanticAnalyzer` を実行し、型制約を収集
2. `collect_call_constraints()` で呼び出し先の型情報を制約として追加
3. 型が確定したら `param_types_cache` と `return_types_cache` に保存
4. 後続のマクロ解析で、キャッシュ経由で確定済みの型が伝播される

### 型制約の伝播例

```
マクロ: sv_dup_inc(a, b) → Perl_sv_dup_inc(aTHX_ a, b)

collect_call_constraints() が Perl_sv_dup_inc の bindings を参照:
  param[0] = my_perl: *mut PerlInterpreter → (自動挿入、無視)
  param[1] = ssv: *const SV               → a に *const SV 制約
  param[2] = param: *mut CLONE_PARAMS      → b に *mut CLONE_PARAMS 制約

これにより sv_dup_inc(a, b) のパラメータ型が推論される:
  a: *const SV, b: *mut CLONE_PARAMS
```

### 型制約の優先順位 (Confidence Tier)

同一パラメータに複数の型制約が付く場合がある。
`TypeRepr::confidence_tier()` (type_repr.rs) で優先順位を決定:

| Tier | 情報源 | 変更可能性 |
|------|--------|-----------|
| 1 | bindings.rs (FnParam, FnReturn, Const) | 変更不可 |
| 2 | C ヘッダー宣言 (InlineFn, Header) | 変更不可 |
| 3 | apidoc (embed.fnc), Parsed | 参考情報 |
| 4 | 推論 (Cast, SvFamilyCast, FieldInference) | 変更可能 |

`get_param_type()` と `get_return_type()` は全制約を走査し、
最小の Tier (最高の確度) を持つ制約を採用する。

### 型の具体性 (Specificity)

Tier に加えて、型の**内容の精度**も選択に影響する。
同 Tier の制約が複数ある場合、より具体的な型を優先する。

```
具体性: 高 ← *mut CV, *mut SV, *mut c_char (具体的なポインタ)
具体性: 低 ← *mut c_void              (汎用ポインタ)
具体性: 無 ← ()                        (void)
```

**適用箇所**: 条件式 (`cond ? A : B`) の型推論。
一方が `NULL` (= `void*`) なら、他方の具体的な型を結果型とする。

```
C:    gp_cvgen ? NULL : gp_cv
then: *mut c_void (NULL)
else: *mut CV     (gp_cv フィールド)
結果: *mut CV     (具体的な方を優先)
```

`UnifiedType::is_void_pointer()` / `is_concrete_pointer()` で判定。

## Phase 2b: const/mut と bool の確定

### const/mut ポインタ推論

`resolve_param_and_return_types()` (macro_infer.rs) は依存順で処理し、
各マクロのポインタパラメータが `*const` で済むか `*mut` 必須かを判定する。

**must-mut 判定** (`collect_must_mut_pointer_params()` in rust_codegen.rs):

以下のいずれかに該当するパラメータは `*mut` 必須:
- ポインタ経由の書き込み: `*param = ...`, `param->field = ...`
- `*mut` を要求する関数への引数渡し（呼び出し先が `callee_const_params` に未登録）
- 非 const ポインタへのキャスト: `(SomeType *)param`
- lvalue 文脈での関数呼び出し: `++func(param)`

**外部関数の const 情報** (`seed_callee_const()`):
- bindings.rs: `param.ty` に `*const` を含むかチェック（スペース正規化付き）
- inline 関数: `DerivedDecl::Pointer(quals.is_const)` をチェック
- 全関数名を `interner.intern()` で確実に登録（`lookup` だと未登録でスキップされる）

**結果の格納**: `MacroInferInfo.const_pointer_positions: HashSet<usize>`

### bool 戻り値推論

`is_boolean_expr_with_context()` (rust_codegen.rs) で本体式が bool かを判定:
- 比較式 (`==`, `!=`, `<` 等), 論理式 (`&&`, `||`), 否定 (`!`)
- `bool_return_macros` / `bool_return_externals` に含まれる関数の呼び出し

依存順で処理するため、リーフマクロの bool 判定が上流マクロに伝播する。

**結果の格納**: `MacroInferInfo.is_bool_return: bool`

## Phase 3: コード生成とキャスト挿入

### パラメータ型の最終決定

`get_param_type()` (rust_codegen.rs):

```
1. ジェネリック型パラメータチェック → "T" 等を返す
2. リテラル文字列パラメータチェック → "&str" を返す
3. 全制約を Tier 順で走査、最高 Tier の型を採用
4. const/mut 調整:
   - const_pointer_positions に含まれる → make_outer_pointer_const()
   - 含まれない＋ポインタ型 → make_outer_pointer_mut()
5. type_repr_to_rust() で Rust 型文字列に変換
```

### 戻り値型の最終決定

`get_return_type()` (rust_codegen.rs):

```
1. ジェネリック戻り値型チェック
2. is_bool_return → "bool" を返す
3. info.get_return_type() から TypeRepr 取得
   - "()" の場合: infer_expr_type() でフォールバック
   - 推論結果で *const を検出 → 戻り値型も *const に変更
4. unknown_marker() → "/* unknown */"
```

### 関数引数のキャスト挿入

`cast_integer_arg_if_needed()` (rust_codegen.rs):

```
actual_ty (推論) vs expected_ty (呼び出し先) を比較:

1. 整数型の幅不一致 → "arg as target_type"
2. SV subtype キャスト (GV→SV, HV→SV 等):
   - is_sv_subtype_cast() で判定
   - actual が *const → expected を *const に変換して cast
3. SV subtype フォールバック:
   - actual 不明でも expected が SV 系ポインタなら cast 試行
4. ※ const→mut キャストは安全でないため行わない
```

### 戻り値のキャスト挿入

`cast_return_expr_if_needed()` (rust_codegen.rs):

```
current_return_type vs infer_expr_type(expr) を比較:

1. 整数型の幅不一致 → "(expr as target_type)"
2. ※ ポインタの const→mut キャストは行わない
```

### 式レベルのキャスト生成

`expr_to_rust_ctx()` / `expr_to_rust_inline_ctx()` の Cast ハンドラ:

```rust
ExprKind::Cast { type_name, expr: inner } => {
    // 内部式を CastInner コンテキストで生成（Binary に括弧を強制）
    let e = self.expr_to_rust_ctx(inner, info, ExprContext::CastInner);
    let t = self.type_name_to_rust(type_name);

    if t == "()"    → "{ expr; }"           // void キャスト → 値を捨てる
    if t == "bool"  → "expr != 0" or "!expr.is_null()"  // bool 変換
    if enum target  → "std::mem::transmute::<_, T>(expr)" // enum キャスト
    if Top context  → "expr as T"           // 括弧不要
    else            → "(expr as T)"         // 括弧必要
}
```

### C の const セマンティクス

`type_name_to_type_str_readonly()` と `apply_simple_derived_with_specs_const()`:

```
C: const SV *p  → specs.qualifiers.is_const = true → *const SV
C: SV * const p → Pointer(quals.is_const = true)  → *mut SV (let で不変性)
```

`DerivedDecl::Pointer(quals)` の `is_const` はポインタ自体の const（再代入不可）で、
pointee の const ではない。pointee の const は `specs.qualifiers.is_const` で表現される。

## 括弧制御

### 2 層構造

括弧制御は文字列ベースとAST ベースの 2 層で行われる:

**層 1: ExprContext（文字列ベース、既存）**

```rust
enum ExprContext {
    Top,        // 括弧不要 (関数引数、let RHS、return 値、代入 RHS)
    Default,    // 括弧が必要な可能性がある位置
}
```

`expr_to_rust_ctx()` 内で式を文字列生成する際に使用。
Cast 式は `Top` で括弧なし、`Default` で括弧あり。
Binary 式は常に括弧あり（演算子優先順位の安全側）。

**層 2: normalize_parens（syn::Expr ベース、Phase 4 で追加）**

出力ポイント（ブロック末尾値、return 文、関数引数）で、
`normalize_parens()` が文字列ベースの過剰な括弧を正規化する:

```
expr_to_rust_ctx() → "((*sv).sv_flags as u32)"   [ExprContext::Default]
normalize_parens() → "(*sv).sv_flags as u32"      [syn ベースで最適化]
```

`normalize_parens` は syn::parse_str でパースし、全 Paren ノードを除去した後、
`parenthesize()` で優先順位に基づく括弧のみを再挿入する。
詳細は `architecture-rust-codegen.md` の「syn::Expr ベースの括弧正規化」を参照。

### strip_outer_parens

文字列レベルで最外の括弧を除去するヘルパー。
代入 RHS、`assert!` 引数、`if` 条件などの**式内部**の文脈で適用。
出力ポイントでは `normalize_parens` に置き換えられつつある。

## エラー検出とコメントアウト

### GenerateStatus による事前判定

```rust
enum GenerateStatus {
    Success,             // 正常生成
    ParseFailed,         // パース失敗
    TypeIncomplete,      // 型推論不完全
    CallsUnavailable,    // 利用不可関数を呼び出す
    ContainsGoto,        // goto を含む
    GenericUnsupported,  // ジェネリクス型パラメータ (as T 不可)
}
```

### codegen_errors による事後検出

`GeneratedCode.codegen_errors: Vec<String>` に記録:
- `"undefined type: PerlIO_funcs"` — 未定義型名
- `"cannot negate unsigned type: -(expr: usize)"` — unsigned への単項マイナス
- `"invalid lvalue: func() cannot be assigned to"` — 関数呼び出し結果への代入

### コメントアウトカテゴリ

| マーカー | 条件 | 件数目安 |
|---------|------|---------|
| `[PARSE_FAILED]` | AST パース失敗 | ~45 |
| `[TYPE_INCOMPLETE]` | 型推論不完全 | ~166 |
| `[CODEGEN_INCOMPLETE]` | 不完全マーカー含む | ~163 |
| `[CALLS_UNAVAILABLE]` | 利用不可関数呼び出し | ~359 |
| `[CASCADE_UNAVAILABLE]` | 依存先が生成失敗 | ~329 |
| `[UNRESOLVED_NAMES]` | 未解決シンボル | ~36 |
| `[GENERIC_UNSUPPORTED]` | ジェネリクス未対応 | ~25 |
| `[CODEGEN_ERROR]` | codegen エラー検出 | ~21 |
| `[CONTAINS_GOTO]` | goto 含む | 少数 |
