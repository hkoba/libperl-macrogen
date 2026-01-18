# 推論パイプラインのデバッグオプション

## 目的

`run_inference_with_preprocessor` の内部状態をデバッグするためのオプションを追加する。
本番のデータセットを使って、パイプラインの特定の段階でデータ構造をダンプし、
問題を調査できるようにする。

## 設計

### DebugOptions 構造体

```rust
/// デバッグ出力オプション
#[derive(Debug, Clone, Default)]
pub struct DebugOptions {
    /// apidoc マージ後にダンプして終了
    pub dump_apidoc_after_merge: Option<ApidocDumpConfig>,

    /// fields_dict 構築後にダンプして終了
    pub dump_fields_dict: Option<FieldsDictDumpConfig>,

    /// 型推論後にダンプして終了
    pub dump_after_inference: Option<InferenceDumpConfig>,
}

/// apidoc ダンプの設定
#[derive(Debug, Clone, Default)]
pub struct ApidocDumpConfig {
    /// 出力するエントリをフィルタ（正規表現）
    /// None の場合は全て出力
    pub filter: Option<String>,

    /// JSON 形式で出力
    pub json: bool,
}
```

### 関数シグネチャの変更

```rust
// 変更前
pub fn run_inference_with_preprocessor(
    mut pp: Preprocessor,
    apidoc_path: Option<&Path>,
    bindings_path: Option<&Path>,
) -> Result<InferResult, InferError>

// 変更後
pub fn run_inference_with_preprocessor(
    mut pp: Preprocessor,
    apidoc_path: Option<&Path>,
    bindings_path: Option<&Path>,
    debug_opts: Option<&DebugOptions>,
) -> Result<InferResult, InferError>
```

### 新しい Result 型

デバッグダンプで早期終了する場合の結果：

```rust
/// 推論結果またはデバッグダンプ
pub enum InferOutput {
    /// 通常の推論結果
    Result(InferResult),

    /// デバッグダンプで早期終了
    DebugDump {
        stage: &'static str,
        output: String,
    },
}
```

または、よりシンプルに：

```rust
// InferResult に stage 情報を追加するか、
// デバッグダンプは stderr に出力して Result は None を返す
pub fn run_inference_with_preprocessor(
    ...
) -> Result<Option<InferResult>, InferError>
```

### CLI オプション

```
--dump-apidoc-after-merge [FILTER]  apidoc マージ後にダンプして終了
                                    FILTER: 正規表現でエントリをフィルタ
                                    例: --dump-apidoc-after-merge "CopFILE.*"
```

## 実装計画

### Step 1: DebugOptions 構造体の追加

**ファイル**: `src/infer_api.rs`

```rust
/// デバッグ出力オプション
#[derive(Debug, Clone, Default)]
pub struct DebugOptions {
    /// apidoc マージ後にダンプして終了
    /// Some(filter) でフィルタ指定、Some("") で全件出力
    pub dump_apidoc_after_merge: Option<String>,
}

impl DebugOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn dump_apidoc(mut self, filter: impl Into<String>) -> Self {
        self.dump_apidoc_after_merge = Some(filter.into());
        self
    }
}
```

### Step 2: run_inference_with_preprocessor の変更

```rust
pub fn run_inference_with_preprocessor(
    mut pp: Preprocessor,
    apidoc_path: Option<&Path>,
    bindings_path: Option<&Path>,
    debug_opts: Option<&DebugOptions>,
) -> Result<Option<InferResult>, InferError> {
    // ... 既存コード ...

    apidoc_collector.merge_into(&mut apidoc);

    // デバッグダンプ
    if let Some(opts) = debug_opts {
        if let Some(filter) = &opts.dump_apidoc_after_merge {
            dump_apidoc(&apidoc, filter, pp.interner());
            return Ok(None);  // 早期終了
        }
    }

    // ... 残りの処理 ...

    Ok(Some(InferResult { ... }))
}
```

### Step 3: ApidocDict のダンプメソッド

**ファイル**: `src/apidoc.rs`

```rust
impl ApidocDict {
    /// エントリをフィルタしてダンプ
    pub fn dump_filtered(&self, filter: &str) -> String {
        let re = regex::Regex::new(filter).ok();

        let mut result = String::new();
        for (name, entry) in self.entries() {
            let matches = re.as_ref()
                .map(|r| r.is_match(name))
                .unwrap_or(true);

            if matches {
                result.push_str(&format!("{}:\n", name));
                result.push_str(&format!("  return_type: {:?}\n", entry.return_type));
                result.push_str(&format!("  args: {:?}\n", entry.args));
                result.push_str("\n");
            }
        }
        result
    }
}
```

### Step 4: CLI の変更

**ファイル**: `src/main.rs`

```rust
#[derive(Parser)]
struct Args {
    // ... 既存オプション ...

    /// apidoc マージ後にダンプして終了（正規表現でフィルタ可能）
    #[arg(long, value_name = "FILTER")]
    dump_apidoc_after_merge: Option<Option<String>>,
}
```

### Step 5: 呼び出し側の変更

`run_macro_inference` と main.rs の呼び出し箇所を更新。

## 使用例

```bash
# CopFILE 関連の apidoc をダンプ
cargo run -- --auto --dump-apidoc-after-merge "CopFILE.*" samples/wrapper.h

# 全ての apidoc をダンプ
cargo run -- --auto --dump-apidoc-after-merge "" samples/wrapper.h

# 出力例:
# CopFILE:
#   return_type: Some("const char *")
#   args: [ApidocArg { name: "c", ty: "const COP *", nullability: NN }]
#
# CopFILEAV:
#   return_type: Some("AV *")
#   args: [ApidocArg { name: "c", ty: "const COP *", nullability: NN }]
```

## 拡張可能性

同じパターンで他のデバッグポイントも追加可能：

- `--dump-fields-dict` - フィールド辞書のダンプ
- `--dump-typedefs` - typedef 辞書のダンプ
- `--dump-macro-info NAME` - 特定マクロの詳細情報
- `--dump-constraints NAME` - 特定マクロの型制約

## 変更対象ファイル

| ファイル | 変更内容 |
|----------|----------|
| `src/infer_api.rs` | `DebugOptions` 追加、関数シグネチャ変更 |
| `src/apidoc.rs` | `dump_filtered()` メソッド追加 |
| `src/main.rs` | CLI オプション追加、呼び出し変更 |
