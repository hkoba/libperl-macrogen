//! マクロ型推論の高レベル API
//!
//! build.rs や外部ツールから型推論を実行するための API を提供する。

use std::collections::HashSet;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};

use crate::apidoc::{ApidocCollector, ApidocDict, ApidocResolveError};
use crate::ast::{ExternalDecl, TypeSpec};
use crate::enum_dict::EnumDict;
use crate::error::CompileError;
use crate::fields_dict::FieldsDict;
use crate::inline_fn::InlineFnDict;
use crate::intern::InternedStr;
use crate::macro_infer::{MacroInferContext, NoExpandSymbols};
use crate::parser::Parser;
use crate::perl_config::PerlConfigError;
use crate::preprocessor::{MacroCallWatcher, Preprocessor};
use crate::rust_decl::RustDeclDict;

/// typedef 辞書の型エイリアス
pub type TypedefDict = HashSet<InternedStr>;

/// 型推論エラー
#[derive(Debug)]
pub enum InferError {
    /// Perl 設定取得エラー
    PerlConfig(PerlConfigError),
    /// apidoc 解決エラー
    ApidocResolve(ApidocResolveError),
    /// プリプロセッサ/パースエラー
    Compile(CompileError),
    /// ファイル I/O エラー
    Io(std::io::Error),
}

impl std::fmt::Display for InferError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InferError::PerlConfig(e) => write!(f, "Perl config error: {}", e),
            InferError::ApidocResolve(e) => write!(f, "Apidoc resolve error: {}", e),
            InferError::Compile(e) => write!(f, "Compile error: {}", e),
            InferError::Io(e) => write!(f, "I/O error: {}", e),
        }
    }
}

impl std::error::Error for InferError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            InferError::PerlConfig(e) => Some(e),
            InferError::ApidocResolve(e) => Some(e),
            InferError::Compile(e) => Some(e),
            InferError::Io(e) => Some(e),
        }
    }
}

impl From<PerlConfigError> for InferError {
    fn from(e: PerlConfigError) -> Self {
        InferError::PerlConfig(e)
    }
}

impl From<ApidocResolveError> for InferError {
    fn from(e: ApidocResolveError) -> Self {
        InferError::ApidocResolve(e)
    }
}

impl From<CompileError> for InferError {
    fn from(e: CompileError) -> Self {
        InferError::Compile(e)
    }
}

impl From<std::io::Error> for InferError {
    fn from(e: std::io::Error) -> Self {
        InferError::Io(e)
    }
}

/// 型推論の設定
#[derive(Debug, Clone)]
pub struct InferConfig {
    /// 入力ファイル（wrapper.h など）
    pub input_file: PathBuf,
    /// apidoc ファイルのパス（省略時は自動検索）
    pub apidoc_path: Option<PathBuf>,
    /// Rust バインディングファイルのパス
    pub bindings_path: Option<PathBuf>,
    /// apidoc ディレクトリの検索パス（省略時は自動検索）
    pub apidoc_dir: Option<PathBuf>,
    /// デバッグ出力
    pub debug: bool,
}

impl InferConfig {
    /// 入力ファイルのみを指定した最小構成
    pub fn new(input_file: PathBuf) -> Self {
        Self {
            input_file,
            apidoc_path: None,
            bindings_path: None,
            apidoc_dir: None,
            debug: false,
        }
    }

    /// apidoc パスを設定
    pub fn with_apidoc(mut self, path: PathBuf) -> Self {
        self.apidoc_path = Some(path);
        self
    }

    /// bindings パスを設定
    pub fn with_bindings(mut self, path: PathBuf) -> Self {
        self.bindings_path = Some(path);
        self
    }

    /// apidoc ディレクトリを設定
    pub fn with_apidoc_dir(mut self, path: PathBuf) -> Self {
        self.apidoc_dir = Some(path);
        self
    }

    /// デバッグモードを設定
    pub fn with_debug(mut self, debug: bool) -> Self {
        self.debug = debug;
        self
    }
}

/// デバッグ出力オプション
///
/// パイプラインの特定の段階でデータ構造をダンプして早期終了するためのオプション。
#[derive(Debug, Clone, Default)]
pub struct DebugOptions {
    /// apidoc マージ後にダンプして終了
    /// Some(filter) でフィルタ指定（正規表現）、Some("") で全件出力
    pub dump_apidoc_after_merge: Option<String>,
}

impl DebugOptions {
    pub fn new() -> Self {
        Self::default()
    }

    /// apidoc マージ後にダンプして終了するオプションを設定
    pub fn dump_apidoc(mut self, filter: impl Into<String>) -> Self {
        self.dump_apidoc_after_merge = Some(filter.into());
        self
    }
}

/// 統計情報
#[derive(Debug, Clone, Default)]
pub struct InferStats {
    /// コメントから収集した apidoc 数
    pub apidoc_from_comments: usize,
    /// THX 依存マクロ数
    pub thx_dependent_count: usize,
}

/// 型推論の結果
pub struct InferResult {
    /// マクロ推論コンテキスト（全マクロの解析結果）
    pub infer_ctx: MacroInferContext,
    /// フィールド辞書
    pub fields_dict: FieldsDict,
    /// Enum 辞書
    pub enum_dict: EnumDict,
    /// インライン関数辞書
    pub inline_fn_dict: InlineFnDict,
    /// Apidoc 辞書
    pub apidoc: ApidocDict,
    /// Rust 宣言辞書
    pub rust_decl_dict: Option<RustDeclDict>,
    /// typedef 辞書
    pub typedefs: TypedefDict,
    /// プリプロセッサ（マクロテーブル、StringInterner、FileRegistry へのアクセス用）
    pub preprocessor: Preprocessor,
    /// 統計情報
    pub stats: InferStats,
}

/// 既存の Preprocessor を使ってマクロ型推論を実行
///
/// Preprocessor が既に初期化されている場合に使用。
/// 主に Pipeline の内部実装から呼び出される。
///
/// **Note**: 新しいコードでは `Pipeline` API の使用を推奨します。
/// この関数は Pipeline の内部実装で使用されており、直接呼び出す必要は
/// 通常ありません。
///
/// `debug_opts` が指定され、デバッグダンプで早期終了した場合は `Ok(None)` を返す。
pub fn run_inference_with_preprocessor(
    mut pp: Preprocessor,
    apidoc_path: Option<&Path>,
    bindings_path: Option<&Path>,
    debug_opts: Option<&DebugOptions>,
) -> Result<Option<InferResult>, InferError> {
    // RustDeclDict をロード（パーサー作成前に行い、展開抑制を設定）
    let rust_decl_dict = if let Some(path) = bindings_path {
        Some(RustDeclDict::parse_file(path)?)
    } else {
        None
    };

    // bindings.rs の定数名を展開抑制に登録
    if let Some(ref dict) = rust_decl_dict {
        for name in dict.consts.keys() {
            let interned = pp.interner_mut().intern(name);
            pp.add_skip_expand_macro(interned);
        }
    }

    // フィールド辞書を作成（パースしながら収集）
    let mut fields_dict = FieldsDict::new();

    // Enum 辞書を作成（パースしながら収集）
    let mut enum_dict = EnumDict::new();

    // ApidocCollector を Preprocessor に設定
    pp.set_comment_callback(Box::new(ApidocCollector::new()));

    // _SV_HEAD マクロ呼び出しを監視
    let sv_head_id = pp.interner_mut().intern("_SV_HEAD");
    pp.set_macro_called_callback(sv_head_id, Box::new(MacroCallWatcher::new()));

    // パーサー作成
    let mut parser = Parser::new(&mut pp)?;

    // inline 関数辞書を作成
    let mut inline_fn_dict = InlineFnDict::new();

    // parse_each_with_pp でフィールド辞書と inline 関数を収集
    // 同時に _SV_HEAD マクロ呼び出しを検出して SV ファミリーを動的に構築
    parser.parse_each_with_pp(|decl, _loc, _path, pp| {
        let interner = pp.interner();
        fields_dict.collect_from_external_decl(decl, decl.is_target(), interner);

        // enum 情報を収集
        enum_dict.collect_from_external_decl(decl, decl.is_target(), interner);

        // inline 関数を収集
        if decl.is_target() {
            if let ExternalDecl::FunctionDef(func_def) = decl {
                inline_fn_dict.collect_from_function_def(func_def, interner);
            }
        }

        // 構造体定義の場合、_SV_HEAD フラグをチェック
        if decl.is_target() {
            if let Some(struct_names) = extract_struct_names(decl) {
                // _SV_HEAD が呼ばれていたら SV ファミリーに追加
                if let Some(cb) = pp.get_macro_called_callback(sv_head_id) {
                    if let Some(watcher) = cb.as_any().downcast_ref::<MacroCallWatcher>() {
                        if watcher.take_called() {
                            // _SV_HEAD(typeName) の引数を取得
                            let type_name = watcher.last_args()
                                .and_then(|args| args.first().cloned())
                                .unwrap_or_default();

                            for name in struct_names {
                                // typeName → 構造体名マッピングも同時に登録
                                fields_dict.add_sv_family_member_with_type(name, &type_name);
                            }
                        }
                    }
                }
            }
        }
        ControlFlow::Continue(())
    })?;

    // パーサーから typedef 辞書を取得
    let typedefs = parser.typedefs().clone();

    // コールバックを取り出してダウンキャスト
    let callback = pp.take_comment_callback().expect("callback should exist");
    let apidoc_collector = callback
        .into_any()
        .downcast::<ApidocCollector>()
        .expect("callback type mismatch");

    // 一致型キャッシュを構築（全フィールドについて型の一貫性を事前計算）
    fields_dict.build_consistent_type_cache(pp.interner());

    // sv_u フィールド型は parse_each で動的に収集済み
    // （SV ファミリー構造体の sv_u union から自動検出）

    // Apidoc をロード（ファイルから + コメントから）
    let mut apidoc = if let Some(path) = apidoc_path {
        ApidocDict::load_auto(path)?
    } else {
        ApidocDict::new()
    };
    let apidoc_from_comments = apidoc_collector.len();
    apidoc_collector.merge_into(&mut apidoc);

    // デバッグ: apidoc マージ後にダンプして早期終了
    if let Some(opts) = debug_opts {
        if let Some(filter) = &opts.dump_apidoc_after_merge {
            apidoc.dump_filtered(filter);
            return Ok(None);
        }
    }

    // MacroInferContext を作成して解析
    let mut infer_ctx = MacroInferContext::new();

    // THX シンボルを事前に intern
    let sym_athx = pp.interner_mut().intern("aTHX");
    let sym_tthx = pp.interner_mut().intern("tTHX");
    let sym_my_perl = pp.interner_mut().intern("my_perl");
    let thx_symbols = (sym_athx, sym_tthx, sym_my_perl);

    // 展開を抑制するマクロシンボルを作成（assert, SvANY など）
    let no_expand = NoExpandSymbols::new(pp.interner_mut());

    {
        let interner = pp.interner();
        let files = pp.files();

        infer_ctx.analyze_all_macros(
            pp.macros(),
            interner,
            files,
            Some(&apidoc),
            Some(&fields_dict),
            rust_decl_dict.as_ref(),
            Some(&inline_fn_dict),
            &typedefs,
            thx_symbols,
            no_expand,
        );
    }

    // THX 依存マクロ数をカウント
    let thx_dependent_count = infer_ctx.macros.values()
        .filter(|info| info.is_target && info.is_thx_dependent)
        .count();

    let stats = InferStats {
        apidoc_from_comments,
        thx_dependent_count,
    };

    Ok(Some(InferResult {
        infer_ctx,
        fields_dict,
        enum_dict,
        inline_fn_dict,
        apidoc,
        rust_decl_dict,
        typedefs,
        preprocessor: pp,
        stats,
    }))
}

/// 宣言から構造体名を抽出
fn extract_struct_names(decl: &ExternalDecl) -> Option<Vec<InternedStr>> {
    let declaration = match decl {
        ExternalDecl::Declaration(d) => d,
        _ => return None,
    };

    let mut names = Vec::new();

    for type_spec in &declaration.specs.type_specs {
        match type_spec {
            TypeSpec::Struct(spec) | TypeSpec::Union(spec) => {
                // メンバーリストを持つ定義のみ（前方宣言は除外）
                if spec.members.is_some() {
                    if let Some(name) = spec.name {
                        names.push(name);
                    }
                }
            }
            _ => {}
        }
    }

    if names.is_empty() {
        None
    } else {
        Some(names)
    }
}
