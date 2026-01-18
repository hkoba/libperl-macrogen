# 型推論結果からの Rust コード生成

## 目標

`run_inference_with_preprocessor()` の結果（`InferResult`）を使って、
is_target な C のマクロ関数と inline 関数に対して Rust 関数を生成する。

## 要件

1. **出力の分離**: inline 関数とマクロ関数を分けて出力
2. **名前順ソート**: それぞれの出力は名前順
3. **パース失敗時**: コメント形式で名前と S 式を出力
4. **型推論失敗時**: コメント形式で名前と型付 S 式を出力

## 入力データ

`InferResult` から以下を使用:

```rust
pub struct InferResult {
    /// マクロ推論コンテキスト（全マクロの解析結果）
    pub infer_ctx: MacroInferContext,
    /// インライン関数辞書
    pub inline_fn_dict: InlineFnDict,
    /// プリプロセッサ（StringInterner, FileRegistry へのアクセス）
    pub preprocessor: Preprocessor,
    // ...
}
```

### マクロ情報 (MacroInferInfo)

```rust
pub struct MacroInferInfo {
    pub name: InternedStr,
    pub is_target: bool,
    pub is_function: bool,           // 関数形式マクロ
    pub is_thx_dependent: bool,      // THX 依存
    pub params: Vec<MacroParam>,     // パラメータリスト
    pub parse_result: ParseResult,   // パース結果
    pub type_env: TypeEnv,           // 型制約
    pub args_infer_status: InferStatus,
    pub return_infer_status: InferStatus,
}

pub enum ParseResult {
    Expression(Box<Expr>),
    Statement(Vec<BlockItem>),
    Unparseable(Option<String>),
}

pub enum InferStatus {
    Pending,
    TypeComplete,
    TypeIncomplete,
    TypeUnknown,
}
```

### inline 関数情報 (InlineFnDict)

```rust
pub struct InlineFnDict {
    fns: HashMap<InternedStr, FunctionDef>,
}

pub struct FunctionDef {
    pub specs: DeclSpecs,      // 戻り値型など
    pub declarator: Declarator, // 関数名、パラメータ
    pub body: CompoundStmt,     // 関数本体
    pub is_target: bool,
}
```

## 出力形式

### 成功時（マクロ）

```rust
/// SvREFCNT(sv) - Get reference count
#[inline]
pub unsafe fn SvREFCNT(sv: *mut SV) -> U32 {
    // マクロ本体のRust変換
    (*sv).sv_refcnt
}
```

### 成功時（inline 関数）

```rust
/// Perl_sv_2pv - inline function
#[inline]
pub unsafe fn Perl_sv_2pv(my_perl: *mut PerlInterpreter, sv: *mut SV, lp: *mut STRLEN) -> *mut c_char {
    // 関数本体のRust変換
}
```

### パース失敗時

```rust
// [PARSE_FAILED] SOME_MACRO
// S-expression:
//   (tokens "FOO" "##" "BAR")
```

### 型推論失敗時

```rust
// [TYPE_INCOMPLETE] SomeFunc(a, b)
// Typed S-expression:
//   (call
//     (ident SomeFunc) :type <unknown>
//     (ident a) :type <unknown>
//     (ident b) :type int)
```

## 設計

### 新規モジュール: `src/rust_codegen.rs`

```rust
//! Rust コード生成モジュール
//!
//! 型推論結果から Rust コードを生成する。

use std::io::Write;

use crate::infer_api::InferResult;
use crate::intern::StringInterner;

/// コード生成の設定
pub struct CodegenConfig {
    /// inline 関数を出力するか
    pub emit_inline_fns: bool,
    /// マクロを出力するか
    pub emit_macros: bool,
    /// コメントにソース位置を含めるか
    pub include_source_location: bool,
}

/// コード生成器
pub struct RustCodegen<'a, W: Write> {
    writer: W,
    interner: &'a StringInterner,
    config: CodegenConfig,
}

impl<'a, W: Write> RustCodegen<'a, W> {
    pub fn new(writer: W, interner: &'a StringInterner, config: CodegenConfig) -> Self;

    /// 全体を生成
    pub fn generate(&mut self, result: &InferResult) -> std::io::Result<()>;

    /// inline 関数セクションを生成
    pub fn generate_inline_fns(&mut self, result: &InferResult) -> std::io::Result<()>;

    /// マクロセクションを生成
    pub fn generate_macros(&mut self, result: &InferResult) -> std::io::Result<()>;
}

/// 生成ステータス
pub enum GenerateStatus {
    /// 正常生成
    Success,
    /// パース失敗（S式をコメント出力）
    ParseFailed,
    /// 型推論不完全（型付S式をコメント出力）
    TypeIncomplete,
}
```

### コード生成の判定ロジック

```rust
fn should_generate_macro(info: &MacroInferInfo) -> GenerateStatus {
    // 1. ターゲットでなければスキップ
    if !info.is_target {
        return GenerateStatus::Skip;
    }

    // 2. 関数形式マクロまたは THX 依存オブジェクトマクロ
    if !info.is_function && !info.is_thx_dependent {
        return GenerateStatus::Skip;
    }

    // 3. パース結果をチェック
    match &info.parse_result {
        ParseResult::Unparseable(_) => GenerateStatus::ParseFailed,
        ParseResult::Expression(_) | ParseResult::Statement(_) => {
            // 4. 型推論状態をチェック
            if info.is_fully_confirmed() {
                GenerateStatus::Success
            } else {
                GenerateStatus::TypeIncomplete
            }
        }
    }
}
```

### 式から Rust コードへの変換

```rust
/// 式を Rust コードに変換
fn expr_to_rust(expr: &Expr, type_env: &TypeEnv, interner: &StringInterner) -> String {
    match &expr.kind {
        ExprKind::Ident(name) => interner.get(*name).to_string(),
        ExprKind::IntLit(n) => n.to_string(),
        ExprKind::Binary { op, lhs, rhs } => {
            let l = expr_to_rust(lhs, type_env, interner);
            let r = expr_to_rust(rhs, type_env, interner);
            format!("({} {} {})", l, op_to_rust(*op), r)
        }
        ExprKind::Call { func, args } => {
            let f = expr_to_rust(func, type_env, interner);
            let a: Vec<_> = args.iter()
                .map(|a| expr_to_rust(a, type_env, interner))
                .collect();
            format!("{}({})", f, a.join(", "))
        }
        ExprKind::Member { expr, member } => {
            let e = expr_to_rust(expr, type_env, interner);
            let m = interner.get(*member);
            format!("({}).{}", e, m)
        }
        ExprKind::PtrMember { expr, member } => {
            let e = expr_to_rust(expr, type_env, interner);
            let m = interner.get(*member);
            format!("(*{}).{}", e, m)
        }
        ExprKind::Cast { type_name, expr } => {
            let e = expr_to_rust(expr, type_env, interner);
            let t = type_name_to_rust(type_name, interner);
            format!("({} as {})", e, t)
        }
        ExprKind::Deref(inner) => {
            let e = expr_to_rust(inner, type_env, interner);
            format!("(*{})", e)
        }
        ExprKind::AddrOf(inner) => {
            let e = expr_to_rust(inner, type_env, interner);
            format!("(&mut {})", e)  // Rust では基本 &mut
        }
        // ... 他の式種別
        _ => format!("/* TODO: {:?} */", expr.kind),
    }
}
```

### C 型から Rust 型への変換

既存の `type_repr.rs` を活用:

```rust
fn c_type_to_rust(type_repr: &TypeRepr, interner: &StringInterner) -> String {
    match type_repr {
        TypeRepr::CType { specs, derived, .. } => {
            let base = c_specs_to_rust(specs, interner);
            apply_derived_to_rust(&base, derived)
        }
        TypeRepr::RustType { repr, .. } => {
            rust_repr_to_string(repr)
        }
        TypeRepr::Inferred(inferred) => {
            inferred_to_rust(inferred, interner)
        }
    }
}

fn c_specs_to_rust(specs: &CTypeSpecs, interner: &StringInterner) -> String {
    match specs {
        CTypeSpecs::Void => "c_void".to_string(),
        CTypeSpecs::Char { signed: Some(false) } => "c_uchar".to_string(),
        CTypeSpecs::Char { .. } => "c_char".to_string(),
        CTypeSpecs::Int { signed: true, size: IntSize::Int } => "c_int".to_string(),
        CTypeSpecs::Int { signed: false, size: IntSize::Int } => "c_uint".to_string(),
        CTypeSpecs::Int { signed: true, size: IntSize::Long } => "c_long".to_string(),
        CTypeSpecs::Int { signed: false, size: IntSize::Long } => "c_ulong".to_string(),
        CTypeSpecs::TypedefName(name) => interner.get(*name).to_string(),
        CTypeSpecs::Struct { name: Some(n), .. } => interner.get(*n).to_string(),
        // ...
    }
}

fn apply_derived_to_rust(base: &str, derived: &[CDerivedType]) -> String {
    let mut result = base.to_string();
    for d in derived.iter().rev() {
        match d {
            CDerivedType::Pointer { is_const: true, .. } => {
                result = format!("*const {}", result);
            }
            CDerivedType::Pointer { .. } => {
                result = format!("*mut {}", result);
            }
            CDerivedType::Array { size: Some(n) } => {
                result = format!("[{}; {}]", result, n);
            }
            CDerivedType::Array { size: None } => {
                result = format!("*mut {}", result);  // VLA → ポインタ
            }
            // ...
        }
    }
    result
}
```

## 実装フェーズ

### Phase 1: 基盤構造

1. `src/rust_codegen.rs` を作成
2. `CodegenConfig`, `RustCodegen`, `GenerateStatus` を定義
3. `lib.rs` にモジュール追加

### Phase 2: マクロ出力（パース失敗/型推論失敗）

1. `generate_macros()` の骨格を実装
2. パース失敗時の S 式コメント出力
3. 型推論失敗時の型付 S 式コメント出力
4. 名前順ソート

### Phase 3: マクロ出力（成功ケース）

1. 式の Rust 変換 (`expr_to_rust`)
2. 関数シグネチャ生成
3. 型変換ロジック
4. THX 依存マクロの my_perl パラメータ追加

### Phase 4: inline 関数出力

1. `generate_inline_fns()` を実装
2. FunctionDef から関数シグネチャ抽出
3. 関数本体の Rust 変換
4. 名前順ソート

### Phase 5: main.rs への統合

1. CLI オプション `--gen-rust` を追加
2. `run_infer_macro_types()` から `RustCodegen` を呼び出す
3. 出力ファイル指定オプション

### Phase 6: テストと調整

1. 代表的なマクロでテスト
2. inline 関数でテスト
3. エッジケースの処理

## 出力例

```rust
// =============================================================================
// Inline Functions
// =============================================================================

/// Perl_SvREFCNT_inc - inline function from /usr/lib64/perl5/CORE/sv.h:1234
#[inline]
pub unsafe fn Perl_SvREFCNT_inc(sv: *mut SV) -> *mut SV {
    if !sv.is_null() {
        (*sv).sv_refcnt += 1;
    }
    sv
}

// =============================================================================
// Macro Functions
// =============================================================================

/// SvREFCNT(sv) - macro from /usr/lib64/perl5/CORE/sv.h:100
#[inline]
pub unsafe fn SvREFCNT(sv: *mut SV) -> U32 {
    (*sv).sv_refcnt
}

/// SvCUR(sv) - macro from /usr/lib64/perl5/CORE/sv.h:200
#[inline]
pub unsafe fn SvCUR(sv: *mut SV) -> STRLEN {
    (*(*sv).sv_u.svu_pv).xpv_cur
}

// [TYPE_INCOMPLETE] SOME_COMPLEX_MACRO(a, b)
// Args: a: <unknown>, b: int
// Return: <unknown>
// Typed S-expression:
//   (binary +
//     (ident a) :type <unknown>
//     (ident b) :type int) :type <unknown>

// [PARSE_FAILED] TOKEN_PASTE_MACRO
// Error: unexpected token after ##
// Tokens: FOO ## BAR
```

## 将来の拡張

1. **unsafe の最小化**: 純粋な計算マクロは unsafe なしで生成
2. **const fn 対応**: 定数式マクロは const fn として生成
3. **ドキュメント生成**: apidoc からのドキュメント抽出
4. **テスト生成**: マクロのテストコード自動生成
