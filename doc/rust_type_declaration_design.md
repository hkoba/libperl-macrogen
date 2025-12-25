# Rust 型宣言解析 設計書

## 概要

TinyCC の型宣言解析を参考に、Rust での実装設計を定める。

## 型の表現 (`ast.rs`)

### 基本型

```rust
/// 基本型
#[derive(Debug, Clone, PartialEq)]
pub enum BaseType {
    Void,
    Bool,
    Char,
    Short,
    Int,
    Long,
    LongLong,
    Float,
    Double,
    LongDouble,
    Struct(InternedStr),        // 構造体名
    Union(InternedStr),         // 共用体名
    Enum(InternedStr),          // 列挙型名
    TypedefName(InternedStr),   // typedef名
}
```

### 型修飾子

```rust
/// 型修飾子
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TypeQualifiers {
    pub is_const: bool,
    pub is_volatile: bool,
    pub is_restrict: bool,
    pub is_atomic: bool,
}

/// 符号修飾子
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Signedness {
    Signed,
    Unsigned,
    Default,  // 明示的指定なし
}

/// ストレージクラス
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StorageClass {
    None,
    Extern,
    Static,
    Typedef,
    Register,
    Auto,
}
```

### 派生型

```rust
/// 派生型（ポインタ、配列、関数）
#[derive(Debug, Clone, PartialEq)]
pub enum DerivedType {
    Pointer {
        qualifiers: TypeQualifiers,
    },
    Array {
        size: Option<Box<Expr>>,  // None = 不完全配列 []
    },
    Function {
        params: Vec<ParamDecl>,
        is_variadic: bool,
    },
}
```

### 完全な型

```rust
/// 完全な型
#[derive(Debug, Clone, PartialEq)]
pub struct CType {
    pub base: BaseType,
    pub signedness: Signedness,
    pub qualifiers: TypeQualifiers,
    pub storage: StorageClass,
    pub is_inline: bool,
    pub derived: Vec<DerivedType>,  // 外側から内側へ
}
```

### 宣言関連

```rust
/// 関数パラメータ
#[derive(Debug, Clone, PartialEq)]
pub struct ParamDecl {
    pub ty: CType,
    pub name: Option<InternedStr>,  // 抽象宣言子の場合はNone
}

/// 宣言子（識別子と型の組み合わせ）
#[derive(Debug, Clone, PartialEq)]
pub struct Declarator {
    pub name: Option<InternedStr>,  // 抽象宣言子の場合はNone
    pub ty: CType,
}

/// 宣言
#[derive(Debug, Clone, PartialEq)]
pub enum Declaration {
    Variable {
        declarator: Declarator,
        initializer: Option<Initializer>,
    },
    Function {
        declarator: Declarator,
        body: Option<CompoundStmt>,  // None = プロトタイプ宣言
    },
    Typedef {
        declarator: Declarator,
    },
}
```

## パーサー関数 (`parser.rs`)

### 関数シグネチャ

```rust
impl<'a> Parser<'a> {
    /// 基本型を解析 (TinyCC: parse_btype)
    /// 戻り値: 型が見つかった場合はSome、なければNone
    fn parse_base_type(&mut self) -> Result<Option<CType>, CompileError>;

    /// 宣言子を解析 (TinyCC: type_decl)
    /// base_type に対してポインタ・配列・関数修飾を適用
    fn parse_declarator(
        &mut self,
        base_type: CType,
        allow_abstract: bool,  // 抽象宣言子を許可するか
    ) -> Result<Declarator, CompileError>;

    /// 後置修飾子を解析 (TinyCC: post_type)
    /// 配列 [] と関数 () を解析
    fn parse_post_type(&mut self, ty: &mut CType) -> Result<(), CompileError>;

    /// 宣言を解析（変数宣言または関数定義）
    pub fn parse_declaration(&mut self) -> Result<Vec<Declaration>, CompileError>;
}
```

## TinyCC との対応関係

| TinyCC 関数 | Rust 関数 | 説明 |
|-------------|-----------|------|
| `parse_btype()` | `parse_base_type()` | 基本型の解析 |
| `type_decl()` | `parse_declarator()` | 宣言子の解析 |
| `post_type()` | `parse_post_type()` | 後置修飾子の解析 |
| `decl()` | `parse_declaration()` | 宣言全体の解析 |

## 解析フロー

```
parse_declaration()
    │
    ├─► parse_base_type()
    │       └─► int, char, struct X, ... を解析
    │
    └─► parse_declarator(base_type)
            │
            ├─► ポインタ '*' を解析
            │
            ├─► 括弧 '(' による入れ子を解析
            │       └─► parse_declarator() 再帰呼び出し
            │
            ├─► 識別子を解析
            │
            └─► parse_post_type()
                    ├─► 配列 '[' size ']' を解析
                    └─► 関数 '(' params ')' を解析
```

## 型の内部表現例

### `int *p`

```rust
CType {
    base: BaseType::Int,
    signedness: Signedness::Default,
    qualifiers: TypeQualifiers::default(),
    storage: StorageClass::None,
    is_inline: false,
    derived: vec![
        DerivedType::Pointer { qualifiers: TypeQualifiers::default() },
    ],
}
```

### `const int * volatile p`

```rust
CType {
    base: BaseType::Int,
    signedness: Signedness::Default,
    qualifiers: TypeQualifiers { is_const: true, ..default() },
    storage: StorageClass::None,
    is_inline: false,
    derived: vec![
        DerivedType::Pointer {
            qualifiers: TypeQualifiers { is_volatile: true, ..default() }
        },
    ],
}
```

### `int arr[10]`

```rust
CType {
    base: BaseType::Int,
    signedness: Signedness::Default,
    qualifiers: TypeQualifiers::default(),
    storage: StorageClass::None,
    is_inline: false,
    derived: vec![
        DerivedType::Array { size: Some(Box::new(Expr::IntLit(10))) },
    ],
}
```

### `int (*fp)(int, int)`

```rust
CType {
    base: BaseType::Int,
    signedness: Signedness::Default,
    qualifiers: TypeQualifiers::default(),
    storage: StorageClass::None,
    is_inline: false,
    derived: vec![
        DerivedType::Function {
            params: vec![
                ParamDecl { ty: int_type(), name: None },
                ParamDecl { ty: int_type(), name: None },
            ],
            is_variadic: false,
        },
        DerivedType::Pointer { qualifiers: TypeQualifiers::default() },
    ],
}
```

## 実装順序

1. `ast.rs` に型定義を追加
2. `parser.rs` に `parse_base_type()` を実装
3. `parser.rs` に `parse_declarator()` を実装
4. `parser.rs` に `parse_post_type()` を実装
5. `parser.rs` に `parse_declaration()` を実装
6. テストを追加
