# sv_u ユニオンフィールドからの型推論

## 背景

SV ファミリー構造体はすべて共通の union `sv_u` を持つが、
ユニオンの特定のフィールドは特定の SV ファミリー型でのみ有効である。

```c
// sv.h より
union {
    char*       svu_pv;      // SV (PV系)
    IV          svu_iv;      // SV (IV系)
    UV          svu_uv;      // SV (UV系)
    REGEXP*     svu_rx;      // SV (正規表現)
    SV*         svu_rv;      // SV (リファレンス)
    SV**        svu_array;   // AV
    HE**        svu_hash;    // HV
    GP*         svu_gp;      // GV
    PerlIO*     svu_fp;      // IO
} sv_u;
```

## 目標

マクロ本体で `arg->sv_u.svu_XXX` パターンを検出し、
`svu_XXX` フィールド名から引数の SV ファミリー型を推論する。

### 推論例

| マクロ | アクセスパターン | 推論される型 |
|--------|------------------|--------------|
| HvARRAY | `hv->sv_u.svu_hash` | HV * |
| IoIFP | `sv->sv_u.svu_fp` | IO * |
| SvRV | `sv->sv_u.svu_rv` | SV * |
| SvPVX | `sv->sv_u.svu_pv` | SV * |
| AvARRAY | `av->sv_u.svu_array` | AV * |
| GvGP | `gv->sv_u.svu_gp` | GV * |

## AST パターン

現在の出力例:
```
HvARRAY: expression (2 constraints, 0 uses)
  (member
    (ptr-member
      (ident hv) :type <unknown> sv_u) :type <unknown>->sv_u svu_hash)
```

検出すべきパターン:
```
(member
  (ptr-member
    (ident ARG) sv_u)
  svu_XXX)
```

つまり:
- `ExprKind::Member { expr, member: svu_field }`
  - `expr` が `ExprKind::PtrMember { expr: base, member: "sv_u" }`
    - `base` が `ExprKind::Ident(arg_name)`

## フィールド→型マッピング

```rust
/// sv_u ユニオンフィールドから SV ファミリー型へのマッピング
fn sv_u_field_to_type(field: &str) -> Option<&'static str> {
    match field {
        "svu_pv" => Some("SV"),    // char* - PV系SV
        "svu_iv" => Some("SV"),    // IV - 整数SV
        "svu_uv" => Some("SV"),    // UV - 符号なし整数SV
        "svu_rv" => Some("SV"),    // SV* - リファレンス
        "svu_rx" => Some("SV"),    // REGEXP* - 正規表現SV
        "svu_array" => Some("AV"), // SV** - 配列
        "svu_hash" => Some("HV"),  // HE** - ハッシュ
        "svu_gp" => Some("GV"),    // GP* - グロブ
        "svu_fp" => Some("IO"),    // PerlIO* - IO
        _ => None,
    }
}
```

## 実装計画

### Phase 1: パターン検出関数の追加

`macro_infer.rs` に新しいパターン検出を追加:

```rust
/// sv_u フィールドアクセスパターン
#[derive(Debug, Clone)]
pub struct SvUFieldPattern {
    /// アクセスされた sv_u のフィールド名 (例: "svu_hash")
    pub sv_u_field: InternedStr,
    /// 引数の識別子名 (例: "hv")
    pub arg_ident: InternedStr,
    /// 推論される SV ファミリー型 (例: "HV")
    pub inferred_type: String,
}

/// AST から sv_u フィールドアクセスパターンを検出
pub fn detect_sv_u_field_patterns(
    expr: &Expr,
    interner: &StringInterner,
) -> Vec<SvUFieldPattern>
```

### Phase 2: 型制約への適用

`MacroInferContext::apply_sv_u_field_constraints()` を追加:

```rust
/// sv_u フィールドアクセスパターンから型制約を適用
///
/// マクロ本体で `arg->sv_u.svu_XXX` パターンを検出し、
/// `arg` パラメータに対応する SV ファミリー型の制約を追加する。
pub fn apply_sv_u_field_constraints(
    &mut self,
    name: InternedStr,
    interner: &StringInterner,
) -> usize
```

### Phase 3: main.rs への統合

`run_infer_macro_types` で新しい制約適用を呼び出す:

```rust
// SvANY パターンから追加の型制約を適用
for name in macro_names {
    sv_any_constraint_count += infer_ctx.apply_sv_any_constraints(...);
}

// sv_u フィールドパターンから追加の型制約を適用
let mut sv_u_field_constraint_count = 0;
for name in macro_names {
    sv_u_field_constraint_count += infer_ctx.apply_sv_u_field_constraints(
        name,
        interner,
    );
}
```

### Phase 4: テストと動作確認

1. `cargo test` で既存テストが通ることを確認
2. 実際のマクロ出力で型推論が機能することを確認:
   - HvARRAY: `(ident hv) :type HV *`
   - IoIFP: `(ident sv) :type IO *`
   - AvARRAY: `(ident av) :type AV *`
   - GvGP: `(ident gv) :type GV *`

## 既存の SvANY パターンとの関係

SvANY パターン: `((XPVAV*) SvANY(av))` → `av: AV *`
sv_u フィールドパターン: `av->sv_u.svu_array` → `av: AV *`

両方のパターンは補完的:
- SvANY パターン: キャスト先の XPV* 型から推論
- sv_u フィールドパターン: アクセスされる union フィールドから推論

同じ引数に対して両方のパターンが適用される場合、
TypeEnv に複数の制約が追加される（矛盾チェックは後の段階で実装）。

## 追加考慮事項

### ネストしたアクセス

一部のマクロでは sv_u アクセスがネストしている可能性がある:
```c
#define SvEND(sv) (SvPVX(sv) + SvCUR(sv))
```

現在のスコープでは直接の `->sv_u.svu_*` パターンのみを対象とする。
マクロ展開後のパターン検出は将来の拡張とする。

### 複数引数

一つのマクロに複数の引数があり、それぞれ異なる sv_u フィールドに
アクセスする場合も正しく処理する必要がある。

### 型の優先度

apidoc からの型情報と sv_u フィールドからの推論が異なる場合:
- apidoc の方が正確な情報源として優先
- sv_u フィールド推論は apidoc がない場合のフォールバック

## 期待される効果

- apidoc に記載がないマクロでも型推論が可能に
- HvARRAY, IoIFP, GvGP などの型情報が自動的に推論される
- コード生成時により正確な型情報を利用可能
