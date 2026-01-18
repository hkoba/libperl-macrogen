# マクロ引数の型推論修正

## 問題

`--gen-rust` で生成される Rust コードにおいて、マクロ引数の型が `/* unknown */` になる問題。

### 具体例

`SvCUR` マクロの型付 S 式では、引数 `sv` を参照する式に対して型が判明している：

```
SvCUR: expression (5 constraints, 1 uses)
  (ptr-member
    (cast (type-name
      (decl-specs (typedef-name XPV)) (abstract-declarator (pointer)))
      (call
        (ident SvANY) :type <unknown>
        (ident sv) :type SV *) :type SvANY()) :type XPV * xpv_cur) :type STRLEN
  expr#71444: SV * (symbol lookup)
  ...
```

しかし、生成される Rust コードでは引数の型が unknown：

```rust
pub unsafe fn SvCUR(sv: /* unknown */) -> STRLEN {
    (*(SvANY(sv) as XPV)).xpv_cur
}
```

### 原因

1. `MacroParam` は `MacroParam::new()` で独自の `Expr`（独自の `ExprId`）を持つ
2. マクロ本体内の `(ident sv)` は別の `Expr`（別の `ExprId`）
3. 型制約は本体内の式の `ExprId` に対して設定される
4. `get_param_type()` は `MacroParam` の `ExprId` で `expr_constraints` を検索
5. → 見つからないため `/* unknown */` になる

### 現状の TypeEnv 構造

```rust
pub struct TypeEnv {
    /// パラメータ名 → 型制約リスト（現在は未使用）
    pub param_constraints: HashMap<InternedStr, Vec<TypeConstraint>>,

    /// ExprId → 型制約リスト
    pub expr_constraints: HashMap<ExprId, Vec<TypeConstraint>>,

    /// ExprId → パラメータ名へのリンク（逆引きなし）
    pub expr_to_param: Vec<ParamLink>,

    // ...
}
```

`link_expr_to_param()` で本体内の式からパラメータ名へのリンクは作られているが、
逆引き（パラメータ名 → 式リスト）がないため、引数の型取得時に活用できていない。

## 解決方針

`link_expr_to_param()` 時に逆引き辞書も同時に構築し、
引数の型取得時にパラメータを参照する全ての式の型制約を調べる。

## 実装計画

### Phase 1: TypeEnv に逆引き辞書を追加

`src/type_env.rs` を修正：

```rust
pub struct TypeEnv {
    // 既存フィールド
    pub param_constraints: HashMap<InternedStr, Vec<TypeConstraint>>,
    pub expr_constraints: HashMap<ExprId, Vec<TypeConstraint>>,
    pub return_constraints: Vec<TypeConstraint>,
    pub expr_to_param: Vec<ParamLink>,

    // 新規追加: パラメータ名 → ExprId リスト（逆引き）
    pub param_to_exprs: HashMap<InternedStr, Vec<ExprId>>,
}
```

### Phase 2: link_expr_to_param() を拡張

```rust
pub fn link_expr_to_param(&mut self, expr_id: ExprId, param_name: InternedStr, context: impl Into<String>) {
    // 既存の処理（正引き）
    self.expr_to_param.push(ParamLink {
        expr_id,
        param_name,
        context: context.into(),
    });

    // 逆引きも同時に更新
    self.param_to_exprs
        .entry(param_name)
        .or_default()
        .push(expr_id);
}
```

### Phase 3: merge() を更新

```rust
pub fn merge(&mut self, other: TypeEnv) {
    // 既存のマージ処理
    // ...

    // 逆引き辞書もマージ
    for (param, expr_ids) in other.param_to_exprs {
        self.param_to_exprs
            .entry(param)
            .or_default()
            .extend(expr_ids);
    }
}
```

### Phase 4: get_param_type() を修正

`src/rust_codegen.rs` を修正：

```rust
fn get_param_type(&self, param: &MacroParam, info: &MacroInferInfo) -> String {
    let param_name = param.name;

    // 方法1: パラメータを参照する式の型制約から取得
    if let Some(expr_ids) = info.type_env.param_to_exprs.get(&param_name) {
        for expr_id in expr_ids {
            if let Some(constraints) = info.type_env.expr_constraints.get(expr_id) {
                if let Some(first) = constraints.first() {
                    return self.type_repr_to_rust(&first.ty);
                }
            }
        }
    }

    // 方法2: 従来の方法（MacroParam の ExprId）- フォールバック
    let expr_id = param.expr_id();
    if let Some(constraints) = info.type_env.expr_constraints.get(&expr_id) {
        if let Some(first) = constraints.first() {
            return self.type_repr_to_rust(&first.ty);
        }
    }

    "/* unknown */".to_string()
}
```

### Phase 5: テストと検証

1. 既存テストが通ることを確認
2. `SvCUR` などの代表的なマクロで引数の型が正しく出力されることを確認
3. 統計情報で改善を確認

## 考慮点

### 同じパラメータに複数の型制約がある場合

```c
#define EXAMPLE(sv) (SvCUR(sv) + SvIVX(sv))
//                   ^^^ SV *    ^^^ SV *
```

同じパラメータが複数の場所で参照され、異なる型制約が付く可能性がある。

対応方針（段階的）：
1. **シンプル**: 最初に見つかった型を使う（今回の実装）
2. **改善**: 全ての型が同じかチェックし、異なる場合は警告
3. **高度**: 型の互換性をチェックし、最も具体的な型を選択

## 期待される効果

修正前：
```rust
pub unsafe fn SvCUR(sv: /* unknown */) -> STRLEN { ... }
```

修正後：
```rust
pub unsafe fn SvCUR(sv: *mut SV) -> STRLEN { ... }
```

## 将来の拡張

1. **型の一貫性チェック**: 同じパラメータに対する複数の型制約が矛盾しないか検証
2. **型の統合**: 複数の型制約から最も適切な型を選択するロジック
3. **警告出力**: 型が推論できなかったパラメータをレポート
