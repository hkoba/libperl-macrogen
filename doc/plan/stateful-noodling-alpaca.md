# Plan: 依存順コード生成によるカスケード E0425 の解消

## Context

E0425 分析（`doc/plan/e0425-improvement-analysis.md`）の **Category B: カスケード依存問題**
（29 エラー）を解消する。

現状 `generate_macros()` はマクロをアルファベット順に処理する。
マクロ M が `should_emit_as_macro_call()` 経由で別マクロ U を関数呼び出しとして
保持するが、U がコメントアウトされていた場合、M の生成コードに `U(...)` が
残り E0425 になる。

例:
- `generic_isCC_` [CODEGEN_INCOMPLETE] → `isALPHA`, `isXDIGIT` が呼び出し → E0425
- `PUSHs` [UNRESOLVED_NAMES] → `mPUSHs`, `mXPUSHs` が呼び出し → E0425
- `isXDIGIT_A` → `generic_isCC_` 依存 → 多段カスケード → E0425

## 既存インフラ

`macro_infer.rs` には型推論で同様の依存順処理が実装済み:

- `MacroInferInfo.uses: HashSet<InternedStr>` — このマクロが使う他のマクロ (L263)
- `MacroInferInfo.used_by: HashSet<InternedStr>` — このマクロを使うマクロ (L265)
- `build_use_relations()` (L470) — `uses` から `used_by` を構築
- `get_inference_candidates()` (L510) — `uses` が全て確定済みのマクロを返す
- `infer_types_in_dependency_order()` (L1312) — 依存順でループ処理

この **同じ `uses` 関係** をコード生成でも活用する。

## 設計

### 原理

1. **依存順に生成**: 葉マクロ（uses が空）から先に処理
2. **成功追跡**: 正常出力されたマクロ名を `successfully_generated` 集合に蓄積
3. **カスケード検査**: マクロ M を生成する前に、M の `uses` のうち
   `should_emit_as_macro_call()` が true のものが全て `successfully_generated` に
   あるか検査。一つでも欠けていれば M を `[CASCADE_UNAVAILABLE]` としてコメントアウト
4. **多段伝播**: M がコメントアウトされると `successfully_generated` に入らないため、
   M の呼び出し元も自動的にカスケード検査で失敗する

---

## 実装

### Step 1: `CodegenStats` にカウンタ追加

**ファイル**: `src/rust_codegen.rs` (L465)

```rust
pub struct CodegenStats {
    // ... 既存フィールド ...
    /// カスケード依存でコメントアウトされたマクロ数
    pub macros_cascade_unavailable: usize,
}
```

### Step 2: トポロジカルソート関数の追加

**ファイル**: `src/rust_codegen.rs`

`CodegenDriver` に新メソッドを追加:

```rust
/// マクロを依存順にソート（葉マクロ先頭、循環はアルファベット順で末尾）
fn topological_sort_macros(
    &self,
    macros: &[(&InternedStr, &MacroInferInfo)],
) -> Vec<InternedStr>
```

アルゴリズム（Kahn's algorithm）:
1. 対象マクロ集合内の `uses` 関係から入次数マップを構築
2. 入次数 0 のマクロをキューに投入（アルファベット順で安定化）
3. キューから取り出し → 結果に追加 → 依存先の入次数を減算 → 0 になったらキューへ
4. 残りは循環メンバー → アルファベット順で末尾に追加

### Step 3: `generate_macros()` の書き換え

**ファイル**: `src/rust_codegen.rs` (L3046)

```rust
pub fn generate_macros(&mut self, result: &InferResult, known_symbols: &KnownSymbols) -> io::Result<()> {
    // 対象マクロを収集
    let macros: Vec<_> = result.infer_ctx.macros.iter()
        .filter(|(_, info)| self.should_include_macro(info))
        .collect();
    let included_set: HashSet<InternedStr> = macros.iter().map(|(n, _)| **n).collect();

    // 依存順にソート（旧: アルファベット順）
    let sorted_names = self.topological_sort_macros(&macros);

    // 正常生成されたマクロを追跡
    let mut successfully_generated: HashSet<InternedStr> = HashSet::new();

    for name in sorted_names {
        let info = result.infer_ctx.macros.get(&name).unwrap();

        // ── カスケード検査 ──
        // uses のうち関数呼び出しとして保持されるマクロが
        // successfully_generated に含まれていなければカスケード失敗
        let unavailable_deps: Vec<String> = info.uses.iter()
            .filter(|used| {
                included_set.contains(used)
                    && result.infer_ctx.macros.get(used)
                        .map(|u| u.is_parseable() && !u.calls_unavailable)
                        .unwrap_or(false)
                    && !successfully_generated.contains(used)
            })
            .map(|used| self.interner.get(*used).to_string())
            .collect();

        if !unavailable_deps.is_empty() {
            // [CASCADE_UNAVAILABLE] としてコメントアウト
            // → successfully_generated に入らない → 呼び出し元もカスケード
            self.generate_macro_cascade_unavailable(info, &unavailable_deps)?;
            self.stats.macros_cascade_unavailable += 1;
            continue;
        }

        // ── 以降は既存ロジックと同じ ──
        let status = self.get_macro_status(info);
        match status {
            GenerateStatus::Success => {
                let generated = ...;
                if !has_unresolved && is_complete {
                    successfully_generated.insert(name);  // ← 追加
                }
                // ... 既存の出力処理 ...
            }
            // ParseFailed, TypeIncomplete, CallsUnavailable, ContainsGoto, Skip
            // → 既存と同じ（successfully_generated には入らない）
        }
    }
    Ok(())
}
```

### Step 4: カスケードコメント出力メソッドの追加

**ファイル**: `src/rust_codegen.rs`

```rust
fn generate_macro_cascade_unavailable(
    &mut self,
    info: &MacroInferInfo,
    unavailable_deps: &[String],
) -> io::Result<()> {
    let name_str = self.interner.get(info.name);
    let thx_info = if info.is_thx_dependent { " [THX]" } else { "" };
    writeln!(self.writer,
        "// [CASCADE_UNAVAILABLE] {}{} - dependency not generated: {}",
        name_str, thx_info, unavailable_deps.join(", "))?;
    writeln!(self.writer)?;
    Ok(())
}
```

### Step 5: stats 出力の更新

**ファイル**: `src/main.rs` (L629)

```rust
eprintln!("Macros: {} success, {} parse failed, {} type incomplete, \
           {} cascade unavailable, {} unresolved names",
    stats.macros_success, stats.macros_parse_failed, stats.macros_type_incomplete,
    stats.macros_cascade_unavailable, stats.macros_unresolved_names);
```

注: `macros_calls_unavailable` は型推論時に既に除外されるため stats 行からは除外可能
（現状も表示されていない）。

---

## 変更ファイル一覧

| ファイル | 変更内容 |
|----------|----------|
| `src/rust_codegen.rs` | `CodegenStats` 拡張、`topological_sort_macros()` 追加、`generate_macros()` 書き換え、`generate_macro_cascade_unavailable()` 追加 |
| `src/main.rs` | stats 出力に `cascade unavailable` 追加 |

## 検証

1. `cargo build && cargo test` — 全テスト通過
2. `cargo run -- --auto --gen-rust samples/xs-wrapper.h --bindings samples/bindings.rs > /dev/null`
   → stats に `cascade unavailable` が非ゼロで表示されることを確認
3. 出力に `[CASCADE_UNAVAILABLE]` コメントが含まれることを確認
   （例: `isALPHA` が `generic_isCC_` 依存で検出される）
4. 統合ビルド → E0425 が ~29 減少することを確認
   （現在 141 → 目標 ~112 以下）
