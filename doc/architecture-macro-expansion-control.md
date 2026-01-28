# マクロ展開制御アーキテクチャ

## 概要

本ドキュメントは、C ヘッダのパースから Rust コード生成に至る流れの中で、
マクロをどのように展開・制御するかを説明する。

**関連ドキュメント**: [C Inline 関数の処理アーキテクチャ](./architecture-inline-function-processing.md)

## パイプライン全体図

```
┌─────────────────────────────────────────────────────────────────────────┐
│                           入力ファイル                                    │
│  wrapper.h, bindings.rs, apidoc (embed.fnc)                             │
└─────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────┐
│ Stage 1: 前処理・初期化                                                  │
│                                                                         │
│  ┌──────────────────┐    ┌──────────────────┐    ┌──────────────────┐  │
│  │  Preprocessor    │    │  RustDeclDict    │    │  ApidocDict      │  │
│  │  (PPConfig)      │    │  (bindings.rs)   │    │  (embed.fnc)     │  │
│  └────────┬─────────┘    └────────┬─────────┘    └────────┬─────────┘  │
│           │                       │                       │            │
│           │    ┌──────────────────┴───────────────────────┘            │
│           │    │                                                        │
│           ▼    ▼                                                        │
│  ┌─────────────────────────────────────────────┐                       │
│  │ skip_expand_macros に bindings 定数を登録     │ ← 制御点 A          │
│  │ (infer_api.rs:214-218)                       │                       │
│  └─────────────────────────────────────────────┘                       │
└─────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────┐
│ Stage 2: パース & データ収集                                             │
│                                                                         │
│  ┌──────────────────────────────────────────────────────────────────┐  │
│  │ Parser.parse_each_with_pp()                                       │  │
│  │   - FieldsDict 収集 (構造体フィールド情報)                          │  │
│  │   - EnumDict 収集 (enum 情報)                                      │  │
│  │   - InlineFnDict 収集 (inline 関数)                                │  │
│  │   - _SV_HEAD 呼び出し検出 → SV ファミリー構築                        │  │
│  └──────────────────────────────────────────────────────────────────┘  │
│                                                                         │
│  この段階でのマクロ展開:                                                 │
│  - Preprocessor が on-demand でマクロを展開                             │
│  - skip_expand_macros にあるマクロは展開されない                         │
│  - NoExpandRegistry で再帰展開を防止                                    │
└─────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────┐
│ Stage 3: マクロ型推論 (MacroInferContext)                                │
│                                                                         │
│  ┌─────────────────────────────────────────────────────────────────┐   │
│  │ analyze_all_macros() の処理順序:                                  │   │
│  │                                                                   │   │
│  │ 1. build_macro_info()                                            │   │
│  │    ├── NoExpandSymbols を設定 ← 制御点 B                         │   │
│  │    │   (assert, assert_, SvANY を展開抑制)                        │   │
│  │    │                                                              │   │
│  │    └── TokenExpander.expand_with_calls()                         │   │
│  │        ├── no_expand セット ← 制御点 C                            │   │
│  │        ├── bindings_consts ← 制御点 D                            │   │
│  │        └── called_macros に呼び出しを記録                         │   │
│  │                                                                   │   │
│  │ 2. build_use_relations() - def-use グラフ構築                     │   │
│  │                                                                   │   │
│  │ 3. propagate_flag_via_used_by() - THX/pasting フラグ伝播          │   │
│  │                                                                   │   │
│  │ 4. check_function_availability() - 関数可用性チェック             │   │
│  │    └── bindings.rs, inline_fn_dict, builtins を確認              │   │
│  │                                                                   │   │
│  │ 5. infer_types_in_dependency_order() - 依存順で型推論            │   │
│  └─────────────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────┐
│ Stage 4: Rust コード生成 (CodegenDriver)                                 │
│                                                                         │
│  ┌─────────────────────────────────────────────────────────────────┐   │
│  │ generate_all_macros()                                            │   │
│  │                                                                   │   │
│  │ 各マクロについて:                                                 │   │
│  │  1. is_function_available() で呼び出し関数をチェック ← 制御点 E   │   │
│  │     - マクロとして存在？                                          │   │
│  │     - bindings.rs に関数定義？                                    │   │
│  │     - inline 関数として存在？                                     │   │
│  │     - builtin 関数？                                              │   │
│  │                                                                   │   │
│  │  2. 不可用な関数呼び出しがあれば CallsUnavailable                 │   │
│  │                                                                   │   │
│  │  3. expr_to_rust() で AST → Rust コード変換                       │   │
│  │     └── escape_rust_keyword() で識別子変換 ← 制御点 F             │   │
│  └─────────────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────┐
│                           出力                                           │
│  generated_macros.rs (Rust 関数定義)                                    │
└─────────────────────────────────────────────────────────────────────────┘
```

---

## 制御点の詳細

### 制御点 A: Preprocessor の skip_expand_macros

**場所**: `src/preprocessor.rs:461`, `src/infer_api.rs:214-218`

**役割**: 指定されたマクロ名を**グローバルに**展開抑制する

**登録方法**:
```rust
// Preprocessor に直接登録
pp.add_skip_expand_macro(interned_name);

// 現在の実装: bindings.rs の定数名を自動登録
if let Some(ref dict) = rust_decl_dict {
    for name in dict.consts.keys() {
        let interned = pp.interner_mut().intern(name);
        pp.add_skip_expand_macro(interned);
    }
}
```

**効果**:
- パース時点でマクロが展開されない
- AST に識別子として残る
- 後続の型推論・コード生成で定数名として処理される

**使用例**: `SVf_NOK`, `SVf_POK` などの定数マクロ

---

### 制御点 B: NoExpandSymbols

**場所**: `src/macro_infer.rs:30-58`

**役割**: 特定のマクロを**型推論時に**展開抑制し、関数呼び出しとして AST に残す

**現在登録されているシンボル**:
```rust
pub struct NoExpandSymbols {
    pub assert: InternedStr,      // assert マクロ
    pub assert_: InternedStr,     // assert_ マクロ (Perl 独自)
    pub sv_any: InternedStr,      // SvANY マクロ
}
```

**拡張方法**:
```rust
// NoExpandSymbols に新しいフィールドを追加
pub struct NoExpandSymbols {
    pub assert: InternedStr,
    pub assert_: InternedStr,
    pub sv_any: InternedStr,
    pub new_macro: InternedStr,  // 新規追加
}

impl NoExpandSymbols {
    pub fn new(interner: &mut StringInterner) -> Self {
        Self {
            // ...
            new_macro: interner.intern("NEW_MACRO"),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = InternedStr> {
        [self.assert, self.assert_, self.sv_any, self.new_macro].into_iter()
    }
}
```

**効果**:
- TokenExpander での展開時にスキップされる
- マクロ本体の AST に `NEW_MACRO(...)` として残る
- 後段で特別な処理が可能

---

### 制御点 C: TokenExpander の no_expand セット

**場所**: `src/token_expander.rs:41`, `src/token_expander.rs:99-101`

**役割**: TokenExpander インスタンス単位での展開抑制

**設定方法**:
```rust
let mut expander = TokenExpander::new(macro_table, interner, files);

// 展開抑制を追加
for sym in no_expand.iter() {
    expander.add_no_expand(sym);
}
```

**判定ロジック** (`src/token_expander.rs:137-143`):
```rust
fn expand_mut(&mut self, tokens: &[Token], visited: &mut HashSet<InternedStr>) -> Vec<Token> {
    // ...
    TokenKind::Ident(id) => {
        // 1. no_expand チェック
        if self.no_expand.contains(&id) {
            self.called_macros.insert(id);  // 呼び出しは記録
            // 展開しない
        }
        // 2. bindings_consts チェック
        else if let Some(consts) = &self.bindings_consts {
            if consts.contains_key(name) {
                self.called_macros.insert(id);
                // 展開しない
            }
        }
        // 3. 再帰防止チェック
        else if visited.contains(&id) {
            // 展開しない
        }
        // 4. 展開実行
        else {
            // expand...
        }
    }
}
```

---

### 制御点 D: bindings_consts (KeySet)

**場所**: `src/token_expander.rs:45`, `src/macro_infer.rs:705-706`

**役割**: bindings.rs の定数名を展開抑制（制御点 A と連携）

**設定方法**:
```rust
if let Some(dict) = rust_decl_dict {
    expander.set_bindings_consts(&dict.consts);
}
```

**特徴**:
- `KeySet` trait を使用して型を隠蔽
- 定数の値ではなく、名前の存在のみをチェック

---

### 制御点 D': wrapped_macros (assert 保存機構)

**場所**: `src/preprocessor.rs:552-554`, `src/pipeline.rs:262-268`

**役割**: 特定のマクロを展開しつつ、**元の引数情報を保存**する

**背景**:
- Perl の `assert` マクロは `DEBUGGING` 未定義時に `((void)0)` に展開される
- 通常の展開では条件式が消失し、Rust の `assert!()` を生成できない
- `wrapped_macros` に登録されたマクロは、展開結果を `MacroBegin`/`MacroEnd` マーカーで囲む
- Parser がこのマーカーを検出して、**元の引数から** `Assert` AST ノードを作成

**設定方法**:
```rust
// Pipeline API
Pipeline::builder("wrapper.h")
    .with_codegen_defaults()  // ← assert, assert_ を wrapped_macros に登録
    .build()?;

// with_codegen_defaults() の実装
pub fn with_codegen_defaults(mut self) -> Self {
    self.preprocess.wrapped_macros = vec![
        "assert".to_string(),
        "assert_".to_string(),
    ];
    self
}
```

**処理フロー**:
```
assert(cond)
    │
    ▼ Preprocessor で展開
┌─────────────────────────────────────────────────┐
│ MacroBegin { name: "assert", args: [cond] }     │
│ ((void)0)  ← 展開結果（DEBUGGING 未定義時）      │
│ MacroEnd                                        │
└─────────────────────────────────────────────────┘
    │
    ▼ Parser で検出
┌─────────────────────────────────────────────────┐
│ ExprKind::Assert {                              │
│     kind: AssertKind::Assert,                   │
│     condition: Box::new(cond)  ← 元の引数から復元│
│ }                                               │
└─────────────────────────────────────────────────┘
    │
    ▼ RustCodegen で変換
┌─────────────────────────────────────────────────┐
│ assert!((cond) != 0)                            │
└─────────────────────────────────────────────────┘
```

**重要**: `with_codegen_defaults()` を呼ばないと、inline 関数内の `assert` が
`{ 0; };` のような空文に変換されてしまう。

---

### 制御点 E: is_function_available()

**場所**: `src/rust_codegen.rs:2821-2876`

**役割**: コード生成時に呼び出される関数の存在確認

**チェック順序**:
```rust
fn is_function_available(&self, fn_id: InternedStr, fn_name: &str, result: &InferResult) -> bool {
    // 1. マクロとして存在？
    if result.infer_ctx.macros.contains_key(&fn_id) {
        return true;
    }

    // 2. bindings.rs に関数定義？
    if let Some(dict) = &result.rust_decl_dict {
        if dict.fns.contains_key(fn_name) {
            return true;
        }
    }

    // 3. inline 関数として存在？
    if result.inline_fn_dict.get(fn_id).is_some() {
        return true;
    }

    // 4. builtin 関数？
    if BUILTIN_FNS.contains(&fn_name) {
        return true;
    }

    false  // 不可用
}
```

**効果**:
- 不可用な関数を呼び出すマクロは `CallsUnavailable` ステータス
- コメントアウトされた形で出力

---

### 制御点 F: escape_rust_keyword()

**場所**: `src/rust_codegen.rs:16-42`

**役割**: 識別子を Rust コードに変換

**現在の処理**:
```rust
fn escape_rust_keyword(name: &str) -> String {
    match name {
        "__FILE__" => "file!()".to_string(),
        "__LINE__" => "line!()".to_string(),
        _ if RUST_KEYWORDS.contains(&name) => format!("r#{}", name),
        _ => name.to_string(),
    }
}
```

**拡張例**: 特定のマクロ名を Rust 関数呼び出しに変換
```rust
fn escape_rust_keyword(name: &str) -> String {
    match name {
        "__FILE__" => "file!()".to_string(),
        "__LINE__" => "line!()".to_string(),
        "SvREFCNT_inc" => "sv_refcnt_inc".to_string(),  // 例
        _ if RUST_KEYWORDS.contains(&name) => format!("r#{}", name),
        _ => name.to_string(),
    }
}
```

---

## ユースケース別ガイド

### ユースケース 1: マクロ X の Rust 関数生成を抑制し、呼び出し元でインライン展開させたい

**目的**: マクロ `FOO(x)` を、Rust 関数 `FOO` として出力せず、呼び出し元でインライン展開する

**実装方針**:

```
FOO を skip_expand_macros に登録 → NG（パース時に展開されなくなる）
FOO を NoExpandSymbols に登録 → NG（関数呼び出しとして残る）

正解: 何も登録しない（デフォルト動作）
```

**理由**:
- デフォルトでは、マクロは TokenExpander により展開される
- `build_macro_info()` 内で `expand_with_calls()` が呼ばれ、マクロ本体が展開される
- 展開後の AST がそのまま Rust コードに変換される

**追加制御が必要な場合**:
- コード生成時に特定マクロをスキップするには `CodegenDriver` を修正
- `generate_all_macros()` でマクロ名をチェックしてスキップ

```rust
// src/rust_codegen.rs の generate_all_macros() に追加
fn generate_all_macros(&mut self, result: &InferResult) {
    let skip_codegen = ["FOO", "BAR"];  // 出力しないマクロ

    for (name, info) in &result.infer_ctx.macros {
        let name_str = result.preprocessor.interner().get(*name);
        if skip_codegen.contains(&name_str) {
            continue;  // スキップ
        }
        // 通常の生成処理...
    }
}
```

---

### ユースケース 2: マクロ X に対して Rust 関数を生成し、呼び出し元でもその関数を呼び出したい

**目的**: マクロ `FOO(x)` を Rust 関数 `FOO` として出力し、`BAR` マクロ内の `FOO(y)` 呼び出しも `FOO(y)` のまま残す

**実装方針**:

**Step 1**: `FOO` を展開抑制に登録

```rust
// 方法 A: NoExpandSymbols に追加（型推論で使用）
pub struct NoExpandSymbols {
    // ...
    pub foo: InternedStr,
}

// 方法 B: skip_expand_macros に追加（パース時から抑制）
pp.add_skip_expand_macro(pp.interner_mut().intern("FOO"));
```

**Step 2**: `is_function_available()` で `FOO` を可用として認識させる

```rust
// FOO は macros に含まれるので、自動的に可用と判定される
// ただし、循環参照に注意（FOO が FOO を呼ぶ場合）
```

**Step 3**: コード生成で両方を出力

デフォルトで動作するはず：
- `FOO` のマクロ定義 → Rust 関数 `FOO` として出力
- `BAR` 内の `FOO(y)` → `FOO(y)` として出力（展開されていないため）

**注意点**:
- 展開抑制により、`FOO` の本体は `FOO` 自身の定義から取得
- `BAR` 内では `FOO(y)` が関数呼び出しとして残る
- 依存関係グラフで `BAR` が `FOO` を使用することが記録される

---

### ユースケース 3: 特定マクロを常に bindings.rs の定数として扱いたい

**目的**: `MY_CONST` を bindings.rs で定義された定数として扱う

**実装方針**:

**Option A**: bindings.rs に追加
```rust
// samples/bindings.rs
pub const MY_CONST: u32 = 42;
```
→ 自動的に `skip_expand_macros` に登録される

**Option B**: プログラム的に登録
```rust
// src/infer_api.rs の run_inference_with_preprocessor() 内
let my_const = pp.interner_mut().intern("MY_CONST");
pp.add_skip_expand_macro(my_const);
```

---

## データフロー詳細

### マクロ展開判定のフローチャート

```
                    ┌─────────────────┐
                    │  識別子 TOKEN   │
                    └────────┬────────┘
                             │
                             ▼
              ┌──────────────────────────────┐
              │ skip_expand_macros に含まれる？ │
              └──────────────┬───────────────┘
                      │              │
                     Yes            No
                      │              │
                      ▼              ▼
              ┌────────────┐  ┌──────────────────────────┐
              │ 展開しない  │  │ no_expand に含まれる？    │
              └────────────┘  └──────────────┬───────────┘
                                      │              │
                                     Yes            No
                                      │              │
                                      ▼              ▼
                              ┌────────────┐  ┌──────────────────────────┐
                              │ 展開しない  │  │ bindings_consts に含まれる？│
                              │ (記録する)  │  └──────────────┬───────────┘
                              └────────────┘          │              │
                                                     Yes            No
                                                      │              │
                                                      ▼              ▼
                                              ┌────────────┐  ┌──────────────────┐
                                              │ 展開しない  │  │ visited に含まれる？│
                                              │ (記録する)  │  └────────┬─────────┘
                                              └────────────┘          │        │
                                                                     Yes      No
                                                                      │        │
                                                                      ▼        ▼
                                                              ┌────────────┐ ┌────────────┐
                                                              │ 展開しない  │ │ マクロ展開  │
                                                              │ (再帰防止) │ │ 実行       │
                                                              └────────────┘ └────────────┘
```

### 記録される情報

| タイミング | 記録先 | 情報 |
|------------|--------|------|
| TokenExpander 展開時 | `expanded_macros` | 実際に展開されたマクロ名 |
| TokenExpander 展開時 | `called_macros` | 呼び出されたマクロ名（展開有無問わず） |
| MacroInferInfo 構築時 | `info.uses` | 使用するマクロ/関数名 |
| 依存関係構築時 | `MacroInferContext.used_by` | 逆依存グラフ |

---

## ファイル別責務まとめ

| ファイル | 責務 |
|----------|------|
| `preprocessor.rs` | マクロ定義の管理、トークン展開、skip_expand_macros |
| `token_expander.rs` | マクロ本体の展開、no_expand/bindings_consts チェック |
| `macro_infer.rs` | マクロ型推論、NoExpandSymbols、def-use グラフ |
| `infer_api.rs` | パイプライン統合、bindings.rs 読み込み、制御点の設定 |
| `rust_codegen.rs` | Rust コード生成、関数可用性チェック、識別子変換 |
| `rust_decl.rs` | bindings.rs のパース、RustDeclDict 構築 |
| `pipeline.rs` | 高レベル API、設定の受け渡し |

---

## 推奨される拡張パターン

### 新しい展開抑制マクロを追加する場合

1. **静的に定義**: `NoExpandSymbols` に追加
2. **動的に定義**: `pp.add_skip_expand_macro()` を呼び出し
3. **bindings.rs ベース**: bindings.rs に定数として追加

### 新しい「可用な関数」を追加する場合

1. **マクロとして**: 通常のマクロ定義が存在すれば自動認識
2. **bindings.rs 関数**: bindings.rs に `extern "C"` 関数として追加
3. **inline 関数**: C ヘッダに `static inline` 関数として定義
4. **builtin**: `is_function_available()` の builtin リストに追加

### コード生成をカスタマイズする場合

1. **識別子変換**: `escape_rust_keyword()` を拡張
2. **式変換**: `expr_to_rust()` を拡張
3. **マクロスキップ**: `generate_all_macros()` でフィルタリング
