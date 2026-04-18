# Plan: perl5 共通フィールドマクロの一級概念化（B 段階分割）

## Context

直接の動機は `xcv_xsub` のような関数ポインタフィールド呼び出しの正しい
Rust codegen だが、根本にあるのは perl5 ヘッダの **「マクロを介した共通
フィールド宣言」** （`_SV_HEAD` / `_XPV_HEAD` / `_XPVCV_COMMON` 等）
を本プロジェクトの一級概念として扱うこと。`_SV_HEAD` には既に
`MacroCallWatcher` 経由のサポートがあるので、その同型を `_XPV_HEAD` /
`_XPVCV_COMMON` にも与え、最終的にフィールドアクセスの型情報を C
ソース側のマクロ定義から canonical に取得できるようにする。

サポートする情報を一段ずつ厚くする「B 段階分割」で進める:

| Step | 達成 | 主な変更 |
|------|------|---------|
| **B-1** | 共通フィールドマクロ呼び出しの観測 | `src/infer_api.rs`, `src/fields_dict.rs` |
| **B-2** | マクロ本体の解析と canonical field set 抽出 | `src/fields_dict.rs`, 必要に応じて `src/parser.rs` |
| **B-3** | codegen を `field → defining_macro → canonical type` 経由に切替し fn ポインタ呼び出しを正しく出力 | `src/rust_codegen.rs` |

各 Step は独立にビルド可能で価値を出せる。

観測対象マクロは perl5 専用ハードコード:
- `_SV_HEAD` (既存、変更なし)
- `_XPV_HEAD`
- `_XPVCV_COMMON`

将来 `_XPVHV_COMMON` 等の追加が必要になれば同所に列挙を増やす。

## Step B-1: 共通フィールドマクロ呼び出しの観測

### 変更ファイル

- `src/fields_dict.rs`
- `src/infer_api.rs`

### 変更内容

**`FieldsDict` に追加するフィールド** (`src/fields_dict.rs:25-40` 周辺):

```rust
/// 構造体 → そこで展開された共通フィールドマクロ集合
struct_to_common_macros: HashMap<InternedStr, Vec<InternedStr>>,
/// 共通フィールドマクロ → それを使う構造体集合
common_macro_to_structs: HashMap<InternedStr, Vec<InternedStr>>,
```

公開アクセサ:

```rust
pub fn add_struct_uses_common_macro(&mut self, struct_name: InternedStr, macro_name: InternedStr);
pub fn structs_using_common_macro(&self, macro_name: InternedStr) -> &[InternedStr];
pub fn common_macros_used_by_struct(&self, struct_name: InternedStr) -> &[InternedStr];
```

**`src/infer_api.rs` の Watcher 登録** (現行 line 245-253 周辺):

```rust
let sv_head_id = pp.interner_mut().intern("_SV_HEAD");
pp.set_macro_called_callback(sv_head_id, Box::new(MacroCallWatcher::new()));
let xpv_head_id = pp.interner_mut().intern("_XPV_HEAD");
pp.set_macro_called_callback(xpv_head_id, Box::new(MacroCallWatcher::new()));
let xpvcv_common_id = pp.interner_mut().intern("_XPVCV_COMMON");
pp.set_macro_called_callback(xpvcv_common_id, Box::new(MacroCallWatcher::new()));
// ... 既存の pthx_id 等
```

ハードコードされたマクロ ID 集合を `static` 配列にしておき、新規追加は
1 行追加で済むようにする:

```rust
const COMMON_FIELD_MACROS: &[&str] = &[
    "_SV_HEAD",       // 既存（fields_dict.add_sv_family_member_with_type も並行）
    "_XPV_HEAD",
    "_XPVCV_COMMON",
];
```

**struct 通過時の検出** (現行 line 301-321 周辺):

`_SV_HEAD` を扱う既存ロジックの隣で、`_XPV_HEAD` / `_XPVCV_COMMON` に
ついても Watcher のフラグをチェックし、立っていれば
`fields_dict.add_struct_uses_common_macro(struct_name, macro_id)` を呼ぶ。
`_SV_HEAD` のみ持つ既存処理（typedef 引数の登録）は変更しない。

### 検証

- `cargo test`（全 350 テスト）が pass。
- 既存の sv_family / FieldsDict 関連テストに regression なし。
- 手動確認: dump_types で `xpvcv` / `xpvfm` が `_XPVCV_COMMON` を、
  `xpv*` 系が `_XPV_HEAD` を使用していることが何らかの形で確認できる
  （unit test を追加しても良い）。

## Step B-2: マクロ本体パースと canonical field set 抽出

### 変更ファイル

- `src/fields_dict.rs`
- 必要に応じて `src/infer_api.rs`（マクロ本体取得経路）

### 変更内容

**`FieldsDict` 拡張**:

```rust
pub struct CommonFieldMacro {
    pub name: InternedStr,             // "_XPVCV_COMMON"
    pub fields: Vec<CommonField>,      // 順序保持
}
pub struct CommonField {
    pub name: InternedStr,             // "xcv_xsub"
    pub ty: TypeRepr,                  // void(*)(pTHX_ CV*)
    pub origin: CommonFieldOrigin,
}
pub enum CommonFieldOrigin {
    Direct,                                   // 直接のフィールド
    InsideUnion { union_field: InternedStr }, // union 内
}

pub struct FieldsDict {
    // ... existing
    common_macros: HashMap<InternedStr, CommonFieldMacro>,
    field_to_defining_macro: HashMap<InternedStr, InternedStr>,
}
```

公開アクセサ:

```rust
pub fn defining_macro_of(&self, field_name: InternedStr) -> Option<InternedStr>;
pub fn common_macro(&self, macro_name: InternedStr) -> Option<&CommonFieldMacro>;
pub fn canonical_field(&self, field_name: InternedStr) -> Option<(&CommonFieldMacro, &CommonField)>;
```

**マクロ本体の取得とパース**:

`MacroDefCallback` で `_XPVCV_COMMON` 等のマクロ定義を捕捉、または
`pp.macros()` 経由で `MacroDef` を取得できる。トークン列を
`parser.rs` の struct member 宣言パーサ（既存）に投入し、
`Vec<StructMember>` AST を取得する。

union 内のフィールド（例: `xcv_root_u` の中の `xcv_xsub`）は
`CommonFieldOrigin::InsideUnion` で記録する。最も深いフィールド名を
`field_to_defining_macro` に登録する（`xcv_xsub` → `_XPVCV_COMMON`）。
union メンバー名（`xcv_root_u`）も同マクロを差すよう登録する。

**Build タイミング**:

B-1 で `_SV_HEAD` 等の Watcher が立っている関係上、struct 通過時点では
マクロ本体は既に展開済みだが、`MacroDef` は `pp.macros()` に保持されている。
Phase 2 終了時（`build_consistent_type_cache` の隣）で
`fields_dict.build_common_macro_fields(&pp)` を呼んで一括構築する。

### 検証

- `cargo test` 全 pass。
- 新規 unit test: `_XPVCV_COMMON` をパースして `xcv_xsub` の型が
  `void (*)(pTHX_ CV*)` 相当（`TypeRepr` 表現で fn ポインタ）になることを確認。
- `field_to_defining_macro["xcv_xsub"] == "_XPVCV_COMMON"` の確認。

## Step B-3: codegen を canonical type 経由にして fn ポインタ呼び出し対応

### 変更ファイル

- `src/rust_codegen.rs`

### 変更内容

**`build_syn_expr` の `Call` arm に検出を追加** (現行 `src/rust_codegen.rs:3099-3198` 付近):

```rust
ExprKind::Call { func, args } => {
    // 既存: __builtin_expect 等の特殊処理
    // ...

    // 追加: 共通マクロの fn ポインタフィールド呼び出しを検出
    if let Some(syn_call) = self.try_build_common_macro_fn_call(func, args, info) {
        return syn_call;
    }

    // 既存: 通常の関数呼び出し
    // ...
}
```

新メソッド:

```rust
fn try_build_common_macro_fn_call(
    &mut self,
    func: &Expr,
    args: &[Expr],
    info: Option<&MacroInferInfo>,
) -> Option<syn::Expr> {
    use crate::syn_codegen::*;

    let member_id = match &func.kind {
        ExprKind::Member { member, .. } | ExprKind::PtrMember { member, .. } => *member,
        _ => return None,
    };

    // canonical 型をマクロ経由で取得
    let dict = self.fields_dict?; // 既存の参照経路を要確認
    let (_macro, canonical_field) = dict.canonical_field(member_id)?;
    if !canonical_field.ty.is_function_pointer() {
        return None;
    }

    // <receiver>.<field> までを syn::Expr で組む
    let field_access = self.build_syn_expr(func, info);

    // bindgen は fn ポインタフィールドを Option<fn> にラップするため、
    // unwrap_unchecked を介してから呼ぶ。生成コードは既に unsafe 内なので
    // C 側の「null fn ptr 呼び出し = UB」と等価。
    let callee = method_call(field_access, "unwrap_unchecked", vec![]);

    // 引数: 既存ヘルパで構築（callee 名なし）
    let arg_syns: Vec<syn::Expr> = args.iter().enumerate().map(|(i, arg)| {
        let s = self.build_arg_string_unified(arg, info, None, i);
        syn::parse_str(&s).unwrap_or_else(|_| int_lit(0))
    }).collect();
    let mut punctuated = syn::punctuated::Punctuated::new();
    for a in arg_syns { punctuated.push(a); }

    Some(syn::Expr::Call(syn::ExprCall {
        attrs: vec![],
        func: Box::new(callee),
        paren_token: Default::default(),
        args: punctuated,
    }))
}
```

**`TypeRepr::is_function_pointer`** が無ければ追加（`src/type_repr.rs`）:

```rust
impl TypeRepr {
    pub fn is_function_pointer(&self) -> bool {
        // 既存の TypeRepr バリアントを見て fn ptr を判定
        // 必要なら専用バリアントを追加
    }
}
```

`TypeRepr` の現行構造を見て妥当な実装を選ぶ（B-2 で fn 型を表現する
バリアントが既に必要なはず）。

### 検証

#### `Perl_rpp_invoke_xs` の出力確認

```bash
~/blob/libperl-rs/12-macrogen-2-build.zsh
grep -A6 "fn Perl_rpp_invoke_xs" ~/db/github/exp-rstinycc-take2/tmp/macro_bindings.rs
```

期待:

```rust
pub unsafe fn Perl_rpp_invoke_xs(my_perl: *mut PerlInterpreter, cv: *mut CV) -> () {
    unsafe {
        assert!(!(cv).is_null());
        (*((*cv).sv_any as *mut XPVCV)).xcv_root_u.xcv_xsub.unwrap_unchecked()(my_perl, cv);
    }
}
```

#### `field, not a method` エラーの消失

```bash
grep -c "field, not a method" ~/db/github/exp-rstinycc-take2/tmp/build-error.log
# 0 を期待
```

#### regression なし

```bash
cargo test
# 350 / 350 pass
tail -1 ~/db/github/exp-rstinycc-take2/tmp/build-error.log
# error 件数が baseline (~128) ± noise band 内
```

## 想定外のケース・フォローアップ

- **bare `fn` フィールド（Option ラップなし）**: bindgen が
  `--no-rustfmt-bindings` 等の特殊設定で出力した場合、Option を介さない fn
  ポインタフィールドが現れる可能性がある。canonical type が fn pointer
  であれば `unwrap_unchecked` を挟まず Paren で囲んで呼ぶ分岐が必要。
  本計画では現サンプル全件が Option<fn> 形式であるため対応しない。
- **`_XPV_HEAD` の活用**: B-2 で field set を取得済みなので、xpv 系構造体間
  のフィールド共通性を type inference に活かす拡張は将来余地。本計画では
  fn ポインタ問題に絞る。
- **typedef 経由の fn 型**: bindings 側で `typedef Perl_ophook_t = ...` の
  ような形になった場合、`field_type_map` の文字列に `fn` が現れない可能性。
  canonical macro 由来で判定する本計画方式ではこの問題は発生しない。

## 実施順序まとめ

| Step | 内容 | 主な変更ファイル | コミット |
|------|------|-----------------|---------|
| B-1 | MacroCallWatcher で `_XPV_HEAD` / `_XPVCV_COMMON` 監視、struct↔macro マップ追加 | `src/fields_dict.rs`, `src/infer_api.rs` | 1 |
| B-2 | マクロ本体パース、`CommonFieldMacro` / `field_to_defining_macro` 構築 | `src/fields_dict.rs` (+ 必要に応じ `src/parser.rs` / `src/type_repr.rs`) | 1〜2 |
| B-3 | codegen `Call` arm に `try_build_common_macro_fn_call` 追加 | `src/rust_codegen.rs` | 1 |
| 検証 | 統合ビルドで `Perl_rpp_invoke_xs` 修正確認、cargo test 全 pass | （検証のみ） | — |

各 Step は独立にコミット可能。B-1 はコード生成出力に影響しない（観測のみ）。
B-2 はデータ構築のみで挙動変化なし。B-3 で初めて生成コードが変わる。
