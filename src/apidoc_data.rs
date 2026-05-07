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
pub const APIDOC_DATA_VERSION: &str = "1.0";

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
