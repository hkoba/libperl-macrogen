# マクロ関数パラメータ型推論問題の調査レポート

## 問題

マクロ関数 `CopLABEL` と `HvFILL` の引数の型が `()` になっている。

### 現象

```rust
/// CopLABEL [THX] - macro function
#[inline]
pub unsafe fn CopLABEL(my_perl: *mut PerlInterpreter, c: ()) -> *const c_char {
    unsafe { Perl_cop_fetch_label(my_perl, c, (0 as *mut c_void), (0 as *mut c_void)) }
}

/// HvFILL [THX] - macro function
#[inline]
pub unsafe fn HvFILL(my_perl: *mut PerlInterpreter, hv: ()) -> STRLEN {
    unsafe { Perl_hv_fill(my_perl, MUTABLE_HV(hv)) }
}
```

### 期待される出力

`Perl_cop_fetch_label` と `Perl_hv_fill` の型宣言から、引数の型が導出されるべき:

```c
// C ヘッダー
PERL_CALLCONV const char *
Perl_cop_fetch_label(pTHX_ COP * const cop, STRLEN *len, U32 *flags);

PERL_CALLCONV STRLEN
Perl_hv_fill(pTHX_ HV * const hv);
```

```rust
// bindings.rs
pub fn Perl_cop_fetch_label(
    my_perl: *mut PerlInterpreter,
    cop: *mut COP,        // ← c の型はこれであるべき
    len: *mut STRLEN,
    flags: *mut U32,
) -> *const ::std::os::raw::c_char;

pub fn Perl_hv_fill(
    my_perl: *mut PerlInterpreter,
    hv: *mut HV,          // ← hv の型はこれであるべき
) -> STRLEN;
```

## 根本原因

2つの問題が複合している。

### 問題1: `parse_c_type_string` が `"TYPE* const"` パターンをパースできない

`type_repr.rs:717-755` の `parse_c_type_string` 関数が `"COP* const"` のような形式を正しく処理できない。

#### 処理の流れ

```
入力: "COP* const"

1. 末尾の * をカウント (727-730行)
   → 末尾が "const" なので ptr_count = 0

2. " const" を除去 (737-740行)
   → base = "COP*"

3. "COP*" を基本型としてパース (743行)
   → * が含まれるので失敗 → Void
```

#### 結果

- **期待**: `CType { specs: TypedefName(COP), derived: [Pointer] }`
- **実際**: `CType { specs: Void, derived: [] }`

#### 問題のコード

```rust
fn parse_c_type_string(s: &str, interner: &StringInterner) -> (CTypeSpecs, Vec<CDerivedType>) {
    let s = s.trim();

    // ポインタ数をカウント
    let mut ptr_count = 0;
    let mut is_const = false;
    let mut base = s;

    // 末尾の * をカウント
    while base.ends_with('*') {
        ptr_count += 1;
        base = base[..base.len() - 1].trim();
    }

    // "const" をチェック
    if base.starts_with("const ") {
        is_const = true;
        base = base[6..].trim();
    }
    if base.ends_with(" const") {
        is_const = true;
        base = base[..base.len() - 6].trim();
    }
    // ← ここで再度 * をチェックすべき！

    // 基本型をパース
    let specs = Self::parse_c_base_type(base, interner);
    // ...
}
```

### 問題2: 制約の優先順位

`rust_codegen.rs:485` の `get_param_type` は `constraints.first()` を使用するが、制約の順序は:

1. `void (symbol lookup)` ← 問題1により誤った型
2. `*mut COP (arg 1 of Perl_cop_fetch_label())` ← 正しい型

関数呼び出しからの正しい型制約は存在するが、先に追加された誤った制約が使われる。

## データの流れ

```
apidoc: "COP *const cop"
    ↓
parse_type_from_string → Type::PointerTo(COP)
    ↓
define_symbol(c, Type::PointerTo(COP))
    ↓
lookup_symbol(c) → sym.ty.display() → "COP* const"
    ↓
TypeRepr::from_apidoc_string("COP* const") → Void  ← ここで失敗
    ↓
SymbolLookup { resolved_type: Void }
    ↓
constraints.first() → void
    ↓
コード生成: c: ()
```

## デバッグ出力の例

### CopLABEL

```
[DEBUG register_macro_params_from_apidoc] macro=CopLABEL
  apidoc entry found, args: [ApidocArg { ty: "COP *const", name: "cop" }]
    param[0] c: apidoc_ty=COP *const
      parsed and registered: "COP* const"

[DEBUG Ident] name=c, sym.ty.display=COP* const
  resolved TypeRepr: CType { specs: Void, derived: [], source: Apidoc { raw: "COP* const" } }

[type_env after collect_expr_constraints]
    expr_id=ExprId(21805): void (symbol lookup)           ← 誤った型
    expr_id=ExprId(21805): *mut COP (arg 1 of Perl_cop_fetch_label())  ← 正しい型
```

### HvFILL

```
[DEBUG register_macro_params_from_apidoc] macro=HvFILL
  apidoc entry found, args: [ApidocArg { ty: "HV *const", name: "hv" }]
    param[0] hv: apidoc_ty=HV *const
      parsed and registered: "HV* const"

[DEBUG Ident] name=hv, sym.ty.display=HV* const
  resolved TypeRepr: CType { specs: Void, derived: [], source: Apidoc { raw: "HV* const" } }

[type_env after collect_expr_constraints]
    expr_id=ExprId(54139): void (symbol lookup)           ← 誤った型
    expr_id=ExprId(54139): HV * (arg 0 (p) of MUTABLE_HV())  ← 正しい型
```

## 修正方針

### 方針A: `parse_c_type_string` の修正

`const` を除去した後に再度ポインタをチェックするロジックを追加。

```rust
fn parse_c_type_string(s: &str, interner: &StringInterner) -> (CTypeSpecs, Vec<CDerivedType>) {
    let s = s.trim();
    let mut ptr_count = 0;
    let mut is_const = false;
    let mut base = s;

    loop {
        // 末尾の * をカウント
        while base.ends_with('*') {
            ptr_count += 1;
            base = base[..base.len() - 1].trim();
        }

        // "const" をチェック
        let had_const = if base.starts_with("const ") {
            base = base[6..].trim();
            true
        } else if base.ends_with(" const") {
            base = base[..base.len() - 6].trim();
            true
        } else {
            false
        };

        if had_const {
            is_const = true;
            // const を除去したので、再度 * をチェック
            continue;
        }

        break;
    }

    // 基本型をパース
    let specs = Self::parse_c_base_type(base, interner);
    // ...
}
```

### 方針B: 制約選択の改善

`void (symbol lookup)` より関数引数由来の型制約を優先する。

```rust
fn get_param_type(&mut self, param: &MacroParam, info: &MacroInferInfo, param_index: usize) -> String {
    if let Some(expr_ids) = info.type_env.param_to_exprs.get(&param_name) {
        for expr_id in expr_ids {
            if let Some(constraints) = info.type_env.expr_constraints.get(expr_id) {
                // void 以外の最初の制約を探す
                for c in constraints {
                    let ty_str = self.type_repr_to_rust(&c.ty);
                    if ty_str != "()" {
                        return ty_str;
                    }
                }
            }
        }
    }
    // ...
}
```

### 推奨

方針A（`parse_c_type_string` の修正）を推奨。根本原因を修正することで、他の箇所でも同様の問題が発生しなくなる。

## 関連ファイル

| ファイル | 役割 |
|----------|------|
| `src/type_repr.rs:717-755` | `parse_c_type_string` - 問題の原因 |
| `src/semantic.rs:1190-1212` | `ExprKind::Ident` 処理 - SymbolLookup 制約の追加 |
| `src/rust_codegen.rs:473-501` | `get_param_type` - 制約からパラメータ型を取得 |
| `src/macro_infer.rs:635-724` | `infer_macro_types` - 型推論のエントリポイント |

## 調査日

2026-02-07
