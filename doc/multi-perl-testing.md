# 複数バージョン Perl テスト基盤

macrogen を偶数マイナー **perl 5.20〜5.42 × {threaded, non-threaded}** に対して
ローカル (podman) と CI で検証するための基盤。バージョン依存の退行
(例: 旧 perl での Cv 一族消滅) を **publish 前に** 検出することが目的。

perl `5.44` は同梱 apidoc データが 5.42 までのため対象外
(`resolve_apidoc_path` は exact match 必須 — macrogen issue #6)。

## 構成

| ファイル | 役割 |
|---|---|
| `scripts/multi-perl.tcl` | ホスト側 podman ラッパ (tclsh 8.6+) |
| `scripts/multi-perl-inner.sh` | コンテナ内 glue (rustup / target / apt / dispatch) |
| `scripts/multi-perl-smoke.pl` | 検査 harness。コンテナ / CI / ホストで同一に動く (core モジュールのみ、perl 5.20 対応) |
| `scripts/multi-perl-expect/` | 版×モード別の期待値 (JSON) |
| `samples/require-partial-eval.txt` | partial eval 必須 API (`--require-codegen-list` 用) |
| `.github/workflows/{ci,full-matrix,downstream}.yml` | CI (下記) |

## Quick start

```console
# 1 バージョンのスモーク (公式 perl:5.36 イメージ、非 threaded = タグ既定)
$ scripts/multi-perl.tcl 5.36

# threaded / 両モード / 複数版
$ scripts/multi-perl.tcl 5.30-threaded
$ scripts/multi-perl.tcl --both 5.36 5.42

# 失敗時にコンテナ内対話 shell へ (生成物は /out に見えている)
$ scripts/multi-perl.tcl --shell-on-fail 5.30-threaded

# 生成物の出力先をホスト側ディレクトリに
$ scripts/multi-perl.tcl --out /path/to/dir 5.36

# 下流 (libperl-sys) ビルドまで通す (実コンパイルエラーの検出)
$ scripts/multi-perl.tcl --downstream 5.30-threaded
$ scripts/multi-perl.tcl --downstream --libperl-rs ~/blob/libperl-rs/12-macrogen-2 5.36-threaded

# フル matrix は明示 opt-in のみ (ディスク見積りを表示して確認)
$ scripts/multi-perl.tcl --all
```

ホストの perl で直接 (コンテナなし):

```console
$ cargo build
$ perl scripts/multi-perl-smoke.pl --require 5.42-threaded
$ PATH=$PWD/tmp/perls/v5.42.3/bin:$PATH perl scripts/multi-perl-smoke.pl --require 5.42-non-threaded
```

成果物は leg 毎に `tmp/multi-perl/out/<ver>-<mode>/` へ:
`macro_bindings.rs` / `macrogen.stderr.log` / `perl-V.txt` / `require-list.txt` /
`summary.json` / (downstream 時) `build-error.log` / `bindings.rs`。

## 検査内容 (multi-perl-smoke.pl)

各検査は ID を持ち、結果は PASS / FAIL / XFAIL / XPASS / WARN / INFO:

- `require-perl` — ambient perl が `--require X.Y-MODE` と一致 (PATH/イメージ取り違え検出)
- `exit-status` — macrogen が正常終了し出力が非空
- `perl-mode` — stderr の `[perl-mode] Threaded|NonThreaded` がモードと一致
- `must-generate/<NAME>` — macrogen 本体の `--require-codegen-list` に委譲。
  失敗理由 (CASCADE_UNAVAILABLE の依存先、apidoc 宣言なし等) がそのまま出る
- `must-not-generate/<NAME>` — その版に存在しない API が生成されないこと
- `bounds.*` — `[CODEGEN_SUPPRESSED]` / `[UNRESOLVED_NAMES]` / 総生成関数数の上下限
- `thx` — 非 threaded は `[THX]` 0 個、threaded は `thx_required` の doc 行を確認
- `rustfmt-parse` — 生成物が `rustfmt --edition 2024` を通ること

## 期待値ファイルのスキーマ

マージ順 (後勝ち): `common.json` → `common[モード]` → `v5.X.json` → `v5.X[モード]`。
モードセクションは `"threaded"` / `"non-threaded"` キーに同スキーマを入れる。

```jsonc
{
  "must_generate": [...],        // common のみ: 全版共通の契約
  "must_generate_extra": [...],  // overlay: 追加分
  "must_not_generate": [...],    // その版に存在しない API (下記)
  "thx_required": [...],         // common のみ
  "bounds": { "codegen_suppressed_max": 5, "unresolved_names_max": 32,
              "generated_fn_min": 1690 },   // null = report-only
  "known_failures": {            // 既知の失敗 (直すべきもの) → XFAIL
    "must_generate": ["OP_CLASS"],
    "assertions": ["thx"]
  }
}
```

- **known_failures (XFAIL/XPASS)**: 登録済みの失敗は XFAIL として容認され exit 0。
  登録済みなのに成功すると **XPASS 警告**(期待値を引き締めよ)。`--strict` で
  どちらも hard fail。既知 red を「可視化したまま」matrix に入れ、修正後の
  引き締め忘れも検出する仕組み
- **must_not_generate**: 二重の意味を持つ。(a) 生成されないことの検査
  (b) `must_generate` からの除外。**その版の Perl にその名前の API が存在しない**
  という正当な不在を表す (例: `Perl_CvDEPTH` は embed.fnc 登場が 5.32 のため
  5.30 以前では must_not_generate)。「本当は生成できるはずの失敗」は
  known_failures の側に置くこと
- **baseline 採取**: `scripts/multi-perl.tcl --baseline 5.XX` (または harness の
  `--baseline`) が known_failures JSON 断片 + 実測 bounds を出力する。
  新版対応や大きな挙動変更時の seed に使う

## must-generate 契約と `--require-codegen-list`

`common.json` の `must_generate` は partial eval 必須 API (Cv 一族 =
`samples/require-partial-eval.txt` と同期) + golden 対象 17 関数。検査は
macrogen 本体の `--require-codegen-list` (→ `doc/reference-cli-usage.md`) に
委譲されるため、失敗時は「なぜ生成されなかったか」が理由付きで報告される。

下流 build.rs からも `PipelineBuilder::with_require_codegen_list()` で
同じ保証を得られる (rustc の E0425 より手前で fail-fast)。

## 下流 (libperl-sys) ビルド層

`--downstream` で libperl-rs (既定: `12-macrogen-2` ブランチを shallow clone、
`--libperl-rs DIR` でローカル checkout) を scratch copy し、workspace root に
`[patch.crates-io] libperl-macrogen = { path = "/app" }` を追記してビルドする。
ユーザーの checkout は変更しない。

**注意**: scratch の Cargo.lock は crates.io 版 macrogen を pin しているため、
semver 互換な patch は**そのままでは適用されない** (`[[patch.unused]]`)。
inner.sh が `cargo update -p libperl-macrogen` → `cargo tree` で適用を検証する。

失敗時の `build-error.log` + `macro_bindings.rs` は
`tools/build-error-to-vpatches.pl <ver> <log> <bindings>` の入力になる。
ただし **CLAUDE.md の skip_codegen 運用ポリシー**に従い、出力された skip 候補は
原因分類 (cascade 伝播漏れ / codegen バグ / apidoc 宣言欠落 / 真の不在) を経てから
使うこと。

## CI との対応

| workflow | トリガー | 内容 |
|---|---|---|
| `ci.yml` | push | 単体テスト + golden (5.42-threaded で実走) + 代表スモーク 5.30/5.36/5.42 × 両モード |
| `full-matrix.yml` | pull_request / dispatch | 5.20〜5.42 × 両モード |
| `downstream.yml` | dispatch | libperl-sys 統合ビルド (perl 版・ブランチ指定)。publish 前検証用 |

CI は podman を使わず、actions-setup-perl の perl に対して
`multi-perl-smoke.pl` を直接実行する (harness がホスト非依存なのはこのため)。
`workflow_dispatch` はデフォルトブランチに workflow が載ってから有効になる。
失敗 leg は out ディレクトリが artifact として回収される。

## ディスク予算と掃除

- イメージ ~0.9〜1.1GB/版 (`-threaded` は base layer 共有で増分小)
- `tmp/multi-perl/rust-home` ~1.5GB (全コンテナ共有、rustup は初回のみ)
- `tmp/multi-perl/target/<codename>` ~1〜1.5GB (Debian base 別共有 —
  buster 系 5.20〜5.32 で 1 個)
- `tmp/multi-perl/target-downstream/<leg>` ~1.5〜2GB (leg 毎、`--downstream` 時のみ)

掃除: `scripts/multi-perl.tcl --clean` (状態ディレクトリ) +
`podman rmi docker.io/library/perl:<tag>` (イメージ)。

## トラブルシューティング

- **旧版イメージのタグ**: 5.20〜5.26 の素タグは stretch ベースで glibc ヘッダが
  古すぎ preprocess 不能 (`__intN_t` 未展開)。ラッパは自動で `-buster` を優先
  する。`--image-tag` で明示指定も可
- **rustup の TLS (buster 等の古い CA)**: `/opt/rust` は共有 volume なので、
  最初に新しい leg (例: `scripts/multi-perl.tcl 5.42`) を 1 回動かして
  toolchain を作れば、古いイメージは rustup に接続しない
- **apt (downstream のみ)**: stretch/buster は自動で `archive.debian.org` に
  書き換え。`--use-debian-archive` / `--no-debian-archive` で強制/抑止
- **SELinux**: `selinuxenabled` なら mount に `:z` を自動付与
- **ホストの perl-build 製 perl が preprocess 不能**: 過去にビルドした perl は
  `$Config{incpth}` に当時の gcc の include パス (例: `.../gcc/.../15/include`)
  を埋め込んでおり、ホストの gcc 更新後は stddef.h が見つからず失敗する。
  ホスト検証には perl を作り直すこと:
  `perl-build -j 8 --noman 5.42.3 $PWD/tmp/perls/v5.42.3`
- **golden 回帰テスト**: `tests/expected_rust/` は基準 perl (5.42-threaded)
  固定。他版では自動 skip され、`MACROGEN_GOLDEN_FORCE=1` で強制実行できる。
  版別の検証は本基盤の役割

## 既知の red (2026-08-22 時点、期待値に XFAIL 登録済み)

green 化は実装計画の後半フェーズ (採取 → 分類 → カテゴリ別修正 → 引き締め) で行う:

- **must_generate の既知 red は全版ゼロ** (2026-08-22 時点)
- **5.20-threaded**: thx 検査 (CvGV の doc 行に [THX] マーカーが付かない)
- **非 threaded 全版**: `samples/bindings.rs` が threaded スナップショットのため
  PL_* interpreter 変数が未解決 (common.json の non-threaded 節に登録)
- **5.20/5.22 downstream**: rustc E0308/E0277/E0614 群 (主因は hv_func.h の
  旧ハッシュ実装 inline 群。採取済み、green 化対象)
- **5.32/5.34 の skip_codegen 残 16/21 件** (`v5.3x.patches.json`): 実走
  再評価済みの真の失敗のみ。クラス別 (戻り値位置ポインタキャスト欠落 /
  bool・int 変換残渣 / 個別型不一致) の理由付きで、将来の codegen 修正候補

**解消済み (2026-08-22)**:

- **Cv 一族 6 + HvFILL (5.20〜5.32)**: `add_decl` パッチ (`MUTABLE_*` 一族 /
  `AvARRAY` / `AvFILLp` の宣言補充、doc/architecture-apidoc-patches.md) と
  `Pad*` 系 12 件の arg_type_override (<=5.30 pad.h の `*` 抜け) で解消
  (apidoc data 1.3/1.4)。下流 libperl-sys ビルドは 5.30-threaded で green
  確認済み (issue #5)
- **OP_CLASS 一族 (〜5.36)**: `Xop*`/`Bhk*` の `which` 引数への `token` 注釈
  補完 (apidoc data 1.5) + パッチ適用後辞書からの explicit-expand 導出で解消
- **newSVpvs 一族 (〜5.34)**: パーサの隣接文字列リテラル連結に仮引数を許容
  (`("" s "")` → `s` 還元) + `ASSERT_IS_LITERAL` の explicit-expand 化で解消

- **CvDEPTH (5.32/5.34)**: embed.fnc エントリ (Perl_CvDEPTH の記述、`I32 *`)
  がマクロ戻り値制約に漏れていたのを version 固有の return override (`I32`)
  で補正 + 三項演算子 bool 分類の codegen 修正 + skip リスト実走再評価
  (apidoc data 1.8) で解消

いずれも known_failures から削除 = enforced 化済み。**must_generate の
既知 red は全版ゼロ**。5.30/5.32-threaded は例外ゼロの完全 green。

存在しないことが確認済みの API (must_not_generate):
`Perl_CvDEPTH` / `Perl_cx_topblock` は **5.32 からこの名前になった**ため
5.30 以前では要求しない (詳細は各 v5.2x.json の must_not_generate_comment)。
