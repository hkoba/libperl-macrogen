# --apidoc 省略時の自動ロード

## 背景

現在、`--apidoc` オプションを省略すると空の `ApidocDict` が作成される：

```rust
let mut apidoc = if let Some(path) = apidoc_path {
    ApidocDict::load_auto(path)?
} else {
    ApidocDict::new()  // ← 空
};
```

`apidoc/` ディレクトリには Perl バージョンごとの JSON ファイルが用意されている：

```
apidoc/
├── v5.10.json
├── v5.12.json
├── ...
├── v5.40.json
└── v5.42.json
```

## 目標

`--apidoc` 省略時に、実行環境の Perl バージョンに対応する JSON ファイルを自動ロードする。

## 設計

### 1. Perl バージョンの取得

`perl_config.rs` に `get_perl_version()` 関数を追加：

```rust
/// Perl のメジャー・マイナーバージョンを取得 (例: "5.40")
pub fn get_perl_version() -> Result<(u32, u32), PerlConfigError> {
    // $Config{version} は "5.40.0" のような形式
    let version = get_config_value("version")?;
    // "5.40" 部分を抽出してパース
    ...
}
```

### 2. apidoc JSON ファイルの検索

`apidoc.rs` に検索関数を追加：

```rust
impl ApidocDict {
    /// 指定バージョン用の JSON ファイルパスを検索
    ///
    /// apidoc/v{major}.{minor}.json が存在すれば Some(path)、なければ None
    /// フォールバックは行わない（完全一致のみ）
    pub fn find_json_for_version(
        apidoc_dir: &Path,
        major: u32,
        minor: u32,
    ) -> Option<PathBuf>
}
```

### 3. 自動ロード関数

```rust
impl ApidocDict {
    /// Perl バージョンに基づいて apidoc を自動ロード
    ///
    /// apidoc_dir: apidoc/ ディレクトリのパス
    /// 成功時: 対応する JSON からロードした ApidocDict
    /// 失敗時: io::Error（ファイルが見つからない場合など）
    pub fn load_for_perl_version(
        apidoc_dir: &Path,
        major: u32,
        minor: u32,
    ) -> io::Result<Self>
}
```

### 4. apidoc ディレクトリの場所

実行ファイルからの相対パスで `apidoc/` を探す：

```rust
/// apidoc ディレクトリのパスを取得
///
/// 検索順序:
/// 1. 実行ファイルと同じディレクトリの apidoc/
/// 2. 実行ファイルの親ディレクトリの apidoc/ (開発時: target/debug/../apidoc)
/// 3. カレントディレクトリの apidoc/
pub fn find_apidoc_dir() -> Option<PathBuf>
```

### 5. main.rs の変更

```rust
// Apidoc をロード（ファイルから + コメントから）
let mut apidoc = if let Some(path) = apidoc_path {
    // 明示的に指定された場合
    ApidocDict::load_auto(path)?
} else if cli.auto {
    // --auto モードで --apidoc 省略時: Perl バージョンから自動検索
    let (major, minor) = get_perl_version()?;

    // 奇数マイナーバージョン（開発版）はエラー
    if minor % 2 == 1 {
        return Err(format!(
            "Perl {}.{} is a development version.\n\
             Please specify --apidoc explicitly (e.g., --apidoc path/to/embed.fnc)",
            major, minor
        ).into());
    }

    // apidoc ディレクトリを検索
    let apidoc_dir = find_apidoc_dir()
        .ok_or("apidoc directory not found")?;

    // バージョンに対応する JSON ファイルをロード
    ApidocDict::load_for_perl_version(&apidoc_dir, major, minor)?
} else {
    // 手動モードでは空
    ApidocDict::new()
};
```

## バージョンマッチング戦略

### 開発版（奇数マイナーバージョン）の扱い

Perl のマイナーバージョンが奇数（5.39, 5.41 など）は**開発版**である。
開発版では API が安定していないため、自動ロードは行わない。

```
Error: Perl 5.39 is a development version.
Please specify --apidoc explicitly (e.g., --apidoc path/to/embed.fnc)
```

### 安定版（偶数マイナーバージョン）の自動ロード

1. **完全一致**: `v5.40.json` が存在すれば使用
2. **見つからない場合**: エラー（将来のバージョン用 JSON がまだない場合など）

```
Error: apidoc/v5.44.json not found for Perl 5.44.
Please specify --apidoc explicitly or add the JSON file.
```

### フォールバックは行わない

- バージョン間で API が変わる可能性があるため、異なるバージョンの JSON を使うのは危険
- ユーザーが明示的に `--apidoc` を指定すれば、任意のファイルを使用可能

## ファイル名規則

現在のファイル名: `v5.XX.json`（メジャー.マイナー）

この形式を維持。パッチバージョンは無視する：
- Perl 5.40.0, 5.40.1 → v5.40.json

## 実装フェーズ

### Phase 1: perl_config.rs の拡張

1. `get_perl_version()` 関数を追加
2. 戻り値: `(major, minor)` タプル

### Phase 2: apidoc.rs の拡張

1. `find_json_for_version()` を追加
2. `load_for_perl_version()` を追加

### Phase 3: 実行ファイルからの apidoc ディレクトリ検索

1. `find_apidoc_dir()` を追加（main.rs または新モジュール）

### Phase 4: main.rs の統合

1. `--auto` モードで自動ロードを有効化
2. 警告メッセージの追加

### Phase 5: テストと動作確認

1. 安定版（偶数マイナー）での自動ロード確認
2. 開発版（奇数マイナー）でのエラー確認
3. 対応 JSON がない場合のエラー確認

## 期待される動作

```bash
# 明示的指定（従来通り）
cargo run -- --auto --apidoc apidoc/v5.38.json samples/wrapper.h

# 自動ロード（Perl 5.40 環境の場合）
cargo run -- --auto samples/wrapper.h
# → apidoc/v5.40.json を自動ロード
# → "Note: Auto-loaded apidoc/v5.40.json for Perl 5.40"

# 開発版（Perl 5.41 環境）の場合 → エラー
cargo run -- --auto samples/wrapper.h
# → Error: Perl 5.41 is a development version.
#    Please specify --apidoc explicitly (e.g., --apidoc path/to/embed.fnc)

# 対応 JSON がない安定版（Perl 5.44 環境で v5.44.json がない）→ エラー
cargo run -- --auto samples/wrapper.h
# → Error: apidoc/v5.44.json not found for Perl 5.44.
#    Please specify --apidoc explicitly or add the JSON file.
```

## 追加考慮事項

### 開発時 vs インストール時

開発時は `cargo run` で実行するため、実行ファイルは `target/debug/` にある。
`apidoc/` はプロジェクトルートにあるため、相対パスで探す必要がある。

インストール後は、実行ファイルと同じディレクトリに `apidoc/` を配置する想定。

### 環境変数でのオーバーライド

将来的には `LIBPERL_MACROGEN_APIDOC_DIR` 環境変数でオーバーライド可能にする
（本計画では実装しない）。
