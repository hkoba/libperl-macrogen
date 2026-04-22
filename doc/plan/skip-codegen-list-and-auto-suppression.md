# コード生成スキップ機構の拡張計画

## 目的の転換

これまでは「生成コードの正確性を上げて統合ビルドのエラーを減らす」正攻法で
進めてきた（56 → 38 err）。しかし本プロジェクトの本来価値は、
生成された `libperl-sys` を **実際に Rust プログラムから使って評価できる**
段階まで到達させることで、初めて発揮される。

そこで当面のゴールを以下に変更する:

**統合ビルドがエラー無く完遂し、libperl-sys が consumer crate から
利用可能になること。コード生成の正確性向上は、そのサブゴール。**

失敗する関数は `pub unsafe fn` を吐かずコメントブロックに置換する
（= 統合ビルド時の Rust コンパイルエラーが起きない）。その結果
consumer は「生成できた関数群」だけを呼び出せる。

---

## 現状調査

### 既存のスキップ機構 — 十分活用されている

`src/apidoc_patches.rs` が JSON ベースのパッチ集を提供している:

- `PatchKind::SkipCodegen` (`apidoc_patches.rs:115-117`) — 明示的に指定された
  macro/inline fn の codegen を抑止
- `apidoc_patches.skip_codegen: HashMap<String, String>` (name → reason)
- `skip_reason(name) -> Option<&str>` で検索
- デフォルトで `<apidoc>.patches.json` を自動ロード（`infer_api.rs:473-486`）

### コード生成側の分岐

inline 関数（`rust_codegen.rs:6140-6342`）と マクロ（同 6350-6400 付近）の
どちらも、生成結果を以下のいずれかに分類して **失敗時はコメント化** している:

```rust
enum InlineGenResult {
    Suppressed      { reason }    // ← skip_codegen 対象。[CODEGEN_SUPPRESSED]
    CallsUnavailable              // [CALLS_UNAVAILABLE]
    ContainsGoto                  // [CONTAINS_GOTO] (goto は未対応)
    UnresolvedNames { ... }       // [UNRESOLVED_NAMES]
    CodegenError    { ... }       // [CODEGEN_ERROR]
    Incomplete      { ... }       // [CODEGEN_INCOMPLETE]
    Success         { ... }       // 正常出力
}
```

**インフラは既に整っている**。つまり既存の `Suppressed` 経路に正しく
関数名を流し込めば、統合ビルドは通る。

### 残る 38 件の性格

これらは codegen 内では **Success 扱い**（= `pub unsafe fn` が吐かれる）だが、
Rust コンパイラ側で E0308 などになる。codegen 時点では型エラーを
検出できていない。現状 Phase 3（code emission）の型情報が不十分であり、
検出精度を上げるのはコスト高。

---

## 提案: 3 層のスキップ機構

### 層 1: テキストファイル `--skip-list` （ユーザ優先）

**シンプルで使い勝手の良い入口**を追加する。

```
# comment OK
# 関数名（マクロまたは inline）を 1 行に 1 つ
RXp_EXTFLAGS
HEK_LEN
Perl_sv_only_taint_gmagic
# inline 由来のケースも同じ形式
```

CLI:
```
libperl-macrogen ... --skip-codegen-list FILE [--skip-codegen-list FILE...]
```

Pipeline API:
```rust
Pipeline::builder(...)
    .with_skip_codegen_list("skip.txt")?   // ファイル
    .with_skip_codegen_names(&["RXp_EXTFLAGS"])  // 直接
```

内部動作: 既存の `ApidocPatchSet.skip_codegen` HashMap に
`name → "skip-list: <source>"` の形で merge する。JSON patches と
テキストリストが同時にあっても共存する（同名は patches 優先で OK）。

**長所**:
- 実装最小（既存経路にデータを流し込むだけ）
- consumer が追加依存なくコントロール可能
- build.rs から動的に生成して渡せる（ビルド 2 段階のワークフロー）

### 層 2: 自動検出（codegen 内シグナル）

「確実に失敗する」と判定できるケースは、codegen 内で
`CodegenError` を push することで自動的にコメント化される
（既に存在する仕組み）。以下を順次拡張:

#### 2a. Lvalue macro の展開失敗を codegen_errors に記録

`try_expand_call_as_lvalue_syn` (`rust_codegen.rs:1718-1741` 付近) が
param 整合性破綻で展開できなかった場合、現状は不正な Rust を出力
してしまう。代わりに:

- 展開可能か事前判定する `can_expand_lvalue_macro` を追加
- 可能でなければ `self.codegen_errors.push(format!("lvalue macro not expandable: ..."))` して早期 return
- 上位の CodegenError 分岐で `[CODEGEN_ERROR] X - lvalue macro not expandable` としてコメント化

**期待カバー**: 考察ドキュメントの カテゴリ C（5 件）+ F（2 件）+ G.4

#### 2b. 「型不明のまま式を扱うとコンパイルエラーになる」既知パターン

例:
- `Option<fn>` を呼び出しているが `unwrap_unchecked()` を挿入できない
  ケース
- pointer const 違いが cast できない（カテゴリ 2 の残余）
- `/* unknown */` を含む生成結果

これらは **codegen 出力文字列に `unknown` が残っている** 等で
検出可能。生成後に軽く self-check を走らせて `codegen_errors` に
追加する。

#### 2c. ビルド失敗ログから自動生成する `skip-list`

補助スクリプト `tools/build-error-to-skiplist.zsh`:
```
tmp/build-error.log を読み → エラー発生関数名を抽出 → skip-list 出力
```

2 段階ビルド運用:
1. 一旦すべて生成してビルド → error log
2. skip-list 自動生成 → もう一度生成（skip 適用）→ 成功

将来 2a/2b の自動検出が充実したら、スクリプトの出力はほぼ空になる。

### 層 3: consumer（libperl-sys）の feature 設定

`libperl-sys/build.rs` は `libperl-macrogen` を呼び出すはず。
そこで:
- `include_str!("skip_codegen.txt")` または `env!("CARGO_MANIFEST_DIR")/skip.txt`
  を Pipeline に渡す
- feature flag で切り替え可能にする

本計画の段階では **libperl-sys 側の変更は最小** に留め、層 1 の
API を「渡すべきファイルパスを受け取れる形」で整備することだけ。

---

## 実装計画（フェーズ分割）

### フェーズ 1（最小実装・1-2 コミット）

**目的**: 層 1 だけ動かす。統合ビルドで errors 0 を達成する道筋を開く。

1. **`ApidocPatchSet` にリスト API 追加**
   - `pub fn add_skip_codegen(&mut self, name: &str, reason: &str)`
   - `pub fn load_skip_list<P: AsRef<Path>>(path: P) -> io::Result<Vec<(String, String)>>`
     - 形式: 1 行 1 名、`#` コメント、空行無視
     - reason は `"skip-list: {filename}"`

2. **`InferConfig` に `skip_codegen_lists: Vec<PathBuf>` を追加**
   - pipeline.rs の `InferConfig` に field 追加
   - `PipelineBuilder::with_skip_codegen_list(path)` を追加
   - `infer_api.rs::run_infer()` で `apidoc_patches` に merge するロジックを
     追加（JSON ロード後、リストをマージ）

3. **CLI `--skip-codegen-list` 追加**
   - `main.rs` に `#[arg(long, value_name = "FILE")]` を複数指定可能に
   - builder にパイプ

4. **既存の `categorize-help-diags.tcl`（または新規 zsh スクリプト）**
   - build-error.log から失敗関数名を抽出して skip-list 形式で出力
   - `tmp/build-error.log` → `tmp/skip.txt` の変換

5. **確認**: `--skip-codegen-list tmp/skip.txt` で再ビルド → 0 err

### フェーズ 2（自動検出強化・4-6 コミット）

層 2 を拡充する。フェーズ 1 で生成される skip.txt を徐々に減らす。

1. **Lvalue macro 失敗検出（2a）** — 考察 C/F/G.4 の 7-8 件を自動化
2. **Success 出力の self-check（2b）**:
   - `/* unknown */`, `/* <unknown>... */`, 未定義シンボル等を grep
   - 見つかれば `codegen_errors` に push して `CodegenError` 経由でコメント化
3. **残りは skip-list で手動指定**（フェーズ 1 の仕組みで吸収）

### フェーズ 3（運用整備・1 コミット）

- `README.md` / `doc/reference-*.md` に skip-list の使い方追記
- `libperl-sys/build.rs` に流し込む方法をドキュメント化
- 空の `skip.txt` から始めて「何を落とすと通るか」を見える化

---

## 判断基準 — 何を自動スキップするか

### スキップして OK（= 誤検出リスク低）

- Lvalue macro で展開経路が破綻するもの（= 現状も壊れた Rust を吐いている）
- `ContainsGoto` / `CallsUnavailable`（既に実装済）
- 出力に `/* unknown */` が残るもの

### スキップしない方が良い（= 正常生成もありえるケース）

- 単なる const/mut cast ミスマッチ（タスク 2 で多くを既に解消）
- integer 幅ミスマッチ（タスク 4 で解消）

つまり **誤検出のリスクが高いのは「単純な型エラー系」。これは自動スキップ
対象にせず、skip-list（層 1）で手動指定する方針にする**。Lvalue や
unknown-token 等の「構造的・原理的な失敗」のみ自動化する。

---

## 期待される効果

### フェーズ 1 完了時点
- 統合ビルド **0 err** 達成可能（skip-list 経由）
- libperl-sys が consumer crate から使える状態に
- コード生成の正確性は未変更（＝ 38 err 分が skip される）

### フェーズ 2 完了時点
- 自動検出で 10-15 件を skip-list から外せる見込み
- skip-list のメンテ負荷が小さくなる

### フェーズ 3 完了時点
- consumer 向けのドキュメントが揃い、外部から利用可能

---

## 残る論点（要確認）

1. **skip-list のフォーマット**: 現在の提案は最もシンプルな「1 行 1 名」。
   将来 reason の追記・タグ付けが欲しくなったら `#` 後に書く運用 or
   `name: reason` の `:` 区切りに拡張する。初版では name のみでよいか?

2. **`libperl-sys/build.rs` 側の変更範囲**:
   本計画では「Pipeline API に `with_skip_codegen_list` を追加する」
   までを libperl-macrogen 側のスコープとする。libperl-sys が
   実際にどう渡すかは別コミット。build.rs の触り方は後で相談。

3. **skip-list のパス解決**:
   絶対パス / カレント相対 / apidoc と同じディレクトリ、どれを基準にするか。
   初版は CLI/Pipeline で受け取ったパスをそのまま open する
   （= 呼び出し側が解決する）シンプル運用が無難。

4. **既存の apidoc patches.json と skip-list の優先順**:
   - 同名が両方にある場合 → patches 側が勝つ（reason もそちら）
   - 両方 skip するので実質同じだが、エラーログで reason を区別したい
