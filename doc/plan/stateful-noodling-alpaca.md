# Plan: `BuiltinCall` AST ノード追加による `offsetof(type, member)` パースの汎用化

## Context

マクロ `RCPVx` 等 6 件が PARSE_FAILED となる。原因は展開後の式に含まれる
`offsetof(struct rcpv, pv)` をパーサが解析できないため。

```c
// cop.h:585
#define RCPVx(pv_arg)  ((RCPV *)((pv_arg) - STRUCT_OFFSET(struct rcpv, pv)))
// STRUCT_OFFSET(s,m) → offsetof(s,m)     (perl.h:1694)
// offsetof(s,m) → __builtin_offsetof(s,m) (stddef.h)
```

パーサは `offsetof` を通常の関数呼び出しとして処理し、第1引数を
`parse_assignment_expr()` で解析。`struct` キーワードは primary expression の
開始にならないため失敗する（6 マクロが影響）。

### 同じパターンの C ビルトイン

型名を引数に取る関数呼び出し風構文は `offsetof` だけではない:

| ビルトイン | 引数パターン | 現状 |
|-----------|------------|------|
| `offsetof(type, member)` | 型名, 式 | **PARSE_FAILED** (6件) |
| `__builtin_offsetof(type, member)` | 型名, 式 | 同上 |
| `__builtin_types_compatible_p(type1, type2)` | 型名, 型名 | KnownSymbols 登録済みだが未パース |
| `__builtin_va_arg(ap, type)` | 式, 型名 | 同上 |

これらに if 文を個別に追加するのではなく、AST に汎用的な
`BuiltinCall` ノードを導入し、引数に型名と式を混在できるようにする。

## 設計

### 1. AST の拡張 (`src/ast.rs`)

```rust
/// ビルトイン呼び出しの引数（型名 or 式）
#[derive(Debug, Clone)]
pub enum BuiltinArg {
    Expr(Box<Expr>),
    TypeName(Box<TypeName>),
}
```

```rust
pub enum ExprKind {
    // ... 既存 ...

    /// ビルトイン関数呼び出し（引数に型名を含みうる）
    /// offsetof(type, member), __builtin_types_compatible_p(type1, type2) 等
    BuiltinCall {
        name: InternedStr,
        args: Vec<BuiltinArg>,
    },
}
```

### 2. パーサの拡張 (`src/parser.rs`)

#### 2a. ビルトイン登録テーブル

パーサに known builtins のセットを持たせる:

```rust
/// 引数に型名を取りうるビルトイン関数名の集合
fn is_type_arg_builtin(&self, name: InternedStr) -> bool {
    let s = self.source.interner().get(name);
    matches!(s, "offsetof" | "__builtin_offsetof"
              | "__builtin_types_compatible_p"
              | "__builtin_va_arg")
}
```

#### 2b. `parse_postfix_expr()` の関数呼び出し分岐を拡張

`LParen` 分岐 (L1961) で、`expr` が `Ident(name)` かつ
`is_type_arg_builtin(name)` の場合に `BuiltinCall` パースに分岐:

```rust
TokenKind::LParen => {
    // ビルトイン呼び出し判定
    if let ExprKind::Ident(name) = &expr.kind {
        if self.is_type_arg_builtin(*name) {
            self.advance()?; // (
            let builtin_name = *name;
            let args = self.parse_builtin_args()?;
            self.expect(&TokenKind::RParen)?;
            self.function_call_count += 1;
            expr = Expr::new(
                ExprKind::BuiltinCall { name: builtin_name, args },
                loc,
            );
            continue;  // postfix ループ継続
        }
    }
    // 通常の関数呼び出し（既存コード）
    self.advance()?;
    // ...
}
```

#### 2c. `parse_builtin_args()` — 引数ごとに型/式を自動判定

```rust
fn parse_builtin_args(&mut self) -> Result<Vec<BuiltinArg>> {
    let mut args = Vec::new();
    if !self.check(&TokenKind::RParen) {
        loop {
            if self.is_type_start() {
                // 型名として解析
                let type_name = self.parse_type_name()?;
                args.push(BuiltinArg::TypeName(Box::new(type_name)));
            } else {
                // 式として解析
                let expr = self.parse_assignment_expr()?;
                args.push(BuiltinArg::Expr(Box::new(expr)));
            }
            if !self.check(&TokenKind::Comma) {
                break;
            }
            self.advance()?;
        }
    }
    Ok(args)
}
```

`is_type_start()` は既存メソッド (L2509) で、`KwStruct`, `KwEnum`, `KwUnion`,
型名キーワード、typedef 名等を判定する。これにより引数ごとに型名 or 式を
自動判定でき、ビルトインごとの引数パターンをハードコードする必要がない。

### 3. Codegen の更新 (`src/rust_codegen.rs`)

`expr_to_rust` / `expr_to_rust_inline` に `ExprKind::BuiltinCall` の
ハンドラを追加。既存の `ExprKind::Call` 内にある `offsetof` 特別処理を移行:

```rust
ExprKind::BuiltinCall { name, args } => {
    let func_name = self.interner.get(*name);

    // offsetof(type, field) → std::mem::offset_of!(Type, field_path)
    if func_name == "offsetof" || func_name == "__builtin_offsetof" {
        if args.len() == 2 {
            let type_str = match &args[0] {
                BuiltinArg::TypeName(tn) => self.type_name_to_rust(tn, info),
                BuiltinArg::Expr(e) => self.expr_to_rust(e, info),
            };
            let field_path = match &args[1] {
                BuiltinArg::Expr(e) => self.expr_to_field_path(e),
                _ => None,
            };
            if let Some(fp) = field_path {
                return format!("std::mem::offset_of!({}, {})", type_str, fp);
            }
        }
    }

    // __builtin_types_compatible_p(type1, type2) → 0 or 1
    if func_name == "__builtin_types_compatible_p" { ... }

    // __builtin_va_arg(ap, type) → ...
    // etc.

    // フォールバック: 通常の関数呼び出しとして出力
    ...
}
```

### 4. S式出力・型推論への影響

- `src/sexp.rs`: `BuiltinCall` の S式出力を追加（`(builtin-call name arg...)`）
- `src/macro_infer.rs`: `BuiltinCall` の `called_functions` 抽出に対応
  （`BuiltinArg::Expr` 内の関数呼び出しを再帰的に走査）
- `src/semantic.rs`: 必要に応じて `BuiltinCall` のトラバースを追加

### 変更ファイル一覧

| ファイル | 変更内容 |
|----------|----------|
| `src/ast.rs` | `BuiltinArg` enum、`ExprKind::BuiltinCall` variant 追加 |
| `src/parser.rs` | `is_type_arg_builtin()`、`parse_builtin_args()`、`parse_postfix_expr()` 分岐 |
| `src/rust_codegen.rs` | `BuiltinCall` ハンドラ追加、既存の `Call` 内 offsetof 処理を移行 |
| `src/sexp.rs` | `BuiltinCall` の S式出力 |
| `src/macro_infer.rs` | `BuiltinCall` の関数呼び出し走査対応 |

## 検証

```bash
# 1. 全テスト通過
cargo test

# 2. KwStruct パースエラーが 0 件になること
cargo run -- --auto --gen-rust samples/xs-wrapper.h --bindings samples/bindings.rs 2>/dev/null \
  | grep 'found KwStruct'

# 3. RCPVx が正常生成されること
cargo run -- --auto --gen-rust samples/xs-wrapper.h --bindings samples/bindings.rs 2>/dev/null \
  | grep -B1 -A5 'fn RCPVx'

# 4. stats 確認 (parse failed 54→48 を期待)
cargo run -- --auto --gen-rust samples/xs-wrapper.h --bindings samples/bindings.rs 2>&1 | tail -3

# 5. 統合ビルドテスト
~/blob/libperl-rs/12-macrogen-2-build.zsh
grep -c 'error\[E0425\]' tmp/build-error.log
grep 'fn RCPVx' tmp/macro_bindings.rs
```
