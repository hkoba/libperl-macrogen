# apidoc の `type`/`cast` パラメータによるジェネリック関数生成

## 目的

apidoc で引数・戻り値として `type` または `cast` キーワードが出現するマクロ関数を、
Rust のジェネリック関数として生成する。

## 例

### 入力 (apidoc)
```
=for apidoc Cyh|type|NUM2PTR|type|int value
```

### C マクロ定義
```c
#define NUM2PTR(any,d)  (any)(PTRV)(d)
#define PTRV            unsigned long
```

### 期待する Rust 出力
```rust
/// NUM2PTR - macro function
#[inline]
pub unsafe fn NUM2PTR<T>(value: c_int) -> T {
    // as キャストで可能な範囲で変換、無理な場合は incomplete
    (value as PTRV) as T  // ← これは実際にはコンパイルエラーになる可能性あり
}
```

## 設計方針

1. **戻り値型**: apidoc の `type` をそのまま `T` としてジェネリック型パラメータにする
2. **関数本体**: `as` キャストのみ使用。不可能な場合は `CODEGEN_INCOMPLETE` とする
3. **複数の type/cast パラメータ**: 出現順に `T`, `U`, `V` ... を割り当てる
4. **対象キーワード**: `type` と `cast` の両方を型パラメータとして扱う

## 実装計画

### Step 1: ApidocEntry に type/cast パラメータ検出機能を追加

**ファイル**: `src/apidoc.rs`

ApidocEntry に以下のメソッドを追加:

```rust
impl ApidocEntry {
    /// 型パラメータキーワードかどうかを判定
    fn is_type_param_keyword(ty: &str) -> bool {
        ty == "type" || ty == "cast"
    }

    /// `type`/`cast` パラメータのインデックスを返す
    pub fn type_param_indices(&self) -> Vec<usize> {
        self.args.iter()
            .enumerate()
            .filter(|(_, arg)| Self::is_type_param_keyword(&arg.ty))
            .map(|(i, _)| i)
            .collect()
    }

    /// 戻り値型が `type` かどうか
    pub fn returns_type_param(&self) -> bool {
        self.return_type.as_ref().map_or(false, |t| Self::is_type_param_keyword(t))
    }

    /// ジェネリック関数として生成すべきか
    pub fn is_generic(&self) -> bool {
        self.returns_type_param() || !self.type_param_indices().is_empty()
    }
}
```

### Step 2: MacroInferInfo にジェネリック情報を追加

**ファイル**: `src/macro_infer.rs`

MacroInferInfo に以下のフィールドを追加:

```rust
pub struct MacroInferInfo {
    // ... existing fields ...

    /// ジェネリック型パラメータ情報
    /// key: パラメータインデックス (または -1 for return type)
    /// value: 型パラメータ名 ("T", "U", etc.)
    pub generic_type_params: HashMap<i32, String>,
}
```

### Step 3: 型推論時にジェネリック情報を収集

**ファイル**: `src/macro_infer.rs` または `src/semantic.rs`

apidoc からマクロ情報を読み込む際に、`type`/`cast` パラメータを検出し、
`generic_type_params` に記録する:

```rust
fn collect_generic_params(entry: &ApidocEntry, info: &mut MacroInferInfo) {
    let param_names = ['T', 'U', 'V', 'W', 'X', 'Y', 'Z'];
    let mut param_idx = 0;

    // パラメータの type/cast を収集
    for (i, arg) in entry.args.iter().enumerate() {
        if ApidocEntry::is_type_param_keyword(&arg.ty) {
            let name = param_names[param_idx].to_string();
            info.generic_type_params.insert(i as i32, name);
            param_idx += 1;
        }
    }

    // 戻り値型の type を収集
    if entry.returns_type_param() {
        // 戻り値型がパラメータと同じ type を参照している場合は同じ名前を使う
        // そうでなければ新しい名前を割り当て
        let name = if param_idx == 0 {
            "T".to_string()
        } else {
            param_names[param_idx].to_string()
        };
        info.generic_type_params.insert(-1, name);  // -1 = return type
    }
}
```

**例: Newxc の場合**
```
=for apidoc Am|void|Newxc|void* ptr|int nitems|type|cast
```
- `type` (index 2) → `T`
- `cast` (index 3) → `U`
- 生成: `pub unsafe fn Newxc<T, U>(ptr: *mut c_void, nitems: c_int) -> ()`

### Step 4: RustCodegen でジェネリック関数を生成

**ファイル**: `src/rust_codegen.rs`

#### 4.1: ジェネリック句の生成

```rust
fn build_generic_clause(&self, info: &MacroInferInfo) -> String {
    if info.generic_type_params.is_empty() {
        return String::new();
    }

    // 型パラメータを収集（重複排除、ソート）
    let mut params: Vec<&String> = info.generic_type_params.values().collect();
    params.sort();
    params.dedup();

    format!("<{}>", params.join(", "))
}
```

#### 4.2: パラメータ型の取得を修正

`get_param_type` を修正して、ジェネリック型パラメータを考慮:

```rust
fn get_param_type(&mut self, param: &MacroParam, info: &MacroInferInfo, param_index: usize) -> String {
    // ジェネリック型パラメータかチェック
    if let Some(generic_name) = info.generic_type_params.get(&(param_index as i32)) {
        return generic_name.clone();
    }

    // 既存のロジック...
}
```

#### 4.3: 戻り値型の取得を修正

`get_return_type` を修正:

```rust
fn get_return_type(&mut self, info: &MacroInferInfo) -> String {
    // ジェネリック戻り値型かチェック
    if let Some(generic_name) = info.generic_type_params.get(&-1) {
        return generic_name.clone();
    }

    // 既存のロジック...
}
```

#### 4.4: generate_macro の修正

```rust
pub fn generate_macro(mut self, info: &MacroInferInfo) -> GeneratedCode {
    let name_str = self.interner.get(info.name);

    // ジェネリック句を生成
    let generic_clause = self.build_generic_clause(info);

    // パラメータリストを構築（型情報付き）
    let params_with_types = self.build_param_list(info);

    // 戻り値の型を取得
    let return_type = self.get_return_type(info);

    // ... THX handling ...

    // 関数定義（ジェネリック句付き）
    self.writeln(&format!(
        "pub unsafe fn {}{}({}) -> {} {{",
        name_str, generic_clause, params_str, return_type
    ));

    // ...
}
```

### Step 5: build_param_list の修正

`build_param_list` を修正して、パラメータインデックスを渡す:

```rust
fn build_param_list(&mut self, info: &MacroInferInfo) -> String {
    info.params.iter()
        .enumerate()
        .filter(|(i, _)| {
            // type パラメータ自体は関数引数から除外
            // (type は型パラメータであり、値引数ではない)
            !info.generic_type_params.contains_key(&(*i as i32))
        })
        .map(|(i, p)| {
            let name = escape_rust_keyword(self.interner.get(p.name));
            let ty = self.get_param_type(p, info, i);
            format!("{}: {}", name, ty)
        })
        .collect::<Vec<_>>()
        .join(", ")
}
```

**重要**: `type` パラメータは「値」ではなく「型」を渡すため、
Rust の関数引数からは除外する必要がある。

NUM2PTR(type, int value) の場合:
- C: `NUM2PTR(SV*, 42)` - 第1引数で型を指定
- Rust: `NUM2PTR::<*mut SV>(42)` - 型パラメータで指定

### Step 6: テスト

1. `cargo build` でビルド確認
2. `cargo test` で既存テストが通ることを確認
3. NUM2PTR が以下のように生成されることを確認:

```rust
/// NUM2PTR - macro function
#[inline]
pub unsafe fn NUM2PTR<T>(value: c_int) -> T {
    // 本体は as キャストで生成を試みる
    // 無理な場合は CODEGEN_INCOMPLETE
}
```

## 変更対象ファイル

| ファイル | 変更内容 |
|----------|----------|
| `src/apidoc.rs` | `is_type_param_keyword()`, `type_param_indices()`, `returns_type_param()`, `is_generic()` 追加 |
| `src/macro_infer.rs` | `MacroInferInfo` に `generic_type_params` 追加 |
| `src/semantic.rs` | ジェネリック情報収集 |
| `src/rust_codegen.rs` | ジェネリック関数生成 |

## 注意点

1. **type/cast パラメータは値引数ではない**
   - C マクロでは `NUM2PTR(SV*, 42)` のように型を第1引数で渡す
   - Rust では `NUM2PTR::<*mut SV>(42)` のように型パラメータとして渡す
   - したがって、`type`/`cast` パラメータは関数の値引数リストから除外する

2. **複数の type/cast パラメータ**
   - `Newxc(void* ptr, int nitems, type, cast)` のように複数の type/cast がある場合
   - それぞれ別の型パラメータ `T`, `U` として扱う

3. **本体の生成**
   - `as` キャストで変換を試みる
   - ジェネリック型 T への as キャストは多くの場合コンパイルエラーになる
   - その場合は `CODEGEN_INCOMPLETE` としてコメントアウト

4. **戻り値型が type で、パラメータにも type がある場合**
   - NUM2PTR のように、戻り値型がパラメータの type と同じ場合は同じ型パラメータ名を使用

5. **対象キーワード**
   - `type`: 型を表すパラメータ
   - `cast`: キャスト先の型を表すパラメータ
   - 両方とも同じようにジェネリック型パラメータとして扱う
