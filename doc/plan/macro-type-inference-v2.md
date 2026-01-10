# マクロ型推論 v2 計画書

## 目標

ExprId を活用したマクロの型推論機能を実装する。
今回は一度に完成を目指さず、**推論の途中の状況を観察できるようにする**ことに集中する。

## 設計方針

1. **マクロテーブルのループ回数を最小限にする**
   - 目的別に何度もスキャンするのではなく、一回のループで必要な情報を収集
2. **既存モジュールの凍結**
   - `macro_analysis.rs`, `macro_analyzer2.rs` は凍結
   - 新規モジュールに実装
3. **観察可能性の重視**
   - 型推論の各ステップで S式出力を行い、途中経過を確認可能にする
   - ExprId に紐づく型情報を S式に含める

---

## Phase 0: Preprocessor の改良（先行実装）

### 背景

従来の `macro_analyzer2.rs` では、マクロ展開に Preprocessor を使わず、
自前の `expand_macro_body` を使用していた。これには以下の制限がある：
- オブジェクトマクロのみ展開、関数マクロは非展開
- マクロ展開マーカー（MacroBegin/MacroEnd）が使えない

### 改良点

#### 1. 部分トークン列用 Preprocessor（TokenExpander）

マクロの body（トークン列）を展開するための軽量 Preprocessor を新設する。

```rust
/// 部分トークン列のマクロ展開器
/// MacroTable は readonly で、新しいマクロ定義は追加しない
pub struct TokenExpander<'a> {
    macro_table: &'a MacroTable,
    interner: &'a mut StringInterner,
    files: &'a FileRegistry,

    /// 展開しないマクロ名（定数マクロ等）
    no_expand: HashSet<InternedStr>,

    /// マクロ展開マーカーを出力するか
    emit_markers: bool,
}

impl<'a> TokenExpander<'a> {
    /// トークン列をマクロ展開する
    pub fn expand(&mut self, tokens: &[Token]) -> Vec<Token>;

    /// 関数マクロ呼び出しも含めて展開する
    pub fn expand_with_calls(&mut self, tokens: &[Token]) -> Vec<Token>;
}
```

**特徴:**
- MacroTable は参照のみ（readonly）
- 関数マクロ呼び出しの展開に対応
- MacroBegin/MacroEnd マーカーの出力対応
- 定数マクロ等の展開抑制リスト

#### 2. Preprocessor へのマクロ定義コールバック

マクロ定義時に呼ばれるコールバックを追加する。

```rust
/// マクロ定義時のコールバック
pub trait MacroDefCallback {
    /// マクロが定義されたときに呼ばれる
    fn on_macro_defined(&mut self, def: &MacroDef);
}

/// THX マクロを収集するコールバック実装
pub struct ThxCollector {
    /// THX 依存マクロ名の集合
    /// （aTHX, tTHX, my_perl を含む、または THX 依存マクロを使用するマクロ）
    pub thx_macros: HashSet<InternedStr>,

    // 事前に intern したシンボル（文字列比較を避けるため）
    sym_athx: InternedStr,
    sym_tthx: InternedStr,
    sym_my_perl: InternedStr,
}

impl ThxCollector {
    pub fn new(interner: &mut StringInterner) -> Self {
        Self {
            thx_macros: HashSet::new(),
            sym_athx: interner.intern("aTHX"),
            sym_tthx: interner.intern("tTHX"),
            sym_my_perl: interner.intern("my_perl"),
        }
    }

    /// トークンが THX 関連かどうか判定
    fn is_thx_token(&self, id: InternedStr) -> bool {
        id == self.sym_athx || id == self.sym_tthx || id == self.sym_my_perl
    }
}

impl MacroDefCallback for ThxCollector {
    fn on_macro_defined(&mut self, def: &MacroDef) {
        // def.body をスキャンして以下の条件をチェック：
        // 1. トークンが aTHX, tTHX, my_perl のいずれか
        // 2. 既に thx_macros に登録済みのトークン
        for token in &def.body {
            if let TokenKind::Ident(id) = &token.kind {
                if self.is_thx_token(*id) || self.thx_macros.contains(id) {
                    self.thx_macros.insert(def.name);
                    break;
                }
            }
        }
    }
}
```

**THX マクロの登録条件:**
1. マクロの body に `aTHX`, `tTHX`, `my_perl` トークンが出現する
2. マクロの body に既に `thx_macros` に登録済みのトークンが出現する

**利点:**
- 最初の parse 時点で THX マクロを識別可能
- マクロテーブルを後からスキャンする必要がない
- 事前に intern したシンボルとの比較で高速化

### 実装ファイル

```
src/
├── preprocessor.rs     # MacroDefCallback の追加（改修）
├── token_expander.rs   # 部分トークン列用マクロ展開器（新規）
└── thx_collector.rs    # THX マクロ収集コールバック（新規）
```

---

## 新規モジュール構成（全体）

```
src/
├── token_expander.rs   # 部分トークン列用マクロ展開器（新規・Phase 0）
├── thx_collector.rs    # THX マクロ収集コールバック（新規・Phase 0）
├── type_env.rs         # 型環境・制約管理（新規）
├── macro_infer.rs      # メインの型推論エンジン（新規）
├── preprocessor.rs     # MacroDefCallback の追加（改修・Phase 0）
├── semantic.rs         # 型推論支援機能の追加（改修）
└── sexp.rs             # ExprId型情報付きS式出力（改修）
```

---

## データ構造

### 型環境 (TypeEnv)

```rust
/// 型の出所を区別するための列挙型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeSource {
    CHeader,           // C ヘッダーから取得
    RustBindings,      // bindings.rs から取得
    Apidoc,            // apidoc から取得
    Inferred,          // 推論で導出
}

/// 型制約（簡約せずにそのまま保持）
#[derive(Debug, Clone)]
pub struct TypeConstraint {
    pub expr_id: ExprId,
    pub ty: String,           // 型文字列（C形式 or Rust形式）
    pub source: TypeSource,
    pub context: String,      // デバッグ用：どこで取得したか
}

/// 型環境
pub struct TypeEnv {
    /// パラメータ名 → 型制約リスト
    pub param_constraints: HashMap<InternedStr, Vec<TypeConstraint>>,
    /// ExprId → 型制約リスト
    pub expr_constraints: HashMap<ExprId, Vec<TypeConstraint>>,
    /// 戻り値の型制約
    pub return_constraints: Vec<TypeConstraint>,
}
```

### マクロ情報 (MacroInferInfo)

```rust
pub struct MacroInferInfo {
    pub name: InternedStr,
    pub is_target: bool,

    // def-use 関係
    pub uses: HashSet<InternedStr>,        // このマクロが使用する他のマクロ
    pub used_by: HashSet<InternedStr>,     // このマクロを使用するマクロ

    // THX 依存（Phase 0 の ThxCollector から取得）
    pub is_thx_dependent: bool,

    // パース結果
    pub parse_result: ParseResult,

    // 型環境
    pub type_env: TypeEnv,

    // 推論状態
    pub infer_status: InferStatus,
}

pub enum ParseResult {
    Expression(Expr),
    Statement(Vec<BlockItem>),
    Unparseable,
}

pub enum InferStatus {
    Pending,           // 未処理
    TypeComplete,      // 全ての型が確定
    TypeIncomplete,    // 一部の型が未確定
    TypeUnknown,       // 型推論不能
}
```

---

## 処理フロー

### Phase 0: Preprocessor 改良

```
┌─────────────────────────────────────────────────────┐
│ Step 0-1: MacroDefCallback の実装                   │
│ - Preprocessor に callback フックを追加             │
│ - ThxCollector の実装                               │
├─────────────────────────────────────────────────────┤
│ Step 0-2: TokenExpander の実装                      │
│ - 部分トークン列のマクロ展開                         │
│ - 関数マクロ呼び出しの展開対応                       │
│ - MacroBegin/MacroEnd マーカー出力                  │
└─────────────────────────────────────────────────────┘
```

### Phase 1: 初期化とデータロード

```
┌─────────────────────────────────────────────────────┐
│ 1. 外部データのロード                                │
│    - apidoc (embed.fnc, *.h のコメント)             │
│    - bindings.rs の型定義                           │
├─────────────────────────────────────────────────────┤
│ 2. C ヘッダーのパース（初回）                        │
│    - ThxCollector を callback として登録             │
│    - コメント内 apidoc の収集                        │
│    - typedef の収集                                 │
│    - inline 関数の型定義の収集                      │
│    - ※ パース完了時点で THX マクロ辞書が完成        │
└─────────────────────────────────────────────────────┘
```

### Phase 2: マクロの一次解析（1回のループ）

```
┌─────────────────────────────────────────────────────┐
│ 全ての is_target なマクロをループ                    │
│                                                     │
│ 各マクロに対して:                                    │
│ ├── TokenExpander で body を展開                    │
│ ├── def-use 関係を記録                              │
│ ├── is_thx_dependent は ThxCollector から取得       │
│ ├── 定数マクロと Rust 定数の紐付け                  │
│ └── 関数マクロの場合:                               │
│     ├── 式としてパースを試行                        │
│     ├── 失敗時: 文としてパースを試行                │
│     ├── パース成功時: 関数呼び出しの型制約を収集    │
│     └── パース結果を記録                            │
└─────────────────────────────────────────────────────┘
```

### Phase 3: 幅優先探索による型推論

```
┌─────────────────────────────────────────────────────┐
│ 初期分類                                            │
│ ├── 型確定マクロ: 全ての型情報が揃っているもの      │
│ └── 型不足マクロ: 一つでも型情報が欠落しているもの  │
├─────────────────────────────────────────────────────┤
│ 推論候補キューの構築                                │
│ - 型不足マクロのうち、使用するマクロが全て確定済み  │
│ - 使用マクロ数の少ない順にソート                    │
├─────────────────────────────────────────────────────┤
│ 推論ループ (キューが空になるまで)                   │
│                                                     │
│ ┌── キューからマクロを取り出す                      │
│ │                                                   │
│ ├── TokenExpander で body をマクロ展開              │
│ │   （マーカー付き）                                │
│ │                                                   │
│ ├── MacroBegin/MacroEnd マーカーを                  │
│ │   関数呼び出しトークン列に変換                    │
│ │                                                   │
│ ├── パース（式 or 文）                              │
│ │                                                   │
│ ├── 関数呼び出しの引数・戻り値の型から              │
│ │   型環境の制約を更新                              │
│ │                                                   │
│ ├── 型が確定したか判定                              │
│ │   ├── 確定 → 型確定辞書に登録                    │
│ │   └── 未確定 → 型不明辞書に登録                  │
│ │                                                   │
│ ├── 型確定時: このマクロを使うマクロを再判定        │
│ │   → 推論候補になれば queue 末尾に追加             │
│ │                                                   │
│ └── [デバッグモード] 型付き S式を出力               │
│                                                     │
│ 終了条件:                                           │
│ - queue が空                                        │
│ - または 確定/不明集合に変化なし                    │
└─────────────────────────────────────────────────────┘
```

---

## 改修対象

### preprocessor.rs の改修（Phase 0）

1. **MacroDefCallback トレイトの追加**
   - マクロ定義時に呼ばれるフック

2. **Preprocessor への callback フィールド追加**
   - `Option<Box<dyn MacroDefCallback>>`
   - `#define` 処理時にコールバックを呼び出し

### semantic.rs の改修

1. **型制約の収集API追加**
   - `collect_expr_constraints(expr, type_env)` - 式全体から型制約を収集（公開API）
   - `collect_call_constraints(...)` - 関数呼び出しから型制約を収集（内部メソッド）

2. **ExprId との連携**
   - 型推論時に ExprId を活用した制約管理

#### 呼び出し関係

```
macro_infer.rs
  │
  ├── Phase 2: analyze_macro()
  │     └── semantic.collect_expr_constraints(expr, type_env)
  │           └── (再帰的に式を走査)
  │                 └── collect_call_constraints(func, args, type_env)
  │
  └── Phase 3: infer_macro_types()
        └── semantic.collect_expr_constraints(expr, type_env)
              └── (同上)
```

#### macro_infer.rs からの呼び出し（疑似コード）

```rust
impl MacroInferContext {
    /// Phase 2: マクロの一次解析
    fn analyze_macro(&mut self, def: &MacroDef) -> MacroInferInfo {
        let mut info = MacroInferInfo::new(def.name);

        // TokenExpander で body を展開
        let expanded = self.expander.expand(&def.body);

        // パースを試行
        match self.try_parse_as_expr(&expanded) {
            Ok(expr) => {
                info.parse_result = ParseResult::Expression(expr.clone());

                // 式全体から型制約を収集
                // ここで collect_expr_constraints を呼ぶ
                self.semantic.collect_expr_constraints(&expr, &mut info.type_env);
            }
            Err(_) => {
                // 文としてパースを試行...
            }
        }

        info
    }

    /// Phase 3: 推論ループ内
    fn infer_macro_types(&mut self, info: &mut MacroInferInfo) {
        // マーカー付きで展開
        let expanded = self.expander.expand_with_markers(&info.body);

        // マーカーを関数呼び出しに変換してパース
        let tokens = self.convert_markers_to_calls(expanded);
        let expr = self.parse(&tokens)?;

        // 再度、型制約を収集（今度は確定済みマクロの情報を使える）
        self.semantic.collect_expr_constraints(&expr, &mut info.type_env);

        // 型が確定したか判定...
    }
}
```

#### semantic.rs の実装（疑似コード）

```rust
impl SemanticAnalyzer {
    /// 式全体から型制約を収集（再帰的に走査）
    pub fn collect_expr_constraints(&mut self, expr: &Expr, type_env: &mut TypeEnv) {
        match &expr.kind {
            ExprKind::Call { func, args } => {
                // 関数呼び出しから型制約を収集
                self.collect_call_constraints(expr.id, func, args, type_env);

                // 引数も再帰的に走査
                for arg in args {
                    self.collect_expr_constraints(arg, type_env);
                }
            }

            ExprKind::Binary { lhs, rhs, .. } => {
                self.collect_expr_constraints(lhs, type_env);
                self.collect_expr_constraints(rhs, type_env);
            }

            ExprKind::Ident(name) => {
                // パラメータ参照の場合、ExprId とパラメータを紐付け
                if self.is_macro_param(*name) {
                    type_env.link_expr_to_param(expr.id, *name);
                }
            }

            // ... 他のケースも再帰的に走査 ...
            _ => {}
        }
    }

    /// 関数呼び出しから型制約を収集（内部メソッド）
    fn collect_call_constraints(
        &mut self,
        call_expr_id: ExprId,
        func: &Expr,
        args: &[Expr],
        type_env: &mut TypeEnv,
    ) {
        // 関数名を取得
        let func_name = match &func.kind {
            ExprKind::Ident(name) => *name,
            _ => return, // 関数ポインタ等は一旦スキップ
        };

        // apidoc から関数シグネチャを検索
        if let Some(sig) = self.lookup_apidoc(func_name) {
            // 戻り値の型制約を追加
            type_env.add_constraint(TypeConstraint {
                expr_id: call_expr_id,
                ty: sig.return_type.clone(),
                source: TypeSource::Apidoc,
                context: format!("return of {}", self.interner.get(func_name)),
            });

            // 各引数の型制約を追加
            for (i, arg) in args.iter().enumerate() {
                if let Some(param_type) = sig.params.get(i) {
                    type_env.add_constraint(TypeConstraint {
                        expr_id: arg.id,
                        ty: param_type.clone(),
                        source: TypeSource::Apidoc,
                        context: format!("arg {} of {}", i, self.interner.get(func_name)),
                    });
                }
            }
        }

        // bindings.rs から関数シグネチャを検索
        if let Some(sig) = self.lookup_rust_decl(func_name) {
            // 同様に制約を追加（source は RustBindings）
            type_env.add_constraint(TypeConstraint {
                expr_id: call_expr_id,
                ty: sig.return_type.clone(),
                source: TypeSource::RustBindings,
                context: format!("return of {}", self.interner.get(func_name)),
            });
            // ...
        }

        // 確定済みマクロから検索
        if let Some(macro_sig) = self.lookup_confirmed_macro(func_name) {
            type_env.add_constraint(TypeConstraint {
                expr_id: call_expr_id,
                ty: macro_sig.return_type.clone(),
                source: TypeSource::Inferred,
                context: format!("macro {}", self.interner.get(func_name)),
            });
            // ...
        }
    }
}
```

### sexp.rs の改修

1. **TypedSexpPrinter の拡張**
   - ExprId ごとの型制約を出力するモード追加
   - `print_expr_with_type_env(expr, type_env)` メソッド

2. **出力フォーマット**
   ```lisp
   (binary :id 42 :op +
     :type-constraints ((c_int :source c-header) (i32 :source rust-bindings))
     :lhs (ident :id 40 :name x :type-constraints (...))
     :rhs (int-lit :id 41 :value 1))
   ```

---

## 実装順序

### Step 0: Preprocessor 改良（Phase 0）

1. **MacroDefCallback トレイトの定義**
   - `preprocessor.rs` に追加

2. **ThxCollector の実装**
   - `thx_collector.rs` を新規作成
   - aTHX, my_perl の検出ロジック

3. **TokenExpander の実装**
   - `token_expander.rs` を新規作成
   - 部分トークン列のマクロ展開
   - 関数マクロ呼び出しの展開
   - MacroBegin/MacroEnd マーカー出力

4. **Preprocessor への callback 統合**
   - `#define` 処理時にコールバック呼び出し

### Step 1: 基盤整備

1. `type_env.rs` を新規作成
   - TypeSource, TypeConstraint, TypeEnv の定義
   - 基本操作メソッド

2. `sexp.rs` の拡張
   - ExprId 出力オプション追加
   - 型制約出力メソッド追加

### Step 2: マクロ情報構造

1. `macro_infer.rs` を新規作成
   - MacroInferInfo, ParseResult, InferStatus の定義
   - MacroInferContext（全体を管理する構造体）

### Step 3: Phase 1-2 実装

1. 外部データロード機能
2. マクロの一次解析ループ
   - def-use 収集
   - パース試行
   - 初期型制約収集

### Step 4: Phase 3 実装

1. 初期分類ロジック
2. 推論候補キュー管理
3. 推論ループ本体
4. デバッグ用 S式出力

### Step 5: CLI 統合

1. 新しい推論モードのオプション追加
2. 途中経過出力オプション

---

## 成果物

### Phase 0（先行）
- `src/thx_collector.rs` - THX マクロ収集コールバック（新規）
- `src/token_expander.rs` - 部分トークン列用マクロ展開器（新規）
- `src/preprocessor.rs` - MacroDefCallback 追加（改修）

### Phase 1-3
- `src/type_env.rs` - 型環境モジュール（新規）
- `src/macro_infer.rs` - マクロ型推論エンジン（新規）
- `src/semantic.rs` - 型制約収集API追加（改修）
- `src/sexp.rs` - 型付きS式出力（改修）

---

## 検証方法

1. **Phase 0 の単体テスト**
   - ThxCollector が aTHX/my_perl を正しく検出するか
   - TokenExpander がマクロを正しく展開するか
   - 関数マクロ呼び出しの展開が正しく動作するか

2. **統合テスト**
   - samples/wrapper.h に対して推論を実行
   - 途中経過の S式出力を確認
   - 型確定/不明マクロの分類結果を確認

---

## 今回のスコープ外

- Rust コード生成（後続フェーズ）
- 型の同一性判定の厳密な実装（制約の収集のみ行う）
- パフォーマンス最適化
