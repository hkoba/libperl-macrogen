# マクロ型推論の改善: typedef 辞書と THX 依存判定の修正

## 目標

3つの改善を行う:
1. typedef 辞書を初回パース時に収集し、マクロ解析で型名として認識させる
2. preprocessor callback を ApidocCollector のみに簡素化
3. THX 依存判定を analyze_all_macros 内で行い、定義順序依存の問題を解決

## 現状の問題

### 問題1: typedef が認識されない
```
MUTABLE_AV: unparseable (0 constraints, 1 uses)
  error: expected primary expression, found RParen
```
- `(AV *)` のキャストで `AV` が typedef として認識されない
- `macro_infer.rs:try_parse_tokens` で空の typedef セットを使用

### 問題2: THX 依存判定の定義順序依存
- `on_macro_defined` はマクロ定義順に呼ばれる
- マクロAがマクロBを使用し、Bが後で定義される場合、AはTHX依存と判定されない
- 例: `#define A THX_BASE` → `#define THX_BASE aTHX` の順だとAが漏れる

## 実装計画

### Step 1: typedef 辞書の収集と受け渡し

**src/main.rs の run_infer_macro_types:**

```rust
// parse_each 完了後に typedef を取得
let typedefs = parser.typedefs().clone();

// analyze_all_macros に渡す
infer_ctx.analyze_all_macros(
    pp.macros(),
    interner,
    files,
    Some(&apidoc),
    Some(&fields_dict),
    rust_decl_dict.as_ref(),
    &typedefs,  // 追加
);
```

### Step 2: MacroInferContext の修正

**src/macro_infer.rs:**

1. `analyze_all_macros` のシグネチャ変更:
```rust
pub fn analyze_all_macros<'a>(
    &mut self,
    macro_table: &MacroTable,
    interner: &'a StringInterner,
    files: &FileRegistry,
    apidoc: Option<&'a ApidocDict>,
    fields_dict: Option<&'a FieldsDict>,
    rust_decl_dict: Option<&'a RustDeclDict>,
    typedefs: &HashSet<InternedStr>,  // 追加
    thx_symbols: (InternedStr, InternedStr, InternedStr),  // 追加
) {
    // Step 3 の THX 収集をここで行う
    let thx_macros = self.collect_thx_dependencies(macro_table, thx_symbols);

    for def in macro_table.iter_target_macros() {
        self.analyze_macro(
            def, macro_table, &thx_macros, interner, files,
            apidoc, fields_dict, rust_decl_dict, typedefs,
        );
    }
    // ...
}
```

2. `analyze_macro` のシグネチャ変更（ThxCollector → HashSet）:
```rust
pub fn analyze_macro<'a>(
    &mut self,
    def: &MacroDef,
    macro_table: &MacroTable,
    thx_macros: &HashSet<InternedStr>,  // ThxCollector から変更
    interner: &'a StringInterner,
    files: &FileRegistry,
    apidoc: Option<&'a ApidocDict>,
    fields_dict: Option<&'a FieldsDict>,
    rust_decl_dict: Option<&'a RustDeclDict>,
    typedefs: &HashSet<InternedStr>,  // 追加
) {
    // ...
    info.is_thx_dependent = thx_macros.contains(&def.name);
    // ...
    info.parse_result = self.try_parse_tokens(&expanded_tokens, interner, files, typedefs);
}
```

3. `try_parse_tokens` のシグネチャ変更:
```rust
fn try_parse_tokens(
    &self,
    tokens: &[Token],
    interner: &StringInterner,
    files: &FileRegistry,
    typedefs: &HashSet<InternedStr>,  // 引数として受け取る
) -> ParseResult {
    // 空の HashSet::new() の代わりに typedefs を使用
    match parse_expression_from_tokens_ref(tokens.to_vec(), interner, files, typedefs) {
        // ...
    }
}
```

### Step 3: THX 依存の収集を analyze_all_macros 内で実装

**注意**: `interner.intern()` は `&mut self` を必要とするため、THX シンボルは main.rs で事前に intern し、引数として渡す。

**src/main.rs で THX シンボルを事前 intern:**
```rust
// THX シンボルを事前に intern
let sym_athx = pp.interner_mut().intern("aTHX");
let sym_tthx = pp.interner_mut().intern("tTHX");
let sym_my_perl = pp.interner_mut().intern("my_perl");
let thx_symbols = (sym_athx, sym_tthx, sym_my_perl);

// analyze_all_macros に渡す
infer_ctx.analyze_all_macros(
    pp.macros(),
    interner,
    files,
    Some(&apidoc),
    Some(&fields_dict),
    rust_decl_dict.as_ref(),
    &typedefs,
    thx_symbols,  // 追加
);
```

**src/macro_infer.rs に新メソッド追加:**

```rust
/// 全マクロから THX 依存関係を収集（定義順序に依存しない）
fn collect_thx_dependencies(
    &self,
    macro_table: &MacroTable,
    thx_symbols: (InternedStr, InternedStr, InternedStr),
) -> HashSet<InternedStr> {
    let (sym_athx, sym_tthx, sym_my_perl) = thx_symbols;

    // Phase 1: 直接 THX トークンを含むマクロを収集
    let mut thx_macros = HashSet::new();
    for def in macro_table.iter() {
        for token in &def.body {
            if let TokenKind::Ident(id) = &token.kind {
                if *id == sym_athx || *id == sym_tthx || *id == sym_my_perl {
                    thx_macros.insert(def.name);
                    break;
                }
            }
        }
    }

    // Phase 2: 推移的閉包を計算（THX マクロを使用するマクロも THX 依存）
    loop {
        let mut added = false;
        for def in macro_table.iter() {
            if thx_macros.contains(&def.name) {
                continue;
            }
            for token in &def.body {
                if let TokenKind::Ident(id) = &token.kind {
                    if thx_macros.contains(id) {
                        thx_macros.insert(def.name);
                        added = true;
                        break;
                    }
                }
            }
        }
        if !added {
            break;
        }
    }

    thx_macros
}
```

### Step 4: preprocessor callback を ApidocCollector のみに変更

**src/main.rs の run_infer_macro_types:**

```rust
// 変更前:
let callback_pair = CallbackPair::new(
    ApidocCollector::new(),
    ThxCollector::new(pp.interner_mut()),
);
pp.set_macro_def_callback(Box::new(callback_pair));

// 変更後:
pp.set_macro_def_callback(Box::new(ApidocCollector::new()));

// ...

// ダウンキャスト部分も変更:
let callback = pp.take_macro_def_callback().expect("callback should exist");
let apidoc_collector = callback
    .into_any()
    .downcast::<ApidocCollector>()
    .expect("callback type mismatch");
```

## 修正対象ファイル

1. **src/macro_infer.rs**
   - `analyze_all_macros`: シグネチャ変更、THX 収集追加
   - `analyze_macro`: シグネチャ変更（ThxCollector → HashSet）
   - `try_parse_tokens`: typedefs 引数追加
   - `collect_thx_dependencies`: 新メソッド追加

2. **src/main.rs**
   - `run_infer_macro_types`:
     - typedef 取得と受け渡し
     - callback を ApidocCollector のみに変更
     - ThxCollector 関連のコードを削除

3. **src/lib.rs**（変更不要の可能性）
   - ThxCollector のエクスポートは維持（他で使われる可能性）

## 期待される結果

1. `MUTABLE_AV` 等の `(AV *)` キャストを含むマクロがパース可能になる
2. THX 依存判定がマクロの定義順序に依存しなくなる
3. preprocessor callback API は維持され、将来の拡張に対応可能
