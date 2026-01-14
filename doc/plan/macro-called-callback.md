# マクロ呼び出しコールバック機能

## 目的

特定のマクロが呼び出されたときにコールバックを実行できるようにする。
主なユースケースは、Perl の SV ファミリー構造体の動的検出。

## 背景

現在、`fields_dict` では `SV_FAMILY_MEMBERS` がハードコードされている：

```rust
const SV_FAMILY_MEMBERS: &[&str] = &["SV", "AV", "HV", "CV", ...];
```

しかし、SV ファミリーは `_SV_HEAD(typeName)` マクロを使用して定義されている：

```c
struct sv {
    _SV_HEAD(SV);  // ← このマクロ呼び出しで SV ファミリーと判定可能
    // ...
};
```

マクロ呼び出しを検出することで、ハードコードを排除し、
将来の Perl バージョン変更にも対応可能にする。

## 設計

### MacroCalledCallback トレイト

```rust
/// マクロ呼び出し時のコールバックトレイト
/// 登録時に指定したマクロが呼び出されると実行される
pub trait MacroCalledCallback {
    /// マクロが呼び出され、展開された後に呼ばれる
    /// - args: 引数トークン列（関数形式マクロの場合）
    ///         オブジェクトマクロの場合は None
    /// - interner: トークンを文字列化するために使用
    fn on_macro_called(&mut self, args: Option<&[Vec<Token>]>, interner: &StringInterner);

    /// ダウンキャスト用
    fn as_any(&self) -> &dyn std::any::Any;
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
}
```

### MacroCallWatcher 実装

```rust
/// 特定マクロの呼び出しを監視するシンプルな実装
pub struct MacroCallWatcher {
    /// 呼び出しフラグ
    called: Cell<bool>,
    /// 最後に呼び出された引数（トークン列を文字列化）
    last_args: RefCell<Option<Vec<String>>>,
}

impl MacroCallWatcher {
    pub fn new() -> Self {
        Self {
            called: Cell::new(false),
            last_args: RefCell::new(None),
        }
    }

    /// フラグをチェックしてリセット
    pub fn take_called(&self) -> bool {
        self.called.replace(false)
    }

    /// 最後の引数を取得してクリア
    pub fn take_args(&self) -> Option<Vec<String>> {
        self.last_args.borrow_mut().take()
    }

    /// フラグをクリア（parse_each の前に呼ぶ）
    pub fn clear(&self) {
        self.called.set(false);
        *self.last_args.borrow_mut() = None;
    }
}

impl MacroCalledCallback for MacroCallWatcher {
    fn on_macro_called(&mut self, args: Option<&[Vec<Token>]>, interner: &StringInterner) {
        self.called.set(true);
        if let Some(args) = args {
            let strs: Vec<String> = args.iter()
                .map(|tokens| Self::tokens_to_string(tokens, interner))
                .collect();
            *self.last_args.borrow_mut() = Some(strs);
        }
    }

    fn as_any(&self) -> &dyn std::any::Any { self }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
}
```

### Preprocessor への統合

```rust
pub struct Preprocessor<'a> {
    // ... 既存フィールド ...

    /// マクロ名 → コールバック のマップ
    macro_called_callbacks: HashMap<InternedStr, Box<dyn MacroCalledCallback>>,
}

impl<'a> Preprocessor<'a> {
    /// 特定マクロの呼び出しコールバックを設定
    pub fn set_macro_called_callback(
        &mut self,
        macro_name: InternedStr,
        callback: Box<dyn MacroCalledCallback>,
    ) {
        self.macro_called_callbacks.insert(macro_name, callback);
    }

    /// マクロ呼び出しコールバックを取得（所有権移動）
    pub fn take_macro_called_callback(
        &mut self,
        macro_name: InternedStr,
    ) -> Option<Box<dyn MacroCalledCallback>> {
        self.macro_called_callbacks.remove(&macro_name)
    }

    /// マクロ呼び出しコールバックへの参照を取得
    pub fn get_macro_called_callback(
        &self,
        macro_name: InternedStr,
    ) -> Option<&dyn MacroCalledCallback> {
        self.macro_called_callbacks.get(&macro_name).map(|b| b.as_ref())
    }

    /// マクロ呼び出しコールバックへの可変参照を取得
    pub fn get_macro_called_callback_mut(
        &mut self,
        macro_name: InternedStr,
    ) -> Option<&mut dyn MacroCalledCallback> {
        self.macro_called_callbacks.get_mut(&macro_name).map(|b| b.as_mut())
    }
}
```

### try_expand_macro での呼び出し

```rust
// try_expand_macro 内、展開完了後に追加

// コールバックがあれば呼び出す（展開後）
if let Some(cb) = self.macro_called_callbacks.get_mut(&id) {
    match &def.kind {
        MacroKind::Object => {
            cb.on_macro_called(None);
        }
        MacroKind::Function { .. } => {
            // args は collect_macro_args で収集済み
            cb.on_macro_called(Some(&args));
        }
    }
}
```

## 使用例：SV ファミリー検出

```rust
// 1. コールバックを登録
let sv_head_id = interner.intern("_SV_HEAD");
pp.set_macro_called_callback(sv_head_id, Box::new(MacroCallWatcher::new()));

// 2. 構造体定義をパース
for decl in declarations {
    if let Some(struct_decl) = decl.as_struct() {
        let struct_name = struct_decl.name.clone();

        // パース前にフラグをクリア
        if let Some(cb) = pp.get_macro_called_callback_mut(sv_head_id) {
            if let Some(watcher) = cb.as_any_mut().downcast_mut::<MacroCallWatcher>() {
                watcher.clear();
            }
        }

        // 構造体本体のパース（_SV_HEAD マクロが展開される）
        parse_struct_body(&mut pp, ...);

        // パース後にフラグをチェック
        if let Some(cb) = pp.get_macro_called_callback(sv_head_id) {
            if let Some(watcher) = cb.as_any().downcast_ref::<MacroCallWatcher>() {
                if watcher.take_called() {
                    // SV ファミリーとして記録
                    sv_family_members.push(struct_name);

                    // 引数も取得可能（将来の拡張用）
                    if let Some(args) = watcher.take_args() {
                        // args[0] = "SV" など
                    }
                }
            }
        }
    }
}

// 3. 検出結果を使用
println!("SV family members: {:?}", sv_family_members);
```

## 実装手順

### Phase 1: Preprocessor への機能追加 ✅ 完了

1. ✅ `MacroCalledCallback` トレイトを定義
2. ✅ `MacroCallWatcher` 構造体を実装
3. ✅ `Preprocessor` に `macro_called_callbacks` フィールドを追加
4. ✅ `set_macro_called_callback`, `take_macro_called_callback` 等を実装
5. ✅ `try_expand_macro` でコールバック呼び出しを追加

**実装時の変更点**:
- `on_macro_called` に `interner: &StringInterner` 引数を追加
  （トークンを文字列化するため）
- `get_macro_called_callback` の戻り値を `Option<&Box<dyn MacroCalledCallback>>` に変更
  （ライフタイムの問題を回避）

### Phase 2: fields_dict での使用 ✅ 完了

1. ✅ `_SV_HEAD` 監視用の `MacroCallWatcher` を登録
2. ✅ 構造体パース時にフラグをチェック
3. ✅ `SV_FAMILY_MEMBERS` ハードコードを動的検出に置き換え

**実装の詳細**:
- `FieldsDict` に `sv_family_members: HashSet<InternedStr>` フィールドを追加
- `add_sv_family_member()` メソッドを追加
- `is_sv_family()` を動的検出のみを使用するよう変更（フォールバックなし）
  - SV ファミリー検出失敗は重大な問題のため、フォールバックで隠蔽しない
- `Parser` に `parse_each_with_pp()` メソッドを追加（Preprocessor へのアクセス付き）
- `run_infer_macro_types()` で `_SV_HEAD` 検出ロジックを実装

### Phase 3: テストと検証 ✅ 完了

1. ✅ 単体テスト追加
   - `test_macro_call_watcher_basic` - 基本機能テスト
   - `test_macro_call_watcher_object_macro` - オブジェクトマクロ検出
   - `test_macro_call_watcher_function_macro` - 関数マクロ検出と引数取得
   - `test_macro_call_watcher_clear` - clear() メソッド
   - `test_macro_call_watcher_take_called` - take_called() メソッド
   - `test_macro_call_watcher_multiple_macros` - 複数マクロ監視
   - `test_macro_call_watcher_sv_head_pattern` - _SV_HEAD パターンシミュレーション
2. ✅ 実際の Perl ヘッダーでの動作確認
   - 検出された SV ファミリー: sv, gv, cv, av, hv, io, p5rx, invlist, object

## 変更対象ファイル

- `src/preprocessor.rs` - MacroCalledCallback トレイト、Preprocessor への統合
- `src/fields_dict.rs` - SV ファミリー動的検出（Phase 2）

## 備考

- コールバックは展開**後**に呼ばれるため、引数のトークン列はそのまま利用可能
- `Cell`/`RefCell` を使用することで、`&self` 経由でもフラグ操作が可能
- 将来的に複数のマクロを監視する場合も、同じ仕組みで対応可能
