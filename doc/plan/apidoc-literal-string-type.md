# Plan: apidoc `"literal string"` 引数の `&str` 型マッピング

## 目標

apidoc（`=for apidoc` / `embed.fnc`）の引数仕様に `"..."` 形式が現れた場合、
それを Rust コード生成で `&str` 型として扱う。

## 背景

### 問題

`newSVpvs` の apidoc は以下の通り:

```
=for apidoc Ama|SV*|newSVpvs|"literal string"
```

`"literal string"` は C の文字列リテラルを要求する引数を表す。
現在は `ApidocArg::split_type_and_name()` で `ty = "\"literal string\""`, `name = ""`
として格納されるが、型推論には使われず、パラメータは `c_int` と推論されてしまう:

```rust
// 現在の出力（不正）
pub unsafe fn newSVpvs(my_perl: *mut PerlInterpreter, str: c_int) -> *mut SV {
```

### 期待される出力

```rust
pub unsafe fn newSVpvs(my_perl: *mut PerlInterpreter, str: &str) -> *mut SV {
```

### 影響するマクロ

`"..."` 引数は多数のマクロで使用されている:

```
newSVpvs("literal string")
newSVpvs_flags("literal string", U32 flags)
newSVpvs_share("literal string")
sv_catpvs(SV* sv, "literal string")
sv_catpvs_flags(SV* sv, "literal string", I32 flags)
savepvs("literal string")
gv_stashpvs("name", I32 create)
hv_fetchs(HV* tb, "key", I32 lval)
STR_WITH_LEN("literal string")
memEQs(char* s1, STRLEN l1, "s2")
memCHRs("list", char c)
deprecate(U32 category, "message")
...
```

### 現在のデータフロー

```
apidoc: "literal string"
  ↓ ApidocArg::split_type_and_name()
  ↓ s.starts_with('"') → ty = "\"literal string\"", name = ""
  ↓
  ↓ semantic.rs: register_macro_params_from_apidoc()
  ↓ parse_type_from_string("\"literal string\"") → Err (パース失敗)
  ↓ → 型情報なし → フォールバックで c_int に推論
```

## 設計

### アプローチ

1. `ApidocArg` に `"..."` 引数を判定するメソッドを追加
2. 型推論（`semantic.rs`）で `"..."` 引数を特別に扱い、文字列リテラル型として登録
3. コード生成（`rust_codegen.rs`）で `&str` として出力

### 新しいデータフロー

```
apidoc: "literal string"
  ↓ ApidocArg::split_type_and_name() [既存: 変更不要]
  ↓ ty = "\"literal string\"", name = ""
  ↓
  ↓ ApidocArg::is_literal_string() [新規]
  ↓ → true
  ↓
  ↓ semantic.rs: register_macro_params_from_apidoc()
  ↓ is_literal_string() → TypeRepr を ConstCharPtr として登録
  ↓
  ↓ rust_codegen.rs: get_param_type()
  ↓ → "&str" として出力
```

## 実装

### Phase 1: `ApidocArg` にリテラル文字列判定を追加

**ファイル**: `src/apidoc.rs`

```rust
impl ApidocArg {
    /// 引数がリテラル文字列型かどうか（"..." 形式）
    pub fn is_literal_string(&self) -> bool {
        self.ty.starts_with('"')
    }
}
```

### Phase 2: 型推論での特別扱い

**ファイル**: `src/semantic.rs`

`register_macro_params_from_apidoc()` で、`is_literal_string()` が true の場合、
`parse_type_from_string()` を呼ばず、直接 `const char *` 相当の `TypeRepr` を登録する。

```rust
for (i, &param_name) in params.iter().enumerate() {
    if let Some(apidoc_arg) = entry.args.get(i) {
        // リテラル文字列型の場合は const char * として登録
        if apidoc_arg.is_literal_string() {
            let ty = TypeRepr::Pointer {
                base: Box::new(TypeRepr::Base("c_char".to_string())),
                is_const: true,
            };
            self.define_symbol(Symbol {
                name: param_name,
                ty,
                loc: SourceLocation::default(),
                kind: SymbolKind::Variable,
            });
            continue;
        }
        // 通常の型パース...
    }
}
```

注: `TypeRepr::Pointer` / `TypeRepr::Base` の実際の構造に合わせて調整が必要。

### Phase 3: コード生成での `&str` 出力

**ファイル**: `src/rust_codegen.rs`

`get_param_type()` で、apidoc が `"..."` 引数の場合に `&str` を返す。

**方法 A**: `get_param_type()` 内で apidoc を参照し、`is_literal_string()` を確認。

**方法 B**: Phase 2 で特別な TypeRepr マーカーを使い、`type_repr_to_rust()` で `&str` に変換。

方法 A の方がシンプル:

```rust
fn get_param_type(&mut self, param: &MacroParam, info: &MacroInferInfo, param_index: usize) -> String {
    // ジェネリック型パラメータかチェック
    if let Some(generic_name) = info.generic_type_params.get(&(param_index as i32)) {
        return generic_name.clone();
    }

    // apidoc のリテラル文字列型チェック
    if let Some(apidoc) = &self.apidoc {
        let macro_name = self.interner.get(info.name);
        if let Some(entry) = apidoc.get(macro_name) {
            if let Some(arg) = entry.args.get(param_index) {
                if arg.is_literal_string() {
                    return "&str".to_string();
                }
            }
        }
    }

    // 既存の型推論ロジック...
}
```

注: `self.apidoc` が `RustCodegen` から参照可能かどうかを確認する必要がある。
現在 `RustCodegen` は `macro_ctx: &MacroInferContext` を持つが、apidoc への
参照は保持していない可能性がある。その場合、コンストラクタに追加する。

### Phase 3 の代替案: MacroInferInfo に情報を格納

apidoc を直接参照するのではなく、`MacroInferInfo` にリテラル文字列パラメータの
インデックス情報を格納する方法もある:

```rust
pub struct MacroInferInfo {
    // ... 既存フィールド ...
    /// リテラル文字列パラメータのインデックス集合
    pub literal_string_params: HashSet<usize>,
}
```

`build_macro_info()` で apidoc を参照して設定:

```rust
if let Some(apidoc) = apidoc {
    if let Some(entry) = apidoc.get(macro_name_str) {
        for (i, arg) in entry.args.iter().enumerate() {
            if arg.is_literal_string() {
                info.literal_string_params.insert(i);
            }
        }
    }
}
```

`get_param_type()` で参照:

```rust
if info.literal_string_params.contains(&param_index) {
    return "&str".to_string();
}
```

この方式は `generic_type_params` と同じパターンであり、一貫性がある。

## 変更ファイル一覧

| ファイル | 変更内容 |
|----------|----------|
| `src/apidoc.rs` | `ApidocArg::is_literal_string()` 追加 |
| `src/macro_infer.rs` | `MacroInferInfo::literal_string_params` フィールド追加、`build_macro_info()` で設定 |
| `src/rust_codegen.rs` | `get_param_type()` でリテラル文字列チェック追加 |

## 検証

1. `cargo build` / `cargo test`

2. 出力確認:
   ```bash
   cargo run -- --auto --gen-rust samples/xs-wrapper.h --bindings samples/bindings.rs 2>/dev/null \
     | grep -E 'fn (newSVpvs|sv_catpvs|savepvs|gv_stashpvs|hv_fetchs|memEQs|memCHRs|STR_WITH_LEN)'
   ```
   - `"..."` 引数が `&str` として出力されること

3. 回帰テスト: `cargo test rust_codegen_regression`
   - `newSVpvs` の期待結果を `&str` で更新

## エッジケース

1. **`"..."` 引数に名前がないケース**: `"literal string"` は名前なし。
   マクロパラメータとの対応は apidoc の引数インデックスで行う。

2. **`STR_WITH_LEN("literal string")`**: 引数が1つだけで `"..."` 型。
   STR_WITH_LEN は ExplicitExpandSymbols なので個別の関数は生成されないが、
   呼び出し元での型情報に影響する可能性がある。

3. **複数の `"..."` 引数**: `deprecate_disappears_in(U32 category, "when", "message")`
   のように複数ある場合も、各引数ごとに `&str` を付与。

4. **`const char *` との違い**: `"..."` は文字列リテラル限定であり、
   `const char *` は任意のポインタ。Rust では `&str`（スライス）で区別する。
