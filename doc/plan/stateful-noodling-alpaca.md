# Plan: マクロ/inline関数の統合依存性追跡

## Context

### 問題

`doc/plan/e0425-improvement-analysis.md` で取り組んだカスケード検出の改善後も、
以下のケースが未解決として残っている:

| シンボル | 現状の出力 | 原因 |
|----------|-----------|------|
| `Perl_newSV_type` | `[UNRESOLVED_NAMES]` (inline) | codegen レベルの失敗 |
| `Perl_newSV_type_mortal` | 正常生成 (inline) | `Perl_newSV_type` を呼ぶがカスケード検出されない |
| `SvIMMORTAL` | `[CASCADE_UNAVAILABLE]` (macro) | `SvIMMORTAL_INTERP` が `[CODEGEN_INCOMPLETE]` |

`Perl_newSV_type_mortal` が正常生成されてしまう問題は、inline→inline カスケード検出
のバグ、またはクロスドメイン（inline↔macro）カスケード検出の欠落に起因する。

### 根本原因: 依存性追跡の断片化

現在、def-use 管理がマクロと inline 関数で統合されていない:

| 処理段階 | マクロ | inline 関数 |
|----------|--------|-------------|
| パース時 | `uses`, `called_functions` を収集 | **何も収集しない** |
| 推論時 | `check_function_availability()` で利用可能性チェック | **対象外** |
| 推論時 | `propagate_unavailable_via_used_by()` で推移閉包 | **対象外** |
| codegen時 | クロスドメインカスケード検査あり | ad-hoc で `inline_calls` を構築、fixpoint ループ |

**inline 関数の依存情報は codegen 段階まで遅延されている。**
これにより、マクロ側の推論段階（`analyze_all_macros`）では inline 関数の
利用可能性を正確に判定できない。

### ユーザー提案との関係

ユーザーの提案:
- `enum DependencyType { Macro, Inline }` を導入
- `uses: HashSet<InternedStr>` を `uses: HashMap<InternedStr, DependencyType>` に変更
- inline 関数もパース時に uses を収集
- codegen での生成順序制御を統合

考察の結果、**`called_functions` を統合の軸とする**のが適切と判断:

| 概念 | 用途 | マクロ | inline 関数 |
|------|------|--------|-------------|
| `uses` | マクロ→マクロのトークン展開関係。型推論の順序制御用 | ✓ | 該当なし |
| `called_functions` | AST 上の関数呼び出し先。利用可能性チェック用 | ✓ | **✓ 追加** |

`uses` はマクロ固有の概念（トークン展開）であり、inline 関数には当てはまらない。
一方 `called_functions` は AST 上の Call 式から収集するもので、
マクロ・inline 関数の両方に自然に適用できる。

## 変更方針

### Phase 1: `InlineFnDict` に依存性追跡を追加

`InlineFnDict` に `called_functions` と `calls_unavailable` を追加し、
パース時に収集する。

**`src/inline_fn.rs`**:

```rust
pub struct InlineFnDict {
    fns: HashMap<InternedStr, FunctionDef>,
    // 追加: 各 inline 関数の呼び出し先
    called_functions: HashMap<InternedStr, HashSet<InternedStr>>,
    // 追加: 利用不可関数の呼び出しを含む
    calls_unavailable: HashSet<InternedStr>,
}
```

`collect_from_function_def()` で、`MacroInferContext::collect_function_calls_from_block_items()`
を再利用して `called_functions` を収集する:

```rust
pub fn collect_from_function_def(&mut self, func_def: &FunctionDef, interner: &StringInterner) {
    // ... 既存の処理 ...

    // 追加: 関数呼び出し先を収集
    let mut calls = HashSet::new();
    MacroInferContext::collect_function_calls_from_block_items(
        &func_def.body.items,
        &mut calls,
    );
    self.called_functions.insert(name, calls);

    self.insert(name, func_def);
}
```

新しい public API:
```rust
impl InlineFnDict {
    pub fn get_called_functions(&self, name: InternedStr) -> Option<&HashSet<InternedStr>>;
    pub fn is_calls_unavailable(&self, name: InternedStr) -> bool;
    pub fn set_calls_unavailable(&mut self, name: InternedStr);
    pub fn called_functions_iter(&self) -> impl Iterator<Item = (&InternedStr, &HashSet<InternedStr>)>;
}
```

### Phase 2: `analyze_all_macros` で inline 関数の利用可能性チェックを統合

`analyze_all_macros` の引数を `&InlineFnDict` → `&mut InlineFnDict` に変更し、
Step 4.5 の後に inline 関数の利用可能性チェックを追加する。

**`src/macro_infer.rs` — `analyze_all_macros()`**:

```
Step 4.5: マクロの利用不可関数チェック（既存）
Step 4.6: inline 関数の利用不可関数チェック（新規） ← 追加
Step 4.7: クロスドメイン推移閉包の計算（新規）    ← 追加
```

**Step 4.6: `check_inline_fn_availability()`** (新規メソッド):

inline 関数の `called_functions` を `check_function_availability()` と同じロジックで
チェックし、`InlineFnDict::set_calls_unavailable()` で結果を記録する。

```rust
fn check_inline_fn_availability(
    &self,
    inline_fn_dict: &mut InlineFnDict,
    rust_decl_dict: Option<&RustDeclDict>,
    interner: &StringInterner,
) {
    // check_function_availability() と同じロジック:
    // called_functions の各呼び出し先が bindings, macros, inlines, builtins の
    // いずれかに存在するかチェック
}
```

**Step 4.7: `propagate_unavailable_cross_domain()`** (新規メソッド):

マクロ↔inline 関数のクロスドメイン伝播を fixpoint ループで実行:

```rust
fn propagate_unavailable_cross_domain(
    &mut self,
    inline_fn_dict: &mut InlineFnDict,
) {
    loop {
        let mut changed = false;

        // (a) macro → macro（既存の propagate_unavailable_via_used_by 相当）
        // (b) inline → inline: inline_fn の called_functions が
        //     calls_unavailable な inline を含む場合、自身も unavailable
        // (c) macro → inline: マクロの called_functions が
        //     calls_unavailable な inline を含む場合、マクロも unavailable
        // (d) inline → macro: inline の called_functions が
        //     calls_unavailable なマクロを含む場合、inline も unavailable

        if !changed { break; }
    }
}
```

### Phase 3: codegen のカスケード検出を簡素化

`calls_unavailable` が事前計算されているため、codegen でのカスケード検出を簡素化。

**`src/rust_codegen.rs` — `generate_inline_fns()`**:

- L3074-3083 の ad-hoc `inline_calls` 構築を削除（`InlineFnDict` から取得）
- L3128-3148 の fixpoint ループを簡素化（pre-codegen で検出済みのものを反映）
- ただし codegen レベルの失敗（`UNRESOLVED_NAMES`, `CODEGEN_INCOMPLETE`）による
  カスケードは引き続き codegen 時に検出する必要がある

**重要**: codegen 後にのみ判明する失敗（型不完全、未解決名）があるため、
codegen 時のカスケード検出を完全に除去することはできない。
しかし、`calls_unavailable` が事前に設定されていれば:
1. 明らかに失敗する関数の codegen をスキップできる
2. カスケード伝播の初期集合が正確になる
3. fixpoint ループの収束が早くなる

具体的な変更:

```rust
// generate_inline_fns の冒頭で、事前に unavailable と判定された関数をスキップ
for (name, func_def) in &fns {
    if result.inline_fn_dict.is_calls_unavailable(*name) {
        gen_results.push((*name, InlineGenResult::CallsUnavailable));
        continue;
    }
    // ... 既存の codegen 処理 ...
}
```

### Phase 4: `analyze_all_macros` シグネチャ変更の波及

`inline_fn_dict` を `&mut InlineFnDict` に変更するため、呼び出し元も修正。

**`src/infer_api.rs` L396-406**:

```rust
// 変更前:
infer_ctx.analyze_all_macros(
    &mut pp,
    ...
    Some(&inline_fn_dict),  // &InlineFnDict
    ...
);

// 変更後:
infer_ctx.analyze_all_macros(
    &mut pp,
    ...
    Some(&mut inline_fn_dict),  // &mut InlineFnDict
    ...
);
```

## 変更ファイル一覧

| ファイル | 変更内容 |
|----------|----------|
| `src/inline_fn.rs` | `called_functions`, `calls_unavailable` フィールド追加、API 追加 |
| `src/macro_infer.rs` | `check_inline_fn_availability()`, `propagate_unavailable_cross_domain()` 追加、`analyze_all_macros` シグネチャ変更 |
| `src/rust_codegen.rs` | `generate_inline_fns()` の ad-hoc 依存性構築を InlineFnDict ベースに変更、`CallsUnavailable` 結果の追加 |
| `src/infer_api.rs` | `analyze_all_macros` 呼び出しの `&` → `&mut` 変更 |

## 期待される効果

| ケース | 変更前 | 変更後 |
|--------|--------|--------|
| `Perl_newSV_type` | `[UNRESOLVED_NAMES]` | `[UNRESOLVED_NAMES]` (変化なし) |
| `Perl_newSV_type_mortal` | 正常生成（呼び出し先が失敗） | `[CASCADE_UNAVAILABLE]` |
| `SvIMMORTAL` | `[CASCADE_UNAVAILABLE]` | `[CASCADE_UNAVAILABLE]` (変化なし) |

主な改善点:
- `Perl_newSV_type_mortal` のような「呼び出し先が失敗しているのに正常生成される」
  ケースが検出されるようになる
- E0425 エラーの削減（コンパイル不可能なコードが出力されなくなる）

## 検証

```bash
# 1. 全テスト通過
cargo test

# 2. Perl_newSV_type_mortal がカスケード検出されること
cargo run -- --auto --gen-rust samples/xs-wrapper.h --bindings samples/bindings.rs 2>/dev/null \
  | grep -A2 'Perl_newSV_type'
# 期待: Perl_newSV_type_mortal が [CASCADE_UNAVAILABLE] になる

# 3. stats が悪化していないこと（inline_fns_cascade が 1 増える程度）
cargo run -- --auto --gen-rust samples/xs-wrapper.h --bindings samples/bindings.rs 2>&1 | tail -5

# 4. 統合ビルドテスト
~/blob/libperl-rs/12-macrogen-2-build.zsh
grep -c 'error\[E0425\]' tmp/build-error.log
# 期待: E0425 エラー数が減少
```
