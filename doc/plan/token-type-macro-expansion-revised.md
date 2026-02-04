# 改訂版: Token 型マクロを TokenExpander で展開する

## 背景

### 問題の状況

`XopENTRYCUSTOM` のようなマクロは apidoc で `token` 型の引数を持つと宣言されている:

```
=for apidoc Amu||XopENTRYCUSTOM|const OP *o|token which
```

このマクロはトークン合成（`##`）を使用しており、展開が必要:

```c
#define XopENTRYCUSTOM(o, which) \
    (Perl_custom_op_get_field(aTHX_ o, XOPe_ ## which).which)
```

### 期待される出力

`OP_CLASS` マクロ（`XopENTRYCUSTOM` を呼び出す）の生成結果:

```rust
pub unsafe fn OP_CLASS(my_perl: *mut PerlInterpreter, o: *mut OP) -> U32 {
    unsafe {
        (if ((*o).op_type == OP_CUSTOM) {
            Perl_custom_op_get_field(my_perl, o, XOPe_xop_class).xop_class  // ← 展開された
        } else {
            ((*PL_opargs.offset((*o).op_type as isize)) & (15 << 8))
        })
    }
}
```

### 現在の問題

初回の実装では、apidoc から検出した token 型マクロを `Preprocessor.explicit_expand_macros` に追加した。しかし:

1. **Inline 関数の body**: Preprocessor でマクロ展開 → `explicit_expand_macros` が使われる ✓
2. **マクロ関数の body**: TokenExpander でマクロ展開 → `ExplicitExpandSymbols` が使われる ✗

マクロ関数の処理経路では、apidoc から検出した情報が渡されていない。

## 処理経路の違い

```
┌─────────────────────────────────────────────────────────────────┐
│                    Inline 関数の処理                             │
│                                                                 │
│  Preprocessor.expand_macro()                                    │
│    └── explicit_expand_macros をチェック ← 私の実装で追加済み    │
│                                                                 │
│  → XopENTRYCUSTOM は展開される（はず）                          │
└─────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────┐
│                    マクロ関数の処理                              │
│                                                                 │
│  MacroInferContext.build_macro_info()                           │
│    └── TokenExpander.extend_explicit_expand(                    │
│            explicit_expand.iter()  ← ExplicitExpandSymbols のみ │
│        )                                                        │
│                                                                 │
│  → XopENTRYCUSTOM は explicit_expand に含まれない               │
│  → 展開されずに関数呼び出しとして残る                            │
└─────────────────────────────────────────────────────────────────┘
```

## 実装計画

### Step 1: ApidocCollector に token 型マクロリストを保持

**ファイル**: `src/apidoc.rs`

```rust
pub struct ApidocCollector {
    entries: HashMap<String, ApidocEntry>,
    token_type_macros: Vec<String>,  // NEW: token 型マクロ名のリスト
}

impl ApidocCollector {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            token_type_macros: Vec::new(),
        }
    }

    /// 検出した token 型マクロ名を返す
    pub fn token_type_macros(&self) -> &[String] {
        &self.token_type_macros
    }
}

impl CommentCallback for ApidocCollector {
    fn on_comment(&mut self, comment: &Comment, _file_id: FileId, _is_target: bool) -> Option<Vec<String>> {
        let mut explicit_expand_macros = Vec::new();

        for line in comment.text.lines() {
            if let Some(entry) = ApidocEntry::parse_apidoc_line(line) {
                if entry.has_token_arg() {
                    explicit_expand_macros.push(entry.name.clone());
                    self.token_type_macros.push(entry.name.clone());  // 内部にも保存
                }
                self.entries.insert(entry.name.clone(), entry);
            }
        }

        if explicit_expand_macros.is_empty() {
            None
        } else {
            Some(explicit_expand_macros)
        }
    }
}
```

### Step 2: analyze_all_macros() のシグネチャ拡張

**ファイル**: `src/macro_infer.rs`

```rust
pub fn analyze_all_macros<'a>(
    &mut self,
    macro_table: &MacroTable,
    interner: &'a StringInterner,
    files: &FileRegistry,
    apidoc: Option<&'a ApidocDict>,
    fields_dict: Option<&'a FieldsDict>,
    rust_decl_dict: Option<&'a RustDeclDict>,
    inline_fn_dict: Option<&'a InlineFnDict>,
    c_fn_decl_dict: Option<&'a CFnDeclDict>,
    typedefs: &HashSet<InternedStr>,
    thx_symbols: (InternedStr, InternedStr, InternedStr),
    no_expand: NoExpandSymbols,
    explicit_expand: ExplicitExpandSymbols,
    additional_explicit_expand: &[InternedStr],  // NEW: 追加の明示展開マクロ
) {
    // ...
}
```

### Step 3: build_macro_info() で追加リストを使用

**ファイル**: `src/macro_infer.rs`

`build_macro_info()` のシグネチャも同様に拡張し、TokenExpander に追加:

```rust
fn build_macro_info(
    &mut self,
    def: &MacroDef,
    // ... existing params ...
    explicit_expand: ExplicitExpandSymbols,
    additional_explicit_expand: &[InternedStr],  // NEW
) -> (MacroInferInfo, bool, bool) {
    // ...

    // TokenExpander に明示展開マクロを設定
    expander.extend_explicit_expand(explicit_expand.iter());
    expander.extend_explicit_expand(additional_explicit_expand.iter().copied());  // NEW

    // ...
}
```

### Step 4: infer_api.rs で情報を受け渡す

**ファイル**: `src/infer_api.rs`

```rust
// コールバックを取り出してダウンキャスト
let callback = pp.take_comment_callback().expect("callback should exist");
let apidoc_collector = callback
    .into_any()
    .downcast::<ApidocCollector>()
    .expect("callback type mismatch");

// token 型マクロを intern して InternedStr のベクターに変換
let token_type_macros: Vec<InternedStr> = apidoc_collector
    .token_type_macros()
    .iter()
    .map(|name| pp.interner_mut().intern(name))
    .collect();

// ...

infer_ctx.analyze_all_macros(
    pp.macros(),
    interner,
    files,
    Some(&apidoc),
    Some(&fields_dict),
    rust_decl_dict.as_ref(),
    Some(&inline_fn_dict),
    Some(&c_fn_decl_dict),
    &typedefs,
    thx_symbols,
    no_expand,
    explicit_expand,
    &token_type_macros,  // NEW
);
```

## 変更ファイル一覧

| ファイル | 変更内容 |
|----------|----------|
| `src/apidoc.rs` | `ApidocCollector` に `token_type_macros` フィールドと取得メソッドを追加 |
| `src/macro_infer.rs` | `analyze_all_macros()` と `build_macro_info()` に追加パラメータ |
| `src/infer_api.rs` | `ApidocCollector` から token 型マクロを取得して渡す |

## 検証

1. ビルド: `cargo build`
2. テスト: `cargo test`
3. 実際の出力確認:
   ```bash
   cargo run -- --auto --gen-rust --bindings samples/bindings.rs samples/xs-wrapper.h 2>&1 | grep -A10 "fn OP_CLASS"
   ```
4. 期待される結果: `XopENTRYCUSTOM` が展開されて `Perl_custom_op_get_field(...)` になっている

## 備考

- 初回の実装（Preprocessor への追加）は inline 関数の処理には有効
- 本計画はマクロ関数の処理経路にも対応するための追加実装
- 両方の経路で token 型マクロが正しく展開されるようになる
