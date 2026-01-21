# goto/label 文の Rust 変換

## 問題

inline 関数のコード生成で `/* TODO: Discriminant(7) */` (goto) と
`/* TODO: Discriminant(11) */` (label) が出力される。

### 例: zaphod32_hash_with_state

```c
switch (len) {
    default: goto zaphod32_read8;
    case 12: v2 += ...;  /* FALLTHROUGH */
    // ...
}
// ...
zaphod32_read8:
    len = key_len & 0x7;
    // ...
zaphod32_finalize:
    ZAPHOD32_FINALIZE(v0,v1,v2);
```

## C と Rust の違い

### C の goto/label

- `goto label;` で関数内の任意の位置にジャンプ可能
- 前方ジャンプ（スキップ）と後方ジャンプ（ループ）の両方が可能

### Rust のラベル付きブロック/ループ

- `'label: { ... }` - ラベル付きブロック、`break 'label;` で脱出
- `'label: loop { ... }` - ラベル付きループ、`break 'label;` / `continue 'label;`
- **重要**: `break` はラベルで囲まれたブロック内からのみ使用可能

## 実装方針

### 基本アプローチ

完全な goto/label 変換は複雑なため、以下の方針で実装：

1. **Label**: ラベル付きブロックとして出力 `'label_name: { stmt }`
2. **Goto**: `break 'label_name;` として出力

### 制限事項

- 前方 goto（ラベルが goto より後にある場合）は正しく動作しない可能性
- 後方 goto（ループ）は `continue` に変換が必要だが、現状は `break` として出力
- 生成コードは手動修正が必要な場合あり

## Stmt 構造

```rust
Goto(InternedStr, SourceLocation),  // goto label_name;

Label {
    name: InternedStr,
    stmt: Box<Stmt>,
    loc: SourceLocation,
}
```

## 実装

```rust
Stmt::Goto(label, _) => {
    let label_str = self.interner.get(*label);
    // Rust のラベル名は ' で始まる必要がある
    format!("{}break '{};", indent, label_str)
}

Stmt::Label { name, stmt, .. } => {
    let label_str = self.interner.get(*name);
    let mut result = format!("{}'{}: {{\n", indent, label_str);
    let nested_indent = format!("{}    ", indent);
    result.push_str(&self.stmt_to_rust_inline(stmt, &nested_indent));
    result.push_str("\n");
    result.push_str(&format!("{}}}", indent));
    result
}
```

## 変更対象ファイル

| ファイル | 変更内容 |
|----------|----------|
| `src/rust_codegen.rs` | `RustCodegen::stmt_to_rust_inline` に `Goto`/`Label` 処理を追加 |
| `src/rust_codegen.rs` | `CodegenDriver::stmt_to_rust_inline` に `Goto`/`Label` 処理を追加 |

## テスト

1. `cargo build` でビルド確認
2. `cargo test` で既存テストが通ることを確認
3. `--gen-rust` で `/* TODO: Discriminant(7) */` と `/* TODO: Discriminant(11) */` が消えることを確認

## 将来の改善

1. **前方 goto の検出**: goto より後にある label を検出し、その間のコードをラベル付きブロックで囲む
2. **後方 goto の変換**: ループパターンを検出し、`'label: loop { ... continue 'label; }` に変換
3. **到達不能コードの処理**: goto 直後のコードが到達不能な場合の処理
