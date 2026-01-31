# THX 依存性検出アーキテクチャ

## 概要

本ドキュメントは、C マクロおよび inline 関数の THX（Thread Context）依存性を
検出・伝播・適用する仕組みを説明する。

**関連ドキュメント**:
- [マクロ展開制御アーキテクチャ](./architecture-macro-expansion-control.md)
- [C Inline 関数の処理アーキテクチャ](./architecture-inline-function-processing.md)

---

## THX とは

Perl のマルチスレッド環境では、各スレッドが独立した Perl インタプリタを持つ。
THX（Thread Context）は、このスレッドローカルなインタプリタへのアクセスを提供する仕組み。

### THX 関連シンボル

| シンボル | 役割 | 定義例 |
|----------|------|--------|
| `aTHX` | 関数呼び出し時の暗黙引数 | `#define aTHX my_perl` |
| `aTHX_` | カンマ付きの暗黙引数 | `#define aTHX_ aTHX,` |
| `tTHX` | 型宣言用の THX 型 | `typedef struct interpreter *tTHX` |
| `my_perl` | THX の実際の変数名 | `PerlInterpreter *my_perl` |
| `dTHX` | THX 変数の宣言 | `#define dTHX PerlInterpreter *my_perl = ...` |
| `pTHX` | 関数パラメータ宣言 | `#define pTHX tTHX my_perl` |
| `pTHX_` | カンマ付きパラメータ | `#define pTHX_ pTHX,` |

### THX 依存マクロの例

```c
// THX 依存マクロ（aTHX を使用）
#define SvREFCNT_inc(sv)  Perl_SvREFCNT_inc(aTHX_ (SV*)(sv))
//                                         ^^^^^ THX を渡す

// THX を受け取る関数
SV* Perl_SvREFCNT_inc(pTHX_ SV *sv);
//                    ^^^^^ THX パラメータ
```

---

## THX 検出・伝播パイプライン

```
┌─────────────────────────────────────────────────────────────────────────┐
│                           入力                                           │
│  wrapper.h (マクロ定義、inline 関数)                                     │
└─────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────┐
│ Stage 1: THX シンボルの intern                                           │
│                                                                         │
│  場所: src/infer_api.rs:316-319                                         │
│                                                                         │
│  let sym_athx = pp.interner_mut().intern("aTHX");                       │
│  let sym_tthx = pp.interner_mut().intern("tTHX");                       │
│  let sym_my_perl = pp.interner_mut().intern("my_perl");                 │
│  let thx_symbols = (sym_athx, sym_tthx, sym_my_perl);                   │
│                                                                         │
│  → 3つの THX シンボルを StringInterner に登録                            │
└─────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────┐
│ Stage 2: 初期 THX 検出                                                   │
│                                                                         │
│  場所: src/macro_infer.rs:572-583                                       │
│                                                                         │
│  build_macro_info() 内で各マクロについて:                                 │
│                                                                         │
│  1. uses (呼び出したマクロ) に aTHX または tTHX が含まれるか？             │
│     let has_thx_from_uses = info.uses.contains(&sym_athx)              │
│                          || info.uses.contains(&sym_tthx);             │
│                                                                         │
│  2. 展開後トークンに my_perl 識別子が含まれるか？                          │
│     let has_my_perl = expanded_tokens.iter().any(|t| {                 │
│         matches!(t.kind, TokenKind::Ident(id) if id == sym_my_perl)    │
│     });                                                                 │
│                                                                         │
│  3. いずれかが true なら is_thx_dependent = true                         │
│     info.is_thx_dependent = has_thx_from_uses || has_my_perl;          │
│                                                                         │
│  → 直接 THX を参照するマクロに is_thx_dependent フラグを設定               │
└─────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────┐
│ Stage 3: THX 依存性の推移的伝播                                          │
│                                                                         │
│  場所: src/macro_infer.rs:1061-1099                                     │
│                                                                         │
│  propagate_flag_via_used_by() で used_by グラフを逆方向に辿る:           │
│                                                                         │
│  ┌───────────────────────────────────────────────────────────────┐      │
│  │  THX 直接参照マクロ                                           │      │
│  │  (aTHX, tTHX, my_perl を使用)                                 │      │
│  └───────────────────────┬───────────────────────────────────────┘      │
│                          │ used_by (逆方向伝播)                          │
│                          ▼                                              │
│  ┌───────────────────────────────────────────────────────────────┐      │
│  │  間接参照マクロ A                                              │      │
│  │  (THX マクロを呼び出す)                                        │      │
│  └───────────────────────┬───────────────────────────────────────┘      │
│                          │ used_by                                      │
│                          ▼                                              │
│  ┌───────────────────────────────────────────────────────────────┐      │
│  │  間接参照マクロ B                                              │      │
│  │  (A を呼び出す → THX 依存が伝播)                               │      │
│  └───────────────────────────────────────────────────────────────┘      │
│                                                                         │
│  アルゴリズム:                                                           │
│  - 初期集合: Stage 2 で検出された THX 直接参照マクロ                      │
│  - BFS で used_by を辿り、全ての使用元マクロに is_thx_dependent を設定    │
└─────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────┐
│ Stage 4: コード生成での THX 適用                                         │
│                                                                         │
│  場所: src/rust_codegen.rs                                              │
│                                                                         │
│  4a. 関数シグネチャに my_perl パラメータを追加 (lines 364-382):           │
│                                                                         │
│      // THX 依存の場合は my_perl パラメータを追加                         │
│      let thx_param = if info.is_thx_dependent {                        │
│          "my_perl: *mut PerlInterpreter"                               │
│      } else {                                                          │
│          ""                                                            │
│      };                                                                 │
│                                                                         │
│  4b. 関数呼び出し時の my_perl 注入 (lines 611-623):                      │
│                                                                         │
│      // THX マクロで my_perl が不足しているかチェック                     │
│      let needs_my_perl = self.needs_my_perl_for_call(*name, args.len());│
│                                                                         │
│      let mut a: Vec<String> = if needs_my_perl {                       │
│          vec!["my_perl".to_string()]  // 第一引数として注入              │
│      } else {                                                          │
│          vec![]                                                        │
│      };                                                                 │
│                                                                         │
│  4c. ドキュメントコメントに [THX] 表示 (line 381):                       │
│                                                                         │
│      /// SvREFCNT_inc [THX] - macro function                           │
│      pub unsafe fn SvREFCNT_inc(my_perl: *mut PerlInterpreter, ...) {} │
└─────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────┐
│                           出力                                           │
│                                                                         │
│  // THX 依存マクロの生成例                                               │
│  /// SvREFCNT_inc [THX] - macro function                               │
│  #[inline]                                                              │
│  pub unsafe fn SvREFCNT_inc(                                           │
│      my_perl: *mut PerlInterpreter,  // ← THX パラメータ追加            │
│      sv: *mut SV,                                                       │
│  ) -> *mut SV {                                                         │
│      Perl_SvREFCNT_inc(my_perl, sv as *mut SV)                         │
│  }                                    ^^^^^^^ my_perl を伝播            │
└─────────────────────────────────────────────────────────────────────────┘
```

---

## THX 検出の詳細

### MacroInferInfo の THX 関連フィールド

```rust
// src/macro_infer.rs
pub struct MacroInferInfo {
    // ...

    /// THX 依存フラグ
    /// - true: このマクロは my_perl パラメータを必要とする
    /// - false: THX 非依存
    pub is_thx_dependent: bool,

    /// 使用するマクロ/識別子の集合
    /// aTHX, tTHX がこの中に含まれれば THX 依存
    pub uses: HashSet<InternedStr>,

    /// このマクロを使用するマクロの集合（逆参照）
    /// THX 依存性の伝播に使用
    pub used_by: HashSet<InternedStr>,
}
```

### 検出パターン

| パターン | 例 | 検出方法 |
|----------|------|----------|
| aTHX 直接使用 | `#define FOO() Perl_foo(aTHX)` | `uses` に `aTHX` が含まれる |
| tTHX 直接使用 | `#define BAR(x) ((tTHX)(x))` | `uses` に `tTHX` が含まれる |
| my_perl 直接使用 | `#define BAZ() my_perl->Istack` | 展開後トークンに `my_perl` が含まれる |
| 間接参照 | `#define QUX() FOO() + 1` | `FOO` が THX 依存 → `used_by` 経由で伝播 |

### needs_my_perl_for_call() の判定ロジック

```rust
// src/rust_codegen.rs:300-312
fn needs_my_perl_for_call(&self, func_name: InternedStr, actual_arg_count: usize) -> bool {
    if let Some(callee_info) = self.macro_ctx.macros.get(&func_name) {
        if callee_info.is_thx_dependent {
            // THX マクロの期待引数数 = params.len() + 1 (my_perl)
            let expected_count = callee_info.params.len() + 1;
            // 実引数が1つ少ない場合、my_perl が必要
            return actual_arg_count + 1 == expected_count;
        }
    }
    false
}
```

**判定フロー**:

```
呼び出し: SvTYPE(sv)  (引数1つ)
         │
         ▼
┌─────────────────────────────────────┐
│ SvTYPE は MacroInferContext にある？ │
└───────────────┬─────────────────────┘
               Yes
                │
                ▼
┌─────────────────────────────────────┐
│ SvTYPE.is_thx_dependent == true？   │
└───────────────┬─────────────────────┘
               Yes
                │
                ▼
┌─────────────────────────────────────┐
│ 期待引数数: params.len() + 1 = 2    │
│ (元パラメータ1 + my_perl)           │
│                                     │
│ 実引数数: 1                         │
│                                     │
│ 1 + 1 == 2? → true                  │
│ → my_perl を注入する                │
└─────────────────────────────────────┘
```

---

## def-use グラフと THX 伝播

### グラフ構造

```
                  uses (順方向)
    ┌─────────────────────────────────────┐
    │                                     │
    ▼                                     │
┌────────┐     uses     ┌────────┐     uses     ┌────────┐
│ aTHX   │◄────────────│ FOO    │◄────────────│ BAR    │
└────────┘              └────────┘              └────────┘
    │                       │                       │
    │      used_by          │      used_by          │
    └──────────────────────►└──────────────────────►│
         (逆方向)                 (逆方向)
```

### 伝播アルゴリズム

```rust
// src/macro_infer.rs:1061-1099
fn propagate_flag_via_used_by(&mut self, initial_set: &HashSet<InternedStr>, is_thx: bool) {
    // 1. 初期集合のフラグを設定
    for name in initial_set {
        if let Some(info) = self.macros.get_mut(name) {
            info.is_thx_dependent = true;
        }
    }

    // 2. BFS で used_by を辿って伝播
    let mut to_propagate: Vec<InternedStr> = initial_set.iter().copied().collect();

    while let Some(name) = to_propagate.pop() {
        // このマクロを使用している全マクロを取得
        let used_by_list = self.macros.get(&name)
            .map(|info| info.used_by.iter().copied().collect())
            .unwrap_or_default();

        for user in used_by_list {
            if let Some(user_info) = self.macros.get_mut(&user) {
                // まだフラグが立っていなければ設定して伝播キューに追加
                if !user_info.is_thx_dependent {
                    user_info.is_thx_dependent = true;
                    to_propagate.push(user);
                }
            }
        }
    }
}
```

**計算量**: O(V + E) - マクロ数とその依存関係数に線形

---

## コード生成での適用

### 関数シグネチャ生成

**THX 非依存マクロ**:
```rust
/// SvTYPE - macro function
#[inline]
pub unsafe fn SvTYPE(sv: *const SV) -> svtype {
    // ...
}
```

**THX 依存マクロ**:
```rust
/// SvREFCNT_inc [THX] - macro function
#[inline]
pub unsafe fn SvREFCNT_inc(
    my_perl: *mut PerlInterpreter,  // ← THX パラメータ
    sv: *mut SV,
) -> *mut SV {
    // ...
}
```

### 関数呼び出し変換

**C コード**:
```c
#define SvREFCNT_inc(sv)  Perl_SvREFCNT_inc(aTHX_ (SV*)(sv))
```

**生成される Rust コード**:
```rust
pub unsafe fn SvREFCNT_inc(my_perl: *mut PerlInterpreter, sv: *mut SV) -> *mut SV {
    Perl_SvREFCNT_inc(my_perl, sv as *mut SV)
}                    ^^^^^^^ aTHX_ が my_perl に変換
```

---

## 制御点まとめ

| 制御点 | 場所 | 役割 |
|--------|------|------|
| **THX シンボル intern** | `infer_api.rs:316-319` | aTHX, tTHX, my_perl を登録 |
| **初期検出** | `macro_infer.rs:572-583` | 直接参照マクロを検出 |
| **伝播** | `macro_infer.rs:1061-1099` | used_by 経由で間接参照を検出 |
| **シグネチャ生成** | `rust_codegen.rs:364-382` | my_perl パラメータを追加 |
| **呼び出し変換** | `rust_codegen.rs:611-623` | my_perl を注入 |
| **ドキュメント** | `rust_codegen.rs:381` | [THX] マーカーを出力 |

---

## 統計情報

`InferStats` で THX 依存マクロ数を追跡:

```rust
// src/infer_api.rs:346-348
let thx_dependent_count = infer_ctx.macros.values()
    .filter(|info| info.is_target && info.is_thx_dependent)
    .count();
```

---

## ファイル別責務

| ファイル | THX 関連の責務 |
|----------|----------------|
| `infer_api.rs` | THX シンボルの intern、統計カウント |
| `macro_infer.rs` | `is_thx_dependent` フィールド、初期検出、伝播ロジック |
| `rust_codegen.rs` | `needs_my_perl_for_call()`、パラメータ追加、呼び出し変換 |

---

## 拡張ポイント

### 新しい THX シンボルを追加する場合

```rust
// src/infer_api.rs
let sym_athx = pp.interner_mut().intern("aTHX");
let sym_tthx = pp.interner_mut().intern("tTHX");
let sym_my_perl = pp.interner_mut().intern("my_perl");
let sym_new_thx = pp.interner_mut().intern("NEW_THX");  // 追加
let thx_symbols = (sym_athx, sym_tthx, sym_my_perl, sym_new_thx);
```

```rust
// src/macro_infer.rs の build_macro_info() 内
let (sym_athx, sym_tthx, sym_my_perl, sym_new_thx) = thx_symbols;
let has_thx_from_uses = info.uses.contains(&sym_athx)
                     || info.uses.contains(&sym_tthx)
                     || info.uses.contains(&sym_new_thx);  // 追加
```

### THX パラメータ名をカスタマイズする場合

```rust
// src/rust_codegen.rs
let thx_param = if info.is_thx_dependent {
    "interp: *mut PerlInterpreter"  // my_perl から変更
} else {
    ""
};
```
