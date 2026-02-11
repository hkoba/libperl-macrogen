# SvPVx の生成コードで一時変数 `_sv` の宣言・代入が欠落する問題

## 症状

`SvPVx` の生成コードで、一時変数 `_sv` が宣言されずに使用されている。

```rust
// 現在の出力
pub unsafe fn SvPVx(my_perl: *mut PerlInterpreter, sv: *mut SV, len: STRLEN) -> *mut c_char {
    unsafe { SvPV(my_perl, _sv, len) }  // _sv は未宣言
}
```

期待される出力:
```rust
pub unsafe fn SvPVx(my_perl: *mut PerlInterpreter, sv: *mut SV, len: STRLEN) -> *mut c_char {
    unsafe {
        let _sv: *mut SV = sv;
        SvPV(my_perl, _sv, len)
    }
}
```

同じ問題が `SvPVx_const`, `SvPVx_nolen` にも存在する。

## マクロ定義

```c
// sv.h:2058
#define SvPVx(sv, len) ({SV *_sv = (sv); SvPV(_sv, len); })
```

GCC 拡張の文式 (statement expression) で、一時変数 `_sv` を宣言し初期化してから
`SvPV` を呼び出す。

## AST 構造

```
SvPVx:
  (stmt-expr (compound-stmt
    (declaration                          ← BlockItem::Decl
      (decl-specs (typedef-name SV))
      (init-declarator
        (declarator _sv (pointer))
        (init (ident sv))))
    (expr-stmt                            ← BlockItem::Stmt
      (call (ident SvPV)
        (ident _sv) (ident len)))))
```

2つの BlockItem:
1. `Declaration`: `SV *_sv = (sv)` — 一時変数の宣言と初期化
2. `Stmt::Expr`: `SvPV(_sv, len)` — 一時変数を使った関数呼び出し

## 原因

`rust_codegen.rs` の `ExprKind::StmtExpr` ハンドラ（2箇所: line 754, line 1681）が、
`BlockItem::Decl` を **無条件にスキップ** している。

```rust
// rust_codegen.rs:773-774 (expr_to_rust)
BlockItem::Decl(_) => {
    // 宣言はスキップ
}

// rust_codegen.rs:1700-1701 (expr_to_rust_inline)
BlockItem::Decl(_) => {
    // 宣言はスキップ（MUTABLE_PTR パターン以外では無視）
}
```

### 処理フロー

1. `detect_mutable_ptr_pattern` でパターンマッチを試みる
   - SvPVx は `({ type *p = expr; p; })` パターンに**該当しない**
     （最後の式が単純な識別子 `p` ではなく関数呼び出し `SvPV(_sv, len)` のため）
2. フォールバック先の一般ハンドラで宣言がスキップされる
3. 結果として `SvPV(my_perl, _sv, len)` のみ出力され、`_sv` が未宣言

### 既存の宣言処理機能

`decl_to_rust_let` メソッド（line 1147）は既に宣言を `let` 文に変換する機能を持ち、
`compound_stmt_to_string`（インライン関数本体の生成）で使用されている。
しかし `StmtExpr` ハンドラからは呼ばれていない。

## 影響範囲

文式内でローカル変数を宣言し、後続の式で使用するマクロすべてに影響する。

| マクロ | パターン | 影響 |
|--------|----------|------|
| `SvPVx(sv, len)` | `({SV *_sv = (sv); SvPV(_sv, len);})` | `_sv` 未宣言 |
| `SvPVx_const(sv, len)` | 同様 | `_sv` 未宣言 |
| `SvPVx_nolen(sv)` | 同様 | `_sv` 未宣言 |
| `SvPVx_force(sv, len)` | `sv_pvn_force` を直接呼ぶため影響なし | — |

## 修正方針

`StmtExpr` ハンドラの `BlockItem::Decl` 処理で、`decl_to_rust_let` を呼び出して
`let` 文を生成する。2箇所 (`expr_to_rust`, `expr_to_rust_inline`) で同様の修正が必要。

## 関連ファイル

- `src/rust_codegen.rs:754-787` — `expr_to_rust` 内の `StmtExpr` ハンドラ
- `src/rust_codegen.rs:1681-1714` — `expr_to_rust_inline` 内の `StmtExpr` ハンドラ
- `src/rust_codegen.rs:545-585` — `detect_mutable_ptr_pattern`
- `src/rust_codegen.rs:1147-1181` — `decl_to_rust_let`（既存の宣言→let 変換）
