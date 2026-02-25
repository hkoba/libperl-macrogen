# Plan: E0425 未解決シンボル検出 — コード生成時マーキング

## Context

Phase 4+5 完了後（commit `0a6ed42`）の統合ビルドエラー: 1,647。
うち E0425（未解決シンボル）は **211 件**。

### 現状の問題

`GenerateStatus::Success` と判定されたマクロ関数でも、コード生成後の Rust コードに
未解決の識別子が含まれるケースがある。既存の `calls_unavailable` チェックは
`MacroInferInfo.called_functions`（マクロ本体の直接呼び出し関数名）のみを対象としているが、
以下が漏れる:

1. **インライン展開されたサブマクロ内の未知関数** — `should_emit_as_macro_call()` が
   false の場合、展開されたマクロ本体の中の関数呼び出しがチェックされない
2. **`__VA_ARGS__`** (18件) — 可変長マクロの未展開パラメータ
3. **コンテキスト変数** `ax`, `sp`, `stash` 等 (21件) — XSUB ランタイムの変数で
   マクロのパラメータにもない
4. **C標準ライブラリ関数** `strlen`, `memset` 等 — 展開後に現れる呼び出し

### 目的

コード生成時に未解決シンボルを検出し、その関数をコメントとして出力する。
既存の `[CALLS_UNAVAILABLE]` / `[CODEGEN_INCOMPLETE]` と同様のパターン。

---

## 設計方針: コード生成時の識別子チェック

`expr_to_rust` / `expr_to_rust_inline` の `ExprKind::Ident` ハンドラで、
各識別子を「既知シンボル集合」と照合して未解決を検出する。

**利点**: AST のコンテキスト（構造体メンバーアクセスかフリーな識別子か）を正確に区別可能。
出力文字列の正規表現解析よりも偽陽性が少ない。

---

## Step 1: `KnownSymbols` 構築

**ファイル**: `src/rust_codegen.rs`

`CodegenDriver::generate()` で1回構築し、各 `RustCodegen` インスタンスに参照を渡す。

```rust
/// コード生成時に解決可能なシンボルの集合
struct KnownSymbols {
    names: HashSet<String>,
}

impl KnownSymbols {
    fn new(result: &InferResult, interner: &StringInterner) -> Self { ... }
    fn contains(&self, name: &str) -> bool { self.names.contains(name) }
}
```

### ソース一覧

| ソース | 内容 |
|--------|------|
| `RustDeclDict.fns` | bindings.rs の関数名 |
| `RustDeclDict.consts` | bindings.rs の定数名 |
| `RustDeclDict.types` | bindings.rs の型別名 |
| `RustDeclDict.structs` | bindings.rs の構造体名 |
| `RustDeclDict.enums` | bindings.rs の enum 名 |
| `EnumDict.variant_to_enum` | enum バリアント名 |
| `MacroInferContext.macros` (parseable かつ !calls_unavailable) | 関数呼び出しとして保持されるマクロ名 |
| `InlineFnDict` | inline 関数名 |
| `is_function_available` のビルトインリスト | `__builtin_*`, `strlen`, `memcpy` 等 |
| Rust プリミティブ | `true`, `false`, `std`, `crate`, `self`, `super` 等 |

---

## Step 2: `RustCodegen` に検出フィールド追加

**ファイル**: `src/rust_codegen.rs`

```rust
pub struct RustCodegen<'a> {
    // ... 既存フィールド ...

    /// 既知シンボル集合への参照
    known_symbols: &'a KnownSymbols,
    /// 現在の関数のローカルスコープ（パラメータ名 + ローカル変数名）
    current_local_names: HashSet<InternedStr>,
    /// 検出された未解決シンボル名（重複なし、出現順）
    unresolved_names: Vec<String>,
}
```

### `current_local_names` の投入タイミング

- `generate_macro()`: `info.params` の各 `.name` を追加。
  THX 依存なら `"my_perl"` の InternedStr も追加。
- `generate_inline_fn()`: 関数パラメータ名 + 本体の `BlockItem::Decl` で宣言された変数名。

---

## Step 3: `ExprKind::Ident` でのチェック挿入

**ファイル**: `src/rust_codegen.rs`

### `expr_to_rust` (L1077) の変更

```rust
ExprKind::Ident(name) => {
    // lvalue 展開時のパラメータ置換
    if let Some(subst) = self.param_substitutions.get(name) {
        return subst.clone();
    }
    let name_str = self.interner.get(*name);
    // 未解決シンボルチェック
    if !self.current_local_names.contains(name)
        && !self.current_type_param_map.contains_key(name)
        && !self.known_symbols.contains(name_str)
    {
        if !self.unresolved_names.contains(&name_str.to_string()) {
            self.unresolved_names.push(name_str.to_string());
        }
    }
    escape_rust_keyword(name_str)
}
```

### `expr_to_rust_inline` (L2320) の変更

同様のロジック。`current_type_param_map` は inline 関数では空なので無視される。

### チェック不要な箇所（偽陽性の回避）

- `ExprKind::Member` / `ExprKind::PtrMember` の `.member` — 構造体メンバー名
  → `ExprKind::Ident` を経由しないので問題なし（確認済み）
- `ExprKind::Cast` の型名 — `type_name_to_rust()` 経由で別パス
- `enum_dict` のバリアント — `KnownSymbols` に含めるので解決される

---

## Step 4: `GeneratedCode` 拡張と出力制御

**ファイル**: `src/rust_codegen.rs`

### `GeneratedCode` の拡張

```rust
pub struct GeneratedCode {
    pub code: String,
    pub incomplete_count: usize,
    /// 検出された未解決シンボル名
    pub unresolved_names: Vec<String>,
}

impl GeneratedCode {
    pub fn has_unresolved_names(&self) -> bool {
        !self.unresolved_names.is_empty()
    }
}
```

`into_generated_code()` で `self.unresolved_names` を含める。

### `CodegenStats` の拡張

```rust
pub struct CodegenStats {
    // ... 既存フィールド ...
    pub macros_unresolved_names: usize,
    pub inline_fns_unresolved_names: usize,
}
```

### 出力制御 (`CodegenDriver`)

`generate_macros()` と `generate_inline_fns()` で、
`generated.has_unresolved_names()` をチェック。

```rust
// generate_macros(), GenerateStatus::Success のブロック内:
let generated = codegen.generate_macro(info);
if generated.has_unresolved_names() {
    let name_str = self.interner.get(info.name);
    let thx_info = if info.is_thx_dependent { " [THX]" } else { "" };
    writeln!(self.writer, "// [UNRESOLVED_NAMES] {}{} - macro function",
             name_str, thx_info)?;
    writeln!(self.writer, "// Unresolved: {}",
             generated.unresolved_names.join(", "))?;
    for line in generated.code.lines() {
        writeln!(self.writer, "// {}", line)?;
    }
    writeln!(self.writer)?;
    self.stats.macros_unresolved_names += 1;
} else if generated.is_complete() {
    write!(self.writer, "{}", generated.code)?;
    self.stats.macros_success += 1;
} else {
    // 既存の CODEGEN_INCOMPLETE パス
}
```

`generate_inline_fns()` も同様のパターン。

---

## Step 5: `RustCodegen::new` のシグネチャ変更

```rust
pub fn new(
    interner: &'a StringInterner,
    enum_dict: &'a EnumDict,
    macro_ctx: &'a MacroInferContext,
    bindings_info: BindingsInfo,
    known_symbols: &'a KnownSymbols,
) -> Self
```

`CodegenDriver` 側の全呼び出し箇所（`generate_macros` 内 L2838, `generate_inline_fns` 内 L2797）を更新。

`CodegenDriver` に `known_symbols: KnownSymbols` フィールドを追加し、
`generate()` の先頭で構築するか、`new()` で受け取る。

---

## 変更ファイル一覧

| ファイル | 変更内容 |
|----------|----------|
| `src/rust_codegen.rs` | `KnownSymbols` 構造体追加、`RustCodegen` フィールド追加、`ExprKind::Ident` チェック、`GeneratedCode` 拡張、`CodegenStats` 拡張、`CodegenDriver` 出力制御 |

**注**: `src/macro_infer.rs` や `src/semantic.rs` は変更不要。
全ての変更は `src/rust_codegen.rs` のみ。

---

## 検証

1. `cargo build && cargo test` — 全テスト通過
2. 統合ビルド: `~/blob/libperl-rs/12-macrogen-2-build.zsh`
3. `tmp/build-error.log` で E0425 エラー数を確認
4. `tmp/macro_bindings.rs` で `[UNRESOLVED_NAMES]` コメントの内容を目視確認
5. `grep -c UNRESOLVED_NAMES tmp/macro_bindings.rs` で検出関数数を確認

### 期待する結果

- E0425 エラーが大幅に減少（211 → 目標 50 以下）
- `[UNRESOLVED_NAMES]` でマーキングされた関数に未解決シンボル名が表示される
- 既存の正常に生成できていた関数が誤ってコメントアウトされない
