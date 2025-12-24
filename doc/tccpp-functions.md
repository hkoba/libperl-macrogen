# TinyCC プリプロセッサ (tccpp.c) 関数詳細分析

## 概要

- **ファイル**: `tinycc/tccpp.c`
- **行数**: 約4092行
- **役割**: Cプリプロセッサ実装（トークン読み取り、マクロ展開、#include処理、条件コンパイル）

---

## 1. 主要エントリーポイント

### 1.1 `next(void)` [Line 3598]
- **役割**: メイントークン取得関数（マクロ展開付き）
- **戻り値**: void（グローバル変数 `tok` と `tokc` に設定）
- **処理フロー**:
  1. `macro_ptr` が設定されていれば、マクロストリームから読み取り
  2. そうでなければ `next_nomacro()` で生トークンを読み取り
  3. 識別子の場合、マクロ定義をチェックし展開

### 1.2 `preprocess(int is_bof)` [Line 1889]
- **役割**: プリプロセッサディレクティブ処理
- **パラメータ**: `is_bof` - ファイル先頭フラグ
- **処理対象**: #define, #undef, #include, #ifdef, #ifndef, #if, #elif, #else, #endif, #pragma, #line

### 1.3 `preprocess_start(TCCState *s1, int filetype)` [Line 3763]
- **役割**: プリプロセッサ状態の初期化
- **設定内容**: インクルードスタック、ifdefスタック、事前定義マクロ、パースフラグ

### 1.4 `preprocess_end(TCCState *s1)` [Line 3799]
- **役割**: プリプロセッサのクリーンアップ

---

## 2. トークン入力関数

### 2.1 `next_nomacro(void)` [Line 2670]
- **役割**: ソースから生トークンを読み取り（マクロ展開なし）
- **処理内容**: 識別子、数値、文字列、演算子、コメントの認識
- **トリガー**: 行頭の '#' で `preprocess()` を呼び出し

### 2.2 `next_c(void)` [Line 746]
- **役割**: バッファから次の文字を読み取り
- **呼び出し先**: 生文字読み取り関数群

### 2.3 `handle_eob(void)` [Line 713]
- **役割**: バッファ終端処理、ファイルからの読み込み
- **処理**: `read()` システムコール、バッファ再充填、EOF検出

### 2.4 `handle_stray(uint8_t **p)` [Line 794]
- **役割**: バックスラッシュ継続の処理
- **機能**: バックスラッシュ+改行による行継続

### 2.5 `handle_bs(uint8_t **p)` [Line 783]
- **役割**: 低レベルバックスラッシュ処理ヘルパー

---

## 3. コメント・文字列解析

### 3.1 `parse_line_comment(uint8_t *p)` [Line 822]
- **役割**: C++スタイルコメント (//) のスキップ
- **戻り値**: コメント終了後のポインタ

### 3.2 `parse_comment(uint8_t *p)` [Line 847]
- **役割**: Cスタイルコメント (/* */) のスキップ
- **戻り値**: コメント終了後のポインタ

### 3.3 `parse_pp_string(uint8_t *p, int sep, CString *str)` [Line 886]
- **役割**: プリプロセッサ文字列リテラルの解析
- **パラメータ**: `sep` - '<'/'>' (#include用) または '"' (文字列用)
- **戻り値**: 文字列終了後のポインタ

---

## 4. マクロ定義・検索

### 4.1 `parse_define(void)` [Line 1616]
- **役割**: #define ディレクティブの解析と登録
- **処理内容**:
  - オブジェクトマクロ、関数マクロ、可変引数マクロの処理
  - マクロ本体のトークンストリーム化
  - `define_push()` による登録

### 4.2 `define_push(int v, int macro_type, int *str, Sym *first_arg)` [Line 1349]
- **役割**: マクロ定義の登録
- **パラメータ**:
  - `v`: トークンID
  - `macro_type`: MACRO_OBJ/MACRO_FUNC
  - `str`: マクロ本体
  - `first_arg`: パラメータリスト

### 4.3 `define_find(int v)` [Line 1371]
- **役割**: トークンIDによるマクロ検索
- **戻り値**: `Sym*` または NULL

### 4.4 `define_undef(Sym *s)` [Line 1364]
- **役割**: マクロの未定義化

### 4.5 `free_defines(Sym *b)` [Line 1380]
- **役割**: スタックトップからマーカー `b` までの定義を解放

---

## 5. マクロ展開

### 5.1 `macro_subst_tok(TokenString *tok_str, Sym **nested_list, Sym *s)` [Line 3365]
- **役割**: 単一マクロトークンの展開、関数マクロ引数の処理
- **処理フロー**:
  1. 関数マクロかチェック
  2. 入力から引数リストを読み取り
  3. 本体に引数を置換 (`macro_arg_subst`)
- **戻り値**: nosubstフラグ（再展開防止）
- **呼び出し元**: `next()`, `macro_subst()`

### 5.2 `macro_subst(TokenString *tok_str, Sym **nested_list, const int *macro_str)` [Line 3536]
- **役割**: マクロ本体トークンの再帰的展開
- **処理フロー**:
  1. マクロ本体を走査
  2. ネストした識別子を再帰展開
  3. `nested_list` による無限再帰防止
- **戻り値**: nosubstフラグ
- **呼び出し元**: `macro_subst_tok()`, `macro_arg_subst()`

### 5.3 `macro_arg_subst(Sym **nested_list, const int *macro_str, Sym *args)` [Line 3114]
- **役割**: マクロ本体内の引数置換
- **処理内容**:
  - `#` 文字列化演算子の処理
  - `##` トークン連結演算子の処理
  - 引数トークンの展開
- **戻り値**: 展開済みトークンストリング
- **呼び出し元**: `macro_subst_tok()`

### 5.4 `macro_twosharps(const int *ptr0)` [Line 3234]
- **役割**: `##` (トークン貼り付け) 演算子の処理
- **処理**: 隣接トークンを単一トークンに結合
- **戻り値**: 結果トークンストリング

---

## 6. ストリームナビゲーション

### 6.1 `next_argstream(Sym **nested_list, TokenString *ws_str)` [Line 3328]
- **役割**: マクロ引数ストリームまたはファイルからトークン読み取り
- **処理**: マクロスタックレベルを遡ってトークンを検索
- **パラメータ**: `ws_str`=NULL で読み取り、非NULL でピーク

### 6.2 `peek_file(TokenString *ws_str)` [Line 3291]
- **役割**: ソースファイルの次トークンをピーク
- **処理**: コメント、空白のスキップ

---

## 7. インクルード処理

### 7.1 `parse_include(TCCState *s1, int do_next, int test)` [Line 1421]
- **役割**: #include および #include_next ディレクティブの処理
- **処理フロー**:
  1. インクルードパスを検索
  2. ファイルを開く
  3. インクルードスタックを管理

### 7.2 `search_cached_include(TCCState *s1, const char *filename, int add)` [Line 1702]
- **役割**: インクルードファイルのキャッシュ確認/追加
- **最適化**: ヘッダーガード付きインクルードの再解析回避

---

## 8. 条件コンパイル

### 8.1 `expr_preprocess(TCCState *s1)` [Line 1532]
- **役割**: #if/#elif の定数式評価
- **戻り値**: ブール値結果

### 8.2 `preprocess_skip(void)` [Line 949]
- **役割**: 偽の #if/#ifdef ブロックのスキップ
- **処理**: ネスト深度を追跡し、対応する #else/#endif までスキップ

---

## 9. トークンストリーム管理

### 9.1 `tok_str_alloc(void)` [Line 1085]
- **役割**: 新規トークンストリング確保
- **戻り値**: `TokenString*`

### 9.2 `tok_str_realloc(TokenString *s, int new_size)` [Line 1103]
- **役割**: トークンストリング配列の拡張

### 9.3 `tok_str_free(TokenString *str)` [Line 1097]
- **役割**: トークンストリングメモリの解放

### 9.4 `tok_str_add(TokenString *s, int t)` [Line 1120]
- **役割**: トークンストリングにトークン追加

### 9.5 `tok_str_add_tok(TokenString *s)` [Line 1231]
- **役割**: 現在のトークン (tok/tokc) をトークンストリングに追加

### 9.6 `tok_str_add2(TokenString *s, int t, CValue *cv)` [Line 1158]
- **役割**: 値付きトークン（数値/文字列データ）を追加

### 9.7 `tok_get(int *t, const int **pp, CValue *cv)` [Line 1254]
- **役割**: ストリームから値付きトークンを抽出
- **処理**: 可変長トークンエンコーディングの処理

### 9.8 `begin_macro(TokenString *str, int alloc)` [Line 1132]
- **役割**: マクロ展開コンテキストをスタックにプッシュ
- **パラメータ**: `alloc` - 0=静的, 1=動的, 2=解放なし

### 9.9 `end_macro(void)` [Line 1142]
- **役割**: マクロ展開コンテキストをスタックからポップ

### 9.10 `unget_tok(int last_tok)` [Line 3657]
- **役割**: 現在のトークンをプッシュバック
- **用途**: 先読みのロールバック

---

## 10. シンボル・識別子管理

### 10.1 `tok_alloc(const char *str, int len)` [Line 573]
- **役割**: 文字列をトークンIDとしてインターン
- **戻り値**: 割り当てられたトークンIDを持つ `TokenSym*`

### 10.2 `tok_alloc_new(TokenSym **pts, const char *str, int len)` [Line 538]
- **役割**: 新規トークンシンボルの作成

### 10.3 `tok_alloc_const(const char *str)` [Line 596]
- **役割**: 定数文字列のトークンID取得

### 10.4 `get_tok_str(int v, CValue *cv)` [Line 604]
- **役割**: トークンの文字列表現を取得
- **処理**: キーワード、識別子、リテラル

### 10.5 `macro_is_equal(const int *a, const int *b)` [Line 1329]
- **役割**: 2つのマクロ定義の等価性比較
- **用途**: 再定義警告

---

## 11. 文字列・数値変換

### 11.1 `parse_string(const char *s, int len)` [Line 2264]
- **役割**: プリプロセッサ文字列をC文字列リテラルに変換
- **処理**: エスケープシーケンス、ワイド文字

### 11.2 `parse_number(const char *p)` [Line 2345]
- **役割**: プリプロセッサ数値をCリテラルに変換
- **処理**: 整数、浮動小数点、16進、8進、2進

### 11.3 `parse_escape_string(CString *outstr, const uint8_t *buf, int is_long)` [Line 2084]
- **役割**: 文字列リテラル内のエスケープシーケンス処理
- **処理**: \n, \x, \u, ワイド文字

---

## 12. ユーティリティ・初期化

### 12.1 `set_idnum(int c, int val)` [Line 3809]
- **役割**: 文字を識別子文字として設定
- **戻り値**: 以前の設定

### 12.2 `tccpp_new(TCCState *s)` [Line 3816]
- **役割**: プリプロセッサ状態構造体の作成

### 12.3 `tccpp_delete(TCCState *s)` [Line 3869]
- **役割**: プリプロセッサ状態の破棄

### 12.4 `tccpp_putfile(const char *filename)` [Line 1866]
- **役割**: ファイル名の #line ディレクティブ出力

### 12.5 `tcc_preprocess(TCCState *s1)` [Line 4022]
- **役割**: スタンドアロンプリプロセッサ実行

### 12.6 `tcc_predefs(TCCState *s1, CString *cs, int is_asm)` [Line 3713]
- **役割**: 事前定義マクロの生成 (__LINE__, __FILE__, 等)

### 12.7 `pragma_parse(TCCState *s1)` [Line 1751]
- **役割**: #pragma ディレクティブの処理

### 12.8 `pp_error(CString *cs)` [Line 1607]
- **役割**: コンテキスト付きプリプロセッサエラーの報告

---

## 13. 呼び出しフロー

### トークン読み取りフロー

```
next()
  ├─ while(macro_ptr)  [展開済みマクロの処理]
  │   └─ tok_get()  [マクロストリームから抽出]
  └─ next_nomacro()  [生トークン読み取り]
      ├─ next_c()
      │   └─ handle_eob()  [ファイルから読み込み]
      └─ parse_ident_fast  [識別子/キーワードのトークン化]
```

### マクロ展開フロー

```
next()
  └─ next_nomacro()  [生トークン取得]
      └─ トークンが識別子でマクロ定義あり
          └─ define_find(t)
              └─ macro_subst_tok(nested_list, s)
                  ├─ if MACRO_FUNC:
                  │   ├─ next_argstream()  [マクロ引数読み取り]
                  │   │   ├─ peek_file()
                  │   │   └─ next_nomacro()
                  │   └─ macro_arg_subst()  [本体に引数置換]
                  │       ├─ macro_twosharps()  [##処理]
                  │       └─ macro_subst()  [再帰展開]
                  └─ macro_subst()  [マクロ本体展開]
                      └─ macro_subst_tok()  [ネストマクロ展開]
```

### プリプロセッサディレクティブフロー

```
next_nomacro()
  └─ 行頭の '#'
      └─ preprocess(is_bof)
          ├─ #define
          │   └─ parse_define()
          │       ├─ next_nomacro()  [マクロ名/引数解析]
          │       └─ tok_str_new/tok_str_add2_spc  [本体キャプチャ]
          │           └─ define_push()  [マクロ登録]
          ├─ #include/#include_next
          │   └─ parse_include()
          │       ├─ skip_spaces()
          │       ├─ parse_pp_string()  [ファイル名取得]
          │       ├─ search_cached_include()  [解析済みチェック]
          │       └─ tcc_open()  [インクルードファイルを開く]
          ├─ #if/#elif
          │   └─ expr_preprocess()  [条件評価]
          ├─ #ifdef/#ifndef
          │   └─ define_find()  [定義チェック]
          ├─ #else/#endif
          │   └─ ifdef_stack 操作
          └─ skip_to_eol()/preprocess_skip()
```

---

## 14. 主要データ構造

```c
// グローバルマクロコンテキスト
TokenString *macro_stack     // マクロ展開スタック
int *macro_ptr               // マクロストリーム内の現在位置

// グローバル解析状態
int tok                      // 現在のトークンID
CValue tokc                  // 現在のトークン値（文字列/数値）
int parse_flags              // 解析モードフラグ

// 定義シンボルストレージ
Sym *define_stack           // マクロ定義スタック
TokenSym **hash_ident       // 識別子ハッシュテーブル

// インクルードスタック
BufferedFile *file          // 現在の入力ファイル
TCCState->include_stack_ptr // インクルードファイルスタック

// 条件スタック
TCCState->ifdef_stack_ptr   // #if条件スタック
```
