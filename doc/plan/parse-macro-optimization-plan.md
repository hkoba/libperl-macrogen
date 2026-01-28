# parse_macro_body 最適化計画

## 問題概要

`--gen-rust-fns` の処理時間が約25秒かかっており、そのうち94.71%が `parse_macro_body` で費やされている。

## プロファイリング結果

flamegraph による分析結果:

| 処理 | CPU時間 |
|------|---------|
| `StringInterner::clone` | 67.29% |
| `HashMap::clone` (typedefs) | 38.68% |
| `parse_expression_from_tokens` (実際のパース) | 26.40% |
| `TokenSlice drop_in_place` | 26.26% |

## 根本原因

`parse_macro_body` 内で毎回大きなデータ構造がクローンされている:

```rust
pub fn parse_macro_body(&self, def: &MacroDef, macros: &MacroTable) -> (Vec<Token>, Result<Expr>) {
    let expanded = self.expand_macro_body(def, macros, &mut HashSet::new());

    // 以下のクローンが非常に高コスト
    let interner = self.interner.clone();   // StringInterner: HashMap + Vec<String>
    let files = self.files.clone();          // FileRegistry
    let typedefs = self.typedefs.clone();    // HashSet<InternedStr>

    let result = parse_expression_from_tokens(expanded.clone(), interner, files, typedefs);
    (expanded, result)
}
```

これが約3,200回（成功したマクロ数）呼ばれるため、クローン操作だけで処理時間の大部分を占めている。

## 改善案

### 案1: parse_expression_from_tokens を参照ベースに変更

現在のシグネチャ:
```rust
pub fn parse_expression_from_tokens(
    tokens: Vec<Token>,
    interner: StringInterner,
    files: FileRegistry,
    typedefs: HashSet<InternedStr>,
) -> Result<Expr>
```

改善後のシグネチャ:
```rust
pub fn parse_expression_from_tokens(
    tokens: Vec<Token>,
    interner: &StringInterner,
    files: &FileRegistry,
    typedefs: &HashSet<InternedStr>,
) -> Result<Expr>
```

**必要な変更:**
1. `TokenSlice` を参照ベースに変更
2. `Parser::from_source_with_typedefs` を参照ベースに変更
3. TokenSource トレイトの `interner()` / `files()` メソッドを調整

**メリット:**
- クローンが不要になり、処理時間を大幅に削減
- メモリ使用量も削減

**デメリット:**
- ライフタイムの管理が複雑になる可能性
- TokenSource トレイトの設計変更が必要

### 案2: パース結果のキャッシュ

`MacroAnalyzer` にパース済みの式をキャッシュする:

```rust
pub struct MacroAnalyzer<'a> {
    // ... 既存フィールド ...
    /// パース済みの式をキャッシュ
    parsed_exprs: HashMap<InternedStr, Result<Expr, CompileError>>,
}
```

**必要な変更:**
1. `analyze()` 時に `parse_macro_body` を呼んでキャッシュ
2. main.rs の forループでキャッシュから取得

**メリット:**
- 既存のAPIを大きく変えずに済む
- パースは1回だけ

**デメリット:**
- メモリ使用量が増加
- キャッシュの管理が必要

### 案3: TokenSlice に Rc/Arc を使用

`TokenSlice` が内部で `Rc<StringInterner>` と `Rc<FileRegistry>` を持つようにする:

```rust
pub struct TokenSlice {
    tokens: Vec<Token>,
    pos: usize,
    interner: Rc<StringInterner>,
    files: Rc<FileRegistry>,
}
```

**メリット:**
- クローンが安価になる (参照カウントのインクリメントのみ)

**デメリット:**
- 既存コードの多くの箇所で型の変更が必要
- Rc の参照カウントオーバーヘッド（軽微）

## 推奨案

**案1 (参照ベース) を推奨**

理由:
- 最もクリーンな解決策
- 余計なメモリ使用量増加がない
- Rust らしい設計

## 実装順序

1. `TokenSlice` を参照ベースに変更
   - `interner: &'a StringInterner`
   - `files: &'a FileRegistry`

2. `TokenSource` トレイトの調整
   - `interner(&self) -> &StringInterner`
   - `files(&self) -> &FileRegistry`

3. `parse_expression_from_tokens` のシグネチャ変更

4. `Parser::from_source_with_typedefs` の調整

5. `MacroAnalyzer::parse_macro_body` からクローンを削除

6. テストと動作確認

## 期待される効果

- 処理時間: 25秒 → 5-8秒程度（約70-80%削減）
- メモリ使用量: 削減（クローンによる一時オブジェクトがなくなる）
