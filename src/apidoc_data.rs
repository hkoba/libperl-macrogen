//! Embedded apidoc data management
//!
//! ビルド時にダウンロードされた apidoc.tar.gz をバイナリに埋め込み、
//! ランタイムで展開してキャッシュディレクトリに保存する。

use std::fs;
use std::io::{self, Cursor};
use std::path::PathBuf;
use std::sync::OnceLock;

use flate2::read::GzDecoder;
use tar::Archive;

/// apidoc データのバージョン（build.rs と一致させる）
/// 1.1: PAD_SET_CUR / PAD_SET_CUR_NOSAVE / PAD_BASE_SV の
///      arg_type_override 追加 (pad.h apidoc の `PADLIST padlist` ポインタ抜け)
/// 1.2: MUTABLE_* の arg_type_override (void *) と AvFILL の return override (SSize_t) 追加
/// 1.3: add_decl 追加 (MUTABLE_* 一族 / AvARRAY / AvFILLp — 5.34 未満のヘッダに
///      無い宣言を補い、旧 perl での Cv/Hv 一族 cascade 消滅を解消) と
///      AvFILLp の return override (5.32 の `int` 宣言を SSize_t に訂正)
/// 1.4: Pad*/Padlist*/Padnamelist* の arg_type_override 12 件
///      (<=5.30 の pad.h apidoc のポインタ `*` 抜け。5.32 で上流修正済み
///      = 新しい版では同値・無害)
/// 1.5: Xop*/Bhk*/CALL_BLOCK_HOOKS の `which` 引数に `token` 注釈 10 件
///      (<=5.36 の op.h apidoc は token 注釈を欠き、token-pasting マクロが
///      呼び出し扱いされて OP_CLASS 等が cascade 消滅。5.38 で上流修正済み)
/// 1.6: v5.32/v5.34.patches.json の auto-generated skip リストを 1.3〜1.5 の
///      根本修正後に下流ビルド実走で再評価し、まだ失敗するもののみに縮小
/// 1.7: 再評価の結果を反映 — CvDEPTH の return override (embed.fnc エントリは
///      Perl_CvDEPTH の記述で、deref するマクロ側は I32) と Perl_atof の
///      skip (5.32/5.34 ヘッダの aTHX_ 抜け、5.36 で上流修正) を登録
/// 1.8: 再評価で残った真の失敗のみをクラス別理由付きで skip_codegen に再登録
///      (5.32: 16 件、5.34: 21 件 — 戻り値位置ポインタキャスト欠落 /
///      bool・int 変換残渣 / 個別型不一致。旧 auto-generated 44/52 件から縮小)
/// 1.9: v5.44.json 追加 (perl 5.44.0 の embed.fnc から --apidoc-to-json で
///      生成、2059 entries — issue #6)
/// 1.10: v5.44.patches.json 新設 — hv_stores の第 3 引数を SV * に訂正
///      (5.44 で追加された apidoc_defn が `U32 flags` と誤記している上流バグ)
/// 1.11: Padname* アクセサ 10 件の arg_type_override (<=5.30 pad.h の `*` 抜け、
///      5.32 で上流修正) を common に追加。v5.20/v5.22.patches.json 新設 —
///      下流実走再評価で残った真の失敗のみをクラス別理由付きで skip 登録
///      (Perl_atof の aTHX_ 抜け / 旧ハッシュ inline 群 / bool・cast 残渣)
/// 1.12: v5.20 に PAD_RESTORE_LOCAL / PAD_COMPNAME_* の 4 件を追加 (5.20 固有の
///      マクロ形状。take4 の下流実走で判明)
/// 1.13: v5.34/v5.36 の CopLINE 系 skip_codegen 3 件を return_type_override
///      line_t 1 件に置換 (cop.h の `=for apidoc Am|STRLEN|CopLINE|...` 誤記が
///      原因。5.38 で上流修正。CopLINE_inc/_dec は CopLINE から推論されるため
///      連動して復活。CopLINE_set は 5.38 以降と同じ CODEGEN_INCOMPLETE)
/// 1.14: v5.28/v5.30 の auto-generated skip 6 件ずつ (S_SvREFCNT_dec{,_NN} /
///      PadlistARRAY / PadlistMAX / PadlistNAMESARRAY / PadlistNAMESMAX) を
///      下流ビルド実走再評価で解除 (0.1.8〜0.1.10 の型推論改善で解消済み
///      だった)。SvREFCNT_dec / PAD_SET_CUR 等が連動して復活。
///      Perl_SvREFCNT_dec という名前自体は 5.32 の inline 関数改名で登場
///      したもので <=5.30 には存在しない (expect の must_not_generate に記録)。
///      また common の RCPV_* return_type_override 4 件を 5.36 以前の各版で
///      `kind: "remove"` により打ち消し (RCPV_* は 5.38 生まれ。それ以前は
///      target 不在の MISS 警告ノイズになるだけだった — issue #17。
///      v5.24.patches.json はこの打ち消しのために新設)
/// 1.15: 5.20〜5.26 downstream green 化ラウンド (doc/plan/round-5.20-5.26.md)。
///      v5.26 の auto-generated skip 62 件を下流ビルド実走で再評価し 17 件を
///      恒久解除 (Padlist* 4 / S_SvREFCNT_dec{,_NN} / Padname·Padnamelist 全 8 /
///      CxLABEL / isUTF8_CHAR_flags / S_is_utf8_fixed_width_buf_loclen_flags。
///      連鎖で SvREFCNT_dec / PAD_SET_CUR 等も復活。PAD_SET_CUR は downstream
///      の実 bindgen bindings なら non-threaded でも生成される — multi-perl
///      smoke で nt 不生成に見えるのは threaded スナップショット
///      samples/bindings.rs に PL_comppad グローバルが無い harness 制限)。
///      真の失敗 12 件はクラス別理由付きで再登録し、初露出の sv_collxfrm
///      (上流 sv.h の typo: sv_cmp_flags を呼ぶ。5.34 で修正) を skip 新設。
///      v5.28/v5.30 の Padname* 8 件ずつも同根 (data 1.11 で解消済み) として解除。
///      v5.24 に skip 21 件を採取・登録 (0.1.7 時代の auto 採取から 5.24 leg
///      だけが漏れていた — hash inline 7 / Perl_atof / 残渣クラス 9 /
///      RX_* 3 / sv_collxfrm)。v5.22 に GvALIASED_SV_{on,off} の skip 2 件
///      (gp_flags ビットフィールドのアクセサが代入 LHS になる E0067、5.22 のみ)。
///      v5.20 に Padname*REFCNT{,_dec} の remove 4 件 (5.20 に API 不在で
///      common override が MISS ノイズになるだけ — issue #17 と同型)。
///      v5.20〜v5.32 の sv_collxfrm skip の reason を正確な原因に更新。
pub const APIDOC_DATA_VERSION: &str = "1.16";

/// 埋め込まれた apidoc.tar.gz データ
const EMBEDDED_APIDOC: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/apidoc.tar.gz"));

/// キャッシュされた apidoc ディレクトリのパス
static CACHED_APIDOC_DIR: OnceLock<Option<PathBuf>> = OnceLock::new();

/// apidoc データのキャッシュディレクトリを取得
///
/// 初回呼び出し時に埋め込みデータを展開してキャッシュする。
/// 既にキャッシュが存在する場合はそれを返す。
///
/// # Returns
/// - `Some(PathBuf)`: 展開された apidoc ディレクトリへのパス
/// - `None`: 展開に失敗した場合
pub fn get_apidoc_dir() -> Option<PathBuf> {
    CACHED_APIDOC_DIR
        .get_or_init(|| extract_apidoc_if_needed().ok())
        .clone()
}

/// キャッシュディレクトリのベース候補を優先度順で列挙
///
/// 1. `LIBPERL_APIDOC_CACHE_DIR` — ユーザの明示 override
/// 2. `OUT_DIR` — build-script ランタイムでは確実に書き込み可能。
///    docs.rs / 他 sandboxed 環境はここで成功する。
/// 3. `HOME/.cache` (Linux) / `Library/Caches` (macOS) /
///    `LOCALAPPDATA` (Windows) — 普段の dev 環境はここでヒット。
///    再ビルド間でキャッシュが効くので速い。
/// 4. `std::env::temp_dir()` — 最後の砦。書き込み可だが揮発的。
///
/// 全候補を試して書き込みに成功した最初のものを採用する。
fn cache_base_candidates() -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Ok(path) = std::env::var("LIBPERL_APIDOC_CACHE_DIR") {
        candidates.push(PathBuf::from(path));
    }

    if let Ok(out_dir) = std::env::var("OUT_DIR") {
        candidates.push(PathBuf::from(out_dir));
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(home) = std::env::var("HOME") {
            candidates.push(PathBuf::from(home).join(".cache"));
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(home) = std::env::var("HOME") {
            candidates.push(PathBuf::from(home).join("Library/Caches"));
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
            candidates.push(PathBuf::from(local_app_data));
        }
    }

    candidates.push(std::env::temp_dir());

    candidates
}

/// apidoc データを展開してキャッシュ
///
/// 候補ディレクトリを順に試し、書き込みに成功したものを採用。
/// 全候補で失敗したら最後のエラーを返す。
fn extract_apidoc_if_needed() -> io::Result<PathBuf> {
    let mut last_err: Option<io::Error> = None;
    for cache_base in cache_base_candidates() {
        match try_extract_to(&cache_base) {
            Ok(dir) => return Ok(dir),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "no writable cache directory candidates",
        )
    }))
}

/// 単一の cache_base で展開を試みる
fn try_extract_to(cache_base: &std::path::Path) -> io::Result<PathBuf> {
    let cache_dir = cache_base
        .join("libperl-macrogen")
        .join(format!("apidoc-v{}", APIDOC_DATA_VERSION));

    // キャッシュが既に存在していて整合していればそれを返す
    let apidoc_dir = cache_dir.join("apidoc");
    if apidoc_dir.is_dir() {
        let version_file = cache_dir.join("version");
        if let Ok(cached_version) = fs::read_to_string(&version_file) {
            if cached_version.trim() == APIDOC_DATA_VERSION {
                return Ok(apidoc_dir);
            }
        }
    }

    // キャッシュディレクトリを作成
    fs::create_dir_all(&cache_dir)?;

    // tar.gz を展開
    let cursor = Cursor::new(EMBEDDED_APIDOC);
    let gz_decoder = GzDecoder::new(cursor);
    let mut archive = Archive::new(gz_decoder);

    archive.unpack(&cache_dir)?;

    // バージョンファイルを書き込み
    fs::write(cache_dir.join("version"), APIDOC_DATA_VERSION)?;

    Ok(apidoc_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_embedded_data_not_empty() {
        assert!(!EMBEDDED_APIDOC.is_empty());
        // gzip マジックナンバー (0x1f 0x8b) を確認
        assert_eq!(EMBEDDED_APIDOC[0], 0x1f);
        assert_eq!(EMBEDDED_APIDOC[1], 0x8b);
    }

    #[test]
    fn test_get_apidoc_dir() {
        let dir = get_apidoc_dir();
        assert!(dir.is_some());
        let dir = dir.unwrap();
        assert!(dir.is_dir());
        // v5.40.json など存在するはず
        assert!(dir.join("v5.40.json").exists());
    }
}
