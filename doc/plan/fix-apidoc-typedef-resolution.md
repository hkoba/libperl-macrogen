# Apidoc の typedef 解決問題の修正

## 問題

CopFILEAV 等のマクロが TYPE_INCOMPLETE となる原因を調査した結果：

1. `macro_infer.rs` で apidoc の戻り値型を追加する際に `TypeConstraint::from_legacy()` を使用
2. `from_legacy` → `TypeRepr::from_legacy_string()` を呼び出し
3. `from_legacy_string` が **新しい空の StringInterner を作成**
4. typedef 名（AV, GV, SV 等）が空の interner で lookup されるため見つからない
5. フォールバックとして `CTypeSpecs::Void` が使われ、型情報が失われる

## 根本原因

deprecated な関数が残っており、それが誤った設計（空の interner を使用）になっている。

## 解決策

deprecated な `from_legacy` を使わず、既存の正しい API を使用する：

- `TypeRepr::from_apidoc_string(s: &str, interner: &StringInterner)` - 外部から interner を受け取る
- `TypeConstraint::new(expr_id, ty: TypeRepr, context)` - TypeRepr を直接受け取る

## 変更箇所

### macro_infer.rs (2箇所)

**変更前** (line 1036):
```rust
#[allow(deprecated)]
info.type_env.add_return_constraint(TypeConstraint::from_legacy(
    expr.id,
    return_type,
    ConstraintSource::Apidoc,
    format!("return type of macro {}", macro_name_str),
));
```

**変更後**:
```rust
let type_repr = TypeRepr::from_apidoc_string(return_type, interner);
info.type_env.add_return_constraint(TypeConstraint::new(
    expr.id,
    type_repr,
    format!("return type of macro {}", macro_name_str),
));
```

同様に line 1354 付近も修正。

## 実装手順

1. `TypeConstraint::new()` のシグネチャを確認
2. `macro_infer.rs` の2箇所を修正
3. `#[allow(deprecated)]` を削除
4. ビルド確認
5. テスト: CopFILEAV 等が正しく生成されることを確認

## 将来の課題

- `TypeConstraint::from_legacy` と `TypeRepr::from_legacy_string` の完全な削除
- 使用箇所がなくなれば deprecated 関数自体を削除可能

## 補足: UnknownTypedef の追加について

先ほど `CTypeSpecs::UnknownTypedef(String)` を追加しましたが、これは別のアプローチとして有用です：

- 外部から interner を渡せない場合のフォールバック
- 型情報を完全に失う（Void）よりは良い

ただし、今回の修正では正しい interner を渡すため、この variant は必須ではありません。
残しておくかどうかは別途判断できます。
