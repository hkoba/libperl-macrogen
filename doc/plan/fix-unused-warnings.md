# 未使用警告の修正計画

## 現状の警告一覧

ビルド時に以下の11件の警告が出力される:

### 1. 非推奨コードの使用 (1件)

| 場所 | 警告内容 |
|------|----------|
| `src/lib.rs:73` | `ConstraintSource` enum は非推奨 |

### 2. 未使用変数 (4件)

| 場所 | 警告内容 |
|------|----------|
| `src/rust_codegen.rs:1593` | `is_signed` 変数は代入されるが使われない |
| `src/rust_codegen.rs:1607` | `is_signed` への代入結果が読まれない |
| `src/rust_codegen.rs:1608` | `is_signed` への代入結果が読まれない |
| `src/semantic.rs:2053` | `return_type` 変数が未使用 |

### 3. 未使用のコード (6件)

| 場所 | 警告内容 |
|------|----------|
| `src/preprocessor.rs:301` | `InputSource::from_tokens` 関数 |
| `src/preprocessor.rs:2160` | `Preprocessor::collect_to_eol` メソッド |
| `src/rust_codegen.rs:235` | `RustCodegen::write` メソッド |
| `src/rust_codegen.rs:2308+` | `CodegenDriver` の6メソッド (`build_param_list`, `get_param_type`, `get_return_type`, `type_repr_to_rust`, `expr_to_rust`, `stmt_to_rust`) |
| `src/token_expander.rs:38` | `TokenExpander::files` フィールド |
| `src/token_expander.rs:339+` | `TokenExpander` の2メソッド (`expand_function_macro`, `substitute_and_expand`) |

---

## 修正方針

### カテゴリ A: 即座に削除可能

以下は明らかに不要なコードで、削除しても影響がない:

1. **`is_signed` 変数** (`rust_codegen.rs:1593-1608`)
   - `is_unsigned` のみを使用しており、`is_signed` は冗長
   - 削除して問題なし

2. **`RustCodegen::write` メソッド** (`rust_codegen.rs:235`)
   - `writeln` のみが使われており、`write` は不要
   - 削除して問題なし

### カテゴリ B: 将来使用予定のため保留

以下は将来の機能実装で使用予定:

1. **`TokenExpander::files` フィールド** (`token_expander.rs:38`)
   - エラーメッセージ改善で使用予定
   - `#[allow(dead_code)]` を付けて保留

2. **`TokenExpander::expand_function_macro`, `substitute_and_expand`** (`token_expander.rs:339+`)
   - Preprocessor から独立したマクロ展開機能として残す可能性
   - `#[allow(dead_code)]` を付けて保留

3. **`InputSource::from_tokens`** (`preprocessor.rs:301`)
   - テスト用途で有用な可能性
   - `#[allow(dead_code)]` を付けて保留

### カテゴリ C: 設計見直しが必要

1. **`ConstraintSource` の非推奨** (`lib.rs:73`)
   - `TypeRepr` の方が情報量が多いため移行済み
   - 公開 API から削除する際は破壊的変更になる
   - **方針**: lib.rs から re-export を削除（非公開に戻す）

2. **`return_type` 未使用** (`semantic.rs:2053`)
   - `get_macro_return_type` の結果を取得しているが使っていない
   - **方針**:
     - 実際に `return_type` を活用するよう修正するか
     - 不要なら if 文ごと削除

3. **`Preprocessor::collect_to_eol`** (`preprocessor.rs:2160`)
   - 以前のパース処理で使用していた可能性
   - **方針**: 使用箇所がなければ削除

4. **`CodegenDriver` の6メソッド** (`rust_codegen.rs:2308+`)
   - `RustCodegen` の同名メソッドの複製（重複コード）
   - `RustCodegen`: 行 333, 350, 385, 406, 466, 637 で定義・使用
   - `CodegenDriver`: 行 2308+ で定義されているが未使用
   - **方針**: CodegenDriver の重複メソッドを削除

---

## 実装計画

### Phase 1: 即座に削除可能なコードの削除

1. `is_signed` 変数を削除 (`rust_codegen.rs`)
2. `RustCodegen::write` メソッドを削除

### Phase 2: 保留コードに `#[allow(dead_code)]` を追加

1. `TokenExpander::files` に属性追加
2. `TokenExpander::expand_function_macro`, `substitute_and_expand` に属性追加
3. `InputSource::from_tokens` に属性追加

### Phase 3: 設計見直し

1. `ConstraintSource` の re-export を削除
2. `return_type` 問題の調査と修正
3. `collect_to_eol` の削除判断
4. `CodegenDriver` の未使用メソッドの削除判断

---

## 期待される結果

- ビルド時の警告が 11件 → 0件 に減少
- コードの意図が明確になる（`#[allow(dead_code)]` はコメント付き）
- 不要なコードが削除され、保守性が向上

---

## 作業量見積もり

| Phase | 作業内容 | 削除行数 (概算) |
|-------|----------|----------------|
| Phase 1 | 変数・メソッド削除 | ~10行 |
| Phase 2 | allow 属性追加 | +10行 |
| Phase 3-1 | ConstraintSource 非公開化 | 1行変更 |
| Phase 3-2 | return_type 調査・修正 | ~5行 |
| Phase 3-3 | collect_to_eol 削除 | ~20行 |
| Phase 3-4 | CodegenDriver 重複削除 | ~250行 |

Phase 3-4 が最も大きな変更だが、単純な削除なのでリスクは低い。
