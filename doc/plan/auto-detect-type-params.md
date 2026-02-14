# Plan: マクロ引数の型パラメータ自動検出と generic 関数生成

## 目標

マクロの引数がキャスト式の型として使用されていることをパーサー内で自動検出し、
Rust の generic 関数として生成する。

## 背景

### 対象マクロ（is_target ヘッダー内）

| マクロ | 定義 | 現状 |
|--------|------|------|
| `xV_FROM_REF(XV, ref)` | `((XV *)(SvRV(ref)))` | PARSE_FAILED |
| `INT2PTR(any, d)` | `(any)(d)` | `any(d)` と誤解釈、CALLS_UNAVAILABLE |
| `NUM2PTR(any, d)` | `(any)(PTRV)(d)` | 同上 |

### 問題の原因

パーサーの `is_type_start()` は `typedefs` セットに登録された識別子のみを型名と認識する。
マクロの仮引数名（`XV`, `any`）は `typedefs` に含まれないため:

- `(XV *)expr` → `XV` が型と認識されず、パース失敗
- `(any)(d)` → `any` が型と認識されず、関数呼び出し `any(d)` として解釈

### 期待される出力

```rust
pub unsafe fn xV_FROM_REF<T>(r#ref: *mut SV) -> *mut T {
    unsafe { (SvRV(r#ref) as *mut T) }
}

pub unsafe fn INT2PTR<T>(d: IV) -> T {
    unsafe { (d as T) }
}
```

### 既存のインフラ

`MacroInferInfo.generic_type_params: HashMap<i32, String>` と
`collect_generic_params`（apidoc 由来）が既に存在する。

コード生成側も `build_generic_clause`, `build_param_list`, `get_return_type` で
`generic_type_params` を使用している。ただし現在は apidoc エントリが必要。

## 設計方針

### 根本的アプローチ: パーサーにマクロパラメータ情報を渡す

マクロ本体のパース開始時に、パーサーへマクロの全仮引数名を
`generic_params` 辞書として渡す。パーサーの `parse_cast_expr` が
キャスト式の文脈で generic param を検出したとき、先読み（lookahead）で
型としての使用かどうかを判定する。

この方法により:
- 事前のトークンスキャン不要
- パーサーの文法規則に基づく正確な判定
- 検出ロジックがパーサーに一元化

### キャスト/式の曖昧性と先読みによる解消

`parse_cast_expr` で `(` の後に generic param が来た場合、
後続トークンを先読みして判定する:

| パターン | 先読み1 | 先読み2 | 判定 |
|----------|---------|---------|------|
| `(XV *)...` | `*` | — | キャスト（ポインタ型） |
| `(any)(d)` | `)` | `(` | キャスト（値型, 後続に式あり） |
| `(any)name` | `)` | `Ident` | キャスト（値型, 後続に式あり） |
| `(d)` 末尾 | `)` | `Eof` | 括弧付き式（後続に式なし） |
| `(a + b)` | `+` | — | 括弧付き式（宣言子にならない） |

先読み1 = generic param の次のトークン、先読み2 = `)` の次のトークン。

### 検出の蓄積

一度型と判定された仮引数名は `detected_type_params: HashSet<InternedStr>` に記録。
`is_type_start` がこのセットも参照することで、同じパラメータの2回目以降の出現では
先読みなしで型として認識される。

## 実装

### Phase 1: Parser への generic_params 導入

**ファイル**: `src/parser.rs`

#### 1a. Parser 構造体にフィールド追加

```rust
pub struct Parser<'a, S: TokenSource> {
    source: &'a mut S,
    current: Token,
    typedefs: HashSet<InternedStr>,
    /// マクロ仮引数の辞書（マクロ本体パース時のみ使用）
    /// key: 仮引数名, value: パラメータインデックス
    generic_params: HashMap<InternedStr, usize>,
    /// パース中に型として使用が検出された generic param
    detected_type_params: HashSet<InternedStr>,
    // ... 既存フィールド
}
```

#### 1b. コンストラクタ拡張

`parse_expression_from_tokens_ref_with_stats` に generic_params を渡す
新しいインターフェースを追加:

```rust
pub fn parse_expression_from_tokens_ref_with_generic_params(
    tokens: Vec<Token>,
    interner: &StringInterner,
    files: &FileRegistry,
    typedefs: &HashSet<InternedStr>,
    generic_params: HashMap<InternedStr, usize>,
) -> Result<(Expr, ParseStats, HashSet<InternedStr>)>
//                               ^^^ detected_type_params を返す
```

#### 1c. `is_type_start` の拡張

```rust
fn is_type_start(&self) -> bool {
    match &self.current.kind {
        // ... 既存の型キーワード ...
        TokenKind::Ident(id) => {
            self.typedefs.contains(id) || self.detected_type_params.contains(id)
        }
        _ => false,
    }
}
```

#### 1d. `parse_cast_expr` の拡張

```rust
fn parse_cast_expr(&mut self) -> Result<Expr> {
    if self.check(&TokenKind::LParen) {
        let loc = self.current.loc.clone();
        self.advance()?; // consume (

        // 1. 確定的な型名（キーワード/typedef/既検出の generic param）
        if self.is_type_start() {
            return self.finish_parse_cast_or_compound_lit(loc);
        }

        // 2. 未検出の generic param → 先読みで判定
        if let Some(id) = self.current_ident() {
            if self.generic_params.contains_key(&id)
                && self.looks_like_generic_cast()
            {
                // 型パラメータとして記録
                self.detected_type_params.insert(id);
                return self.finish_parse_cast_or_compound_lit(loc);
            }
        }

        // 3. 括弧付き式 or ステートメント式
        if self.check(&TokenKind::LBrace) {
            // ... ステートメント式 ...
        } else {
            let expr = self.parse_expr()?;
            self.expect(&TokenKind::RParen)?;
            return self.parse_postfix_on(expr);
        }
    }
    self.parse_unary_expr()
}
```

#### 1e. 先読みメソッド

```rust
/// generic param がキャスト式の型として使われているかを先読みで判定
///
/// 呼び出し時点: current = generic param の Ident
/// TokenSource::next_token + unget_token で先読み
fn looks_like_generic_cast(&mut self) -> bool {
    // 先読み1: param の次のトークン
    let next1 = match self.source.next_token() {
        Ok(t) => t,
        Err(_) => return false,
    };

    let result = match &next1.kind {
        // (PARAM *...) — ポインタキャスト
        TokenKind::Star => true,

        // (PARAM) — 値キャスト候補、後続の文脈で判定
        TokenKind::RParen => {
            let next2 = match self.source.next_token() {
                Ok(t) => t,
                Err(_) => {
                    self.source.unget_token(next1);
                    return false;
                }
            };
            // ) の後に式の開始トークンが続くならキャスト
            let is_cast = matches!(&next2.kind,
                TokenKind::LParen     // (PARAM)(expr)
                | TokenKind::Ident(_) // (PARAM)ident
                | TokenKind::IntLit(_) | TokenKind::UIntLit(_)
                | TokenKind::FloatLit(_)
                | TokenKind::StringLit(_) | TokenKind::CharLit(_)
                | TokenKind::Star     // (PARAM)*ptr (deref)
                | TokenKind::Amp      // (PARAM)&x
                | TokenKind::Minus    // (PARAM)-x
                | TokenKind::Bang     // (PARAM)!x
                | TokenKind::Tilde    // (PARAM)~x
                | TokenKind::KwSizeof // (PARAM)sizeof(x)
            );
            self.source.unget_token(next2);
            is_cast
        }

        // (PARAM + ...) など — 括弧付き式
        _ => false,
    };

    self.source.unget_token(next1);
    result
}
```

#### 1f. `finish_parse_cast_or_compound_lit` の抽出

`parse_cast_expr` の既存キャスト処理ロジックをメソッドに分離。
`detected_type_params` に追加されたパラメータが `parse_type_name` →
`parse_decl_specs` → `is_type_start` で正しく型と認識されるようにする。

### Phase 2: macro_infer からの呼び出し

**ファイル**: `src/macro_infer.rs`

#### 2a. `try_parse_tokens` での generic_params 受け渡し

```rust
fn try_parse_tokens(
    &self,
    tokens: &[Token],
    interner: &StringInterner,
    files: &FileRegistry,
    typedefs: &HashSet<InternedStr>,
    generic_params: HashMap<InternedStr, usize>,  // 追加
) -> (ParseResult, ParseStats, HashSet<InternedStr>)
//                              ^^^ detected_type_params
```

#### 2b. `build_macro_info` での generic_params 構築

```rust
// パラメータ名 → インデックスの辞書を構築
let generic_params: HashMap<InternedStr, usize> = params.iter()
    .enumerate()
    .map(|(i, &name)| (name, i))
    .collect();

let (parse_result, stats, detected_type_params) =
    self.try_parse_tokens(&expanded_tokens, interner, files, typedefs, generic_params);

info.parse_result = parse_result;

// 検出された型パラメータを generic_type_params にマッピング
if !detected_type_params.is_empty() {
    let param_names = ['T', 'U', 'V', 'W', 'X', 'Y', 'Z'];
    let mut idx = 0;
    for (i, param) in params.iter().enumerate() {
        if detected_type_params.contains(param) && idx < param_names.len() {
            info.generic_type_params.insert(i as i32, param_names[idx].to_string());
            idx += 1;
        }
    }
}
```

### Phase 3: コード生成での型パラメータ置換

**ファイル**: `src/rust_codegen.rs`

#### 3a. 型パラメータマップの管理

`RustCodegen` 構造体にフィールド追加:

```rust
/// 現在生成中のマクロの型パラメータマップ
/// 仮引数名(InternedStr) → ジェネリック名("T", "U", ...)
current_type_param_map: HashMap<InternedStr, String>,
```

マクロ関数生成の前後で設定・クリア:

```rust
// 生成前: info.generic_type_params + info.params からマップ構築
self.current_type_param_map = info.generic_type_params.iter()
    .filter(|(&idx, _)| idx >= 0)
    .filter_map(|(&idx, name)| {
        info.params.get(idx as usize).map(|p| (p.name, name.clone()))
    })
    .collect();

// ... 関数生成 ...

// 生成後: クリア
self.current_type_param_map.clear();
```

#### 3b. `decl_specs_to_rust` での置換

AST 経由のキャスト式（関数本体）で型パラメータを置換:

```rust
fn decl_specs_to_rust(&mut self, specs: &DeclSpecs) -> String {
    for spec in &specs.type_specs {
        if let TypeSpec::TypedefName(name) = spec {
            // 型パラメータなら generic 名に置換
            if let Some(generic_name) = self.current_type_param_map.get(name) {
                return generic_name.clone();
            }
            return self.interner.get(*name).to_string();
        }
    }
    // ... 既存コード
}
```

#### 3c. 戻り値型の置換

`type_repr_to_rust` でも型パラメータ置換を行う:

```rust
fn type_repr_to_rust(&mut self, ty: &TypeRepr) -> String {
    let result = ty.to_rust_string(self.interner);
    let result = self.substitute_type_params(&result);
    if result.contains("/*") {
        self.incomplete_count += 1;
    }
    result
}

/// 型文字列中の型パラメータ名を generic 名に置換
fn substitute_type_params(&self, type_str: &str) -> String {
    if self.current_type_param_map.is_empty() {
        return type_str.to_string();
    }
    let mut result = type_str.to_string();
    for (param_name, generic_name) in &self.current_type_param_map {
        let name_str = self.interner.get(*param_name);
        result = result.replace(name_str, generic_name);
    }
    result
}
```

### Phase 4: テストと検証

1. `cargo build` / `cargo test`
2. 出力確認: `xV_FROM_REF`, `INT2PTR`, `NUM2PTR` の生成コード
3. 回帰テスト

## 処理フロー全体

```
build_macro_info:
  1. expand_macro_body_for_inference → expanded_tokens
  2. generic_params = { XV → 0, ref → 1 }  (全仮引数)
  3. try_parse_tokens(typedefs, generic_params)
     Parser 内部:
       parse_cast_expr で (XV を検出
       → generic_params に XV あり
       → 先読み: 次が * → キャスト確定
       → detected_type_params に XV を追加
       → is_type_start(XV) → true（detected_type_params にある）
       → parse_type_name → TypedefName("XV"), pointer
  4. 返却: (AST, stats, detected_type_params = {XV})
  5. generic_type_params = { 0 → "T" }

infer_macro_types:
  6. collect_expr_constraints → Cast 制約: TypeRepr(TypedefName("XV"), Pointer)

generate_macro_function:
  7. current_type_param_map = { XV → "T" }
  8. build_generic_clause → "<T>"
  9. build_param_list → "r#ref: *mut SV" (XV は除外)
 10. get_return_type → type_repr_to_rust("*mut XV") → substitute → "*mut T"
 11. expr_to_rust(body) → Cast で decl_specs_to_rust("XV") → "T"
     → "(SvRV(r#ref) as *mut T)"
 12. current_type_param_map.clear()
```

## 変更ファイル一覧

| ファイル | 変更内容 |
|----------|----------|
| `src/parser.rs` | `generic_params`, `detected_type_params` フィールド追加、`is_type_start` 拡張、`parse_cast_expr` に先読みロジック追加、新しいパース関数 |
| `src/macro_infer.rs` | `try_parse_tokens` に generic_params 引数追加、`build_macro_info` で全仮引数を渡し結果をマッピング |
| `src/rust_codegen.rs` | `current_type_param_map` フィールド追加、`decl_specs_to_rust` での置換、`type_repr_to_rust` での置換 |

## エッジケース

1. **型パラメータが2つ**: `MACRO(T, U, expr)` → `<T, U>`
   - 既存の PARAM_NAMES 配列で対応

2. **apidoc エントリが既にある場合**: `collect_generic_params` で上書きされる
   - apidoc の型情報が優先される（既存動作と整合）

3. **パラメータ名が実在の型名と同じ場合**: `MACRO(SV, expr)` で `SV` が仮引数かつ typedef
   - `is_type_start` は typedefs を先にチェックするため、通常のキャストとしてパースされる
   - 型パラメータの自動検出は起きない（generic_params チェックに到達しない）
   - → generic 化されず、具象型のまま生成される（安全側の動作）

4. **値パラメータの誤検出防止**: `(d)` がマクロ本体末尾にある場合
   - 先読み2: `)` の後が `Eof` → 式の開始トークンではない → キャストと判定しない
   - → 括弧付き式として正しくパースされる

5. **sizeof(PARAM)**: `sizeof` 内の generic param は別の文脈
   - `parse_unary_expr` の sizeof 処理は `is_type_start()` を使用
   - `detected_type_params` に既にあれば型として認識される
   - 未検出なら式として扱われる（sizeof(expr) として正しい動作）
