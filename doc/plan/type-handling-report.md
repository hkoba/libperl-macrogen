# 型の取り扱い現状レポートと記号処理的アプローチの考察

## 1. 現状概要

本プロジェクトには **3つの型表現システム** が並存している:

| システム | 定義場所 | ポインタ表現 | 主な用途 |
|---------|---------|------------|---------|
| TypeRepr | `src/type_repr.rs` | `CDerivedType::Pointer { is_const, .. }` / `RustTypeRepr::Pointer { inner, is_const }` | 型推論 (semantic.rs) |
| UnifiedType | `src/unified_type.rs` | `Pointer { inner, is_const }` | 設計済みだが限定的に使用 |
| String | `src/rust_decl.rs`, `src/rust_codegen.rs` | `"*mut SV"` / `"* mut SV"` | コード生成 (rust_codegen.rs) |

## 2. 各システムの詳細

### 2.1 TypeRepr (`src/type_repr.rs`)

最も構造化された型表現。出所情報 (CTypeSource / RustTypeSource) も保持する。

```
TypeRepr
├─ CType { specs: CTypeSpecs, derived: Vec<CDerivedType>, source }
│  ├─ CTypeSpecs: Void, Char, Int, Float, Double, Bool, Struct, Enum, TypedefName
│  └─ CDerivedType: Pointer { is_const, is_volatile, is_restrict }, Array, Function
├─ RustType { repr: RustTypeRepr, source }
│  └─ RustTypeRepr: CPrimitive, RustPrimitive, Pointer { inner, is_const }, Named, ...
└─ Inferred(InferredType)
   └─ InferredType: IntLiteral, FieldAccess, BinaryOp, ...
```

**使用箇所**: `TypeEnv` の `param_constraints` / `return_constraints` / `expr_constraints` で使用。
`semantic.rs::collect_expr_constraints` で制約収集、`macro_infer.rs` で型推論に利用。

**ポインタ判定**: `is_type_repr_pointer()` (rust_codegen.rs:343) で構造的にマッチ。

### 2.2 UnifiedType (`src/unified_type.rs`)

C型とRust型の両方を正規化して表現する中間型。

```
UnifiedType: Void, Bool, Char, Int, Float, Double, LongDouble,
             Pointer { inner, is_const }, Array, Named(String), Unknown
```

**パーサ**: `from_c_str()` (C文字列 → 構造型), `from_rust_str()` (Rust文字列 → 構造型)
- `from_rust_str()` は syn の `"* mut"` 問題を内部で正規化済み (L261-264)

**比較メソッド**: `equals_ignoring_const()`, `equals_ignoring_case()`

**現状**: 定義されているが、コード生成の中核パスではほとんど使われていない。

### 2.3 String ベース (`src/rust_decl.rs`, `src/rust_codegen.rs`)

**RustDeclDict** がバインディング情報を文字列で保持:
```rust
// rust_decl.rs:199
fn type_to_string(ty: &Type) -> String {
    ty.to_token_stream().to_string()  // syn の出力をそのまま
}
```

この文字列が以下の場所に格納される:
- `RustFn.params[].ty: String` — 関数引数の型
- `RustFn.return_type: Option<String>` — 戻り値の型
- `RustField.ty: String` — 構造体フィールドの型
- `field_type_map: HashMap<String, String>` — フィールド名→型の逆引き

## 3. 脆弱性の分析

### 3.1 syn 出力の不安定性 (根本問題)

syn の `to_token_stream().to_string()` は以下のように不安定な出力を生成する:

| 入力型 | 期待される出力 | 実際の出力 |
|-------|--------------|-----------|
| `*mut Stack_off_t` | `"*mut Stack_off_t"` | `"* mut Stack_off_t"` |
| `*const c_char` | `"*const c_char"` | `"* const c_char"` |
| `std::ffi::c_int` | `"std::ffi::c_int"` | `"std :: ffi :: c_int"` |

この問題に対する現在の対策:

1. **`is_pointer_type_str()`** (L372): 両方のフォーマットをチェック
   ```rust
   ty.starts_with("*mut ") || ty.starts_with("*const ")
       || ty.starts_with("* mut ") || ty.starts_with("* const ")
   ```

2. **`normalize_type_str()`** (L633): 正規化関数
   ```rust
   ty.replace("* mut ", "*mut ").replace("* const ", "*const ")
     .replace(":: ", "::").replace(" ::", "::")
   ```

3. **`build_field_type_map()`** (L603): フィールド型をビルド時に正規化

### 3.2 文字列比較の脆弱ポイント一覧

`is_pointer_type_str()` の呼び出し箇所 (14箇所):

| 行 | コンテキスト | 用途 |
|----|------------|------|
| 1020 | `infer_type_hint` MacroCall | マクロ戻り値のポインタ判定 |
| 1058 | `infer_type_hint` Call | 関数戻り値のポインタ判定 |
| 1081 | `infer_type_hint` PtrMember/Member | フィールド deref 後のポインタ判定 |
| 1091 | `infer_type_hint` PtrMember/Member | フィールド型のポインタ判定 |
| 1190 | `is_pointer_expr_inline` Call | inline関数内の関数戻り値ポインタ判定 |
| 1203 | `is_pointer_expr_inline` PtrMember/Member | inline関数内のフィールドポインタ判定 |
| 1209 | `is_pointer_expr_inline` PtrMember/Member | deref後のポインタ判定 |
| 1226 | `is_pointer_expr_inline` Call | 関数戻り値ポインタ判定 |
| 2694 | `expr_to_rust` Cast | null ポインタキャスト検出 |
| 2711 | `expr_to_rust` Cast (inline) | null ポインタキャスト検出 |
| 2734 | `expr_to_rust` Assignment | 代入時 null ポインタ検出 |
| 3206 | `expr_to_rust_inline` Assignment | inline関数代入時 null ポインタ検出 |

加えて `deref_type()` が 2箇所、`is_const_pointer_type_str()` が 2箇所で使用。

### 3.3 正規化されていない箇所

`normalize_type_str()` は `build_field_type_map()` でのみ適用。以下は未正規化:

- **`RustDeclDict` の関数引数型**: `get_callee_return_type()` が返す文字列
- **`infer_expr_type_str()` の戻り値**: `to_rust_string()` の出力
- **`current_param_types`**: inline関数パラメータ型 (String)

## 4. 記号処理的アプローチの考察

### 4.1 理想的なアーキテクチャ

文字列比較を完全に排除し、すべての型判定を構造的に行う:

```
bindings.rs パース
    ↓ syn::Type
    ↓ RustTypeRepr に変換 (文字列化しない)
    ↓
RustDeclDict に RustTypeRepr で保管
    ↓
codegen: is_pointer() → RustTypeRepr::Pointer をパターンマッチ
```

### 4.2 既存の基盤

このアプローチに使える既存コードが **すでに存在する**:

1. **`is_type_repr_pointer()`** (rust_codegen.rs:343)
   - TypeRepr の構造的ポインタ判定。CType, RustType, Inferred すべてに対応
   - 現在は `infer_type_hint` 内で TypeEnv の return_constraints にのみ使用

2. **`UnifiedType::from_rust_str()`** (unified_type.rs:260)
   - Rust型文字列を構造型にパース。syn の `"* mut"` 問題も内部処理済み
   - `is_pointer()` メソッドで構造的判定可能

3. **`RustTypeRepr::from_syn_type()`** (type_repr.rs 内、もし存在すれば)
   - syn::Type から直接 RustTypeRepr を構築する変換

### 4.3 実現可能な段階的移行

#### Phase 1: RustDeclDict の二重保持 (低リスク)

`RustDeclDict` に文字列と構造型の両方を保持:

```rust
// 現在
pub struct RustFn {
    pub params: Vec<RustParam>,  // RustParam { ty: String, .. }
    pub return_type: Option<String>,
}

// Phase 1: 追加
pub struct RustFn {
    pub params: Vec<RustParam>,
    pub return_type: Option<String>,
    pub param_types: Vec<UnifiedType>,       // 追加
    pub return_unified: Option<UnifiedType>, // 追加
}
```

利点: 既存コードを壊さずに、新しいコードは構造型を使える。

#### Phase 2: field_type_map の構造型化

```rust
// 現在
field_type_map: HashMap<String, String>

// Phase 2
field_type_map: HashMap<String, UnifiedType>
```

`is_pointer_type_str(ty)` → `ty.is_pointer()` に置換。

#### Phase 3: infer_type_hint の戻り値拡張

```rust
// 現在
enum TypeHint { Pointer, Integer, Bool, Unknown }

// Phase 3: より豊かな情報
enum TypeHint {
    Typed(UnifiedType),  // 完全な型情報
    Pointer,             // ポインタであることだけ分かる
    Integer,
    Bool,
    Unknown,
}
```

### 4.4 UnifiedType vs TypeRepr の選択

| 基準 | UnifiedType | TypeRepr |
|------|------------|---------|
| 複雑さ | シンプル (9 variant) | 複雑 (3大 variant × 各種sub-variant) |
| 出所情報 | 別途 `SourcedType` で保持 | 組み込み (CTypeSource/RustTypeSource) |
| ポインタ判定 | `is_pointer()` | `is_type_repr_pointer()` |
| from_rust_str | あり (syn対応済み) | なし (syn::Type から直接変換が必要) |
| 等値比較 | `PartialEq, Eq, Hash` derive済み | PartialEq なし (source 情報が異なるため) |
| コード生成との親和性 | 高い (シンプルで十分な情報) | やや過剰 (推論履歴は不要) |

**推奨**: コード生成パスには **UnifiedType** が適切。
TypeRepr は型推論エンジン (semantic.rs) に特化させるのが良い。

### 4.5 移行コスト vs 効果

**現在の脆弱箇所**: 約20箇所の文字列比較
**修正済みの workaround**: `normalize_type_str()` + 二重フォーマットチェック

**移行の利点**:
- syn バージョンアップへの耐性
- 新しい型パターン (参照、Option<*mut T> 等) への対応が容易
- `const` / non-const の判定が構造的に可能
- ポインタの deref が文字列操作でなくなる

**移行のリスク**:
- 既存の動作を壊す可能性 (特に文字列比較の微妙な挙動に依存しているケース)
- `UnifiedType::from_rust_str()` のパース精度がすべての型をカバーしているか要検証
- `to_rust_string()` 出力が必要な箇所 (生成コードへの型名出力) では文字列が引き続き必要

### 4.6 現実的な推奨

**短期 (現在の 5 件残存エラー解消)**:
- `normalize_type_str()` を `RustDeclDict` 構築時にも適用
- これだけで残存エラーの大半が解消する可能性が高い

**中期 (安定化)**:
- Phase 1 の二重保持を実装
- 新規コードは `UnifiedType` の構造的判定を使用
- `is_pointer_type_str()` の呼び出し元を段階的に移行

**長期 (完全移行)**:
- Phase 2, 3 を実施
- 文字列ベースの型比較を完全に排除

## 5. まとめ

| 現状 | 課題 | 解決策 |
|------|------|--------|
| syn 出力が `"* mut T"` | 文字列比較が失敗 | normalize_type_str() workaround |
| RustDeclDict が String 保持 | 正規化漏れ | UnifiedType への段階的移行 |
| TypeRepr が活用不足 | 構造型が文字列化される | codegen で直接利用 |
| UnifiedType が未活用 | 実装済みだが使われない | Phase 1-3 で導入 |
| field_type_map が String | deref_type() が脆弱 | HashMap<String, UnifiedType> に変更 |

**結論**: UnifiedType を codegen パスの中核型表現として段階的に導入することで、
文字列比較の脆弱性を構造的に解消できる。既存の `from_rust_str()` が syn 問題に
対応済みであるため、移行の基盤はすでに整っている。
