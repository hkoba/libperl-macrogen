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

    // rerun トリガを早めに宣言（早期 return / panic でも cargo に反映される）。
    // build.rs と apidoc/ 配下の任意の変更で再実行する。これを宣言しておかないと、
    // GitHub Actions の target/ キャッシュ等で stale な apidoc.tar.gz が
    // OUT_DIR に居座り続けて library binary が古い patches を embed してしまう。
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=apidoc");
    println!("cargo:rerun-if-env-changed=DOCS_RS");
    println!("cargo:rerun-if-env-changed=LIBPERL_APIDOC_URL");

    // docs.rs builds run inside a `--network none` sandbox, so the
    // GitHub Releases download below would panic with a DNS error.
    // docs.rs only invokes `rustdoc`, never the binary or runtime
    // code that consumes the apidoc, so an empty placeholder archive
    // is sufficient. Detected via the `DOCS_RS=1` env var that
    // docs.rs sets in its build environment.
    if env::var("DOCS_RS").is_ok() {
        let _ = std::fs::remove_file(&archive_path);
        write_empty_archive(&archive_path)
            .expect("write empty apidoc placeholder for docs.rs");
        return;
    }

    // 開発時: ローカルの apidoc/ ディレクトリから tar.gz を作成。
    // **キャッシュされた archive を信用せず毎回作り直す**（数 MB の tar 化なので
    // 十分速く、stale 化リスクの方が大きい）。
    let local_apidoc = Path::new("apidoc");
    if local_apidoc.is_dir() {
        // 既存の archive は明示的に削除（mtime 比較ではなく毎回まっさら）
        let _ = std::fs::remove_file(&archive_path);
        if let Err(e) = create_local_archive(local_apidoc, &archive_path) {
            panic!("Failed to create local apidoc archive: {}", e);
        }
        return;
    }

    // リリース時（crates.io 由来など、apidoc/ が exclude されている）:
    // 既存の archive があれば再利用、無ければ GitHub Releases からダウンロード。
    if archive_path.exists() {
        return;
    }
    let url = get_download_url();
    println!("cargo:warning=Downloading apidoc from {}", url);
    if let Err(e) = download_file(&url, &archive_path) {
        panic!("Failed to download apidoc archive: {}", e);
    }
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

/// 空の `tar.gz` を書き出す。docs.rs のような offline / 機能を実行しない
/// ビルド環境向けの placeholder。`tar`/`flate2` 依存を build-deps に
/// 追加せずに済むよう、最小の有効な gzip header (空の gzip stream) を
/// 直接書く。runtime の `flate2::read::GzDecoder` で読めば 0 バイトの
/// uncompressed stream が返る。
fn write_empty_archive(dest: &Path) -> io::Result<()> {
    // RFC 1952 minimal gzip: ID1, ID2, CM=8 (deflate), FLG=0,
    // MTIME=0 (4 bytes), XFL=0, OS=255 (unknown), then a zero-length
    // deflate block (BFINAL=1, BTYPE=00, no data, byte-aligned),
    // then CRC32(empty)=0 and ISIZE=0 (each 4 bytes LE).
    // Canonical 20-byte empty gzip stream (the bytes `gzip < /dev/null`
    // produces, with MTIME zeroed for reproducibility).
    let bytes: [u8; 20] = [
        0x1f, 0x8b, 0x08, 0x00, // ID1, ID2, CM=8 (deflate), FLG=0
        0x00, 0x00, 0x00, 0x00, // MTIME=0
        0x00, 0xff,             // XFL=0, OS=255 (unknown)
        0x03, 0x00,             // empty deflate block
        0x00, 0x00, 0x00, 0x00, // CRC32 = 0 (CRC of empty input)
        0x00, 0x00, 0x00, 0x00, // ISIZE = 0 (uncompressed length)
    ];
    let mut file = File::create(dest)?;
    file.write_all(&bytes)?;
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
