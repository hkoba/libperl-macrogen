# Plan: 型の確度 (Tier) に基づく const/mut 推論と安全なコード生成

## 背景

現在 const→mut キャストを3箇所で生成しているが、これは Rust の安全性モデルに反する。
代わりに以下の2つのアプローチを採用する:

1. **安全なコードが生成できない場合はコメントとして出力**し、問題点を列挙する
2. **型の確度 (Tier)** に基づいて const/mut を判定し、低 Tier 側を調整する

## 型の確度 Tier

| Tier | 情報源 | 変更可能性 | 例 |
|------|--------|-----------|-----|
| **Tier 1: 不変** | `bindings.rs` (bindgen生成) | 変更不可 | `Perl_sv_dup(ssv: *const SV)` |
| **Tier 2: 高確度** | C ヘッダー宣言 (inline 関数パラメータ) | 変更不可 | `Perl_foldEQ(s1: const char*)` |
| **Tier 3: 中確度** | apidoc (`embed.fnc`) | 参考情報、変更可能 | `=for apidoc Am|int|SvIOK|SV* sv` |
| **Tier 4: 推論** | マクロ型推論 (Phase 2) | 変更可能 | `SvTYPE(sv)` の `sv` の型推論結果 |

### const/mut 判定ルール

Tier の高い方が低い方に合わせるのではなく、**低い方が高い方に合わせる**:

- **Tier 1 (bindings) が `*mut`** → Tier 4 (推論) も `*mut` にすべき
- **Tier 1 (bindings) が `*const`** → Tier 4 (推論) も `*const` にすべき
- **Tier 4 同士の衝突** → Phase 2 の must-mut 解析結果に従う

### 具体例

```
newSVsv_flags(a, b, c) → Perl_newSVsv_flags(aTHX_ a, b, c)
```

- `Perl_newSVsv_flags` の `a` は `*mut SV` (Tier 1: bindings)
- `newSVsv_flags` の `a` は Phase 2 推論で `*const SV` (Tier 4)
- Tier 1 > Tier 4 → `newSVsv_flags` の `a` を `*mut SV` に**戻す**

これは const→mut キャストではなく、推論結果の修正。

## 実現可能性

### 既存インフラ

型の出所は既に `TypeRepr` の `source` フィールドで追跡されている:

| TypeRepr variant | source フィールド | 対応 Tier |
|-----------------|-------------------|-----------|
| `RustType { source: FnParam }` | bindings.rs 関数引数 | Tier 1 |
| `RustType { source: FnReturn }` | bindings.rs 関数戻り値 | Tier 1 |
| `RustType { source: Const }` | bindings.rs 定数 | Tier 1 |
| `CType { source: InlineFn }` | inline 関数 AST | Tier 2 |
| `CType { source: Apidoc }` | embed.fnc | Tier 3 |
| `CType { source: SvFamilyCast }` | SV ファミリーキャスト推論 | Tier 4 |
| `CType { source: FieldInference }` | フィールドアクセス推論 | Tier 4 |
| `CType { source: Cast }` | キャスト式 | Tier 4 |

→ **Tier を source から機械的に導出可能**。新しいフィールドの追加は不要。

### 必要な変更

#### Phase 2: const/mut 推論の改善

現在の `collect_must_mut_pointer_params` は「呼び出し先の const 情報」だけを見る。
これを Tier ベースに拡張:

1. パラメータが `Call(Perl_xxx, [param])` で使われる場合、
   `Perl_xxx` の引数型は Tier 1 (bindings)
2. パラメータの現在の型制約の source を確認:
   - Tier 4 (推論) で `*const` → Tier 1 に合わせて `*mut` に
   - Tier 1 (bindings) で `*const` → 変更不可

**具体的な変更**: `collect_must_mut_pointer_params` で、
呼び出し先が bindings.rs 関数（Tier 1）で `*mut` を要求する場合は must-mut とする。
これは **既存ロジックの延長** で実現可能。

#### Phase 2: let 宣言の型推論

inline 関数の `let a: *mut U8 = (s1 as *const U8)` ケース:
- `a` の宣言型 `*mut U8` は C の AST から来る（Tier 2: inline 関数宣言）
- 初期化式の `(s1 as *const U8)` は Cast 式
- Tier 2 > Tier 4 → 宣言型 `*mut` が優先

ただしこのケースは、C でも `const` キャスト結果を `non-const` 変数に入れている。
**C のソースコード自体が const-correctness に違反している**。

→ 対処: `a` の使用箇所を解析し、書き込みがなければ `let a: *const U8` に変更。
書き込みがあれば C ソースの意図通り `*mut` のまま（キャストは C の挙動を反映）。

#### Phase 3: 安全でないコードのコメント出力

生成コードが unsafe 操作（const→mut 等）を含む場合、
関数全体をコメントアウトして問題点を列挙:

```rust
// [UNSAFE_CONST_MUT] newSVsv - macro function
// const→mut cast required at argument 1 of newSVsv_flags()
//   caller param: sv (*const SV, inferred Tier 4)
//   callee expects: a (*mut SV, bindings Tier 1)
// pub unsafe fn newSVsv(my_perl: *mut PerlInterpreter, sv: *const SV) -> *mut SV {
//     unsafe { newSVsv_flags(my_perl, sv, ...) }
// }
```

---

## 実装計画

### Step 1: TypeRepr に Tier 導出メソッドを追加

```rust
impl TypeRepr {
    pub fn confidence_tier(&self) -> u8 {
        match self {
            TypeRepr::RustType { source, .. } => match source {
                RustTypeSource::FnParam { .. } |
                RustTypeSource::FnReturn { .. } |
                RustTypeSource::Const { .. } => 1,
                RustTypeSource::Parsed { .. } => 3,
            },
            TypeRepr::CType { source, .. } => match source {
                CTypeSource::InlineFn { .. } | CTypeSource::Header => 2,
                CTypeSource::Apidoc { .. } => 3,
                _ => 4,
            },
            TypeRepr::Inferred(_) => 4,
        }
    }
}
```

### Step 2: Phase 2 の const/mut 推論に Tier を組み込む

`collect_must_mut_pointer_params` を拡張:
- 呼び出し先が **bindings.rs 関数** (Tier 1) で `*mut` を要求する場合、must-mut
- 呼び出し先が **自家生成マクロ** (Tier 4) で `*mut` の場合、
  そのマクロ自身も Tier 1 由来で must-mut なら must-mut を伝播

これにより `newSVsv` の `sv` が正しく `*mut SV` になる。

### Step 3: 既存の const→mut キャスト生成を除去

3箇所の const→mut キャスト生成を削除:
1. `cast_integer_arg_if_needed` の const→mut (L2270-2281)
2. `cast_return_expr_if_needed` の const→mut (L2305, L2324)
3. `decl_to_rust_let` の const→mut (L3973-3987)

代わりに、const/mut 不一致が残った場合は:
- 関数を `[CONST_MUT_CONFLICT]` としてコメントアウト
- 問題点を列挙

### Step 4: let 宣言の const 化

inline 関数の let 宣言で、変数への書き込みがなければ `*const` に変更。
`collect_mut_params` の inline 版を使って判定。

---

## 段階的実装

| Phase | 内容 | リスク | エラー影響予想 |
|-------|------|--------|--------------|
| I | Tier 導出メソッド追加 | 低 | 0 |
| II | Phase 2 の must-mut に Tier 1 伝播追加 | 中 | -20〜-30 (正しく mut に戻る) |
| III | const→mut キャスト除去 + コメント出力 | 中 | +10〜+20 (一時的に増加) |
| IV | let 宣言の const 化 | 低 | -5〜-10 |

Phase II を実施すれば、Phase III で増加するエラーは最小限に抑えられるはず。

## 期待効果

- const→mut キャストが 0 になり、生成コードの安全性が向上
- Tier ベースの判定により、推論の精度が向上
- 安全でないコードは明示的にコメントアウトされ、問題が可視化される
