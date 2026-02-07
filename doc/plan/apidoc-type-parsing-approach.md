# 考察: apidoc パース時に C パーサーを活用するアプローチ

## 背景

現在の実装では、型推論時（`SemanticAnalyzer.parse_type_string()`）に C パーサーを
呼び出して apidoc の型文字列を解析している。

```
現在のフロー:
apidoc (embed.fnc)
    ↓ 文字列のまま保存
ApidocDict { args: [ApidocArg { ty: "COP* const", ... }] }
    ↓ 型推論時にパース
TypeRepr::from_c_type_string("COP* const", ...)
    ↓
TypeRepr::CType { specs: TypedefName(COP), derived: [Pointer], ... }
```

## 代替案: apidoc パース時に型を解析

```
代替フロー:
apidoc (embed.fnc)
    ↓ パース時に型文字列も解析
ApidocDict { args: [ApidocArg { ty: TypeRepr::CType { ... } }] }
    ↓ 型推論時はそのまま使用
（追加のパース不要）
```

## 技術的な課題

### 1. 依存関係の問題

C パーサー (`parse_type_from_string`) は以下を必要とする:

```rust
pub fn parse_type_from_string(
    type_str: &str,
    interner: &StringInterner,
    files: &FileRegistry,      // ← ヘッダー処理後に作成
    typedefs: &HashSet<InternedStr>,  // ← ヘッダーパース後に収集
) -> Result<TypeName>
```

apidoc のパースタイミング:
```
1. apidoc (embed.fnc) を読み込み    ← この時点では files/typedefs がない
2. xs-wrapper.h をプリプロセス      ← files が作成される
3. ヘッダーをパース                 ← typedefs が収集される
4. マクロの型推論                   ← ここで apidoc の型情報を使用
```

apidoc パース時には `files` と `typedefs` が存在しない。

### 2. 解決策の選択肢

#### 選択肢 A: 2パスアプローチ

```
1. apidoc を読み込み（型文字列は生のまま保存）
2. ヘッダー処理（files, typedefs を収集）
3. apidoc の型文字列を一括パース（TypeRepr に変換）
4. 型推論で使用
```

**メリット**:
- 1回のパースで済む（現在は使用箇所ごとにパース）
- エラーを早期に検出できる

**デメリット**:
- パイプラインが複雑になる
- apidoc の構造体を変更する必要がある（String → TypeRepr）

#### 選択肢 B: 遅延パース + キャッシュ

```rust
pub struct ApidocArg {
    pub name: String,
    pub ty_raw: String,                    // 生の文字列
    pub ty_parsed: OnceCell<TypeRepr>,     // 遅延パース結果をキャッシュ
}
```

**メリット**:
- 既存の apidoc パースロジックを変更不要
- 必要になった時点でパースし、結果をキャッシュ

**デメリット**:
- `OnceCell` の使用で内部可変性が必要
- パース時のコンテキスト（files, typedefs）の管理が複雑

#### 選択肢 C: typedef なしでパース

多くの型文字列は typedef を含まない基本型またはポインタ:
- `int`, `void`, `char *`, `const char *`
- `SV *`, `HV *`, `AV *` （これらは typedef）

typedef を未知として扱い、後で解決する:

```rust
pub enum CTypeSpecs {
    // ...
    TypedefName(InternedStr),     // 既知の typedef
    UnresolvedName(String),       // 未解決の名前（後で解決）
}
```

**メリット**:
- apidoc パース時に files/typedefs 不要
- パース自体は完了、解決は遅延

**デメリット**:
- 型表現が複雑になる
- 解決処理を別途実装する必要がある

## 現在の実装との比較

| 観点 | 現在の実装 | 選択肢 A (2パス) | 選択肢 B (遅延+キャッシュ) |
|------|------------|------------------|---------------------------|
| パース回数 | 使用ごと | 1回 | 1回（キャッシュ） |
| 実装の複雑さ | 低 | 中 | 中 |
| メモリ使用 | 低（文字列のみ） | 高（TypeRepr保存） | 中（遅延生成） |
| エラー検出 | 遅い（使用時） | 早い（一括パース時） | 遅い（使用時） |
| 変更範囲 | semantic.rs のみ | apidoc.rs, pipeline.rs | apidoc.rs |

## 現在の実装が妥当な理由

1. **シンプルさ**: 型文字列をそのまま保存し、必要時にパースする
2. **依存関係の回避**: apidoc モジュールが parser に依存しない
3. **パフォーマンス**: 型推論は一度だけ実行されるため、複数回パースのオーバーヘッドは限定的
4. **柔軟性**: パース方法を呼び出し側で制御できる

## 将来の改善の可能性

もしパフォーマンスが問題になった場合:

1. **選択肢 B の採用を検討**: 遅延パース + キャッシュで重複パースを排除
2. **パース結果のハッシュマップ**: 型文字列 → TypeRepr のキャッシュを `SemanticAnalyzer` に持つ

```rust
impl SemanticAnalyzer<'a> {
    type_cache: HashMap<String, TypeRepr>,

    fn parse_type_string_cached(&mut self, s: &str) -> TypeRepr {
        if let Some(cached) = self.type_cache.get(s) {
            return cached.clone();
        }
        let parsed = self.parse_type_string(s);
        self.type_cache.insert(s.to_string(), parsed.clone());
        parsed
    }
}
```

## 結論

現在の実装（型推論時にパース）は、以下の理由で妥当:

1. 依存関係の問題を自然に回避
2. 実装がシンプル
3. 実際のパフォーマンス影響は軽微

apidoc パース時に型を解析するアプローチは技術的に可能だが、
依存関係の問題を解決するための追加の複雑さが必要となる。
現時点では現在の実装を維持し、必要に応じてキャッシュ機構を追加するのが
良いトレードオフと考えられる。
