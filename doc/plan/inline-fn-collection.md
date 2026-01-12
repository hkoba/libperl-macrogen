# inline 関数の収集と型推論・Rust コード生成への活用

## 目標

is_target なヘッダーファイルに含まれる inline 関数を `parse_each` 段階で収集し：

1. **型推論**: `MacroInferContext` → `SemanticAnalyzer` に渡して、マクロ内で使用される
   inline 関数呼び出しの型推論に活用
2. **Rust コード生成**: C の AST を保持し、後で Rust 関数の生成に活用

## 背景

### 現状

- `RustDeclDict` が bindings.rs から Rust 関数シグネチャを収集
- `SemanticAnalyzer` は `lookup_rust_decl_param_type()` で関数の引数型を解決
- inline 関数は検出されているが、シグネチャは収集されていない（名前を出力するのみ）

### 問題

マクロ内で inline 関数を呼び出している場合、その引数や戻り値の型が `<unknown>` になる。

### 期待する動作

```c
// inline関数
static inline SV* newSVpvn(const char* s, STRLEN len) { ... }

// マクロ
#define sv_setpvn(sv, ptr, len) newSVpvn(ptr, len)
```

`sv_setpvn` マクロの解析時に、`newSVpvn` の引数型と戻り値型が利用可能になる。

## 実装計画

### Step 1: データ構造の定義

**新規ファイル: src/inline_fn.rs**

AST (`FunctionDef`) をそのまま保持し、型情報は AST から直接取得する。
シグネチャ用の追加構造体は不要。

```rust
use std::collections::HashMap;
use crate::ast::FunctionDef;
use crate::intern::InternedStr;

/// inline 関数辞書
///
/// FunctionDef をそのまま保持し、型情報は AST から直接取得する。
#[derive(Debug, Default)]
pub struct InlineFnDict {
    fns: HashMap<InternedStr, FunctionDef>,
}

impl InlineFnDict {
    pub fn new() -> Self {
        Self::default()
    }

    /// inline 関数を登録
    pub fn insert(&mut self, name: InternedStr, func_def: FunctionDef) {
        self.fns.insert(name, func_def);
    }

    /// inline 関数を取得
    pub fn get(&self, name: InternedStr) -> Option<&FunctionDef> {
        self.fns.get(&name)
    }

    /// 全ての inline 関数を走査
    pub fn iter(&self) -> impl Iterator<Item = (&InternedStr, &FunctionDef)> {
        self.fns.iter()
    }

    /// inline 関数の数
    pub fn len(&self) -> usize {
        self.fns.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fns.is_empty()
    }
}
```

### Step 2: FunctionDef の収集

**src/inline_fn.rs に追加**

```rust
impl InlineFnDict {
    /// FunctionDef から inline 関数を収集
    pub fn collect_from_function_def(&mut self, func_def: &FunctionDef) {
        if !func_def.specs.is_inline {
            return;
        }

        let name = match func_def.declarator.name {
            Some(n) => n,
            None => return,
        };

        self.insert(name, func_def.clone());
    }
}
```

**注意**: `FunctionDef` は既に `#[derive(Debug, Clone)]` が付いているのでクローン可能。

### Step 3: parse_each での収集

**src/main.rs (infer-macro-types モード)**

```rust
// 既存のコード
let mut inline_count = 0usize;
// 新規追加
let mut inline_fn_dict = InlineFnDict::new();

parser.parse_each(|result, _loc, path, interner| {
    if let Ok(ref decl) = result {
        fields_dict.collect_from_external_decl(decl, decl.is_target(), interner);

        // inline関数を収集
        if decl.is_target() {
            if let ExternalDecl::FunctionDef(func_def) = decl {
                if func_def.specs.is_inline {
                    inline_fn_dict.collect_from_function_def(func_def);
                    inline_count += 1;
                }
            }
        }
    }
    std::ops::ControlFlow::Continue(())
});
```

### Step 4: MacroInferContext への受け渡し

**src/macro_infer.rs**

`analyze_all_macros` と `infer_macro_types` のシグネチャに `inline_fn_dict` を追加:

```rust
pub fn analyze_all_macros<'a>(
    &mut self,
    macro_table: &MacroTable,
    interner: &'a StringInterner,
    files: &FileRegistry,
    apidoc: Option<&'a ApidocDict>,
    fields_dict: Option<&'a FieldsDict>,
    rust_decl_dict: Option<&'a RustDeclDict>,
    inline_fn_dict: Option<&'a InlineFnDict>,  // 追加
    typedefs: &HashSet<InternedStr>,
    thx_symbols: (InternedStr, InternedStr, InternedStr),
)
```

### Step 5: SemanticAnalyzer への統合

**src/semantic.rs**

1. フィールド追加:
```rust
/// inline 関数シグネチャ辞書
inline_fn_dict: Option<&'a InlineFnDict>,
```

2. コンストラクタ更新

3. lookup メソッド追加（AST から直接型を解決）:
```rust
/// inline 関数から引数型を取得
fn lookup_inline_fn_param_type(
    &self,
    func_name: InternedStr,
    arg_index: usize
) -> Option<Type> {
    let dict = self.inline_fn_dict?;
    let func_def = dict.get(func_name)?;

    // Declarator から ParamList を取得
    let param_list = func_def.declarator.derived.iter()
        .find_map(|d| match d {
            DerivedDecl::Function(params) => Some(params),
            _ => None,
        })?;

    let param = param_list.params.get(arg_index)?;

    // ParamDecl から Type を構築
    let base_ty = self.resolve_decl_specs(&param.specs);
    if let Some(ref declarator) = param.declarator {
        Some(self.apply_declarator(base_ty, declarator))
    } else {
        Some(base_ty)
    }
}

/// inline 関数から戻り値型を取得
fn lookup_inline_fn_return_type(&self, func_name: InternedStr) -> Option<Type> {
    let dict = self.inline_fn_dict?;
    let func_def = dict.get(func_name)?;

    // DeclSpecs から戻り値の基本型を取得
    let base_ty = self.resolve_decl_specs(&func_def.specs);

    // Declarator のポインタ等を適用（関数の DerivedDecl::Function より前の部分）
    Some(self.apply_return_type_declarator(base_ty, &func_def.declarator))
}
```

**注意**: `resolve_decl_specs()` と `apply_declarator()` は既存のメソッドを再利用。
`apply_return_type_declarator()` は新規追加が必要（Declarator のポインタ部分のみ適用）。

4. `collect_call_constraints` を更新して inline 関数の型も参照:
```rust
// 既存: RustDeclDict から
if let Some(ty) = self.lookup_rust_decl_param_type(func_name, i) { ... }
// 追加: InlineFnDict から
else if let Some(ty) = self.lookup_inline_fn_param_type(func_name, i) { ... }
```

## 修正対象ファイル

1. **src/inline_fn.rs** (新規)
   - `InlineFnDict` 定義
   - `collect_from_function_def()` 実装

2. **src/lib.rs**
   - `mod inline_fn;` 追加
   - `pub use inline_fn::InlineFnDict;` 追加

3. **src/main.rs**
   - `InlineFnDict` の作成と収集
   - `MacroInferContext` への受け渡し

4. **src/macro_infer.rs**
   - `analyze_all_macros` シグネチャ更新
   - `infer_macro_types` シグネチャ更新
   - `SemanticAnalyzer` 作成時に `inline_fn_dict` を渡す

5. **src/semantic.rs**
   - `inline_fn_dict` フィールド追加
   - コンストラクタ更新
   - `lookup_inline_fn_param_type()` 追加
   - `lookup_inline_fn_return_type()` 追加
   - `apply_return_type_declarator()` 追加（戻り値型のポインタ適用）
   - `collect_call_constraints()` 更新

## Rust コード生成への活用（将来）

`InlineFnDict` に保持された `FunctionDef` は、以下のように Rust コード生成に活用できる:

```rust
// inline 関数を Rust 関数に変換
for (name, func_def) in inline_fn_dict.iter() {
    // func_def.body (CompoundStmt) を Rust コードに変換
    // func_def.specs, func_def.declarator からシグネチャを生成
    rust_codegen.emit_inline_function(func_def, interner);
}
```

これにより、C の inline 関数を Rust の `#[inline]` 関数として再実装できる。

## 注意点

1. **名前の衝突**: 同じ名前の inline 関数が複数ファイルにある場合、
   最後に見つかったものが優先される（上書き）

2. **パフォーマンス**: `RustDeclDict` と同様に HashMap ベースなので O(1) ルックアップ

3. **AST のクローン**: `FunctionDef` は `Clone` を derive しているため、
   `parse_each` のコールバック内でクローンして保持できる

4. **既存メソッドの再利用**: `resolve_decl_specs()` と `apply_declarator()` は
   既に `SemanticAnalyzer` に実装済みなので、型解決のロジックを共有できる
