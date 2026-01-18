# on_comment コールバック導入計画

## 背景・問題

現在の `ApidocCollector` は `MacroDefCallback::on_macro_defined()` で呼ばれ、
`MacroDef.leading_comments` のみを見ている。

しかし、Perl ヘッダーでは apidoc コメントがマクロ定義から離れた場所にあることがある：

```c
// Line 472: apidoc コメント
=for apidoc Am|const char *|CopFILE|const COP * c

// ... 121 行の間隔 ...

// Line 593: マクロ定義
#define CopFILE(c)		((c)->cop_file)
```

このため、`CopFILE` の apidoc が収集されない。

## 解決方針

Preprocessor に `on_comment` コールバックフックを導入し、
**`is_target` なヘッダーファイルのコメント**を収集できるようにする。

`ApidocCollector` は `on_comment` を実装し、
`=for apidoc` を含むコメントを見つけたら即座に辞書に登録する。

## 実装計画

### Phase 1: CommentCallback トレイト定義

**ファイル**: `src/preprocessor.rs`

```rust
/// コメントが読み込まれたときに呼ばれるコールバック
pub trait CommentCallback {
    /// コメントが読み込まれたときに呼ばれる
    ///
    /// - `comment`: コメント内容
    /// - `file_id`: ファイルID
    /// - `is_target`: このファイルが解析対象（samples/wrapper.h からの include）かどうか
    fn on_comment(&mut self, comment: &Comment, file_id: FileId, is_target: bool);

    /// ダウンキャスト用に Any に変換
    fn into_any(self: Box<Self>) -> Box<dyn Any>;
}
```

### Phase 2: Preprocessor へのコールバック追加

**ファイル**: `src/preprocessor.rs`

```rust
pub struct Preprocessor {
    // ... 既存フィールド ...

    /// コメントコールバック
    comment_callback: Option<Box<dyn CommentCallback>>,
}

impl Preprocessor {
    /// コメントコールバックを設定
    pub fn set_comment_callback(&mut self, callback: Box<dyn CommentCallback>) {
        self.comment_callback = Some(callback);
    }

    /// コメントコールバックを取り出す
    pub fn take_comment_callback(&mut self) -> Option<Box<dyn CommentCallback>> {
        self.comment_callback.take()
    }
}
```

### Phase 3: コメントスキャン時にコールバック呼び出し

**ファイル**: `src/preprocessor.rs`

`scan_line_comment()` と `scan_block_comment()` でコメントを生成した後、
`is_target` なファイルの場合のみコールバックを呼び出す：

```rust
fn scan_line_comment(&mut self) -> Comment {
    // ... 既存のコメントスキャン処理 ...
    let comment = Comment::new(CommentKind::Line, text, loc);

    // コールバック呼び出し（is_target なファイルのみ）
    if let Some(cb) = &mut self.comment_callback {
        let source = self.sources.last().unwrap();
        let file_id = source.file_id;
        let is_target = source.is_target;
        if is_target {
            cb.on_comment(&comment, file_id, is_target);
        }
    }

    comment
}

fn scan_block_comment(&mut self) -> Result<Comment, CompileError> {
    // ... 既存のコメントスキャン処理 ...
    let comment = Comment::new(CommentKind::Block, text, loc);

    // コールバック呼び出し（is_target なファイルのみ）
    if let Some(cb) = &mut self.comment_callback {
        let file_id = source.file_id;
        let is_target = source.is_target;
        if is_target {
            cb.on_comment(&comment, file_id, is_target);
        }
    }

    Ok(comment)
}
```

### Phase 4: ApidocCollector の変更

**ファイル**: `src/apidoc.rs`

`ApidocCollector` を `CommentCallback` を実装するように変更：

```rust
impl CommentCallback for ApidocCollector {
    fn on_comment(&mut self, comment: &Comment, _file_id: FileId, _is_target: bool) {
        // コメント内の各行を処理
        // （is_target チェックは呼び出し側で行われるため、ここでは常に処理）
        for line in comment.text.lines() {
            if let Some(entry) = ApidocEntry::parse_apidoc_line(line) {
                self.entries.insert(entry.name.clone(), entry);
            }
        }
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}
```

`MacroDefCallback` の実装は削除する。

### Phase 5: infer_api.rs の変更

**ファイル**: `src/infer_api.rs`

`set_macro_def_callback` の代わりに `set_comment_callback` を使用：

```rust
// 変更前
pp.set_macro_def_callback(Box::new(ApidocCollector::new()));

// 変更後
pp.set_comment_callback(Box::new(ApidocCollector::new()));
```

取り出しも同様に変更：

```rust
// 変更前
let callback = pp.take_macro_def_callback().expect("callback should exist");
let apidoc_collector = callback
    .into_any()
    .downcast::<ApidocCollector>()
    .expect("callback type mismatch");

// 変更後
let callback = pp.take_comment_callback().expect("callback should exist");
let apidoc_collector = callback
    .into_any()
    .downcast::<ApidocCollector>()
    .expect("callback type mismatch");
```

### Phase 6: lib.rs のエクスポート更新

**ファイル**: `src/lib.rs`

`CommentCallback` をエクスポートに追加。

## 変更対象ファイル一覧

| ファイル | 変更内容 |
|----------|----------|
| `src/preprocessor.rs` | `CommentCallback` トレイト定義、フィールド追加、setter/getter、コールバック呼び出し |
| `src/apidoc.rs` | `ApidocCollector` に `CommentCallback` 実装追加、`MacroDefCallback` 実装削除 |
| `src/infer_api.rs` | `set_comment_callback` / `take_comment_callback` 使用に変更 |
| `src/lib.rs` | `CommentCallback` エクスポート追加 |

## テスト計画

1. 既存のテストが通ることを確認 ✅
2. `CopFILE` の apidoc が収集されることを確認 ✅
   - 以前: `apidoc.get(CopFILE) = None`
   - 修正後: `apidoc.get(CopFILE) = Some(ApidocEntry { return_type: Some("const char *"), ... })`
3. コメントから収集された apidoc 数が増加することを確認 ✅

## 実装完了日

2026-01-18

## 注意事項

- `MacroDefCallback` は `ApidocCollector` 以外にも使われている可能性がある
  - `ThxCollector` が使用している → これは `MacroDefCallback` のまま残す
- 両方のコールバックを設定できるようにする（既存の `macro_def_callback` は残す）
