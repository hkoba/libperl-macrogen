# Plan: libc use 文の動的生成

## Context

`macro_bindings.rs` で `strcmp`, `strlen` 等の libc 関数を使う関数が生成される。
libperl-sys 側に `libc` crate を追加済みだが、use 文が `macro_bindings.rs` に
含まれていないため E0425 になる。

use 文はハードコードではなく、**実際に生成されたコードで使われている libc 関数のみ**を
含むように動的に決定したい。

## 課題

`CodegenDriver::generate()` は use 文をファイル先頭に出力した後に
関数を生成するため、生成時点ではどの libc 関数が使われるか不明。

## 設計: 末尾出力 + rustfmt 依存

生成済みコードは rustfmt を通過する（`main.rs` L602）。
Rust の `use` 文はモジュール内のどこに書いても有効で、
rustfmt が自動的に先頭に移動する。

→ **全関数の生成後に use 文を末尾に出力** し、rustfmt に整列を任せる。

---

## 実装

### Step 1: libc 関数テーブルの定義

**ファイル**: `src/rust_codegen.rs`

```rust
/// libc crate から提供される関数名のリスト
/// codegen がそのまま関数呼び出しとして出力する関数のみ
const LIBC_FUNCTIONS: &[&str] = &[
    "strcmp", "strlen", "strncmp", "strcpy", "strncpy",
    "memset", "memchr", "memcpy", "memmove",
];
```

注: `__builtin_expect` 等の codegen 変換済み関数は含めない。
`pthread_*` や `getenv` は現状 UNRESOLVED_NAMES で検出されるため不要
（UNRESOLVED_NAMES 側で既に抑制済み）。

### Step 2: RustCodegen に使用済み libc 関数の追跡を追加

**ファイル**: `src/rust_codegen.rs`

`RustCodegen` に `used_libc_fns: HashSet<String>` フィールドを追加。

`ExprKind::Ident` ハンドラ（`expr_to_rust`, `expr_to_rust_inline` の両方）で、
識別子が `LIBC_FUNCTIONS` に含まれる場合に記録:

```rust
ExprKind::Ident(name) => {
    // ... 既存のパラメータ置換・未解決チェック ...
    let name_str = self.interner.get(*name);
    // libc 関数の使用を記録
    if LIBC_FUNCTIONS.contains(&name_str) {
        self.used_libc_fns.insert(name_str.to_string());
    }
    escape_rust_keyword(name_str)
}
```

### Step 3: GeneratedCode 経由で CodegenDriver に伝播

**ファイル**: `src/rust_codegen.rs`

`GeneratedCode` に `used_libc_fns: HashSet<String>` を追加。
`into_generated_code()` で含める。

`CodegenDriver` に `used_libc_fns: HashSet<String>` フィールドを追加。

`generate_macros()` / `generate_inline_fns()` で、
**正常に出力された関数のみ**（コメントアウトされなかった関数）の
`used_libc_fns` を `CodegenDriver` 側にマージ:

```rust
if !generated.has_unresolved_names() && generated.is_complete() {
    write!(self.writer, "{}", generated.code)?;
    self.used_libc_fns.extend(generated.used_libc_fns.iter().cloned());
    self.stats.macros_success += 1;
}
```

### Step 4: generate() 末尾で use libc 文を出力

**ファイル**: `src/rust_codegen.rs`

`CodegenDriver::generate()` の末尾で、使用済み libc 関数があれば出力:

```rust
pub fn generate(&mut self, result: &InferResult) -> io::Result<()> {
    // ... 既存のヘッダー、use 文、enum import、関数生成 ...

    // 使用された libc 関数の use 文を出力（rustfmt が先頭に移動）
    if !self.used_libc_fns.is_empty() {
        let mut fns: Vec<_> = self.used_libc_fns.iter().cloned().collect();
        fns.sort();
        writeln!(self.writer, "use libc::{{{}}};", fns.join(", "))?;
    }

    Ok(())
}
```

### Step 5: KnownSymbols のビルトインリスト整合

**ファイル**: `src/rust_codegen.rs`

`KnownSymbols::new()` のビルトインリストに libc 関数が含まれていることを確認。
現状既に含まれているので変更不要だが、`LIBC_FUNCTIONS` 定数を参照する形に統一する。

---

## 変更ファイル一覧

| ファイル | 変更内容 |
|----------|----------|
| `src/rust_codegen.rs` | `LIBC_FUNCTIONS` 定数、`RustCodegen` フィールド追加、`ExprKind::Ident` で記録、`GeneratedCode` 拡張、`CodegenDriver` 蓄積・出力 |

`src/main.rs` は変更不要（既存の rustfmt パスがそのまま使える）。

---

## 検証

1. `cargo build && cargo test` — 全テスト通過
2. `cargo run -- --auto --gen-rust samples/xs-wrapper.h --bindings samples/bindings.rs > /dev/null`
   → stderr の stats 確認
3. 出力の先頭付近に `use libc::{...}` が含まれることを確認
4. 統合ビルドで `strcmp` 等の E0425 が消えることを確認
