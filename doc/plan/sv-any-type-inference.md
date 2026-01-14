# SvANY キャストパターンによる型推論

## 目的

`SvANY(sv)` マクロとキャストを組み合わせたパターンから、
引数の型を推論する機能を実装する。

## 背景

Perl の C ヘッダーには以下のようなパターンが頻出する：

```c
#define AvALLOC(av)   ((XPVAV*)  SvANY(av))->xav_alloc
#define CvSTASH(sv)   (MUTABLE_HV(((XPVCV*)MUTABLE_PTR(SvANY(sv)))->xcv_stash))
#define IoOFP(sv)     ((XPVIO*)  SvANY(sv))->xio_ofp
#define SvCUR(sv)     ((XPV*) SvANY(sv))->xpv_cur
#define SvIVX(sv)     ((XPVIV*) SvANY(sv))->xiv_iv
#define GvXPVGV(gv)   ((XPVGV*)SvANY(gv))
```

これらのキャスト先の型名（`XPVAV*`, `XPVCV*` など）は、
`_SV_HEAD(typeName)` マクロの引数として出現する：

```c
struct av {
    _SV_HEAD(XPVAV*);  // typeName = "XPVAV*"
    _SV_HEAD_UNION;
};
```

つまり、`((XPVAV*) SvANY(av))` というパターンから、
引数 `av` の型が `AV*`（= `struct av*`）であることが推論できる。

## 設計

### 前提: SvANY の展開抑制

`TokenExpander` で `SvANY` を `no_expand` リストに追加し、
AST に関数呼び出しとして残す。

```rust
// macro_infer.rs
let sym_sv_any = interner.intern("SvANY");
expander.add_no_expand(sym_sv_any);
```

これにより：
- `info.uses` に `SvANY` が含まれる → パターン検出対象の判定に使用
- AST に `Call { func: "SvANY", args: [...] }` が残る → 引数の特定が容易

### Phase 1: typeName → 構造体名マッピングの構築

`_SV_HEAD(typeName)` の引数を取得し、マッピングを構築する。

#### 1.1 MacroCallWatcher の引数取得を活用

現在の実装では `MacroCallWatcher::last_args()` で引数を取得可能。

```rust
// main.rs での使用例
if watcher.take_called() {
    if let Some(args) = watcher.last_args() {
        // args[0] = "XPVAV*" など
        for name in &struct_names {
            fields_dict.add_sv_family_member_with_type(*name, &args[0]);
        }
    }
}
```

#### 1.2 FieldsDict への新しいマッピング追加

```rust
// fields_dict.rs
pub struct FieldsDict {
    // 既存フィールド...

    /// SV ファミリーメンバー（_SV_HEAD マクロを使用する構造体）
    sv_family_members: HashSet<InternedStr>,

    /// NEW: typeName → 構造体名のマッピング
    /// 例: "XPVAV" → "av", "XPVCV" → "cv"
    sv_head_type_to_struct: HashMap<String, InternedStr>,
}

impl FieldsDict {
    /// SV ファミリーメンバーと typeName を同時に登録
    pub fn add_sv_family_member_with_type(
        &mut self,
        struct_name: InternedStr,
        type_name: &str,  // "XPVAV*" など
    ) {
        self.sv_family_members.insert(struct_name);

        // ポインタ記号を除去して正規化
        let normalized = type_name.trim().trim_end_matches('*').trim();
        if !normalized.is_empty() && normalized != "void" {
            self.sv_head_type_to_struct.insert(
                normalized.to_string(),
                struct_name,
            );
        }
    }

    /// typeName から構造体名を取得
    pub fn get_struct_for_sv_head_type(&self, type_name: &str) -> Option<InternedStr> {
        let normalized = type_name.trim().trim_end_matches('*').trim();
        self.sv_head_type_to_struct.get(normalized).copied()
    }
}
```

### Phase 2: SvANY パターンの検出（AST ベース）

マクロ本体の AST から `(TYPE*) SvANY(arg)` パターンを検出する。

#### 2.1 検出対象の判定

`info.uses` に `SvANY` が含まれるマクロのみを対象とする：

```rust
// macro_infer.rs
let sv_any_id = interner.intern("SvANY");
if info.uses.contains(&sv_any_id) {
    // パターン検出を実行
}
```

#### 2.2 AST パターンの種類

検出すべき AST パターン：

```
Cast {
    type_name: TypeName { specs: Struct("XPVAV"), derived: [Pointer] },
    expr: Call { func: Ident("SvANY"), args: [arg] }
}
```

または MUTABLE_PTR 経由：
```
Cast {
    type_name: TypeName { specs: Struct("XPVCV"), derived: [Pointer] },
    expr: Call {
        func: Ident("MUTABLE_PTR"),
        args: [Call { func: Ident("SvANY"), args: [arg] }]
    }
}
```

#### 2.3 パターン検出関数

```rust
/// SvANY パターンの検出結果
pub struct SvAnyPattern {
    /// キャスト先の型名（例: "XPVAV"）
    pub cast_type: String,
    /// SvANY の引数の識別子
    pub arg_ident: InternedStr,
}

/// 式から SvANY パターンを再帰的に検出
fn detect_sv_any_patterns(
    expr: &Expr,
    sv_any_id: InternedStr,
    interner: &StringInterner,
) -> Vec<SvAnyPattern> {
    let mut patterns = Vec::new();
    detect_sv_any_patterns_recursive(expr, sv_any_id, interner, &mut patterns);
    patterns
}

fn detect_sv_any_patterns_recursive(
    expr: &Expr,
    sv_any_id: InternedStr,
    interner: &StringInterner,
    patterns: &mut Vec<SvAnyPattern>,
) {
    match &expr.kind {
        ExprKind::Cast { type_name, expr: inner } => {
            // キャスト先がポインタ型か確認
            if let Some(cast_type) = extract_pointer_base_type(type_name, interner) {
                // 内部が SvANY 呼び出しか確認
                if let Some(arg) = extract_sv_any_arg(inner, sv_any_id) {
                    patterns.push(SvAnyPattern {
                        cast_type,
                        arg_ident: arg,
                    });
                }
                // MUTABLE_PTR 経由のパターンも検出
                else if let Some(arg) = extract_sv_any_through_mutable_ptr(inner, sv_any_id) {
                    patterns.push(SvAnyPattern {
                        cast_type,
                        arg_ident: arg,
                    });
                }
            }
            // 内部も再帰的に検索
            detect_sv_any_patterns_recursive(inner, sv_any_id, interner, patterns);
        }
        // 他の式種別も再帰的に検索
        ExprKind::Binary { lhs, rhs, .. } => {
            detect_sv_any_patterns_recursive(lhs, sv_any_id, interner, patterns);
            detect_sv_any_patterns_recursive(rhs, sv_any_id, interner, patterns);
        }
        // ... 他のケース
        _ => {}
    }
}

/// SvANY(arg) の arg を抽出（arg が識別子の場合のみ）
fn extract_sv_any_arg(expr: &Expr, sv_any_id: InternedStr) -> Option<InternedStr> {
    if let ExprKind::Call { func, args } = &expr.kind {
        if let ExprKind::Ident(id) = &func.kind {
            if *id == sv_any_id && args.len() == 1 {
                if let ExprKind::Ident(arg_id) = &args[0].kind {
                    return Some(*arg_id);
                }
            }
        }
    }
    None
}
```

### Phase 3: 型制約の適用

検出したパターンから型制約を生成・適用する。

#### 3.1 制約生成ロジック

```rust
impl MacroInferContext {
    /// SvANY パターンから型制約を追加
    pub fn apply_sv_any_constraints(
        &mut self,
        name: InternedStr,
        params: &[InternedStr],
        fields_dict: &FieldsDict,
        interner: &StringInterner,
    ) {
        let info = match self.macros.get_mut(&name) {
            Some(info) => info,
            None => return,
        };

        let sv_any_id = interner.intern("SvANY");

        // SvANY を使用していなければスキップ
        if !info.uses.contains(&sv_any_id) {
            return;
        }

        // パターン検出（式マクロと文マクロの両方に対応）
        let mut patterns = Vec::new();
        match &info.parse_result {
            ParseResult::Expression(expr) => {
                patterns = detect_sv_any_patterns(expr, sv_any_id, interner);
            }
            ParseResult::Statement(block_items) => {
                // 文マクロの場合、各 BlockItem から式を抽出して検出
                for item in block_items {
                    collect_sv_any_patterns_from_block_item(item, sv_any_id, interner, &mut patterns);
                }
            }
            _ => return,
        };

        for pattern in patterns {
            // パラメータかどうか確認
            if !params.contains(&pattern.arg_ident) {
                continue;
            }

            // typeName から構造体名を取得
            let struct_name = match fields_dict.get_struct_for_sv_head_type(&pattern.cast_type) {
                Some(name) => name,
                None => continue,
            };

            // 型制約を追加
            // 例: av の型は AV* (= struct av*)
            let struct_name_str = interner.get(struct_name);
            let type_str = format!("{}*", struct_name_str.to_uppercase());

            info.type_env.add_constraint(TypeConstraint::new(
                ExprId::default(), // パラメータ用の特別な ID が必要
                TypeRepr::from_c_type_string(&type_str, interner),
                format!("SvANY pattern: ({}*) SvANY({})",
                    pattern.cast_type,
                    interner.get(pattern.arg_ident)),
            ));
        }
    }
}

/// BlockItem から SvANY パターンを収集
fn collect_sv_any_patterns_from_block_item(
    item: &BlockItem,
    sv_any_id: InternedStr,
    interner: &StringInterner,
    patterns: &mut Vec<SvAnyPattern>,
) {
    match item {
        BlockItem::Stmt(stmt) => {
            collect_sv_any_patterns_from_stmt(stmt, sv_any_id, interner, patterns);
        }
        BlockItem::Decl(_) => {
            // 宣言内の初期化式からも検出可能（必要に応じて実装）
        }
    }
}

/// 文から SvANY パターンを収集
fn collect_sv_any_patterns_from_stmt(
    stmt: &Stmt,
    sv_any_id: InternedStr,
    interner: &StringInterner,
    patterns: &mut Vec<SvAnyPattern>,
) {
    match &stmt.kind {
        StmtKind::Expr(expr) | StmtKind::Return(Some(expr)) => {
            let found = detect_sv_any_patterns(expr, sv_any_id, interner);
            patterns.extend(found);
        }
        StmtKind::If { cond, then_branch, else_branch } => {
            let found = detect_sv_any_patterns(cond, sv_any_id, interner);
            patterns.extend(found);
            collect_sv_any_patterns_from_stmt(then_branch, sv_any_id, interner, patterns);
            if let Some(else_stmt) = else_branch {
                collect_sv_any_patterns_from_stmt(else_stmt, sv_any_id, interner, patterns);
            }
        }
        StmtKind::While { cond, body } | StmtKind::DoWhile { body, cond } => {
            let found = detect_sv_any_patterns(cond, sv_any_id, interner);
            patterns.extend(found);
            collect_sv_any_patterns_from_stmt(body, sv_any_id, interner, patterns);
        }
        StmtKind::For { init, cond, step, body } => {
            if let Some(expr) = init {
                let found = detect_sv_any_patterns(expr, sv_any_id, interner);
                patterns.extend(found);
            }
            if let Some(expr) = cond {
                let found = detect_sv_any_patterns(expr, sv_any_id, interner);
                patterns.extend(found);
            }
            if let Some(expr) = step {
                let found = detect_sv_any_patterns(expr, sv_any_id, interner);
                patterns.extend(found);
            }
            collect_sv_any_patterns_from_stmt(body, sv_any_id, interner, patterns);
        }
        StmtKind::Block(items) => {
            for item in items {
                collect_sv_any_patterns_from_block_item(item, sv_any_id, interner, patterns);
            }
        }
        _ => {}
    }
}
```

## 実装手順

### Phase 1: マッピング構築

1. `FieldsDict` に `sv_head_type_to_struct: HashMap<String, InternedStr>` フィールドを追加
2. `add_sv_family_member_with_type()` メソッドを追加
3. `get_struct_for_sv_head_type()` メソッドを追加
4. `main.rs` で `_SV_HEAD` の引数も取得してマッピングを構築
5. 単体テスト追加

### Phase 2: SvANY 展開抑制とパターン検出

1. `macro_infer.rs` で `SvANY` を `no_expand` リストに追加
2. `SvAnyPattern` 構造体を定義（`macro_infer.rs` または新モジュール）
3. `detect_sv_any_patterns()` 関数を実装
4. `extract_sv_any_arg()` ヘルパー関数を実装
5. `extract_pointer_base_type()` ヘルパー関数を実装
6. 単体テスト追加

### Phase 3: 制約適用

1. `apply_sv_any_constraints()` を実装
2. `infer_macro_types()` から呼び出し
3. 統合テスト追加
4. 実際の Perl ヘッダーで動作確認

## 期待される効果

### 推論可能になる例

| マクロ | パターン | 推論される型 |
|--------|----------|--------------|
| `AvALLOC(av)` | `(XPVAV*) SvANY(av)` | `av: AV*` |
| `CvSTASH(sv)` | `(XPVCV*) ... SvANY(sv)` | `sv: CV*` |
| `IoOFP(sv)` | `(XPVIO*) SvANY(sv)` | `sv: IO*` |
| `GvXPVGV(gv)` | `(XPVGV*) SvANY(gv)` | `gv: GV*` |
| `LvTYPE(sv)` | `(XPVLV*) SvANY(sv)` | `sv: SV*` (LV は SV の派生) |

### マッピング例

`_SV_HEAD(typeName)` から構築されるマッピング：

| typeName | 構造体名 | 推論される型 |
|----------|----------|--------------|
| `XPVAV*` | `av` | `AV*` |
| `XPVCV*` | `cv` | `CV*` |
| `XPVHV*` | `hv` | `HV*` |
| `XPVIO*` | `io` | `IO*` |
| `XPVGV*` | `gv` | `GV*` |
| `XPVLV*` | - | (SV* として扱う) |
| `void*` | `sv` | (汎用、マッピングに含めない) |

### 制限事項

- `void*` を使用する `struct sv` は汎用のため、マッピングには含めない
- `MUTABLE_PTR`, `MUTABLE_HV` などのラッパーマクロは個別対応が必要
- パラメータ以外の変数への制約は対象外

## 変更対象ファイル

- `src/fields_dict.rs` - マッピング追加
- `src/main.rs` - マッピング構築ロジック
- `src/macro_infer.rs` - SvANY 展開抑制、パターン検出、制約適用

## 備考

- `SvANY` マクロの定義: `#define SvANY(sv) (sv)->sv_any`
- `no_expand` に追加することで、`assert` と同様に AST で検出可能になる
- `info.uses.contains(&sv_any_id)` でパターン検出対象を絞り込める

---

## 設計課題: マクロパラメータの型制約管理

### 現状の問題

現在の実装では、型制約が2つのコレクションに分散している：

```rust
pub struct TypeEnv {
    param_constraints: HashMap<InternedStr, Vec<TypeConstraint>>,  // 名前ベース
    expr_constraints: HashMap<ExprId, Vec<TypeConstraint>>,        // ExprId ベース
}
```

この設計には以下の問題がある：

1. **キーの不一致**: パラメータは名前（`InternedStr`）、式は `ExprId` という異なるキーで管理
2. **型検索の複雑化**: `TypedSexpPrinter::get_type_str()` が `expr_constraints` のみを検索
3. **一貫性の欠如**: パラメータと本体内の識別子参照を統一的に扱えない

### 出力での問題

```
AvALLOC: expression (4 constraints, 1 uses)
  (ident av) :type <unknown>   ← パラメータ制約が表示されない
```

`apply_sv_any_constraints` で `av: AV*` の制約を追加しても、
`TypedSexpPrinter` は `expr_constraints` のみを検索するため `<unknown>` と表示される。

### 解決策: マクロを Lambda 的な AST として表現

マクロを関数（lambda）のように、パラメータと本体を持つ構造として表現する。

#### 新しい AST 構造

```rust
/// マクロのパース結果（パラメータ情報付き）
pub struct MacroAst {
    /// パラメータリスト（各パラメータは ExprId を持つ Expr）
    pub params: Vec<Expr>,  // ExprKind::Ident を持つ Expr
    /// 本体
    pub body: MacroBodyKind,
}

pub enum MacroBodyKind {
    Expression(Expr),
    Statement(Vec<BlockItem>),
    Unparseable(String),
}
```

#### 利点

1. **型制約の統一**: パラメータも `ExprId` を持つため、`expr_constraints` のみで管理可能
2. **param_constraints の廃止**: 型制約が一元化され、バグの原因が減少
3. **自然な型検索**: `get_type_str(expr_id)` でパラメータの型も取得可能
4. **expr_to_param リンクの明確化**: 本体内の識別子 → パラメータ Expr へのリンクが自然

#### 本体内の識別子との関係

```c
#define AvALLOC(av)  (((XPVAV*) SvANY(av))->xav_alloc)
```

このマクロでは：
- パラメータ `av` に `ExprId`（例: `expr#1000`）を割り当て
- 本体内の `(ident av)` にも `ExprId`（例: `expr#1001`）がある
- `expr_to_param` で `expr#1001` → `expr#1000` のリンクを管理

SvANY パターンから推論した制約：
- `expr#1000`（パラメータ）に `AV*` 制約を追加
- `expr#1001`（本体内の参照）は `expr_to_param` 経由で型を解決

### 実装方針

#### Phase 4: MacroAst への移行

1. **`MacroAst` 構造体の定義** (`ast.rs` または新モジュール)
   - パラメータリスト: `Vec<Expr>`（各要素が `ExprKind::Ident` で `ExprId` を持つ）
   - 本体: `MacroBodyKind`

2. **`ParseResult` の置き換え**
   - `MacroInferInfo.parse_result: ParseResult` → `MacroInferInfo.ast: Option<MacroAst>`

3. **パース時のパラメータ Expr 生成**
   - `build_macro_info()` でパラメータごとに `Expr` を生成
   - 各パラメータに固有の `ExprId` を割り当て

4. **`param_constraints` の廃止**
   - `TypeEnv` から `param_constraints` フィールドを削除
   - `add_param_constraint()` を削除
   - 全ての制約を `expr_constraints` に統一

5. **`apply_sv_any_constraints` の修正**
   - パラメータの `ExprId` を使って `expr_constraints` に制約を追加

6. **`TypedSexpPrinter` の修正**
   - パラメータも出力に含めるか検討
   - `get_type_str()` の変更は不要（統一された `expr_constraints` を検索）

7. **`expr_to_param` の活用**
   - 本体内の識別子参照とパラメータ Expr をリンク
   - 型解決時にリンクを辿って型を伝播

### 互換性

- `ParseResult` を使用している既存コードは段階的に移行
- 移行期間中は両方のインターフェースをサポート可能
