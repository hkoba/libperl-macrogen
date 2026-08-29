# 引継: perl 5.20〜5.26 の downstream (libperl-sys) green 化ラウンド

作成: 2026-08-29 (perl-LibPerlRs-PartialEval セッションからの依頼指示書)。
`unskip-copline-5.34-5.36.md` (0.1.10) / `unskip-refcnt-padlist-5.28-5.30.md`
(0.1.11) に続く第 3 弾。対になる libperl-rs 側指示書 =
`~/db/github/libperl-rs/docs/plan/plan-5.20-5.26-round.md`
(その §1 が本書へ依頼する内容。§0 に下流 CI 実測の棚卸し全文)。

**前例の必読箇所**: unskip-refcnt-padlist-5.28-5.30.md §0' — 「skip 6 件は
stale だった (unskip だけで green)」の実績と、末尾「ハーネスの罠」
(apidoc-cache / stale target)。本ラウンドは同じ罠を踏む反復作業になる。

## 0. 背景 (下流の状況)

perl-LibPerlRs-PartialEval の対応 perl 拡大。2026-08-29 時点で
**5.28〜5(latest) × threaded/non-threaded は全 green**
(GH-20 MULTIPLICITY 吸収層まで込み、下流 CI run 33248242079)。
残る赤 = **5.20/5.22/5.24/5.26 × 両モードの 8 probe セル**。いずれも
libperl-sys の段階 (require-codegen fail-fast または macro_bindings.rs の
コンパイル) で落ち、症状はモード非依存。

注意: multi-perl の **smoke (macrogen 単体) は 5.20〜5.26 で baseline
採取済み** (expect overlay v5.20〜v5.26.json、known_failures は現在空)。
本ラウンドの対象はその先、**`--downstream` (libperl-sys ビルド) レグの
green 化**。smoke green ≠ downstream green。

## 1. 版別の症状と作業 (下流 CI 実測 + 本リポジトリ内観察)

### 5.26 — stale auto-skip の再評価 (本丸)

下流の require-codegen fail-fast:

```
PadlistARRAY: CODEGEN_SUPPRESSED (apidoc patch):
    type-check fails on perl 5.26 (auto-generated)
PadlistMAX:   同上
PadlistNAMES: CASCADE_UNAVAILABLE (dep: PadlistARRAY)
PAD_SET_CUR:  CASCADE_UNAVAILABLE (dep: PAD_SET_CUR_NOSAVE)
```

`apidoc/v5.26.patches.json` は **auto-generated skip_codegen 62 件**
(78762b9、2026-04-25 = 0.1.7 時代の採取)。5.28/5.30 の同種 skip
(46/49 件中の対象 6 件) は 0.1.11 ラウンドで全部 stale だった —
apidoc data 1.3〜1.14 の改善で解消済みのものを外すだけで green。
5.26 も同じ期待値で入る。

作業を 2 段に分けるのが現実的:

1. **require 直撃 + 下流 alias 前提の分を先に unskip** して downstream を
   通す。最低限:
   - `PadlistARRAY` / `PadlistMAX` / `PadlistNAMESARRAY` / `PadlistNAMESMAX`
     (require 直撃 + 連鎖元)
   - `S_SvREFCNT_dec` / `S_SvREFCNT_dec_NN` — libperl-rs 側の
     `Perl_SvREFCNT_dec` alias (perl_core.rs と lib.rs thx モジュールの
     2 箇所、現在 cfg = ver28..ver32) の下限を 5.26 以下へ広げる前提
   - `Padnamelist{MAX,ARRAY}` / `Padname{PV,LEN,TYPE}` — libperl-rs
     perl_core.rs の手書き compat (現在 5.28..5.30 用) の対象拡大を
     不要にできるなら unskip が望ましい (native 生成優先)
   - **PAD_SET_CUR_NOSAVE の連鎖仮説**: 62 件の skip リストに
     PAD_SET_CUR_NOSAVE 自体は**無い** — 展開が `PadlistARRAY` を含むため
     その skip からの連鎖不生成とみられる。PadlistARRAY を unskip すれば
     PAD_SET_CUR_NOSAVE → PAD_SET_CUR が復活する見込み
     (0.1.11 ラウンドの「連鎖で PAD_SET_CUR も復活」と同型)。要実証。
2. **残りの棚卸しは余力で**。type-check 失敗が実在する分は理由を採取して
   残す (blind unskip はしない — CopLINE 前例の方針。ただし「stale を
   まとめて外して実走で確認」は 0.1.11 で実績のある進め方)。

解除できたものは expect overlay `scripts/multi-perl-expect/v5.26.json` の
`must_generate_extra` に登録して再退行防止 (PAD_SET_CUR は threaded 節へ —
non-threaded では PAD_SET_CUR_NOSAVE が unresolved で従来から不生成)。
なお v5.26.json の `must_not_generate` (Perl_SvREFCNT_dec /
Perl_CvDEPTH / Perl_cx_topblock = 5.32 改名で生まれた名前) は正当な不在の
記録なので維持。

### 5.24 — skip 採取漏れの補充

下流の macro_bindings.rs コンパイルエラー **44 件** (E0308 mismatched
types / E0277 cannot add `u8` to `u32`)、行 586〜750 の先頭領域 =
hv_func.h のハッシュ関数群。

決定的な事実: **`v5.24.patches.json` は `RCPV_*` 4 件のみ**で、
`S_perl_hash_djb2 / _sdbm / _superfast / _murmur3 / _one_at_a_time /
_one_at_a_time_hard / _old_one_at_a_time` の skip が無い。
**v5.20 / v5.22 の patches.json には同一族が入っている** (だから両版では
このクラスが出ない) — 5.24 だけ auto 採取から漏れている。

作業: `scripts/build-error-to-vpatches.pl` で 5.24 leg のビルドエラーから
再採取して patches.json に反映 (patches.json 冒頭コメントの
`tools/build-error-to-vpatches.pl` というパスは stale — 実体は
`scripts/`)。整数リテラル型推論 (u8 vs u32) の根治は任意 (やるなら
別タスクに切り出し、まず skip で green を先行)。

### 5.22 — E0067 が 2 件だけ

```
macro_bindings.rs:17712: { (* GvGP (gv)) . gp_flags () &= ! 1 as u32 ; ... }
macro_bindings.rs:17721: { (* GvGP (gv)) . gp_flags () |= 1 as u32 ; ... }
```

この版では `gp` 構造体の `gp_flags` が bindgen の **bitfield アクセサ
(メソッド)** になり、生成体が lvalue にならない (invalid left-hand side
of assignment)。GVf_INTRO (=1) の on/off マクロ (GvINTRO_on /
GvINTRO_off 相当と推定 — 生成ログで名前を確定させる) の生成体 2 つ。
下流に利用者は居ないので **skip 追加が最短**。bitfield 書き込みの
codegen 対応は新機能 (Signature Approval 案件) になるため今回は見送りで
良い。

### 5.20 — macrogen 必須作業は無い見込み

下流の初弾は `OpSIBLING` の require violation で、これは**正当な不在**
(5.21.2 新設) — 対応は libperl-rs 側 (require の版分岐 + perl_core.rs
compat shim)。macrogen 側は `--downstream` 5.20 leg を回して、その先の
残エラーを確認するだけ。旧観測 (下流 run 32679249770、2026-08-24) では
「unused_parens ×3 が -D warnings で error 化」があった — 再現したら
skip (libperl-sys 側 skip-codegen-legacy.txt の区分) か生成側修正。

## 2. 一次情報 (2026-08-29 時点の観察まとめ)

- patches.json の件数: v5.20 = 29 / v5.22 = 23 / v5.24 = 4 / v5.26 = 62。
- v5.26 の 62 件と libperl-sys `require-codegen.txt` (下流必須セット) の
  交差は **Padlist 4 種のみ** — 下流 CI の violation 4 件と整合し、
  CvGV / GvGP / CopFILE 等の他の必須シンボルは 5.26 で生成済み
  (CvHASGV や gv_init が skip されていても連鎖していない)。
- 下流 CI のエラー実物: PartialEval run 33248242079 の job
  99089328989 (5.26it) / 99089329028 (5.24it) / 99089329005 (5.22it) /
  99089329062 (5.20it)。non-threaded 側も同一症状。

## 3. 再現手順

`doc/multi-perl-testing.md` の podman ハーネス:

```console
$ scripts/multi-perl.tcl --downstream --both 5.26
$ scripts/multi-perl.tcl --downstream 5.24-threaded   # 等
# libperl-rs はローカル作業コピーを指す (require 分岐等の同時検証時)
$ scripts/multi-perl.tcl --downstream --libperl-rs ~/db/github/libperl-rs 5.26-threaded
```

**キャッシュの罠** (unskip-refcnt §0' 末尾の再掲、今回も必ず踏む):
apidoc キャッシュの key は APIDOC_DATA_VERSION のみ。patches 編集の
反復中は version を上げないので、都度

```console
$ rm -rf tmp/multi-perl/out/<leg>/apidoc-cache
$ rm -rf tmp/multi-perl/target-downstream/<leg>/debug/build/libperl-sys-*
```

## 4. 完了条件

- 5.26 / 5.24 / 5.22 の `--downstream` leg green (threaded /
  non-threaded 両方)。5.20 は「macrogen 起因のエラーが無い」こと
  (OpSIBLING の require violation は libperl-rs 側修正待ちで可)。
- 5.28〜5.44 にリグレッションなし (multi-perl 一括確認)。
- expect overlay 更新: 解除分の `must_generate_extra` 昇格 + bounds
  再較正 (codegen_suppressed_max は 5.26 で大きく下がるはず)。
- apidoc data version bump + **0.1.12 リリース** (publish.tcl、
  apidoc.tar.gz 再生成 — 0.1.11 リリース手順 be74f8b/90fe357 の前例)。
- **分割リリース可**: 5.26 だけで 0.1.12 を切り、5.24/5.22 を次版に
  回しても良い (下流は 1 版 1 PR で降順に進める想定 — libperl-rs 側
  指示書 §5)。

## 5. 下流での最終確認 (リリース後)

1. libperl-rs 側タスク (`plan-5.20-5.26-round.md` §2): 0.1.12 bump +
   OpSIBLING の require 版分岐 + compat shim + alias cfg 下限拡大
   (perl_core.rs と lib.rs thx モジュールの 2 箇所) + CI matrix 拡張。
2. PartialEval 側 CI の workflow_dispatch (input `libperl-rs-ref`) で
   5.26〜5.20 probe の green を確認。**Build green で満足せず Test
   (193 件) まで見る** — no-ithreads ラウンド (2026-08-29) では
   コンパイルが通った後、初実行の cfg 分岐 (anoncode の proto 取得) が
   t/45 の 1 サブテストでだけ発覚した前例がある。

## 6. 関連リンク

- 前例: `doc/plan/unskip-copline-5.34-5.36.md` /
  `doc/plan/unskip-refcnt-padlist-5.28-5.30.md`
- 対の libperl-rs 側指示書:
  `~/db/github/libperl-rs/docs/plan/plan-5.20-5.26-round.md`
- 下流 CI: https://github.com/hkoba/perl-LibPerlRs-PartialEval/actions/runs/33248242079
- ハーネス: `doc/multi-perl-testing.md` / `scripts/multi-perl.tcl` /
  `scripts/multi-perl-expect/v5.2{0,2,4,6}.json` /
  `scripts/build-error-to-vpatches.pl`
