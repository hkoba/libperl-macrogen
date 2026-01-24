//! Build script for libperl-macrogen
//!
//! Downloads apidoc.tar.gz from GitHub Releases and embeds it into the binary.

use std::env;
use std::fs::File;
use std::io::{self, Write};
use std::path::Path;

/// apidoc データのバージョン
/// このバージョンは GitHub Releases のタグ名に対応
const APIDOC_DATA_VERSION: &str = "1.0";

/// GitHub リポジトリ情報
const GITHUB_OWNER: &str = "hkoba";
const GITHUB_REPO: &str = "libperl-macrogen";

fn main() {
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR not set");
    let archive_path = Path::new(&out_dir).join("apidoc.tar.gz");

    // 既にダウンロード済みならスキップ
    if archive_path.exists() {
        println!("cargo:rerun-if-changed=build.rs");
        return;
    }

    // 開発時: ローカルの apidoc/ ディレクトリから tar.gz を作成
    let local_apidoc = Path::new("apidoc");
    if local_apidoc.is_dir() {
        println!("cargo:warning=Using local apidoc/ directory");
        if let Err(e) = create_local_archive(local_apidoc, &archive_path) {
            panic!("Failed to create local apidoc archive: {}", e);
        }
        println!("cargo:rerun-if-changed=apidoc");
        return;
    }

    // リリース時: GitHub Releases からダウンロード
    let url = get_download_url();
    println!("cargo:warning=Downloading apidoc from {}", url);

    if let Err(e) = download_file(&url, &archive_path) {
        panic!("Failed to download apidoc archive: {}", e);
    }

    println!("cargo:rerun-if-changed=build.rs");
}

/// GitHub Releases からのダウンロード URL を生成
fn get_download_url() -> String {
    // 環境変数でオーバーライド可能
    if let Ok(url) = env::var("LIBPERL_APIDOC_URL") {
        return url;
    }

    format!(
        "https://github.com/{}/{}/releases/download/apidoc-v{}/apidoc.tar.gz",
        GITHUB_OWNER, GITHUB_REPO, APIDOC_DATA_VERSION
    )
}

/// ローカルの apidoc ディレクトリから tar.gz を作成
fn create_local_archive(_src_dir: &Path, dest: &Path) -> io::Result<()> {
    use std::process::Command;

    let output = Command::new("tar")
        .args(["-czf", dest.to_str().unwrap(), "-C", ".", "apidoc"])
        .output()?;

    if !output.status.success() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!(
                "tar command failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ),
        ));
    }

    Ok(())
}

/// ファイルをダウンロード
fn download_file(url: &str, dest: &Path) -> io::Result<()> {
    let response = ureq::get(url)
        .call()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

    if response.status() != 200 {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!("HTTP error: {}", response.status()),
        ));
    }

    // レスポンスボディを読み取り
    let mut bytes = Vec::new();
    response
        .into_reader()
        .read_to_end(&mut bytes)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

    // ファイルに書き込み
    let mut file = File::create(dest)?;
    file.write_all(&bytes)?;

    Ok(())
}
