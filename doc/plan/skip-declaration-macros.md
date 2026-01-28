# オブジェクトマクロの関数生成スキップ

## 概要

`BASEOP`、`dSP`、`PL_sv_undef` などの引数を持たないマクロ（オブジェクトマクロ）が
Rust 関数として生成されているが、これらは不要である。

## 背景

オブジェクトマクロは、他のマクロ内で使用される際に**常にインライン展開**される。

```c
#define PL_stack_sp  (my_perl->Istack_sp)
#define dSP          SV **sp = PL_stack_sp
```

生成されたコードを確認すると：
- `PL_stack_sp` → `(*my_perl).Istack_sp` として展開
- 関数呼び出し `PL_stack_sp(my_perl)` は存在しない

つまり、オブジェクトマクロを Rust 関数として生成しても、生成コード内では使われない。

## 現在の動作

### 現在のフィルタリングロジック（`should_include_macro`）

```rust
fn should_include_macro(&self, info: &MacroInferInfo) -> bool {
    if !info.is_target { return false; }
    if !info.has_body { return false; }

    // 関数形式マクロまたは THX 依存オブジェクトマクロ
    info.is_function || info.is_thx_dependent
}
```

`is_thx_dependent` により、THX 依存のオブジェクトマクロも含まれてしまう。

### 不要な関数が生成される例

```rust
// PL_sv_undef - 使われない関数が生成される
pub unsafe fn PL_sv_undef(my_perl: *mut PerlInterpreter) -> SV {
    unsafe { (*my_perl).Isv_undef }
}

// dSP - 意味のない関数が生成される
pub unsafe fn dSP(my_perl: *mut PerlInterpreter) -> () {
    unsafe { { (SV * (*sp)) = (*my_perl).Istack_sp; (SV * (*sp)) } }
}
```

## 解決策

### 方針

**オブジェクトマクロは関数として生成しない**

オブジェクトマクロは常にインライン展開されるため、Rust 関数として生成する必要がない。

### 判定ロジック

```rust
fn should_include_macro(&self, info: &MacroInferInfo) -> bool {
    if !info.is_target { return false; }
    if !info.has_body { return false; }

    // 関数形式マクロのみ含める
    info.is_function
}
```

### 動作確認

| マクロ | `is_function` | 結果 |
|--------|---------------|------|
| `dSP` | `false` | スキップ |
| `BASEOP` | `false` | スキップ |
| `PL_sv_undef` | `false` | スキップ |
| `PL_stack_sp` | `false` | スキップ |
| `SvFLAGS(sv)` | `true` | 含める |
| `SvREFCNT_inc(sv)` | `true` | 含める |

## 実装手順

| Step | 内容 |
|------|------|
| 1 | `should_include_macro` を修正（`info.is_function` のみに変更） |
| 2 | テストと検証 |

## テスト方法

```bash
# 結合テスト
~/blob/libperl-rs/12-macrogen-2-build.zsh

# オブジェクトマクロが生成されていないことを確認
grep "pub unsafe fn dSP\|pub unsafe fn BASEOP\|pub unsafe fn PL_sv_undef" tmp/macro_bindings.rs
# → 出力がないこと

# 関数形式マクロは引き続き生成されることを確認
grep "pub unsafe fn SvFLAGS\|pub unsafe fn SvREFCNT" tmp/macro_bindings.rs
# → 出力があること

# エラー数の確認
grep -c "^error\[E" tmp/build-error.log
```

## 影響範囲

### スキップされるマクロ

全てのオブジェクトマクロ（`is_function = false`）：
- `PL_sv_undef`, `PL_stack_sp`, `PL_stack_base` など
- `dSP`, `djSP`, `dMARK`, `dORIGMARK`, `dTARGET` など
- `BASEOP` など

### 引き続き生成されるマクロ

関数形式マクロのみ（`is_function = true`）：
- `SvFLAGS(sv)`, `SvREFCNT(sv)`, `SvREFCNT_inc(sv)` など

## 備考

- オブジェクトマクロは他マクロ内で展開されるため、関数として提供する必要がない
- 生成される関数数が減り、コンパイル時間とバイナリサイズが改善される可能性がある
