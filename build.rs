//! Build script for libperl-macrogen.
//!
//! Stages a copy of `apidoc.tar.gz` into `OUT_DIR` so the runtime
//! (which `include_bytes!`s the file via `concat!(env!("OUT_DIR"), …)`)
//! always has something to read. The selection logic is:
//!
//! 1. **`LIBPERL_APIDOC_URL` set** — explicit network override; download
//!    from there. Useful for offline mirrors / pre-release apidoc data.
//! 2. **`apidoc/` source dir present** — re-tar locally each build. This
//!    is the development path: regenerating from the source means edits
//!    to `apidoc/v5.X.json` propagate without touching the tarball.
//! 3. **`apidoc.tar.gz` shipped at repo / crate root** — copy as-is.
//!    This is the release path for crates.io consumers: the tarball is
//!    bundled in the published crate (~1.9 MiB compressed), so no
//!    network is required at build time. Works under docs.rs's
//!    `--network none` sandbox out of the box.
//! 4. **Last-resort fallback** — write an empty placeholder tar.gz and
//!    print a `cargo:warning`. The library will run but generate zero
//!    macro wrappers; useful to keep `cargo check` green in unusual
//!    environments rather than panic.

use std::env;
use std::fs::File;
use std::io::{self, Write};
use std::path::Path;

fn main() {
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR not set");
    let archive_path = Path::new(&out_dir).join("apidoc.tar.gz");

    // rerun トリガを早めに宣言（早期 return / panic でも cargo に反映）。
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=apidoc");
    println!("cargo:rerun-if-changed=apidoc.tar.gz");
    println!("cargo:rerun-if-env-changed=LIBPERL_APIDOC_URL");

    // (1) Explicit URL override — try download first, panic on failure
    //     (the user asked for a specific URL, so silent fallback would
    //     hide misconfiguration).
    if let Ok(url) = env::var("LIBPERL_APIDOC_URL") {
        let _ = std::fs::remove_file(&archive_path);
        println!("cargo:warning=Downloading apidoc from {}", url);
        if let Err(e) = download_file(&url, &archive_path) {
            panic!("LIBPERL_APIDOC_URL download failed: {}", e);
        }
        return;
    }

    // (2) Development: re-tar from apidoc/ source so edits propagate.
    //     "Don't trust cached archive" — re-tar every build (fast).
    let local_apidoc = Path::new("apidoc");
    if local_apidoc.is_dir() {
        let _ = std::fs::remove_file(&archive_path);
        if let Err(e) = create_local_archive(local_apidoc, &archive_path) {
            panic!("Failed to create local apidoc archive: {}", e);
        }
        return;
    }

    // (3) Release: bundled tarball at crate root (apidoc.tar.gz).
    //     This is the crates.io path. Works offline (docs.rs etc.).
    let bundled = Path::new("apidoc.tar.gz");
    if bundled.exists() {
        std::fs::copy(bundled, &archive_path)
            .expect("copy bundled apidoc.tar.gz to OUT_DIR");
        return;
    }

    // (4) Last resort: write an empty placeholder so the library compiles
    //     but emits zero macro wrappers. Issue a `cargo:warning` so the
    //     consumer notices something's off.
    println!(
        "cargo:warning=No apidoc source found (no apidoc/ dir, no \
         apidoc.tar.gz, no LIBPERL_APIDOC_URL). Macrogen will produce \
         zero macro wrappers; this is probably not what you want."
    );
    let _ = std::fs::remove_file(&archive_path);
    write_empty_archive(&archive_path)
        .expect("write empty apidoc placeholder");
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

/// 空の `tar.gz` を書き出す。Last-resort fallback only.
///
/// Canonical 20-byte empty gzip stream — what `gzip < /dev/null`
/// produces with MTIME zeroed for reproducibility. Avoids pulling
/// `tar`/`flate2` into build-deps.
fn write_empty_archive(dest: &Path) -> io::Result<()> {
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

    let mut bytes = Vec::new();
    response
        .into_reader()
        .read_to_end(&mut bytes)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

    let mut file = File::create(dest)?;
    file.write_all(&bytes)?;

    Ok(())
}
