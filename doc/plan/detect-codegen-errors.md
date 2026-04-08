# Plan: codegen 段階でのエラー検知とコメントアウト出力

## 目標

Rust コンパイルエラーになるコードを codegen 段階で検知し、
関数全体をコメントアウトして問題点を列挙する。
既存のカテゴリ (`[TYPE_INCOMPLETE]`, `[CODEGEN_INCOMPLETE]` 等) と
同じ仕組みで新しいカテゴリを追加する。

## 既存のコメントアウト仕組み

`RustCodegen::generate_macro()` → `GeneratedCode` を返す。
`GeneratedCode` に `has_unresolved_names()` や `incomplete_count` があり、
`CodegenDriver` がこれを見てコメントアウト出力を決定。

同様に、新しいエラーカテゴリを `GeneratedCode` に追加し、
`CodegenDriver` で判定・コメントアウトする。

---

## 難易度順の実装計画

### Step 1: ジェネリクス `as T` (23件) — ★☆

**検知方法**: マクロの `generic_type_params` が非空の場合、
生成コードに `as T` (ジェネリック型への `as` キャスト) が含まれるか。

実際にはもっとシンプル: ジェネリクスを含むマクロは
E0605 (non-primitive cast), E0369 (binary op on T), E0747 (const as type)
など複数のエラーを引き起こす。

**判定**: `info.generic_type_params` が非空のマクロは
現在のコード生成では正しく生成できないため、
一律 `[GENERIC_UNSUPPORTED]` としてコメントアウト。

**影響**: 23 (E0605) + 4 (E0369) + 1 (E0308 `T` 関連) = ~28件

**変更箇所**: `CodegenDriver::generate_macros()` の `get_macro_status()` に
ジェネリクスチェックを追加。

### Step 2: 未定義型 (5件) — ★☆

**検知方法**: 生成コードに `RustDeclDict` に存在しない型名が含まれる。

既に `[UNRESOLVED_NAMES]` カテゴリで部分的に対応されている。
`PerlIO_funcs`, `caddr_t`, `body_details` は型名であり、
シンボル名ではない。

**判定**: `type_name_to_rust()` で生成した型名が bindings.rs の型辞書
（structs, typedefs）に存在しないか、かつ基本型でもなければ未定義。

**変更箇所**: 型名生成時に未知の型名を検出し `GeneratedCode` にフラグ設定。

### Step 3: lvalue 代入不正 (9件) — ★★

**検知方法**: `Assign` の LHS が以下のパターン:

1. `Call(func, args)` — 関数呼び出し結果への代入 (E0067)
   ただし C のマクロ展開で lvalue になるケースは `try_expand_call_as_lvalue` で
   既に対応。対応できなかったものが残る。

2. `(*o).op_moresib()` — ビットフィールドメソッド結果への代入 (E0070)
   ビットフィールドの setter メソッドが bindings.rs に存在しないケース。

**判定**: `Assign` の LHS 展開後が `Call` のままで lvalue 化できない場合を検知。
ビットフィールドは `is_bitfield_method()` で判定済み。

**変更箇所**: `expr_to_rust` の `Assign` ハンドラで lvalue 不可を検知し、
`GeneratedCode` にエラー情報を記録。

### Step 4: flexible array `[i8; 1]` (8件) — ★★

**検知方法**: フィールドアクセスで型が `[T; 1]` の場合、
`.offset()` や加算ができない。

**判定**: `field_type_map` でフィールド型を確認し、
`UnifiedType::Array { size: Some(1) }` の場合をフラグ。

**変更箇所**: `expr_to_rust_inline` の `Index` / ポインタ加算 ハンドラで
配列型フィールドへの `.offset()` 生成時にエラー記録。

### Step 5: `-usize` 不正 (5件) — ★★

**検知方法**: `UnaryMinus(expr)` で `infer_expr_type` が `usize` を返す場合、
`-usize` は Rust で不可。

**判定**: `UnaryMinus` 生成時に式の推論型を確認。
`usize` なら `-(expr as isize)` に変換するか、エラーとしてコメントアウト。

**変更箇所**: `expr_to_rust` / `expr_to_rust_inline` の `UnaryMinus` ハンドラ。

### Step 6: opcode enum 変換 (7件) — ★★★

**検知方法**: `Cast { target: opcode }` で `opcode` が enum 型。
`i32 as opcode` は `transmute` が必要で、現在のコードでは
`is_enum_cast_target` で一部対応済み。逆方向 (`opcode as i32`) も問題。

**判定**: enum 辞書を参照して enum ↔ 整数のキャスト方向をチェック。

### Step 7: mutability 不一致 (30件) — ★★★

**検知方法**: 関数引数渡し / 戻り値で `*const` と `*mut` の不一致。
`infer_expr_type` が actual type を返せば検知可能。

**難点**: `infer_expr_type` が `None` を返すケースが多い。
Phase 2 の型推論改善と連動する必要がある。

**判定**: `cast_integer_arg_if_needed` で actual と expected が
ポインタ型で const/mut が異なる場合にフラグ。

### Step 8: SV subtype キャスト不足 (16件) — ★★★

**検知方法**: 引数が `*mut GV` だが callee が `*const SV` を要求。
SV subtype 関係にあるが actual type が推論できず cast が挿入されない。

**難点**: Step 7 と同じく `infer_expr_type` の精度問題。

---

## 実装アーキテクチャ

### `GeneratedCode` への問題点記録

```rust
pub struct GeneratedCode {
    pub code: String,
    pub incomplete_count: usize,
    pub unresolved_names: Vec<String>,
    // 追加:
    pub codegen_errors: Vec<CodegenError>,
}

pub struct CodegenError {
    pub kind: CodegenErrorKind,
    pub message: String,
}

pub enum CodegenErrorKind {
    GenericUnsupported,
    UndefinedType,
    InvalidLvalue,
    FlexibleArray,
    NegateUsize,
    EnumCast,
    MutabilityMismatch,
    SvSubtypeCast,
}
```

### `CodegenDriver` での出力

```rust
if !generated.codegen_errors.is_empty() {
    // [CODEGEN_ERROR] category — コメントアウトして問題点列挙
    writeln!(self.writer, "// [CODEGEN_ERROR] {} - macro function", name_str)?;
    for err in &generated.codegen_errors {
        writeln!(self.writer, "//   {:?}: {}", err.kind, err.message)?;
    }
    for line in generated.code.lines() {
        writeln!(self.writer, "// {}", line)?;
    }
}
```

---

## 実装順序と期待効果

| Step | カテゴリ | 件数 | 難易度 | 累計削減 |
|------|----------|------|--------|---------|
| 1 | ジェネリクス `as T` | ~28 | ★☆ | 28 |
| 2 | 未定義型 | 5 | ★☆ | 33 |
| 3 | lvalue 代入不正 | 9 | ★★ | 42 |
| 4 | flexible array | 8 | ★★ | 50 |
| 5 | `-usize` | 5 | ★★ | 55 |
| 6 | opcode enum | 7 | ★★★ | 62 |
| 7 | mutability 不一致 | 30 | ★★★ | 92 |
| 8 | SV subtype | 16 | ★★★ | 108 |

Step 1-5 で約 55 件（エラー全体の 31%）をコメントアウトに変換可能。
Step 6-8 は `infer_expr_type` の精度に依存するため、
Phase 2 の型推論改善と併せて実施。
