# C言語 整数サイズ関連マクロ

## 概要

C言語の標準ヘッダーで定義される整数サイズ関連マクロについての調査結果。
Perl ヘッダー (`op_reg_common.h`) の処理中に発見した問題と解決策を記録。

## 問題の発生

`samples/wrapper.h` (Perl ヘッダー) を処理中、以下のエラーが発生:

```
Error: /usr/lib64/perl5/CORE/op_reg_common.h:152:1: preprocessor error: invalid preprocessor condition: expected ')'
```

原因: `#if` 条件式内で `UINTMAX_C(1)` マクロが展開されなかった。

## 関連するヘッダーファイル構造

### 1. GCC の stdint-gcc.h

場所: `/usr/lib/gcc/x86_64-redhat-linux/15/include/stdint-gcc.h`

```c
// 行 257-258
#undef UINTMAX_C
#define UINTMAX_C(c) __UINTMAX_C(c)
```

`__UINTMAX_C` は GCC のビルトインマクロで、実行時に以下のように定義される:

```c
#define __UINTMAX_C(c) c ## UL   // 64-bit
```

### 2. glibc の stdint.h

場所: `/usr/include/stdint.h`

```c
// 行 259-262
# if __WORDSIZE == 64
#  define UINTMAX_C(c)	c ## UL
# else
#  define UINTMAX_C(c)	c ## ULL
# endif
```

### 3. __WORDSIZE の定義

場所: `/usr/include/bits/wordsize.h`

```c
#if defined __x86_64__ && !defined __ILP32__
# define __WORDSIZE	64
#else
# define __WORDSIZE	32
#endif
```

## マクロの依存関係

```
UINTMAX_C(c)
    └── __UINTMAX_C(c)  [GCC builtin, または glibc の場合は __WORDSIZE に依存]
            └── c ## UL  または  c ## ULL
                    └── __WORDSIZE
                            └── __x86_64__, __ILP32__
```

## 必要なビルトインマクロ

Perl ヘッダーを正しく処理するには、以下の GCC ビルトインマクロが必要:

| マクロ | 説明 | x86_64 での値 |
|--------|------|---------------|
| `__GNUC__` | GCC メジャーバージョン | 14 など |
| `__GNUC_MINOR__` | GCC マイナーバージョン | 2 など |
| `__x86_64__` | x86-64 アーキテクチャ | 1 (定義済み) |
| `__STDC__` | 標準 C 準拠 | 1 |
| `__UINTMAX_C(c)` | 関数マクロ | `c ## UL` |
| `__INTMAX_C(c)` | 関数マクロ | `c ## L` |
| `__WORDSIZE` | ワードサイズ | 64 |

## TinyCC の解決策

TinyCC は `-D` オプションを `#define` ディレクティブとして処理する:

```c
// libtcc.c:845-853
LIBTCCAPI void tcc_define_symbol(TCCState *s1, const char *sym, const char *value)
{
    const char *eq;
    if (NULL == (eq = strchr(sym, '=')))
        eq = strchr(sym, 0);
    if (NULL == value)
        value = *eq ? eq + 1 : "1";
    cstr_printf(&s1->cmdline_defs, "#define %.*s %s\n", (int)(eq-sym), sym, value);
}
```

これにより、関数マクロも正しく処理できる:

```bash
-D'__UINTMAX_C(c)=c ## UL'
↓
#define __UINTMAX_C(c) c ## UL
```

## 本プロジェクトでの実装

TinyCC 方式を採用し、`define_predefined_macros()` で `-D` オプションを `#define` ディレクティブとして処理:

1. `-D` オプションから `#define NAME VALUE\n` 形式の文字列を生成
2. 仮想ファイルとしてソーススタックにプッシュ
3. 通常のディレクティブ処理ループで `#define` を解析
4. 処理後、仮想ファイルをポップ

## テストコマンド

```bash
cargo run -- -E \
  -I/usr/lib/gcc/x86_64-redhat-linux/15/include \
  -I/usr/lib64/perl5/CORE \
  -I/usr/include \
  -DPERL_CORE \
  -D__GNUC__=14 \
  -D__STDC__=1 \
  -D__x86_64__ \
  '-D__UINTMAX_C(c)=c ## UL' \
  samples/test_op_reg2.c
```
