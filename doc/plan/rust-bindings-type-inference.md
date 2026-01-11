# collect_expr_constraints への型計算統合と infer_expr_type 廃止

## 目標

1. `collect_expr_constraints` に全式の型計算を統合
2. `infer_expr_type` を廃止
3. RustDeclDict からの型情報をマクロ型推論に活用

## 背景

### 現状の問題

```
sv_2mortal: expression (4 constraints, 0 uses) [THX]
  (call
    (ident Perl_sv_2mortal) :type <unknown>
    (ident my_perl) :type <unknown>
    (ident a) :type <unknown>) :type <unknown>
```

- `Perl_sv_2mortal` は bindings.rs に定義あり
- `collect_expr_constraints` で type_env に制約を追加しているが
- `infer_expr_type` は type_env を参照せず、別経路で型推論している
- 結果として RustDeclDict の情報が活用されていない

### 現状の処理フロー

```
analyze_macro
  ├─ register_macro_params_from_apidoc  → シンボルテーブルにパラメータ登録
  ├─ collect_expr_constraints           → type_env に Call の引数・戻り値のみ追加
  └─ compute_and_store_expr_types       → infer_expr_type で全式の型を計算
                                           (type_env を参照しない)
```

## 解決策

`collect_expr_constraints` を拡張し、全式の型を計算して type_env に追加する。
`compute_and_store_expr_types` と `infer_expr_type` は廃止。

### 新しい処理フロー

```
analyze_macro
  ├─ register_macro_params_from_apidoc  → シンボルテーブルにパラメータ登録
  └─ collect_expr_constraints           → 全式の型を計算して type_env に追加
                                           (子式を先に処理、親式を後で計算)
```

## 実装計画

### Step 1: collect_expr_constraints を拡張

**src/semantic.rs:**

各 ExprKind に対して、子式を処理した後、自身の型を計算して type_env に追加する。

```rust
pub fn collect_expr_constraints(&mut self, expr: &Expr, type_env: &mut TypeEnv) {
    match &expr.kind {
        ExprKind::IntLit(_) => {
            type_env.add_constraint(TypeConstraint::new(
                expr.id, "int", ConstraintSource::Inferred, "integer literal"
            ));
        }
        ExprKind::FloatLit(_) => {
            type_env.add_constraint(TypeConstraint::new(
                expr.id, "double", ConstraintSource::Inferred, "float literal"
            ));
        }
        ExprKind::StringLit(_) => {
            type_env.add_constraint(TypeConstraint::new(
                expr.id, "char *", ConstraintSource::Inferred, "string literal"
            ));
        }
        ExprKind::Ident(name) => {
            // シンボルテーブルから型を取得
            if let Some(sym) = self.lookup_symbol(*name) {
                let ty_str = sym.ty.display(self.interner);
                type_env.add_constraint(TypeConstraint::new(
                    expr.id, &ty_str, ConstraintSource::Inferred, "symbol lookup"
                ));
            }
            // パラメータ参照の紐付け（既存）
            if self.is_macro_param(*name) {
                type_env.link_expr_to_param(expr.id, *name, "parameter reference");
            }
        }
        ExprKind::Call { func, args } => {
            // 子式を先に処理
            self.collect_expr_constraints(func, type_env);
            for arg in args {
                self.collect_expr_constraints(arg, type_env);
            }
            // Call の型制約を追加（RustDeclDict / Apidoc から）
            self.collect_call_constraints(expr.id, func, args, type_env);
        }
        ExprKind::Binary { op, lhs, rhs } => {
            // 子式を先に処理
            self.collect_expr_constraints(lhs, type_env);
            self.collect_expr_constraints(rhs, type_env);
            // 親式の型を計算
            let result_ty = self.compute_binary_type(op, lhs.id, rhs.id, type_env);
            type_env.add_constraint(TypeConstraint::new(
                expr.id, &result_ty, ConstraintSource::Inferred, "binary expression"
            ));
        }
        ExprKind::Cast { type_name, expr: inner } => {
            self.collect_expr_constraints(inner, type_env);
            let ty = self.resolve_type_name(type_name);
            let ty_str = ty.display(self.interner);
            type_env.add_constraint(TypeConstraint::new(
                expr.id, &ty_str, ConstraintSource::Inferred, "cast expression"
            ));
        }
        // ... 他の ExprKind も同様に拡張
    }
}
```

### Step 2: 型計算ヘルパーメソッドを追加

**src/semantic.rs:**

```rust
/// type_env から式の型を取得
fn get_expr_type_str(&self, expr_id: ExprId, type_env: &TypeEnv) -> String {
    if let Some(constraints) = type_env.expr_constraints.get(&expr_id) {
        if let Some(c) = constraints.first() {
            return c.ty.clone();
        }
    }
    "<unknown>".to_string()
}

/// 二項演算の結果型を計算
fn compute_binary_type(&self, op: &BinOp, lhs_id: ExprId, rhs_id: ExprId, type_env: &TypeEnv) -> String {
    match op {
        // 比較演算子は int を返す
        BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge |
        BinOp::Eq | BinOp::Ne | BinOp::LogAnd | BinOp::LogOr => "int".to_string(),
        // 算術演算子は通常の型昇格
        _ => {
            let lhs_ty = self.get_expr_type_str(lhs_id, type_env);
            let rhs_ty = self.get_expr_type_str(rhs_id, type_env);
            self.usual_arithmetic_conversion_str(&lhs_ty, &rhs_ty)
        }
    }
}
```

### Step 3: compute_and_store_expr_types を削除

**src/macro_infer.rs:**

`analyze_macro` から `compute_and_store_expr_types` の呼び出しを削除。

### Step 4: infer_expr_type を段階的に廃止

**src/semantic.rs:**

- `infer_expr_type` に `#[deprecated]` を付与
- 関連メソッドも同様に deprecated に
- 呼び出し箇所（`compute_and_store_expr_types`）を削除すれば、未使用警告が出る
- 警告が出なくなるまで他の使用箇所を確認・修正

```rust
#[deprecated(note = "use collect_expr_constraints instead")]
pub fn infer_expr_type(&mut self, expr: &Expr) -> Type {
    // 既存の実装
}
```

### Step 5: lib.rs のエクスポートを更新

`compute_and_store_expr_types` のエクスポートを削除。

## 修正対象ファイル

1. **src/semantic.rs**
   - `collect_expr_constraints` を拡張（全式の型を計算）
   - `get_expr_type_str`, `compute_binary_type` 等のヘルパー追加
   - `infer_expr_type` を削除

2. **src/macro_infer.rs**
   - `compute_and_store_expr_types` を削除
   - `analyze_macro` から呼び出しを削除

3. **src/lib.rs**
   - エクスポートから `compute_and_store_expr_types` を削除

## 期待される結果

```
sv_2mortal: expression (4 constraints, 0 uses) [THX]
  (call
    (ident Perl_sv_2mortal) :type <unknown>
    (ident my_perl) :type *mut PerlInterpreter
    (ident a) :type *mut SV) :type *mut SV
```

- `a` の型は関数の第2引数の型 `*mut SV` として推論される
- `my_perl` の型は関数の第1引数の型 `*mut PerlInterpreter` として推論される
- call 式全体の戻り値型も `*mut SV` として推論される
