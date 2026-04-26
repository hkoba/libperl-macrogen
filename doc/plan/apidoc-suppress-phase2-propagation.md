# apidoc skip_codegen を Phase 2 で前倒し伝播する計画

## 背景と問題

`apidoc_patches.skip_codegen`（および skip-list ファイル経由で同集合に
マージされる名前）は、現状 **Phase 3（`rust_codegen.rs`）出力時にしか
参照されない**。このため:

1. **Phase 2 の `calls_unavailable` 伝播から漏れる**
   `check_function_availability` / `check_inline_fn_availability` /
   `propagate_unavailable_cross_domain`（`src/macro_infer.rs:1237`,
   `:1367`, `:1447`）は被呼び出し名を bindings/macros/inline-fns/builtins
   と照合するだけで、`apidoc_patches.skip_codegen` を一切見ない。
   よって skip_codegen 対象を呼ぶ caller には `calls_unavailable=true`
   が立たない。

2. **Phase 3 で 3 つの集合を個別管理しており部分的に漏れる**
   - `successfully_generated` (macro)
   - `successfully_generated_inlines`
   - `generatable_macros`（`precompute_macro_generability` で事前計算）

   `successfully_generated*` は早期 SUPPRESSED 分岐の前に追加されないので
   skip_codegen 対象を呼ぶ macro/inline は cascade で降格される。
   しかし **`precompute_macro_generability` は `apidoc_patches.skip_reason()`
   を参照していない**（`src/rust_codegen.rs:6187-6228`）。trial codegen が
   通る skip_codegen マクロは `generatable_macros` に入ってしまうため、
   それを呼ぶ inline 関数の cascade 検査（`src/rust_codegen.rs:6340`）が
   スルーされる。これが **inline → macro 抜け穴**。

3. **CI 失敗ログから skip_codegen を継ぎ足す運用が常態化する**
   抜け穴 (1)(2) があるため、本来 cascade で自動降格されるべき関数も
   個別に skip_codegen に追加せざるを得ず、リストが肥大化する。
   CLAUDE.md「skip_codegen 運用ポリシー」で禁止した運用そのもの。

## 中心方針

**「skip_codegen 対象は不在として伝播するが、フラグは別建て」**

- `MacroInferInfo` / `InlineFnDict` に **新フラグ** `apidoc_suppressed`
  を追加（既存 `calls_unavailable` とは別フィールド）。
- 伝播ロジックは「`calls_unavailable || apidoc_suppressed` を不可と
  みなす」一点だけを変更。`is_unavailable_for_codegen()` ヘルパーで集約。
- **`apidoc_suppressed` は直接の skip 対象自身にしか立てない**。
  伝播で他に伝染しても、伝染先には `calls_unavailable=true` が立つ
  （診断性のキモ: 「自分が skip 対象」「不在関数を呼ぶ」「skip 対象や
  不在関数に推移依存」を Phase 3 で区別したい）。

## データ構造変更

### `src/macro_infer.rs` — `MacroInferInfo`

```rust
pub struct MacroInferInfo {
    // 既存
    pub calls_unavailable: bool,
    // 新規
    pub apidoc_suppressed: bool,
}

impl MacroInferInfo {
    /// 出力可否の総合判定（伝播でも参照する）
    pub fn is_unavailable_for_codegen(&self) -> bool {
        self.calls_unavailable || self.apidoc_suppressed
    }
}
```

### `src/inline_fn.rs` — `InlineFnDict`

```rust
pub struct InlineFnDict {
    // 既存
    calls_unavailable: HashSet<InternedStr>,
    // 新規
    apidoc_suppressed: HashSet<InternedStr>,
}

impl InlineFnDict {
    pub fn is_apidoc_suppressed(&self, name: InternedStr) -> bool;
    pub fn set_apidoc_suppressed(&mut self, name: InternedStr);

    pub fn is_unavailable_for_codegen(&self, name: InternedStr) -> bool {
        self.is_calls_unavailable(name) || self.is_apidoc_suppressed(name)
    }
}
```

### 新規ヘルパー

```rust
impl MacroInferContext {
    /// apidoc skip_codegen を初期集合として apidoc_suppressed に反映
    pub fn apply_apidoc_suppressions(
        &mut self,
        patches: &ApidocPatchSet,
        interner: &StringInterner,
    );
}

impl InlineFnDict {
    pub fn apply_apidoc_suppressions(
        &mut self,
        patches: &ApidocPatchSet,
        interner: &StringInterner,
    );
}
```

### `analyze_all_macros` シグネチャ拡張

```rust
pub fn analyze_all_macros<'a>(
    &mut self,
    pp: &mut Preprocessor,
    apidoc: Option<&'a ApidocDict>,
    apidoc_patches: Option<&'a ApidocPatchSet>,  // ← 追加
    fields_dict: Option<&'a FieldsDict>,
    rust_decl_dict: Option<&'a RustDeclDict>,
    mut inline_fn_dict: Option<&'a mut InlineFnDict>,
    c_fn_decl_dict: Option<&'a CFnDeclDict>,
    typedefs: &HashSet<InternedStr>,
    thx_symbols: (InternedStr, InternedStr, InternedStr),
    no_expand: NoExpandSymbols,
);
```

呼び出し元は `infer_api.rs:552-562` の 1 箇所のみ。

## analyze_all_macros 内部の Step 構成

```
Step 1     構築（既存）
Step 1.5   THX 検出（既存）
Step 2     used_by 構築（既存）
Step 3     THX 伝播（既存）
Step 4     ## 伝播（既存）
Step 4.4   ★ 新規: apidoc_suppressed をマクロ/inline に反映
Step 4.5   check_function_availability（is_unavailable_for_codegen 経由）
Step 4.6   check_inline_fn_availability（同上）
Step 4.7   propagate_unavailable_cross_domain（同上）
Step 5-6   既存
```

## 伝播ロジックの修正点

`check_function_availability` / `check_inline_fn_availability` /
`propagate_unavailable_cross_domain` の各 4 方向 (a)-(d) で、
被呼び出し先の判定を `is_unavailable_for_codegen()` ベースに統一。

例 (c) macro → inline:

```rust
let has_unavailable_inline = called_fns.iter().any(|called| {
    inline_fn_dict.get(*called).is_some()
        && inline_fn_dict.is_unavailable_for_codegen(*called)
});
```

ただし **`calls_unavailable` フラグ自体を立てる側は変更しない**。
`apidoc_suppressed` は伝播で他に伝染するが、伝染先には
`calls_unavailable=true` が立つ。`apidoc_suppressed` は直接の skip 対象
自身にしか立てない。

## Phase 3 側の整理

### 修正対象

- `precompute_macro_generability`（`src/rust_codegen.rs:6187`）の
  cascade 検査内 `!u.calls_unavailable` を `!u.is_unavailable_for_codegen()`
  に置換 → 抜け穴 (a) 解消
- 早期 SUPPRESSED 分岐（`src/rust_codegen.rs:6266-6272`, `:6498-6506`）
  は **残す**（高速パス）が、判定を `info.apidoc_suppressed`
  参照に切り替え（reason 文字列の取得は引き続き
  `result.apidoc_patches.skip_reason()`）

### メッセージ分岐

```
[CODEGEN_SUPPRESSED]   ← apidoc_suppressed == true
                          （自分が直接 skip 対象、reason 表示）
[CALLS_UNAVAILABLE]    ← calls_unavailable == true かつ
                          called_functions に bindings/macros/inline どれにも
                          無い名前を含む（純粋に不在関数を呼ぶ）
[CASCADE_UNAVAILABLE]  ← calls_unavailable == true かつ上記いずれでもない
                          （推移的に unavailable な依存先を呼ぶ）
```

具体的な原因表示は `called_functions` を再走査して、
- `apidoc_suppressed` な依存先
- 純粋に不在の名前
- `calls_unavailable` な依存先

を区別して列挙する。

## テスト戦略

- **単体テスト追加**:
  - `MacroInferContext::apply_apidoc_suppressions` /
    `InlineFnDict::apply_apidoc_suppressions` が patches の名前を
    正しく `apidoc_suppressed` に反映するか
  - `propagate_unavailable_cross_domain` が apidoc_suppressed を起点として
    4 方向すべてに伝播するか（macro→macro, inline→inline, macro→inline,
    inline→macro）
- **統合テスト**: 既存の `samples/skip-list-*.txt`（あれば）を使った
  ケースで `[CODEGEN_SUPPRESSED]` / `[CASCADE_UNAVAILABLE]` の総数を
  比較
- **回帰テスト**: 既存 299 テスト全件 pass

## リスクと対処

| リスク | 対処 |
|---|---|
| skip_codegen 対象が inline_fn_dict にも macros にも存在しない（名前ミス・古いリスト） | `apply_apidoc_suppressions` で missing 名を `eprintln!` |
| 既存 `calls_unavailable` 利用箇所の見落とし | `calls_unavailable\|is_calls_unavailable` を grep で機械的に列挙し置換要否レビュー |
| Phase 3 メッセージ reason の取得元 | `apidoc_patches` は `InferResult` 経由で見えるので CODEGEN_SUPPRESSED の reason 取得は変えない |
| 統合ビルド err 数の変動 | 抜け穴解消で増減し得る。CLAUDE.md ポリシーに従い、漏れた cascade は根本原因を直す方向で対応 |

## 実装ステップ（コミット粒度）

1. **コミット 1**: `MacroInferInfo.apidoc_suppressed` /
   `InlineFnDict.apidoc_suppressed` フィールド追加 + accessor +
   `is_unavailable_for_codegen()` ヘルパー（伝播未変更、振る舞い不変）
2. **コミット 2**: `apply_apidoc_suppressions` 実装 +
   `analyze_all_macros` シグネチャ拡張 + 呼び出し元更新（フラグは立つが
   伝播でまだ読まれない、振る舞い不変）
3. **コミット 3**: `check_function_availability` /
   `check_inline_fn_availability` / `propagate_unavailable_cross_domain`
   を `is_unavailable_for_codegen` ベースに切替（**挙動変化点**）
4. **コミット 4**: `precompute_macro_generability` の判定を
   `is_unavailable_for_codegen` に切替（抜け穴 (a) 修正）
5. **コミット 5**: Phase 3 早期 SUPPRESSED 分岐を `info.apidoc_suppressed`
   参照に切替 + メッセージ分岐の整理
6. **コミット 6**: ドキュメント更新（`doc/architecture-*` の該当箇所、
   既存 `doc/plan/skip-codegen-list-and-auto-suppression.md` の現状反映）

各コミットで `cargo test` を通す。コミット 3-4 で統合ビルドのエラー件数が
変動するはずなので、変動した関数群を精査し、必要なら個別に対処。
