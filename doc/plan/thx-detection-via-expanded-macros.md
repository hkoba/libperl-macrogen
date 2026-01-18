# THX 判定の改良: 展開マクロ追跡による推移的検出

## 問題

`SvENDx` マクロが THX 依存として判定されない。

```c
#define SvENDx(sv) ((PL_Sv = (sv)), SvEND(PL_Sv))
#define PL_Sv      (vTHX->ISv)
#define vTHX       aTHX
```

`PL_Sv` → `vTHX` → `aTHX` と推移的に THX 依存だが、検出されていない。

## 原因分析

`build_macro_info` の処理フロー:

1. **展開前の THX チェック** (931-939行目): `def.body` で `aTHX`, `tTHX`, `my_perl` を検索
2. **マクロ展開** (954行目): `TokenExpander::expand_with_calls` でオブジェクトマクロを展開
3. **uses 収集** (957行目): 展開**後**のトークンから使用マクロを収集

問題点:
- `PL_Sv` はオブジェクトマクロとして展開され、展開後のトークンには残らない
- `collect_uses` は展開後のトークンから `macro_table.get(id)` で存在確認
- `PL_Sv` や `vTHX` は展開済みなので `uses` に入らない
- `used_by` 関係が構築されず、推移閉包が機能しない

## 解決方針

### アイデア 1: TokenExpander で展開したマクロを追跡

`TokenExpander::expand_with_calls` は内部で `visited` を使って再帰を防止している。
この情報を外部に公開すれば、「どのマクロを展開したか」がわかる。

### アイデア 2: THX 判定を展開後に行う

- `aTHX`, `tTHX` → `uses` に含まれていれば THX 依存
- `my_perl` → 展開後トークンをスキャンして検出

## 実装計画

### Phase 1: TokenExpander の改良

`src/token_expander.rs` を修正:

```rust
pub struct TokenExpander<'a> {
    // 既存フィールド...

    /// 展開されたマクロの集合（展開プロセス中に記録）
    expanded_macros: HashSet<InternedStr>,
}

impl<'a> TokenExpander<'a> {
    pub fn new(...) -> Self {
        Self {
            // ...
            expanded_macros: HashSet::new(),
        }
    }

    /// 展開されたマクロの集合を取得
    pub fn expanded_macros(&self) -> &HashSet<InternedStr> {
        &self.expanded_macros
    }

    /// 展開状態をクリア（再利用時）
    pub fn clear_expanded(&mut self) {
        self.expanded_macros.clear();
    }

    /// トークン列をマクロ展開（展開マクロを記録）
    pub fn expand_with_calls(&mut self, tokens: &[Token]) -> Vec<Token> {
        self.expanded_macros.clear();
        self.expand_with_calls_internal(tokens)
    }

    fn expand_with_calls_internal(&mut self, tokens: &[Token]) -> Vec<Token> {
        // 展開時に self.expanded_macros.insert(*id) を呼ぶ
        // visited の代わりに self.expanded_macros を使用
        // ...
    }
}
```

**注意**: `expand_with_calls` のシグネチャが `&self` → `&mut self` に変更される。
呼び出し側の修正が必要。

### Phase 2: collect_uses の改良

`src/macro_infer.rs` を修正:

```rust
fn collect_uses(
    &self,
    expanded_macros: &HashSet<InternedStr>,  // TokenExpander から取得
    info: &mut MacroInferInfo,
) {
    for &id in expanded_macros {
        if id != info.name {
            info.add_use(id);
        }
    }
}
```

これにより:
- `PL_Sv` を展開したら `PL_Sv` が `uses` に入る
- `vTHX` を展開したら `vTHX` が `uses` に入る
- `aTHX` を展開したら `aTHX` が `uses` に入る

### Phase 3: THX 判定の改良

`build_macro_info` を修正:

```rust
// マクロ本体を展開（TokenExpander を使用）
let mut expander = TokenExpander::new(macro_table, interner, files);
// ... no_expand 設定 ...
let expanded_tokens = expander.expand_with_calls(&def.body);

// def-use 関係を収集（展開されたマクロから）
self.collect_uses(expander.expanded_macros(), &mut info);

// THX 判定: aTHX, tTHX が uses に含まれているか
let (sym_athx, sym_tthx, sym_my_perl) = thx_symbols;
let has_thx_from_uses = info.uses.contains(&sym_athx) || info.uses.contains(&sym_tthx);

// THX 判定: my_perl が展開後トークンに含まれているか
let has_my_perl = expanded_tokens.iter().any(|t| {
    matches!(t.kind, TokenKind::Ident(id) if id == sym_my_perl)
});

let has_thx = has_thx_from_uses || has_my_perl;
info.is_thx_dependent = has_thx;
```

### Phase 4: 呼び出し側の修正

`expand_with_calls` が `&mut self` になるため、以下を修正:
- `build_macro_info` 内の `expander` を `let mut` に

## 期待される効果

修正前:
```rust
/// SvENDx - macro function  // [THX] マーカーなし
pub unsafe fn SvENDx(sv: ...) -> ... { ... }
```

修正後:
```rust
/// SvENDx [THX] - macro function
pub unsafe fn SvENDx(my_perl: *mut PerlInterpreter, sv: ...) -> ... { ... }
```

## 考慮点

### 1. `visited` と `expanded_macros` の違い

現在の `visited` は再帰防止用で、展開完了後に `remove` される。
新しい `expanded_macros` は「展開したマクロの累積」なので `remove` しない。

```rust
// 現状
visited.insert(*id);
let expanded = self.expand_object_macro(def, token, visited);
visited.remove(id);  // ← これは不要になる

// 改良後
self.expanded_macros.insert(*id);
let expanded = self.expand_object_macro(def, token);
// remove しない（累積）
```

ただし再帰防止は依然として必要なので、別途ローカル変数で管理するか、
`expanded_macros` が既に含まれているかで再帰判定する。

### 2. 関数マクロの扱い

`expand_with_calls` は関数マクロも展開する。
関数マクロの引数内で使われたマクロも `expanded_macros` に含まれる。
これは意図通り。

### 3. `propagate_flag_via_used_by` との関係

現在のロジック:
1. 初期 THX 集合を構築（直接 THX トークンを含むマクロ）
2. `used_by` を辿って伝播

改良後:
- `aTHX` や `tTHX` が `uses` に含まれていれば直接 THX 依存
- 推移閉包の計算は不要になる可能性がある（展開時に既に推移的に解決されるため）

ただし、現状の推移閉包ロジックは残しておいても問題ない（冗長だが安全）。

## テスト計画

1. `SvENDx` が THX 依存として判定されることを確認
2. `PL_*` 系マクロ全般が THX 依存として判定されることを確認
3. THX に依存しないマクロは引き続き非 THX として判定されることを確認
