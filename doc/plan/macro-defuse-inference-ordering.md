# マクロ型推論の def-use 依存順序制御

## 目標

マクロの型推論を def-use 関係に基づく順序で実行し、依存先マクロの型情報を
依存元マクロの推論に活用できるようにする。

## 背景

### 現状の問題

`SvFLAGS(sv)` マクロは戻り値の型が `U32` と正しく推論されているが、
`SvFLAGS` を使用する `SvUOK`, `SvVOK` などのマクロでは、
`SvFLAGS` の呼び出し結果が `<unknown>` となっている。

原因: 現在の `analyze_all_macros` は全マクロを一括で解析しており、
def-use 関係を型推論に活用していない。

### 期待する動作

1. `SvFLAGS` の型推論が完了（戻り値 `U32` 確定）
2. `SvUOK` の型推論時に、`SvFLAGS` の戻り値型 `U32` を参照可能
3. `SvUOK` の推論結果に `SvFLAGS` の型情報が反映される

## 第一の改良: InferStatus の分割

### 現状

```rust
pub struct MacroInferInfo {
    // ...
    pub infer_status: InferStatus,  // 単一のフィールド
}

pub enum InferStatus {
    Pending,        // 未処理
    TypeComplete,   // 全て確定
    TypeIncomplete, // 一部未確定
    TypeUnknown,    // 推論不能
}
```

### 変更後

```rust
pub struct MacroInferInfo {
    // ...
    /// 引数の型推論状態
    pub args_infer_status: InferStatus,
    /// 戻り値の型推論状態
    pub return_infer_status: InferStatus,
}
```

### 変更点

1. `MacroInferInfo` の `infer_status` を削除
2. `args_infer_status` と `return_infer_status` を追加
3. `new()` で両方を `Pending` に初期化
4. `is_fully_confirmed()` メソッドを追加（両方が `TypeComplete` なら true）
5. `mark_confirmed()`, `mark_unknown()` を更新

### MacroInferStats も分割

```rust
pub struct MacroInferStats {
    pub total: usize,
    pub confirmed: usize,
    pub unconfirmed: usize,
    /// 引数の型が unknown のマクロ数
    pub args_unknown: usize,
    /// 戻り値の型が unknown のマクロ数
    pub return_unknown: usize,
    // unknown は削除（args_unknown + return_unknown で代替可能）
}
```

`is_unknown()` メソッドは不要になる。各マクロの `args_infer_status` と `return_infer_status` を
直接参照すればよい。

## 第二の改良: analyze_all_macros のフロー変更

### 現状のフロー

```
1. collect_thx_dependencies() - THX 依存を2パスで収集
2. collect_pasting_dependencies() - ## 依存を2パスで収集
3. for each target macro:
     analyze_macro() - パース AND 型推論を同時実行
4. build_use_relations() - used_by を構築
5. classify_initial() - confirmed/unconfirmed/unknown に分類
```

問題: 型推論(Step 3)の時点で used_by がまだ構築されていない

### 変更後のフロー

```
1. build_all_macro_info() - 全マクロの初期構築（パースのみ、型推論なし）
   - MacroInferInfo を作成
   - has_token_pasting なマクロを pasting_initial に追加
   - is_thx_dependent なマクロを thx_initial に追加（直接 THX トークンを含む）

2. build_use_relations() - uses から used_by を構築

3. propagate_thx_via_used_by() - used_by を使って thx_macros の推移閉包を計算
   - thx_initial に含まれるマクロの used_by を辿って伝播

4. propagate_pasting_via_used_by() - used_by を使って pasting_macros の推移閉包を計算

5. 全マクロを unconfirmed に分類

6. infer_types_in_dependency_order() - 依存順に型推論
   while unconfirmed is not empty:
     candidates = get_inference_candidates()
     if candidates is empty:
       // 残りは全て unknown へ
       move all unconfirmed to unknown
       break
     for each candidate:
       infer_macro_types(candidate)  // 型推論のみ
       if is_fully_confirmed():
         move to confirmed
       else:
         move to unknown
```

## 詳細実装計画

### Step 1: InferStatus の分割

**src/macro_infer.rs:**

```rust
impl MacroInferInfo {
    pub fn new(name: InternedStr) -> Self {
        Self {
            // ...
            args_infer_status: InferStatus::Pending,
            return_infer_status: InferStatus::Pending,
            // infer_status を削除
        }
    }

    /// 引数と戻り値の両方が確定しているか
    pub fn is_fully_confirmed(&self) -> bool {
        self.args_infer_status == InferStatus::TypeComplete
            && self.return_infer_status == InferStatus::TypeComplete
    }

    // is_unknown() は不要 - args_infer_status/return_infer_status を直接参照
}
```

### Step 2: analyze_macro を分割

**現在の analyze_macro を2つに分割:**

```rust
/// Phase 1: MacroInferInfo の初期構築（パースまで）
pub fn build_macro_info(
    &mut self,
    def: &MacroDef,
    macro_table: &MacroTable,
    interner: &StringInterner,
    files: &FileRegistry,
    rust_decl_dict: Option<&RustDeclDict>,
    typedefs: &HashSet<InternedStr>,
) -> (MacroInferInfo, bool, bool) {
    // MacroInferInfo を作成
    // トークン展開
    // uses を収集
    // パース試行
    // (info, has_token_pasting, has_thx_direct) を返す
}

/// Phase 2: 型推論の適用
pub fn infer_macro_types<'a>(
    &mut self,
    name: InternedStr,
    interner: &'a StringInterner,
    files: &FileRegistry,
    apidoc: Option<&'a ApidocDict>,
    fields_dict: Option<&'a FieldsDict>,
    rust_decl_dict: Option<&'a RustDeclDict>,
    typedefs: &HashSet<InternedStr>,
) {
    // MacroInferInfo から parse_result を取得
    // SemanticAnalyzer で型制約を収集
    // type_env を更新
    // args_infer_status, return_infer_status を設定
}
```

### Step 3: analyze_all_macros の再構成

```rust
pub fn analyze_all_macros<'a>(
    &mut self,
    macro_table: &MacroTable,
    interner: &'a StringInterner,
    files: &FileRegistry,
    apidoc: Option<&'a ApidocDict>,
    fields_dict: Option<&'a FieldsDict>,
    rust_decl_dict: Option<&'a RustDeclDict>,
    typedefs: &HashSet<InternedStr>,
    thx_symbols: (InternedStr, InternedStr, InternedStr),
) {
    // Step 1: 全マクロの初期構築
    let mut thx_initial = HashSet::new();
    let mut pasting_initial = HashSet::new();

    for def in macro_table.iter_target_macros() {
        let (info, has_pasting, has_thx) = self.build_macro_info(
            def, macro_table, interner, files, rust_decl_dict, typedefs
        );
        if has_pasting {
            pasting_initial.insert(def.name);
        }
        if has_thx {
            thx_initial.insert(def.name);
        }
        self.register(info);
    }

    // Step 2: used_by を構築
    self.build_use_relations();

    // Step 3: THX の推移閉包を計算（used_by 経由）
    self.propagate_flag_via_used_by(&thx_initial, |info| &mut info.is_thx_dependent);

    // Step 4: ## の推移閉包を計算（used_by 経由）
    self.propagate_flag_via_used_by(&pasting_initial, |info| &mut info.has_token_pasting);

    // Step 5: 全マクロを unconfirmed に
    for name in self.macros.keys().copied().collect::<Vec<_>>() {
        self.unconfirmed.insert(name);
    }

    // Step 6: 依存順に型推論
    self.infer_types_in_dependency_order(
        interner, files, apidoc, fields_dict, rust_decl_dict, typedefs
    );
}

fn infer_types_in_dependency_order<'a>(
    &mut self,
    interner: &'a StringInterner,
    files: &FileRegistry,
    apidoc: Option<&'a ApidocDict>,
    fields_dict: Option<&'a FieldsDict>,
    rust_decl_dict: Option<&'a RustDeclDict>,
    typedefs: &HashSet<InternedStr>,
) {
    loop {
        let candidates = self.get_inference_candidates();
        if candidates.is_empty() {
            // 残りを全て unknown へ
            let remaining: Vec<_> = self.unconfirmed.iter().copied().collect();
            for name in remaining {
                self.mark_unknown(name);
            }
            break;
        }

        for name in candidates {
            self.infer_macro_types(
                name, interner, files, apidoc, fields_dict, rust_decl_dict, typedefs
            );

            let is_confirmed = self.macros.get(&name)
                .map(|info| info.is_fully_confirmed())
                .unwrap_or(false);

            if is_confirmed {
                self.mark_confirmed(name);
            } else {
                self.mark_unknown(name);
            }
        }
    }
}
```

### Step 4: propagate_flag_via_used_by の実装

```rust
/// used_by を辿ってフラグを推移的に伝播
fn propagate_flag_via_used_by<F>(
    &mut self,
    initial_set: &HashSet<InternedStr>,
    get_flag: F,
) where
    F: Fn(&mut MacroInferInfo) -> &mut bool,
{
    // 初期集合のフラグを設定
    for name in initial_set {
        if let Some(info) = self.macros.get_mut(name) {
            *get_flag(info) = true;
        }
    }

    // used_by を辿って伝播
    let mut to_propagate: Vec<InternedStr> = initial_set.iter().copied().collect();

    while let Some(name) = to_propagate.pop() {
        let used_by_list: Vec<InternedStr> = self.macros
            .get(&name)
            .map(|info| info.used_by.iter().copied().collect())
            .unwrap_or_default();

        for user in used_by_list {
            if let Some(user_info) = self.macros.get_mut(&user) {
                let flag = get_flag(user_info);
                if !*flag {
                    *flag = true;
                    to_propagate.push(user);
                }
            }
        }
    }
}
```

## 修正対象ファイル

1. **src/macro_infer.rs**
   - `MacroInferInfo`: `infer_status` を `args_infer_status` + `return_infer_status` に分割
   - `MacroInferStats`: `unknown` を `args_unknown` + `return_unknown` に分割
   - `get_stats()`: 新しい統計フィールドに対応
   - `analyze_macro` を `build_macro_info` + `infer_macro_types` に分割
   - `analyze_all_macros` のフロー変更
   - `propagate_flag_via_used_by` 追加
   - `infer_types_in_dependency_order` 追加
   - `collect_thx_dependencies`, `collect_pasting_dependencies` を削除（不要に）

2. **src/main.rs**
   - `infer_status` 参照箇所を新フィールドに更新
   - `MacroInferStats` の表示を更新（`args_unknown`, `return_unknown`）

## 期待される結果

変更前:
```
SvUOK: expression (N constraints, 1 uses)
  (call (ident SvFLAGS) ...) :type <unknown>
```

変更後:
```
SvUOK: expression (N constraints, 1 uses)
  (call (ident SvFLAGS) ...) :type U32
```

`SvFLAGS` が先に推論され confirmed になった後、`SvUOK` が推論されるため、
`SvFLAGS` の戻り値型 `U32` が参照可能になる。

## 注意点

1. **依存ループの処理**: 循環依存がある場合、`get_inference_candidates` が空を返し、
   残りは全て `unknown` に分類される

2. **パフォーマンス**: `used_by` を使った伝播は O(E) で、現在の2パス方式と同等

3. **後方互換性**: `infer_status` を使っている箇所を全て更新する必要あり
