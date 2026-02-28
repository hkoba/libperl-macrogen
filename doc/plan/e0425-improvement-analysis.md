# E0425 未解決シンボル — 残存エラー分析と改善策

## 進捗サマリー

| 時点 | 総エラー | E0425 | 正常生成 | 主な施策 |
|------|---------|-------|---------|---------|
| commit `0741033` | 1,328 | 147 | 2,512 | UNRESOLVED_NAMES 検出 |
| commit `86228c5` | 1,325 | 141 | — | `use libc::{...}` 動的生成 |
| commit `1d44339` | 1,212 | 69 | 1,949 (1820 macro + 129 inline) | 依存順コード生成 + マクロ間カスケード検出 |
| (WIP) | **1,066** | **23** | **1,882** (1768 macro + 114 inline) | クロスドメインカスケード検出 (inline↔macro) |

### 実施済み施策

- **A-1 (libc 関数)**: `LIBC_FUNCTIONS` 定数を定義し `use libc::{...}` を動的生成。
  `strcmp`, `strlen` 等の E0425 を解消。✅ 完了
- **B (カスケード依存 — マクロ間)**: `called_functions` の依存グラフから
  トポロジカルソートで生成順を決定。`successfully_generated` 集合を追跡し、
  生成失敗マクロの呼び出し元を `[CASCADE_UNAVAILABLE]` で自動コメントアウト。
  434 マクロを検出し E0425 を 72 件削減。✅ 完了
- **F-1 (クロスドメインカスケード)**: inline 関数の生成成功を追跡し、
  マクロ→inline 関数依存と inline→inline 関数依存の両方をカスケード検出。
  inline 関数は 2 パス方式 + fixpoint 伝播で処理。
  inline 15 + macro 58 = 合計 73 カスケードを追加検出し E0425 を 46 件削減。✅ 完了

---

## 現状 (WIP)

| 指標 | 値 |
|------|-----|
| 総ビルドエラー | 1,066 |
| E0425 エラー | 23 |
| CASCADE_UNAVAILABLE | 492 マクロ + 15 inline 関数 |
| UNRESOLVED_NAMES | 79 関数 (49 macro + 30 inline) |
| CODEGEN_INCOMPLETE | 427 関数 (425 macro + 2 inline) |
| CONTAINS_GOTO | 6 inline 関数 |
| 正常生成 | 1,882 関数 (1,768 macro + 114 inline) |

---

## 残存 E0425 エラーの分類 (23 件)

### F. クロスドメインカスケード (実施済み, 残り 1 件)

クロスドメインカスケード検出 (F-1) により 46 E0425 を削減。

**残り 1 件**: `Perl_newSV_type` — `Perl_newSV_type_mortal` (inline fn) が
`newSV_type` (マクロ名) を AST レベルで呼び出し、codegen 時に
`Perl_newSV_type` (失敗 inline fn) に展開される間接依存。
AST の `called_functions` には `newSV_type`(マクロ)しか含まれないため
inline→inline カスケード検査で検出されない。

→ **優先度**: 極低。1 件のみの edge case。

---

### H. マクロの生成失敗・未生成 (8 エラー)

| シンボル | E0425 数 | 状態 |
|----------|---------|------|
| `SvIMMORTAL` | 3 | THX 版 `SvIMMORTAL_INTERP` のみ生成、非 THX 版なし |
| `toUPPER` | 2 | 派生マクロ (`toUPPER_A` 等) は生成、本体は未生成 |
| `RCPVx` | 2 | パース失敗 (KwStruct) |
| `SvSHARED_HEK_FROM_PV` | 1 | パース失敗 (KwStruct) |

**原因分析**:
- `SvIMMORTAL`: マクロ名の THX 変換ルールの問題。
  `SvIMMORTAL_INTERP` は生成されるが `SvIMMORTAL` として呼び出される。
- `toUPPER`: `called_functions` に含まれるが、マクロ辞書に対応するエントリが
  ないか、カスケードで除外されている。
- `RCPVx`, `SvSHARED_HEK_FROM_PV`: C の `struct` リテラルを含む式の
  パースに失敗。パーサーの制限。

**改善策H-1**: 個別対応が必要。優先度は低い（計 8 件）。

---

### C'. ローカル変数参照 (5 エラー, 旧 Category C の残り)

| シンボル | E0425 数 |
|----------|---------|
| `s` | 2 |
| `c` | 2 |
| `n` | 1 |

**原因**: マクロ展開後のコードにマクロパラメータでもローカル変数でもない
識別子が残る。UNRESOLVED_NAMES で検出されるべきだが、
`KnownSymbols` にオブジェクトマクロ名として登録されていて既知扱いになっている。

**改善策C'-1: KnownSymbols マクロ登録条件の厳格化**

`KnownSymbols` にマクロ名を登録する条件を変更:
- `info.is_function == true` のマクロのみ登録
- オブジェクトマクロ（`is_function == false`）は除外

**期待削減**: ~5 E0425。実装コスト: 極小。

---

### D'. 型名の未解決 (5 エラー, 旧 Category D と同じ)

| シンボル | E0425 数 |
|----------|---------|
| `body_details` | 2 |
| `PerlIO_funcs` | 2 |
| `caddr_t` | 1 |

**原因**: C の型名で `bindings.rs` に含まれていない。
`ExprKind::Cast` や関数パラメータの型名は `ExprKind::Ident` を経由しないため
UNRESOLVED_NAMES で検出されない。

**改善策D'-1**: `type_name_to_rust` で未知の型名を検出し
`unresolved_names` に記録する。ただし 5 件のみで優先度は低い。

---

### E'. __errno_location (1 エラー)

`__errno_location` は glibc の内部関数。`errno` マクロの展開結果。
libc crate では `libc::__errno_location` として提供されていない
（Rust では別のアプローチで errno を取得する）。

→ **対応**: この関数を呼ぶマクロを UNRESOLVED_NAMES 検出に任せる。
  `KnownSymbols` のビルトインリストから除外すれば検出される。

---

## 改善策の優先度と期待効果

| 優先度 | 改善策 | 対象 | 期待 E0425 削減 | 実装コスト |
|--------|--------|------|----------------|-----------|
| ~~1~~ | ~~F-1: クロスドメインカスケード検出~~ | ~~inline 関数依存~~ | ~~-46~~ | ✅ 完了 |
| 1 | C'-1: マクロ登録条件厳格化 | ローカル変数 | ~7 | 極小 |
| 2 | E'-1: __errno_location 除外 | errno | ~1 + カスケード | 極小 |
| 3 | D'-1: 型名チェック | 型名 | ~5 | 中 |
| — | H-1: 個別マクロ修正 | マクロ | ~9 | 中〜大 |

### 推奨実装順序

**Phase 1 (即実行可能)**: C'-1 + E'-1 — KnownSymbols の登録条件修正。
~8 E0425 + カスケード効果。

**Phase 2 (任意)**: D'-1 — 型名の未解決検出。5 件のみ。

### 全体目標の達成状況

当初目標: E0425 147 → 30 以下

**現在: 23** ✅ 目標達成

| 時点 | E0425 | 備考 |
|------|-------|------|
| 開始 | 147 | |
| UNRESOLVED_NAMES | 147 | 検出インフラ |
| `use libc` | 141 | -6 |
| マクロ間カスケード | 69 | -72 |
| **クロスドメインカスケード** | **23** | **-46** |
| 目標 | ≤30 | ✅ |

残り 23 件の内訳:
- H: マクロ生成失敗・未生成 (9): `SvIMMORTAL`(3), `toUPPER`(2), `RCPVx`(2), `SvSHARED_HEK_FROM_PV`(1), `Perl_newSV_type`(1)
- C': ローカル変数 (7): `s`(2), `c`(2), `t`(2), `n`(1)
- D': 型名未解決 (5): `PerlIO_funcs`(2), `body_details`(2), `caddr_t`(1)
- E': glibc 内部 (1): `__errno_location`(1)
- F: 間接依存 (1): `Perl_newSV_type`(1)
