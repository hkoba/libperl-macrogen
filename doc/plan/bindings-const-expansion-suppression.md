# bindings.rs 定数によるオブジェクトマクロ展開抑制

## 目標

TokenExpander を改良し、オブジェクトマクロ展開時に bindings.rs に同名の定数が
定義されている場合、展開を抑制して定数名をそのまま出力する。
型情報は bindings.rs のものを使用する。

## 背景

### 現状の動作

例: `SvNOK(sv)` マクロの展開

```c
#define SvNOK(sv)    (SvFLAGS(sv) & SVf_NOK)
#define SVf_NOK      0x00000200  // = 512
```

現在の出力:
```
(&
  (call (ident SvFLAGS) ...)
  (int 512) :type int)  // ← 512 に展開されている
```

### 期待する動作

bindings.rs に `pub const SVf_NOK: u32 = 512;` がある場合:
```
(&
  (call (ident SvFLAGS) ...)
  (ident SVf_NOK) :type u32)  // ← 定数名のまま、型は bindings.rs から
```

### 利点

- Rust コード生成時に定数名をそのまま使える
- 型情報が正確（bindings.rs の型を継承）
- マジックナンバーではなく意味のある名前が残る

## 調査結果

### TokenExpander の no_expand メカニズム

`src/token_expander.rs`:
- `no_expand: HashSet<InternedStr>` - 展開抑制リスト
- `add_no_expand()` / `extend_no_expand()` で追加可能
- 展開判定で**最初にチェック**される（最高優先度）

```rust
// expand_internal() 内
if self.no_expand.contains(id) {
    result.push(token.clone());  // 展開せずそのまま
    continue;
}
```

### RustDeclDict の定数情報

`src/rust_decl.rs`:
- `consts: HashMap<String, RustConst>` - 定数名→型情報
- `lookup_const(name: &str) -> Option<&RustConst>`
- `RustConst { name: String, ty: String }`

### macro_infer.rs での利用状況

- `analyze_macro()` に `rust_decl_dict: Option<&RustDeclDict>` が渡されている
- TokenExpander 作成時（L299-301）には未使用
- SemanticAnalyzer には渡されている

## 実装計画

### Step 1: KeySet trait を定義

**src/token_expander.rs:**

```rust
/// キーの存在チェックのみを抽象化する trait
///
/// HashMap<String, V> の値の型を隠蔽し、キー検索のみを公開する。
pub trait KeySet {
    fn contains(&self, key: &str) -> bool;
}

// HashMap<String, V> に対する汎用実装
impl<V> KeySet for std::collections::HashMap<String, V> {
    fn contains(&self, key: &str) -> bool {
        self.contains_key(key)
    }
}
```

### Step 2: TokenExpander に KeySet の trait object を保持

**src/token_expander.rs:**

```rust
pub struct TokenExpander<'a> {
    macro_table: &'a MacroTable,
    interner: &'a StringInterner,
    files: &'a FileRegistry,
    no_expand: HashSet<InternedStr>,
    emit_markers: bool,
    // 追加: bindings 定数名のキーセット（値の型は隠蔽）
    bindings_consts: Option<&'a dyn KeySet>,
}

impl<'a> TokenExpander<'a> {
    pub fn new(...) -> Self {
        Self {
            // ... 既存フィールド ...
            bindings_consts: None,
        }
    }

    /// bindings.rs の定数名セットを設定
    pub fn set_bindings_consts(&mut self, consts: &'a dyn KeySet) {
        self.bindings_consts = Some(consts);
    }
}
```

### Step 3: expand_internal() で KeySet を参照

**src/token_expander.rs** の `expand_internal()`:

```rust
TokenKind::Ident(id) => {
    // 展開禁止リストにあればそのまま
    if self.no_expand.contains(id) {
        result.push(token.clone());
        continue;
    }

    // 追加: bindings.rs に定数として存在すればそのまま
    if let Some(consts) = self.bindings_consts {
        let name_str = self.interner.get(*id);
        if consts.contains(name_str) {
            result.push(token.clone());
            continue;
        }
    }

    // 再帰防止
    if visited.contains(id) {
        // ...
    }
    // ...
}
```

### Step 4: macro_infer.rs で bindings 定数を設定

**src/macro_infer.rs (L299-301 付近):**

```rust
// 変更前
let expander = TokenExpander::new(macro_table, interner, files);
let expanded_tokens = expander.expand(&def.body);

// 変更後
let mut expander = TokenExpander::new(macro_table, interner, files);
if let Some(dict) = rust_decl_dict {
    expander.set_bindings_consts(&dict.consts);  // HashMap<String, RustConst> → &dyn KeySet
}
let expanded_tokens = expander.expand(&def.body);
```

**利点**:
- TokenExpander は RustConst の存在を知らない
- trait object により値の型を完全に隠蔽
- `consts.contains()` は O(1) のハッシュルックアップ

### Step 5: SemanticAnalyzer で定数の型を解決

**src/semantic.rs** の `collect_expr_constraints()` (L1364付近):

現在、識別子の型解決は以下の順序:
1. シンボルテーブル (`lookup_symbol`)
2. 特殊ケース (`my_perl`)

**rust_decl_dict.consts からの定数型解決を追加する:**

```rust
ExprKind::Ident(name) => {
    let name_str = self.interner.get(*name);

    // シンボルテーブルから型を取得
    if let Some(sym) = self.lookup_symbol(*name) {
        let ty_str = sym.ty.display(self.interner);
        type_env.add_constraint(TypeEnvConstraint::new(
            expr.id, &ty_str, ConstraintSource::Inferred, "symbol lookup"
        ));
    // 追加: RustDeclDict から定数の型を取得
    } else if let Some(rust_decl_dict) = self.rust_decl_dict {
        if let Some(rust_const) = rust_decl_dict.lookup_const(name_str) {
            type_env.add_constraint(TypeEnvConstraint::new(
                expr.id, &rust_const.ty, ConstraintSource::RustBindings, "bindings constant"
            ));
        }
    } else if name_str == "my_perl" {
        // ...
    }
}
```

**注意**: `ConstraintSource::RustBindings` は既に定義済み。

## 修正対象ファイル

1. **src/token_expander.rs**
   - `KeySet` trait 定義を追加
   - `impl<V> KeySet for HashMap<String, V>` を追加
   - `bindings_consts: Option<&'a dyn KeySet>` フィールド追加
   - `set_bindings_consts()` メソッド追加
   - `expand_internal()` に bindings 定数チェックを追加

2. **src/macro_infer.rs** (L299-301付近)
   - TokenExpander 作成後に `set_bindings_consts(&dict.consts)` 呼び出し

3. **src/semantic.rs** (L1364付近の `collect_expr_constraints`)
   - `ExprKind::Ident` 処理に `rust_decl_dict.lookup_const()` による定数型解決を追加

## 期待される結果

変更前:
```
SvNOK: expression (4 constraints, 1 uses)
  (&
    (call (ident SvFLAGS) :type <unknown> ...)
    (int 512) :type int) :type int
```

変更後:
```
SvNOK: expression (4 constraints, 1 uses)
  (&
    (call (ident SvFLAGS) :type <unknown> ...)
    (ident SVf_NOK) :type u32) :type u32
```

## 注意点

1. **型の隠蔽**: KeySet trait により TokenExpander は RustConst を知らない
2. **型解決の二段階**: 展開抑制は TokenExpander、型付けは SemanticAnalyzer
3. **チェック順序**: no_expand → bindings_consts → マクロ展開
