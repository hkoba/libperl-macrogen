# THX マクロ関数呼び出しへの自動 my_perl 引数追加

## 概要

生成された Rust コードで、THX マクロ関数の呼び出し時に `my_perl` 引数が不足している。
本計画では、呼び出し先が THX 依存マクロの場合に、自動で `my_perl` を先頭に追加する。

## 問題の詳細

### 現在の出力（問題あり）

```rust
/// GvAVn [THX] - macro function
#[inline]
pub unsafe fn GvAVn(my_perl: *mut PerlInterpreter, gv: *mut GV) -> *mut AV {
    unsafe {
        (if (((*(0 + ((*gv).sv_u).svu_gp)).gp_av) != 0) {
            (*(0 + ((*gv).sv_u).svu_gp)).gp_av
        } else {
            // ↓ gv_add_by_type は THX マクロだが my_perl が足りない
            (*(0 + ((*gv_add_by_type(gv, SVt_PVAV)).sv_u).svu_gp)).gp_av
        })
    }
}
```

### 期待される出力

```rust
/// GvAVn [THX] - macro function
#[inline]
pub unsafe fn GvAVn(my_perl: *mut PerlInterpreter, gv: *mut GV) -> *mut AV {
    unsafe {
        (if (((*(0 + ((*gv).sv_u).svu_gp)).gp_av) != 0) {
            (*(0 + ((*gv).sv_u).svu_gp)).gp_av
        } else {
            // ↓ my_perl を自動追加
            (*(0 + ((*gv_add_by_type(my_perl, gv, SVt_PVAV)).sv_u).svu_gp)).gp_av
        })
    }
}
```

### 対象外（既に正しく動作）

C 関数（`Perl_*` 系）の呼び出しは既に `my_perl` が正しく渡されている：

```rust
/// sv_2iv [THX] - macro function
#[inline]
pub unsafe fn sv_2iv(my_perl: *mut PerlInterpreter, sv: *mut SV) -> IV {
    unsafe {
        Perl_sv_2iv_flags(my_perl, sv, SV_GMAGIC)  // ← 既に正しい
    }
}
```

## 検出方法

関数呼び出しの際に以下の条件を満たす場合、`my_perl` を先頭に追加する：

1. **呼び出し先が `MacroInferContext.macros` に存在する**
2. **その `MacroInferInfo.is_thx_dependent` が `true`**
3. **実引数が仮引数より1つ少ない**
   - `info.params.len() + 1`（THX マクロは my_perl を含めた数）と `args.len()` を比較

## 設計

### RustCodegen の変更

`RustCodegen` に `MacroInferContext` への参照を追加：

```rust
pub struct RustCodegen<'a> {
    interner: &'a StringInterner,
    enum_dict: &'a EnumDict,
    macro_ctx: &'a MacroInferContext,  // 追加
    buffer: String,
    incomplete_count: usize,
}

impl<'a> RustCodegen<'a> {
    pub fn new(
        interner: &'a StringInterner,
        enum_dict: &'a EnumDict,
        macro_ctx: &'a MacroInferContext,  // 追加
    ) -> Self {
        Self {
            interner,
            enum_dict,
            macro_ctx,
            buffer: String::new(),
            incomplete_count: 0,
        }
    }
}
```

### THX マクロ呼び出し判定

```rust
/// 呼び出し先が THX マクロで、my_perl が不足しているかチェック
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

### 関数呼び出し生成の変更

`expr_to_rust_inline` の `ExprKind::Call` 処理を修正：

```rust
ExprKind::Call { func, args } => {
    // __builtin_expect の処理（既存）
    if let ExprKind::Ident(name) = &func.kind {
        let func_name = self.interner.get(*name);
        if func_name == "__builtin_expect" && args.len() >= 1 {
            return self.expr_to_rust_inline(&args[0]);
        }
    }

    let f = self.expr_to_rust_inline(func);

    // THX マクロで my_perl が不足しているかチェック
    let needs_my_perl = if let ExprKind::Ident(name) = &func.kind {
        self.needs_my_perl_for_call(*name, args.len())
    } else {
        false
    };

    let mut a: Vec<String> = if needs_my_perl {
        vec!["my_perl".to_string()]
    } else {
        vec![]
    };
    a.extend(args.iter().map(|arg| self.expr_to_rust_inline(arg)));

    format!("{}({})", f, a.join(", "))
}
```

### CodegenDriver の変更

同様に `CodegenDriver` にも `macro_ctx` を追加し、`expr_to_rust_inline` を修正する。

## 実装手順

| Step | 内容 |
|------|------|
| 1 | `RustCodegen` に `macro_ctx` フィールドを追加 |
| 2 | `RustCodegen::new` の引数に `macro_ctx` を追加 |
| 3 | `needs_my_perl_for_call` メソッドを追加 |
| 4 | `RustCodegen::expr_to_rust_inline` の `Call` 処理を修正 |
| 5 | `CodegenDriver` にも同様の変更を適用 |
| 6 | `pipeline.rs` の呼び出し箇所を更新 |
| 7 | テストと検証 |

## 変更対象ファイル

| ファイル | 変更内容 |
|----------|----------|
| `src/rust_codegen.rs` | `RustCodegen`/`CodegenDriver` に `macro_ctx` 追加、Call 処理修正 |
| `src/pipeline.rs` | `CodegenDriver::new` への引数追加 |

## テスト方法

結合テストには以下のスクリプトを使用する：

```bash
~/blob/libperl-rs/12-macrogen-2-build.zsh
```

テスト結果の確認：
- **エラーログ**: `tmp/build-error.log`
- **生成コード**: `tmp/macro_bindings.rs`

エラー数の確認：
```bash
# 総エラー数
grep -c "^error\[E" tmp/build-error.log

# 引数の数が合わない（E0061）
grep "^error\[E0061\]" tmp/build-error.log | wc -l
```

特定の関数の生成結果を確認：
```bash
# gv_add_by_type の呼び出し箇所を確認
grep -B2 -A2 "gv_add_by_type" tmp/macro_bindings.rs

# GvAVn の生成結果を確認
grep -A10 "pub unsafe fn GvAVn" tmp/macro_bindings.rs
```

## 注意事項

- C 関数（`Perl_*` 系）の呼び出しは対象外（既に正しく動作）
- `MacroInferContext.macros` に存在しない関数は対象外
- 可変長引数マクロは引数数の比較ができないため、この方法では対応できない

## 想定される効果

- E0061（argument count mismatch）エラーの削減
- THX マクロ間の呼び出しで `my_perl` が自動補完される
