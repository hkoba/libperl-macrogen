# 不完全型検出の修正計画

## 問題

`HeVAL` マクロの戻り値型が `/* union.hent_val */` という不完全な形式で生成されているにもかかわらず、
CODEGEN_INCOMPLETE としてコメントアウトされずに出力されている。

```rust
/// HeVAL - macro function
#[inline]
pub unsafe fn HeVAL(he: *mut HE) -> /* union.hent_val */ {
    ((*he).he_valu).hent_val
}
```

## 原因分析

### 問題1: 不完全型がカウントされない

`RustCodegen` は `incomplete_count` で不完全なコードをカウントし、
`is_complete()` で判定している。

しかし、`TypeRepr::to_rust_string()` → `InferredType::to_rust_string()` で
フィールド型が解決できない場合に返される `/* base.member */` 形式は、
`incomplete_count` をインクリメントしない。

```rust
// type_repr.rs:1191-1192
InferredType::MemberAccess { base_type, member, .. } => {
    format!("/* {}.{} */", base_type, interner.get(*member))
}
```

### 問題2: apidoc の戻り値型が使われていない

HeVAL の apidoc には `return_type: SV*` があるが、
実際の推論結果では式の型 `union.hent_val` が使われている。

```
HeVAL: expression (4 constraints, 0 uses)
  ...
  expr#42413: union.hent_val (union.hent_val)
```

apidoc 制約が追加されているはずだが、`get_return_type` で
ルート式の制約が優先されている可能性がある。

## 解決策

### 案1: 型文字列にコメントが含まれるかチェック（簡易）

`type_repr_to_rust` の戻り値に `/*` が含まれていたら不完全としてカウントする。

**メリット**: 変更が最小限
**デメリット**: ハック的

### 案2: TypeRepr に is_complete メソッドを追加

`TypeRepr` に `is_complete()` メソッドを追加し、
不完全な型を表す場合に false を返すようにする。

**メリット**: 型システムとして正しい
**デメリット**: 変更箇所が多い

### 案3: to_rust_string を Result 型に変更

`to_rust_string` を `Result<String, IncompleteType>` に変更し、
不完全な場合は Err を返す。

**メリット**: エラーハンドリングが明確
**デメリット**: 変更箇所が多い

### 案4: apidoc 制約の優先度を上げる

戻り値型の取得時に、apidoc からの制約を優先するようにする。

**メリット**: 根本解決
**デメリット**: apidoc が間違っている場合に問題

## 推奨案

**案1 + 案4 の組み合わせ**

1. まず案1で不完全型の検出を確実にする（防御的コーディング）
2. 次に案4で apidoc 制約を優先するようにする（本来あるべき動作）

## 実装計画

### Step 1: 不完全型文字列の検出

`RustCodegen::type_repr_to_rust` を修正し、結果に `/*` が含まれていたら
`incomplete_count` をインクリメントする。

```rust
fn type_repr_to_rust(&mut self, ty: &TypeRepr) -> String {
    let result = ty.to_rust_string(self.interner);
    if result.contains("/*") {
        self.incomplete_count += 1;
    }
    result
}
```

### Step 2: apidoc 戻り値型の優先

`get_return_type` で apidoc からの制約（`CTypeSource::Apidoc`）を
他の制約より優先して使用する。

```rust
fn get_return_type(&mut self, info: &MacroInferInfo) -> String {
    // まず apidoc 制約を探す
    if let Some(ty) = info.type_env.get_return_type() {
        if ty.is_from_apidoc() {
            return self.type_repr_to_rust(ty);
        }
    }
    // ルート式の制約も apidoc 優先でチェック
    ...
}
```

### Step 3: テスト

- HeVAL が `*mut SV` を返すことを確認
- 他のマクロで退行がないことを確認

## 変更対象ファイル

| ファイル | 変更内容 |
|----------|----------|
| `src/rust_codegen.rs` | `type_repr_to_rust` に不完全チェック追加 |
| `src/rust_codegen.rs` | `get_return_type` で apidoc 優先 |
| `src/type_repr.rs` | (オプション) `is_from_apidoc()` メソッド追加 |
