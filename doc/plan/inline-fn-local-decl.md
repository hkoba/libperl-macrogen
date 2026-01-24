# inline 関数のローカル変数宣言を Rust の let に変換

## 問題

inline 関数内のローカル変数宣言が、`let` 宣言ではなく
コメント `// local decl: DeclSpecs {...}` として出力される。

### 例: Perl_av_remove_offset

```c
// C の定義
SSize_t i = AvARRAY(av) - AvALLOC(av);
```

```rust
// 現在の出力（誤り）
// local decl: DeclSpecs { storage: None, type_specs: [TypedefName(InternedStr(654))], ... }
if i != 0 { ... }

// 期待する出力
let i: SSize_t = (AvARRAY(av) - AvALLOC(av));
if i != 0 { ... }
```

### 例: Perl_newPADxVOP

```c
// C の定義
OP *o = newOP(type, flags);
```

```rust
// 現在の出力（誤り）
// local decl: DeclSpecs { ... }
...
return o;

// 期待する出力
let o: *mut OP = newOP(r#type, flags);
...
return o;
```

## 原因

`compound_stmt_to_string` (line 884-886) で `BlockItem::Decl` を処理する際、
単にコメントを出力しているだけ:

```rust
BlockItem::Decl(decl) => {
    result.push_str(&format!("{}// local decl: {:?}\n", indent, decl.specs));
}
```

## 解決策

`Declaration` 構造体から Rust の `let` 宣言を生成するメソッドを追加。

### Declaration の構造

```rust
pub struct Declaration {
    pub specs: DeclSpecs,          // 型指定子
    pub declarators: Vec<InitDeclarator>,  // 宣言子のリスト
    ...
}

pub struct InitDeclarator {
    pub declarator: Declarator,    // 変数名と派生型
    pub init: Option<Initializer>, // 初期化子
}

pub struct Declarator {
    pub name: Option<InternedStr>, // 変数名
    pub derived: Vec<DerivedDecl>, // ポインタ、配列など
    ...
}
```

### 実装

新しいメソッド `decl_to_rust_let` を追加:

```rust
/// Declaration を Rust の let 宣言に変換
fn decl_to_rust_let(&mut self, decl: &Declaration, indent: &str) -> String {
    let mut result = String::new();

    // 基本型を取得
    let base_type = self.decl_specs_to_rust(&decl.specs);

    // 各宣言子を処理
    for init_decl in &decl.declarators {
        let name = init_decl.declarator.name
            .map(|n| escape_rust_keyword(self.interner.get(n)))
            .unwrap_or_else(|| "_".to_string());

        // 派生型（ポインタなど）を適用
        let ty = self.apply_derived_to_type(&base_type, &init_decl.declarator.derived);

        // 初期化子
        if let Some(ref init) = init_decl.init {
            match init {
                Initializer::Expr(expr) => {
                    let init_expr = self.expr_to_rust_inline(expr);
                    result.push_str(&format!("{}let {}: {} = {};\n", indent, name, ty, init_expr));
                }
                Initializer::List(_) => {
                    // 初期化リストは複雑なので TODO
                    result.push_str(&format!("{}let {}: {} = /* init list */;\n", indent, name, ty));
                }
            }
        } else {
            // 初期化子なし（未初期化変数 - Rust では unsafe かデフォルト値が必要）
            result.push_str(&format!("{}let {}: {}; // uninitialized\n", indent, name, ty));
        }
    }

    result
}
```

### compound_stmt_to_string の修正

```rust
BlockItem::Decl(decl) => {
    result.push_str(&self.decl_to_rust_let(decl, indent));
}
```

## 変更対象ファイル

| ファイル | 変更内容 |
|----------|----------|
| `src/rust_codegen.rs` | `decl_to_rust_let` メソッド追加、`compound_stmt_to_string` で使用 |

## 注意点

1. **複数の宣言子**: `int a, b, c;` のような複数宣言を個別の `let` に分解
2. **ポインタ型**: `OP *o` → `let o: *mut OP`
3. **初期化リスト**: `{...}` 形式は複雑なので一旦 TODO コメント
4. **未初期化変数**: Rust では unsafe か、後で代入する形が必要

## テスト

1. `cargo build` でビルド確認
2. `cargo test` で既存テストが通ることを確認
3. `--gen-rust` で以下を確認:
   - `Perl_av_remove_offset` に `let i: SSize_t = ...;` が生成される
   - `Perl_newPADxVOP` に `let o: *mut OP = ...;` が生成される
