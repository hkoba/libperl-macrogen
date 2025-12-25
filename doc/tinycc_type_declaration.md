# TinyCC 型宣言解析

## 概要

TinyCC (`tccgen.c`) における型宣言（type declaration）の解析方法を調査した結果をまとめる。

## 主要な関数

### 1. `parse_btype` (行 4734-5010)

**役割**: 基本型（base type）を解析する

```c
static int parse_btype(CType *type, AttributeDef *ad, int ignore_label)
```

**解析対象**:
- 基本型キーワード: `void`, `char`, `short`, `int`, `long`, `float`, `double`, `_Bool`
- 符号修飾子: `signed`, `unsigned`
- 型修飾子: `const`, `volatile`, `_Atomic`, `restrict`
- ストレージクラス: `extern`, `static`, `typedef`, `inline`, `register`, `auto`
- 複合型: `struct`, `union`, `enum`
- typedef 名 (既存の型名)
- GCC 拡張: `__attribute__`, `__extension__`

**処理の流れ**:
```
while(1) {
    switch(tok) {
        case TOK_CHAR:   → VT_BYTE
        case TOK_SHORT:  → VT_SHORT
        case TOK_INT:    → VT_INT
        case TOK_LONG:   → VT_LONG または VT_LLONG
        case TOK_FLOAT:  → VT_FLOAT
        case TOK_DOUBLE: → VT_DOUBLE
        case TOK_STRUCT: → struct_decl()
        case TOK_UNION:  → struct_decl()
        case TOK_ENUM:   → struct_decl()
        case TOK_CONST:  → VT_CONSTANT フラグ追加
        case TOK_VOLATILE: → VT_VOLATILE フラグ追加
        case TOK_EXTERN: → VT_EXTERN フラグ追加
        case TOK_STATIC: → VT_STATIC フラグ追加
        case TOK_TYPEDEF: → VT_TYPEDEF フラグ追加
        ...
    }
}
```

### 2. `type_decl` (行 5251-5324)

**役割**: 宣言子（declarator）を解析する（ポインタ、識別子、括弧）

```c
static CType *type_decl(CType *type, AttributeDef *ad, int *v, int td)
```

**引数**:
- `type`: 入力/出力の型情報
- `ad`: 属性情報
- `v`: 識別子のトークンID（出力）
- `td`: 宣言の種類フラグ (TYPE_DIRECT, TYPE_ABSTRACT, TYPE_PARAM)

**解析対象**:
1. ポインタ修飾: `*` と修飾子 (const, volatile, restrict, _Atomic)
2. 括弧による入れ子: `(` declarator `)`
3. 識別子: 変数名・関数名

**処理の流れ**:
```c
// 1. ポインタ解析
while (tok == '*') {
    qualifiers = 0;
    next();
    // const, volatile, restrict, _Atomic を収集
    mk_pointer(type);
    type->t |= qualifiers;
}

// 2. 括弧または識別子
if (tok == '(') {
    // 入れ子の宣言子、または関数パラメータリスト
    if (!post_type(...)) {
        // 入れ子: int (*p)[10] の (*p) 部分
        post = type_decl(type, ad, v, td);
        skip(')');
    }
} else if (tok >= TOK_IDENT && (td & TYPE_DIRECT)) {
    // 識別子
    *v = tok;
    next();
}

// 3. 後置修飾（配列、関数）
post_type(post, ad, storage, td);
```

### 3. `post_type` (行 5034-5248)

**役割**: 後置型修飾子（配列・関数）を解析する

```c
static int post_type(CType *type, AttributeDef *ad, int storage, int td)
```

**解析対象**:
1. 関数型: `(` パラメータリスト `)`
2. 配列型: `[` サイズ式 `]`

**関数型の処理** (行 5043-5133):
```c
if (tok == '(') {
    // パラメータリストを解析
    while (...) {
        parse_btype(&pt, &ad1, 0);  // パラメータの型
        type_decl(&pt, &ad1, &n, TYPE_DIRECT | TYPE_ABSTRACT | TYPE_PARAM);
    }
    type->t = VT_FUNC;
    type->ref = s;  // パラメータ情報を保持
}
```

**配列型の処理** (行 5134-5248):
```c
if (tok == '[') {
    // サイズ式を評価
    if (tok != ']') {
        n = expr_const();  // または VLA
    }
    // 再帰的に後続の配列次元を解析
    post_type(type, ad, storage, td | TYPE_NEST);

    // 配列型を構築
    s = sym_push(SYM_FIELD, type, 0, n);
    type->t = VT_ARRAY | VT_PTR;
    type->ref = s;
}
```

### 4. `decl` (行 8686-8900+)

**役割**: トップレベルの宣言を解析する

```c
static int decl(int l)
```

**処理の流れ**:
```c
while (1) {
    // 1. 基本型を解析
    if (!parse_btype(&btype, &adbase, l == VT_LOCAL)) {
        // 型がない場合の処理
    }

    // 2. 宣言子を解析（カンマ区切りで複数可）
    while (1) {
        type = btype;
        type_decl(&type, &ad, &v, TYPE_DIRECT);

        // 3. 関数定義または初期化子
        if ((type.t & VT_BTYPE) == VT_FUNC) {
            // 関数の処理
        }

        // 4. カンマまたはセミコロン
        if (tok == ',') {
            next();
        } else {
            break;
        }
    }
    skip(';');
}
```

## 型の表現 (CType 構造体)

```c
typedef struct CType {
    int t;           // 型フラグ（VT_* の組み合わせ）
    struct Sym *ref; // 参照情報（関数パラメータ、配列サイズ、struct定義など）
} CType;
```

### 基本型フラグ (VT_BTYPE マスク)

| 値 | 定数 | 説明 |
|----|------|------|
| 0 | VT_VOID | void |
| 1 | VT_BYTE | char (signed) |
| 2 | VT_SHORT | short |
| 3 | VT_INT | int |
| 4 | VT_LLONG | long long |
| 5 | VT_PTR | ポインタ |
| 6 | VT_FUNC | 関数 |
| 7 | VT_STRUCT | struct/union |
| 8 | VT_FLOAT | float |
| 9 | VT_DOUBLE | double |
| 10 | VT_LDOUBLE | long double |
| 11 | VT_BOOL | _Bool |

### 修飾子フラグ

| フラグ | 説明 |
|--------|------|
| VT_UNSIGNED (0x0010) | unsigned |
| VT_ARRAY (0x0040) | 配列（VT_PTR と併用） |
| VT_CONSTANT (0x0100) | const |
| VT_VOLATILE (0x0200) | volatile |
| VT_LONG (0x0800) | long |

### ストレージクラス

| フラグ | 説明 |
|--------|------|
| VT_EXTERN (0x1000) | extern |
| VT_STATIC (0x2000) | static |
| VT_TYPEDEF (0x4000) | typedef |
| VT_INLINE (0x8000) | inline |

## 宣言の解析例

### 例1: `int *p;`

```
decl()
  → parse_btype() → VT_INT
  → type_decl()
      → tok == '*' → mk_pointer() → VT_PTR
      → tok == IDENT "p" → v = "p"
```

### 例2: `int arr[10];`

```
decl()
  → parse_btype() → VT_INT
  → type_decl()
      → tok == IDENT "arr" → v = "arr"
      → post_type()
          → tok == '[' → n = 10 → VT_ARRAY | VT_PTR
```

### 例3: `int (*fp)(int, int);`

```
decl()
  → parse_btype() → VT_INT
  → type_decl()
      → tok == '*' → mk_pointer() → VT_PTR
      → tok == '(' → 入れ子
          → type_decl() → tok == IDENT "fp"
          → skip(')')
      → post_type()
          → tok == '(' → 関数パラメータ解析 → VT_FUNC
```

### 例4: `const int * volatile p;`

```
decl()
  → parse_btype() → VT_INT | VT_CONSTANT
  → type_decl()
      → tok == '*' → mk_pointer()
      → tok == TOK_VOLATILE → qualifiers |= VT_VOLATILE
      → type->t |= VT_VOLATILE (ポインタに適用)
      → tok == IDENT "p"
```

## Rust 実装への示唆

1. **型は2層構造**: 基本型 (`parse_btype`) と宣言子 (`type_decl`) を分離
2. **再帰的構造**: ポインタ・配列・関数は再帰的に処理
3. **参照による拡張**: 複雑な型は `ref` フィールドで追加情報を保持
4. **フラグベース**: 型情報はビットフラグで効率的に表現
