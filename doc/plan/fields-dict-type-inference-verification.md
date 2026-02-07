# タスク: フィールド型推論の構想と実装の確認

## 目的

`fields_dict` が型推論に役立っているかを検証し、改善点を特定する。

## 現在のアーキテクチャ理解

`FieldsDict` は以下の機能を提供する:

| 機能 | 説明 |
|------|------|
| `lookup_unique` | フィールド名から一意に構造体を特定 |
| `get_consistent_field_type` | 全構造体で型が一致するフィールドの型を取得 |
| `get_field_type_by_name` | 構造体名+フィールド名から型を取得 |
| `sv_u_field_types` | sv_u ユニオンフィールドの型 |
| `sv_head_type_to_struct` | _SV_HEAD typeName → 構造体名 |

## 役立つと想定されるマクロパターン

### 1. 一意なフィールドによるベース型推論 (`lookup_unique`)

フィールド名が一つの構造体にしか存在しない場合、`ptr->field` からベース型を逆推論できる。

```c
// xpv_pv は xpv 構造体に一意（と仮定）
#define SvPVX(sv)  ((sv)->sv_u.svu_pv)

// マクロパラメータ sv の型推論:
// sv->sv_u.svu_pv というアクセスパターンから
// sv が SV* 系の型であることを推論
```

```c
// av 構造体固有のフィールド（と仮定）
#define AvARRAY(av)  ((XPVAV*)SvANY(av))->xav_array
#define AvALLOC(av)  ((XPVAV*)SvANY(av))->xav_alloc
#define AvMAX(av)    ((XPVAV*)SvANY(av))->xav_max
#define AvFILL(av)   ((XPVAV*)SvANY(av))->xav_fill
```

### 2. 一致型キャッシュ (`get_consistent_field_type`)

複数の構造体で同名フィールドがあっても、全てで同じ型なら型推論可能。

```c
// xpv_cur は複数の XPV 系構造体に存在するが、全て STRLEN 型
#define SvCUR(sv)  (((XPV*)SvANY(sv))->xpv_cur)

// ベース型が不明でも、xpv_cur の型は STRLEN と推論可能
```

```c
// sv_flags は SV ファミリー全体で U32 型
#define SvFLAGS(sv)  ((sv)->sv_flags)
```

### 3. 構造体名が既知の場合のフィールド型取得 (`get_field_type_by_name`)

キャスト式でベース型が明示されている場合。

```c
// (XPVAV*) キャストにより構造体名が既知
#define AvARRAY(av)  (((XPVAV*)SvANY(av))->xav_array)

// XPVAV.xav_array の型を fields_dict から取得
// → SV** と推論
```

```c
// (XPVCV*) キャストで CV 構造体のフィールドにアクセス
#define CvSTASH(cv)  (((XPVCV*)SvANY(cv))->xcv_stash)
#define CvROOT(cv)   (((XPVCV*)SvANY(cv))->xcv_root)
```

### 4. sv_u ユニオンフィールド (`sv_u_field_types`)

SV 構造体の `sv_u` ユニオンのフィールド型。

```c
// sv_u.svu_pv → char*
#define SvPVX(sv)  ((sv)->sv_u.svu_pv)

// sv_u.svu_rv → SV*
#define SvRV(sv)   ((sv)->sv_u.svu_rv)

// sv_u.svu_fp → PerlIO* (IO 構造体)
#define IoIFP(sv)  (sv)->sv_u.svu_fp
```

### 5. SvANY キャストパターン (`sv_head_type_to_struct`)

`_SV_HEAD(typeName)` で登録された情報を使用。

```c
// _SV_HEAD(XPVAV*) で登録された av 構造体
// (XPVAV*)SvANY(x) の結果型を推論
#define AvARRAY(av)  (((XPVAV*)SvANY(av))->xav_array)

// _SV_HEAD(XPVHV*) で登録された hv 構造体
#define HvARRAY(hv)  (((XPVHV*)SvANY(hv))->xhv_array)
```

## 機能別まとめ

| FieldsDict 機能 | 役立つマクロパターン | 例 |
|----------------|---------------------|-----|
| `lookup_unique` | 一意フィールドへのアクセス | `AvARRAY`, `HvKEYS` |
| `get_consistent_field_type` | 共通フィールドへのアクセス | `SvCUR`, `SvLEN`, `SvFLAGS` |
| `get_field_type_by_name` | キャスト付きアクセス | `CvSTASH`, `CvROOT` |
| `sv_u_field_types` | sv_u ユニオンアクセス | `SvPVX`, `SvRV`, `IoIFP` |
| `sv_head_type_to_struct` | SvANY キャスト | `(XPVAV*)SvANY(av)` |

## 検証フェーズ

### Phase 1: 現状確認

各機能について、対象マクロの Rust コード生成結果を確認する。

#### 1.1 lookup_unique の検証
- [ ] `HvKEYS` の Rust 関数生成結果を確認
- [ ] パラメータ `hv` の型が `*mut HV` になっているか

#### 1.2 get_consistent_field_type の検証
- [ ] `SvCUR` の Rust 関数生成結果を確認
- [ ] 戻り値型が `STRLEN` になっているか

#### 1.3 get_field_type_by_name の検証
- [ ] `CvSTASH` の Rust 関数生成結果を確認
- [ ] 戻り値型が `*mut HV` になっているか

#### 1.4 sv_u_field_types の検証
- [ ] `SvRV` の Rust 関数生成結果を確認
- [ ] 戻り値型が `*mut SV` になっているか
- [ ] `IoIFP` の Rust 関数生成結果を確認

#### 1.5 sv_head_type_to_struct の検証
- [ ] `AvARRAY` の Rust 関数生成結果を確認
- [ ] 戻り値型が `*mut *mut SV` になっているか

#### 1.6 内部動作の確認
- [ ] `--debug-type-inference` で制約収集過程を調査
- [ ] `fields_dict` の各機能が実際に呼ばれているか確認

### Phase 2: 問題特定

- [ ] 型が正しく推論されていないマクロを特定
- [ ] 原因を分析（fields_dict の問題か、他の問題か）

### Phase 3: 改善

- [ ] 必要に応じて fields_dict の機能を強化
- [ ] regression テストに追加

## 関連ファイル

| ファイル | 役割 |
|----------|------|
| `src/fields_dict.rs` | FieldsDict 本体 |
| `src/semantic.rs` | FieldsDict の活用（型推論） |
| `doc/architecture-fields-dict.md` | アーキテクチャ文書 |
