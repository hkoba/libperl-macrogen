# Apidoc Patches アーキテクチャ

`perl` の C ヘッダや `=for apidoc` コメントには稀に誤りがあり、
そのまま codegen に渡すと不正な Rust が生成される。本モジュールは
**外部 JSON ファイルからパッチ情報を読み込んで apidoc データを訂正する
データ駆動型の補正機構**である。

## 動機

具体例:

| マクロ | バグ箇所 | 種類 |
|--------|---------|------|
| `RCPV_LEN`, `RCPV_REFCOUNT`, `RCPV_REFCNT_inc`, `RCPV_REFCNT_dec` | `cop.h:540-567` の `=for apidoc Am\|RCPV *\|...` が誤った戻り値型を宣言 | 戻り値型訂正 |
| `Perl_custom_op_xop` | `op.h:977` のマクロ定義が `aTHX_` を渡し忘れ → 引数数不一致 (E0061) | codegen 抑制 |

ハードコード（`src/` にバグリストを埋め込む）よりも、**バージョン別の
JSON ファイル**で外部化すれば:

- バージョン (`v$major.$minor`) ごとに上流バグの存在/消滅を追跡できる
- ライセンス的にも、上流側のバグレポートと修正状況を `reason` /
  `upstream_status` フィールドで記録できる
- 利用者は perl のバージョンに応じたパッチセットを差し替え可能

## ファイル構成

```
apidoc/
  v5.40.json              ← pre-generated apidoc data (再生成対象)
  v5.42.json
  ...
  common.patches.json     ← 全バージョン共通のパッチ（手動メンテ）
  v5.40.patches.json      ← v5.40 固有のパッチ・打ち消し（任意）
  v5.42.patches.json      ← v5.42 固有のパッチ・打ち消し（任意）
```

ローダ (`ApidocPatchSet::load_for_apidoc_path`) は次の 2 段マージで読む:

1. **`<dir>/common.patches.json`** を最初にロード（あれば）
2. **`<dir>/v$X.$Y.patches.json`** を上書きマージ（あれば）

両方とも存在しなくてよい（その場合 patch 適用は no-op）。

### マージ規則

- 同一 `name` のエントリは **後者（version-specific）が前者（common）を上書き**
- version-specific 側の `kind: "remove"` は common 側の同名エントリを **削除**
  （上流で fix されたバージョンでパッチを撤去する用途）
- `kind: "remove"` を common 側に書いても効果なし（打ち消す対象が存在しない）

### 配布

開発時はプロジェクトの `apidoc/` ディレクトリ、リリース時は
`~/.cache/libperl-macrogen/apidoc-v1.0/apidoc/` にダウンロード/抽出された
コピーが使われる（`build.rs` が tar.gz 化して埋め込み）。
詳細は [operations-apidoc-release.md](operations-apidoc-release.md) を参照。

## JSON スキーマ

```json
{
  "schema_version": 1,
  "comment": "free-form (optional)",
  "patches": [
    {
      "name": "RCPV_LEN",
      "kind": "return_type_override",
      "value": "STRLEN",
      "source_loc": "/usr/lib64/perl5/CORE/cop.h:560",
      "reason": "perl cop.h apidoc declares `RCPV *` but body returns len-1 (STRLEN)",
      "upstream_status": "to-report"
    },
    {
      "name": "Perl_custom_op_xop",
      "kind": "skip_codegen",
      "source_loc": "/usr/lib64/perl5/CORE/op.h:977",
      "reason": "macro lacks aTHX_; would generate 2-arg call to 3-arg fn",
      "upstream_status": "to-report"
    }
  ]
}
```

### フィールド

| フィールド | 必須 | 説明 |
|-----------|------|------|
| `schema_version` | ✓ | 現在 `1` のみ |
| `comment` |   | 自由記述（ファイル全体の意図） |
| `patches[].name` | ✓ | 対象 macro/function 名 |
| `patches[].kind` | ✓ | `return_type_override` / `arg_type_override` / `skip_codegen` / `remove` |
| `patches[].value` | kind 依存 | `*_override` 系で必須（C 型文字列） |
| `patches[].arg_index` | kind 依存 | `arg_type_override` で必須 |
| `patches[].source_loc` |   | バグ箇所 `/path:line`（デバッグ・上流報告用） |
| `patches[].reason` | ✓ | パッチが必要な理由（必須） |
| `patches[].upstream_status` |   | `to-report` / `reported:URL` / `merged` / `fixed-in-5.42` |

## パッチ種別 (`kind`)

### `return_type_override`

`ApidocDict.entries[name].return_type` を `value` で上書き。
inline comment 由来の entry にも、JSON 由来の entry にも適用可能。

### `arg_type_override`（将来用）

`ApidocDict.entries[name].args[arg_index].ty` を `value` で上書き。
初版未使用、必要時実装。

### `skip_codegen`

このマクロ/inline fn 自身の生成を抑制し、`[CODEGEN_SUPPRESSED]` コメントに
置換。マクロ本体の C コードがバグっていて訂正不能な場合の最後の手段。

**caller のカスケード抑止は Phase 2 で自動伝播される**。`MacroInferInfo`
（および `InlineFnDict`）には `apidoc_suppressed` フラグが用意されており、
`analyze_all_macros` の Step 4.4 で skip_codegen 各エントリ名に対して
立てられる。続く `propagate_unavailable_cross_domain` の四方向 fixpoint
（macro→macro, inline→inline, macro→inline, inline→macro）が
`is_unavailable_for_codegen()`（= `calls_unavailable || apidoc_suppressed`）
を起点として `calls_unavailable` を caller に立てるため、skip 対象を
直接または推移的に呼ぶマクロ／inline 関数も自動的に降格される。

このため **CI ビルド失敗を見て skip_codegen に名前を継ぎ足す運用は
不要**。skip_codegen には「上流バグまたは構造的に対応不能な構文を
持つ関数自身」のみを登録すれば、その caller 群は cascade で自然に
抑制される（CLAUDE.md「skip_codegen 運用ポリシー」参照）。

### `remove`

**version-specific ファイルでのみ意味がある**。`common.patches.json` で
登録されているパッチを当該 perl バージョンでだけ無効化する。上流が一部の
バージョンで fix した場合の打ち消し用。

```json
// apidoc/v5.42.patches.json
{
  "schema_version": 1,
  "patches": [
    { "name": "RCPV_LEN", "kind": "remove",
      "reason": "fixed in perl 5.42 commit abc123",
      "upstream_status": "merged" }
  ]
}
```

`value` などのフィールドは無視される。`name` と `reason` のみ意味を持つ。
common 側に書いても打ち消す対象が無いので no-op。

## パイプライン統合

```
Phase 2 (infer): src/infer_api.rs
    ApidocDict::load_auto(apidoc_path)
        + ApidocCollector::merge_into          ← inline =for apidoc
        + apidoc_patches.apply_to_apidoc       ← パッチ適用
        + apidoc.expand_type_macros            ← Off_t → off_t 等
    MacroInferContext::analyze_all_macros(..., Some(&apidoc_patches), ...)
      ├ Step 4.4: skip_codegen → apidoc_suppressed フラグ反映
      │   - MacroInferContext::apply_apidoc_suppressions
      │   - InlineFnDict::apply_apidoc_suppressions
      └ Step 4.7: propagate_unavailable_cross_domain
          - is_unavailable_for_codegen() 起点で四方向に伝播
          - caller には calls_unavailable=true が立つ
    InferResult.apidoc_patches を返す（reason 文字列の取得用）

Phase 3 (codegen): src/rust_codegen.rs
    generate_macros / generate_inline_fns:
      ├ 早期 SUPPRESSED: info.apidoc_suppressed が true なら
      │   [CODEGEN_SUPPRESSED] を出力（reason は apidoc_patches 経由）
      ├ CallsUnavailable:
      │   - called_functions が「存在しない名前」を含めば [CALLS_UNAVAILABLE]
      │   - そうでなければ（推移依存のみ）[CASCADE_UNAVAILABLE]
      └ 通常生成（success / type_incomplete / cascade 検査 / ...）
```

## 警告と検出

- **対象が見つからない警告**: パッチが指す名前 (`name`) が apidoc にも inline
  comment にもない場合、`stderr` に warning。perl 側で fix された等の状況を
  検知でき、撤去タイミングの目安になる。
- **適用ログ**: ロード時に「N 件適用、M 件 skip 登録」を `stderr` に出力。
- **skip_codegen 反映ログ**: `analyze_all_macros` Step 4.4 で
  `[apidoc-suppress] skip_codegen reflected: N macro(s) + M inline fn(s);
  K of T entries unmatched` を出力。`unmatched` が大きい場合、
  そのバージョンに存在しない関数名を skip-list に残している可能性が高く、
  クリーンアップ候補となる。
- **デバッグ**: 各 entry の `_patched: true` フラグ付与は将来用。現状は
  `--dump-apidoc-after-merge` で patch 適用後の状態を確認可能。

## 上流バグレポート

各 patch の `reason` フィールドはそのまま上流バグレポートのドラフトに使える:

```
File: /usr/lib64/perl5/CORE/op.h:977
Issue: Perl_custom_op_xop macro definition lacks aTHX_

  #define Perl_custom_op_xop(x) \
      (Perl_custom_op_get_field(x, XOPe_xop_ptr).xop_ptr)

Should be (matching XopENTRYCUSTOM at line 967):
  #define Perl_custom_op_xop(x) \
      (Perl_custom_op_get_field(aTHX_ x, XOPe_xop_ptr).xop_ptr)

Impact: FFI bindgen workflows generate a 2-arg call to a 3-arg function.
```

`upstream_status` を更新することで、レポート → マージ → リリース → 撤去
というライフサイクルを追跡できる。

## 関連ファイル

| ファイル | 役割 |
|---------|------|
| `src/apidoc_patches.rs` | スキーマ・ロード・適用ロジック |
| `apidoc/v$major.$minor.patches.json` | バージョン別パッチデータ（手動メンテ） |
| `src/infer_api.rs` | パイプライン統合（apidoc load 後に適用） |
| `src/rust_codegen.rs` | `skip_codegen` の codegen 側ハンドリング |
| `src/apidoc.rs` | 適用対象の `ApidocDict` 本体 |
