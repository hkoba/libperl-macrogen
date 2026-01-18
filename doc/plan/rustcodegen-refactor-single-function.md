# RustCodegen の再設計: 単一関数生成への変更

## 背景

生成された Rust 関数に `/* unknown */`, `/* type */`, `/* TODO: ... */` などの
不完全マーカーが含まれる場合、その関数全体をコメントアウトして出力する必要がある。

### 当初のアプローチ（却下）

生成後の文字列を検査する `has_incomplete_markers()` 関数を使用。

```rust
fn has_incomplete_markers(code: &str) -> bool {
    code.contains("/* unknown */")
        || code.contains("/* type */")
        || code.contains("/* TODO:")
}
```

**問題点**: 文字列ベースの検査は構造的でなく、好ましくない。

### 採用するアプローチ

RustCodegen を毎回作り直し、生成中に不完全マーカーの数をカウントする。

## 設計

### GeneratedCode: 生成結果

```rust
/// 一つの関数の生成結果
pub struct GeneratedCode {
    /// 生成されたコード
    pub code: String,
    /// 不完全マーカーの数
    pub incomplete_count: usize,
}

impl GeneratedCode {
    pub fn is_complete(&self) -> bool {
        self.incomplete_count == 0
    }
}
```

### RustCodegen: 単一関数生成器（使い捨て）

```rust
/// 一つの関数を生成するためのコードジェネレータ
///
/// 各関数の生成ごとにフレッシュなインスタンスを作成して使用する。
pub struct RustCodegen<'a> {
    interner: &'a StringInterner,
    /// 内部バッファ（生成結果を蓄積）
    buffer: String,
    /// 不完全マーカーの生成回数
    incomplete_count: usize,
}

impl<'a> RustCodegen<'a> {
    pub fn new(interner: &'a StringInterner) -> Self {
        Self {
            interner,
            buffer: String::new(),
            incomplete_count: 0,
        }
    }

    /// マクロ関数を生成（self を消費して結果を返す）
    pub fn generate_macro(mut self, info: &MacroInferInfo) -> GeneratedCode {
        // 生成ロジック（self.buffer に書き込み）
        // ...
        GeneratedCode {
            code: self.buffer,
            incomplete_count: self.incomplete_count,
        }
    }

    /// inline 関数を生成（self を消費して結果を返す）
    pub fn generate_inline_fn(
        mut self,
        name: InternedStr,
        func_def: &FunctionDef,
    ) -> GeneratedCode {
        // 生成ロジック
        // ...
        GeneratedCode {
            code: self.buffer,
            incomplete_count: self.incomplete_count,
        }
    }

    // ========== マーカー生成（カウンターをインクリメント） ==========

    fn unknown_marker(&mut self) -> &'static str {
        self.incomplete_count += 1;
        "/* unknown */"
    }

    fn todo_marker(&mut self, msg: &str) -> String {
        self.incomplete_count += 1;
        format!("/* TODO: {} */", msg)
    }

    fn type_marker(&mut self) -> &'static str {
        self.incomplete_count += 1;
        "/* type */"
    }
}
```

### CodegenDriver: 全体管理

```rust
/// コード生成全体を管理する構造体
///
/// 実際の出力先（Write）を保持し、生成の成功/失敗に応じて
/// 適切な形式で出力する。
pub struct CodegenDriver<'a, W: Write> {
    writer: W,
    interner: &'a StringInterner,
    stats: GenerateStats,
}

impl<'a, W: Write> CodegenDriver<'a, W> {
    pub fn new(writer: W, interner: &'a StringInterner) -> Self {
        Self {
            writer,
            interner,
            stats: GenerateStats::default(),
        }
    }

    pub fn generate_macros(&mut self, result: &InferResult) -> io::Result<()> {
        // ヘッダー出力
        writeln!(self.writer, "// Macro Functions")?;

        for (_, info) in &result.infer_ctx.macros {
            // フレッシュな RustCodegen を作成
            let codegen = RustCodegen::new(self.interner);
            let generated = codegen.generate_macro(info);

            if generated.is_complete() {
                // 完全な生成：そのまま出力
                writeln!(self.writer, "{}", generated.code)?;
                self.stats.macros_success += 1;
            } else {
                // 不完全な生成：コメントアウトして出力
                self.output_as_incomplete(&generated, "macro function")?;
                self.stats.macros_type_incomplete += 1;
            }
        }
        Ok(())
    }

    pub fn generate_inline_fns(&mut self, result: &InferResult) -> io::Result<()> {
        writeln!(self.writer, "// Inline Functions")?;

        for (name, func_def) in &result.inline_fn_dict {
            let codegen = RustCodegen::new(self.interner);
            let generated = codegen.generate_inline_fn(*name, func_def);

            if generated.is_complete() {
                writeln!(self.writer, "{}", generated.code)?;
                self.stats.inline_fns_success += 1;
            } else {
                self.output_as_incomplete(&generated, "inline function")?;
                self.stats.inline_fns_type_incomplete += 1;
            }
        }
        Ok(())
    }

    /// 不完全な生成結果をコメントアウトして出力
    fn output_as_incomplete(&mut self, gen: &GeneratedCode, kind: &str) -> io::Result<()> {
        writeln!(self.writer, "// [CODEGEN_INCOMPLETE] {}", kind)?;
        for line in gen.code.lines() {
            writeln!(self.writer, "// {}", line)?;
        }
        writeln!(self.writer)
    }

    pub fn stats(&self) -> &GenerateStats {
        &self.stats
    }
}
```

## 実装フェーズ

### Phase 1: 構造体の追加・分離

1. `GeneratedCode` 構造体を追加
2. `RustCodegen` を変更:
   - `writer: W` → `buffer: String`
   - `incomplete_count: usize` フィールドを追加
3. `CodegenDriver` 構造体を新規追加

### Phase 2: マーカー生成メソッドの追加

1. `RustCodegen` に以下を追加:
   - `fn unknown_marker(&mut self) -> &'static str`
   - `fn todo_marker(&mut self, msg: &str) -> String`
   - `fn type_marker(&mut self) -> &'static str`

2. 既存のマーカー文字列生成を置き換え:
   - `"/* unknown */".to_string()` → `self.unknown_marker().to_string()`
   - `format!("/* TODO: ... */")` → `self.todo_marker(...)`
   - `"/* type */".to_string()` → `self.type_marker().to_string()`

### Phase 3: 生成メソッドの変更

1. `generate_macro(self, info) -> GeneratedCode` を実装
2. `generate_inline_fn(self, name, func_def) -> GeneratedCode` を実装
3. 内部の `writeln!` を `write!(&mut self.buffer, ...)` に変更

### Phase 4: CodegenDriver の実装

1. `CodegenDriver::generate_macros()` を実装
2. `CodegenDriver::generate_inline_fns()` を実装
3. `output_as_incomplete()` ヘルパーを実装
4. 統計情報の管理を移行

### Phase 5: クリーンアップ

1. `has_incomplete_markers()` 関数を削除
2. 不要になった旧コードを削除
3. テストの更新

## 利点

1. **Cell 不要**: `&mut self` で素直にカウント可能
2. **独立性**: 各生成が完全に独立しており、副作用がない
3. **明確な分離**: 生成結果（code）とメタ情報（incomplete_count）が構造体で分離
4. **テスト容易性**: `GeneratedCode` を検査することでテスト可能
5. **構造的検出**: 文字列検索ではなく、生成時点でカウント

## 変更対象ファイル

- `src/rust_codegen.rs`: 主要な変更
- `src/lib.rs`: 必要に応じて再エクスポート
- `src/main.rs`: CodegenDriver を使用するよう変更

## マーカー生成箇所（現状）

```
rust_codegen.rs:462  - stmt_to_rust_inline: 未対応 stmt パターン
rust_codegen.rs:573  - expr_to_rust_inline: 未対応 expr パターン
rust_codegen.rs:763  - get_param_type: パラメータ型が不明
rust_codegen.rs:775  - get_return_type: 戻り値型が不明
rust_codegen.rs:907  - expr_to_rust: 未対応 expr パターン
rust_codegen.rs:923  - stmt_to_rust: 未対応 stmt パターン
rust_codegen.rs:935  - type_repr_to_rust: 型表現が不明
```

これらすべてを `unknown_marker()`, `todo_marker()`, `type_marker()` に置き換える。
