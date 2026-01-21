# マクロ定数名の保持（実装完了）

## 問題

inline 関数のコード生成で、C のマクロ定数が数値リテラルに展開されてしまう。

### 例: SvFLAGS

```c
// C の定義
#define SVTYPEMASK  0xff
#define SVpgv_GP    0x00008000
#define SVf_FAKE    0x01000000

// inline 関数内
((*sv).sv_flags & SVTYPEMASK)
```

```rust
// 以前の出力
((*sv).sv_flags & 255)

// 改善後の出力
((*sv).sv_flags & SVTYPEMASK)
```

## 解決策

プリプロセッサレベルで、bindings.rs に存在する定数名のマクロ展開を抑制する。

## 実装内容

### 1. Preprocessor に展開抑制機能を追加

**ファイル**: `src/preprocessor.rs`

```rust
pub struct Preprocessor {
    // ... 既存フィールド ...
    /// グローバルな展開抑制マクロ名（bindings.rs の定数など）
    skip_expand_macros: HashSet<InternedStr>,
}

impl Preprocessor {
    /// 展開抑制マクロを追加
    pub fn add_skip_expand_macro(&mut self, name: InternedStr) {
        self.skip_expand_macros.insert(name);
    }

    /// 複数の展開抑制マクロを追加
    pub fn add_skip_expand_macros(&mut self, names: impl IntoIterator<Item = InternedStr>) {
        self.skip_expand_macros.extend(names);
    }
}
```

### 2. マクロ展開時のチェック

**ファイル**: `src/preprocessor.rs`

`try_expand_macro` の先頭でグローバルな展開抑制リストをチェック:

```rust
fn try_expand_macro(&mut self, id: InternedStr, token: &Token) -> Result<Option<Vec<Token>>, CompileError> {
    // グローバルな展開抑制リストをチェック（bindings.rs の定数など）
    if self.skip_expand_macros.contains(&id) {
        return Ok(None);
    }
    // ... 既存の処理
}
```

### 3. 型推論実行時に展開抑制を設定

**ファイル**: `src/infer_api.rs`

`run_inference_with_preprocessor` で bindings をパーサー作成前にロードし、
定数名を展開抑制に登録:

```rust
pub fn run_inference_with_preprocessor(...) -> Result<Option<InferResult>, InferError> {
    // RustDeclDict をロード（パーサー作成前に行い、展開抑制を設定）
    let rust_decl_dict = if let Some(path) = bindings_path {
        Some(RustDeclDict::parse_file(path)?)
    } else {
        None
    };

    // bindings.rs の定数名を展開抑制に登録
    if let Some(ref dict) = rust_decl_dict {
        for name in dict.consts.keys() {
            let interned = pp.interner_mut().intern(name);
            pp.add_skip_expand_macro(interned);
        }
    }

    // ... 以降は既存の処理
}
```

## 変更ファイル

| ファイル | 変更内容 |
|----------|----------|
| `src/preprocessor.rs` | `skip_expand_macros` フィールド追加、API 追加、`try_expand_macro` で展開抑制チェック |
| `src/infer_api.rs` | bindings のロードを前倒しし、定数名を展開抑制に登録 |

## 利点

- **シンプル**: プリプロセッサレベルでの抑制なので、コード生成時の逆引きより単純
- **正確**: 展開されないため、識別子としてパース・コード生成される
- **パフォーマンス**: HashSet の O(1) チェックのみ

## 使用方法

```bash
cargo run -- --auto --gen-rust samples/wrapper.h --bindings samples/bindings.rs
```

`--bindings` オプションで bindings.rs を指定すると、その中に定義されている定数名が
マクロ展開されずに保持される。
