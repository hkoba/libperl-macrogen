# E0425 未解決シンボル — 残存エラー分析と改善策

## 現状 (commit `0741033`)

| 指標 | 値 |
|------|-----|
| 総ビルドエラー | 1,328 |
| E0425 エラー | 147 |
| UNRESOLVED_NAMES 検出 | 97 関数 (67 macro + 30 inline) |
| CALLS_UNAVAILABLE | 734 関数 |
| CODEGEN_INCOMPLETE | 376 関数 |
| 正常生成 | 2,512 関数 |

---

## 残存 E0425 エラーの分類

### A. ビルトインリスト問題 (19 エラー)

**問題**: `strcmp`, `strlen` 等の C 標準ライブラリ関数が `KnownSymbols` の
ビルトインリストに含まれているため UNRESOLVED_NAMES 検出を免れるが、
実際の Rust コードでは定義がなく E0425 になる。

同じ関数群は `macro_infer.rs` の `check_function_availability` のビルトインリストにもあり、
`calls_unavailable` チェックも通過する。つまり二重にすり抜けている。

| シンボル | E0425 数 | 呼び出し元マクロ例 |
|----------|---------|-------------------|
| `strcmp` | 6 | `strEQ`, `strGE`, `strGT`, `strLE`, `strLT`, `strNE` |
| `strlen` | 3 | 文字列検証マクロ |
| `strncmp` | 2 | `strnEQ` 等 |
| `memset` | 1 | 初期化マクロ |
| `memchr` | 1 | 文字列検索マクロ |
| `__errno_location` | 1 | `errno` 関連 |

**改善策A-1: ビルトインリストの分割**

ビルトインリストを2種類に分ける:

1. **codegen で変換済み** — `__builtin_expect` → 削除済み、
   `__builtin_unreachable` → `std::hint::unreachable_unchecked()` 等。
   これらは codegen が別の Rust コードに変換するため、シンボルとして残らない。
   → `KnownSymbols` に含めて良い。

2. **そのまま関数呼び出しとして出力される** — `strcmp`, `strlen`, `memset` 等。
   Rust 側に定義がなければ E0425 になる。
   → `KnownSymbols` から除外し、UNRESOLVED_NAMES として検出すべき。

具体的には:
- `KnownSymbols` のビルトインリストから `strcmp`, `strlen`, `strncmp`,
  `memset`, `memchr`, `memcpy`, `memmove`, `strcpy`, `strncpy`,
  `__errno_location`, `pthread_*`, `getenv` を削除。
- `macro_infer.rs` の `check_function_availability` からも同様に削除。

**期待削減**: ~19 E0425（これらを呼ぶマクロが UNRESOLVED_NAMES 化される）。
さらにカスケード効果で他のエラーも減る可能性あり。

**リスク**: `strcmp` 等を呼ぶマクロが多数コメントアウトされ、正常生成数が減少する。
ただしこれらはどのみちコンパイルできないのでコメントアウトが正しい。

---

### B. カスケード依存問題 (29 エラー)

**問題**: UNRESOLVED_NAMES / CODEGEN_INCOMPLETE でコメントアウトされたマクロが、
他のマクロから `should_emit_as_macro_call` で呼び出し保持される。
呼び出し先が実体化されていないので呼び出し元でも E0425 になる。

| コメントアウト元 | 呼び出し元 (生成済み) | E0425 数 |
|-----------------|---------------------|---------|
| `generic_isCC_` [CODEGEN_INCOMPLETE] | `isALPHA`, `isXDIGIT` 等 | 3 |
| `generic_isCC_A_` [CODEGEN_INCOMPLETE] | `isIDFIRST_A`, `isDIGIT_A` 等 | 4 |
| `inRANGE_helper_` [TYPE_INCOMPLETE] | UTF-8 検証マクロ | 6 |
| `inRANGE` [TYPE_INCOMPLETE] | `_generic_isCC` 系 | 1 |
| `toUPPER` (未生成) | `toUPPER_A` | 2 |
| `PUSHs` [UNRESOLVED_NAMES] | `mPUSHs`, `mXPUSHs` | 5 |
| `is_SURROGATE_utf8_safe` (未生成) | UTF-8 マクロ | 1 |
| `isASCII_utf8_safe` (未生成) | UTF-8 マクロ | 1 |
| `isXDIGIT_A` 等 (generic_isCC_ 依存) | 文字分類マクロ | 6 |

**改善策B-1: UNRESOLVED_NAMES の伝播**

コード生成を2パスにする:

1. **パス1**: 全マクロを生成し、UNRESOLVED_NAMES を持つ関数名を収集。
2. **パス2**: パス1の結果を `KnownSymbols` から除外して再生成。
   呼び出し元がパス1でコメントアウトされた関数を呼んでいれば、
   パス2で UNRESOLVED_NAMES として検出される。

ただし2パス方式はコストが大きい。

**改善策B-2: should_emit_as_macro_call の強化 (軽量版)**

`should_emit_as_macro_call` が true を返す条件に、
呼び出し先マクロが実際に正常生成されるかどうかのチェックを追加する。

```
should_emit_as_macro_call(name) が true のとき:
  → そのマクロの GenerateStatus が Success かつ
    is_fully_confirmed() が true かつ
    calls_unavailable が false
  → でなければ false を返す（= インライン展開に切り替え）
```

インライン展開に切り替えても、展開先で UNRESOLVED_NAMES が検出されれば
呼び出し元がコメントアウトされるので、最終的にはカスケードが止まる。

**注**: これは `should_emit_as_macro_call` の既存ロジック
（`info.is_parseable() && !info.calls_unavailable`）にさらに
`is_fully_confirmed()` チェックを加えるだけなので、影響範囲は小さい。

**改善策B-3: コメントアウトされた関数名の KnownSymbols 除外 (中量版)**

`CodegenDriver::generate()` 内で、マクロ生成前に
「生成対象外」の関数名リストを事前計算して `KnownSymbols` から除外する:

```
生成対象外 = CALLS_UNAVAILABLE ∪ TYPE_INCOMPLETE ∪ PARSE_FAILED ∪ CONTAINS_GOTO
```

これらの関数名は Rust コード中に定義が存在しないため、
呼び出し元が UNRESOLVED_NAMES として検出される。

**期待削減**: ~29 E0425 + カスケード効果。

---

### C. ローカル変数参照 (10 エラー)

**問題**: マクロ展開後に、マクロパラメータでもローカル宣言でもない変数が
参照される。これらは UNRESOLVED_NAMES で既に検出されるべきだが、
一部が漏れている。

| シンボル | E0425 数 | 原因 |
|----------|---------|------|
| `uv` | 3 | マクロ展開内のローカル変数 |
| `s` | 2 | マクロ展開内のポインタ変数 |
| `c` | 2 | マクロ展開内の文字変数 |
| `t` | 1 | 一時変数 |
| `n` | 1 | カウンタ変数 |
| `e` | 1 | 終端ポインタ |

**原因分析**: これらは `ExprKind::Ident` チェックで検出されるはずだが、
`KnownSymbols` のマクロ名リストに `has_body` 条件だけで登録しているため、
単一文字のオブジェクトマクロ名（`s`, `c`, `n` 等）が既知シンボルと誤判定
される可能性がある。

→ 要調査: `s`, `c`, `n` 等が `MacroInferContext.macros` に
オブジェクトマクロとして登録されていないか確認。

**改善策C-1: KnownSymbols マクロ登録条件の厳格化**

`KnownSymbols` にマクロ名を登録する条件を厳格化:
- `info.is_function` が true のマクロのみ登録
- オブジェクトマクロ（定数展開）は含めない

---

### D. 型名の未解決 (5 エラー)

| シンボル | E0425 数 |
|----------|---------|
| `body_details` | 2 |
| `PerlIO_funcs` | 2 |
| `caddr_t` | 1 |

これらは C の型名で、Rust の FFI bindings に含まれていない。
`ExprKind::Cast` の型名パスは `ExprKind::Ident` を経由しないため
UNRESOLVED_NAMES で検出されない。

**改善策D-1**: 型名の未解決検出は `type_name_to_rust` のパスで
別途チェックする必要がある。ただし5件と少なく優先度は低い。

---

### E. その他 (3 エラー)

| シンボル | E0425 数 | 原因 |
|----------|---------|------|
| `cophh_exists_sv` | 1 | マクロ呼び出し保持、実体なし |
| `cophh_exists_pvs` | 1 | 同上 |
| `cophh_exists_pv` | 1 | 同上 |

`cophh_exists_*` は `cop.h` のマクロだが、bindings.rs にもマクロ辞書にもない。
改善策B でカバー可能。

---

## 改善策の優先度と期待効果

| 優先度 | 改善策 | 対象 | 期待 E0425 削減 | 実装コスト |
|--------|--------|------|----------------|-----------|
| 1 | B-3: 非生成関数の KnownSymbols 除外 | カスケード依存 | ~29 + カスケード | 小（事前計算のみ） |
| 2 | A-1: ビルトインリスト分割 | C stdlib | ~19 + カスケード | 小（リスト編集） |
| 3 | C-1: マクロ登録条件厳格化 | ローカル変数 | ~10 | 極小（条件追加） |
| 4 | B-2: should_emit_as_macro_call 強化 | カスケード依存 | B-3 と重複 | 小 |
| 5 | D-1: 型名チェック | 型名 | ~5 | 中 |

### 推奨実装順序

**Phase 1 (即実行可能)**: A-1 + C-1 — ビルトインリストの整理とマクロ登録条件の修正。
影響範囲が小さく、副作用リスクが低い。

**Phase 2**: B-3 — 非生成関数の KnownSymbols 除外。
`get_macro_status` の結果を事前収集して `KnownSymbols` 構築に反映。

**Phase 3 (任意)**: D-1 — 型名チェック。5件のみなので必要に応じて。

### 全体目標

現在の E0425: 147 → 目標: 30 以下（Phase 1+2 完了時）
