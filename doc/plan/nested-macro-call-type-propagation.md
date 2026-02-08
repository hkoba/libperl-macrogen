# 計画: ネストしたマクロ呼び出しからの型伝播

## 背景

### 問題

マクロ `HEK_FLAGS(hek)` が内部で `HEK_KEY(hek)` を呼び出す場合、
`HEK_KEY` のパラメータ型 `*mut HEK` が `hek` に伝播されない。

```c
#define HEK_KEY(hek)   (hek)->hek_key    // hek: *mut HEK（フィールド推論で確定）
#define HEK_FLAGS(hek) (*((unsigned char *)(HEK_KEY(hek))+HEK_LEN(hek)+1))
```

現状:
- `HEK_KEY(hek: *mut HEK)` → 正常に型推論される（フィールドアクセスから）
- `HEK_FLAGS(hek: /* unknown */)` → パラメータ型が不明のまま

### 原因

`collect_call_constraints` は以下のソースから型情報を取得:
1. `rust_decl_dict` (bindings.rs)
2. `apidoc` (embed.fnc)

しかし、**他のマクロのパラメータ型情報**は参照していない。

### 期待する動作

`HEK_FLAGS(hek)` 内で `HEK_KEY(hek)` が呼ばれている場合:
- `HEK_KEY` の第1パラメータ型 `*mut HEK` を取得
- `hek` 引数にこの型制約を追加
- 結果: `hek: *mut HEK`

## 現在のアーキテクチャ

### 型推論フロー

```
MacroInferContext.infer_types_in_dependency_order()
    │
    ├─ 依存順でマクロを処理
    │
    └─ 各マクロに対して:
        ├─ SemanticAnalyzer を作成
        ├─ set_macro_return_types(cache) ← 戻り値型のみ
        ├─ register_macro_params_from_apidoc()
        ├─ collect_expr_constraints()
        │   ├─ ExprKind::Call → collect_call_constraints()
        │   │   ├─ rust_decl_dict から型取得
        │   │   └─ apidoc から型取得
        │   │       ↑ マクロのパラメータ型は参照されない
        │   │
        │   └─ ExprKind::MacroCall → args の制約収集
        │       ↑ 呼び出すマクロのパラメータ型は参照されない
        │
        └─ 結果を MacroInferInfo に保存
```

### 既存の仕組み

| コンポーネント | 役割 | 現状 |
|----------------|------|------|
| `macro_return_types` | 確定済みマクロの戻り値型キャッシュ | 戻り値型のみ |
| `MacroInferInfo.params` | マクロのパラメータリスト | 型情報あり（推論済みの場合） |
| `collect_call_constraints` | 関数呼び出しから引数型を推論 | マクロを参照しない |

## 設計案

### 案 A: マクロパラメータ型キャッシュの追加

`macro_return_types` と同様に、マクロのパラメータ型キャッシュを追加。

```rust
// macro_infer.rs
pub struct MacroInferContext {
    // 既存
    pub macros: HashMap<InternedStr, MacroInferInfo>,

    // 追加: マクロ名 → パラメータ型リスト
    pub macro_param_types: HashMap<String, Vec<(String, String)>>,  // [(param_name, type_str)]
}

// semantic.rs
pub struct SemanticAnalyzer<'a> {
    // 既存
    macro_return_types: Option<&'a HashMap<String, String>>,

    // 追加
    macro_param_types: Option<&'a HashMap<String, Vec<(String, String)>>>,
}
```

**実装手順**:

1. `MacroInferContext` にパラメータ型キャッシュを追加
2. マクロ処理後、確定したパラメータ型をキャッシュに保存
3. `SemanticAnalyzer` にキャッシュへの参照を渡す
4. `collect_call_constraints` でマクロ名をキャッシュから検索
5. 見つかった場合、引数に型制約を追加

**利点**:
- 既存パターン (`macro_return_types`) と一貫性がある
- 実装が比較的シンプル

**欠点**:
- 依存順処理が必要（被呼び出しマクロが先に処理される必要あり）
- キャッシュ管理のオーバーヘッド

### 案 B: MacroInferInfo を直接参照

`MacroInferContext.macros` から直接パラメータ型を取得。

```rust
// semantic.rs
pub struct SemanticAnalyzer<'a> {
    // 追加: MacroInferContext への参照
    macro_context: Option<&'a MacroInferContext>,
}

impl SemanticAnalyzer<'_> {
    fn collect_call_constraints(&mut self, ...) {
        // 既存: rust_decl_dict, apidoc から検索

        // 追加: macro_context から検索
        if let Some(ctx) = self.macro_context {
            if let Some(info) = ctx.macros.get(&func_name) {
                for (i, param) in info.params.iter().enumerate() {
                    if let Some(arg) = args.get(i) {
                        if let Some(ty) = &param.resolved_type {
                            // 型制約を追加
                        }
                    }
                }
            }
        }
    }
}
```

**利点**:
- キャッシュ不要、直接参照
- 常に最新の情報を使用

**欠点**:
- `MacroInferContext` への参照が必要（ライフタイム管理が複雑化）
- 循環参照の可能性

### 案 C: ExprKind::MacroCall での型伝播

`MacroCall` AST ノードにマクロ情報への参照を含め、処理時に型を伝播。

```rust
pub enum ExprKind {
    MacroCall {
        name: InternedStr,
        args: Vec<Expr>,
        expanded: Box<Expr>,
        // 追加: パラメータ型情報
        param_types: Option<Vec<TypeRepr>>,
    },
    // ...
}
```

**利点**:
- AST に型情報が含まれる
- 再計算不要

**欠点**:
- AST 構造の変更が必要
- パース時に型情報が不明な場合がある

### 案 D: 2パス処理

1パス目: 全マクロのパラメータ型を収集
2パス目: 収集した情報を使って再度型推論

**利点**:
- 依存順序に関係なく処理可能

**欠点**:
- 処理時間が2倍
- 実装が複雑

## 推奨案

**案 A（マクロパラメータ型キャッシュ）** を推奨。

理由:
1. 既存パターン (`macro_return_types`) と一貫性がある
2. 依存順処理は既に `infer_types_in_dependency_order` で実装済み
3. 実装変更が最小限

## 実装ステップ

### Phase 1: パラメータ型キャッシュの追加

1. `MacroInferContext` に `macro_param_types: HashMap<String, Vec<(String, String)>>` を追加
2. `confirmed` に移動する際、パラメータ型をキャッシュに保存
3. `SemanticAnalyzer` に `set_macro_param_types()` を追加

### Phase 2: collect_call_constraints の拡張

1. `collect_call_constraints` でマクロパラメータ型キャッシュを参照
2. 関数名がキャッシュに存在する場合、引数に型制約を追加
3. 優先順位: `rust_decl_dict` > `apidoc` > `macro_param_types`

### Phase 3: テストと検証

1. `HEK_FLAGS` が `*mut HEK` を推論できることを確認
2. 他のネストしたマクロ呼び出しパターンを検証
3. 回帰テストに追加

## 影響範囲

| ファイル | 変更内容 |
|----------|----------|
| `src/macro_infer.rs` | `macro_param_types` キャッシュ追加、保存ロジック |
| `src/semantic.rs` | `set_macro_param_types()`、`collect_call_constraints` 拡張 |
| `doc/architecture-semantic-type-inference.md` | ドキュメント更新 |

## 対象となるマクロ例

| マクロ | ネストした呼び出し | 期待される推論 |
|--------|-------------------|----------------|
| `HEK_FLAGS(hek)` | `HEK_KEY(hek)` | `hek: *mut HEK` |
| `HEK_UTF8(hek)` | `HEK_FLAGS(hek)` | `hek: *mut HEK` |
| `HeKFLAGS(he)` | `HEK_FLAGS(HeKEY_hek(he))` | `he: *mut HE` |

## 注意点

### 依存順序の重要性

ネストした呼び出しの型伝播は、**被呼び出しマクロが先に処理される**必要がある。

```
HEK_FLAGS → HEK_KEY
    ↑          ↑
    呼び出し側  被呼び出し側（先に処理）
```

現在の `infer_types_in_dependency_order` は def-use 関係を使用しており、
この順序は自然に満たされる。

### 型の競合

同じパラメータに複数の型制約がある場合の優先順位:
1. `rust_decl_dict` (bindings.rs)
2. `apidoc` (embed.fnc)
3. `macro_param_types` (マクロからの伝播)
4. フィールドアクセスからの推論

## 実装状況

| Phase | 状態 | 内容 |
|-------|------|------|
| Phase 1 | 完了 | `macro_param_types` キャッシュを `MacroInferContext` に追加 |
| Phase 2 | 完了 | `collect_call_constraints` と `ExprKind::MacroCall` でキャッシュを参照 |
| Phase 3 | 完了 | 下記マクロで型推論が正常に動作することを確認 |

### 検証結果

| マクロ | 以前の推論結果 | 修正後の推論結果 |
|--------|---------------|-----------------|
| `HEK_FLAGS(hek)` | `/* unknown */` | `*mut HEK` |
| `HEK_UTF8(hek)` | `/* unknown */` | `*mut HEK` |
| `HeKFLAGS(he)` | `/* unknown */` | `*mut HE` |

### 実装の詳細

1. `cache_param_types_to()`: 確定したマクロのパラメータ型を外部キャッシュに保存
2. `infer_types_in_dependency_order()`: ローカルな `param_types_cache` を使用し、最後に `self.macro_param_types` に同期
3. `collect_call_constraints()`: `ExprKind::Call` で呼び出されるマクロのパラメータ型を参照
4. `ExprKind::MacroCall` ハンドラ: 保存されたマクロ呼び出しのパラメータ型を参照

キャッシュには Rust 形式の型文字列（例: `*mut HEK`）が保存されるため、
`TypeRepr::from_rust_string()` を使用して型制約を生成する。

## 関連ドキュメント

- `doc/architecture-semantic-type-inference.md` - 型推論アーキテクチャ
- `doc/plan/reverse-type-inference-from-field-access.md` - フィールドからの型推論
- `doc/architecture-fields-dict.md` - FieldsDict アーキテクチャ
