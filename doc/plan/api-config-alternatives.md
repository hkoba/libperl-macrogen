# Config 設計の代替案

## 前提条件（共通目標）

1. 3フェーズ構成: Preprocessor → Inference → Codegen
2. 個別フェーズ呼び出しと一括呼び出しの両方をサポート

## 案A: 単一の大きな Config（現在の提案）

```rust
pub struct PipelineConfig {
    // Preprocessor 設定
    pub input_file: PathBuf,
    pub include_paths: Vec<PathBuf>,
    pub defines: HashMap<String, Option<String>>,
    pub wrapped_macros: Vec<String>,

    // Inference 設定
    pub bindings_path: Option<PathBuf>,
    pub apidoc_path: Option<PathBuf>,

    // Codegen 設定
    pub rust_edition: String,
    pub strict_rustfmt: bool,
}
```

### 長所
- シンプル、型が一つだけ
- 設定の一覧性が高い
- Builder パターンで構築しやすい

### 短所
- 関心の分離がない（Codegen 設定が Preprocessor 段階でも見える）
- 「この設定はどのフェーズで使われるか」が型から読み取れない
- フェーズ毎に異なるバリデーションが必要な場合に対応しにくい

---

## 案B: フェーズ別 Config + 合成

```rust
/// Preprocessor フェーズの設定
pub struct PreprocessConfig {
    pub input_file: PathBuf,
    pub include_paths: Vec<PathBuf>,
    pub defines: HashMap<String, Option<String>>,
    pub wrapped_macros: Vec<String>,
}

/// Inference フェーズの設定
pub struct InferConfig {
    pub bindings_path: Option<PathBuf>,
    pub apidoc_path: Option<PathBuf>,
}

/// Codegen フェーズの設定
pub struct CodegenConfig {
    pub rust_edition: String,
    pub strict_rustfmt: bool,
}

/// 全フェーズの設定を束ねる
pub struct PipelineConfig {
    pub preprocess: PreprocessConfig,
    pub infer: InferConfig,
    pub codegen: CodegenConfig,
}
```

### 使用例

```rust
// 一括設定
let config = PipelineConfig {
    preprocess: PreprocessConfig::auto_perl()?
        .with_codegen_defaults(),
    infer: InferConfig::new()
        .with_bindings("bindings.rs"),
    codegen: CodegenConfig::default(),
};

Pipeline::new(config)?.generate(&mut output)?;

// Preprocessor のみ使用
let pp_config = PreprocessConfig::auto_perl()?;
let pp = Preprocessor::from_config(pp_config)?;
```

### 長所
- 関心が明確に分離されている
- 各フェーズの設定を独立して再利用可能
- 型からどの設定がどのフェーズで使われるか明確

### 短所
- 型が増える
- ネストした構造体の構築がやや冗長

---

## 案C: フェーズ別 Config + Builder

案B の構造を維持しつつ、Builder で構築を簡略化。

```rust
pub struct PipelineBuilder {
    preprocess: PreprocessConfig,
    infer: InferConfig,
    codegen: CodegenConfig,
}

impl PipelineBuilder {
    pub fn new(input_file: PathBuf) -> Self { ... }

    // Preprocess 設定
    pub fn with_auto_perl_config(mut self) -> Result<Self, Error> { ... }
    pub fn with_codegen_defaults(mut self) -> Self { ... }
    pub fn with_include(mut self, path: PathBuf) -> Self { ... }

    // Infer 設定
    pub fn with_bindings(mut self, path: PathBuf) -> Self { ... }
    pub fn with_apidoc(mut self, path: PathBuf) -> Self { ... }

    // Codegen 設定
    pub fn with_rust_edition(mut self, edition: &str) -> Self { ... }
    pub fn with_strict_rustfmt(mut self) -> Self { ... }

    // Pipeline を構築
    pub fn build(self) -> Pipeline { ... }

    // 個別の Config を取り出す
    pub fn preprocess_config(self) -> PreprocessConfig { self.preprocess }
    pub fn infer_config(self) -> (PreprocessConfig, InferConfig) { ... }
}
```

### 使用例

```rust
// 一括実行
PipelineBuilder::new("wrapper.h".into())
    .with_auto_perl_config()?
    .with_codegen_defaults()
    .with_bindings("bindings.rs".into())
    .build()
    .generate(&mut output)?;

// Preprocessor 設定だけ取り出す
let pp_config = PipelineBuilder::new("wrapper.h".into())
    .with_auto_perl_config()?
    .preprocess_config();
```

### 長所
- フラットな Builder API で構築が簡単
- 内部では関心が分離されている
- 個別の Config も取り出せる

### 短所
- Builder と Config の二重構造
- 「設定を変更して再実行」がしにくい（Builder は消費される）

---

## 案D: 遅延評価 Config

設定を「どう取得するか」の形で保持し、必要になった時点で評価。

```rust
pub struct PipelineConfig {
    input_file: PathBuf,

    // 遅延評価される設定
    perl_config: Option<PerlConfigSource>,  // Auto | Manual(paths, defines)
    bindings: Option<PathBuf>,
    // ...
}

enum PerlConfigSource {
    Auto,  // Config.pm から取得
    Manual { include_paths: Vec<PathBuf>, defines: HashMap<...> },
}

impl Pipeline {
    pub fn preprocess(self) -> Result<PreprocessedPipeline, Error> {
        // ここで初めて PerlConfigSource を評価
        let pp_config = self.config.resolve_preprocess_config()?;
        // ...
    }
}
```

### 長所
- 設定の指定時点ではエラーが発生しない
- 必要なフェーズまで評価を遅延できる

### 短所
- エラーが遅延して発生するため、問題の原因が分かりにくい
- 複雑度が上がる

---

## 案E: Trait ベース

各フェーズが必要とする設定を Trait で定義。

```rust
pub trait PreprocessSettings {
    fn input_file(&self) -> &Path;
    fn include_paths(&self) -> &[PathBuf];
    fn defines(&self) -> &HashMap<String, Option<String>>;
    fn wrapped_macros(&self) -> &[String];
}

pub trait InferSettings {
    fn bindings_path(&self) -> Option<&Path>;
    fn apidoc_path(&self) -> Option<&Path>;
}

pub trait CodegenSettings {
    fn rust_edition(&self) -> &str;
    fn strict_rustfmt(&self) -> bool;
}

// 全ての Trait を実装する統合 Config
pub struct PipelineConfig { ... }
impl PreprocessSettings for PipelineConfig { ... }
impl InferSettings for PipelineConfig { ... }
impl CodegenSettings for PipelineConfig { ... }

// 各フェーズは必要な Trait のみを要求
impl Preprocessor {
    pub fn from_settings(settings: &impl PreprocessSettings) -> Result<Self, Error> { ... }
}
```

### 長所
- 各フェーズが必要とする設定が型で明示される
- 部分的な設定だけを渡すことが可能
- テスト時にモックを注入しやすい

### 短所
- Trait bounds が複雑になりがち
- 実装の手間が増える

---

## 比較表

| 観点 | 案A (単一Config) | 案B (フェーズ別) | 案C (Builder) | 案D (遅延) | 案E (Trait) |
|------|-----------------|-----------------|---------------|-----------|-------------|
| シンプルさ | ◎ | ○ | ○ | △ | △ |
| 関心の分離 | △ | ◎ | ◎ | ○ | ◎ |
| 構築の容易さ | ◎ | △ | ◎ | ○ | ○ |
| 型安全性 | ○ | ◎ | ◎ | ○ | ◎ |
| 拡張性 | ○ | ◎ | ○ | ○ | ◎ |
| 学習コスト | 低 | 中 | 中 | 高 | 高 |

---

## 推奨: 案C（フェーズ別 Config + Builder）

### 理由

1. **利用者視点**: Builder API により、設定はフラットに記述できる
2. **内部設計**: フェーズ別 Config により、関心が分離される
3. **バランス**: シンプルさと型安全性のバランスが良い
4. **段階的導入**: 最初は Builder だけ公開し、後から個別 Config を公開可能

### 提案する API

```rust
// 通常の使用（Builder 経由）
Pipeline::builder("wrapper.h")
    .with_auto_perl_config()?
    .with_codegen_defaults()
    .with_bindings("bindings.rs")
    .build()?
    .generate(&mut output)?;

// 段階的実行
let pipeline = Pipeline::builder("wrapper.h")
    .with_auto_perl_config()?
    .with_codegen_defaults()
    .build()?;

let preprocessed = pipeline.preprocess()?;
// preprocessed.preprocessor() で Preprocessor にアクセス

let inferred = preprocessed
    .with_bindings("bindings.rs")  // Infer 設定を追加
    .infer()?;

let generated = inferred
    .with_strict_rustfmt()  // Codegen 設定を追加
    .generate(&mut output)?;
```

### 案A との主な違い

1. 内部的にはフェーズ別に Config が分離されている
2. 各フェーズの遷移時に追加設定が可能（上記の段階的実行の例）
3. 将来的に個別 Config を公開しやすい

---

## 結論

**案A（単一Config）** で十分な場合:
- ライブラリが小規模で、設定項目が少ない
- ほとんどのユーザーが一括実行のみ使用

**案C（Builder + フェーズ別Config）** が望ましい場合:
- 設定項目が多く、フェーズ毎に異なる
- 個別フェーズの利用が重要なユースケース
- 将来的な拡張を見越している

現状のこのライブラリでは、設定項目がそれなりに多く、`-E`（Preprocessor のみ）や
`--typed-sexp`（Inference まで）など個別フェーズの利用もあるため、
**案C を推奨**します。ただし、案A との差は大きくないため、
シンプルさを優先するなら案A でも問題ありません。
