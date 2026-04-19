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

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::apidoc::ApidocDict;

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
    /// ロードしたパッチファイルのパス（デバッグ用）
    pub source_path: Option<PathBuf>,
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
        set.source_path = Some(path_ref.to_path_buf());
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
    pub fn apply_to_apidoc(&self, dict: &mut ApidocDict) -> Vec<String> {
        let mut applied: Vec<String> = Vec::new();
        for (name, (new_ty, _reason)) in &self.return_overrides {
            if let Some(entry) = dict.get_mut(name) {
                entry.return_type = Some(new_ty.clone());
                applied.push(name.clone());
            } else {
                eprintln!(
                    "warning: apidoc patch target `{}` not found in apidoc — \
                     may have been fixed upstream; consider removing the patch",
                    name
                );
            }
        }
        for (name, list) in &self.arg_overrides {
            if let Some(entry) = dict.get_mut(name) {
                for (idx, new_ty, _reason) in list {
                    if let Some(arg) = entry.args.get_mut(*idx) {
                        arg.ty = new_ty.clone();
                    } else {
                        eprintln!(
                            "warning: apidoc patch target `{}` arg_index {} out of range \
                             (entry has {} args)",
                            name, idx, entry.args.len()
                        );
                    }
                }
                applied.push(name.clone());
            } else {
                eprintln!(
                    "warning: apidoc patch target `{}` not found in apidoc \
                     (arg_type_override)",
                    name
                );
            }
        }
        applied
    }

    /// codegen 抑制対象なら reason を返す
    pub fn skip_reason(&self, name: &str) -> Option<&str> {
        self.skip_codegen.get(name).map(|s| s.as_str())
    }
}
