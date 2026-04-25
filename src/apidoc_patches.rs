//! Apidoc patches: data-driven corrections for known perl source bugs
//!
//! perl の C ヘッダや apidoc コメントには稀に誤りがある。例えば:
//!
//! - `cop.h` の `RCPV_LEN` `=for apidoc Am|RCPV *|RCPV_LEN|char *pv` は
//!   戻り値型を `RCPV *` と謳っているが、実際の本体 `(RCPVx(pv)->len-1)` は
//!   `STRLEN` を返す
//! - `op.h` の `Perl_custom_op_xop(x)` マクロは `Perl_custom_op_get_field(x, ...)`
//!   と展開されるが、`aTHX_` を渡し忘れているため Rust では引数数不一致 (E0061)
//!
//! 上流が修正されるまで、本モジュールは外部 JSON ファイル
//! (`apidoc/v$ver.patches.json`) から訂正情報を読み込み、apidoc データの
//! `return_type` 上書きや codegen 抑制を行う。
//!
//! ## ファイル形式
//!
//! ```json
//! {
//!     "schema_version": 1,
//!     "comment": "free-form comment",
//!     "patches": [
//!         {
//!             "name": "RCPV_LEN",
//!             "kind": "return_type_override",
//!             "value": "STRLEN",
//!             "source_loc": "/usr/lib64/perl5/CORE/cop.h:560",
//!             "reason": "apidoc claims `RCPV *` but body returns len-1 (STRLEN)",
//!             "upstream_status": "to-report"
//!         },
//!         {
//!             "name": "Perl_custom_op_xop",
//!             "kind": "skip_codegen",
//!             "source_loc": "/usr/lib64/perl5/CORE/op.h:977",
//!             "reason": "macro lacks aTHX_; would generate 2-arg call to 3-arg fn",
//!             "upstream_status": "to-report"
//!         }
//!     ]
//! }
//! ```
//!
//! ## 設計上の選択
//!
//! - **バージョン別**: `vX.Y.patches.json` で perl バージョンに紐付ける
//!   （上流で修正されたら該当バージョンのファイルから消すだけで撤去可能）
//! - **メイン apidoc JSON とは分離**: pre-generated `vX.Y.json` は perl-extract
//!   等で再生成される可能性があり、手動編集は失われる。patches は手動メンテ用
//! - **適用タイミング**:
//!   - `return_type_override` / `arg_type_override`: apidoc load + inline merge 後
//!   - `skip_codegen`: マクロ codegen 入口で early-return
//!
//! ## 既知の限界
//!
//! `kind` は初版で `return_type_override` と `skip_codegen` の 2 種のみ対応。
//! 必要に応じて `arg_type_override`、`param_type_override`、`inject_thx_to_call`
//! 等を追加する。

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::apidoc::ApidocDict;

/// `LIBPERL_MACROGEN_DEBUG_APIDOC=1` でデバッグ出力を有効化。
pub(crate) fn is_apidoc_debug_enabled() -> bool {
    std::env::var("LIBPERL_MACROGEN_DEBUG_APIDOC")
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false)
}

/// build script 経由で呼ばれた場合 `cargo:warning=` で CI ログに可視化する。
/// CLI 直接実行（cargo run など）の場合は stderr にも複製し、両方の経路で
/// 確認できるようにする。
pub(crate) fn cargo_warning(msg: &str) {
    println!("cargo:warning={}", msg);
    eprintln!("{}", msg);
}

/// Patch ファイル全体
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ApidocPatchFile {
    /// スキーマバージョン（互換性管理）
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    /// 自由記述コメント
    #[serde(default)]
    pub comment: Option<String>,
    /// パッチ列
    #[serde(default)]
    pub patches: Vec<ApidocPatch>,
}

fn default_schema_version() -> u32 { 1 }

/// 1 つのパッチエントリ
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApidocPatch {
    /// 対象 macro/function 名
    pub name: String,
    /// パッチ種別
    pub kind: PatchKind,
    /// `*_override` 系で必須の値
    #[serde(default)]
    pub value: Option<String>,
    /// `arg_type_override` 用の引数 index
    #[serde(default)]
    pub arg_index: Option<usize>,
    /// バグ箇所（デバッグ・上流報告用、`/path/to/file.h:line`）
    #[serde(default)]
    pub source_loc: Option<String>,
    /// 何が間違っているか・なぜパッチが必要かの説明（必須）
    pub reason: String,
    /// 上流ステータス: "to-report" / "reported:URL" / "merged" / "fixed-in-5.42" 等
    #[serde(default)]
    pub upstream_status: Option<String>,
}

/// パッチ種別
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PatchKind {
    /// apidoc entry の `return_type` を上書き
    #[serde(rename = "return_type_override")]
    ReturnTypeOverride,
    /// apidoc entry の `args[arg_index].ty` を上書き（将来用）
    #[serde(rename = "arg_type_override")]
    ArgTypeOverride,
    /// codegen 段階でこのマクロ/inline fn の生成を抑制し、
    /// `[CODEGEN_SUPPRESSED]` コメントに置換
    #[serde(rename = "skip_codegen")]
    SkipCodegen,
    /// 上位レイヤ（典型的には `common.patches.json`）で登録されているパッチを
    /// 当該バージョンでだけ無効化する。`v$X.$Y.patches.json` で「上流が修正
    /// された」ケースに使う。`value` などのフィールドは無視される。
    #[serde(rename = "remove")]
    Remove,
}

/// ロード後の正規化された patch 集合（高速ルックアップ用）
#[derive(Debug, Default)]
pub struct ApidocPatchSet {
    /// macro/fn 名 → (新 return_type, reason)
    pub return_overrides: HashMap<String, (String, String)>,
    /// macro/fn 名 → (arg_index, 新 ty, reason)
    pub arg_overrides: HashMap<String, Vec<(usize, String, String)>>,
    /// macro/fn 名 → reason（codegen 抑制対象）
    pub skip_codegen: HashMap<String, String>,
    /// `kind: "remove"` で名指しされた、上位レイヤから取り除くべき名前。
    /// 単一ファイル `load_json` 単独では実体に影響しないが、
    /// `load_for_apidoc_path` の 2 段マージで version-specific から common
    /// レイヤを打ち消すために使う。
    pub removals: HashSet<String>,
    /// ロードしたパッチファイルのパス（デバッグ用、ロード順）。
    /// 2 段マージのときは `[common.patches.json, v$X.$Y.patches.json]`。
    pub source_paths: Vec<PathBuf>,
}

impl ApidocPatchSet {
    pub fn empty() -> Self { Self::default() }

    /// JSON ファイルから読み込み
    pub fn load_json<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let path_ref = path.as_ref();
        let content = std::fs::read_to_string(path_ref)?;
        let file: ApidocPatchFile = serde_json::from_str(&content).map_err(|e| {
            io::Error::new(io::ErrorKind::InvalidData,
                format!("apidoc patches JSON parse error: {}", e))
        })?;
        if file.schema_version != 1 {
            return Err(io::Error::new(io::ErrorKind::InvalidData,
                format!("unsupported apidoc patches schema_version: {}", file.schema_version)));
        }
        let mut set = Self::default();
        set.source_paths.push(path_ref.to_path_buf());
        for p in file.patches {
            match p.kind {
                PatchKind::ReturnTypeOverride => {
                    let v = p.value.clone().ok_or_else(|| io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("patch for {}: return_type_override requires `value`", p.name)))?;
                    set.return_overrides.insert(p.name, (v, p.reason));
                }
                PatchKind::ArgTypeOverride => {
                    let v = p.value.clone().ok_or_else(|| io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("patch for {}: arg_type_override requires `value`", p.name)))?;
                    let idx = p.arg_index.ok_or_else(|| io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("patch for {}: arg_type_override requires `arg_index`", p.name)))?;
                    set.arg_overrides.entry(p.name).or_default().push((idx, v, p.reason));
                }
                PatchKind::SkipCodegen => {
                    set.skip_codegen.insert(p.name, p.reason);
                }
                PatchKind::Remove => {
                    // 単独 load では実体には影響しない（removals に記録するだけ）。
                    // 2 段マージ時に上位レイヤから打ち消すために使われる。
                    set.removals.insert(p.name);
                }
            }
        }
        Ok(set)
    }

    /// `apidoc/v$major.$minor.patches.json` を解決して読み込み
    /// ファイルが存在しない場合は空の patch set を返す（エラーにしない）
    pub fn load_for_perl_version<P: AsRef<Path>>(
        apidoc_dir: P, major: u32, minor: u32,
    ) -> io::Result<Self> {
        let filename = format!("v{}.{}.patches.json", major, minor);
        let path = apidoc_dir.as_ref().join(&filename);
        if !path.exists() {
            return Ok(Self::empty());
        }
        Self::load_json(&path)
    }

    /// **2 段マージ版ローダ**: `apidoc_path` (`<dir>/v$X.$Y.json`) と同じ
    /// ディレクトリにある以下の 2 ファイルを優先順位付きで読み込む:
    ///
    /// 1. **`<dir>/common.patches.json`** — 全バージョン共通のパッチ
    /// 2. **`<dir>/v$X.$Y.patches.json`** — 当該バージョン固有のパッチ
    ///
    /// マージ規則:
    /// - 同一 `name` のエントリは **後者（version-specific）が前者（common）を上書き**
    /// - version-specific 側の `kind: "remove"` は common 側の同名エントリを **削除**
    ///   （上流で fix されたバージョンでパッチを撤去する用途）
    ///
    /// 両ファイルとも存在しない場合は空 set を返す（エラーにしない）。
    pub fn load_for_apidoc_path<P: AsRef<Path>>(apidoc_path: P) -> io::Result<Self> {
        let path_ref = apidoc_path.as_ref();
        let dir = path_ref.parent().unwrap_or_else(|| Path::new("."));
        let debug = is_apidoc_debug_enabled();

        let mut set = Self::default();

        if debug {
            cargo_warning(&format!(
                "[apidoc-patches] load_for_apidoc_path: apidoc_path={}, dir={}",
                path_ref.display(), dir.display()
            ));
        }

        // 1. common.patches.json（あれば）
        let common_path = dir.join("common.patches.json");
        if common_path.exists() {
            let common = Self::load_json(&common_path)?;
            if debug {
                cargo_warning(&format!(
                    "[apidoc-patches] loaded common.patches.json: \
                     {} return_overrides, {} arg_overrides, {} skip_codegen, {} removals",
                    common.return_overrides.len(),
                    common.arg_overrides.len(),
                    common.skip_codegen.len(),
                    common.removals.len(),
                ));
            }
            set.merge_overlay(common);
        } else if debug {
            cargo_warning(&format!(
                "[apidoc-patches] common.patches.json NOT FOUND at {}",
                common_path.display()
            ));
        }

        // 2. v$X.$Y.patches.json（あれば）
        let version_path = {
            let stem = path_ref.file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(String::new);
            path_ref.with_file_name(format!("{}.patches.json", stem))
        };
        if version_path.exists() {
            let version = Self::load_json(&version_path)?;
            if debug {
                cargo_warning(&format!(
                    "[apidoc-patches] loaded {}: \
                     {} return_overrides, {} skip_codegen, {} removals",
                    version_path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
                    version.return_overrides.len(),
                    version.skip_codegen.len(),
                    version.removals.len(),
                ));
            }
            // 先に version 側の removals で common を打ち消す
            for name in &version.removals {
                set.return_overrides.remove(name);
                set.arg_overrides.remove(name);
                set.skip_codegen.remove(name);
            }
            // それから version 側のパッチを上書きマージ
            set.merge_overlay(version);
        } else if debug {
            cargo_warning(&format!(
                "[apidoc-patches] {} NOT FOUND",
                version_path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
            ));
        }

        Ok(set)
    }

    /// 別の patch set を **後勝ち** で重ね合わせる。
    /// 同名の override は上書き、`source_paths` は追記、`removals` は和集合。
    fn merge_overlay(&mut self, other: ApidocPatchSet) {
        for (k, v) in other.return_overrides {
            self.return_overrides.insert(k, v);
        }
        for (k, v) in other.arg_overrides {
            // arg_overrides は配列。同名で上書きするときは置換（追加ではない）。
            self.arg_overrides.insert(k, v);
        }
        for (k, v) in other.skip_codegen {
            self.skip_codegen.insert(k, v);
        }
        for name in other.removals {
            self.removals.insert(name);
        }
        self.source_paths.extend(other.source_paths);
    }

    /// テキスト形式の skip-list ファイルを読み込んで
    /// skip_codegen に名前を追加する。
    ///
    /// フォーマット:
    /// - 1 行に 1 つの関数名（マクロまたは inline 関数）
    /// - `#` で始まる行は comment として無視
    /// - 前後の空白はトリム、空行は無視
    ///
    /// 同名が既に存在する場合は **既存を優先**（JSON patches で設定済みなど）。
    /// reason は `"skip-list: <filename>"` を埋め込む。
    pub fn merge_skip_list<P: AsRef<Path>>(&mut self, path: P) -> io::Result<usize> {
        let path_ref = path.as_ref();
        let content = std::fs::read_to_string(path_ref)?;
        let display_name = path_ref.file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| path_ref.display().to_string());
        let reason = format!("skip-list: {}", display_name);
        let mut added = 0usize;
        for raw_line in content.lines() {
            let line = raw_line.split('#').next().unwrap_or("").trim();
            if line.is_empty() { continue; }
            // 既存（JSON patches 等）を優先、同名は上書きしない
            if !self.skip_codegen.contains_key(line) {
                self.skip_codegen.insert(line.to_string(), reason.clone());
                added += 1;
            }
        }
        Ok(added)
    }

    /// パッチが空（適用するものが無い）か
    pub fn is_empty(&self) -> bool {
        self.return_overrides.is_empty()
            && self.arg_overrides.is_empty()
            && self.skip_codegen.is_empty()
    }

    /// パッチ件数
    pub fn count(&self) -> usize {
        self.return_overrides.len()
            + self.arg_overrides.iter().map(|(_, v)| v.len()).sum::<usize>()
            + self.skip_codegen.len()
    }

    /// `return_type_override` と `arg_type_override` を `ApidocDict` に適用
    /// 適用された entry 名のリストを返す。対象が dict に存在しない場合は warning
    /// として stderr に出力（perl 側で fix された等の状況検知用）。
    ///
    /// **デバッグ出力**: 環境変数 `LIBPERL_MACROGEN_DEBUG_APIDOC=1` を設定すると、
    /// パッチ適用の hit/miss、適用前後の戻り値型、dict 全体の RCPV 関連エントリ等を
    /// `cargo:warning=` 経由で出力する（build script 経由で呼ばれた場合は CI ログに
    /// 可視化される）。CI で patch が一部バージョンで効かない問題の調査用。
    /// **MISS は環境変数なしでも常に `cargo:warning=` として出力する**（黙って
    /// 取りこぼされる事故を防ぐため）。
    pub fn apply_to_apidoc(&self, dict: &mut ApidocDict) -> Vec<String> {
        let debug = is_apidoc_debug_enabled();
        let mut applied: Vec<String> = Vec::new();

        if debug {
            cargo_warning(&format!(
                "[apidoc-patches] apply_to_apidoc: dict has {} entries; \
                 patches: {} return_overrides, {} arg_overrides, {} skip_codegen",
                dict.len(),
                self.return_overrides.len(),
                self.arg_overrides.len(),
                self.skip_codegen.len(),
            ));
        }

        for (name, (new_ty, _reason)) in &self.return_overrides {
            if let Some(entry) = dict.get_mut(name) {
                let old = entry.return_type.clone();
                entry.return_type = Some(new_ty.clone());
                applied.push(name.clone());
                if debug {
                    cargo_warning(&format!(
                        "[apidoc-patches] return_type_override APPLIED `{}`: {} -> {}",
                        name,
                        old.as_deref().unwrap_or("(none)"),
                        new_ty,
                    ));
                }
            } else {
                // MISS は env var 不要で常に可視化（黙って取りこぼされるのを防ぐ）
                cargo_warning(&format!(
                    "[apidoc-patches] return_type_override MISS `{}`: \
                     target not found in apidoc dict (dict has {} entries) — \
                     codegen falls back to whatever else is inferred",
                    name, dict.len()
                ));
            }
        }
        for (name, list) in &self.arg_overrides {
            if let Some(entry) = dict.get_mut(name) {
                for (idx, new_ty, _reason) in list {
                    if let Some(arg) = entry.args.get_mut(*idx) {
                        arg.ty = new_ty.clone();
                    } else {
                        cargo_warning(&format!(
                            "[apidoc-patches] arg_type_override `{}` arg_index {} \
                             out of range (entry has {} args)",
                            name, idx, entry.args.len()
                        ));
                    }
                }
                applied.push(name.clone());
            } else {
                cargo_warning(&format!(
                    "[apidoc-patches] arg_type_override MISS `{}`: \
                     target not found in apidoc dict",
                    name
                ));
            }
        }

        // デバッグ時のみ、dict 内の patch 関連エントリ群を dump
        // （inline merge が `=for apidoc` を拾えているかの判別用）
        if debug {
            let interest_prefixes: Vec<&str> = self.return_overrides.keys()
                .chain(self.skip_codegen.keys())
                .map(|s| s.as_str())
                .collect();
            let mut prefixes_set: std::collections::HashSet<&str> = std::collections::HashSet::new();
            for n in &interest_prefixes {
                // 共通プレフィックスを抽出（例: "RCPV_"）。簡易的に "_" までの先頭部。
                if let Some(idx) = n.find('_') {
                    prefixes_set.insert(&n[..idx + 1]);
                }
            }
            for prefix in prefixes_set {
                let matches: Vec<String> = dict.iter()
                    .filter(|(name, _)| name.starts_with(prefix))
                    .map(|(name, e)| format!("{}->{}", name, e.return_type.as_deref().unwrap_or("?")))
                    .collect();
                cargo_warning(&format!(
                    "[apidoc-patches] dict entries with prefix `{}` ({} entries): {:?}",
                    prefix, matches.len(), matches
                ));
            }
        }

        applied
    }

    /// codegen 抑制対象なら reason を返す
    pub fn skip_reason(&self, name: &str) -> Option<&str> {
        self.skip_codegen.get(name).map(|s| s.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use tempfile::TempDir;

    fn write_json(dir: &Path, name: &str, content: &str) -> PathBuf {
        let path = dir.join(name);
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        path
    }

    const COMMON_PATCH: &str = r#"{
        "schema_version": 1,
        "patches": [
            { "name": "RCPV_LEN", "kind": "return_type_override",
              "value": "STRLEN", "reason": "common: wrong apidoc" },
            { "name": "Perl_custom_op_xop", "kind": "skip_codegen",
              "reason": "common: macro lacks aTHX_" }
        ]
    }"#;

    #[test]
    fn test_load_common_only() {
        let tmp = TempDir::new().unwrap();
        write_json(tmp.path(), "common.patches.json", COMMON_PATCH);
        let apidoc_path = tmp.path().join("v5.40.json");
        // v5.40.json 自体は存在しなくても OK（patches 解決はパスから派生するだけ）

        let set = ApidocPatchSet::load_for_apidoc_path(&apidoc_path).unwrap();
        assert_eq!(set.return_overrides.len(), 1);
        assert_eq!(set.return_overrides["RCPV_LEN"].0, "STRLEN");
        assert_eq!(set.skip_codegen.len(), 1);
        assert!(set.skip_codegen.contains_key("Perl_custom_op_xop"));
        assert_eq!(set.source_paths.len(), 1);
    }

    #[test]
    fn test_version_overrides_common() {
        let tmp = TempDir::new().unwrap();
        write_json(tmp.path(), "common.patches.json", COMMON_PATCH);
        // v5.42 で RCPV_LEN の戻り値型を別の値に上書き
        let version_json = r#"{
            "schema_version": 1,
            "patches": [
                { "name": "RCPV_LEN", "kind": "return_type_override",
                  "value": "Size_t", "reason": "v5.42: tweaked" }
            ]
        }"#;
        write_json(tmp.path(), "v5.42.patches.json", version_json);
        let apidoc_path = tmp.path().join("v5.42.json");

        let set = ApidocPatchSet::load_for_apidoc_path(&apidoc_path).unwrap();
        // RCPV_LEN は version-specific が勝つ
        assert_eq!(set.return_overrides["RCPV_LEN"].0, "Size_t");
        // common 由来の Perl_custom_op_xop はそのまま残る
        assert!(set.skip_codegen.contains_key("Perl_custom_op_xop"));
        // ロードしたファイル数は 2
        assert_eq!(set.source_paths.len(), 2);
    }

    #[test]
    fn test_remove_kind_drops_common_entry() {
        let tmp = TempDir::new().unwrap();
        write_json(tmp.path(), "common.patches.json", COMMON_PATCH);
        // v5.42 で Perl_custom_op_xop が修正されたとして打ち消す
        let version_json = r#"{
            "schema_version": 1,
            "patches": [
                { "name": "Perl_custom_op_xop", "kind": "remove",
                  "reason": "fixed upstream in 5.42" }
            ]
        }"#;
        write_json(tmp.path(), "v5.42.patches.json", version_json);
        let apidoc_path = tmp.path().join("v5.42.json");

        let set = ApidocPatchSet::load_for_apidoc_path(&apidoc_path).unwrap();
        // Perl_custom_op_xop は removed
        assert!(!set.skip_codegen.contains_key("Perl_custom_op_xop"));
        // RCPV_LEN は common 由来でそのまま残る
        assert!(set.return_overrides.contains_key("RCPV_LEN"));
        // removals フィールドにも記録されている
        assert!(set.removals.contains("Perl_custom_op_xop"));
    }

    #[test]
    fn test_no_patches_files_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let apidoc_path = tmp.path().join("v5.40.json");
        let set = ApidocPatchSet::load_for_apidoc_path(&apidoc_path).unwrap();
        assert!(set.is_empty());
        assert_eq!(set.source_paths.len(), 0);
    }

    #[test]
    fn test_remove_only_in_singlefile_load_does_not_panic() {
        // 単独 load_json で kind: "remove" を読んでも実体には影響しない
        // （removals に記録されるだけ、override 系には触らない）
        let tmp = TempDir::new().unwrap();
        let json = r#"{
            "schema_version": 1,
            "patches": [
                { "name": "FOO", "kind": "remove", "reason": "test" }
            ]
        }"#;
        let path = write_json(tmp.path(), "v5.42.patches.json", json);
        let set = ApidocPatchSet::load_json(&path).unwrap();
        assert!(set.return_overrides.is_empty());
        assert!(set.skip_codegen.is_empty());
        assert!(set.removals.contains("FOO"));
    }
}
