# Plan: 文字列ベース型比較から UnifiedType 構造型への移行

## Context

コード生成パス (`rust_codegen.rs`) の型判定が文字列比較に依存しており、
syn ライブラリの `to_token_stream().to_string()` が `"* mut T"` (スペース入り) を
出力する問題で脆弱性がある。`normalize_type_str()` や二重プレフィックスチェックで
回避しているが、根本的でない。

既に定義済みだが **未使用** の `UnifiedType` (`src/unified_type.rs`) を
コード生成パスに導入し、文字列ベースの型比較を構造的な型判定に置き換える。

### 解消対象の脆弱な関数

| 関数 | 呼出数 | 置換先 |
|------|--------|--------|
| `is_pointer_type_str(&str)` | 15 | `ut.is_pointer()` |
| `is_const_pointer_type_str(&str)` | 1 | `ut.is_const_pointer()` |
| `deref_type(&str) -> Option<&str>` | 4 | `ut.inner_type()` |
| `normalize_type_str(&str)` | 1 | 不要になる |
| `is_float_type_str(&str)` | 2 | `ut.is_float()` |

## 修正ファイル

| ファイル | 変更内容 |
|---------|---------|
| `src/unified_type.rs` | ヘルパーメソッド追加 |
| `src/rust_decl.rs` | UnifiedType フィールド追加 |
| `src/rust_codegen.rs` | データストア・消費者の移行 |

## Phase 0: UnifiedType にヘルパーメソッド追加

**ファイル**: `src/unified_type.rs`

既存メソッド: `is_pointer()`, `inner_type()`, `is_named()`, `as_named()`

追加するメソッド:

```rust
pub fn is_const_pointer(&self) -> bool {
    matches!(self, Self::Pointer { is_const: true, .. })
}

pub fn is_float(&self) -> bool {
    matches!(self, Self::Float | Self::Double | Self::LongDouble)
        || matches!(self, Self::Named(n) if n == "NV")
}

pub fn is_bool(&self) -> bool {
    matches!(self, Self::Bool)
}

pub fn is_void(&self) -> bool {
    matches!(self, Self::Void)
}
```

単体テストも追加。

**検証**: `cargo test`

## Phase 1: RustDeclDict に UnifiedType フィールド追加 (二重保持)

**ファイル**: `src/rust_decl.rs`

既存の `ty: String` フィールドと並行して `uty: UnifiedType` を追加。
既存コードを一切壊さず、新しいフィールドだけ追加する。

```rust
pub struct RustParam {
    pub name: String,
    pub ty: String,
    pub uty: UnifiedType,       // NEW
}

pub struct RustField {
    pub name: String,
    pub ty: String,
    pub uty: UnifiedType,       // NEW
}

pub struct RustFn {
    pub name: String,
    pub params: Vec<RustParam>,
    pub ret_ty: Option<String>,
    pub uret_ty: Option<UnifiedType>,  // NEW
}

pub struct RustConst {
    pub name: String,
    pub ty: String,
    pub uty: UnifiedType,       // NEW
}

pub struct RustTypeAlias {
    pub name: String,
    pub ty: String,
    pub uty: UnifiedType,       // NEW
}
```

変換は既存の `type_to_string()` 出力を `UnifiedType::from_rust_str()` で構造化:

```rust
fn type_to_unified(ty: &Type) -> UnifiedType {
    UnifiedType::from_rust_str(&Self::type_to_string(ty))
}
```

`from_rust_str()` は syn の `"* mut"` スペース問題を内部で正規化済み (L261-264)。

`process_item()` 内の全構造体構築箇所で `uty`/`uret_ty` を設定。

**検証**: `cargo test` (既存テストは ty: String を参照するので変更なし)

## Phase 2: `field_type_map` を UnifiedType に移行

**ファイル**: `src/rust_codegen.rs`

### 変更 1: 型を変更

```rust
// L843: HashMap<String, String> → HashMap<String, UnifiedType>
field_type_map: HashMap<String, UnifiedType>,
```

### 変更 2: `build_field_type_map()` (L603-629)

```rust
fn build_field_type_map(dict: Option<&RustDeclDict>) -> HashMap<String, UnifiedType> {
    // field.uty.clone() を使用。normalize_type_str() は不要になる。
    // 型の衝突判定: UnifiedType の PartialEq で構造比較。
}
```

### 変更 3: 消費者 6 箇所を更新

| 行 | 現在 | 変更後 |
|----|------|--------|
| L1090 | `is_pointer_type_str(ty)` | `ut.is_pointer()` |
| L1081 | `deref_type(ty)` + `is_pointer_type_str` | `ut.inner_type().map(|t| t.is_pointer())` |
| L1203 | `is_pointer_type_str(ty)` | `ut.is_pointer()` |
| L1209 | `deref_type` + `is_pointer_type_str` | `ut.inner_type().map(|t| t.is_pointer())` |
| L1283 | `.cloned()` → String | `.map(|ut| ut.to_rust_string())` |
| L1436 | `.cloned()` → String | `.map(|ut| ut.to_rust_string())` |

### 変更 4: `normalize_type_str()` を削除 (L633-638)

**検証**: `cargo test` + `~/blob/libperl-rs/12-macrogen-2-build.zsh`

## Phase 3: `get_callee_return_type` / `get_callee_param_type` を UnifiedType に移行

**ファイル**: `src/rust_codegen.rs`

### 変更 1: 戻り値型を変更

```rust
// L1626: Option<&str> → Option<&UnifiedType>
fn get_callee_return_type(&self, func_name: &str) -> Option<&UnifiedType> {
    self.rust_decl_dict?.fns.get(func_name).and_then(|f| f.uret_ty.as_ref())
}

// L1571: Option<&str> → Option<&UnifiedType>
fn get_callee_param_type(&self, func_name: &str, arg_index: usize) -> Option<&UnifiedType> {
    self.rust_decl_dict?.fns.get(func_name).and_then(|f|
        f.params.get(arg_index).map(|p| &p.uty)
    )
}
```

### 変更 2: 消費者を更新

`get_callee_return_type` 消費者:

| 行 | 現在 | 変更後 |
|----|------|--------|
| L1058 | `is_pointer_type_str(ret_ty)` | `ret_ut.is_pointer()` |
| L1226 | `is_pointer_type_str(ret_ty)` | `ret_ut.is_pointer()` |
| L1319 | `ret_ty.to_string()` | `ret_ut.to_rust_string()` |
| L1472 | `ret_ty.to_string()` | `ret_ut.to_rust_string()` |
| L1642 | `ret_ty == "bool"` | `ret_ut.is_bool()` |

`get_callee_param_type` 消費者:

| 行 | 現在 | 変更後 |
|----|------|--------|
| L1580 | `param_ty == "bool"` | `param_ut.is_bool()` |
| L1718 | `expected_ty` → `normalize_integer_type` | `expected_ut.to_rust_string()` → `normalize_integer_type` |

**検証**: `cargo test` + `~/blob/libperl-rs/12-macrogen-2-build.zsh`

## Phase 4: `current_param_types` と `current_return_type` を UnifiedType に移行

**ファイル**: `src/rust_codegen.rs`

### 変更 1: 型を変更

```rust
// L823: Option<String> → Option<UnifiedType>
current_return_type: Option<UnifiedType>,
// L829: HashMap<InternedStr, String> → HashMap<InternedStr, UnifiedType>
current_param_types: HashMap<InternedStr, UnifiedType>,
```

### 変更 2: 格納箇所を更新

```rust
// L1781: collect_decl_names
let ty_str = self.apply_derived_to_type(&base_type, &derived);
self.current_param_types.insert(name, UnifiedType::from_rust_str(&ty_str));

// L2995: generate_inline_fn
let ty_str = self.param_type_only(p);
self.current_param_types.insert(param_name, UnifiedType::from_rust_str(&ty_str));

// L1839: macro get_return_type
self.current_return_type = Some(UnifiedType::from_rust_str(&return_type));

// L2986: inline fn
self.current_return_type = Some(UnifiedType::from_rust_str(&return_type));
```

### 変更 3: 消費者を更新 (~15 箇所)

`current_param_types` 消費者:

| 行 | 現在 | 変更後 |
|----|------|--------|
| L1170 | `ty == "bool"` | `ut.is_bool()` |
| L1189 | `is_pointer_type_str(ty)` | `ut.is_pointer()` |
| L1266 | `.cloned()` → String | `.map(|ut| ut.to_rust_string())` |

`current_return_type` 消費者:

| 行 | 現在 | 変更後 |
|----|------|--------|
| L1881 | `== Some("()")` | `.is_void()` |
| L1884 | `== Some("bool")` | `.is_bool()` |
| L2694,2711 | `is_pointer_type_str(rt)` + `is_null_literal` | `ut.is_pointer()` + `ut.is_const_pointer()` |
| L2734,3206 | 同上 (return 文) | 同上 |
| L2533,3965 | `.clone()` for type_hint | `.map(|ut| ut.to_rust_string())` |

**検証**: `cargo test` + `~/blob/libperl-rs/12-macrogen-2-build.zsh`

## Phase 5: `infer_expr_type_str` を `Option<UnifiedType>` に移行

**ファイル**: `src/rust_codegen.rs`

最大の変更フェーズ。`infer_expr_type_str()` と `infer_expr_type_str_inline()` の
戻り値を `Option<String>` → `Option<UnifiedType>` に変更。

### 変更 1: 関数シグネチャ変更

```rust
fn infer_expr_type_str(&self, expr: &Expr, info: &MacroInferInfo) -> Option<UnifiedType>
fn infer_expr_type_str_inline(&self, expr: &Expr) -> Option<UnifiedType>
```

### 変更 2: 関数内部の各 match arm

- Ident → `current_param_types.get()` は Phase 4 で既に `&UnifiedType` を返す
- Member/PtrMember → `field_type_map.get()` は Phase 2 で既に `&UnifiedType` を返す
- Call → `get_callee_return_type()` は Phase 3 で既に `&UnifiedType` を返す
- Cast → `UnifiedType::from_rust_str(&type_name_to_type_str_readonly(...))`
- Deref → `inner_ut.inner_type().cloned()`
- Binary → 再帰呼出しの結果がそのまま `Option<UnifiedType>`
- リテラル → `Some(UnifiedType::Int { signed: false, size: IntSize::Int })` など

### 変更 3: 消費者 (~20 箇所) を更新

| パターン | 消費者の変更 |
|----------|------------|
| ポインタ判定 (L1079, L1207) | `ut.is_pointer()` |
| float 判定 (L2234, L2244, L3671, L3681) | `ut.is_float()` |
| 整数幅比較 (L2264, L2565, L3185, L3997) | `ut.to_rust_string()` → `normalize_integer_type()` |
| 引数キャスト (L1719) | `ut.to_rust_string()` → `cast_integer_arg_if_needed()` |

### 変更 4: 旧関数削除

- `is_pointer_type_str()`
- `is_const_pointer_type_str()`
- `deref_type()`
- `is_float_type_str()`
- `null_ptr_expr(&str)` → `null_ptr_expr(&UnifiedType)` に変更

**検証**: `cargo test` + `~/blob/libperl-rs/12-macrogen-2-build.zsh` + regression test

## Phase 6 (後日): String フィールド削除

`RustDeclDict` の `ty: String` / `ret_ty: Option<String>` フィールドを削除し、
`uty` → `ty` にリネーム。Phase 5 完了後に実施。

## 設計ポイント

### なぜ TypeRepr ではなく UnifiedType か

| 基準 | UnifiedType | TypeRepr |
|------|------------|---------|
| `PartialEq, Eq, Hash` | ✓ derive 済み | ✗ source 情報が異なる |
| `from_rust_str()` | ✓ syn 対応済み | ✗ なし |
| 複雑さ | シンプル (9 variant) | 複雑 (3 大 variant) |
| codegen 親和性 | 高い | 推論向け |

TypeRepr は `semantic.rs` の型推論に特化。UnifiedType は codegen パス向け。

### syn 問題の完全解消ポイント

syn の `"* mut T"` 問題は、型文字列が codegen パスに入る **3 つの入口** で発生:

1. `field_type_map` ← Phase 2 で解消 (`UnifiedType::from_rust_str` が正規化)
2. `get_callee_return_type()` ← Phase 3 で解消
3. `get_callee_param_type()` ← Phase 3 で解消

Phase 0-4 完了時点で **syn 脆弱性は完全に排除** される。
Phase 5 は正確性ではなく **アーキテクチャの一貫性** のための改善。

## 検証コマンド (各 Phase 共通)

```bash
cargo test
~/blob/libperl-rs/12-macrogen-2-build.zsh
grep -c '^error' tmp/build-error.log
grep 'consider using' tmp/build-error.log | sort | uniq -c | sort -rn
```

生成コードの出力が Phase 間で変化しないことを確認 (regression test)。
