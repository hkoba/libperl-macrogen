# SvANY が入れ子の関数呼び出し内で展開されない問題の修正計画

## 問題

`SvANY` は `ExplicitExpandSymbols` に登録されているにもかかわらず、一部の生成コードで `SvANY()` が関数呼び出しとして残っている。

### 症状の例

```rust
/// CvSTASH - macro function
#[inline]
pub unsafe fn CvSTASH(sv: *mut CV) -> *mut HV {
    unsafe {
        MUTABLE_HV((*(MUTABLE_PTR(SvANY(sv)) as *mut XPVCV)).xcv_stash)
        //                        ^^^^^^^^ 展開されていない
    }
}
```

期待される出力:
```rust
MUTABLE_HV((*(MUTABLE_PTR((*sv).sv_any) as *mut XPVCV)).xcv_stash)
//                        ^^^^^^^^^^^^ 展開済み
```

## 原因分析

### アーキテクチャの確認

マクロ展開は主に2つの経路で行われる：

1. **トップレベル展開** (`expand_with_calls_internal`, line 194-330)
   - 関数マクロの展開時に `explicit_expand` をチェック (lines 275-277)
   - `SvANY` などが `explicit_expand` に含まれていれば展開される

2. **入れ子展開** (`substitute_and_expand_mut`, lines 581-640)
   - マクロ本体内での展開
   - **関数マクロに対して `explicit_expand` をチェックしていない** (lines 604-614, 618-628)

### 問題箇所: `expand_with_calls_internal` の else 分岐

```rust
// src/token_expander.rs:294-303
else {
    // 展開しない: 関数呼び出しとして保存
    self.called_macros.insert(*id);
    result.push(token.clone());
    // 引数もそのまま追加 ← ★問題: 引数内のマクロが展開されない
    for j in 0..=end_idx {
        result.push(tokens[i + 1 + j].clone());
    }
    i += 1 + end_idx + 1;
    continue;
}
```

### 問題の流れ

`CvSTASH` の処理を例に:

```c
#define CvSTASH(sv) (MUTABLE_HV(((XPVCV*)MUTABLE_PTR(SvANY(sv)))->xcv_stash))
```

1. `build_macro_info` で `CvSTASH` の本体トークンを `expand_with_calls` に渡す
2. `MUTABLE_HV(...)` を検出 - 関数マクロで `explicit_expand` にない
3. `should_expand = false` なので else 分岐へ
4. **引数トークン `((XPVCV*)MUTABLE_PTR(SvANY(sv)))->xcv_stash)` がそのまま追加される**
5. 引数内の `SvANY(sv)` は処理されないまま残る

### 同様の問題: `substitute_and_expand_mut`

```rust
// src/token_expander.rs:604-614, 618-628
if !def.is_function() {
    // オブジェクトマクロは展開
    self.expanded_macros.insert(*id);
    in_progress.insert(*id);
    let expanded = self.expand_object_macro_mut(def, token, in_progress);
    result.extend(expanded);
    in_progress.remove(id);
} else {
    // 関数マクロはそのまま ← ★問題: explicit_expand をチェックしていない
    result.push(token.clone());
}
```

この関数は引数置換時に呼ばれるが、関数マクロを識別子として認識した時点で
`explicit_expand` を考慮せずにそのまま保存している。

## 修正計画

### 修正方針

2つのアプローチが考えられる：

**アプローチ A**: 引数を再帰的に展開する
- `expand_with_calls_internal` の else 分岐で、引数トークンを再帰展開
- より包括的だが、構造変更が大きい

**アプローチ B**: `substitute_and_expand_mut` で `explicit_expand` を処理
- 関数マクロも `explicit_expand` に含まれていれば展開
- より焦点を絞った修正

### 推奨: アプローチ A

理由:
- 問題の根本原因に対処
- 引数内のどの深さでも `explicit_expand` マクロが展開される
- 既存のテストとの整合性が取りやすい

### 実装詳細

#### Step 1: `expand_with_calls_internal` の修正

**場所**: `src/token_expander.rs:294-303`

**変更前**:
```rust
} else {
    // 展開しない: 関数呼び出しとして保存
    self.called_macros.insert(*id);
    result.push(token.clone());
    // 引数もそのまま追加
    for j in 0..=end_idx {
        result.push(tokens[i + 1 + j].clone());
    }
    i += 1 + end_idx + 1;
    continue;
}
```

**変更後**:
```rust
} else {
    // 展開しない: 関数呼び出しとして保存
    self.called_macros.insert(*id);
    result.push(token.clone());

    // 引数を再帰的に展開してから追加
    // 開き括弧を追加
    result.push(tokens[i + 1].clone());  // '('

    // 各引数を展開
    for (arg_idx, arg_tokens) in args.iter().enumerate() {
        if arg_idx > 0 {
            // カンマを追加
            result.push(Token::new(TokenKind::Comma, token.loc.clone()));
        }
        // 引数内のマクロを再帰展開
        let expanded_arg = self.expand_with_calls_internal(arg_tokens, in_progress);
        result.extend(expanded_arg);
    }

    // 閉じ括弧を追加
    result.push(tokens[i + 1 + end_idx].clone());  // ')'

    i += 1 + end_idx + 1;
    continue;
}
```

#### Step 2: (オプション) `substitute_and_expand_mut` の修正

より完全な修正として、`substitute_and_expand_mut` でも関数マクロを処理できるようにする。

**場所**: `src/token_expander.rs:604-628`

ただし、この関数はトークン単位で処理するため、関数マクロの引数収集ロジックを
追加する必要がある。Step 1 で十分な場合はスキップ可能。

## テスト計画

### 1. 単体テスト

```rust
#[test]
fn test_explicit_expand_in_nested_function_call() {
    // SvANY が入れ子の関数呼び出し内で展開されることを確認
}
```

### 2. 統合テスト

```bash
cargo run -- --auto --gen-rust --bindings samples/bindings.rs samples/wrapper.h 2>&1 | grep -A5 "CvSTASH"
```

期待: `SvANY(sv)` ではなく `(*sv).sv_any` が出力される

### 3. 影響範囲の確認

```bash
# 変更前後で SvANY の出現回数を比較
cargo run -- --auto --gen-rust samples/wrapper.h 2>&1 | grep -c "SvANY("
```

## リスク評価

### 低リスク

- 既存の `explicit_expand` ロジックを拡張するだけ
- 引数のトークン列は既に収集済み（`args` 変数）

### 考慮点

1. **再帰深度**: 深く入れ子になった場合のスタック消費
   - 既存の `in_progress` で再帰防止されているため問題なし

2. **性能**: 引数の再帰展開による処理時間増加
   - 引数は通常小さいため影響は軽微

3. **正確性**: カンマの挿入位置
   - 元のトークン列から正確にカンマ位置を復元する必要あり
   - `try_collect_args` の戻り値を活用

## 実装順序

1. [x] `expand_with_calls_internal` の else 分岐を修正
2. [x] 単体テストを追加
3. [x] 統合テストで `CvSTASH` の出力を確認
4. [x] `SvANY(` の残存をチェック
5. [ ] (オプション) `substitute_and_expand_mut` も修正

## 代替案

### `explicit_expand` に `MUTABLE_PTR`, `MUTABLE_HV` も追加する

利点:
- コード変更なし

欠点:
- 根本解決にならない
- 新しいマクロが追加されるたびに対応が必要
- これらのマクロは単純なフィールドアクセスではないため、関数として残す方が適切
