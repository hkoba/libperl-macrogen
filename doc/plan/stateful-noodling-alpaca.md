# Plan: ジェネリックマクロ呼び出しの turbofish 構文生成

## Context

`xV_FROM_REF(XV, ref)` は `fn xV_FROM_REF<T>(r#ref: *mut SV) -> *mut T` として正しく
定義されるようになった。しかし、呼び出し側（例: `AV_FROM_REF(ref)` → `xV_FROM_REF(AV, ref)`）
では型引数が通常の値引数として出力されている:

```rust
// 現状（不正）
xV_FROM_REF(AV, r#ref)

// 期待
xV_FROM_REF::<AV>(r#ref)
```

## 設計

### アプローチ

`expr_to_rust` / `expr_to_rust_inline` の `ExprKind::Call` ハンドラで、
呼び出し先が `generic_type_params` を持つマクロの場合、型パラメータに対応する引数を
turbofish 構文に分離する。

`needs_my_perl_for_call` と同じパターンで `macro_ctx` から呼び出し先のメタデータを参照する。

### 処理フロー

```
ExprKind::Call { func: Ident("xV_FROM_REF"), args: [Ident("AV"), Ident("ref")] }

1. macro_ctx.macros.get("xV_FROM_REF")
   → callee_info.generic_type_params = { 0 => "T" }

2. 引数を分類:
   - args[0] ("AV"): generic_type_params に index 0 がある → 型引数
   - args[1] ("ref"): generic_type_params に index 1 がない → 値引数

3. 型引数を expr_to_rust で文字列化: "AV"
4. 値引数を expr_to_rust で文字列化: "r#ref"

5. 出力: "xV_FROM_REF::<AV>(r#ref)"
```

## 実装

### ファイル: `src/rust_codegen.rs`

#### 1. ヘルパーメソッド追加

```rust
/// 呼び出し先マクロのジェネリック型パラメータ情報を取得
/// Returns: Some(generic_type_params) if callee has type params, None otherwise
fn get_callee_generic_params(&self, func_name: InternedStr) -> Option<&HashMap<i32, String>> {
    let callee_info = self.macro_ctx.macros.get(&func_name)?;
    if callee_info.generic_type_params.is_empty() {
        return None;
    }
    // 値パラメータ用の型引数のみ（index >= 0）
    if callee_info.generic_type_params.keys().any(|&k| k >= 0) {
        Some(&callee_info.generic_type_params)
    } else {
        None
    }
}
```

#### 2. `expr_to_rust` の Call ハンドラ修正

既存の `needs_my_perl` チェックの後に、ジェネリック型引数の分離ロジックを追加:

```rust
ExprKind::Call { func, args } => {
    // ... 既存の __builtin_expect, offsetof 処理 ...

    // ジェネリック型パラメータのチェック
    let callee_generics = if let ExprKind::Ident(name) = &func.kind {
        self.get_callee_generic_params(*name)
            .map(|g| g.clone())
    } else {
        None
    };

    if let Some(ref generics) = callee_generics {
        // 型引数と値引数を分離
        let mut type_args = Vec::new();
        let mut value_args = Vec::new();

        for (i, arg) in args.iter().enumerate() {
            if generics.contains_key(&(i as i32)) {
                type_args.push(self.expr_to_rust(arg, info));
            } else {
                value_args.push(self.expr_to_rust(arg, info));
            }
        }

        // THX my_perl 注入
        if needs_my_perl {
            value_args.insert(0, "my_perl".to_string());
        }

        let f = self.expr_to_rust(func, info);
        return format!("{}::<{}>({})",
            f,
            type_args.join(", "),
            value_args.join(", "));
    }

    // ... 既存の通常関数呼び出し処理 ...
}
```

#### 3. `expr_to_rust_inline` にも同様の修正

同じロジックを `expr_to_rust_inline` の `ExprKind::Call` ハンドラにも適用。

## 変更ファイル一覧

| ファイル | 変更内容 |
|----------|----------|
| `src/rust_codegen.rs` | `get_callee_generic_params` 追加、`expr_to_rust` / `expr_to_rust_inline` の Call ハンドラにジェネリック型引数の分離ロジック追加 |

## 検証

1. `cargo build` / `cargo test`

2. 出力確認:
   ```bash
   cargo run -- --auto --gen-rust samples/xs-wrapper.h --bindings samples/bindings.rs 2>/dev/null \
     | grep -E 'xV_FROM_REF|AV_FROM_REF|CV_FROM_REF|HV_FROM_REF|INT2PTR|NUM2PTR|PTR2IV|PTR2UV'
   ```
   - `xV_FROM_REF::<AV>(r#ref)` 形式で出力されること
   - `INT2PTR::<IV>(p)` 形式で出力されること

3. 回帰テスト: `cargo test rust_codegen_regression`

## エッジケース

1. **呼び出し先が macro_ctx に存在しない**: `get_callee_generic_params` が `None` を返す → 通常の関数呼び出しとして処理

2. **THX + ジェネリック**: `needs_my_perl` と `callee_generics` が同時に適用される場合
   → 値引数に `my_perl` を先頭に追加

3. **全引数が型パラメータ**: 値引数が空になる → `func::<T>()` として生成（正しい）
