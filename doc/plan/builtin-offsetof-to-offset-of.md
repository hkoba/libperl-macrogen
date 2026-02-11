# Plan: `offsetof` / `__builtin_offsetof` → `std::mem::offset_of!` 変換

## 目標

C の `offsetof(type, field_expr)` / `__builtin_offsetof(type, field_expr)` 呼び出しを
Rust の `std::mem::offset_of!(Type, field_path)` マクロ呼び出しとして生成する。

## 背景

### 問題

`STRUCT_OFFSET(XPVNV, xnv_u.xnv_nv)` は `offsetof(XPVNV, xnv_u.xnv_nv)` に展開される。
しかし `offsetof` は C 標準ライブラリマクロ（`<stddef.h>`）であり、Rust には存在しない。
現在 `offsetof` は未知の関数として扱われ、`CALLS_UNAVAILABLE` が伝播する。

```
STRUCT_OFFSET(s, m) → offsetof(s, m) → 利用不可
  ↑ SET_SVANY_FOR_BODYLESS_NV/IV が呼び出す
    ↑ Perl_newSV_type が呼び出す（インライン関数）
```

結果として `Perl_newSV_type` は正常に生成されるが、その中の `__builtin_offsetof` 呼び出しが
Rust として不正なコードのまま出力されている:

```rust
// 現在の出力（不正）
(*sv_).sv_any = ((((&mut ((*sv_).sv_u).svu_iv) as *mut c_char)
    - __builtin_offsetof(XPVIV, (xiv_u).xivu_iv))
    as *mut XPVIV);
```

### マクロ展開チェーン

```
SET_SVANY_FOR_BODYLESS_IV(sv):
  SvANY(sv_) = (XPVIV*)((char*)&(sv_->sv_u.svu_iv) - STRUCT_OFFSET(XPVIV, xiv_iv))

STRUCT_OFFSET(s, m) → offsetof(s, m)
offsetof → __builtin_offsetof（GCC/stddef.h）

xiv_iv は Object macro: #define xiv_iv xiv_u.xivu_iv
```

展開後: `offsetof(XPVIV, xiv_u.xivu_iv)`

### AST 表現

```
(call
  (ident offsetof)          ← ExprKind::Call, func = Ident("offsetof")
  (ident XPVNV)             ← 第1引数: 型名（Ident として解析）
  (member
    (ident xnv_u) xnv_nv))  ← 第2引数: フィールドパス（Member 式）
```

- 第1引数の型名は `ExprKind::Ident` として解析される（型名と変数名は構文的に同一）
- 第2引数のフィールドパスは `ExprKind::Member` の連鎖として解析される
  - 単純フィールド: `xiv_iv` → `Ident("xiv_iv")`（ただし実際にはマクロ展開で compound になる）
  - 複合フィールド: `xnv_u.xnv_nv` → `Member(Ident("xnn_u"), "xnv_nv")`

### TinyCC のアプローチ

TinyCC は `__builtin_offsetof` を **プリプロセッサマクロ** として定義する:

```c
// tinycc/include/tccdefs.h:172
#define __builtin_offsetof(type, field) ((__SIZE_TYPE__)&((type*)0)->field)
```

パーサや AST に専用ノードは持たず、`((size_t)&((TYPE*)0)->field)` に展開して
通常の式として処理する。

### 本プロジェクトでのアプローチ

TinyCC のマクロ展開アプローチは `(TYPE*)0` を経由するため、Rust のコード生成には不適切。
代わりに **コード生成レベル** で `offsetof` / `__builtin_offsetof` を認識し、
Rust の `std::mem::offset_of!` マクロに変換する。

### 期待される出力

```rust
// 変換前（不正な Rust）
__builtin_offsetof(XPVIV, (xiv_u).xivu_iv)

// 変換後
std::mem::offset_of!(XPVIV, xiv_u.xivu_iv)
```

## 実装

### Phase 1: コード生成

**ファイル**: `src/rust_codegen.rs`

#### 1a. フィールドパス変換ヘルパーの追加

`ExprKind::Member` / `ExprKind::Ident` の連鎖をドット区切りのフィールドパス文字列に変換する。

```rust
/// offsetof のフィールドパス式をドット区切り文字列に変換
/// Ident("xnv_u") → "xnv_u"
/// Member(Ident("xnv_u"), "xnv_nv") → "xnv_u.xnv_nv"
fn expr_to_field_path(&self, expr: &Expr) -> Option<String> {
    match &expr.kind {
        ExprKind::Ident(name) => {
            Some(self.interner.get(*name).to_string())
        }
        ExprKind::Member { expr: base, member } => {
            let base_path = self.expr_to_field_path(base)?;
            let member_name = self.interner.get(*member);
            Some(format!("{}.{}", base_path, member_name))
        }
        _ => None,
    }
}
```

#### 1b. `expr_to_rust` の Call ハンドラに分岐追加

`__builtin_expect` と同様のパターンで、`offsetof` / `__builtin_offsetof` を検出:

```rust
ExprKind::Call { func, args } => {
    if let ExprKind::Ident(name) = &func.kind {
        let func_name = self.interner.get(*name);

        // __builtin_expect(cond, expected) -> cond
        if func_name == "__builtin_expect" && args.len() >= 1 {
            return self.expr_to_rust(&args[0], info);
        }

        // offsetof(type, field) → std::mem::offset_of!(Type, field_path)
        if (func_name == "offsetof" || func_name == "__builtin_offsetof")
            && args.len() == 2
        {
            let type_name = self.expr_to_rust(&args[0], info);
            if let Some(field_path) = self.expr_to_field_path(&args[1]) {
                return format!("std::mem::offset_of!({}, {})", type_name, field_path);
            }
        }
    }
    // ... 既存の一般的な Call 処理
}
```

#### 1c. `expr_to_rust_inline` にも同様の分岐追加

`expr_to_rust_inline` の `ExprKind::Call` ハンドラにも同じロジックを追加。

### Phase 2: 利用可能性の登録

**ファイル**: `src/macro_infer.rs`, `src/rust_codegen.rs`

#### 2a. `offsetof` を builtin_fns に追加

現在 `__builtin_offsetof` は builtin_fns に含まれるが、`offsetof` は含まれない。
`STRUCT_OFFSET` マクロは `offsetof(s, m)` に展開されるため、`offsetof` も追加する。

```rust
// macro_infer.rs: check_function_availability
let builtin_fns: std::collections::HashSet<&str> = [
    "__builtin_expect",
    "__builtin_offsetof",
    "offsetof",           // ← 追加
    // ...
].into_iter().collect();

// rust_codegen.rs: function_is_known
let builtin_fns = [
    "__builtin_expect",
    "__builtin_offsetof",
    "offsetof",           // ← 追加
    // ...
];
```

これにより `STRUCT_OFFSET` → `SET_SVANY_FOR_BODYLESS_*` → `Perl_newSV_type` の
`CALLS_UNAVAILABLE` 伝播が解消される。

### Phase 3: テストと検証

1. **ビルド確認**: `cargo build`

2. **既存テスト**: `cargo test`

3. **出力確認**:
   ```bash
   cargo run -- --auto --gen-rust samples/xs-wrapper.h --bindings samples/bindings.rs 2>/dev/null \
     | grep -E 'offset_of|STRUCT_OFFSET|SET_SVANY'
   ```
   - `STRUCT_OFFSET` が `CALLS_UNAVAILABLE` でないこと
   - `offset_of!` が正しく生成されること

4. **回帰テスト**: `cargo test rust_codegen_regression`

## フィールドパス式の詳細

### パターンと変換

| C 式 | AST | Rust 出力 |
|------|-----|-----------|
| `xiv_iv` | `Ident("xiv_iv")` | `xiv_iv` |
| `xnv_u.xnv_nv` | `Member(Ident("xnv_u"), "xnv_nv")` | `xnv_u.xnv_nv` |
| `a.b.c` | `Member(Member(Ident("a"), "b"), "c")` | `a.b.c` |

### Rust `offset_of!` のネストフィールド対応

Rust 1.82 以降、`std::mem::offset_of!` はネストしたフィールドアクセスをサポート:

```rust
std::mem::offset_of!(XPVNV, xnv_u.xnv_nv)  // OK (Rust 1.82+)
```

## 変更ファイル一覧

| ファイル | 変更内容 |
|----------|----------|
| `src/rust_codegen.rs` | `expr_to_field_path` 追加、`expr_to_rust` / `expr_to_rust_inline` の Call 分岐追加 |
| `src/macro_infer.rs` | `offsetof` を `builtin_fns` に追加 |
| `src/rust_codegen.rs` | `offsetof` を `function_is_known` の `builtin_fns` に追加 |

## エッジケース

1. **型名が struct 修飾付き**: `offsetof(struct foo, bar)`
   - 現在の Perl ヘッダでは typedef 名のみ使用（XPVNV, XPVIV 等）
   - `struct foo` の場合は通常の `expr_to_rust` が処理（対応不要）

2. **フィールドパスが式ではない場合**: `expr_to_field_path` が `None` を返す
   - フォールバックとして通常の関数呼び出し生成に戻る

3. **配列添字付きフィールド**: `offsetof(struct s, arr[0].field)`
   - 現在の使用例には含まれない
   - 必要になった場合は `expr_to_field_path` を拡張
