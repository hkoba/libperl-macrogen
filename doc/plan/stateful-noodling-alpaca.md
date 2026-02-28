# Plan: extern static 変数を KnownSymbols に登録する

## Context

`bindings.rs` に `pub static mut PL_sv_placeholder: SV;` 等の extern static 変数が
39 個宣言されているが、`KnownSymbols` に登録されていないため、
マクロ関数生成時に未定義シンボル扱い (`[UNRESOLVED_NAMES]`) となる。

具体例: `SvIMMORTAL` が `PL_sv_placeholder` を参照するが、
`// [UNRESOLVED_NAMES] // Unresolved: PL_sv_placeholder` でコメントアウトされる。

## 根本原因

**`src/rust_decl.rs` L146-152**: `ForeignItem::Static` のパース時、
配列型 (`ty_str.starts_with("[")`) のみ `static_arrays` に登録し、
非配列型の static 変数は完全に無視される。

**`src/rust_codegen.rs` L75-77**: `KnownSymbols::new()` で
`dict.static_arrays` のみ登録。非配列 static 変数のフィールドが存在しない。

## 修正方針

`RustDeclDict` に `statics: HashSet<String>` フィールドを追加し、
全ての `extern static` 変数名を登録する。`KnownSymbols` でもこれを参照する。

### 変更箇所

#### 1. `src/rust_decl.rs` — `RustDeclDict` 構造体 (L58-69)

```rust
pub struct RustDeclDict {
    // ... 既存フィールド ...
    /// 全 extern static 変数名の集合
    pub statics: HashSet<String>,
    /// 配列型の extern static 変数名の集合
    pub static_arrays: HashSet<String>,
    // ...
}
```

#### 2. `src/rust_decl.rs` — `ForeignItem::Static` パース (L146-152)

```rust
syn::ForeignItem::Static(static_item) => {
    let name = static_item.ident.to_string();
    let ty_str = Self::type_to_string(&static_item.ty);
    self.statics.insert(name.clone());  // ← 追加: 全 static を登録
    if ty_str.starts_with("[") {
        self.static_arrays.insert(name);
    }
}
```

#### 3. `src/rust_codegen.rs` — `KnownSymbols::new()` (L75-78)

```rust
for name in &dict.static_arrays {
    names.insert(name.clone());
}
// ↓ 追加
for name in &dict.statics {
    names.insert(name.clone());
}
```

### 変更不要の箇所

- `static_arrays` の既存使用箇所: 配列検出用途で引き続き必要（変更不要）
- `KnownSymbols::contains()`: 既存の `HashSet::contains` がそのまま機能
- カスケード検出ロジック: 影響なし

## 検証

```bash
# 1. 全テスト通過
cargo test

# 2. SvIMMORTAL が正常生成されること確認
cargo run -- --auto --gen-rust samples/xs-wrapper.h --bindings samples/bindings.rs 2>/dev/null \
  | grep -A5 'fn SvIMMORTAL'

# 3. PL_sv_placeholder が UNRESOLVED でないこと
cargo run -- --auto --gen-rust samples/xs-wrapper.h --bindings samples/bindings.rs 2>/dev/null \
  | grep 'PL_sv_placeholder'

# 4. stats の改善確認（unresolved names 減少を期待）
cargo run -- --auto --gen-rust samples/xs-wrapper.h --bindings samples/bindings.rs 2>&1 | tail -3

# 5. 統合ビルドテスト
~/blob/libperl-rs/12-macrogen-2-build.zsh
# → tmp/build-error.log を確認し、E0425 等のエラー数が悪化していないこと
# → 生成された tmp/macro_bindings.rs に SvIMMORTAL が含まれていること
grep -c 'error\[E0425\]' tmp/build-error.log   # 減少を期待
grep 'fn SvIMMORTAL' tmp/macro_bindings.rs      # 正常生成を確認
```
