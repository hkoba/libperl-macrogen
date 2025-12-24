# TinyCC ソースコード分析と Rust 移植計画

## 1. 全体アーキテクチャ

### 主要ファイル構成

| ファイル | サイズ | 役割 |
|----------|--------|------|
| `tcc.h` | 69KB | メインヘッダー（全データ構造定義） |
| `tccpp.c` | 115KB | プリプロセッサ（マクロ、#include、条件コンパイル） |
| `tccgen.c` | 266KB | パーサー/意味解析/コード生成基盤 |
| `libtcc.c` | 67KB | ライブラリAPI、ファイル管理 |
| `tcctok.h` | 14KB | トークン定義 |

---

## 2. 主要データ構造一覧

### 2.1 トークン管理

| 構造体 | 役割 | Rust代替案 |
|--------|------|------------|
| `TokenSym` | トークンシンボル（識別子のインターン） | `HashMap<String, TokenId>` + `Vec<String>` (string interner) |
| `CString` | 動的文字列バッファ | `String` |
| `TokenString` | トークンストリーム（マクロ本体等） | `Vec<Token>` |

### 2.2 型システム

| 構造体 | 役割 | Rust代替案 |
|--------|------|------------|
| `CType` | 型情報（型フラグ + 参照） | `enum Type { Int, Ptr(Box<Type>), ... }` |
| `CValue` | 定数値（共用体） | `enum Value { Int(i64), Float(f64), ... }` |
| `SValue` | スタック値（型+値+レジスタ情報） | `struct Value { ty: Type, loc: Location, val: Option<Const> }` |

### 2.3 シンボルテーブル

| 構造体 | 役割 | Rust代替案 |
|--------|------|------------|
| `Sym` | シンボル（変数/関数/マクロ/struct等） | `struct Symbol` with スコープID |
| `SymAttr` | シンボル属性（aligned, packed等） | `struct SymbolAttr` |
| `FuncAttr` | 関数属性（calling convention等） | `struct FuncAttr` |

### 2.4 ファイル入力

| 構造体 | 役割 | Rust代替案 |
|--------|------|------------|
| `BufferedFile` | バッファ付きファイル入力 | `BufReader<File>` + メタデータ |
| `CachedInclude` | インクルードファイルキャッシュ | `HashSet<PathBuf>` |

### 2.5 コンパイラ状態

| 構造体 | 役割 | Rust代替案 |
|--------|------|------------|
| `TCCState` | コンパイラ全体状態 | `struct CompilerState` (複数の小構造体に分割推奨) |

---

## 3. 関数の役割分担リスト

### 3.1 プリプロセッサ（tccpp.c）

#### エントリーポイント
| 関数 | 役割 | 呼び出し元 |
|------|------|------------|
| `next()` | 次トークン取得（マクロ展開付き） | パーサー全体 |
| `preprocess()` | プリプロセッサディレクティブ処理 | `next_nomacro()` |
| `preprocess_start()` | プリプロセッサ初期化 | コンパイル開始時 |
| `preprocess_end()` | プリプロセッサ終了処理 | コンパイル終了時 |

#### トークン入力
| 関数 | 役割 | 呼び出し元 |
|------|------|------------|
| `next_nomacro()` | 生トークン読み取り | `next()` |
| `next_c()` | 次文字読み取り | `next_nomacro()` |
| `handle_eob()` | バッファ終端処理 | `next_c()` |
| `handle_stray()` | バックスラッシュ継続 | `next_nomacro()` |

#### マクロ処理
| 関数 | 役割 | 呼び出し元 |
|------|------|------------|
| `parse_define()` | #define解析 | `preprocess()` |
| `define_push()` | マクロ登録 | `parse_define()` |
| `define_find()` | マクロ検索 | `next()`, `macro_subst_tok()` |
| `macro_subst_tok()` | マクロ展開（1トークン） | `next()` |
| `macro_subst()` | マクロ本体展開 | `macro_subst_tok()` |
| `macro_arg_subst()` | マクロ引数置換 | `macro_subst_tok()` |
| `macro_twosharps()` | `##` 演算子処理 | `macro_arg_subst()` |

#### インクルード処理
| 関数 | 役割 | 呼び出し元 |
|------|------|------------|
| `parse_include()` | #include解析 | `preprocess()` |
| `search_cached_include()` | インクルードキャッシュ検索 | `parse_include()` |

#### 条件コンパイル
| 関数 | 役割 | 呼び出し元 |
|------|------|------------|
| `expr_preprocess()` | #if式評価 | `preprocess()` |
| `preprocess_skip()` | 偽ブロックスキップ | `preprocess()` |

### 3.2 パーサー/意味解析（tccgen.c）

#### エントリーポイント
| 関数 | 役割 | 呼び出し元 |
|------|------|------------|
| `tccgen_compile()` | コンパイルメイン | libtcc API |
| `tccgen_init()` | コンパイラ初期化 | `tccgen_compile()` |
| `tccgen_finish()` | コンパイラ終了 | `tccgen_compile()` |

#### 宣言解析
| 関数 | 役割 | 呼び出し元 |
|------|------|------------|
| `decl()` | 宣言解析 | `tccgen_compile()`, `block()` |
| `parse_btype()` | 基本型解析 | `decl()`, `unary()` |
| `type_decl()` | 宣言子解析 | `decl()` |
| `post_type()` | 後置型（配列/関数） | `type_decl()` |
| `struct_decl()` | struct/union/enum解析 | `parse_btype()` |
| `struct_layout()` | struct レイアウト計算 | `struct_decl()` |

#### 式解析（優先順位順）
| 関数 | 優先順位 | 演算子 |
|------|----------|--------|
| `gexpr()` | 最低 | `,` |
| `expr_eq()` | | `=`, `+=`, `-=`, ... |
| `expr_cond()` | | `?:` |
| `expr_lor()` | | `\|\|` |
| `expr_land()` | | `&&` |
| `expr_or()` | | `\|` |
| `expr_xor()` | | `^` |
| `expr_and()` | | `&` |
| `expr_cmpeq()` | | `==`, `!=` |
| `expr_cmp()` | | `<`, `>`, `<=`, `>=` |
| `expr_shift()` | | `<<`, `>>` |
| `expr_sum()` | | `+`, `-` |
| `expr_prod()` | | `*`, `/`, `%` |
| `unary()` | 最高 | 単項演算子、一次式 |

#### シンボルテーブル管理
| 関数 | 役割 | 呼び出し元 |
|------|------|------------|
| `sym_push()` | シンボル追加 | `decl()`, `block()` |
| `sym_find()` | シンボル検索 | `unary()` |
| `sym_pop()` | シンボル除去 | スコープ終了時 |
| `sym_link()` | シンボル可視性設定 | `sym_push()`, `sym_pop()` |

#### 型システム
| 関数 | 役割 | 呼び出し元 |
|------|------|------------|
| `type_size()` | 型サイズ計算 | 多数 |
| `compare_types()` | 型互換性比較 | 意味解析全体 |
| `combine_types()` | 型昇格/結合 | `gen_op()` |
| `gen_cast()` | キャスト生成 | 式評価 |
| `gen_assign_cast()` | 代入キャスト | `vstore()` |

#### コード生成基盤
| 関数 | 役割 | 呼び出し元 |
|------|------|------------|
| `gen_op()` | 二項演算生成 | 式解析 |
| `gv()` | 値をレジスタに | コード生成全体 |
| `vstore()` | 値格納 | 代入処理 |
| `gen_function()` | 関数コード生成 | `decl()` |

#### 文解析
| 関数 | 役割 | 呼び出し元 |
|------|------|------------|
| `block()` | 文/ブロック解析 | `gen_function()`, 再帰 |
| `lblock()` | ループ本体解析 | `block()` |

---

## 4. 呼び出しフロー図

```
tccgen_compile()
    │
    ├─→ preprocess_start()
    │       └─→ tccpp_new() / 初期化
    │
    ├─→ decl(VT_CONST)  [グローバル宣言ループ]
    │       │
    │       ├─→ parse_btype()
    │       │       └─→ struct_decl() [struct/union/enum]
    │       │
    │       ├─→ type_decl()
    │       │       └─→ post_type() [配列/関数型]
    │       │
    │       └─→ [関数定義の場合]
    │               └─→ gen_function()
    │                       │
    │                       ├─→ sym_push() [パラメータ]
    │                       │
    │                       └─→ block() [関数本体]
    │                               │
    │                               ├─→ gexpr() [式評価]
    │                               │       │
    │                               │       └─→ ... → unary()
    │                               │               │
    │                               │               └─→ next() ←──┐
    │                               │                       │     │
    │                               │                       ↓     │
    │                               │               macro_subst_tok()
    │                               │                       │
    │                               │                       └─→ next_nomacro()
    │                               │                               │
    │                               │                               └─→ preprocess()
    │                               │                                       │
    │                               │                                       └─→ parse_define()
    │                               │                                           parse_include()
    │                               │
    │                               └─→ decl(VT_LOCAL) [ローカル宣言]
    │
    ├─→ gen_inline_functions()
    │
    └─→ tccgen_finish()
```
