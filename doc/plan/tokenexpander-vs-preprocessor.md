# TokenExpander と Preprocessor の分離: メリット・デメリット分析

## 背景

現在、マクロ展開は2つの異なるモジュールで行われている：

1. **Preprocessor** (`src/preprocessor.rs`)
   - C ヘッダのプリプロセス処理全般を担当
   - トークンペースト（`##`）、文字列化（`#`）を完全サポート
   - `#include`, `#if`, `#define` などのディレクティブ処理
   - inline 関数の body 内のマクロ展開

2. **TokenExpander** (`src/token_expander.rs`)
   - マクロ推論（`MacroInferContext`）で使用
   - マクロ本体のトークン列を展開
   - トークンペースト（`##`）機能**なし**
   - `preserve_function_macros` / `explicit_expand` による展開制御

## 現在の処理フロー

```
┌─────────────────────────────────────────────────────────────────────────┐
│ Stage 1: Preprocessor によるプリプロセス                                 │
│                                                                         │
│  - #include の解決                                                       │
│  - #define でマクロ定義を MacroTable に登録                              │
│  - #if/#ifdef の評価                                                     │
│  - inline 関数の body 内のマクロ展開（## サポートあり）                   │
│                                                                         │
│  出力: トークン列 + MacroTable                                           │
└─────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────┐
│ Stage 2: Parser による AST 構築                                          │
│                                                                         │
│  - inline 関数 → InlineFnDict                                           │
│  - 構造体定義 → FieldsDict                                               │
│  - マクロ定義は MacroTable から取得                                       │
└─────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────┐
│ Stage 3: MacroInferContext によるマクロ型推論                            │
│                                                                         │
│  各マクロについて:                                                        │
│  1. TokenExpander でマクロ本体を展開（## サポートなし）                    │
│  2. 展開結果をパースして AST 構築                                         │
│  3. 型推論を実行                                                         │
│                                                                         │
│  問題: ## を含むマクロの展開が不完全                                      │
└─────────────────────────────────────────────────────────────────────────┘
```

## 選択肢

### 選択肢 A: TokenExpander にトークンペースト機能を追加

TokenExpander を改良して Preprocessor と同等の `##` 処理を実装する。

### 選択肢 B: TokenExpander を廃止して Preprocessor に一本化

マクロ推論でも Preprocessor を使用するように変更する。

---

## 選択肢 A: TokenExpander を改良

### メリット

1. **責務の分離が維持される**
   - Preprocessor: ファイル処理、ディレクティブ処理
   - TokenExpander: 純粋なマクロ展開
   - 各モジュールの責務が明確

2. **軽量な処理**
   - TokenExpander はファイル I/O やディレクティブ処理を含まない
   - メモリ使用量が少ない
   - テストが書きやすい

3. **既存コードへの影響が限定的**
   - TokenExpander に `##` 処理を追加するだけ
   - MacroInferContext の呼び出し側は変更不要

4. **柔軟な展開制御**
   - `preserve_function_macros` / `explicit_expand` の仕組みがそのまま使える
   - マクロごとに展開/非展開を細かく制御可能

### デメリット

1. **コードの重複**
   - Preprocessor と TokenExpander の両方にトークンペースト処理を実装
   - 将来的なバグ修正・機能追加が二重作業になる可能性

2. **実装コスト**
   - `paste_tokens()` 相当の機能を TokenExpander に移植する必要
   - `token_to_string()` などの補助関数も必要

3. **一貫性の欠如リスク**
   - 2つのマクロ展開器の挙動が微妙に異なる可能性
   - デバッグが困難になる場合がある

### 実装見積もり

- `paste_tokens()` の移植: 約50行
- `token_to_string()` の移植: 約40行
- `substitute_and_expand_mut()` の修正: 約30行
- テスト追加: 約50行

**合計: 約170行の追加/変更**

---

## 選択肢 B: Preprocessor に一本化

### メリット

1. **コードの重複がなくなる**
   - マクロ展開ロジックが一箇所に集約
   - バグ修正・機能追加が一度で済む

2. **一貫した挙動**
   - inline 関数とマクロ関数で同じ展開ロジックを使用
   - デバッグが容易

3. **完全な機能セット**
   - トークンペースト、文字列化など全機能が自動的に利用可能
   - 将来的な C プリプロセッサ機能追加にも対応

4. **TinyCC との整合性**
   - TinyCC も単一のプリプロセッサでマクロ展開を処理
   - CLAUDE.md の「TinyCC のアプローチに従う」方針に合致

### デメリット

1. **Preprocessor の複雑化**
   - 新たな使用パターン（マクロ本体の展開のみ）をサポート
   - API の拡張が必要

2. **状態管理の複雑さ**
   - Preprocessor はファイル状態を持つ
   - マクロ本体展開時に不要な状態を持ち運ぶオーバーヘッド

3. **大規模なリファクタリング**
   - MacroInferContext の大幅な変更が必要
   - TokenExpander を使用している全箇所の修正

4. **展開制御の再設計**
   - `preserve_function_macros` / `explicit_expand` の仕組みを Preprocessor に移植
   - または、新しい制御方式を設計

### 実装見積もり

- Preprocessor に「マクロ本体展開モード」を追加: 約100行
- MacroInferContext の書き換え: 約200行
- 展開制御の移植: 約150行
- テストの修正: 約100行
- TokenExpander の削除: -約1000行

**合計: 約550行の変更、約1000行の削除**

---

## 比較表

| 観点 | 選択肢 A (TokenExpander 改良) | 選択肢 B (Preprocessor 一本化) |
|------|------------------------------|-------------------------------|
| **実装コスト** | 小（約170行追加） | 中（約550行変更） |
| **コード重複** | あり（2箇所で展開処理） | なし |
| **責務の分離** | 維持される | 統合される |
| **一貫性** | リスクあり | 保証される |
| **テスト容易性** | 良い | 普通 |
| **TinyCC 整合性** | なし | あり |
| **将来の拡張性** | 二重作業リスク | 一箇所で対応 |
| **既存コードへの影響** | 小 | 大 |

---

## 現在の問題の具体例

### `XopENTRYCUSTOM` マクロ

```c
// apidoc: token 型の引数 "which"
// =for apidoc Amu||XopENTRYCUSTOM|const OP *o|token which

#define XopENTRYCUSTOM(o, which) \
    (Perl_custom_op_get_field(aTHX_ o, XOPe_ ## which).which)
```

### `OP_CLASS` マクロ（`XopENTRYCUSTOM` を使用）

```c
#define OP_CLASS(o) ((o)->op_type == OP_CUSTOM \
                     ? XopENTRYCUSTOM(o, xop_class) \
                     : ((*PL_opargs.offset((*o).op_type as isize)) & (15 << 8)))
```

### 期待される展開結果

`OP_CLASS` の本体で `XopENTRYCUSTOM(o, xop_class)` が展開されると：

```c
(Perl_custom_op_get_field(aTHX_ o, XOPe_xop_class).xop_class)
```

- `XOPe_ ## xop_class` → `XOPe_xop_class`（トークンペースト）
- `.which` → `.xop_class`（パラメータ置換）

### 現在の問題

TokenExpander ではトークンペーストが処理されないため、展開結果に `##` が残り、パースが失敗する。

---

## 推奨事項

### 短期的な解決（選択肢 A）

- 実装コストが低い
- 既存の動作に影響を与えにくい
- 今回の `XopENTRYCUSTOM` 問題を迅速に解決できる

### 長期的な方向性（選択肢 B を検討）

- コードの重複を解消
- TinyCC のアプローチに合わせる
- ただし、十分なテストと段階的な移行が必要

---

## 参考: TinyCC のアプローチ

TinyCC (`tccpp.c`) では：

1. 単一の `macro_subst()` 関数でマクロ展開を処理
2. トークンペースト（`##`）は `paste_tokens()` で処理
3. 文字列化（`#`）は `tok_str_add2()` で処理
4. 再帰的な展開は同じ関数で処理

TinyCC は「単一のプリプロセッサで全てを処理」というアプローチを採用しており、選択肢 B の方向性に近い。
