# 引継: perl 5.28/5.30 の SvREFCNT_dec / Padlist 系 skip_codegen 解除

作成: 2026-08-26(perl-LibPerlRs-PartialEval セッションからの依頼指示書)。
状態: **解決済み (2026-08-26)** — 残りはリリース (0.1.11 publish.tcl) と
下流の最終確認 (§6)。
CopLINE のとき(`doc/plan/unskip-copline-5.34-5.36.md`、0.1.10 で解決)と
同じ形の「skip の真因調査 → 型注釈/生成修正 → リリース」タスク。

## 0'. 解決サマリ (2026-08-26)

**真因は 2 層あった**:

1. **skip 6 件は stale だった**: v5.28/v5.30.patches.json の auto-generated
   skip (`S_SvREFCNT_dec`, `S_SvREFCNT_dec_NN`, `PadlistARRAY`, `PadlistMAX`,
   `PadlistNAMESARRAY`, `PadlistNAMESMAX`) は 78762b9 (2026-04-25、0.1.7 時代)
   の採取で、0.1.8〜0.1.10 の改善 (apidoc data 1.3〜1.7 の Pad* override 等)
   で既に解消していた。6 件を外して multi-perl --downstream を実走した結果、
   **5.28/5.30 とも threaded downstream ビルドまで一発 green**
   (型修正・codegen 修正は一切不要)。連鎖で `SvREFCNT_dec` /
   `SvREFCNT_dec_NN` / `PAD_SET_CUR_NOSAVE` / `PAD_SET_CUR` / `PadlistNAMES` /
   `PAD_BASE_SV` も復活。apidoc data 1.14 として反映。
2. **`Perl_SvREFCNT_dec` は unskip では出ない**: この名前は **perl 5.31.x の
   inline 関数改名 (S_ → Perl_) で 5.32 から登場**したもので、<=5.30 の
   ヘッダには存在しない (sv.h は `SvREFCNT_dec(sv) → S_SvREFCNT_dec(aTHX_
   MUTABLE_SV(sv))`、proto.h にも Perl_ 名なし)。§2 の「連鎖不生成」仮説は
   S_SvREFCNT_dec の skip についてのみ正しく、Perl_ 名については誤り。
   これは expect の `Perl_CvDEPTH` / `Perl_cx_topblock` (「この名前は 5.32
   から」) と同じ「正当な不在」であり、v5.20〜v5.30 の expect overlay の
   `must_not_generate` に理由付きで登録した。
   **Perl_ 名の提供は libperl-rs 側タスクの fallback shim
   (plan-5.28-5.32-round.md §1-4、Stack_off_t 前例の cfg-gated 手書き shim)
   で行う**。macrogen 側で出すには「alias 生成」の新機能 (新 PatchKind +
   emission) が必要で、Signature Approval を経ていないため今回は見送り
   (必要になったら次段で提案する)。

**修正** (apidoc data 1.14 / 0.1.11):
- v5.28/v5.30.patches.json: 上記 6 件の skip_codegen を削除
- 再退行防止: v5.28/v5.30 expect overlay の `must_generate_extra` に
  S_SvREFCNT_dec 一族 + Padlist* を、threaded 節に PAD_SET_CUR を追加
  (PAD_SET_CUR は non-threaded では PAD_SET_CUR_NOSAVE が unresolved で
  従来から不生成のため threaded 限定)。samples/require-partial-eval.txt に
  PadlistARRAY/PadlistMAX を追加 (PAD_SET_CUR / Perl_SvREFCNT_dec は
  版・モード依存のため載せずコメントで案内)
- `Perl_SvREFCNT_dec` を v5.20〜v5.30 overlay の must_not_generate に登録

**検証**: 5.28/5.30 threaded smoke+downstream green、5.28/5.30 non-threaded
smoke green。5.32〜5.44 リグレッションなし (下記 §5 の一括確認)。

**ハーネスの罠 (次回のため)**: apidoc キャッシュ (LIBPERL_APIDOC_CACHE_DIR =
/out/apidoc-cache) の key は APIDOC_DATA_VERSION のみなので、**version を
bump せずに apidoc/ を編集して再走すると stale キャッシュが使われる**。
patches 編集の反復中は `rm -rf tmp/multi-perl/out/<leg>/apidoc-cache` と
`rm -rf tmp/multi-perl/target-downstream/<leg>/debug/build/libperl-sys-*`
(build.rs 再実行の強制) を挟むこと。また tmp/multi-perl/out/ の 5.44 等の
成果物は旧 `--branch 12-macrogen-2` 時代の stale が残っている場合がある
(MUTABLE_SV が skip-codegen.txt で skip されていた頃のもの)。

**下流タスク (plan-5.28-5.32-round.md) への注意**:
- require-codegen.txt に足せるのは `PadlistARRAY` / `PadlistMAX` /
  `PAD_SET_CUR` (threaded CI 前提)。**`Perl_SvREFCNT_dec` は <=5.30 で
  macro_bindings.rs に現れない**ため、require に入れると 5.28/5.30 で
  build.rs 段階 fail になる。shim (perl_core.rs 側) で提供する名前は
  require 対象外にすること。
- shim は plan-5.28-5.32-round.md §1-4 の記載どおりで良い (S_SvREFCNT_dec
  が macro_bindings.rs に生成されるようになったので、シンプルに
  `#[cfg(not(perlapi_ver32))] pub use crate::S_SvREFCNT_dec as
  Perl_SvREFCNT_dec;` 形でも、記載の手書きミラーでも可)。

## 0. 依頼の背景(下流の状況)

perl-LibPerlRs-PartialEval(`~/db/github/perl-LibPerlRs-PartialEval`)の
対応 perl 拡大ラウンド第 2 段(5.28〜5.32 ithreads)。第 1 段(5.34〜5.44)
は macrogen 0.1.10 + libperl-rs PR #22 で完了済み。

下流 CI(run 32922407592、2026-08-26)の probe 失敗の確定症状:
- **5.32 ithreads**: `Perl_av_count` の E0425 が 1 件のみ
  → これは**下流 engine 側で `AvFILLp(av)+1` に置換して解消する**
  (magic なし前提の内部 AV 用途と明記済みのヘルパ)。**macrogen 対応不要**。
- **5.28 / 5.30 ithreads**: 下記 §1 のシンボル群が macro_bindings.rs に
  生成されず E0425(+ 下流自身の E0308 が 1 件 — これも下流側で修正)。

つまり本タスクの対象は **5.28 / 5.30(threaded)のみ**。

## 1. 生成が必要なシンボル(下流の使用実態つき)

5.32〜5.44 では全て生成済み(下流はそのまま使えている)。5.28/5.30 で欠落:

| シンボル | 下流での使用 |
|---|---|
| `Perl_SvREFCNT_dec` | 約 15 呼び出し箇所(Term/引数 SV の解放) |
| `PadlistARRAY` | 領域 CV の pad slot 0(@_)取得 |
| `PadlistMAX` | push_frame の depth-1 pad 存在チェック |
| `PAD_SET_CUR` | 評価フレーム構築(SAVECOMPPAD 込みの pad 切替) |

**スコープ外**:
- `Perl_SvREFCNT_inc` — v5.32.patches.json の skip("macrogen limitation")
  は維持のままで良い。下流は sv_refcnt フィールド加算ヘルパで回避済み。
- `Perl_av_count` — 上記のとおり下流側で置換予定(apidoc data として
  5.32 に足せるならそれは歓迎だが、本タスクの完了条件には含めない)。
- 5.20〜5.26 — libperl-sys の macro_bindings 生成不良という別問題。
  次段のタスク。

## 2. 一次情報(このリポジトリ内、2026-08-26 時点の観察)

- `apidoc/v5.28.patches.json` / `v5.30.patches.json`(auto-generated
  skip_codegen、それぞれ全 46 / 49 件)に以下が入っている:
  - `PadlistARRAY`, `PadlistMAX`(+ `PadlistNAMESARRAY`, `PadlistNAMESMAX`)
  - `S_SvREFCNT_dec`, `S_SvREFCNT_dec_NN`
  - reason は一律 "type-check fails on perl 5.2x (auto-generated)"
- `PAD_SET_CUR` と `Perl_SvREFCNT_dec` **自体は skip リストに無い** —
  おそらく依存先(`PadlistARRAY` / `S_SvREFCNT_dec`)の skip による
  連鎖不生成(`doc/plan/unavailable-function-calls.md` の機構?)。要確認。
- apidoc json の出現(単純 grep による確認なので要再検証):
  `SvREFCNT_dec` は v5.32 以降の apidoc に有り、v5.30 以前に無い。
  Padlist 系はどの版の apidoc にも無い(マクロ推論由来の生成と思われる)。

## 3. 再現手順

`doc/plan/unskip-copline-5.34-5.36.md` §3 の multi-perl ハーネス
(tmp/multi-perl)と同じ。対象は perl 5.28 / 5.30 の threaded。
skip を一時的に外して type-check エラーの実物を採取するところから。

## 4. 調査の観点(仮説)

CopLINE の前例 = 上流 apidoc 注釈の型誤記 → `return_type_override`
(apidoc data 1.13)で解決、blind unskip はしない方針だった。今回の候補:

1. `S_SvREFCNT_dec`: 5.30 以前は sv.h マクロ + inline.h 静的関数の形が
   5.32+ と違う(5.32 で SvREFCNT_dec まわりの整理が入った)。展開形の
   型差を type-check ログで特定する。
2. `PadlistARRAY`/`PadlistMAX`: padlist 構造体アクセサ。5.28/5.30 の
   pad.h のマクロ形状(キャストの有無・型)差が原因の可能性。
3. 修正手段は CopLINE 同様、apidoc data の版別 override か codegen 側の
   対応。skip をただ外して通るならそれで良いが、理由の特定を先に。

## 5. 完了条件

- perl 5.28 / 5.30(threaded)の生成 macro_bindings.rs に
  `Perl_SvREFCNT_dec` / `PadlistARRAY` / `PadlistMAX` / `PAD_SET_CUR` が
  現れ、type-check が通る
- 既存対応版(5.32〜5.44)にリグレッションなし(multi-perl 一括確認)
- **0.1.11 リリース**(apidoc data 版数 bump、apidoc.tar.gz 再生成 —
  `publish.tcl` / 0.1.10 リリース手順の前例に従う)

## 6. 下流での最終確認(リリース後)

1. libperl-rs 側タスク(`~/db/github/libperl-rs/docs/plan/plan-5.28-5.32-round.md`)
   が 0.1.11 bump + require-codegen.txt 拡充 + CI 5.28 追加を行う
2. PartialEval 側 CI の workflow_dispatch(input `libperl-rs-ref`)で
   5.28/5.30 ithreads probe が green になることを確認

## 7. 関連リンク

- 前例: `doc/plan/unskip-copline-5.34-5.36.md`(§0 に解決サマリ)
- `tools/build-error-to-vpatches.pl`(エラー→ patches 変換。修正が成功
  すれば該当エントリが再生成で消えるはず)
- 下流 CI: https://github.com/hkoba/perl-LibPerlRs-PartialEval/actions/runs/32922407592
  (5.28 job 98038407341 / 5.30 job 98038407229 に E0425 の実物ログ)
- 下流の総括指示書: perl-LibPerlRs-PartialEval の
  `docs/TASK-perl-5.28-5.32-2026-08.md`
