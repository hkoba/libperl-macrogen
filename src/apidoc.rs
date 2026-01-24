//! Perl Apidoc Format Parser
//!
//! Parses Perl's embed.fnc file and =for apidoc comments in header files.
//! These provide type information for Perl's internal API functions and macros.

use std::any::Any;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::perl_config::{get_perl_version, PerlConfigError};

use serde::{Deserialize, Serialize};

use crate::preprocessor::CommentCallback;
use crate::source::FileId;
use crate::token::Comment;

/// 引数のNULL許容性
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Nullability {
    /// NN - ポインタはNULLであってはならない
    NotNull,
    /// NULLOK - ポインタはNULLでも良い
    Nullable,
    /// 指定なし
    #[default]
    Unspecified,
}

/// パースされた引数
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApidocArg {
    /// NULL許容性 (NN/NULLOK)
    pub nullability: Nullability,
    /// 非ゼロ制約 (NZ)
    pub non_zero: bool,
    /// 型 (例: "SV *", "const char *")
    pub ty: String,
    /// 引数名 (例: "sv", "name")
    pub name: String,
    /// 生の引数文字列 (パース前)
    pub raw: String,
}

/// パースされたフラグ
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ApidocFlags {
    // 可視性
    pub api: bool,           // A - 公開API
    pub core_only: bool,     // C - コア専用
    pub ext_visible: bool,   // E - 拡張から見える
    pub exported: bool,      // X - 明示的にエクスポート
    pub not_exported: bool,  // e - エクスポートしない

    // 関数タイプ
    pub perl_prefix: bool,   // p - Perl_プレフィックス
    pub static_fn: bool,     // S - S_プレフィックス (static)
    pub static_perl: bool,   // s - Perl_プレフィックス (static)
    pub inline: bool,        // i - インライン
    pub force_inline: bool,  // I - 強制インライン
    pub is_macro: bool,      // m - マクロのみ
    pub custom_macro: bool,  // M - カスタムマクロ
    pub no_thread_ctx: bool, // T - スレッドコンテキストなし

    // ドキュメント
    pub documented: bool,    // d - ドキュメントあり
    pub hide_docs: bool,     // h - ドキュメント非表示
    pub no_usage: bool,      // U - 使用例なし

    // 属性
    pub allocates: bool,     // a - メモリ確保
    pub pure: bool,          // P - 純粋関数
    pub return_required: bool, // R - 戻り値必須
    pub no_return: bool,     // r - 返らない
    pub deprecated: bool,    // D - 非推奨
    pub compat: bool,        // b - バイナリ互換性

    // その他
    pub format_string: bool, // f - フォーマット文字列
    pub varargs_no_fmt: bool, // F - 可変引数だがフォーマットではない
    pub no_args: bool,       // n - 引数なし
    pub unorthodox: bool,    // u - 非標準
    pub experimental: bool,  // x - 実験的
    pub is_typedef: bool,    // y - typedef

    /// 生のフラグ文字列
    pub raw: String,
}

/// apidocエントリ（関数/マクロの定義）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApidocEntry {
    /// フラグ
    pub flags: ApidocFlags,
    /// 戻り値の型（なければNone）
    pub return_type: Option<String>,
    /// 関数/マクロ名
    pub name: String,
    /// 引数リスト
    pub args: Vec<ApidocArg>,
    /// ソースファイル（分かる場合）
    pub source_file: Option<String>,
    /// 行番号（分かる場合）
    pub line_number: Option<usize>,
    /// トークン合成を行うマクロか（引数型が `"name"` のような引用符付きの場合）
    #[serde(default)]
    pub has_token_pasting: bool,
}

/// apidoc辞書（名前でエントリを検索可能）
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ApidocDict {
    entries: HashMap<String, ApidocEntry>,
}

impl ApidocFlags {
    /// フラグ文字列をパース
    pub fn parse(flags: &str) -> Self {
        let mut result = Self {
            raw: flags.to_string(),
            ..Default::default()
        };

        for ch in flags.chars() {
            match ch {
                // 可視性
                'A' => result.api = true,
                'C' => result.core_only = true,
                'E' => result.ext_visible = true,
                'X' => result.exported = true,
                'e' => result.not_exported = true,

                // 関数タイプ
                'p' => result.perl_prefix = true,
                'S' => result.static_fn = true,
                's' => result.static_perl = true,
                'i' => result.inline = true,
                'I' => result.force_inline = true,
                'm' => result.is_macro = true,
                'M' => result.custom_macro = true,
                'T' => result.no_thread_ctx = true,

                // ドキュメント
                'd' => result.documented = true,
                'h' => result.hide_docs = true,
                'U' => result.no_usage = true,

                // 属性
                'a' => {
                    result.allocates = true;
                    result.return_required = true; // 'a' implies 'R'
                }
                'P' => {
                    result.pure = true;
                    result.return_required = true; // 'P' implies 'R'
                }
                'R' => result.return_required = true,
                'r' => result.no_return = true,
                'D' => result.deprecated = true,
                'b' => result.compat = true,

                // その他
                'f' => result.format_string = true,
                'F' => result.varargs_no_fmt = true,
                'n' => result.no_args = true,
                'u' => result.unorthodox = true,
                'x' => result.experimental = true,
                'y' => result.is_typedef = true,

                // 特殊文字（無視）
                'G' | 'N' | 'O' | 'o' | 'v' | 'W' | ';' | '#' | '?' => {}

                // 未知のフラグは無視
                _ => {}
            }
        }

        result
    }
}

impl ApidocArg {
    /// 引数文字列をパース (例: "NN SV *sv", "NULLOK const char *name")
    pub fn parse(arg: &str) -> Option<Self> {
        let raw = arg.to_string();
        let trimmed = arg.trim();

        if trimmed.is_empty() {
            return None;
        }

        let mut nullability = Nullability::Unspecified;
        let mut non_zero = false;
        let mut remaining = trimmed;

        // プレフィックスを処理
        loop {
            if remaining.starts_with("NN ") {
                nullability = Nullability::NotNull;
                remaining = remaining[3..].trim_start();
            } else if remaining.starts_with("NULLOK ") {
                nullability = Nullability::Nullable;
                remaining = remaining[7..].trim_start();
            } else if remaining.starts_with("NZ ") {
                non_zero = true;
                remaining = remaining[3..].trim_start();
            } else {
                break;
            }
        }

        // 型と名前を分離
        // C言語の引数は "type name" の形式
        // ポインタの場合は "type *name" や "type * name" もありうる
        let (ty, name) = Self::split_type_and_name(remaining);

        Some(Self {
            nullability,
            non_zero,
            ty,
            name,
            raw,
        })
    }

    /// 型と名前を分離
    fn split_type_and_name(s: &str) -> (String, String) {
        let s = s.trim();

        // 特殊なケース: "..." (可変引数)
        if s == "..." {
            return ("...".to_string(), String::new());
        }

        // 特殊なケース: 型のみ (type, cast, block, number, token, "string")
        // これらは名前がない
        if s == "type" || s == "cast" || s == "SP" || s == "block"
            || s == "number" || s == "token" || s.starts_with('"')
        {
            return (s.to_string(), String::new());
        }

        // 最後の識別子を名前として取り出す
        // 例: "const char * const name" -> ty="const char * const", name="name"
        // 例: "SV *sv" -> ty="SV *", name="sv"
        // 例: "int method" -> ty="int", name="method"

        // 末尾から識別子を探す
        let bytes = s.as_bytes();
        let mut name_end = bytes.len();
        let mut name_start;

        // 末尾の空白をスキップ
        while name_end > 0 && bytes[name_end - 1].is_ascii_whitespace() {
            name_end -= 1;
        }

        // 識別子を後ろから取得
        name_start = name_end;
        while name_start > 0 {
            let ch = bytes[name_start - 1];
            if ch.is_ascii_alphanumeric() || ch == b'_' {
                name_start -= 1;
            } else {
                break;
            }
        }

        if name_start == name_end {
            // 名前が見つからない場合、全体を型として扱う
            return (s.to_string(), String::new());
        }

        let name = &s[name_start..name_end];
        let ty = s[..name_start].trim_end();

        // 型がポインタで終わる場合（"SV *"）、末尾の空白は除去済み
        // ただし "const" だけで終わるようなケースを避ける

        // 型が空の場合や "const" "struct" などで終わる場合は
        // 名前を型として戻す（型名のみのケース）
        if ty.is_empty() {
            return (name.to_string(), String::new());
        }

        // 型が予約語のみの場合は名前を型とする
        let type_keywords = ["const", "struct", "union", "enum", "unsigned", "signed", "volatile"];
        let ty_lower = ty.to_lowercase();
        for kw in &type_keywords {
            if ty_lower == *kw {
                // "const name" のようなケースは "const name" を型とする
                return (s.to_string(), String::new());
            }
        }

        (ty.to_string(), name.to_string())
    }
}

impl ApidocEntry {
    /// 単一行をパース（データ行のみ、コメントはNone）
    /// 形式: flags|return_type|name|arg1|arg2|...|argN
    pub fn parse_line(line: &str) -> Option<Self> {
        let trimmed = line.trim();

        // コメント行はスキップ
        if trimmed.starts_with(": ") || trimmed == ":" || trimmed.is_empty() {
            return None;
        }

        Self::parse_fields(trimmed)
    }

    /// =for apidoc 行をパース
    /// 形式: =for apidoc name
    /// または: =for apidoc flags|return_type|name|arg1|...
    pub fn parse_apidoc_line(line: &str) -> Option<Self> {
        let trimmed = line.trim();

        // =for apidoc または =for apidoc_item で始まるか確認
        let rest = if let Some(rest) = trimmed.strip_prefix("=for apidoc_item") {
            rest.trim()
        } else if let Some(rest) = trimmed.strip_prefix("=for apidoc") {
            rest.trim()
        } else {
            return None;
        };

        if rest.is_empty() {
            return None;
        }

        // パイプを含む場合は完全形式
        if rest.contains('|') {
            Self::parse_fields(rest)
        } else {
            // 名前のみの場合
            Some(Self {
                flags: ApidocFlags::default(),
                return_type: None,
                name: rest.to_string(),
                args: Vec::new(),
                source_file: None,
                line_number: None,
                has_token_pasting: false,
            })
        }
    }

    /// フィールド形式をパース
    fn parse_fields(s: &str) -> Option<Self> {
        let fields: Vec<&str> = s.split('|').collect();

        if fields.len() < 3 {
            return None;
        }

        let flags = ApidocFlags::parse(fields[0].trim());
        let return_type = {
            let rt = fields[1].trim();
            if rt.is_empty() {
                None
            } else {
                Some(rt.to_string())
            }
        };
        let name = fields[2].trim().to_string();

        if name.is_empty() {
            return None;
        }

        let args: Vec<ApidocArg> = fields[3..]
            .iter()
            .filter_map(|arg| ApidocArg::parse(arg))
            .collect();

        // トークン合成マクロかどうかをチェック
        // 引数型が `"name"` のような引用符で囲まれた文字列の場合はトークン合成用
        let has_token_pasting = args.iter().any(|arg| {
            arg.ty.starts_with('"') && arg.ty.ends_with('"')
        });

        Some(Self {
            flags,
            return_type,
            name,
            args,
            source_file: None,
            line_number: None,
            has_token_pasting,
        })
    }

    /// この関数がAPI公開かどうか
    pub fn is_public_api(&self) -> bool {
        self.flags.api
    }

    /// この関数がマクロかどうか
    pub fn is_macro(&self) -> bool {
        self.flags.is_macro
    }

    /// この関数がインラインかどうか
    pub fn is_inline(&self) -> bool {
        self.flags.inline || self.flags.force_inline
    }

    /// 型パラメータキーワードかどうかを判定
    ///
    /// apidoc で `type` や `cast` は特殊な引数で、C の型名を表す。
    /// Rust では generic 型パラメータとして扱う。
    pub fn is_type_param_keyword(ty: &str) -> bool {
        ty == "type" || ty == "cast"
    }

    /// `type`/`cast` パラメータのインデックスを返す
    pub fn type_param_indices(&self) -> Vec<usize> {
        self.args
            .iter()
            .enumerate()
            .filter(|(_, arg)| Self::is_type_param_keyword(&arg.ty))
            .map(|(i, _)| i)
            .collect()
    }

    /// 戻り値型が `type` または `cast` かどうか
    pub fn returns_type_param(&self) -> bool {
        self.return_type
            .as_ref()
            .map_or(false, |t| Self::is_type_param_keyword(t))
    }

    /// ジェネリック関数として生成すべきか
    pub fn is_generic(&self) -> bool {
        self.returns_type_param() || !self.type_param_indices().is_empty()
    }
}

impl ApidocDict {
    /// 新しい辞書を作成
    pub fn new() -> Self {
        Self::default()
    }

    /// エントリを追加
    pub fn insert(&mut self, name: String, entry: ApidocEntry) {
        self.entries.insert(name, entry);
    }

    /// embed.fncファイルをパース
    pub fn parse_embed_fnc<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let content = fs::read_to_string(path)?;
        Ok(Self::parse_embed_fnc_str(&content))
    }

    /// 文字列からembed.fnc形式をパース
    pub fn parse_embed_fnc_str(content: &str) -> Self {
        let mut dict = Self::new();
        let mut continued_line = String::new();
        let mut line_number = 0usize;

        for line in content.lines() {
            line_number += 1;

            // 行継続の処理
            if line.ends_with('\\') {
                // 末尾のバックスラッシュを除去して追加
                continued_line.push_str(line.trim_end_matches('\\'));
                continued_line.push(' ');
                continue;
            }

            let full_line = if continued_line.is_empty() {
                line.to_string()
            } else {
                continued_line.push_str(line);
                let result = continued_line.clone();
                continued_line.clear();
                result
            };

            if let Some(mut entry) = ApidocEntry::parse_line(&full_line) {
                entry.line_number = Some(line_number);
                dict.entries.insert(entry.name.clone(), entry);
            }
        }

        dict
    }

    /// ヘッダーファイルから =for apidoc コメントを抽出
    pub fn parse_header_apidoc<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let content = fs::read_to_string(&path)?;
        let mut dict = Self::parse_header_apidoc_str(&content);

        // ソースファイル情報を設定
        let path_str = path.as_ref().to_string_lossy().to_string();
        for entry in dict.entries.values_mut() {
            entry.source_file = Some(path_str.clone());
        }

        Ok(dict)
    }

    /// 文字列からヘッダーのapidocコメントをパース
    pub fn parse_header_apidoc_str(content: &str) -> Self {
        let mut dict = Self::new();
        let mut line_number = 0usize;

        for line in content.lines() {
            line_number += 1;

            // =for apidoc を探す
            if let Some(idx) = line.find("=for apidoc") {
                let apidoc_part = &line[idx..];
                if let Some(mut entry) = ApidocEntry::parse_apidoc_line(apidoc_part) {
                    entry.line_number = Some(line_number);
                    dict.entries.insert(entry.name.clone(), entry);
                }
            }
        }

        dict
    }

    /// 別の辞書をマージ
    pub fn merge(&mut self, other: Self) {
        for (name, entry) in other.entries {
            self.entries.entry(name).or_insert(entry);
        }
    }

    /// エントリ数を取得
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 辞書が空かどうか
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 名前でエントリを検索
    pub fn get(&self, name: &str) -> Option<&ApidocEntry> {
        self.entries.get(name)
    }

    /// イテレータを取得
    pub fn iter(&self) -> impl Iterator<Item = (&String, &ApidocEntry)> {
        self.entries.iter()
    }

    /// 関数のみをイテレート（マクロを除く）
    pub fn functions(&self) -> impl Iterator<Item = (&String, &ApidocEntry)> {
        self.entries.iter().filter(|(_, e)| !e.is_macro())
    }

    /// マクロのみをイテレート
    pub fn macros(&self) -> impl Iterator<Item = (&String, &ApidocEntry)> {
        self.entries.iter().filter(|(_, e)| e.is_macro())
    }

    /// フィルタにマッチするエントリをダンプ（デバッグ用）
    ///
    /// filter が空文字列の場合は全エントリを出力。
    /// filter に文字列が指定された場合は、名前にその文字列を含むエントリのみ出力。
    pub fn dump_filtered(&self, filter: &str) {
        let mut names: Vec<_> = self.entries.keys().collect();
        names.sort();

        for name in names {
            // フィルタが空でない場合、名前にフィルタ文字列を含むかチェック
            if !filter.is_empty() && !name.contains(filter) {
                continue;
            }

            if let Some(entry) = self.entries.get(name) {
                eprintln!("{}:", name);
                eprintln!("  flags: {}", entry.flags.raw);
                if let Some(ref ret) = entry.return_type {
                    eprintln!("  return_type: {}", ret);
                } else {
                    eprintln!("  return_type: (none)");
                }
                eprintln!("  args:");
                for (i, arg) in entry.args.iter().enumerate() {
                    eprintln!("    [{}] {} {} ({:?}{})",
                        i,
                        arg.ty,
                        arg.name,
                        arg.nullability,
                        if arg.non_zero { ", NZ" } else { "" }
                    );
                }
                if let Some(ref src) = entry.source_file {
                    eprintln!("  source: {}:{}", src, entry.line_number.unwrap_or(0));
                }
                eprintln!();
            }
        }
    }

    /// 統計情報を出力
    pub fn stats(&self) -> ApidocStats {
        let mut stats = ApidocStats::default();

        for entry in self.entries.values() {
            if entry.is_macro() {
                stats.macro_count += 1;
            } else if entry.is_inline() {
                stats.inline_count += 1;
            } else {
                stats.function_count += 1;
            }

            if entry.is_public_api() {
                stats.api_count += 1;
            }
        }

        stats.total = self.entries.len();
        stats
    }

    /// JSONファイルに保存
    pub fn save_json<P: AsRef<Path>>(&self, path: P) -> io::Result<()> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        fs::write(path, json)
    }

    /// JSON文字列にシリアライズ
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// JSONファイルから読み込み
    pub fn load_json<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let content = fs::read_to_string(path)?;
        Self::from_json(&content)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    /// JSON文字列からデシリアライズ
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// ファイル拡張子に基づいて適切な形式で読み込み
    /// - .json -> JSON形式
    /// - それ以外 -> embed.fnc形式
    pub fn load_auto<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let path_ref = path.as_ref();
        if path_ref.extension().is_some_and(|ext| ext == "json") {
            Self::load_json(path_ref)
        } else {
            Self::parse_embed_fnc(path_ref)
        }
    }

    /// 指定バージョン用の JSON ファイルパスを検索
    ///
    /// apidoc/v{major}.{minor}.json が存在すれば Some(path)、なければ None
    /// フォールバックは行わない（完全一致のみ）
    pub fn find_json_for_version<P: AsRef<Path>>(
        apidoc_dir: P,
        major: u32,
        minor: u32,
    ) -> Option<std::path::PathBuf> {
        let filename = format!("v{}.{}.json", major, minor);
        let path = apidoc_dir.as_ref().join(&filename);
        if path.exists() {
            Some(path)
        } else {
            None
        }
    }

    /// Perl バージョンに基づいて apidoc を自動ロード
    ///
    /// apidoc_dir: apidoc/ ディレクトリのパス
    /// 成功時: 対応する JSON からロードした ApidocDict
    /// 失敗時: io::Error（ファイルが見つからない場合など）
    pub fn load_for_perl_version<P: AsRef<Path>>(
        apidoc_dir: P,
        major: u32,
        minor: u32,
    ) -> io::Result<Self> {
        let path = Self::find_json_for_version(&apidoc_dir, major, minor).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "{}/v{}.{}.json not found for Perl {}.{}.\n\
                     Please specify --apidoc explicitly or add the JSON file.",
                    apidoc_dir.as_ref().display(),
                    major,
                    minor,
                    major,
                    minor
                ),
            )
        })?;
        Self::load_json(&path)
    }
}

/// apidoc 解決エラー
#[derive(Debug)]
pub enum ApidocResolveError {
    /// 開発版 Perl（奇数マイナーバージョン）
    DevelopmentVersion { major: u32, minor: u32 },
    /// apidoc ディレクトリが見つからない
    DirectoryNotFound,
    /// 対応する JSON ファイルが見つからない
    JsonNotFound { path: PathBuf, major: u32, minor: u32 },
    /// Perl バージョン取得エラー
    VersionError(PerlConfigError),
}

impl std::fmt::Display for ApidocResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApidocResolveError::DevelopmentVersion { major, minor } => {
                write!(
                    f,
                    "Perl {}.{} is a development version.\n\
                     Please specify --apidoc explicitly (e.g., --apidoc path/to/embed.fnc)",
                    major, minor
                )
            }
            ApidocResolveError::DirectoryNotFound => {
                write!(
                    f,
                    "apidoc directory not found.\n\
                     Please specify --apidoc explicitly."
                )
            }
            ApidocResolveError::JsonNotFound { path, major, minor } => {
                write!(
                    f,
                    "{}/v{}.{}.json not found for Perl {}.{}.\n\
                     Please specify --apidoc explicitly or add the JSON file.",
                    path.display(),
                    major, minor, major, minor
                )
            }
            ApidocResolveError::VersionError(e) => {
                write!(f, "Failed to get Perl version: {}", e)
            }
        }
    }
}

impl std::error::Error for ApidocResolveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ApidocResolveError::VersionError(e) => Some(e),
            _ => None,
        }
    }
}

impl From<PerlConfigError> for ApidocResolveError {
    fn from(e: PerlConfigError) -> Self {
        ApidocResolveError::VersionError(e)
    }
}

/// apidoc ディレクトリを検索
///
/// 検索順序:
/// 1. 指定されたベースディレクトリの apidoc/（base_dir が Some の場合）
/// 2. 埋め込みデータのキャッシュディレクトリ（ライブラリ配布時）
/// 3. 実行ファイルと同じディレクトリの apidoc/
/// 4. 実行ファイルの親ディレクトリの apidoc/ (開発時: target/debug/../apidoc)
/// 5. カレントディレクトリの apidoc/
pub fn find_apidoc_dir_from(base_dir: Option<&Path>) -> Option<PathBuf> {
    // 1. 指定されたベースディレクトリ
    if let Some(base) = base_dir {
        let apidoc_dir = base.join("apidoc");
        if apidoc_dir.is_dir() {
            return Some(apidoc_dir);
        }
        // base_dir 自体が apidoc ディレクトリかもしれない
        if base.is_dir() && base.file_name().is_some_and(|n| n == "apidoc") {
            return Some(base.to_path_buf());
        }
    }

    // 2. 埋め込みデータのキャッシュディレクトリ
    if let Some(embedded_dir) = crate::apidoc_data::get_apidoc_dir() {
        if embedded_dir.is_dir() {
            return Some(embedded_dir);
        }
    }

    // 3. 実行ファイルと同じディレクトリ
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            let apidoc_dir = exe_dir.join("apidoc");
            if apidoc_dir.is_dir() {
                return Some(apidoc_dir);
            }

            // 4. 実行ファイルの親ディレクトリ (開発時: target/debug/../apidoc -> project/apidoc)
            if let Some(parent_dir) = exe_dir.parent() {
                let apidoc_dir = parent_dir.join("apidoc");
                if apidoc_dir.is_dir() {
                    return Some(apidoc_dir);
                }

                // target/debug の場合は 2階層上
                if let Some(grandparent_dir) = parent_dir.parent() {
                    let apidoc_dir = grandparent_dir.join("apidoc");
                    if apidoc_dir.is_dir() {
                        return Some(apidoc_dir);
                    }
                }
            }
        }
    }

    // 5. カレントディレクトリ
    if let Ok(cwd) = std::env::current_dir() {
        let apidoc_dir = cwd.join("apidoc");
        if apidoc_dir.is_dir() {
            return Some(apidoc_dir);
        }
    }

    None
}

/// apidoc ファイルのパスを解決
///
/// - explicit_path が Some なら: そのまま返す
/// - explicit_path が None で auto_mode なら: Perl バージョンから自動検索
/// - それ以外: None を返す
///
/// # Arguments
/// - `explicit_path`: 明示的に指定されたパス
/// - `auto_mode`: 自動モード（Perl バージョンから検索）
/// - `apidoc_dir`: 検索対象ディレクトリ（None なら find_apidoc_dir_from() で検索）
///
/// # Returns
/// - `Ok(Some(path))`: 解決されたパス
/// - `Ok(None)`: 自動モードでなく、明示的パスもない場合
/// - `Err(...)`: 自動モードで解決に失敗した場合
pub fn resolve_apidoc_path(
    explicit_path: Option<&Path>,
    auto_mode: bool,
    apidoc_dir: Option<&Path>,
) -> Result<Option<PathBuf>, ApidocResolveError> {
    // 明示的に指定されている場合
    if let Some(path) = explicit_path {
        return Ok(Some(path.to_path_buf()));
    }

    // 自動モードでない場合
    if !auto_mode {
        return Ok(None);
    }

    // 自動モード: Perl バージョンから検索
    let (major, minor) = get_perl_version()?;

    // 奇数マイナーバージョン（開発版）はエラー
    if minor % 2 == 1 {
        return Err(ApidocResolveError::DevelopmentVersion { major, minor });
    }

    // apidoc ディレクトリを検索
    let resolved_apidoc_dir = find_apidoc_dir_from(apidoc_dir)
        .ok_or(ApidocResolveError::DirectoryNotFound)?;

    // バージョンに対応する JSON ファイルを検索
    let json_path = ApidocDict::find_json_for_version(&resolved_apidoc_dir, major, minor)
        .ok_or_else(|| ApidocResolveError::JsonNotFound {
            path: resolved_apidoc_dir,
            major,
            minor,
        })?;

    Ok(Some(json_path))
}

/// 統計情報
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ApidocStats {
    pub total: usize,
    pub function_count: usize,
    pub macro_count: usize,
    pub inline_count: usize,
    pub api_count: usize,
}

/// コメントから apidoc を抽出するコレクター
///
/// Preprocessor の CommentCallback として登録し、
/// `=for apidoc` を含むコメントを見つけたら辞書に登録する。
pub struct ApidocCollector {
    entries: HashMap<String, ApidocEntry>,
}

impl ApidocCollector {
    /// 新しいコレクターを作成
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// 収集した apidoc を ApidocDict にマージ
    pub fn merge_into(self, dict: &mut ApidocDict) {
        for (name, entry) in self.entries {
            dict.insert(name, entry);
        }
    }

    /// 収集数を返す
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 空かどうか
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for ApidocCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl CommentCallback for ApidocCollector {
    fn on_comment(&mut self, comment: &Comment, _file_id: FileId, _is_target: bool) {
        // コメント内の各行を処理
        // （is_target チェックは呼び出し側で行われるため、ここでは常に処理）
        for line in comment.text.lines() {
            if let Some(entry) = ApidocEntry::parse_apidoc_line(line) {
                self.entries.insert(entry.name.clone(), entry);
            }
        }
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_flags() {
        let flags = ApidocFlags::parse("Adp");
        assert!(flags.api);
        assert!(flags.documented);
        assert!(flags.perl_prefix);
        assert!(!flags.is_macro);
    }

    #[test]
    fn test_parse_flags_macro() {
        let flags = ApidocFlags::parse("ARdm");
        assert!(flags.api);
        assert!(flags.return_required);
        assert!(flags.documented);
        assert!(flags.is_macro);
    }

    #[test]
    fn test_parse_flags_allocates_implies_r() {
        let flags = ApidocFlags::parse("a");
        assert!(flags.allocates);
        assert!(flags.return_required);
    }

    #[test]
    fn test_parse_arg_simple() {
        let arg = ApidocArg::parse("int method").unwrap();
        assert_eq!(arg.nullability, Nullability::Unspecified);
        assert!(!arg.non_zero);
        assert_eq!(arg.ty, "int");
        assert_eq!(arg.name, "method");
    }

    #[test]
    fn test_parse_arg_pointer() {
        let arg = ApidocArg::parse("SV *sv").unwrap();
        assert_eq!(arg.ty, "SV *");
        assert_eq!(arg.name, "sv");
    }

    #[test]
    fn test_parse_arg_not_null() {
        let arg = ApidocArg::parse("NN SV *sv").unwrap();
        assert_eq!(arg.nullability, Nullability::NotNull);
        assert_eq!(arg.ty, "SV *");
        assert_eq!(arg.name, "sv");
    }

    #[test]
    fn test_parse_arg_nullok() {
        let arg = ApidocArg::parse("NULLOK SV *sv").unwrap();
        assert_eq!(arg.nullability, Nullability::Nullable);
        assert_eq!(arg.ty, "SV *");
        assert_eq!(arg.name, "sv");
    }

    #[test]
    fn test_parse_arg_const_pointer() {
        let arg = ApidocArg::parse("NN const char * const name").unwrap();
        assert_eq!(arg.nullability, Nullability::NotNull);
        assert_eq!(arg.ty, "const char * const");
        assert_eq!(arg.name, "name");
    }

    #[test]
    fn test_parse_arg_varargs() {
        let arg = ApidocArg::parse("...").unwrap();
        assert_eq!(arg.ty, "...");
        assert_eq!(arg.name, "");
    }

    #[test]
    fn test_parse_line_simple() {
        let entry = ApidocEntry::parse_line("Adp	|SV *	|av_pop 	|NN AV *av").unwrap();
        assert!(entry.flags.api);
        assert!(entry.flags.documented);
        assert!(entry.flags.perl_prefix);
        assert_eq!(entry.return_type, Some("SV *".to_string()));
        assert_eq!(entry.name, "av_pop");
        assert_eq!(entry.args.len(), 1);
        assert_eq!(entry.args[0].ty, "AV *");
        assert_eq!(entry.args[0].name, "av");
        assert_eq!(entry.args[0].nullability, Nullability::NotNull);
    }

    #[test]
    fn test_parse_line_comment() {
        assert!(ApidocEntry::parse_line(": This is a comment").is_none());
        assert!(ApidocEntry::parse_line(":").is_none());
        assert!(ApidocEntry::parse_line("").is_none());
    }

    #[test]
    fn test_parse_line_macro() {
        let entry = ApidocEntry::parse_line("ARdm	|SSize_t|av_tindex	|NN AV *av").unwrap();
        assert!(entry.flags.is_macro);
        assert!(entry.flags.return_required);
        assert_eq!(entry.name, "av_tindex");
    }

    #[test]
    fn test_parse_line_multiple_args() {
        let entry = ApidocEntry::parse_line(
            "Adp	|SV *	|amagic_call	|NN SV *left	|NN SV *right	|int method	|int dir"
        ).unwrap();
        assert_eq!(entry.args.len(), 4);
        assert_eq!(entry.args[0].name, "left");
        assert_eq!(entry.args[1].name, "right");
        assert_eq!(entry.args[2].name, "method");
        assert_eq!(entry.args[3].name, "dir");
    }

    #[test]
    fn test_parse_apidoc_line_name_only() {
        let entry = ApidocEntry::parse_apidoc_line("=for apidoc av_pop").unwrap();
        assert_eq!(entry.name, "av_pop");
        assert!(entry.return_type.is_none());
        assert!(entry.args.is_empty());
    }

    #[test]
    fn test_parse_apidoc_line_full() {
        let entry = ApidocEntry::parse_apidoc_line(
            "=for apidoc Am|char*|SvPV|SV* sv|STRLEN len"
        ).unwrap();
        assert!(entry.flags.api);
        assert!(entry.flags.is_macro);
        assert_eq!(entry.return_type, Some("char*".to_string()));
        assert_eq!(entry.name, "SvPV");
        assert_eq!(entry.args.len(), 2);
    }

    #[test]
    fn test_parse_apidoc_item() {
        let entry = ApidocEntry::parse_apidoc_line(
            "=for apidoc_item |const char*|SvPV_const|SV* sv|STRLEN len"
        ).unwrap();
        assert_eq!(entry.return_type, Some("const char*".to_string()));
        assert_eq!(entry.name, "SvPV_const");
    }

    #[test]
    fn test_embed_fnc_str() {
        let content = r#"
: This is a comment
Adp	|SV *	|av_pop 	|NN AV *av
ARdm	|SSize_t|av_tindex	|NN AV *av
"#;
        let dict = ApidocDict::parse_embed_fnc_str(content);
        assert_eq!(dict.len(), 2);
        assert!(dict.get("av_pop").is_some());
        assert!(dict.get("av_tindex").is_some());
    }

    #[test]
    fn test_embed_fnc_continuation() {
        let content = r#"
pr	|void	|abort_execution|NULLOK SV *msg_sv			\
				|NN const char * const name
"#;
        let dict = ApidocDict::parse_embed_fnc_str(content);
        assert_eq!(dict.len(), 1);
        let entry = dict.get("abort_execution").unwrap();
        assert_eq!(entry.args.len(), 2);
        assert_eq!(entry.args[0].nullability, Nullability::Nullable);
        assert_eq!(entry.args[1].nullability, Nullability::NotNull);
    }

    #[test]
    fn test_header_apidoc_str() {
        let content = r#"
/*
=for apidoc Am|char*|SvPV|SV* sv|STRLEN len

Returns a pointer to the string value of the SV.

=cut
*/
"#;
        let dict = ApidocDict::parse_header_apidoc_str(content);
        assert_eq!(dict.len(), 1);
        assert!(dict.get("SvPV").is_some());
    }

    #[test]
    fn test_dict_stats() {
        let content = r#"
Adp	|SV *	|av_pop 	|NN AV *av
ARdm	|SSize_t|av_tindex	|NN AV *av
ARdip	|Size_t |av_count	|NN AV *av
Cp	|void	|internal_fn	|int x
"#;
        let dict = ApidocDict::parse_embed_fnc_str(content);
        let stats = dict.stats();
        assert_eq!(stats.total, 4);
        assert_eq!(stats.macro_count, 1);
        assert_eq!(stats.inline_count, 1);
        assert_eq!(stats.function_count, 2);
        assert_eq!(stats.api_count, 3);
    }

    #[test]
    fn test_dict_merge() {
        let content1 = "Adp	|SV *	|av_pop 	|NN AV *av";
        let content2 = "ARdm	|SSize_t|av_tindex	|NN AV *av";

        let mut dict1 = ApidocDict::parse_embed_fnc_str(content1);
        let dict2 = ApidocDict::parse_embed_fnc_str(content2);

        dict1.merge(dict2);
        assert_eq!(dict1.len(), 2);
    }
}
