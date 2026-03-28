# Plan: パラメータポインタの const/mut 推論強化

## 目標

マクロ関数のポインタパラメータについて、本体の解析により `*mut` が不要な場合に
`*const` に変更する。例：

```rust
// Before
pub unsafe fn SvTYPE(sv: *mut SV) -> svtype;
// After
pub unsafe fn SvTYPE(sv: *const SV) -> svtype;
```

### 対象関数（ユーザ提示のエラーから）

| 関数 | 現在 | 目標 |
|------|------|------|
| `isREGEXP(sv)` | `*mut SV` | `*const SV` |
| `isGV_with_GP(pwadak)` | `*mut SV` | `*const SV` |
| `SvTYPE(sv)` | `*mut SV` | `*const SV` |
| `SvROK(sv)` | `*mut SV` | `*const SV` |
| `SvRV(sv)` | `*mut SV` | `*const SV` |
| `SvPVX_const(sv)` | `*mut SV`, 戻り値 `*mut c_char` | `*const SV`, 戻り値 `*const c_char` |
| `ReANY(re)` | `*mut SV` | `*const SV` |

---

## 現状の問題分析

### 問題1: SV ファミリーキャストが常に `*mut` を生成

`semantic.rs:1067` の `make_sv_ptr_type()` が `is_const: false` 固定で
`*mut SV` 制約を生成する。
```rust
CDerivedType::Pointer { is_const: false, ... }  // 常に *mut
```

本来は「キャスト先が `const` かどうか」を見て決めるべき。

### 問題2: 更新操作の有無を考慮しない型推論

現在の型推論は「パラメータがどの型で使われているか」のみを見る。
「パラメータが更新されるかどうか」は `collect_mut_params()` で追跡されるが、
これは変数自体の再代入 (`param = ...`) のみを検出し、
ポインタ経由の更新 (`*param = ...`, `param->field = ...`) は含むが
ポインタの const/mut の決定には使われていない。

### 問題3: 呼び出す関数の引数 mutability が考慮されない

マクロ A がマクロ B を `B(param)` で呼ぶとき、
B のパラメータが `*mut SV` なら A のパラメータも `*mut SV` になるべき。
逆に B が `*const SV` なら A は `*const SV` で済むかもしれない。

現状は `collect_call_constraints()` が呼び出し先の型をそのまま伝播するので、
B が `*mut SV` なら A にも `*mut SV` が伝わる。
→ B を先に `*const SV` に修正すれば、A にも伝播する。

---

## 設計方針

### アプローチ: 「デフォルト const、mut が必要な場合のみ mut」

パラメータのポインタ型を決定する際、以下のルールで const/mut を判定する：

1. **const にできない条件（must-mut）の検出**:
   - パラメータ経由の書き込み: `*param = ...`, `param->field = ...`
   - `*mut` を要求する関数への引数渡し（呼び出し先の引数が `*mut`）
   - `AddrOf(Deref(param))` = `&(*param)` で mut 参照が必要なケース

2. **const にできる条件**:
   - 読み取りのみ: `(*param).field`, `*param`（読み取り）
   - `*const` を受ける関数への引数渡し
   - キャスト: `(const SomeType*)param`

3. **依存順序**: def/use 関係に基づくトポロジカル順で処理し、
   呼び出し先マクロの const/mut が確定してから呼び出し元を処理

### 処理フロー

```
Phase 1: 既存の型推論（現状通り、*mut SV で制約生成）
Phase 2: const/mut 解析（新規追加）
  2a. 各マクロの各ポインタパラメータについて must-mut 条件を収集
  2b. 依存順序で処理（リーフマクロから）
  2c. must-mut でなければ *mut → *const に変換
Phase 3: コード生成（変換後の型を使用）
```

---

## 実装計画

### Phase 1: must-mut 解析関数の実装

**場所**: `src/rust_codegen.rs` に新関数を追加

```rust
/// ポインタパラメータが *mut である必要があるか判定する
fn param_requires_mut_pointer(
    parse_result: &ParseResult,
    param_name: InternedStr,
    callee_param_types: &HashMap<String, Vec<(usize, bool)>>,  // func → [(arg_idx, is_mut)]
) -> bool
```

**must-mut 条件の検出**:

| パターン | AST 表現 | must-mut |
|----------|----------|----------|
| `*param = expr` | `Assign { lhs: Deref(Ident(param)), ... }` | Yes |
| `param->field = expr` | `Assign { lhs: PtrMember { expr: Ident(param), ... }, ... }` | Yes |
| `(*param).field = expr` | `Assign { lhs: Member { expr: Deref(Ident(param)), ... }, ... }` | Yes |
| `func(param)` where func takes `*mut` | `Call { args: [Ident(param)] }` | Yes |
| `(SomeType*)param` → `*mut` cast | `Cast { expr: Ident(param) }` with non-const pointer | Yes |
| `param++`, `param--` 等 | `PostInc(Ident(param))` etc. | Yes (pointer itself modified) |
| `(*param).field` (read) | `Member { expr: Deref(Ident(param)) }` 右辺 | No |
| `func(param)` where func takes `*const` | `Call { args: [Ident(param)] }` | No |

### Phase 2: 依存順序での const/mut 確定

**場所**: `src/rust_codegen.rs` の `generate()` 関数内、
コード生成の前に const/mut 解析パスを追加

```rust
// 依存順序で処理（リーフマクロから）
let ordered = macro_ctx.get_generation_order();  // 既存の依存順序
let mut const_params: HashMap<InternedStr, HashSet<InternedStr>> = HashMap::new();

for name in &ordered {
    if let Some(info) = macro_ctx.macros.get(name) {
        for param in &info.params {
            let requires_mut = param_requires_mut_pointer(
                &info.parse_result,
                param.name,
                &callee_mut_info,  // 確定済みの呼び出し先の mut/const 情報
            );
            if !requires_mut {
                const_params.entry(*name).or_default().insert(param.name);
            }
        }
        // この関数の結果を callee_mut_info に追加
        update_callee_mut_info(name, info, &const_params, &mut callee_mut_info);
    }
}
```

### Phase 3: TypeRepr レベルでの const 変換

**場所**: `src/rust_codegen.rs` の `get_param_type()` 付近

パラメータが `const_params` に含まれている場合、
`TypeRepr` の `CDerivedType::Pointer { is_const }` を `true` に変更する。

**文字列置換ではなく構造化型を操作する理由**:
- `TypeRepr` は `CDerivedType::Pointer { is_const, ... }` で const/mut を構造的に保持
- `cache_param_types_to()` のキャッシュは現在 `String` だが、
  const 変換は `TypeRepr` 取得時点（文字列化の前）に行うのが正確

```rust
fn get_param_type(&mut self, param: &MacroParam, info: &MacroInferInfo, param_index: usize) -> String {
    // ... 既存のジェネリック/リテラル文字列チェック ...

    // TypeRepr を取得
    let type_repr = self.get_param_type_repr(param, info);

    if let Some(mut ty) = type_repr {
        // const/mut 変換: must-mut でなければ最外ポインタを const に
        if self.const_params.get(&current_macro).map_or(false, |s| s.contains(&param.name)) {
            ty.make_outer_pointer_const();  // TypeRepr に追加するメソッド
        }
        return self.type_repr_to_rust(&ty);
    }
    self.unknown_marker().to_string()
}
```

**`TypeRepr::make_outer_pointer_const()`** — `type_repr.rs` に追加:

```rust
impl TypeRepr {
    /// 最外ポインタの is_const を true に変更する
    pub fn make_outer_pointer_const(&mut self) {
        match self {
            TypeRepr::CType { derived, .. } => {
                // derived の最後（最外側）のポインタを const に
                if let Some(last_ptr) = derived.iter_mut().rev()
                    .find(|d| matches!(d, CDerivedType::Pointer { .. }))
                {
                    if let CDerivedType::Pointer { is_const, .. } = last_ptr {
                        *is_const = true;
                    }
                }
            }
            TypeRepr::RustType { repr, .. } => {
                repr.make_outer_pointer_const();  // RustTypeRepr にも同様のメソッド
            }
            _ => {}
        }
    }
}
```

**キャッシュへの反映**:
- `cache_param_types_to()` は `to_rust_string()` で文字列化してキャッシュに保存
- const 変換を `cache_param_types_to()` の **前** に適用すれば、
  キャッシュには変換済みの文字列が保存される
- これにより下流マクロの `collect_call_constraints()` が
  `TypeRepr::from_rust_string()` で復元する際にも `*const` が正しく伝播する

### Phase 4: SvPVX_const の戻り値修正

`SvPVX_const` は戻り値が `*const c_char` であるべきだが、
現在 `*mut c_char` になっている。これは:
- C のキャスト `(const char*)` の `const` が戻り値型に反映されていない可能性

戻り値の const は、式の型推論結果（Cast の target type）から
そのまま反映されるべき。Phase 1 の `type_name_to_type_str_readonly` 改善で
既に部分的に対応されているが、追加の確認が必要。

---

## 注意点

### 保守的アプローチ

- **安全側に倒す**: 判定できない場合は `*mut` のまま
- **段階的に拡大**: まず明確なケースのみ対応し、徐々に拡張

### inline 関数との整合

- inline 関数のパラメータは C の宣言から const/mut が決まる
- マクロ関数のみが推論対象

### キャッシュの更新

- `param_types_cache` に const/mut 変換後の型を格納する必要がある
- 依存順序で処理するため、下流マクロの解析時に上流の結果が利用可能

---

## テスト計画

1. `cargo test` — 既存テスト通過
2. `~/blob/libperl-rs/12-macrogen-2-build.zsh` — エラー数減少の確認
3. 個別確認:
   - `grep 'fn SvTYPE' tmp/macro_bindings.rs` → `*const SV` になること
   - `grep 'fn isREGEXP' tmp/macro_bindings.rs` → `*const SV` になること
   - `grep 'fn SvPVX_const' tmp/macro_bindings.rs` → 戻り値 `*const c_char`
4. regression test の expected output 更新（`Perl_CvDEPTH.rs` 等）

## 期待される効果

- mutability mismatch エラー（ユーザ提示の7件+関連エラー）の解消
- 生成コードの型安全性向上
