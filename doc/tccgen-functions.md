# TinyCC パーサー/意味解析 (tccgen.c) 関数詳細分析

## 概要

- **ファイル**: `tinycc/tccgen.c`
- **行数**: 約8944行
- **役割**: パーサー、意味解析、型チェック、シンボルテーブル管理、コード生成基盤

---

## 1. 初期化・メインエントリーポイント

| 関数 | シグネチャ | 役割 |
|------|-----------|------|
| `tccgen_init` | `void tccgen_init(TCCState *s1)` | 値スタック、型、優先順位パーサー初期化 |
| `tccgen_compile` | `int tccgen_compile(TCCState *s1)` | メインコンパイルエントリポイント、グローバル宣言解析 |
| `tccgen_finish` | `void tccgen_finish(TCCState *s1)` | クリーンアップ、メモリ解放、コンパイル終了 |

---

## 2. シンボルテーブル管理

### 基本操作

| 関数 | シグネチャ | 役割 |
|------|-----------|------|
| `sym_push2` | `Sym *sym_push2(Sym **ps, int v, int t, int c)` | 低レベルシンボル作成・スタックプッシュ |
| `sym_find2` | `Sym *sym_find2(Sym *s, int v)` | スタック内シンボル検索 |
| `sym_push` | `Sym *sym_push(int v, CType *type, int r, int c)` | 適切なスタック（local/global）へシンボルプッシュ |
| `sym_find` | `Sym *sym_find(int v)` | シンボルテーブルで識別子検索 |
| `sym_link` | `void sym_link(Sym *s, int yes)` | シンボルのパーサーへの可視性設定 |
| `sym_pop` | `void sym_pop(Sym **ptop, Sym *b, int keep)` | 境界までシンボルをポップ |
| `struct_find` | `Sym *struct_find(int v)` | struct/union/enumタグ検索 |

### ラベル・スコープ

| 関数 | シグネチャ | 役割 |
|------|-----------|------|
| `label_find` | `Sym *label_find(int v)` | ラベルシンボル検索 |
| `label_push` | `Sym *label_push(Sym **ptop, int v, int flags)` | ラベルシンボル作成 |
| `label_pop` | `void label_pop(Sym **ptop, Sym *slast, int keep)` | ラベルの検証とポップ |
| `sym_scope_ex` | `int sym_scope_ex(Sym *s)` | シンボルのスコープ取得 |
| `global_identifier_push` | `Sym *global_identifier_push(int v, int t, int c)` | グローバル識別子プッシュ |

---

## 3. 型宣言解析

### 基本型解析

| 関数 | シグネチャ | 役割 |
|------|-----------|------|
| `parse_btype` | `int parse_btype(CType *type, AttributeDef *ad, int ignore_label)` | 基本型解析（int, float, struct等） |
| `type_decl` | `CType *type_decl(CType *type, AttributeDef *ad, int *v, int td)` | 完全な宣言子解析（ポインタ、関数、配列） |
| `post_type` | `int post_type(CType *type, AttributeDef *ad, int storage, int td)` | 後置宣言子解析（配列[]、関数()） |
| `parse_btype_qualify` | `void parse_btype_qualify(CType *type, int qualifiers)` | 型修飾子適用（const, volatile） |
| `parse_attribute` | `void parse_attribute(AttributeDef *ad)` | GCC属性解析 |

### 構造体・列挙型

| 関数 | シグネチャ | 役割 |
|------|-----------|------|
| `struct_decl` | `void struct_decl(CType *type, int u)` | struct, union, enum定義解析 |
| `struct_layout` | `void struct_layout(CType *type, AttributeDef *ad)` | struct/unionレイアウト計算（オフセット、アライメント） |
| `check_fields` | `void check_fields(CType *type, int check)` | structフィールド検証 |
| `find_field` | `Sym * find_field(CType *type, int v, int *cumofs)` | struct内のフィールド名検索 |

---

## 4. 式解析（演算子優先順位順）

### 優先順位降順

| 関数 | 優先順位 | 演算子 |
|------|----------|--------|
| `gexpr` | 最低 | `,` (カンマ) |
| `expr_eq` | | `=`, `+=`, `-=`, `*=`, `/=`, `%=`, `&=`, `\|=`, `^=`, `<<=`, `>>=` |
| `expr_cond` | | `?:` (三項条件) |
| `expr_lor` | | `\|\|` (論理OR) |
| `expr_land` | | `&&` (論理AND) |
| `expr_or` | | `\|` (ビットOR) |
| `expr_xor` | | `^` (ビットXOR) |
| `expr_and` | | `&` (ビットAND) |
| `expr_cmpeq` | | `==`, `!=` (等価) |
| `expr_cmp` | | `<`, `>`, `<=`, `>=` (比較) |
| `expr_shift` | | `<<`, `>>` (シフト) |
| `expr_sum` | | `+`, `-` (加減算) |
| `expr_prod` | | `*`, `/`, `%` (乗除算) |
| `unary` | 最高 | 単項演算子、一次式 |

### 補助関数

| 関数 | シグネチャ | 役割 |
|------|-----------|------|
| `expr_landor` | `void expr_landor(int op)` | 短絡評価付き論理AND/OR処理 |
| `expr_infix` | `void expr_infix(int p)` | 二項演算子の優先順位クライミングパーサー |
| `init_prec` | `void init_prec(void)` | 演算子優先順位テーブル初期化 |
| `precedence` | `int precedence(int tok)` | 演算子優先順位レベル取得 |

---

## 5. 定数式解析

| 関数 | シグネチャ | 役割 |
|------|-----------|------|
| `expr_const1` | `void expr_const1(void)` | 定数式解析 |
| `expr_const` | `int expr_const(void)` | 32ビット整数定数を解析・返却 |
| `expr_const64` | `int64_t expr_const64(void)` | 64ビット整数定数を解析・返却 |

---

## 6. 値スタック（仮想スタック）操作

### プッシュ操作

| 関数 | シグネチャ | 役割 |
|------|-----------|------|
| `vsetc` | `void vsetc(CType *type, int r, CValue *vc)` | 型付き値をvstackにプッシュ |
| `vpush` | `void vpush(CType *type)` | 型をプッシュ（vstack上に値作成） |
| `vpush64` | `void vpush64(int ty, unsigned long long v)` | 64ビット定数プッシュ |
| `vpushi` | `void vpushi(int v)` | 32ビット整数定数プッシュ |
| `vpushs` | `void vpushs(addr_t v)` | アドレス定数プッシュ |
| `vpushll` | `void vpushll(long long v)` | long long定数プッシュ |
| `vpushv` | `void vpushv(SValue *v)` | SValue構造体プッシュ |
| `vpushsym` | `void vpushsym(CType *type, Sym *sym)` | シンボル参照プッシュ |
| `vpush_helper_func` | `void vpush_helper_func(int v)` | ビルトインヘルパー関数プッシュ |
| `vpush_type_size` | `void vpush_type_size(CType *type, int *a)` | 型サイズを定数としてプッシュ |

### スタック操作

| 関数 | シグネチャ | 役割 |
|------|-----------|------|
| `vset` | `void vset(CType *type, int r, int v)` | 値の型と位置設定 |
| `vseti` | `void vseti(int r, int v)` | 整数値設定 |
| `vswap` | `void vswap(void)` | トップ2値の交換 |
| `vpop` | `void vpop(void)` | トップ値ポップ |
| `vdup` | `void vdup(void)` | トップ値複製 |
| `vrotb` | `void vrotb(int n)` | vstack値を後方回転 |
| `vrott` | `void vrott(int n)` | vstack値をトップ方向回転 |
| `vrev` | `void vrev(int n)` | vstack上のn値を反転 |
| `vcheck_cmp` | `void vcheck_cmp(void)` | 必要ならVT_CMPをレジスタに変換 |

---

## 7. 型システムとキャスト

### 型サイズ・情報

| 関数 | シグネチャ | 役割 |
|------|-----------|------|
| `type_size` | `int type_size(CType *type, int *a)` | 型サイズとアライメント計算 |
| `pointed_type` | `CType *pointed_type(CType *type)` | ポインタ先の型取得 |
| `mk_pointer` | `void mk_pointer(CType *type)` | 型をポインタ型に変換 |
| `pointed_size` | `int pointed_size(CType *type)` | ポインタ先型のサイズ取得 |
| `btype_size` | `int btype_size(int bt)` | 基本型サイズ取得 |

### 型チェック・互換性

| 関数 | シグネチャ | 役割 |
|------|-----------|------|
| `is_compatible_types` | `int is_compatible_types(CType *type1, CType *type2)` | 型互換性チェック |
| `is_compatible_unqualified_types` | `int is_compatible_unqualified_types(CType *type1, CType *type2)` | 修飾子無視の型互換性 |
| `compare_types` | `int compare_types(CType *type1, CType *type2, int unqualified)` | 深い型比較 |
| `is_compatible_func` | `int is_compatible_func(CType *type1, CType *type2)` | 関数型互換性 |
| `combine_types` | `int combine_types(CType *dest, SValue *op1, SValue *op2, int op)` | 演算のオペランド型結合 |
| `is_float` | `int is_float(int t)` | 浮動小数点型チェック |
| `is_integer_btype` | `int is_integer_btype(int bt)` | 整数基本型チェック |

### キャスト生成

| 関数 | シグネチャ | 役割 |
|------|-----------|------|
| `gen_cast` | `void gen_cast(CType *type)` | キャスト操作生成 |
| `gen_cast_s` | `void gen_cast_s(int t)` | 特定型へのキャスト生成 |
| `gen_cvt_itof1` | `void gen_cvt_itof1(int t)` | 整数→浮動小数点変換生成 |
| `gen_cvt_ftoi1` | `void gen_cvt_ftoi1(int t)` | 浮動小数点→整数変換生成 |
| `force_charshort_cast` | `void force_charshort_cast(void)` | char/shortキャスト強制 |
| `cast_error` | `void cast_error(CType *st, CType *dt)` | 無効キャストエラー報告 |
| `verify_assign_cast` | `void verify_assign_cast(CType *dt)` | 代入キャスト検証 |
| `gen_assign_cast` | `void gen_assign_cast(CType *dt)` | キャスト付き代入生成 |

### 型エラー報告

| 関数 | シグネチャ | 役割 |
|------|-----------|------|
| `type_incompatibility_error` | `void type_incompatibility_error(CType* st, CType* dt, const char* fmt)` | 型不一致エラー報告 |
| `type_incompatibility_warning` | `void type_incompatibility_warning(CType* st, CType* dt, const char* fmt)` | 型不一致警告 |

---

## 8. 二項演算と意味解析

### 演算生成

| 関数 | シグネチャ | 役割 |
|------|-----------|------|
| `gen_op` | `void gen_op(int op)` | 型チェック付き二項演算子コード生成 |
| `gen_opic` | `void gen_opic(int op)` | 整数演算コード生成 |
| `gen_opif` | `void gen_opif(int op)` | 浮動小数点演算コード生成 |
| `gen_test_zero` | `void gen_test_zero(int op)` | ゼロ比較テスト生成 |
| `gen_negf` | `void gen_negf(int op)` | 浮動小数点否定生成 |

### レジスタ・値操作

| 関数 | シグネチャ | 役割 |
|------|-----------|------|
| `gv` | `int gv(int rc)` | 値をレジスタに変換 |
| `gv2` | `void gv2(int rc1, int rc2)` | 2値をレジスタに変換 |
| `gv_dup` | `void gv_dup(void)` | レジスタ内の値を複製 |
| `gvtst` | `int gvtst(int inv, int t)` | 条件テスト/ジャンプ生成 |
| `gvtst_set` | `void gvtst_set(int inv, int t)` | テストのジャンプターゲット設定 |
| `condition_3way` | `int condition_3way(void)` | 条件を0/1/-1に評価 |
| `is_cond_bool` | `int is_cond_bool(SValue *sv)` | 値がブール条件かチェック |

---

## 9. 単項・アドレス操作

| 関数 | シグネチャ | 役割 |
|------|-----------|------|
| `indir` | `void indir(void)` | ポインタ間接参照 (*ptr) |
| `gaddrof` | `void gaddrof(void)` | アドレス取得操作生成 |
| `vstore` | `void vstore(void)` | ストア操作生成 |
| `inc` | `void inc(int post, int c)` | 前置/後置インクリメント/デクリメント |
| `test_lvalue` | `void test_lvalue(void)` | 代入用左辺値検証 |

---

## 10. ストレージ・外部シンボル管理

| 関数 | シグネチャ | 役割 |
|------|-----------|------|
| `update_storage` | `void update_storage(Sym *sym)` | ELFシンボル属性更新 |
| `put_extern_sym` | `void put_extern_sym(Sym *sym, Section *s, addr_t value, unsigned long size)` | 外部シンボル作成 |
| `external_global_sym` | `Sym *external_global_sym(int v, CType *type)` | グローバル外部シンボル取得/作成 |
| `external_sym` | `Sym *external_sym(int v, CType *type, int r, AttributeDef *ad)` | 外部シンボル処理 |
| `patch_type` | `void patch_type(Sym *sym, CType *type)` | シンボル型更新 |
| `patch_storage` | `void patch_storage(Sym *sym, AttributeDef *ad, CType *type)` | ストレージクラス更新 |
| `sym_copy` | `Sym *sym_copy(Sym *s0, Sym **ps)` | シンボル構造体コピー |
| `move_ref_to_global` | `void move_ref_to_global(Sym *s)` | 参照をグローバルに昇格 |
| `elfsym` | `ElfSym *elfsym(Sym *s)` | ELFシンボルエントリ取得 |
| `greloc` | `void greloc(Section *s, Sym *sym, unsigned long offset, int type)` | リロケーションエントリ追加 |
| `get_sym_ref` | `Sym *get_sym_ref(CType *type, Section *sec, unsigned long offset, unsigned long size)` | シンボル参照作成 |

---

## 11. 宣言と初期化

### 宣言解析

| 関数 | シグネチャ | 役割 |
|------|-----------|------|
| `decl` | `int decl(int l)` | グローバル/ローカル宣言解析 |
| `type_decl` | `CType *type_decl(CType *type, AttributeDef *ad, int *v, int td)` | 型付き宣言子解析 |

### 初期化

| 関数 | シグネチャ | 役割 |
|------|-----------|------|
| `decl_initializer` | `void decl_initializer(init_params *p, CType *type, unsigned long c, int flags)` | 初期化子解析 |
| `init_putv` | `void init_putv(init_params *p, CType *type, unsigned long c)` | 初期化コード生成 |
| `decl_design_flex` | `void decl_design_flex(init_params *p, Sym *ref, int index)` | 柔軟配列初期化処理 |
| `decl_design_delrels` | `void decl_design_delrels(Section *sec, int c, int size)` | 指示子ベース初期化処理 |
| `parse_init_elem` | `void parse_init_elem(int expr_type)` | 初期化子要素解析 |
| `init_assert` | `void init_assert(init_params *p, int offset)` | 初期化オフセット検証 |
| `init_putz` | `void init_putz(init_params *p, unsigned long c, int size)` | ゼロ初期化 |

---

## 12. ブロック・文解析

| 関数 | シグネチャ | 役割 |
|------|-----------|------|
| `block` | `void block(int flags)` | 文またはブロック解析 |
| `lblock` | `void lblock(int *bsym, int *csym)` | ループ/switch本体解析 |
| `gexpr_decl` | `void gexpr_decl(void)` | 宣言かもしれない式の解析 |
| `parse_expr_type` | `void parse_expr_type(CType *type)` | キャストまたは式型解析 |
| `parse_type` | `void parse_type(CType *type)` | 型仕様解析 |

---

## 13. スコープ・変数管理

### スコープ操作

| 関数 | シグネチャ | 役割 |
|------|-----------|------|
| `new_scope` | `void new_scope(struct scope *o)` | 新規ブロックスコープ開始 |
| `prev_scope` | `void prev_scope(struct scope *o, int is_expr)` | ブロックスコープ終了 |
| `leave_scope` | `void leave_scope(struct scope *o)` | クリーンアップ付きスコープ離脱 |
| `new_scope_s` | `void new_scope_s(struct scope *o)` | 文スコープ開始（簡易） |
| `prev_scope_s` | `void prev_scope_s(struct scope *o)` | 文スコープ終了 |

### VLA・クリーンアップ

| 関数 | シグネチャ | 役割 |
|------|-----------|------|
| `vla_restore` | `void vla_restore(int loc)` | VLAスタック位置復元 |
| `vla_leave` | `void vla_leave(struct scope *o)` | スコープ内VLAクリーンアップ |
| `save_lvalues` | `void save_lvalues(void)` | クリーンアップ前に左辺値保存 |
| `block_cleanup` | `void block_cleanup(struct scope *o)` | クリーンアップコード実行 |
| `try_call_scope_cleanup` | `void try_call_scope_cleanup(Sym *stop)` | スコープクリーンアップハンドラ呼び出し |
| `try_call_cleanup_goto` | `void try_call_cleanup_goto(Sym *cleanupstate)` | goto用クリーンアップ呼び出し |
| `add_local_bounds` | `void add_local_bounds(Sym *s, Sym *e)` | ローカル変数の境界チェック追加 |

---

## 14. 関数生成と呼び出し

### 関数生成

| 関数 | シグネチャ | 役割 |
|------|-----------|------|
| `gen_function` | `void gen_function(Sym *sym)` | 関数コード生成 |
| `gfunc_set_param` | `Sym *gfunc_set_param(Sym *s, int c, int byref)` | 関数パラメータ設定 |
| `sym_push_params` | `void sym_push_params(Sym *ref)` | パラメータシンボルプッシュ |
| `gfunc_param_typed` | `void gfunc_param_typed(Sym *func, Sym *arg)` | 関数呼び出しパラメータ型チェック |
| `gfunc_return` | `void gfunc_return(CType *func_type)` | return文コード生成 |
| `check_func_return` | `void check_func_return(void)` | 関数return検証 |

### インライン関数

| 関数 | シグネチャ | 役割 |
|------|-----------|------|
| `gen_inline_functions` | `void gen_inline_functions(TCCState *s)` | インライン関数本体生成 |
| `free_inline_functions` | `void free_inline_functions(TCCState *s)` | インライン関数データ解放 |

### VLA引数

| 関数 | シグネチャ | 役割 |
|------|-----------|------|
| `func_vla_arg` | `void func_vla_arg(Sym *sym)` | VLA関数引数処理 |
| `func_vla_arg_code` | `void func_vla_arg_code(Sym *arg)` | VLA引数コード生成 |

---

## 15. レジスタ割り当てとストレージ

| 関数 | シグネチャ | 役割 |
|------|-----------|------|
| `save_regs` | `void save_regs(int n)` | 上位nレジスタ保存 |
| `save_reg` | `void save_reg(int r)` | 単一レジスタ保存 |
| `save_reg_upstack` | `void save_reg_upstack(int r, int n)` | スタック上方へレジスタ保存 |
| `get_reg` | `int get_reg(int rc)` | クラスに合致するレジスタ割り当て |
| `get_reg_ex` | `int get_reg_ex(int rc, int rc2)` | 制約付きレジスタ割り当て |
| `get_temp_local_var` | `int get_temp_local_var(int size,int align, int *r2)` | 一時ローカル変数割り当て |
| `move_reg` | `void move_reg(int r, int s, int t)` | レジスタ間の値移動 |

---

## 16. Switch/Case文

| 関数 | シグネチャ | 役割 |
|------|-----------|------|
| `gcase` | `int gcase(struct case_t **base, int len, int dsym)` | caseディスパッチ生成 |
| `end_switch` | `void end_switch(void)` | switch文終了処理 |
| `case_sort` | `void case_sort(struct switch_t *sw)` | case値ソート |
| `case_cmp` | `int case_cmp(uint64_t a, uint64_t b)` | case値比較 |
| `case_cmp_qs` | `int case_cmp_qs(const void *pa, const void *pb)` | クイックソート用case比較 |

---

## 17. 境界チェックとデバッグ

| 関数 | シグネチャ | 役割 |
|------|-----------|------|
| `gbound` | `void gbound(void)` | 境界チェックコード生成 |
| `gbound_args` | `void gbound_args(int nb_args)` | 引数の境界チェック追加 |
| `gen_bounded_ptr_add` | `void gen_bounded_ptr_add(void)` | ポインタ演算の境界チェック追加 |
| `gen_bounded_ptr_deref` | `void gen_bounded_ptr_deref(void)` | 参照外しの境界チェック追加 |

### ビットフィールド

| 関数 | シグネチャ | 役割 |
|------|-----------|------|
| `load_packed_bf` | `void load_packed_bf(CType *type, int bit_pos, int bit_size)` | ビットフィールド値ロード |
| `store_packed_bf` | `void store_packed_bf(int bit_pos, int bit_size)` | ビットフィールド値ストア |
| `adjust_bf` | `int adjust_bf(SValue *sv, int bit_pos, int bit_size)` | ビットフィールドアクセス調整 |
| `incr_bf_adr` | `void incr_bf_adr(int o)` | ビットフィールドアドレス増分 |

---

## 18. ユーティリティと属性関数

| 関数 | シグネチャ | 役割 |
|------|-----------|------|
| `parse_builtin_params` | `void parse_builtin_params(int nc, const char *args)` | ビルトイン関数パラメータ解析 |
| `parse_mult_str` | `CString* parse_mult_str(const char *msg)` | 複数文字列連結解析 |
| `exact_log2p1` | `int exact_log2p1(int i)` | log2(i)+1計算 |
| `do_Static_assert` | `void do_Static_assert(void)` | _Static_assert処理 |
| `convert_parameter_type` | `void convert_parameter_type(CType *pt)` | パラメータ型変換（配列→ptr、関数→ptr） |
| `merge_symattr` | `void merge_symattr(struct SymAttr *sa, struct SymAttr *sa1)` | シンボル属性マージ |
| `merge_funcattr` | `void merge_funcattr(struct FuncAttr *fa, struct FuncAttr *fa1)` | 関数属性マージ |
| `merge_attr` | `void merge_attr(AttributeDef *ad, AttributeDef *ad1)` | 属性定義マージ |
| `skip_or_save_block` | `void skip_or_save_block(TokenString **str)` | トークンブロックスキップまたは保存 |
| `parse_atomic` | `void parse_atomic(int atok)` | _Atomic指定子解析 |
| `ieee_finite` | `int ieee_finite(double d)` | doubleが有限かチェック |
| `in_range` | `int in_range(long long n, int t)` | 値が型範囲内かチェック |

---

## 19. Long Long・64ビット操作

| 関数 | シグネチャ | 役割 |
|------|-----------|------|
| `vset_VT_CMP` | `void vset_VT_CMP(int op)` | VT_CMP比較結果設定 |
| `lexpand` | `void lexpand(void)` | long long操作展開 |
| `lbuild` | `void lbuild(int t)` | 32ビット部分からlong long構築 |
| `gen_opl` | `void gen_opl(int op)` | long long演算生成 |
| `value64` | `uint64_t value64(uint64_t l1, int t)` | 64ビット値抽出 |
| `gen_opic_sdiv` | `uint64_t gen_opic_sdiv(uint64_t a, uint64_t b)` | コンパイル時符号付き除算 |
| `gen_opic_lt` | `int gen_opic_lt(uint64_t a, uint64_t b)` | コンパイル時less-than比較 |
| `is_null_pointer` | `int is_null_pointer(SValue *p)` | ヌルポインタ定数チェック |

---

## 20. 主要呼び出しグラフ

### コンパイルメインフロー

```
tccgen_compile() [エントリポイント]
    │
    ├─→ parse_flags 初期化
    ├─→ next() [最初のトークン取得]
    ├─→ decl(VT_CONST) [グローバル宣言解析]
    │   │
    │   ├─→ parse_btype() [基本型解析]
    │   ├─→ type_decl() [宣言子解析]
    │   └─→ [関数本体の場合]
    │       └─→ gen_function() [関数コード生成]
    │
    ├─→ gen_inline_functions(s1) [インライン関数生成]
    ├─→ check_vstack()
    └─→ tccgen_finish()
```

### 式解析カスケード

```
gexpr() [カンマ演算子、最低優先順位]
    │
    └─→ expr_eq() [代入演算子]
        │
        └─→ expr_cond() [三項条件 ?:]
            │
            └─→ expr_lor() [論理OR ||]
                │
                └─→ expr_land() [論理AND &&]
                    │
                    └─→ expr_or() [ビットOR |]
                        │
                        └─→ ... [中間優先順位]
                            │
                            └─→ expr_prod() [乗除算 * / %]
                                │
                                └─→ unary() [単項演算子、一次式]
```

### 型解析フロー

```
parse_btype() [基本型: int, float, struct等]
    │
    ├─→ [ストレージクラス、型修飾子分類]
    ├─→ struct_decl() [struct/union/enumの場合]
    │   │
    │   ├─→ sym_push() [struct シンボル作成]
    │   └─→ struct_layout() [メンバオフセット計算]
    │
    └─→ type_decl() [完全な宣言子]
        │
        └─→ post_type() [後置: *, [], ()]
```

### シンボルテーブル管理

```
sym_push() [スタックにシンボル追加]
    │
    ├─→ sym_push2() [Sym構造体作成]
    ├─→ [非匿名の場合]
    │   └─→ sym_link() [パーサーに可視化]
    │       └─→ table_ident[] トークンテーブル更新
    │
    └─→ [同一スコープでの再宣言チェック]

sym_find() [識別子検索]
    │
    └─→ table_ident[] トークンテーブル検索
        │
        └─→ リンクリストのトップシンボル返却

sym_pop() [スコープからシンボル除去]
    │
    ├─→ sym_link() [非可視化]
    └─→ sym_free() [keepでなければメモリ解放]
```
