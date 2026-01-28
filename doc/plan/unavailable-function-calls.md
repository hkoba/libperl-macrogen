# hidden 関数呼び出しを含むマクロの生成抑制

## 概要

`__attribute__((visibility("hidden")))` が付与された C 関数（例: `Perl_wait4pid`, `Perl_magic_clearenv`）は、
bindgen が bindings.rs に出力しない。そのような関数を呼び出すマクロ関数を Rust コードとして
生成すると、未定義関数エラー (E0425) となる。

## 問題の例

```
error[E0425]: cannot find function `Perl_wait4pid` in this scope
error[E0425]: cannot find function `Perl_magic_clearenv` in this scope
```

## 設計方針

### 2つのアプローチ比較

| 方針 | 説明 | メリット | デメリット |
|------|------|----------|------------|
| **方針1: bindings.rs 照合** | AST 内の関数呼び出しを収集し、bindings.rs/macros に存在するか確認 | シンプル、正確 | hidden 以外の理由で欠落した関数も検出 |
| **方針2: hidden 属性追跡** | パーサーで hidden 属性を収集、マクロ推論時にチェック | 明示的な理由付け | パーサー修正が必要、複雑 |

**推奨: 方針1**

方針1を採用する理由：
- パーサーの修正が不要
- bindings.rs に存在しない関数は hidden 以外の理由でも欠落する可能性がある
- 最終的に「呼び出し可能か」が重要であり、hidden かどうかは二次的

### 判定ロジック

関数呼び出し `foo(...)` について、以下のすべてに該当しない場合「利用不可」：

1. `RustDeclDict.fns` に存在（bindgen 生成の C 関数）
2. `MacroInferContext.macros` に存在（マクロ関数）
3. 既知のビルトイン関数（`__builtin_expect` など）

## 実装設計

### データ構造の変更

```rust
// MacroInferInfo に追加
pub struct MacroInferInfo {
    // ...既存フィールド...

    /// 利用不可関数の呼び出しを含む（直接または推移的）
    pub calls_unavailable: bool,

    /// 呼び出される関数名の集合（マクロ以外）
    pub called_functions: HashSet<InternedStr>,
}
```

### 処理フロー

```
1. マクロ本体のパース
       ↓
2. AST から関数呼び出しを収集 (called_functions)
       ↓
3. 各 called_function を判定:
   - bindings.rs に存在? → OK
   - macros に存在? → OK (uses として記録済み)
   - ビルトイン? → OK
   - それ以外 → calls_unavailable = true
       ↓
4. used_by を辿って calls_unavailable を伝播（推移閉包）
       ↓
5. should_include_macro で calls_unavailable をチェック
```

### 関数呼び出しの収集

既存の `collect_uses_from_expr` を拡張または新規メソッドを追加：

```rust
/// AST から関数呼び出しのみを収集（マクロ呼び出しは除く）
fn collect_function_calls_from_expr(
    expr: &Expr,
    calls: &mut HashSet<InternedStr>,
) {
    match &expr.kind {
        ExprKind::Call { func, args } => {
            if let ExprKind::Ident(name) = &func.kind {
                calls.insert(*name);
            }
            // 再帰処理...
        }
        // ...他のケース...
    }
}
```

### 利用可能性チェック

```rust
fn check_function_availability(
    &mut self,
    rust_decl_dict: Option<&RustDeclDict>,
) {
    let bindings_fns: HashSet<&str> = rust_decl_dict
        .map(|d| d.fns.keys().map(|s| s.as_str()).collect())
        .unwrap_or_default();

    let builtin_fns: HashSet<&str> = [
        "__builtin_expect",
        "__builtin_offsetof",
        // ...他のビルトイン...
    ].into_iter().collect();

    for (name, info) in &mut self.macros {
        for &called_fn in &info.called_functions {
            let fn_name = self.interner.get(called_fn);

            // マクロとして存在する場合はスキップ
            if self.macros.contains_key(&called_fn) {
                continue;
            }

            // bindings.rs に存在する場合はOK
            if bindings_fns.contains(fn_name) {
                continue;
            }

            // ビルトイン関数の場合はOK
            if builtin_fns.contains(fn_name) {
                continue;
            }

            // それ以外は利用不可
            info.calls_unavailable = true;
            break;
        }
    }
}
```

### 推移閉包の伝播

既存の `propagate_flag_via_used_by` を活用：

```rust
// Step N: 利用不可関数呼び出しの推移閉包
let unavailable_initial: HashSet<InternedStr> = self.macros
    .iter()
    .filter(|(_, info)| info.calls_unavailable)
    .map(|(name, _)| *name)
    .collect();
self.propagate_unavailable_via_used_by(&unavailable_initial);
```

### フィルタリング

```rust
fn should_include_macro(&self, info: &MacroInferInfo) -> bool {
    if !info.is_target { return false; }
    if !info.has_body { return false; }
    if !info.is_function { return false; }

    // 利用不可関数を呼び出すマクロは除外
    if info.calls_unavailable { return false; }

    true
}
```

## 実装手順

| Step | 内容 |
|------|------|
| 1 | `MacroInferInfo` に `calls_unavailable` と `called_functions` を追加 |
| 2 | `collect_function_calls_from_expr` を実装（または既存メソッドを拡張） |
| 3 | マクロ推論時に `called_functions` を収集 |
| 4 | `check_function_availability` を実装 |
| 5 | `propagate_flag_via_used_by` を拡張して `calls_unavailable` を伝播 |
| 6 | `should_include_macro` に `calls_unavailable` チェックを追加 |
| 7 | テストと検証 |

## 変更対象ファイル

| ファイル | 変更内容 |
|----------|----------|
| `src/macro_infer.rs` | `MacroInferInfo` の拡張、収集・チェック・伝播ロジック |
| `src/rust_codegen.rs` | `should_include_macro` の修正 |

## テスト方法

```bash
# 結合テスト
~/blob/libperl-rs/12-macrogen-2-build.zsh

# E0425 エラーの確認
grep "cannot find function" tmp/build-error.log | grep -v "__FILE__\|__LINE__" | wc -l

# 特定の関数のエラー確認
grep "Perl_wait4pid\|Perl_magic_clearenv" tmp/build-error.log
```

## 注意事項

- `__FILE__`, `__LINE__`, `__VA_ARGS__` などは別問題として扱う
- ビルトイン関数のリストは必要に応じて拡張
- inline 関数の扱いは別途検討が必要かもしれない

## 想定される効果

- hidden 関数に起因する E0425 エラーの削減
- 呼び出し不可能な関数を含むマクロが自動的に除外される
