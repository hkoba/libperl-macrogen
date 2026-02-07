//! Rust コード生成の回帰テスト
//!
//! 特定の関数・マクロについて、生成されるRustコードが期待通りかを検証する。
//! 期待結果は `tests/expected_rust/` ディレクトリに個別ファイルとして保存されている。

use std::fs;
use std::path::Path;
use std::process::Command;

/// 期待結果ディレクトリ
const EXPECTED_DIR: &str = "tests/expected_rust";

/// テスト対象の関数名リスト
const TARGET_FUNCTIONS: &[&str] = &[
    "Perl_CvDEPTH",
    "Perl_cx_topblock",
    "OP_CLASS",
    "CvSTASH",
    "CopFILE",
    "CopFILEAV",
    "CopLABEL",
    "HvFILL",
    "PerlIO_seek",
    "PerlIO_tell",
    "AMG_CALLunary",
    "newSVpvs",
];

/// 生成された Rust コードから特定の関数を抽出する
fn extract_function(output: &str, fn_name: &str) -> Option<String> {
    let lines: Vec<&str> = output.lines().collect();
    let mut result = Vec::new();
    let mut in_function = false;
    let mut brace_count = 0;
    let mut seen_open_brace = false;

    for (i, line) in lines.iter().enumerate() {
        // 関数の開始を検出（pub unsafe fn NAME）
        if line.contains(&format!("pub unsafe fn {}(", fn_name))
           || line.contains(&format!("pub unsafe fn {}<", fn_name))
        {
            in_function = true;

            // 直前の doc コメントと #[inline] を含める
            let mut start = i;
            for j in (0..i).rev() {
                let prev = lines[j].trim();
                if prev.starts_with("///") || prev.starts_with("#[inline]") {
                    start = j;
                } else if !prev.is_empty() {
                    break;
                }
            }

            // doc コメントから追加
            for k in start..i {
                result.push(lines[k].to_string());
            }
        }

        if in_function {
            result.push(line.to_string());

            // ブレースのカウント
            let open_braces = line.chars().filter(|&c| c == '{').count() as i32;
            let close_braces = line.chars().filter(|&c| c == '}').count() as i32;
            brace_count += open_braces;
            brace_count -= close_braces;

            if open_braces > 0 {
                seen_open_brace = true;
            }

            // 関数の終了（開きブレースを見た後に閉じたとき）
            if seen_open_brace && brace_count == 0 {
                break;
            }
        }
    }

    if result.is_empty() {
        None
    } else {
        Some(result.join("\n"))
    }
}

/// 期待結果ファイルを読み込む
fn load_expected(fn_name: &str) -> Result<String, String> {
    let path = Path::new(EXPECTED_DIR).join(format!("{}.rs", fn_name));
    fs::read_to_string(&path)
        .map(|s| s.trim().to_string())
        .map_err(|e| format!("Failed to read {}: {}", path.display(), e))
}

/// cargo run で Rust コードを生成する
fn generate_rust_code() -> Result<String, String> {
    let output = Command::new("cargo")
        .args([
            "run", "--",
            "--auto",
            "--gen-rust",
            "samples/xs-wrapper.h",
            "--bindings", "samples/bindings.rs",
        ])
        .output()
        .map_err(|e| format!("Failed to run cargo: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // stderr に警告が含まれていても、stdout に出力があれば成功とみなす
        if output.stdout.is_empty() {
            return Err(format!("cargo run failed: {}", stderr));
        }
    }

    String::from_utf8(output.stdout)
        .map_err(|e| format!("Invalid UTF-8 output: {}", e))
}

/// 空白の正規化（比較用）
fn normalize_whitespace(s: &str) -> String {
    s.lines()
        .map(|line| line.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

#[test]
fn test_rust_codegen_regression() {
    // Rust コードを生成
    let generated = generate_rust_code().expect("Failed to generate Rust code");

    let mut failures = Vec::new();
    let mut successes = Vec::new();

    for fn_name in TARGET_FUNCTIONS {
        // 期待結果を読み込み
        let expected = match load_expected(fn_name) {
            Ok(e) => e,
            Err(e) => {
                failures.push(format!("{}: {}", fn_name, e));
                continue;
            }
        };

        // 生成結果から関数を抽出
        let actual = match extract_function(&generated, fn_name) {
            Some(a) => a,
            None => {
                failures.push(format!("{}: Function not found in generated output", fn_name));
                continue;
            }
        };

        // 比較（空白を正規化）
        let expected_normalized = normalize_whitespace(&expected);
        let actual_normalized = normalize_whitespace(&actual);

        if expected_normalized != actual_normalized {
            failures.push(format!(
                "{}: Output mismatch\n--- Expected ---\n{}\n--- Actual ---\n{}",
                fn_name, expected_normalized, actual_normalized
            ));
        } else {
            successes.push(fn_name.to_string());
        }
    }

    // 結果の表示
    if !successes.is_empty() {
        println!("\n=== Passed ({}) ===", successes.len());
        for name in &successes {
            println!("  ✓ {}", name);
        }
    }

    if !failures.is_empty() {
        println!("\n=== Failed ({}) ===", failures.len());
        for failure in &failures {
            println!("\n{}", failure);
        }
        panic!(
            "Rust codegen regression test failed: {} of {} functions",
            failures.len(),
            TARGET_FUNCTIONS.len()
        );
    }
}

/// 個別の関数テスト用ヘルパーマクロ
/// 新しい関数を追加する際は、TARGET_FUNCTIONS に追加し、
/// tests/expected_rust/{関数名}.rs ファイルを作成する
#[cfg(test)]
mod individual_tests {
    use super::*;

    /// 単一の関数をテストする
    #[allow(dead_code)]
    fn test_single_function(fn_name: &str) {
        let generated = generate_rust_code().expect("Failed to generate Rust code");
        let expected = load_expected(fn_name).expect("Failed to load expected output");
        let actual = extract_function(&generated, fn_name)
            .expect(&format!("Function {} not found in generated output", fn_name));

        let expected_normalized = normalize_whitespace(&expected);
        let actual_normalized = normalize_whitespace(&actual);

        assert_eq!(
            expected_normalized, actual_normalized,
            "Output mismatch for {}\n--- Expected ---\n{}\n--- Actual ---\n{}",
            fn_name, expected_normalized, actual_normalized
        );
    }

    // 個別テスト - 必要に応じてコメントを外して使用
    // #[test]
    // fn test_perl_cvdepth() { test_single_function("Perl_CvDEPTH"); }
    //
    // #[test]
    // fn test_perl_cx_topblock() { test_single_function("Perl_cx_topblock"); }
    //
    // #[test]
    // fn test_op_class() { test_single_function("OP_CLASS"); }
    //
    // #[test]
    // fn test_cvstash() { test_single_function("CvSTASH"); }
    //
    // #[test]
    // fn test_copfile() { test_single_function("CopFILE"); }
}
