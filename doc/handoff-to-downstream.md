# 下流プロジェクト（libperl-sys / libperl-rs）への引き継ぎ

このドキュメントは、本リポジトリ（libperl-macrogen）外で作業する際に
必要となる運用知識をまとめたもの。下流プロジェクトの `CLAUDE.md` から
このファイルを参照させる想定。

---

## プロジェクト族の構成

```
libperl-macrogen  (このリポジトリ)
    ↓ 生成する macro_bindings.rs
libperl-sys       (consumer crate、bindgen + macrogen を呼ぶ build.rs)
    ↓ 依存
libperl-rs        (上位 API)
```

3 者は別ディレクトリの別 Cargo プロジェクト。統合ビルドは
`libperl-sys` の build.rs で bindgen と libperl-macrogen を両方呼ぶ
形になっている。

---

## 統合ビルドと成果物の位置

### ビルドスクリプト

```
~/blob/libperl-rs/12-macrogen-2-build.zsh
```

実行すると `libperl-sys` の `cargo build` を走らせ、以下を生成する:

| ファイル | 役割 |
|---------|------|
| `tmp/build-error.log` | cargo build 時のエラーログ |
| `tmp/macro_bindings.rs` | libperl-macrogen が生成したコード |
| `samples/bindings.rs` | bindgen が生成したバインディング（参照用） |

**重要**: エラー行番号は `macro_bindings.rs:LINE` を指すので、
そのファイルを参照して関数単位に遡る。

### 失敗時のトリアージ

- `grep -c "^error" tmp/build-error.log` で件数確認
- `grep -B3 "^error\[E" tmp/build-error.log | less` でエラー種別と文脈
- 関数名は `pub unsafe fn NAME` を遡って特定

---

## skip-list 機構（現状の運用の要）

### 目的

libperl-macrogen 側で直せない生成失敗を **一時的にコメントアウトして
統合ビルドを通す** ための仕組み。`[CODEGEN_SUPPRESSED]` コメント
ブロックに落ちる。

### ファイル配置

```
libperl-sys/skip-codegen.txt     ← 1 行 1 関数名、# コメント可、空行無視
libperl-sys/build.rs             ← .with_skip_codegen_list(&skip_list) で読み込み
```

build.rs の書き方は `doc/plan/skip-codegen-list-and-auto-suppression.md`
の末尾にサンプルあり。

### 失敗した場合の再生成手順

統合ビルドで新たなエラーが出たら:

```zsh
# 1) エラーログと生成物から失敗関数名を抽出
<libperl-macrogen>/tools/build-error-to-skiplist.pl \
    <libperl-macrogen>/tmp/build-error.log \
    <libperl-macrogen>/tmp/macro_bindings.rs \
  > /tmp/new-skip.txt

# 2) diff を見て追加が妥当か確認し、libperl-sys/skip-codegen.txt に追記
diff <(grep -v '^#' libperl-sys/skip-codegen.txt | sort) \
     <(grep -v '^#' /tmp/new-skip.txt | sort)

# 3) 再ビルド
~/blob/libperl-rs/12-macrogen-2-build.zsh
```

### skip-list エントリを減らす方向（正攻法）

新エントリを増やす前に「この失敗は libperl-macrogen 側で直せる類型か」を
検討する。判定基準は次節の分類表を参照。

---

## 既知の失敗パターン分類

`doc/plan/build-error-next-considerations.md` に詳細がある。要約:

| カテゴリ | 症状 | 対応状況 |
|---------|------|---------|
| A | 関数引数の const/mut ミスマッチ | タスク 2 で多くを cast 自動化済み |
| B | `*mut c_char` vs `*mut i8` | タスク 1 で基盤整備、残件は fields_dict 側の問題 |
| C | lvalue macro 展開失敗（`RXp_EXTFLAGS(x) &= mask`） | **未解決 → skip-list 対象** |
| D | Member 式の struct 型推論不足（`block.oldcomppad`） | **未解決** |
| E | 三項式の型ヒントが引数型まで伝搬しない | **未解決** |
| F | lvalue 文脈の arg cast 脱落（`GvGP`） | **未解決、C と同根** |
| G | 個別イディオム（transmute、Option<fn>、offset_from 等） | 部分的にタスク 3/4 で解消 |

### 典型的な失敗メッセージと類型

| Rust エラー | 多くの場合の類型 |
|------------|-----------------|
| `types differ in mutability, expected *mut X` | A（arg const/mut）か F（lvalue） |
| `types differ in mutability, expected *const X` | 戻り値推論か A |
| `expected Option<unsafe extern ... fn(...)>, found integer` | G.3（fn pointer null 比較） |
| `expected usize, found u64` / `found isize` | G.5/G.6（assign 整数 cast） |
| `E0610: \`i32\` is a primitive type` | D（base 型が struct のはずが int と誤推論） |
| `E0067: invalid left-hand side of assignment` | C/F（lvalue macro 展開） |

---

## 関数名から生成経路を逆引き

### マクロ関数の生成結果
コメントヘッダで判別できる:
- `/// NAME - macro function` — マクロ由来
- `/// NAME - inline function` — inline 関数由来
- `// [CODEGEN_SUPPRESSED] NAME` — skip-list 対象
- `// [CODEGEN_ERROR] NAME` — 生成失敗
- `// [CALLS_UNAVAILABLE] NAME` — 依存関数が bindings に無い
- `// [UNRESOLVED_NAMES] NAME` — 未解決シンボル

これらのコメント形式は `src/rust_codegen.rs` の `InlineGenResult` /
マクロ側の対応 enum が出力する。

### libperl-macrogen 側の型推論をデバッグ

```bash
cd <libperl-macrogen>
cargo run -- <xs-wrapper.h> --auto --gen-rust \
    --bindings <bindings.rs> \
    --dump-types-for FUNC_NAME 2>&1 | grep -A30 '=== Type dump'
```

出力: param 制約の Tier、戻り値 TypeRepr、Root expr 制約など。

---

## 下流で修正 vs 上流で修正

### 下流（libperl-sys / libperl-rs）で閉じる

- 新しく生成失敗が増えた関数を skip-list に追加
- 生成コード外のラッパ実装（`libperl-sys/src/lib.rs` 等）
- 依存版数や feature flag の調整
- consumer 視点での API 設計

### 上流（libperl-macrogen）に戻す

- skip-list が膨らみ続ける（= 類似パターンが多い）
- 特定の Rust エラー種別が繰り返し出る
- 考察ドキュメントに既に対応予定と書かれている項目
- bindgen 側の変更に追随が必要

**判断のヒント**: skip-list エントリ 5-10 件以上が同じ C マクロ系
（例: `RXp_*`）に属していたら、上流でのパターン対応が効率的。

---

## 上流にフィードバックすべき観察

下流作業で次のような観察が得られたら `doc/plan/` に追記して欲しい:

1. 下流で特定関数が「実用上使えない形で生成されている」と判明
   （型は正しくても意味的に不正確など）
2. skip-list に入っているが実は下流でも必要な関数
3. Perl ヘッダ更新で新しく失敗するパターン

---

## 主要な参照ドキュメント（libperl-macrogen 内）

- `CLAUDE.md` — プロジェクト規約、3-pass アーキテクチャ、signature approval rule
- `doc/architecture-overview.md` — 全体構造
- `doc/architecture-rust-codegen.md` — Phase 3 詳細
- `doc/architecture-semantic-type-inference.md` — Phase 2 詳細
- `doc/plan/build-error-next-considerations.md` — 既知エラー 7 カテゴリの根本原因
- `doc/plan/build-error-priorities-1-to-4.md` — 既実施の最適化
- `doc/plan/skip-codegen-list-and-auto-suppression.md` — skip-list の設計

## 固有の環境メモ

- **shell**: ユーザは zsh。Bash tool で裸の `=` を含む文字列は zsh の
  EQUALS 展開でエラーになるため必ず quote する（`'==='`）
- **tmp の扱い**: 一時ファイルは `./tmp/`（プロジェクトルート下）、
  `/tmp` ではない
- **Rust edition**: 2024 固定（変更禁止）
- **対象 Perl C ヘッダ**: `/usr/lib64/perl5/CORE/`
