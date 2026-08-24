# 引継: perl 5.34/5.36 の CopLINE skip_codegen 解除 (真因調査と修正)

- 作成: 2026-08-24 (perl-LibPerlRs-PartialEval 側セッションからの引継)
- 状態: **解決済み (2026-08-24)** — 残りはリリース (0.1.9) と下流の最終確認 (§5-4, §6)
- ゴール: v5.34/v5.36.patches.json の CopLINE 系 skip_codegen を「正しい生成」で
  置き換え、下流 perl-LibPerlRs-PartialEval の CI probe セル 5.34/5.36
  (ithreads) をビルド通過させる

## 0. 解決サマリ (2026-08-24)

**真因**: 5.34/5.36 の cop.h にある `=for apidoc Am|STRLEN|CopLINE|const COP * c`
が戻り値型を STRLEN と**誤記**している (上流バグ、5.38 で line_t に修正。
<=5.32 は注釈自体が無く純粋推論で正しい)。ヘッダ scrape がこのエントリを
辞書に取り込み推論に勝つため、`-> STRLEN` + U32 本体の E0308 になっていた。
skip 3 件を外した再現走で `pub unsafe fn CopLINE(c: *const COP) -> STRLEN`
を実物確認済み。§4 の仮説 (b) = CvDEPTH と同系統の版別 override が正解だった。

**修正** (apidoc data 1.13):
- v5.34/v5.36.patches.json: CopLINE 系 skip_codegen 3 件 →
  `return_type_override: line_t` 1 件に置換 (CopLINE_inc/_dec は注釈を持たず
  CopLINE から推論されるため連動して復活。CopLINE_set は 5.38+ と同じ
  CODEGEN_INCOMPLETE で parity — §5-1 の「4 関数」は 3 関数が正)
- 再退行防止: `samples/require-partial-eval.txt` と multi-perl-expect
  common.json の must_generate に CopLINE を追加
- 副産物: multi-perl-inner.sh の成果物回収が「複数の libperl-sys-* build dir
  から古い方を拾う」バグを発見・修正 (mtime 最新優先。下流 build.rs の
  apidoc-version スタンプ再生成で新 hash dir ができると顕在化する)

**検証**: 5.34/5.36-threaded downstream green (libperl-rs master、
`CopLINE -> line_t` を実物確認)。smoke 5.20〜5.44 (+5.34 非 threaded)
リグレッションなし。host cargo test + golden green。

## 1. なぜ今これか (下流の状況)

perl-LibPerlRs-PartialEval に perl 5.20〜5(最新) × ithreads 有無の CI
マトリクスが入った (`.github/workflows/ci.yml`)。現在 ithreads の
**5.38/5.40/5.42/5.44 が全テスト green**。5.34/5.36 (ithreads) の残エラーは
次の **2 件だけ**:

```
error[E0425]: cannot find function ... `CopLINE` in crate `sys`
  --> crates/partial-eval-engine/src/raw.rs:73
  --> crates/partial-eval-engine/src/runloop.rs:543
```

- 参照 run (この 2 件だけになった状態):
  https://github.com/hkoba/perl-LibPerlRs-PartialEval/actions/runs/32680643661
  (perl 5.36 ithreads [probe] セル)
- つまり CopLINE さえ生成されれば 5.34/5.36 はビルドが通り、テスト段階へ進む。
  ランタイム面は「5.38 以前の call_sv(G_EVAL) の blk_oldsp ずれ」を下流
  engine 側で吸収済み (PartialEval commit 059e67c、5.38 実測で全 122 テスト
  PASS) なので、5.34/5.36 もテストまで通る見込みが高い (未実証)。

## 2. 現状の一次情報

### skip エントリ (このリポジトリ内)

- `apidoc/v5.36.patches.json` — 自動生成の 3 エントリ:

  ```json
  { "name": "CopLINE",     "kind": "skip_codegen", "reason": "type-check fails on perl 5.36 (auto-generated)" },
  { "name": "CopLINE_dec", "kind": "skip_codegen", "reason": "..." },
  { "name": "CopLINE_inc", "kind": "skip_codegen", "reason": "..." }
  ```

- `apidoc/v5.34.patches.json` (117 行目付近) — 手書き reason 付きの 3 エントリ:
  「Type mismatch specific to this perl's macro shape (see 2026-08-22
  build-error.log); kept after the re-evaluation that removed the entries
  fixed by apidoc data 1.3-1.7.」 / upstream_status: n/a (macrogen limitation)

- **5.30/5.32/5.38 以降には CopLINE の skip エントリは無く、正しく生成される**。
  5.34/5.36 だけの問題。

### 元症状の記録

- 導入コミット **78762b9** (2026-04-25「apidoc: 古い perl 版用の skip_codegen
  patches と生成ツールを追加」) に元症状の記述あり:
  「各版の生成コードに型不整合(**CopLINE が STRLEN を返さない**/Perl_atof の
  引数数違い/padlist の deref 不可、等)」
- 0.1.8 リリース前の全面再評価 (2026-08-22、apidoc data 1.3〜1.7 で多数解消)
  でも CopLINE 系は「まだ失敗する」として存置された。当時の build-error.log
  は現存しない (tmp/multi-perl/out/ の log は patch 適用後の再走で上書き済み)
  → **再現から始める必要がある**。

### 生成物の実物 (tmp/multi-perl/out/ に前回走の成果物が残っている)

- 5.42 (正常) の生成形:

  ```rust
  pub unsafe fn CopLINE(c: *const COP) -> line_t {
      unsafe { (*c).cop_line }
  }
  ```

- 5.34/5.36 の macro_bindings.rs:

  ```
  // [CODEGEN_SUPPRESSED] CopLINE - macro function (apidoc patch)
  // Reason: type-check fails on perl 5.36 (auto-generated)
  ...
  // [CASCADE_UNAVAILABLE] CopLINE_set - dependency not generated: CopLINE
  ```

  (CopLINE_set が連鎖不可になる点に注意 — 直すと 4 関数が復活する)

- **bindings.rs 側は 5.32〜5.38 で同一形**: `cop_line` フィールドあり、
  `pub type line_t = U32;`。→ 構造体・typedef の差ではなく、
  **cop.h のマクロ形状差に対する展開/型推論の問題**の可能性が高い。
  apidoc 辞書に CopLINE のエントリは無い (全版で 0 hit) ので、型は推論由来。

## 3. 再現手順

1. `apidoc/v5.36.patches.json` から CopLINE / CopLINE_dec / CopLINE_inc の
   3 エントリを一時的に削除 (v5.34 も同様)
2. multi-perl harness (podman) で downstream ビルドまで回す:

   ```bash
   scripts/multi-perl.tcl --downstream 5.36-threaded \
     --libperl-rs /home/hkoba/blob/libperl-rs/libperl-rs
   ```

   - `--libperl-rs` は clone の代わりにローカル master checkout を使う指定。
     **既定の `--branch 12-macrogen-2` は古いので使わない**こと
     (master には GH-18 の Stack_off_t compat alias 等が入っている)
   - 失敗時は build-error.log / macro_bindings.rs が
     `tmp/multi-perl/out/5.36-threaded/` に回収される
   - 詳細は doc/multi-perl-testing.md と CLAUDE.md の該当節
3. build-error.log の CopLINE 関連 rustc エラーが「真因」。
   `--shell-on-fail` でコンテナ内に入り、5.36 の cop.h
   (コンテナ内 perl の CORE/) と 5.32/5.38 の同マクロを diff するのが近道

## 4. 調査の観点 (仮説)

- 元症状は「CopLINE が STRLEN を返さない」型不整合。apidoc に CopLINE は
  無いので、期待型 STRLEN がどこから来たか (依存側の生成関数? 型推論の
  経路?) を特定する。doc/architecture-semantic-type-inference.md と
  doc/architecture-apidoc-patches.md (2 段マージ機構 load_for_apidoc_path)
  が背景資料
- 5.32 では通り 5.34/5.36 で落ち、5.38 で再び通る — 5.34 で cop.h に入り
  5.38 で消えた何か (マクロの中間形状) が原因のはず。cop.h の CopLINE
  周辺の版間 diff が最短の切り分け
- 修正の置き場所は真因次第: (a) codegen/型推論の一般修正、(b) apidoc
  patches.json への型宣言補正 (add_decl / return_type_override 系。
  0.1.8 で pad.h の `*` 抜けを直したのと同系統)、のどちらか

## 5. 完了条件

1. 5.34/5.36 (threaded) で CopLINE / CopLINE_dec / CopLINE_inc / CopLINE_set
   が正しい型 (`*const COP -> line_t` 系) で生成される
2. `scripts/multi-perl.tcl --downstream` が 5.30〜5.44 でリグレッションなし
   (両モードを見るなら `--both`。expect の bounds が生成数増で警告したら
   再較正する — 前例: commit 26d58df「expect: 5.20/5.22 の bounds 再較正」)
3. golden 回帰 (tests/rust_codegen_regression.rs、基準 5.42-threaded) 通過
4. 事後 (別コミットで可):
   - `samples/require-partial-eval.txt` と libperl-rs 側
     `libperl-sys/require-codegen.txt` に CopLINE を追加し、再退行を
     ビルド段階で fail-fast にする (現状どちらにも無い)
   - リリース反映は doc/operations-apidoc-release.md の手順どおり
     (apidoc.tar.gz 再生成 + APIDOC_DATA_VERSION 更新 → 0.1.9 は
     ユーザー主導の publish.tcl)。下流 libperl-sys は apidoc data version
     スタンプ機構で自動再生成される

## 6. 下流での最終確認 (リリース後)

- PartialEval 側: `cargo update -p libperl-macrogen` で lock を 0.1.9 に
  上げ、CI を dispatch (`gh workflow run CI -R hkoba/perl-LibPerlRs-PartialEval`)。
  probe セル「perl 5.34/5.36 ithreads」が Build を通過し Test へ進むこと。
  Test で新たな版差が出たらそれは下流 engine の課題として持ち帰る
- 注意: 下流でローカル perl を切り替えて検証する場合、libperl-sys の
  鮮度判定が PERL 切替を見ない問題があり stale bindings が残る
  (libperl-rs issue #21)。`rm -rf target/*/build/libperl-sys-*` が回避策

## 7. 関連リンク

- 下流 CI: https://github.com/hkoba/perl-LibPerlRs-PartialEval/actions
  (run 32680643661 = CopLINE 2 件だけの状態 / run 32681825926 = 現状の
  全体像。5.28/5.30 は別クラス: Perl_SvREFCNT_dec/inc・Perl_av_count・
  PadlistMAX・PAD_SET_CUR 未生成 + op_type() u32 差の 16 件。≤5.26 は
  macro_bindings 生成不良で 5.26: 1 件 / 5.24: 44 件 / 5.20: 3 件)
- libperl-rs: issue #18 / PR #19 (Stack_off_t compat、マージ済み)、
  issue #20 (MULTIPLICITY 吸収層 = non-ithreads 対応)、issue #21 (stale
  bindings)
- このリポジトリ: commit 78762b9 (skip patches 導入、元症状の記述)、
  tools/build-error-to-vpatches.pl (エラー→patches 変換。修正が成功すれば
  このツールが CopLINE を再出力しなくなるのが健全性の証拠)
- 隣接課題 (このタスクの外): v5.34.patches.json には Perl_SvREFCNT_inc 等の
  skip も残っており、5.28/5.30 の下流 16 件と地続き。CopLINE 完了後の
  次候補
