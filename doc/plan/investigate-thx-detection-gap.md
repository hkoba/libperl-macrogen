# Perl_custom_op_get_field の THX 判定漏れ調査計画

## 問題

`Perl_custom_op_get_field` を呼び出すマクロに THX フラグが立っていない。

## 発見した事実

### 関連定義（`/usr/lib64/perl5/CORE/op.h`）

```c
// Line 968: aTHX_ あり
#define XopENTRYCUSTOM(o, which) \
    (Perl_custom_op_get_field(aTHX_ o, XOPe_ ## which).which)

// Line 978: aTHX_ なし ← 問題の可能性
#define Perl_custom_op_xop(x) \
    (Perl_custom_op_get_field(x, XOPe_xop_ptr).xop_ptr)
```

### 関数宣言（`/usr/lib64/perl5/CORE/proto.h`）

```c
// Line 678: pTHX_ あり = THX パラメータが必要
Perl_custom_op_get_field(pTHX_ const OP *o, const xop_flags_enum field)
```

---

## 仮説

### 仮説 1: 外部関数呼び出しの THX 要件が追跡されていない

**説明**:
- `Perl_custom_op_get_field` はマクロではなく外部関数
- 現在の実装は `uses` に `aTHX`/`tTHX` が含まれるか、展開後トークンに `my_perl` があるかで判定
- 外部関数の THX 要件（`pTHX_` 宣言）は追跡していない

**検証方法**:
```bash
# 生成された出力で Perl_custom_op_get_field の呼び出しを確認
cargo run -- --auto --gen-rust samples/wrapper.h 2>&1 | grep -A5 "custom_op"
```

**予想**:
- `XopENTRYCUSTOM` は THX 判定される（`aTHX_` を直接使用）
- `Perl_custom_op_xop` は THX 判定されない（外部関数を直接呼び出し、`aTHX_` なし）

---

### 仮説 2: `Perl_custom_op_xop` が Perl ヘッダのバグ

**説明**:
- `Perl_custom_op_xop(x)` は `aTHX_` なしで THX 関数を呼んでいる
- これは MULTIPLICITY 無効ビルド向けの古いコードか、バグの可能性

**検証方法**:
```bash
# Perl のスレッドサポート確認
perl -V:usethreads

# 実際にコンパイルしてエラーになるか確認
echo '#include <EXTERN.h>
#include <perl.h>
void test(OP *o) { Perl_custom_op_xop(o); }' | \
gcc -c -x c - -I/usr/lib64/perl5/CORE $(perl -MExtUtils::Embed -e ccopts) 2>&1
```

**予想**:
- スレッド有効ビルドでコンパイルエラーになる可能性

---

### 仮説 3: `aTHX_` の展開タイミングの問題

**説明**:
- `aTHX_` → `aTHX,` → `my_perl,` と展開される
- 展開タイミングによっては `aTHX` が `uses` に記録されない可能性

**検証方法**:
```rust
// デバッグコードを追加して uses の内容を出力
// src/macro_infer.rs の build_macro_info() 内
eprintln!("Macro {} uses: {:?}",
    interner.get(name),
    info.uses.iter().map(|s| interner.get(*s)).collect::<Vec<_>>());
```

```bash
cargo run -- --auto --gen-rust samples/wrapper.h 2>&1 | grep "XopENTRYCUSTOM"
```

**予想**:
- `XopENTRYCUSTOM` の `uses` に `aTHX` が含まれているはず

---

### 仮説 4: bindings.rs に `Perl_custom_op_get_field` が存在しない

**説明**:
- 外部関数は `bindings.rs` または `proto.h` から取得
- `Perl_custom_op_get_field` が bindings に存在しない場合、関数として認識されない

**検証方法**:
```bash
grep "custom_op_get_field" samples/bindings.rs
```

**予想**:
- 存在しない場合、関数呼び出しとして認識されず、THX 追跡もされない

---

### 仮説 5: `XOPe_ ## which` のトークン貼り付けが問題

**説明**:
- `XopENTRYCUSTOM(o, which)` は `XOPe_ ## which` を使用
- トークン貼り付けの処理中に `aTHX_` の検出がスキップされる可能性

**検証方法**:
```rust
// TokenExpander の展開結果をダンプ
// 展開後トークン列に aTHX や my_perl が含まれるか確認
```

---

## 調査手順

### Step 1: 対象マクロの特定

```bash
# THX 関連の custom_op マクロを列挙
grep -n "custom_op\|XopENTRY" /usr/lib64/perl5/CORE/*.h | head -20
```

### Step 2: 現在の出力確認

```bash
# 生成結果で THX フラグを確認
cargo run -- --auto --gen-rust --bindings samples/bindings.rs samples/wrapper.h 2>&1 | \
  grep -E "(custom_op|XopENTRY|Perl_custom_op)" | head -20
```

### Step 3: デバッグ出力の追加

`src/macro_infer.rs` の `build_macro_info()` に以下を追加:

```rust
// 特定マクロのデバッグ出力
let name_str = interner.get(name);
if name_str.contains("custom_op") || name_str.contains("XopENTRY") {
    eprintln!("[DEBUG] Macro: {}", name_str);
    eprintln!("  uses: {:?}", info.uses.iter().map(|s| interner.get(*s)).collect::<Vec<_>>());
    eprintln!("  is_thx_dependent: {}", info.is_thx_dependent);
    eprintln!("  expanded_tokens: {:?}", expanded_tokens.iter()
        .filter_map(|t| match &t.kind {
            TokenKind::Ident(id) => Some(interner.get(*id)),
            _ => None,
        })
        .collect::<Vec<_>>());
}
```

### Step 4: 仮説の検証

1. デバッグ出力から `XopENTRYCUSTOM` の `uses` を確認
2. `Perl_custom_op_xop` が THX 判定されない理由を特定
3. 根本原因を特定

### Step 5: 修正方針の決定

根本原因に応じて:

**A. 外部関数の THX 要件追跡が必要な場合**:
- `proto.h` または `apidoc` から `pTHX_` 関数リストを構築
- 関数呼び出し時に THX 要件をチェック

**B. Perl ヘッダのバグの場合**:
- 上流に報告
- ワークアラウンドとして手動で THX マクロリストを追加

**C. 展開タイミングの問題の場合**:
- `aTHX_` の展開順序を調整
- `uses` 収集のタイミングを見直し

---

## 実装順序（修正が必要な場合）

1. [ ] Step 1-3 で調査を実行
2. [ ] 根本原因を特定
3. [ ] 修正方針を決定
4. [ ] テストケースを作成
5. [ ] 修正を実装
6. [ ] 回帰テストを実行

---

## 関連ファイル

| ファイル | 役割 |
|----------|------|
| `src/macro_infer.rs:572-583` | THX 初期検出 |
| `src/macro_infer.rs:1061-1099` | THX 伝播 |
| `src/infer_api.rs:316-319` | THX シンボル定義 |
| `/usr/lib64/perl5/CORE/op.h` | 問題のマクロ定義 |
| `/usr/lib64/perl5/CORE/proto.h` | 関数宣言（pTHX_） |
