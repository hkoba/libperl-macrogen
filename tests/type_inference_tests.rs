//! 型推論テスト
//!
//! tests/type_inference/*.c ファイルをパースし、
//! 対応する .expected ファイルと比較する
//!
//! 注: TypedSexpPrinter は type_env ベースの型取得に変更されたため、
//! フル AST の型推論は現在サポートされていません。
//! マクロ式の型推論のみがサポートされます。

use std::fs;
use std::path::PathBuf;
use libperl_macrogen::{
    Parser, Preprocessor, PPConfig, TypedSexpPrinter, ExternalDecl,
};

/// テストケースを実行
fn run_test_case(name: &str) {
    let test_dir = PathBuf::from("tests/type_inference");
    let c_file = test_dir.join(format!("{}.c", name));
    let expected_file = test_dir.join(format!("{}.expected", name));

    // 期待される出力を読み込む
    let expected = fs::read_to_string(&expected_file)
        .expect(&format!("Failed to read {:?}", expected_file));

    // C ファイルをパース
    let mut pp = Preprocessor::new(PPConfig::default());
    pp.process_file(&c_file)
        .expect(&format!("Failed to preprocess {:?}", c_file));

    let mut parser = Parser::new(&mut pp)
        .expect("Failed to create parser");
    let tu = parser.parse()
        .expect("Failed to parse");

    // 型注釈付き S-expression を生成
    let mut output = Vec::new();
    {
        let mut printer = TypedSexpPrinter::new(&mut output, pp.interner());
        for decl in &tu.decls {
            printer.print_external_decl(decl).unwrap();
        }
    }

    let actual = String::from_utf8(output).unwrap();

    // 比較
    assert_eq!(
        actual.trim(),
        expected.trim(),
        "\n=== Test: {} ===\nExpected:\n{}\nActual:\n{}",
        name,
        expected.trim(),
        actual.trim()
    );
}

#[test]
fn test_t001_int_literal() {
    run_test_case("t001_int_literal");
}

#[test]
#[ignore = "full AST type inference not supported with type_env-based TypedSexpPrinter"]
fn test_t002_var_ref() {
    run_test_case("t002_var_ref");
}

#[test]
#[ignore = "full AST type inference not supported with type_env-based TypedSexpPrinter"]
fn test_t003_binary_op() {
    run_test_case("t003_binary_op");
}

#[test]
#[ignore = "full AST type inference not supported with type_env-based TypedSexpPrinter"]
fn test_t004_pointer() {
    run_test_case("t004_pointer");
}

#[test]
#[ignore = "full AST type inference not supported with type_env-based TypedSexpPrinter"]
fn test_t005_function_scope() {
    run_test_case("t005_function_scope");
}

#[test]
#[ignore = "full AST type inference not supported with type_env-based TypedSexpPrinter"]
fn test_t006_shadowing() {
    run_test_case("t006_shadowing");
}

#[test]
#[ignore = "full AST type inference not supported with type_env-based TypedSexpPrinter"]
fn test_t007_function_call() {
    run_test_case("t007_function_call");
}

#[test]
#[ignore = "full AST type inference not supported with type_env-based TypedSexpPrinter"]
fn test_t008_struct_member() {
    run_test_case("t008_struct_member");
}

#[test]
#[ignore = "full AST type inference not supported with type_env-based TypedSexpPrinter"]
fn test_t009_cast() {
    run_test_case("t009_cast");
}
