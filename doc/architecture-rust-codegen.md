# Rust コード生成アーキテクチャ

このドキュメントでは、libperl-macrogen における Rust コード生成の仕組みを解説する。

## Perl Build Mode との連携

`InferResult.perl_build_mode` が `Threaded` か `NonThreaded` かによって、
codegen は `is_thx_dependent` 経路の挙動を切り替える。`CodegenDriver` /
`RustCodegen` は内部に `perl_threaded: bool` フィールドを持ち、
`generate()` 開始時に `result.perl_build_mode.is_threaded()` から書き換える。
すべての `is_thx_dependent` 読み取り箇所は `self.perl_threaded && info.is_thx_dependent`
形でガードしており、非 threaded build に対しては:

- 関数シグネチャに `my_perl: *mut PerlInterpreter` を注入しない
- 呼び出し時に `my_perl,` を自動挿入しない
- `arg_index 0` のオフセット補正を入れない
- inline 関数の THX 表示を抑止

これにより同じ libperl-macrogen バイナリで両 build mode 向けの
出力を生成できる。詳細は
[architecture-thx-dependency.md](architecture-thx-dependency.md) と
[plan/non-threaded-perl-support.md](plan/non-threaded-perl-support.md) を参照。


## 概要

コード生成システムは、型推論結果（`InferResult`）から Rust コードを生成する。
2層構造を採用し、責務を明確に分離している。

```
┌─────────────────────────────────────────────────────────────────────┐
│                        InferResult                                  │
│  ・MacroInferContext (マクロ推論結果)                               │
│  ・InlineFnDict (inline 関数辞書)                                   │
│  ・EnumDict (enum バリアント辞書)                                   │
│  ・RustDeclDict (bindings.rs 情報)                                  │
└─────────────────────────────────────────────────────────────────────┘
                              │
                    ┌─────────┴─────────┐
                    ▼                   ▼
          ┌──────────────────┐  ┌──────────────────┐
          │  BindingsInfo    │  │  KnownSymbols    │
          │  ・static_arrays │  │  ・既知シンボル集合│
          │  ・bitfield      │  │  ・未解決検出用    │
          │    _methods      │  │                  │
          └──────────────────┘  └──────────────────┘
                    │                   │
                    └─────────┬─────────┘
                              ▼
┌─────────────────────────────────────────────────────────────────────┐
│                     CodegenDriver                                    │
│  ・出力先 (Writer) の管理                                           │
│  ・セクション構造の制御                                              │
│  ・依存順でのコード生成（カスケード検出付き）                        │
│  ・動的 use libc 生成                                               │
│  ・統計情報の収集                                                    │
└─────────────────────────────────────────────────────────────────────┘
                              │
          ┌───────────────────┼───────────────────┐
          ▼                                       ▼
┌───────────────────────┐               ┌───────────────────────┐
│     RustCodegen       │               │   フォールバック出力   │
│  ・単一関数の生成      │               │  ・PARSE_FAILED       │
│  ・AST → Rust 変換    │               │  ・TYPE_INCOMPLETE    │
│  ・不完全マーカー管理  │               │  ・CALLS_UNAVAILABLE  │
│  ・未解決シンボル検出  │               │  ・CASCADE_UNAVAILABLE│
│  ・libc 使用追跡       │               │  ・UNRESOLVED_NAMES   │
│  ・codegen エラー検出   │               │  ・GENERIC_UNSUPPORTED│
│                       │               │  ・CODEGEN_ERROR      │
└───────────────────────┘               └───────────────────────┘
          │
          ▼
┌─────────────────────────────────────────────────────────────────────┐
│                     GeneratedCode                                    │
│  ・code: String (生成されたコード)                                   │
│  ・incomplete_count: usize (不完全マーカー数)                        │
│  ・unresolved_names: Vec<String> (未解決シンボル名)                  │
│  ・used_libc_fns: HashSet<String> (使用した libc 関数名)            │
│  ・codegen_errors: Vec<String> (検出されたエラー)                    │
└─────────────────────────────────────────────────────────────────────┘

型推論とキャスト生成の詳細は `architecture-type-inference-and-cast.md` を参照。
```

## 主要コンポーネント

### 1. BindingsInfo - bindings.rs 抽出情報

**責務**: RustDeclDict からコード生成に必要な情報を抽出

```rust
pub struct BindingsInfo {
    /// 配列型の extern static 変数名の集合
    pub static_arrays: HashSet<String>,
    /// ビットフィールドのメソッド名集合（構造体名 → メソッド名セット）
    pub bitfield_methods: HashMap<String, HashSet<String>>,
}
```

### 2. KnownSymbols - 既知シンボル集合

**責務**: コード生成時に未解決シンボルを検出するための参照集合

```rust
pub struct KnownSymbols {
    names: HashSet<String>,
}
```

以下のソースからシンボルを収集:
- bindings.rs の関数名、定数名、型名、構造体名、enum 名、extern static 変数名
- マクロ辞書のマクロ名
- inline 関数辞書の関数名
- ビルトイン関数名
- libc 関数名

### 3. CodegenDriver - 出力管理

**責務**: 生成全体の制御、出力先管理、依存順コード生成、統計収集

```rust
pub struct CodegenDriver<'a, W: Write> {
    writer: W,                              // 出力先
    interner: &'a StringInterner,
    enum_dict: &'a EnumDict,
    macro_ctx: &'a MacroInferContext,
    bindings_info: BindingsInfo,            // bindings.rs 抽出情報
    config: CodegenConfig,                  // 生成設定
    stats: CodegenStats,                    // 統計情報
    used_libc_fns: HashSet<String>,         // 使用された libc 関数名
    successfully_generated_inlines: HashSet<InternedStr>, // 正常生成 inline 関数
    generatable_macros: HashSet<InternedStr>, // 生成可能マクロ（inline→macro カスケード検出用）
}
```

#### 主要メソッド

| メソッド | 役割 |
|----------|------|
| `generate()` | 全体の生成エントリポイント |
| `generate_use_statements()` | 動的 use libc 生成 |
| `generate_enum_imports()` | enum インポートを出力 |
| `precompute_macro_generability()` | マクロの生成可能性を trial codegen で事前計算 |
| `generate_inline_fns()` | inline 関数セクション（依存順、カスケード検出） |
| `generate_macros()` | マクロセクション（クロスドメインカスケード検出） |
| `get_macro_status()` | マクロの生成ステータスを判定 |

#### 生成フロー（依存順コード生成）

```
generate()
    │
    ├─ precompute_macro_generability()  // マクロの生成可能性を trial codegen で事前計算
    │   └─ for each macro:
    │       ├─ cascade check + get_macro_status()
    │       ├─ trial codegen (RustCodegen::generate_macro)
    │       └─ is_complete() && !has_unresolved_names() → generatable_macros に追加
    │
    ├─ generate_inline_fns()
    │   │
    │   ├─ Pass 0: 事前スキップ
    │   │   └─ InlineFnDict.is_calls_unavailable() → CallsUnavailable
    │   │
    │   ├─ Pass 1: 残りの inline 関数を個別に生成
    │   │   └─ for each inline_fn:
    │   │       ├─ RustCodegen::generate_inline_fn()
    │   │       └─ 完全なら成功バッファに、不完全なら理由を記録
    │   │
    │   ├─ Pass 1.5: successfully_generated_inlines 集合を構築
    │   │
    │   ├─ Pass 2: カスケード依存の不動点ループ
    │   │   └─ loop:
    │   │       ├─ 成功 inline が利用不可 inline を呼ぶ場合 → 降格
    │   │       ├─ 成功 inline が generatable_macros にないマクロを呼ぶ場合 → 降格
    │   │       └─ 変更がなくなるまで繰り返し
    │   │
    │   └─ Pass 3: inline 関数の出力（最終結果）
    │       └─ 成功/TYPE_INCOMPLETE/CASCADE_UNAVAILABLE/CALLS_UNAVAILABLE/UNRESOLVED_NAMES を出力
    │
    ├─ generate_macros() — クロスドメインカスケード検出
    │   └─ for each macro:
    │       ├─ get_macro_status() で分類
    │       ├─ Success → RustCodegen::generate_macro()
    │       │   └─ 未解決シンボルあり → UNRESOLVED_NAMES としてコメントアウト
    │       │   └─ codegen エラーあり → CODEGEN_ERROR としてコメントアウト
    │       │   └─ 依存先 inline が失敗 → CASCADE_UNAVAILABLE
    │       ├─ GenericUnsupported → GENERIC_UNSUPPORTED としてコメントアウト
    │       ├─ ParseFailed → generate_macro_parse_failed()
    │       ├─ TypeIncomplete → generate_macro_type_incomplete()
    │       └─ CallsUnavailable → generate_macro_calls_unavailable()
    │
    └─ generate_use_statements()  // 動的 use libc（実際に使用した関数のみ）
```

#### 動的 use libc 生成

`use libc::{...}` 文は、コード生成後に実際に使用された libc 関数のみを含む。
`LIBC_FUNCTIONS` リスト（`strcmp`, `strlen`, `memset` 等）と照合し、
使用されたもののみを動的に出力する。

#### カスケード依存検出

カスケード検出は 2 段階で行われる:

**1. 推論段階（`analyze_all_macros` Step 4.6〜4.7）**:
- `check_inline_fn_availability()` で inline 関数の呼び出し先を事前チェック
- `propagate_unavailable_cross_domain()` で macro↔inline の 4 方向推移閉包を計算
- 結果は `InlineFnDict.calls_unavailable` と `MacroInferInfo.calls_unavailable` に記録

**2. codegen 段階**:

**Inline→Inline カスケード**: Pass 2 の不動点ループで検出。
inline 関数 A が inline 関数 B を呼び出し、B がコード生成失敗した場合、
A も CASCADE_UNAVAILABLE として出力する。

**Inline→Macro カスケード**: Pass 2 の不動点ループで検出。
inline 関数が呼び出すマクロが `generatable_macros` に含まれない場合、
その inline 関数も CASCADE_UNAVAILABLE として出力する。
`generatable_macros` は `precompute_macro_generability()` で trial codegen
により事前計算される。

**Macro→Inline クロスドメインカスケード**: `generate_macros()` 内で検出。
マクロが `called_functions` で inline 関数を参照している場合、
その inline 関数が `successfully_generated_inlines` に含まれなければ、
マクロも CASCADE_UNAVAILABLE として出力する。

#### precompute_macro_generability()

inline 関数はマクロより**先**に生成されるため、マクロの codegen 結果を
直接参照できない。この問題を解決するため、inline 関数生成**前**に
マクロの codegen を trial 実行する:

```rust
fn precompute_macro_generability(&mut self, result: &InferResult, known_symbols: &KnownSymbols) {
    // generate_macros() と同じロジック:
    // 1. 対象マクロを収集・依存順ソート
    // 2. カスケードチェック（依存先がgeneratable かどうか）
    // 3. get_macro_status() でステータス判定
    // 4. Success → trial codegen → is_complete() && !has_unresolved_names()
    //    → generatable_macros に追加
}
```

これにより `SvIMMORTAL_INTERP`（codegen incomplete）→ `SvIMMORTAL`（cascade failure）
→ inline 関数（cascade）のような連鎖が inline 生成前に検出可能になる。

### 4. RustCodegen - 単一関数生成

**責務**: 単一の関数/マクロを Rust コードに変換

```rust
pub struct RustCodegen<'a> {
    interner: &'a StringInterner,
    enum_dict: &'a EnumDict,
    macro_ctx: &'a MacroInferContext,
    bindings_info: BindingsInfo,            // bindings.rs 抽出情報
    buffer: String,                         // 生成結果バッファ
    incomplete_count: usize,                // 不完全マーカー数
    current_type_param_map: HashMap<InternedStr, String>,   // ジェネリック型パラメータ
    current_literal_string_params: HashSet<InternedStr>,    // &str パラメータ
    current_return_type: Option<String>,    // 戻り値型文字列
    param_substitutions: HashMap<InternedStr, String>,      // lvalue Call 展開用
    current_param_types: HashMap<InternedStr, String>,      // パラメータ型情報
    known_symbols: &'a KnownSymbols,       // 既知シンボル集合
    current_local_names: HashSet<InternedStr>, // ローカルスコープ名
    unresolved_names: Vec<String>,         // 検出された未解決シンボル
    used_libc_fns: HashSet<String>,        // 使用した libc 関数名
}
```

#### 設計方針

- **使い捨てインスタンス**: 各関数の生成ごとに新規作成
- **不完全マーカー追跡**: 型が不明な箇所に `__UNKNOWN__` 等を出力し、カウント
- **未解決シンボル検出**: `KnownSymbols` と照合して未定義シンボルを検出
- **純粋な変換**: IO を持たず、結果を `GeneratedCode` として返す

#### 主要メソッド

| メソッド | 役割 |
|----------|------|
| `generate_macro()` | マクロを Rust 関数に変換 |
| `generate_inline_fn()` | inline 関数を Rust 関数に変換 |
| `expr_to_rust()` | 式を Rust コードに変換（マクロ用、型推論統合） |
| `expr_to_rust_inline()` | 式を Rust コードに変換（inline 関数用） |
| `stmt_to_rust()` | 文を Rust コードに変換 |
| `type_repr_to_rust()` | TypeRepr を Rust 型文字列に変換 |
| `get_param_type()` | パラメータの宣言型を best-tier 制約から取得 |
| `get_callee_param_type_extended()` | 呼び出し先パラメータ型を取得（bindings → inline → マクロ type_env、最後の段は best-tier + 自前の const 調整） |
| `best_constraint_for_macro_param()` *(自由関数)* | param の全 ExprId 制約から void を除き Tier 最良の TypeRepr を返す共通ヘルパ |
| `try_expand_call_as_lvalue()` | Call 式の lvalue 展開（マクロ用） |
| `try_expand_call_as_lvalue_inline()` | Call 式の lvalue 展開（inline 用） |

#### パラメータ型の解決

`get_param_type()` はパラメータを参照する全 ExprId
（`param.expr_id()` ＋ `type_env.param_to_exprs[param.name]`）の型制約を
収集し、**void 型はスキップ** して **`confidence_tier()` の最良 Tier**
（=数値が最小）を採用する。同タスクは `get_callee_param_type_extended()`
の自家生成マクロ callee パスでも必要なため、共通の自由関数
`best_constraint_for_macro_param(info, param) -> Option<TypeRepr>`
として `src/rust_codegen.rs` 上部に切り出されている。

```rust
// 例: CvHASGV(cv) の cv パラメータに対する制約:
//   - *mut SV  (Tier 4, SvFamilyCast)              ← 旧来の総称
//   - *mut CV  (Tier 3, CommonMacroFieldInference) ← 採用 (best tier)
//   - void     (symbol lookup)                     ← スキップ
```

選択後、`const_pointer_positions` に応じて `make_outer_pointer_const()` /
`make_outer_pointer_mut()` で const/mut を最終調整する。

#### マクロ呼出時の callee パラメータ型解決

`get_callee_param_type_extended(func_name, arg_index) -> Option<UnifiedType>`
は呼び出し先の引数型をルックアップし、`build_arg_string_unified` から
キャスト要否判定（`cast_arg_syn_if_needed`）に渡される。優先順は:

1. `bindings.rs` (`RustDeclDict`)
2. inline 関数の AST (`inline_fn_dict`)
3. **自家生成マクロの type_env** — `best_constraint_for_macro_param`
   と **callee 自身の `const_pointer_positions`** で const/mut 調整

3 番目の段で `get_param_type` と **同じ Tier-best 選択 + 同じ const 調整**
を適用するのが重要。これにより:

- **callee 宣言**: `pub unsafe fn CvHASGV(cv: *const CV)`
- **caller 側の callee 期待型ルックアップ**: `*const CV`

が一致する。以前は callee パスが「最初の非 void 制約」を返していたため、
callee の真の宣言型と乖離して SV→CV 自動キャストが挿入されない不具合
（`expected *const sv, found *mut gv` 系の連鎖エラー）が発生していた。

詳細は `architecture-type-inference-and-cast.md` の「マクロ間呼出境界での
キャスト挿入」節を参照。

#### 不完全マーカー

```rust
fn unknown_marker(&mut self) -> &'static str {
    self.incomplete_count += 1;
    "__UNKNOWN__"
}

fn type_marker(&mut self) -> &'static str {
    self.incomplete_count += 1;
    "__TYPE__"
}
```

### 5. GeneratedCode - 生成結果

```rust
pub struct GeneratedCode {
    pub code: String,               // 生成されたコード
    pub incomplete_count: usize,    // 不完全マーカー数
    pub unresolved_names: Vec<String>, // 未解決シンボル名
    pub used_libc_fns: HashSet<String>, // 使用した libc 関数名
}

impl GeneratedCode {
    pub fn is_complete(&self) -> bool {
        self.incomplete_count == 0
    }
}
```

### 6. CodegenConfig - 生成設定

```rust
pub struct CodegenConfig {
    pub emit_inline_fns: bool,          // inline 関数を出力するか
    pub emit_macros: bool,              // マクロを出力するか
    pub include_source_location: bool,  // ソース位置をコメントに含めるか
    pub use_statements: Vec<String>,    // カスタム use 文
}
```

### 7. CodegenStats - 統計情報

```rust
pub struct CodegenStats {
    pub macros_success: usize,              // 正常生成されたマクロ数
    pub macros_parse_failed: usize,         // パース失敗マクロ数
    pub macros_type_incomplete: usize,      // 型推論失敗マクロ数
    pub macros_calls_unavailable: usize,    // 利用不可関数呼び出しマクロ数
    pub macros_cascade_unavailable: usize,  // カスケード依存でコメントアウトされたマクロ数
    pub macros_unresolved_names: usize,     // 未解決シンボルを含むマクロ数
    pub inline_fns_success: usize,              // 正常生成された inline 関数数
    pub inline_fns_type_incomplete: usize,      // 型推論失敗 inline 関数数
    pub inline_fns_unresolved_names: usize,     // 未解決シンボルを含む inline 関数数
    pub inline_fns_cascade_unavailable: usize,  // カスケード依存 inline 関数数
    pub inline_fns_contains_goto: usize,        // goto を含む inline 関数数
}
```

### 8. GenerateStatus - 生成ステータス

```rust
pub enum GenerateStatus {
    Success,            // 正常生成可能
    ParseFailed,        // パース失敗（トークン列のみ）
    TypeIncomplete,     // 型推論不完全
    CallsUnavailable,   // 利用不可関数を呼び出す
    Skip,               // 対象外（非関数形式マクロ等）
}
```

## C → Rust 変換パターン

### 型変換

| C パターン | Rust 変換 | 理由 |
|------------|-----------|------|
| `(enum_type)expr` | `std::mem::transmute::<_, EnumType>(expr)` | Rust では整数→enum の as キャスト不可 |
| `(bool)expr` | `((expr) != 0)` | Rust では整数→bool の as キャスト不可 |
| `NULL` / `(void*)0` | `null_mut()` / `null()` | 型ヒントから const/mut を判定 |
| `offsetof(T, field)` | `std::mem::offset_of!(T, field)` | BuiltinCall AST ノード経由 |

### ポインタ操作

| C パターン | Rust 変換 |
|------------|-----------|
| `arr[i]` (配列型) | `(*arr.as_ptr().offset(i as isize))` |
| `(T)-1` (ジェネリック) | `((-1) as T)` + turbofish 構文 |

### 文字列変換（&str パラメータ）

| C パターン | Rust 変換 |
|------------|-----------|
| `func(str_param)` | `func(str_param.as_ptr() as *const c_char)` |
| `strlen(str_param)` | `str_param.len()` |

### lvalue マクロ展開

lvalue コンテキスト（代入の左辺）でのマクロ/関数呼び出しは、
定義を展開してフィールドアクセスに変換する:

```c
// C: SvFLAGS(sv) |= flag
// パラメータ置換を使って展開 → (*sv).sv_flags |= flag
```

`param_substitutions` テーブルにマクロの仮引数→実引数のマッピングを保持し、
展開時に適用する。

### ジェネリック型パラメータと turbofish 構文

ジェネリック型パラメータを持つマクロは turbofish 構文で呼び出す:

```rust
// 呼び出し側
isPOWER_OF_2::<U32>(n)

// 定義側
pub unsafe fn isPOWER_OF_2<T>(n: T) -> c_int { ... }
```

### StmtExpr 内の let バインディング

Statement expression (`({ ... })`) 内のローカル変数宣言は
Rust の `let` バインディングに変換する:

```c
// C: ({ int __x = expr; __x; })
// Rust: { let __x: c_int = expr; __x }
```

## CodegenDriver と RustCodegen の役割分担

| 観点 | CodegenDriver | RustCodegen |
|------|---------------|-------------|
| インスタンス寿命 | 生成全体で1つ | 関数ごとに使い捨て |
| IO | Writer を保持、直接出力 | バッファに蓄積、結果を返す |
| 状態管理 | 統計情報を累積、libc 使用集約 | 不完全マーカー数、未解決シンボル |
| 出力形式決定 | 完全/不完全に応じて形式を選択 | 常に同じ形式で生成 |
| エラー処理 | フォールバック出力を担当 | 不完全マーカーを挿入 |
| 依存管理 | カスケード検出、依存順出力 | なし |

### 連携パターン

```rust
// CodegenDriver から RustCodegen を呼び出す
fn generate_inline_fns(&mut self, result: &InferResult) -> io::Result<()> {
    for (name, func_def) in fns {
        // Pass 0: 事前に利用不可と判定された関数はスキップ
        if result.inline_fn_dict.is_calls_unavailable(*name) {
            gen_results.push((*name, InlineGenResult::CallsUnavailable));
            continue;
        }

        // 使い捨てインスタンスを作成
        let codegen = RustCodegen::new(
            self.interner, self.enum_dict, self.macro_ctx,
            self.bindings_info.clone(), &known_symbols,
        );
        let generated = codegen.generate_inline_fn(*name, func_def);

        if generated.is_complete() && generated.unresolved_names.is_empty() {
            // 完全な生成：そのまま出力
            self.stats.inline_fns_success += 1;
            self.successfully_generated_inlines.insert(*name);
            self.used_libc_fns.extend(generated.used_libc_fns);
        } else if !generated.unresolved_names.is_empty() {
            // 未解決シンボルあり
            self.stats.inline_fns_unresolved_names += 1;
        } else {
            // 不完全な生成：コメントアウトして出力
            self.stats.inline_fns_type_incomplete += 1;
        }
    }
}
```

## 出力形式

### 成功時（is_complete() == true）

```rust
#[inline]
pub unsafe fn SvPVX(sv: *mut SV) -> *mut c_char {
    ((*SvANY(sv)).xpv_pv)
}
```

### 不完全時（is_complete() == false）

```rust
// [CODEGEN_INCOMPLETE] SomeFunction - inline function
// #[inline]
// pub unsafe fn SomeFunction(arg: __TYPE__) -> __UNKNOWN__ {
//     ...
// }
```

### パース失敗時

```rust
// [PARSE_FAILED] SOME_MACRO(x, y)
// Error: unexpected token
// (tokens not available in parsed form)
```

### 利用不可関数呼び出し時

```rust
// [CALLS_UNAVAILABLE] SOME_MACRO(x) [THX] - calls unavailable function(s)
// Unavailable: some_internal_func
```

### カスケード依存時

```rust
// [CASCADE_UNAVAILABLE] SOME_MACRO(x) - depends on unavailable: OtherMacro
// #[inline]
// pub unsafe fn SOME_MACRO(x: *mut SV) -> c_int { ... }
```

### 未解決シンボル時

```rust
// [UNRESOLVED_NAMES] SOME_MACRO(x) - unresolved: unknown_func
// #[inline]
// pub unsafe fn SOME_MACRO(x: *mut SV) -> c_int { ... }
```

## ヘルパー関数

モジュールレベルで定義される共通ヘルパー:

| 関数 | 役割 |
|------|------|
| `escape_rust_keyword()` | Rust 予約語のエスケープ（r# 付与） |
| `bin_op_to_rust()` | 二項演算子を Rust 形式に変換 |
| `assign_op_to_rust()` | 代入演算子を Rust 形式に変換 |
| `escape_char()` / `escape_string()` | 文字/文字列のエスケープ |
| `is_zero_constant()` | ゼロ定数判定（do-while(0) 検出用） |
| `is_boolean_expr()` | bool 式判定（条件式の変換用） |
| `wrap_as_bool_condition()` | 必要に応じて `!= 0` を追加 |
| `is_null_literal()` | NULL リテラル判定（`null_mut()` / `null()` 変換用） |

## syn::Expr ベースの括弧正規化 (syn_codegen モジュール)

### 背景

文字列ベースの式生成では、`ExprContext` (Top/Default) で括弧を制御しているが、
限界がある。例えば `(*a).field` の括弧は構文上必須だが、
`(*a)` 単体がブロック末尾値として使われる場合は不要。
文字列レベルでは使用文脈を判定できないため、安全側で常に括弧を付ける結果、
`unnecessary parentheses around block return value` 警告が発生する。

### アーキテクチャ

`src/syn_codegen.rs` モジュールは `syn` crate の AST を活用して
正確な括弧制御を実現する。

```
expr_to_rust_ctx() → String (文字列ベースの式生成、既存)
         │
         ▼
normalize_parens(s: &str) → String
         │
         ├─ syn::parse_str()        文字列 → syn::Expr
         ├─ strip_all_parens()      全 Paren ノードを除去
         ├─ parenthesize()          優先順位に基づく括弧再挿入
         └─ pretty_expr()           prettyplease で整形
```

### 主要コンポーネント

#### parenthesize(expr) → syn::Expr

syn::Expr 木を走査し、親子の演算子優先順位に基づいて
`Expr::Paren` ノードを挿入する。

| 式タイプ | 優先順位 | 括弧が必要になる例 |
|----------|---------|-------------------|
| Lit, Path, Paren | 100 | なし（最高優先） |
| MethodCall, Field, Call | 90 | Cast(75) の子 → 不要 |
| Unary (!, -, *, &) | 80 | Field(90) の子 → 必要: `(*a).field` |
| Cast (as) | 75 | Binary の子 → 不要（as > 全 binop） |
| Binary (*) | 70 | Cast(75) の子 → 必要: `(a * b) as T` |
| If, Match | 1 | Cast の子 → 必要: `(if ... else ...) as T` |
| Block, Unsafe | 100 | なし（`{}` で自己完結） |

If/Match に優先順位 1 を割り当てるのは、`if ... else { } as T` が
Rust で `if ... else { (... as T) }` と誤解析されるため。

#### strip_all_parens(expr) → syn::Expr

syn::Expr 木のすべての `Expr::Paren` ラッパーノードを再帰的に除去する。
木構造（Binary, Unary, Cast 等）は保持される。

#### normalize_parens(s: &str) → String

式文字列の括弧を正規化する統合関数。

1. `syn::parse_str` でパース
2. `strip_all_parens` で全括弧除去
3. `parenthesize` で必要な括弧のみ再挿入
4. `prettyplease` で整形して文字列に戻す

結果が多行（if-else 等）やパース失敗の場合は、
`strip_outer_parens` 相当のフォールバックを使用。

#### 適用箇所

`normalize_parens` は以下の出力ポイントで適用される:

| 箇所 | 対象の警告 |
|------|-----------|
| `generate_macro`: Expression 結果 | block return value |
| `generate_macro`: 返り値キャスト後 | block return value |
| `stmt_to_rust`: return 文 | block return value |
| `stmt_to_rust_inline`: return 文 | block return value |
| `expr_to_rust_arg`: 関数引数 | function argument |
| `stmt_to_rust_inline`: if 条件 | if condition |

#### AST 変換ヘルパー (Phase 3)

将来の完全移行に向けた syn::Expr 構築ヘルパー:

| 関数 | 役割 |
|------|------|
| `wrap_as_bool(expr)` | int→`expr != 0`, ptr→`!expr.is_null()` |
| `insert_cast(expr, ty)` | `expr as T` ノード構築 |
| `null_for_type(ty_str)` | `null()` / `null_mut()` / `0` |
| `deref(expr)` | `*expr` |
| `field_access(expr, name)` | `expr.field` |
| `call(name, args)` | `func(args...)` |
| `if_else(cond, then, else)` | if-else 式 |

### 移行ステータス

C AST → syn::Expr 直接構築 → parenthesize → 整形 のフローへ完全移行済み。

- Phase 1-3: ✅ 基盤モジュール、括弧挿入パス、AST 変換ヘルパー
- Phase 4: ✅ normalize_parens の codegen 統合（53→46 警告）
- Phase 5: ✅ `build_syn_expr` を主経路として完成
  （`doc/plan/concurrent-leaping-petal.md` 参照）。
- フォロー作業: ✅ `expr_to_rust_ctx` / `expr_to_rust_inline_ctx` および
  関連の文字列ベースヘルパー (`expr_to_rust_arg`, `expr_with_type_hint*`,
  `cast_integer_arg_if_needed`, `infer_type_hint`, `wrap_as_bool_condition_macro`,
  `is_pointer_expr_inline`, `TypeHint`, `ExprContext` 等、約 1,800 行) を削除。
  - `expr_to_rust` / `expr_to_rust_inline` は薄いシムとして残り、
    内部で `build_syn_expr` を呼ぶのみ。
  - `try_expand_call_as_lvalue{,_inline}` は `build_syn_expr` を経由する
    統一実装に置き換え。
  - `decl_to_rust_let` の整数幅キャストも syn::Expr レベルで挿入。
  - `build_return_stmt` / `cast_return_syn_expr_if_needed` も syn ベース。

整数幅キャスト・bool 変換・ポインタオフセット・`as` cast 等は全て syn::Expr
上で挿入される。文字列レベルの "as type" 優先順位崩壊バグは発生しない。

残存する文字列パスはなし（`expr_to_rust*` のシムは移行期の互換のため残置）。

## bindings.rs に無い struct/union の自動生成

C ヘッダで宣言されているが bindings.rs に存在しない struct/union（典型例:
`sv_inline.h` の `body_details`）を **`macro_bindings.rs` 側で `#[repr(C)]`
付き Rust 定義として自動生成**する。

### 仕組み

- `FieldsDict.struct_defs: HashMap<InternedStr, StructDef>` に struct/union
  定義をメンバー順保持して登録（`collect_from_struct_spec` で蓄積、bitfield
  幅も保持）
- `src/struct_emitter.rs` の `emit_missing_structs()` が:
  1. `FieldsDict.struct_defs` 全件から `RustDeclDict.structs` に既存のものを除外
  2. Rust 予約語（`loop`, `type`, `struct` 等）と一致する名前を除外
  3. 各 struct/union を `format_struct()` で Rust ソース化
  4. **`syn::parse_str` で valid Rust か検証**し、失敗すれば `// [SKIPPED]`
     コメントに置換（関数ポインタ等の未対応型）

- `RustCodegen::generate_module_with_known_symbols` が、enum import 直後・関数
  定義より前に挿入

### Bitfield の扱い

連続する bitfield グループ（`PERL_BITFIELD8 type:5; ...:1; ...:1; ...:1;`）を
**1 つの packed `u8`/`u16`/`u32` フィールド**にまとめる:

```rust
#[repr(C)]
#[derive(Copy, Clone)]
pub struct body_details {
    pub body_size: U8,
    pub copy: U8,
    pub offset: U8,
    /// packed bitfields (8 bit total): type, cant_upgrade, zero_nv, arena
    pub _bitfield_0: u8,
    pub arena_size: U32,
}
```

bindgen 風 getter/setter は付けない（Phase 1 簡易版）。値の構築は呼び出し側で
ビット演算する想定。

### Flexible array member

最終メンバー `T name[1]` / `[0]` / `[]` は `[T; 0]` に変換して出力する
（Rust の flex array 表現）。アクセス時の pointer-decay は別ロジック（`type-inference-and-cast.md`
「Flexible Array Member」節参照）。

### 既知の限界

- 関数ポインタフィールドは `to_rust_string` が `/* fn */` を返すため
  `[SKIPPED]` 扱い

### inline 関数収集と未解決名検出

`InlineFnDict::collect_from_function_def` は `inline` だけでなく **`static`
（内部リンケージ）の関数**も対象にする。`STATIC`-only 関数（例:
`perlstatic.h::Perl_croak_memory_wrap`）も翻訳単位ローカルなので、Rust
側に独自に持つ意味論的問題はない。これにより、これを呼ぶ inline 関数
（`Perl_newSV_type` 等）のカスケード解消に寄与する。

`RustCodegen.current_local_names` は inline 関数本体内のローカル変数を
追跡し、それらを未解決名検出から除外する。Phase 1 では関数本体の
**トップレベルのみ**を走査していたが、`STMT_START { ... } STMT_END` 等の
展開で **ネストした compound 内の `let`** が頻出するため、
`collect_local_names_recursive` で全 block / StmtExpr / for-init を
再帰走査するように拡張した。

例:
```c
case SVt_IV:
    SET_SVANY_FOR_BODYLESS_IV(sv);  // expands to
                                    //   STMT_START { SV* sv_ = sv; ... } STMT_END
    SvIV_set(sv, 0);                // expands to similar pattern
    break;
```

各 `STMT_START` の `sv_` ローカルが `current_local_names` に登録されるため、
未解決名として誤検出されない。

### `static const X[] = {...}` の Rust `static` 配列への翻訳

`sv_inline.h` の `bodies_by_type[]` のような **翻訳単位ローカルな
`static const` 配列**を Rust 側に再現する仕組み。`Perl_newSV_type` 等の
inline 関数がこの配列を参照可能にするのが目的（C では各 TU が独自コピーを
持つので、Rust 側に独自定義しても意味論的に問題なし）。

**捕捉**: `src/global_const_dict.rs` の `GlobalConstDict::try_collect` が
`parse_each_with_pp` callback で `storage=Static` + `qualifier=const` +
配列 derived + initializer 持ち の宣言を保持。`InferResult.global_const_dict`
に格納される。

**出力**: `src/static_array_emitter.rs::emit_static_arrays` が各エントリを
Rust struct literal 配列に翻訳:

- 各 initializer エントリ `{ a, b, c, ... }` を struct literal に
  - 位置順で `StructDef.members` 名と対応付け
  - bitfield 連続グループは値を pack して 1 つの `_bitfield_N` に
- 各値の式は `translate_const_expr` で翻訳:
  - `IntLit(n)` → `n`
  - `Ident(SVt_NULL)` → `SVt_NULL`（bindings.rs 由来）
  - `SizeofType(T)` → `core::mem::size_of::<T>()`
  - `Sizeof((T*)X)->f1.f2)` → `{ let _z = unsafe { core::mem::zeroed::<T>() };
    core::mem::size_of_val(unsafe { &_z.f1.f2 }) }` （`copy_length` マクロ
    展開での典型形）
  - `BuiltinCall(__builtin_offsetof, T, m)` → `core::mem::offset_of!(T, m)`
  - `Cast { type_name, expr }` → `((expr) as type_name)`
  - 算術・条件演算は通常の Rust 式
  - `Conditional { c, t, e }` → `if (c) != 0 { t } else { e }`（ただし
    `c` が比較式なら `if (c) {...}`）

**前提**: 全ての特殊マクロ（`STRUCT_OFFSET`, `FIT_ARENA`, `copy_length`,
`HASARENA`, `NONV` 等）は **preprocessor で完全展開済み**で AST に到達する。
codegen 側で特殊マクロを認識する必要なし。

**known_symbols 連携**: 出力に成功した struct/typedef/static 名のみ
`KnownSymbols` に追加。これにより未生成の型を参照する code を
"unresolved" として正しくフラグできる（`generate()` が emit を先行実施し、
得た名前リストで `KnownSymbols` を補完）。

```rust
// 生成例: bodies_by_type[3] (SVt_PV)
body_details {
    body_size: ((core::mem::size_of::<XPV>() - core::mem::offset_of!(XPV, xpv_cur))) as U8,
    copy: (((core::mem::offset_of!(XPV, xpv_len_u.xpvlenu_len) +
            { let _z = unsafe { core::mem::zeroed::<XPV>() };
              core::mem::size_of_val(unsafe { &_z.xpv_len_u.xpvlenu_len }) })
           - core::mem::offset_of!(XPV, xpv_cur))) as U8,
    offset: (core::mem::offset_of!(XPV, xpv_cur)) as U8,
    _bitfield_0: ((SVt_PV) as u8 & 0x1f) << 0 | ((0) as u8 & 0x1) << 5
                | ((1) as u8 & 0x1) << 6 | ((1) as u8 & 0x1) << 7,
    arena_size: /* FIT_ARENA(0, ...) 展開 */ as U32,
}
```

## 関連ファイル

| ファイル | 役割 |
|----------|------|
| `src/rust_codegen.rs` | コード生成モジュール本体 |
| `src/struct_emitter.rs` | bindings.rs に無い struct/union の Rust 定義生成 |
| `src/static_array_emitter.rs` | `static const X[]={...}` の Rust `static` への翻訳 |
| `src/global_const_dict.rs` | parse 時に static const 配列宣言を捕捉 |
| `src/syn_codegen.rs` | syn::Expr ベースの括弧正規化・AST 変換ヘルパー |
| `src/infer_api.rs` | InferResult の定義 |
| `src/macro_infer.rs` | MacroInferInfo の定義 |
| `src/type_repr.rs` | TypeRepr の定義 |
| `src/enum_dict.rs` | EnumDict の定義 |
| `src/rust_decl.rs` | RustDeclDict の定義（BindingsInfo のソース） |
