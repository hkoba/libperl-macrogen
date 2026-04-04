# Plan: 意味解析を Phase 2 に移動

## 目標

Phase 3 (`rust_codegen.rs`) に混入している意味解析処理を
Phase 2 (`macro_infer.rs` / `semantic.rs`) に移動し、
パイプラインアーキテクチャ (Preprocess → Infer → Generate) を遵守する。

---

## 現在の Phase 3 混入解析の一覧

### グループ A: 依存順解析パス（`generate_macros()` 内）

| 処理 | 場所 | 結果の保存先 |
|------|------|-------------|
| const/mut ポインタ推論 | `generate_macros()` 内ループ | `CodegenDriver.const_pointer_params` |
| bool 戻り値型推論 | `generate_macros()` 内ループ | `CodegenDriver.bool_return_macros` |

これらは `topological_sort_macros` で依存順に処理し、
結果を `RustCodegen` に渡して `get_param_type()` / `get_return_type()` で使用。

### グループ B: 式レベルの型推論（`RustCodegen` 内）

| 処理 | 場所 | 用途 |
|------|------|------|
| `infer_expr_type()` | `RustCodegen` | マクロ式の型推論（キャスト挿入等） |
| `infer_expr_type_inline()` | `RustCodegen` | inline 関数式の型推論 |
| `infer_type_hint()` | `RustCodegen` | マクロ式の型ヒント（ポインタ/bool/整数） |
| `is_pointer_expr_inline()` | `RustCodegen` | ポインタ式判定 |
| `is_bool_expr_with_dict()` | `RustCodegen` | bool 式判定（関数戻り値型考慮） |

### グループ C: パラメータ/戻り値型の決定（`RustCodegen` 内）

| 処理 | 場所 | 用途 |
|------|------|------|
| `get_param_type()` | `RustCodegen` | パラメータ型の最終決定（FnParam 優先、const 変換） |
| `get_return_type()` | `RustCodegen` | 戻り値型の最終決定（bool override、void fallback） |
| `collect_mut_params()` | free fn | パラメータの `mut` キーワード決定 |

### グループ D: 外部情報の収集（`CodegenDriver` 内）

| 処理 | 場所 | 用途 |
|------|------|------|
| `seed_callee_const_from_externals()` | `CodegenDriver` | bindings/inline の const パラメータ収集 |
| `seed_bool_return_externals()` | `CodegenDriver` | bindings/inline の bool 戻り値収集 |
| `build_field_type_map()` | free fn | フィールド名 → 型マップ構築 |

---

## 移動計画

### Step 1: `MacroInferInfo` にフィールドを追加

Phase 2 の解析結果を `MacroInferInfo` に格納するフィールドを追加する。

```rust
pub struct MacroInferInfo {
    // ... 既存フィールド ...

    /// パラメータの確定型（Phase 2 で決定）
    /// key: パラメータインデックス, value: Rust 型文字列
    pub resolved_param_types: Vec<String>,

    /// 戻り値の確定型（Phase 2 で決定）
    pub resolved_return_type: Option<String>,

    /// ポインタパラメータの const 位置集合
    pub const_pointer_positions: HashSet<usize>,

    /// bool を返すマクロか
    pub is_bool_return: bool,
}
```

### Step 2: グループ A を Phase 2 に移動

`infer_types_in_dependency_order()` の後に、新しい依存順パスを追加。

```
Phase 2 の処理フロー:
  1. build_macro_info() — 各マクロの展開・パース
  2. infer_types_in_dependency_order() — 型制約収集・伝播 [既存]
  3. resolve_param_and_return_types() — パラメータ/戻り値型の確定 [新規]
     3a. const/mut ポインタ推論
     3b. bool 戻り値推論
     3c. FnParam 優先ロジック
     3d. void フォールバック (infer_expr_type)
```

**移動する関数**:
- `collect_must_mut_pointer_params()` → `macro_infer.rs`
- `mark_lvalue_mut()` → `macro_infer.rs`
- `is_boolean_expr_with_context()` → `macro_infer.rs`
- `seed_callee_const_from_externals()` → `macro_infer.rs`
- `seed_bool_return_externals()` → `macro_infer.rs`

**新規関数**: `resolve_param_and_return_types()`
- `InferResult` のデータを使って全マクロの型を確定
- 結果を `MacroInferInfo.resolved_param_types` / `resolved_return_type` に格納

### Step 3: グループ B の段階的移動

式レベル型推論は Phase 3 のコード生成で密結合しているため、
一度に移動するのは困難。段階的に進める:

**Phase 2 に移動可能な部分**:
- `is_boolean_expr()` / `is_boolean_expr_recursive()` — 純粋関数、Phase 2 でも使用可能
- `build_field_type_map()` — `InferResult` の構築時に実行可能

**Phase 3 に残す部分（当面）**:
- `infer_expr_type()` / `infer_expr_type_inline()` — コード生成中のキャスト挿入判断に使用。
  将来的には Phase 2 で全式の型を事前計算して `ExprId → UnifiedType` マップを作るのが理想だが、
  現時点では影響が大きすぎる。
- `wrap_as_bool_condition()` — コード出力の整形なので Phase 3 で良い。
  ただし「この式が bool か」の判断は Phase 2 の結果を参照すべき。

### Step 4: グループ C を Phase 2 に移動

`get_param_type()` と `get_return_type()` の **型決定ロジック** を Phase 2 に移動。
Phase 3 の同名関数は `resolved_param_types` / `resolved_return_type` を読むだけにする。

```rust
// Phase 3 (生成時) — 単純な参照のみ
fn get_param_type(&self, info: &MacroInferInfo, param_index: usize) -> &str {
    &info.resolved_param_types[param_index]
}

fn get_return_type(&self, info: &MacroInferInfo) -> &str {
    info.resolved_return_type.as_deref().unwrap_or("()")
}
```

### Step 5: グループ D を Phase 2 に移動

`seed_callee_const_from_externals()` と `seed_bool_return_externals()` は
`InferResult` の構築時に実行できる。`build_field_type_map()` も同様。

---

## 実装順序

移動のリスクを最小化するため、段階的に実施する:

### Phase I: 結果フィールドの追加（低リスク）

1. `MacroInferInfo` に `resolved_param_types`, `resolved_return_type`,
   `const_pointer_positions`, `is_bool_return` フィールドを追加
2. Phase 2 (`infer_types_in_dependency_order` の後) で解析パスを実行し、
   結果をフィールドに格納
3. Phase 3 は**両方を参照可能**: 新フィールドがあればそちらを使い、
   なければ従来のロジックにフォールバック
4. テストで結果が一致することを確認

### Phase II: Phase 3 側の置き換え（中リスク）

1. `get_param_type()` を `resolved_param_types` 参照に置き換え
2. `get_return_type()` を `resolved_return_type` 参照に置き換え
3. `CodegenDriver` の依存順解析パスを削除
4. `RustCodegen` から `const_pointer_positions`, `is_bool_return` フィールドを削除

### Phase III: 式レベル型推論の移動（高リスク・将来）

1. Phase 2 で全式の型を事前計算: `HashMap<ExprId, UnifiedType>`
2. Phase 3 の `infer_expr_type()` を事前計算マップの参照に置き換え
3. `wrap_as_bool_condition()` の bool 判定を Phase 2 の結果に基づかせる

→ Phase III は本計画のスコープ外。別の計画で実施。

---

## 影響範囲

| ファイル | 変更内容 |
|----------|----------|
| `src/macro_infer.rs` | `MacroInferInfo` フィールド追加、`resolve_param_and_return_types()` 新規 |
| `src/infer_api.rs` | `infer()` に解析パス追加 |
| `src/rust_codegen.rs` | `get_param_type()`/`get_return_type()` 簡素化、解析パス削除 |
| `src/semantic.rs` | 変更なし（既存の型制約収集はそのまま） |

## テスト計画

- `cargo test` — 全テスト通過
- `~/blob/libperl-rs/12-macrogen-2-build.zsh` — エラー数が同等以下
- regression test — 出力一致

## 期待効果

- inline 関数とマクロで解析結果を共有可能（bool_return_macros 問題の解決）
- Phase 3 のコードが大幅に簡素化
- 将来の型推論改善が Phase 2 に集約されて管理しやすくなる
