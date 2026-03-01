# Plan: Assert 内の依存性追跡の修正

## 問題

`Perl_rpp_push_IMM`, `Perl_rpp_replace_1_IMM_NN`, `Perl_rpp_replace_2_IMM_NN` が
正常生成されているが、assert 内で `SvIMMORTAL` を呼んでいる。
`SvIMMORTAL` は `[CASCADE_UNAVAILABLE]` なので、これらの関数も
CASCADE_UNAVAILABLE になるべき。

```rust
// 変更前の出力（問題）
pub unsafe fn Perl_rpp_push_IMM(...) {
    assert!(!sv.is_null());
    assert!((SvIMMORTAL(my_perl, sv)) != 0);  // ← SvIMMORTAL は利用不可
    ...
}
```

## 根本原因（2つ）

### 原因 1: ExprKind::Assert のハンドリング漏れ

`macro_infer.rs` の以下 2 つの関数が `ExprKind::Assert` をハンドリングしていない:

| 関数 | 用途 | `ExprKind::Assert` |
|------|------|-------------------|
| `collect_uses_from_expr` | マクロの uses（def-use 関係）を収集 | **漏れ** (`_ => {}` に落ちる) |
| `collect_function_calls_from_expr` | called_functions を収集 | **漏れ** (`_ => {}` に落ちる) |
| `convert_assert_calls` | assert 呼び出しを Assert 式に変換 | ✓ 処理済み |

### 原因 2: inline→macro カスケード検出の欠落

`SvIMMORTAL` は推論段階では `get_macro_status() = Success` と判定されるが、
codegen 段階で `SvIMMORTAL_INTERP`（TypeIncomplete）が生成できず
CASCADE_UNAVAILABLE になる。しかし inline 関数はマクロより先に生成されるため、
マクロの codegen 結果を参照できない。

## 修正内容（実施済み）

### 修正 1: `src/macro_infer.rs`

2 箇所に `ExprKind::Assert` のハンドリングを追加:

```rust
// collect_uses_from_expr
ExprKind::Assert { condition, .. } => {
    Self::collect_uses_from_expr(condition, uses);
}

// collect_function_calls_from_expr
ExprKind::Assert { condition, .. } => {
    Self::collect_function_calls_from_expr(condition, calls);
}
```

### 修正 2: `src/rust_codegen.rs` — `precompute_macro_generability()`

inline 関数生成前にマクロの生成可能性を事前計算するメソッドを追加。
`generate_macros` と同じロジック（依存順ソート + カスケード検査）に加えて、
**trial codegen** を実行して `is_complete()` と `has_unresolved_names()` を確認する。

これにより `SvIMMORTAL_INTERP` → codegen incomplete → `SvIMMORTAL` → cascade failure
が inline 生成前に検出可能になる。

### 修正 3: `src/rust_codegen.rs` — Pass 2 拡張

`generate_inline_fns` の fixpoint ループを拡張し、inline→macro のカスケードも検査。
`generatable_macros` 集合（事前計算済み）を参照。

## 変更ファイル

| ファイル | 変更内容 |
|----------|----------|
| `src/macro_infer.rs` | `collect_uses_from_expr`, `collect_function_calls_from_expr` に `ExprKind::Assert` 追加 |
| `src/rust_codegen.rs` | `precompute_macro_generability()` 追加、Pass 2 で inline→macro カスケード検査、`generatable_macros` フィールド追加 |

## 結果

| ケース | 変更前 | 変更後 |
|--------|--------|--------|
| `Perl_rpp_push_IMM` | 正常生成（assert 内 SvIMMORTAL 呼出） | `[CASCADE_UNAVAILABLE]` |
| `Perl_rpp_replace_1_IMM_NN` | 正常生成（同上） | `[CASCADE_UNAVAILABLE]` |
| `Perl_rpp_replace_2_IMM_NN` | 正常生成（同上） | `[CASCADE_UNAVAILABLE]` |
| `Perl_rpp_xpush_IMM` | 正常生成 | `[CASCADE_UNAVAILABLE]`（Perl_rpp_push_IMM 依存） |

| Metric | ベースライン | 前コミット後 | 今回 |
|--------|------------|------------|------|
| E0425 errors | 17 | 15 | **12** |
| Total build errors | 1065 | 999 | **998** |
| Inline success | 116 | 107 | **103** |
| Inline cascade | 16 | 38 | **42** |
| Macro success | 1781 | 1739 | **1734** |
| Macro cascade | 502 | 522 | **526** |
