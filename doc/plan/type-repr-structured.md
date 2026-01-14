# TypeConstraint の型表現を構造化

## 目標

`TypeConstraint.ty: String` を、情報源ごとの構造化された型表現に置き換え、
文字列ベースの型比較を排除する。

## 現状の問題

```rust
// type_env.rs
pub struct TypeConstraint {
    pub ty: String,                    // ← 文字列
    pub source: ConstraintSource,      // ← 出所は別 enum
    // ...
}

pub enum ConstraintSource {
    CHeader, RustBindings, Apidoc, InlineFn, Inferred,
}
```

- `ty` が文字列なので、型比較が文字列比較になる
- C 型と Rust 型が混在（"int" vs "c_int"）
- 正規化が必要（"* mut" vs "*mut"）
- 構造的な型情報が失われている

## 設計方針

### TypeRepr: 出所と型表現を統合した enum

```rust
/// 型表現（出所情報を含む）
pub enum TypeRepr {
    /// C 言語の型（CHeader, Apidoc, InlineFn 共通）
    CType {
        /// 型指定子（int, char, struct X, など）
        specs: CTypeSpecs,
        /// 派生型（ポインタ、配列など）
        derived: Vec<CDerivedType>,
        /// 出所（デバッグ用）
        source: CTypeSource,
    },

    /// Rust バインディングからの型（syn::Type 由来）
    RustType {
        repr: RustTypeRepr,
        /// 出所（関数名など）
        source: RustTypeSource,
    },

    /// 推論で導出
    Inferred(InferredType),
}

/// C 型の出所
#[derive(Debug, Clone)]
pub enum CTypeSource {
    /// C ヘッダーのパース結果
    Header,
    /// apidoc（embed.fnc 等）- 元の文字列を保持
    Apidoc { raw: String },
    /// inline 関数の AST
    InlineFn { func_name: InternedStr },
}

/// Rust 型の出所
#[derive(Debug, Clone)]
pub enum RustTypeSource {
    /// bindings.rs の関数引数
    FnParam { func_name: String, param_index: usize },
    /// bindings.rs の関数戻り値
    FnReturn { func_name: String },
    /// bindings.rs の定数
    Const { const_name: String },
}
```

### C 型の構造化表現

```rust
/// C 型指定子（DeclSpecs から抽出）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CTypeSpecs {
    Void,
    Char { signed: Option<bool> },  // None = plain char
    Int { signed: bool, size: IntSize },
    Float,
    Double { is_long: bool },
    Bool,
    /// 構造体/共用体
    Struct { name: Option<InternedStr>, is_union: bool },
    /// enum
    Enum { name: Option<InternedStr> },
    /// typedef 名
    TypedefName(InternedStr),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntSize {
    Short,    // short
    Int,      // int (default)
    Long,     // long
    LongLong, // long long
    Int128,   // __int128
}

/// C 派生型
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CDerivedType {
    Pointer { is_const: bool, is_volatile: bool, is_restrict: bool },
    Array { size: Option<usize> },
    Function { params: Vec<CTypeSpecs>, variadic: bool },
}
```

### Rust 型の構造化表現

```rust
/// Rust 型表現（syn::Type から変換）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RustTypeRepr {
    /// C互換基本型 (c_int, c_char, etc.)
    CPrimitive(CPrimitiveKind),
    /// Rust基本型 (i32, u64, bool, etc.)
    RustPrimitive(RustPrimitiveKind),
    /// ポインタ (*mut T, *const T)
    Pointer { inner: Box<RustTypeRepr>, is_const: bool },
    /// 参照 (&T, &mut T)
    Reference { inner: Box<RustTypeRepr>, is_mut: bool },
    /// 名前付き型 (SV, AV, PerlInterpreter, etc.)
    Named(String),
    /// Option<T>
    Option(Box<RustTypeRepr>),
    /// 関数ポインタ
    FnPointer {
        params: Vec<RustTypeRepr>,
        ret: Option<Box<RustTypeRepr>>,
    },
    /// ユニット ()
    Unit,
    /// パース不能だった型（文字列で保持）
    Unknown(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CPrimitiveKind {
    CChar, CSchar, CUchar,
    CShort, CUshort,
    CInt, CUint,
    CLong, CUlong,
    CLongLong, CUlongLong,
    CFloat, CDouble,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RustPrimitiveKind {
    I8, I16, I32, I64, I128, Isize,
    U8, U16, U32, U64, U128, Usize,
    F32, F64,
    Bool,
}
```

### 推論の根拠 (InferredType)

semantic.rs の推論パターンを分析した結果に基づく設計:

```rust
/// 推論で導出された型
#[derive(Debug, Clone)]
pub enum InferredType {
    // ==================== リテラル ====================
    /// 整数リテラル (42, 0x1F, etc.)
    IntLiteral,
    /// 符号なし整数リテラル (42u, etc.)
    UIntLiteral,
    /// 浮動小数点リテラル (3.14, etc.)
    FloatLiteral,
    /// 文字リテラル ('a')
    CharLiteral,
    /// 文字列リテラル ("hello")
    StringLiteral,

    // ==================== 識別子参照 ====================
    /// シンボルテーブルからの参照
    SymbolLookup {
        name: InternedStr,
        /// 解決された型（別の TypeRepr への参照）
        resolved_type: Box<TypeRepr>,
    },
    /// THX (my_perl) のデフォルト型
    ThxDefault,

    // ==================== 演算子 ====================
    /// 二項演算の結果
    BinaryOp {
        op: BinaryOp,
        /// 左右オペランドの型から計算された結果型
        result_type: Box<TypeRepr>,
    },
    /// 単項演算 (+, -, ~)
    UnaryArithmetic {
        /// 内部式の型をそのまま継承
        inner_type: Box<TypeRepr>,
    },
    /// 論理否定 (!)
    LogicalNot,  // 常に int
    /// アドレス取得 (&x)
    AddressOf {
        inner_type: Box<TypeRepr>,
    },
    /// 間接参照 (*p)
    Dereference {
        pointer_type: Box<TypeRepr>,
    },
    /// インクリメント/デクリメント (++, --)
    IncDec {
        inner_type: Box<TypeRepr>,
    },

    // ==================== メンバーアクセス ====================
    /// 直接メンバーアクセス (expr.member)
    MemberAccess {
        base_type: String,  // 構造体名
        member: InternedStr,
        /// 解決されたフィールド型
        field_type: Option<Box<TypeRepr>>,
    },
    /// ポインタメンバーアクセス (expr->member)
    PtrMemberAccess {
        base_type: String,  // ポインタの基底型名
        member: InternedStr,
        /// 解決されたフィールド型
        field_type: Option<Box<TypeRepr>>,
        /// 一致型を使用した場合（ベース型が不明時）
        used_consistent_type: bool,
    },

    // ==================== 配列/添字 ====================
    /// 配列添字 (arr[i])
    ArraySubscript {
        base_type: Box<TypeRepr>,
        /// 要素型
        element_type: Box<TypeRepr>,
    },

    // ==================== 条件・制御 ====================
    /// 条件演算子 (cond ? then : else)
    Conditional {
        then_type: Box<TypeRepr>,
        else_type: Box<TypeRepr>,
        /// 計算された共通型
        result_type: Box<TypeRepr>,
    },
    /// コンマ式 (a, b)
    Comma {
        /// 右辺の型
        rhs_type: Box<TypeRepr>,
    },
    /// 代入式 (a = b)
    Assignment {
        /// 左辺の型
        lhs_type: Box<TypeRepr>,
    },

    // ==================== 型操作 ====================
    /// キャスト式 ((type)expr)
    Cast {
        target_type: Box<TypeRepr>,
    },
    /// sizeof 式/型
    Sizeof,  // 常に unsigned long
    /// alignof
    Alignof, // 常に unsigned long
    /// 複合リテラル ((type){...})
    CompoundLiteral {
        type_name: Box<TypeRepr>,
    },

    // ==================== その他 ====================
    /// 文式 ({ ... })
    StmtExpr {
        /// 最後の式の型
        last_expr_type: Option<Box<TypeRepr>>,
    },
    /// アサーション
    Assert,  // 常に void
    /// 関数呼び出しの戻り値（RustBindings/Apidoc から取得できなかった場合）
    FunctionReturn {
        func_name: InternedStr,
    },
}
```

## 変更対象

### 1. 新規モジュール: `src/type_repr.rs`

上記の enum 定義と、以下の機能:

```rust
// AST からの変換
impl CTypeSpecs {
    pub fn from_decl_specs(specs: &DeclSpecs, interner: &StringInterner) -> Self;
}

impl CDerivedType {
    pub fn from_derived_decls(derived: &[DerivedDecl]) -> Vec<Self>;
}

// syn からの変換
impl RustTypeRepr {
    pub fn from_syn_type(ty: &syn::Type) -> Self;
    pub fn from_type_string(s: &str) -> Self;  // 既存文字列からのパース
}

// Apidoc 文字列からの変換
impl TypeRepr {
    pub fn from_apidoc_string(s: &str, interner: &StringInterner) -> Self;
}

// デバッグ用表示
impl std::fmt::Display for TypeRepr { ... }
impl std::fmt::Display for CTypeSpecs { ... }
impl std::fmt::Display for RustTypeRepr { ... }
```

### 2. `src/type_env.rs` の変更

```rust
// Before
pub struct TypeConstraint {
    pub expr_id: ExprId,
    pub ty: String,
    pub source: ConstraintSource,
    pub context: String,
}

// After
pub struct TypeConstraint {
    pub expr_id: ExprId,
    pub ty: TypeRepr,          // ← 構造化
    pub context: String,       // source は TypeRepr に統合
}
```

`ConstraintSource` は削除（TypeRepr の各バリアントに統合）

### 3. `src/semantic.rs` の変更

制約追加時に文字列ではなく `TypeRepr` を構築:

```rust
// Before
type_env.add_constraint(TypeEnvConstraint::new(
    expr.id, "int", ConstraintSource::Inferred, "integer literal"
));

// After
type_env.add_constraint(TypeEnvConstraint::new(
    expr.id,
    TypeRepr::Inferred(InferredType::IntLiteral),
    "integer literal",
));
```

### 4. 新規モジュール: `src/type_equiv.rs` (後で実装)

型の等価性・簡約判定ロジック:

```rust
/// 型の等価性を判定
pub fn types_equivalent(a: &TypeRepr, b: &TypeRepr) -> bool;

/// C 型と Rust 型が互換か判定
pub fn c_rust_compatible(c: &CTypeSpecs, r: &RustTypeRepr) -> bool;

/// 型を簡約（複数の制約から最も具体的な型を選択）
pub fn reduce_types(types: &[TypeRepr]) -> Option<TypeRepr>;
```

## 実装フェーズ

### Phase 1: 基盤 (今回)

1. `src/type_repr.rs` を作成
   - `CTypeSpecs`, `CDerivedType`, `CTypeSource` の定義
   - `RustTypeRepr`, `RustTypeSource` の定義
   - `InferredType` の定義（全バリアント）
   - `TypeRepr` enum の定義
   - 基本的な変換関数

2. `src/lib.rs` にモジュール追加・再エクスポート

### Phase 2: type_env の移行

1. `TypeConstraint.ty` を `String` から `TypeRepr` に変更
2. `ConstraintSource` を削除
3. `TypeConstraint::new()` のシグネチャ変更

### Phase 3: semantic.rs の移行

1. リテラル系の制約追加を移行
2. 演算子系の制約追加を移行
3. メンバーアクセス系の制約追加を移行
4. 関数呼び出し系の制約追加を移行

### Phase 4: 型等価性ロジック

1. `src/type_equiv.rs` を作成
2. `types_equivalent()` を実装
3. `c_rust_compatible()` を実装
4. 既存の文字列比較を置き換え

## 段階的移行のための互換性

移行期間中は以下のヘルパーを用意:

```rust
impl TypeRepr {
    /// 後方互換: 文字列に変換（デバッグ用）
    pub fn to_display_string(&self) -> String;

    /// 後方互換: 文字列から Inferred として作成
    #[deprecated]
    pub fn from_legacy_string(s: &str, source: ConstraintSource) -> Self {
        // ConstraintSource に応じて適切な TypeRepr を生成
        match source {
            ConstraintSource::CHeader => Self::CType { ... },
            ConstraintSource::RustBindings => Self::RustType { ... },
            ConstraintSource::Apidoc => Self::CType { source: CTypeSource::Apidoc { raw: s.to_string() }, ... },
            ConstraintSource::InlineFn => Self::CType { source: CTypeSource::InlineFn { ... }, ... },
            ConstraintSource::Inferred => Self::Inferred(InferredType::from_legacy_string(s)),
        }
    }
}
```

## 期待される効果

1. **文字列比較の排除**: 構造的な型比較が可能に
2. **情報源の保持**: C 型/Rust 型の元情報を保持
3. **C 型の統一表現**: Header/Apidoc/InlineFn を同じ構造で表現
4. **推論根拠の明示**: どのような推論で型が導出されたか追跡可能
5. **デバッグ改善**: 型の出所と推論過程が明確になる
6. **SV ファミリー検出の基盤**: 構造的な型比較により継承関係の検出が可能に
