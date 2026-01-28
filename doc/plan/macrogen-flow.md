# libperl_macrogen::generate() 処理フロー

このドキュメントは `src/macrogen.rs` の `generate()` 関数の処理フローを説明します。

## 概要

```
1. Rustバインディング読込 → 2. フィールド辞書準備 → 3. プリプロセス
→ 4. パース＋収集 → 5-9. 分析準備 → 10-11. inline関数処理
→ 12-14. マクロ関数処理 → 15-16. 出力
```

## 処理の詳細

### 1. Rustバインディングをパース (246-262行)

- `config.bindings` (例: `samples/bindings.rs`) をパースして型情報を取得
- THX依存関数（`my_perl: *mut PerlInterpreter` 引数を持つ関数）を抽出
- 結果: `rust_decls: RustDeclDict`, `thx_functions: HashSet<String>`

### 2. フィールド辞書を作成 (268-271行)

- `FieldsDict` を作成
- `config.target_dir` を設定（デフォルト: `/usr/lib64/perl5/CORE`）
- 用途: マクロ引数の型推論（フィールド名から構造体を特定）

### 3. プリプロセッサを実行 (273-285行)

- `Preprocessor::new()` で初期化
- `process_file()` でファイルを読み込み（ソーススタックに追加）
- **注意**: この時点ではマクロ展開は行われない

### 4. パースしてフィールド辞書を構築 + inline関数を収集 (287-320行)

**ループ対象**: `parser.parse_each()` — プリプロセス済みの全宣言

**処理**:
1. 各宣言を `fields_dict.collect_from_external_decl()` でフィールド情報を収集
2. inline関数を収集

**inline関数収集のフィルタ条件**:
- `config.include_inline_functions` が true
- AND パスが `config.target_dir` で始まる
- AND `ExternalDecl::FunctionDef` である
- AND `func_def.specs.is_inline` が true
- AND 関数名がある

**マクロ展開のタイミング**:
- `parser.parse_each()` → `next_token()` → `try_expand_macro()` のチェーンでオンデマンド展開
- `#define` ディレクティブもこのタイミングでマクロテーブルに登録される

### 5. カスタムフィールド型を登録 (332-345行)

- `config.field_type_overrides` で指定されたフィールド→型マッピングを登録
- 例: `("sv_flags", "sv")` で `sv_flags` フィールドアクセスを `sv` 型と推論

### 6. RustCodeGen を作成 (347-352行)

- コード生成器を初期化
- `bindings.rs` の定数名を収集（重複出力を避けるため）

### 7. 反復型推論コンテキストを作成 (354-360行)

- `InferenceContext::new()` で作成
- `bindings.rs` から既知の型情報をロード

### 8. Apidocから型情報を読み込み (362-383行) [オプション]

- `config.apidoc` が指定されていれば `embed.fnc` などから追加の型情報をロード
- `infer_ctx.load_apidoc()` で確定済み関数として追加

### 9. マクロ分析 (385-412行)

- `MacroAnalyzer::new()` で作成
- `analyzer.set_target_dir()` でターゲットディレクトリを設定
- `identify_constant_macros()`: 定数マクロを識別
- `analyze()`: 全マクロをループしてカテゴリ・型を推論
- `identify_thx_dependent_macros()`: THX依存マクロを識別

### 10. inline関数を確定済みとして追加 (414-433行)

**ループ対象**: `inline_functions` (ステップ4で収集済み)

- 各inline関数からシグネチャを抽出
- `infer_ctx.add_confirmed()` で型推論コンテキストに追加

### 11. inline関数を出力 (441-469行)

**ループ対象**: `inline_functions`

- `codegen.inline_fn_to_rust()` でRustコードに変換
- 成功/失敗をカウント
- 使用した定数マクロを `all_used_constants` に記録

### 12. マクロ関数を処理 (471-546行)

**ループ対象**: `analyzer.iter()` — 分析済みの全マクロ

**フィルタリング** (順次適用):
1. `info.is_target` — ターゲットディレクトリ内で定義されたマクロのみ
2. `macros.get(*name)` — マクロ定義が存在する
3. `MacroKind::Object` をスキップ — 関数マクロのみ
4. `info.category == MacroCategory::Expression` — 式マクロのみ
5. `analyzer.parse_macro_body()` が成功

**処理**:
- パース成功したマクロを `PendingFunction` として `infer_ctx.add_pending()` に追加
- 式ASTを `macro_exprs` に保存

### 13. 反復推論を実行 (552-567行)

- `infer_ctx.run_inference()` で型推論を反復実行
- 呼び出し先関数の型情報を使って未解決の型を解決
- 収束するまで反復

### 14. マクロ関数を出力 (571-666行)

**ループ対象**: `macro_names` (ソート済みのマクロ名)

**フィルタリング** (再度、出力時):
1. パース失敗したものはスキップ（失敗理由をコメントで出力）
2. `MacroKind::Object` をスキップ
3. `info.category == MacroCategory::Expression` のみ

**処理**:
- 推論結果から型情報を取得
- `codegen.macro_to_rust_fn()` でRustコードに変換
- 使用した定数マクロを `all_used_constants` に記録

### 15. ヘッダーを出力 (681-686行)

- `use std::ffi::...` などの標準インポート
- `use crate::bindings::*;`

### 16. 定数定義を生成 (688-730行)

**ループ対象**: `all_used_constants` (ステップ11, 14で収集)

**フィルタ**: `bindings.rs` に既に定義されている定数は除外

**処理**:
- 各定数マクロの本体を式としてパース
- Rustの `const` 定義として出力

## 主要なデータ構造

| 変数 | 型 | 用途 |
|------|-----|------|
| `rust_decls` | `RustDeclDict` | bindings.rsの型情報 |
| `fields_dict` | `FieldsDict` | フィールド名→構造体名マッピング |
| `inline_functions` | `Vec<(String, ExternalDecl, String)>` | 収集されたinline関数 |
| `analyzer` | `MacroAnalyzer` | マクロ分析結果 |
| `infer_ctx` | `InferenceContext` | 型推論コンテキスト |
| `codegen` | `RustCodeGen` | Rustコード生成器 |

## 更新履歴

- 2024-12-31: 初版作成。`target_dirs` を単一の `target_dir` に変更した時点の状態を記録。
