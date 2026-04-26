# 非 threaded perl 対応計画

## 背景

これまで本プロジェクトは **threaded perl (`-Dusethreads`)** を前提にコード
生成してきた。具体的には:

- 関数シグネチャ先頭に `my_perl: *mut PerlInterpreter` を自動注入
- 呼び出し先が `is_thx_dependent` なら `my_perl,` を引数列に自動挿入
- bindings.rs / apidoc は threaded build から取得したものを利用

しかし perl は `-Uusethreads` でビルドすることもできる。非 threaded perl
では:

- `pTHX_` / `aTHX_` などのマクロが **空に展開** される
- 関数は `my_perl` 引数を取らない (`Perl_foo(int x)` のまま)
- `PL_curcop` 等のグローバルが **実 extern 変数** になる
  （threaded では `(*my_perl).Icurcop` というマクロ展開）
- bindgen 出力 (bindings.rs) もこれらを反映

Perl の C API はかつて非 threaded がデフォルトだった経緯があり、
組み込み perl や軽量化目的の perl では今でも非 threaded ビルドが選ばれる。
本対応で生成された `libperl-sys` がそうした perl 環境にも追従できるように
する。

## 現状調査

### 既に動的に追従できる部分

`src/perl_config.rs::get_perl_config()` は `perl -V:cppsymbols` /
`perl -V:ccflags` から実 perl の `-D` フラグを取得して preprocessor に渡す。
このため非 threaded perl 上で動かせば:

- `PERL_IMPLICIT_CONTEXT` / `MULTIPLICITY` / `USE_ITHREADS` が **未定義**
- `pTHX_` は空展開、`aTHX_` も空展開
- 既存の `MacroInferInfo.is_thx_dependent` 検出（展開後トークン中の
  `aTHX`, `tTHX`, `my_perl` を探す）は自然に **false** を返す
- bindings.rs (bindgen 出力) も非 threaded のシグネチャになる

つまり **大部分は preprocessor / bindgen の差し替えで自動的に動く** はず。

### 動かない / 危険なポイント

1. **`pTHX_` callback (`infer_api.rs:302-305`)**
   - `set_macro_called_callback` は **マクロが呼び出されたか** を見る。
     展開結果が空でもマクロ呼び出し自体は発生するため、非 threaded でも
     `pTHX_` 呼び出しは検出され、`CFnDecl.is_thx = true` が立ってしまう。
   - 一方 bindings.rs にはその関数に `my_perl` パラメータがない。
   - `MacroInferInfo` 側の `is_thx_dependent` 推移伝播
     (`macro_infer.rs:1172-1180`) が `c_fn_dict.is_thx_dependent(fn_name)`
     を見ているため、誤って THX 依存とマークされ、
     `is_thx_dependent` が立ったマクロには **存在しない `my_perl` が
     インジェクトされる**。

2. **`is_thx_dependent` を信じる codegen 各所**
   ```
   rust_codegen.rs:2326-2336  needs_my_perl_for_call
   rust_codegen.rs:2465-2470  arg_index 0 = my_perl 扱い
   rust_codegen.rs:2507-2510  arg_index オフセット
   rust_codegen.rs:2915-2918  関数シグネチャに my_perl 追加
   rust_codegen.rs:3257-3258  THX マクロ呼び出しの zip ずれ補正
   rust_codegen.rs:4251-4296  実引数列に my_perl 自動挿入
   rust_codegen.rs:5421-5424  inline 関数の THX 検出
   rust_codegen.rs:5482-5490  最初のパラメータ名 == "my_perl" で判定
   ```
   どれも threaded 前提。非 threaded で `is_thx_dependent=false` が
   一貫していれば自然と発火しないが、(1) のような誤検出が混入すると
   破綻する。

3. **存在を仮定している `PerlInterpreter` / `my_perl`**
   - `rust_codegen.rs:206` のシンボル定数列
   - 非 threaded の bindings.rs にも `PerlInterpreter` 型は通常存在する
     （構造体定義自体は両方の build にある）ため、型の存在は問題なし
   - ただし「`my_perl` という名前のローカル変数 / パラメータが常に
     スコープにある」前提はもはや成立しない

4. **inline 関数の THX 判定** (`rust_codegen.rs:5482-5490`)
   - 「最初のパラメータ名が `my_perl` であれば THX 依存」
   - 非 threaded perl の inline 関数は `my_perl` パラメータを持たない
     ので、自然に `false`。これは OK。

5. **apidoc/*.json の建付け**
   - 現在の apidoc データは threaded build でダンプしたもの
   - 関数のパラメータ列に `my_perl` が含まれる（threaded 表現）
   - 非 threaded build に対しては、bindings.rs と apidoc の表現が
     **不一致** になる（bindings は引数なし、apidoc は引数あり）
   - apidoc には `T` フラグ (`no_thread_ctx`) が存在するが現状未使用
     (`apidoc.rs:67, 145`)

6. **`#define PL_curcop (*my_perl).Icurcop` の期待**
   - 既存 codegen が `(*my_perl).Ixxx` というパターンを特別扱い
     しているか要確認（grep 上は明示的なパスは見つからない）
   - 非 threaded では `PL_curcop` がそのまま token として現れ、
     `RustDeclDict.statics` で解決する経路に入る想定
   - 最近のコミット
     `eaacfe1`/`2f6ccfa`/`bb9ab8e` で extern static 配列処理が
     拡充済みなので、非配列 static (`PL_curcop` など) も同様に
     処理できる可能性が高い

## 設計

### 全体方針

**Option A: auto-detect（推奨）**

- ビルド時に対象 perl の `Config{usethreads}` を取得し、`PerlBuildMode`
  で表現
- 同一の libperl-macrogen で非 threaded / threaded どちらの
  `libperl-sys` も生成可能
- 出力は単一フレーバー（`#[cfg]` ガードなし）。perl が変われば
  再生成

代替案 A は不採用:

- **Option B (cfg ガード両対応)**: 単一ソースに
  `#[cfg(perl_threaded)]` を散らす方式。consumer 側で feature
  選択できる利点はあるが、`my_perl` パラメータの有無がシグネチャ
  自体に影響するため `#[cfg]` のメンテが過大。
- **Option C (両フレーバー個別生成)**: 出力を
  `libperl-sys-threaded` / `libperl-sys-nonthreaded` に分離。冗長。

### `PerlBuildMode` の表現

新規 enum を `src/perl_config.rs` に追加:

```rust
/// 対象 perl の build mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PerlBuildMode {
    /// `-Dusethreads` ありの perl（PERL_IMPLICIT_CONTEXT 定義済）
    /// 関数は `my_perl: *mut PerlInterpreter` を第一引数に取る
    Threaded,
    /// `-Uusethreads`（または明示なし）の perl
    /// pTHX_ / aTHX_ は空展開、my_perl パラメータなし
    NonThreaded,
}

impl PerlBuildMode {
    /// perl Config から自動検出
    ///
    /// 判定基準（優先順）:
    /// 1. `Config{usethreads}` == "define" → Threaded
    /// 2. `cppsymbols` に `MULTIPLICITY` または `PERL_IMPLICIT_CONTEXT`
    ///    が含まれる → Threaded
    /// 3. それ以外 → NonThreaded
    pub fn detect_from_perl_config() -> Result<Self, PerlConfigError>;

    pub fn is_threaded(self) -> bool { matches!(self, Self::Threaded) }
}
```

`PerlConfig` 構造体にもフィールドを追加:

```rust
pub struct PerlConfig {
    pub include_paths: Vec<PathBuf>,
    pub defines: Vec<(String, Option<String>)>,
    pub build_mode: PerlBuildMode,  // 新規
}
```

### CLI / 設定への伝播

`InferConfig` (`src/pipeline.rs` 周辺) に `perl_build_mode` を追加。
`get_perl_config()` 経由で auto-detect、または CLI で明示指定可能:

```
libperl-macrogen ... --perl-build-mode threaded
libperl-macrogen ... --perl-build-mode non-threaded
libperl-macrogen ...                              # auto-detect
```

CLI フラグは「perl Config を読み取れない環境」「テスト用に強制設定」
の両方の用途。デフォルトは auto-detect。

### 検出ロジックの mode 化

#### 1. `pTHX_` / `pTHX` callback の抑止

```rust
// infer_api.rs:301-305 付近
if config.perl_build_mode.is_threaded() {
    let pthx_id = pp.interner_mut().intern("pTHX_");
    let pthx_no_comma_id = pp.interner_mut().intern("pTHX");
    pp.set_macro_called_callback(pthx_id, ...);
    pp.set_macro_called_callback(pthx_no_comma_id, ...);
}
```

非 threaded では callback 自体を登録しない → `CFnDecl.is_thx` は
全関数で false。

#### 2. `MacroInferInfo.is_thx_dependent` 検出の mode 化

`macro_infer.rs:799-810` の検出ロジック:

```rust
let has_thx = if config.perl_build_mode.is_threaded() {
    let has_thx_from_uses = info.uses.contains(&sym_athx) || info.uses.contains(&sym_tthx);
    let has_my_perl = expanded_tokens.iter().any(|t| ...);
    has_thx_from_uses || has_my_perl
} else {
    false  // 非 threaded では誰も my_perl を必要としない
};
```

伝播 (`macro_infer.rs:1172-1180`, `1255-1290`) も同様に mode で
ガード（または `c_fn_dict.is_thx_dependent` が常に false なら自然
に no-op）。

#### 3. codegen の mode 化

`is_thx_dependent` 系のすべての使用箇所を、`config.perl_threaded` も
合わせて参照するように変える。最も簡単な実装は **`is_thx_dependent` を
立てる側を mode で制御** すること（上記 #1, #2）。これで読み取り側の
ロジックは触らずに済む。

ただし防御的に `RustCodegen` 側にも `perl_threaded: bool` を持たせ、
`is_thx_dependent` を読む箇所すべてで `perl_threaded && info.is_thx_dependent`
にガードしておく。読み取りロジックを変えれば、立てる側の万が一の
誤検出にも耐えられる。

### bindings.rs / apidoc の整合

#### bindings.rs

bindgen は build 時の perl ヘッダに対して動くので、`my_perl`
パラメータの有無は build mode に追従する。codegen 側で
`is_thx_dependent=false` を一貫させれば、bindings.rs と一致。

`pub static mut PL_curcop: *mut COP;` のような extern static は、
非 threaded でのみ bindings に出る。codegen の static 解決経路が
`pub static` と `pub static mut` の両方を扱えるか確認・必要なら拡張。

#### apidoc

apidoc は threaded perl からダンプした表現で、関数シグネチャに
`my_perl` を含むケースがある。非 threaded で apidoc を使うと
シグネチャ不整合になる。

選択肢:

1. **非 threaded でも threaded apidoc をそのまま使う**: codegen 側で
   build mode を見て `my_perl` 引数を「なかったこと」にする
   - apidoc の `T` フラグ（`no_thread_ctx`）に頼らずに済む
2. **非 threaded 専用 apidoc を別途生成**: `apidoc/v$X.$Y.nothread.json`
   を追加。再生成スクリプトを `apidoc-import.zsh` で対応
3. **apidoc を mode 中立に再設計**: `my_perl` 引数を抜いた表現にし、
   build mode に応じて codegen 側で追加

採用案: **(1) を当面採用**。理由:
- apidoc データセットを 1 系列に保てる
- codegen での補正は局所的（パラメータ取り出しで先頭 my_perl を
  skip するヘルパーを 1 つ作る）
- 後で (3) に移行する余地は残る（apidoc から `my_perl` を抜く
  正規化は非破壊で実施可能）

## 実装ステップ（コミット粒度）

各コミットで `cargo test` を通す。

### コミット 1: `PerlBuildMode` 型と検出ロジック

- `src/perl_config.rs` に `PerlBuildMode` enum と
  `detect_from_perl_config()` を追加
- `PerlConfig` 構造体に `build_mode` フィールド追加
- 既存 `get_perl_config()` を拡張
- 単体テスト: `Config{usethreads}` モックで両方の mode を検証
- **挙動変化なし**

### コミット 2: 設定の配線

- `InferConfig` / `PipelineConfig` に `perl_build_mode: PerlBuildMode`
  を追加（デフォルト `Threaded` で既存挙動と一致）
- `infer_api::infer()` 等で受け取り、後続パスへ伝播
- CLI に `--perl-build-mode <threaded|non-threaded|auto>` 追加
  （デフォルトは `auto`）
- `auto` のとき `PerlBuildMode::detect_from_perl_config()` で取得
- 起動時に `eprintln!("[perl-mode] {:?}", build_mode)` を出力（診断）
- **挙動変化なし**（受け取るだけで使わない）

### コミット 3: `pTHX_` callback と `is_thx_dependent` 検出の mode 化

- `infer_api.rs:301-305` の callback 登録を `if threaded` でガード
- `macro_infer.rs:799-810` の検出を mode でガード
- `macro_infer.rs:1172-1180`（`c_fn_dict.is_thx_dependent` 経由の
  伝播）は `c_fn_dict` 側で全 false になるので自然に no-op
- 単体テスト: 非 threaded モードで `MacroInferInfo.is_thx_dependent`
  が全 false になることを確認
- **挙動変化点**（threaded mode の挙動は不変、non-threaded は false 化）

### コミット 4: codegen 側の防御的 mode ガード

- `RustCodegen` 系の構造体に `perl_threaded: bool` を持たせる
  （`InferResult` 経由で受け取る）
- `is_thx_dependent` 参照箇所を全て `perl_threaded && info.is_thx_dependent`
  形に変える（grep で機械的に）
- inline 関数 THX 判定 (`rust_codegen.rs:5482-5490`) も同様に mode
  ガード
- **挙動変化なし**（threaded で立てた検出が読み取り側で活きる、
  non-threaded で立たないものは元々無効化される）が、誤検出耐性が向上

### コミット 5: 非 threaded apidoc 補正

- apidoc から得たパラメータリストの先頭が `my_perl` だった場合に
  非 threaded mode で skip する補正を、`MacroInferContext::infer_macro_types`
  / `RustDeclDict` のロード時のいずれかに入れる
  - 場所は実装時に決定（apidoc → param 列を組み立てる初期化箇所が
    最も局所的）
- 「補正適用件数」を `[apidoc-mode-fix] stripped my_perl from N entries`
  として `eprintln!`（診断）
- **挙動変化点（非 threaded のみ）**: apidoc と bindings の整合が取れる

### コミット 6: extern static の `mut` 対応確認

- 非 threaded perl で出る `pub static mut PL_curcop: *mut COP;` を
  codegen で参照できるか確認・必要なら拡張
- `RustDeclDict` の static 走査ロジックを確認 (`src/rust_decl.rs`)
- もし unsafe extern static アクセスが Rust 2024 で警告になる場合、
  `&raw mut NAME` 経由に揃える（既存の `&raw const NAME[0]` 方式と
  対称）
- **挙動変化点**（必要に応じて）

### コミット 7: テスト整備

- 非 threaded perl 環境を CI に追加（`~/blob/libperl-rs/12-macrogen-2-build.zsh`
  と並列の non-threaded 版スクリプト）
- 統合ビルドで非 threaded perl 5.30〜5.42 をマトリクスに加える
- ローカルテスト用に perlbrew で `perl-5.40.0t` （threaded）と
  `perl-5.40.0` （non-threaded）の両方を用意する手順を
  `doc/operations-build.md`（あれば）に追記

### コミット 8: ドキュメント更新

- `CLAUDE.md`: 対応 perl build mode の記述追加
- `doc/architecture-overview.md`: PerlBuildMode の解説
- `doc/architecture-thx-dependency.md`: 非 threaded mode での挙動を追記
- `doc/architecture-rust-codegen.md`: my_perl 注入の mode ガードに関する
  記述更新

## テスト戦略

### 単体テスト

- `PerlBuildMode::detect_from_perl_config` を `Config{usethreads}` の
  各値で検証（"define", undef, "")
- `is_thx_dependent` が non-threaded mode で全マクロ false になる
  ことを samples ベースのテストで確認

### 統合テスト

- threaded perl 5.30〜5.42 で生成 → 既存挙動と一致（regression）
- non-threaded perl 5.30〜5.42 で生成 → `cargo build` 成功
- 統合スクリプトで両モードのビルド成功率を比較

### 回帰検出

- 既存 299 単体テスト pass
- threaded 環境での生成出力 diff（mode 設定追加前後で 0 diff のはず）

## リスクと対処

| リスク | 対処 |
|---|---|
| 非 threaded perl が手元になく検証できない | perlbrew で `perl-5.40.0` (default) を用意する手順をドキュメント化 |
| apidoc の `my_perl` skip 補正が一部マクロで誤動作 | 補正適用ログを出し、想定外のケースは `--debug-apidoc-after-merge` で確認 |
| extern static の `pub static mut` 対応漏れで非 threaded でビルド失敗 | コミット 6 で先回り対応。出ない場合はリリース可 |
| `is_thx_dependent` の伝播が複雑で漏れる | コミット 4 の防御的 mode ガードでカバー |
| consumer crate (libperl-rs) が threaded 前提のコードを書いている | 別計画。本計画では libperl-sys 生成側のみを対象 |
| CI 時間が倍増 (mode 2 倍) | 並列実行 + 主要バージョンのみマトリクス対象に |

## 既知の論点（本計画で扱わないもの）

- **dynamic loading**: `Perl_xs_handshake` 等の非 threaded 用 ABI
  との互換性。consumer 側で必要になったら別計画
- **perl の non-stdio**: `nostdio.h` パスでの差分。本計画では
  対象外
- **完全な mode 中立な libperl-sys**: `#[cfg]` ベースの両対応は
  本計画では採用せず（Option B）

## 参考資料

- perl ソース `perl.h`, `intrpvar.h`, `embed.h` での `pTHX_` /
  `aTHX_` / `PL_xxx` 定義
- 既存 `doc/architecture-thx-dependency.md`
- 既存 `apidoc/*.json` のフィールド構造（`apidoc.rs`）
