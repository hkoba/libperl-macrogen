//! End-to-end integration tests

use std::io::Write;
use std::ops::ControlFlow;
use tempfile::NamedTempFile;
use libperl_macrogen::{
    ExternalDecl, PPConfig, Parser, Preprocessor, SexpPrinter,
};

/// Helper to parse source and return declarations
fn parse_source(source: &str) -> Vec<ExternalDecl> {
    let mut file = NamedTempFile::new().unwrap();
    file.write_all(source.as_bytes()).unwrap();
    file.flush().unwrap();

    let config = PPConfig {
        include_paths: vec![],
        predefined: vec![],
        debug_pp: false,
        target_dir: None,
        ..Default::default()
    };

    let mut pp = Preprocessor::new(config);
    pp.add_source_file(file.path()).unwrap();

    let mut parser = Parser::new(&mut pp).unwrap();
    parser.parse().unwrap().decls
}

/// Helper to parse and output S-expression
fn parse_to_sexp(source: &str) -> String {
    let mut file = NamedTempFile::new().unwrap();
    file.write_all(source.as_bytes()).unwrap();
    file.flush().unwrap();

    let config = PPConfig {
        include_paths: vec![],
        predefined: vec![],
        debug_pp: false,
        target_dir: None,
        ..Default::default()
    };

    let mut pp = Preprocessor::new(config);
    pp.add_source_file(file.path()).unwrap();

    let mut parser = Parser::new(&mut pp).unwrap();
    let tu = parser.parse().unwrap();

    let mut output = Vec::new();
    {
        let mut printer = SexpPrinter::new(&mut output, parser.interner());
        printer.print_translation_unit(&tu).unwrap();
    }
    String::from_utf8(output).unwrap()
}

/// Helper to use streaming parser
fn parse_streaming(source: &str) -> (Vec<ExternalDecl>, Option<String>) {
    let mut file = NamedTempFile::new().unwrap();
    file.write_all(source.as_bytes()).unwrap();
    file.flush().unwrap();

    let config = PPConfig {
        include_paths: vec![],
        predefined: vec![],
        debug_pp: false,
        target_dir: None,
        ..Default::default()
    };

    let mut pp = Preprocessor::new(config);
    pp.add_source_file(file.path()).unwrap();

    let mut parser = Parser::new(&mut pp).unwrap();
    let mut decls = Vec::new();

    let result = parser.parse_each(|decl, _loc, _path, _interner| {
        decls.push(decl.clone());
        ControlFlow::Continue(())
    });

    let error = result.err().map(|e| format!("{}", e));
    (decls, error)
}

#[test]
fn test_empty_source() {
    let decls = parse_source("");
    assert!(decls.is_empty());
}

#[test]
fn test_single_declaration() {
    let decls = parse_source("int x;");
    assert_eq!(decls.len(), 1);
}

#[test]
fn test_multiple_declarations() {
    let decls = parse_source("int x;\nint y;\nint z;");
    assert_eq!(decls.len(), 3);
}

#[test]
fn test_preprocessor_and_parser() {
    let source = r#"
        #define VALUE 42
        int x = VALUE;
    "#;
    let decls = parse_source(source);
    assert_eq!(decls.len(), 1);
}

#[test]
fn test_conditional_compilation() {
    let source = r#"
        #define FEATURE_A
        #ifdef FEATURE_A
        int a;
        #endif
        #ifdef FEATURE_B
        int b;
        #endif
        int c;
    "#;
    let decls = parse_source(source);
    // a and c should be parsed, b should be skipped
    assert_eq!(decls.len(), 2);
}

#[test]
fn test_macro_in_function() {
    let source = r#"
        #define RETURN_ZERO return 0
        int foo(void) { RETURN_ZERO; }
    "#;
    let decls = parse_source(source);
    assert_eq!(decls.len(), 1);
}

#[test]
fn test_sexp_output_declaration() {
    let sexp = parse_to_sexp("int x;");
    assert!(sexp.contains("translation-unit"));
    assert!(sexp.contains("declaration"));
    assert!(sexp.contains("int"));
}

#[test]
fn test_sexp_output_function() {
    let sexp = parse_to_sexp("int foo(void) { return 0; }");
    assert!(sexp.contains("function-def"));
    assert!(sexp.contains("return"));
}

#[test]
fn test_sexp_output_struct() {
    let sexp = parse_to_sexp("struct Point { int x; int y; };");
    assert!(sexp.contains("struct"));
}

#[test]
fn test_streaming_parser_success() {
    let (decls, error) = parse_streaming("int x; int y;");
    assert_eq!(decls.len(), 2);
    assert!(error.is_none());
}

#[test]
fn test_streaming_parser_partial() {
    // Even if there's a parse error, previously parsed declarations are returned
    // Use "int x {" which has an unexpected brace
    let source = "int x; int y {";
    let (decls, error) = parse_streaming(source);
    // Should have parsed 'int x;' before the error
    assert!(decls.len() >= 1);
    assert!(error.is_some());
}

#[test]
fn test_complex_struct() {
    let source = r#"
        struct Node {
            int value;
            struct Node *next;
            struct Node *prev;
        };
    "#;
    let decls = parse_source(source);
    assert_eq!(decls.len(), 1);
}

#[test]
fn test_typedef_struct() {
    let source = r#"
        typedef struct {
            int x;
            int y;
        } Point;
        Point p;
    "#;
    let decls = parse_source(source);
    assert_eq!(decls.len(), 2);
}

#[test]
fn test_function_with_complex_body() {
    let source = r#"
        int factorial(int n) {
            if (n <= 1) {
                return 1;
            }
            return n * factorial(n - 1);
        }
    "#;
    let decls = parse_source(source);
    assert_eq!(decls.len(), 1);
}

#[test]
fn test_multiple_functions() {
    let source = r#"
        int add(int a, int b) { return a + b; }
        int sub(int a, int b) { return a - b; }
        int mul(int a, int b) { return a * b; }
    "#;
    let decls = parse_source(source);
    assert_eq!(decls.len(), 3);
}

#[test]
fn test_predefined_macro_integration() {
    let mut file = NamedTempFile::new().unwrap();
    file.write_all(b"int x = PREDEFINED;").unwrap();
    file.flush().unwrap();

    let config = PPConfig {
        include_paths: vec![],
        predefined: vec![("PREDEFINED".to_string(), Some("100".to_string()))],
        debug_pp: false,
        target_dir: None,
        ..Default::default()
    };

    let mut pp = Preprocessor::new(config);
    pp.add_source_file(file.path()).unwrap();

    let mut parser = Parser::new(&mut pp).unwrap();
    let tu = parser.parse().unwrap();

    assert_eq!(tu.decls.len(), 1);
}

#[test]
fn test_nested_macros() {
    let source = r#"
        #define A B
        #define B C
        #define C 123
        int x = A;
    "#;
    let decls = parse_source(source);
    assert_eq!(decls.len(), 1);
}

#[test]
fn test_macro_with_operators() {
    let source = r#"
        #define MAX(a, b) ((a) > (b) ? (a) : (b))
        int x = MAX(10, 20);
    "#;
    let decls = parse_source(source);
    assert_eq!(decls.len(), 1);
}

#[test]
fn test_inline_function_with_macro() {
    let source = r#"
        #define UNUSED __attribute__((unused))
        static inline int foo(int x UNUSED) {
            return x + 1;
        }
    "#;
    let decls = parse_source(source);
    assert_eq!(decls.len(), 1);
}

#[test]
fn test_gcc_attributes() {
    let source = r#"
        void foo(void) __attribute__((noreturn));
        int bar(int x) __attribute__((pure));
        struct __attribute__((packed)) S { int x; };
    "#;
    let decls = parse_source(source);
    assert_eq!(decls.len(), 3);
}

#[test]
fn test_statement_expression_in_macro() {
    let source = r#"
        #define SWAP(a, b) ({ int tmp = (a); (a) = (b); (b) = tmp; })
        void foo(void) {
            int x = 1, y = 2;
            SWAP(x, y);
        }
    "#;
    let decls = parse_source(source);
    assert_eq!(decls.len(), 1);
}

#[test]
fn test_complex_expression() {
    let source = r#"
        int x = (1 + 2) * 3 / 4 % 5 - 6;
        int y = 1 << 2 >> 1;
        int z = 1 & 2 | 3 ^ 4;
        int w = 1 && 2 || !3;
    "#;
    let decls = parse_source(source);
    assert_eq!(decls.len(), 4);
}

#[test]
fn test_cast_expression() {
    let source = r#"
        void *p;
        int x = (int)(long)p;
    "#;
    let decls = parse_source(source);
    assert_eq!(decls.len(), 2);
}

#[test]
fn test_sizeof_expression() {
    let source = r#"
        int x = sizeof(int);
        int y = sizeof(struct { int a; int b; });
    "#;
    let decls = parse_source(source);
    assert_eq!(decls.len(), 2);
}

#[test]
fn test_designated_initializer() {
    let source = r#"
        struct Point { int x; int y; };
        struct Point p = { .x = 1, .y = 2 };
    "#;
    let decls = parse_source(source);
    assert_eq!(decls.len(), 2);
}

#[test]
fn test_compound_literal() {
    let source = r#"
        struct Point { int x; int y; };
        void foo(void) {
            struct Point *p = &(struct Point){ 1, 2 };
        }
    "#;
    let decls = parse_source(source);
    assert_eq!(decls.len(), 2);
}

#[test]
fn test_bitfield() {
    let source = r#"
        struct Flags {
            unsigned int a : 1;
            unsigned int b : 2;
            unsigned int c : 5;
        };
    "#;
    let decls = parse_source(source);
    assert_eq!(decls.len(), 1);
}

#[test]
fn test_anonymous_struct() {
    let source = r#"
        struct Outer {
            int x;
            struct {
                int a;
                int b;
            };
        };
    "#;
    let decls = parse_source(source);
    assert_eq!(decls.len(), 1);
}

#[test]
fn test_forward_declaration() {
    let source = r#"
        struct Node;
        struct Node {
            struct Node *next;
        };
    "#;
    let decls = parse_source(source);
    assert_eq!(decls.len(), 2);
}

#[test]
fn test_all_storage_classes() {
    let source = r#"
        extern int a;
        static int b;
        int c;
        typedef int MyInt;
    "#;
    let decls = parse_source(source);
    assert_eq!(decls.len(), 4);
}

#[test]
fn test_all_type_qualifiers() {
    let source = r#"
        const int a = 1;
        volatile int b;
        const volatile int c;
    "#;
    let decls = parse_source(source);
    assert_eq!(decls.len(), 3);
}

#[test]
fn test_pointer_qualifiers() {
    let source = r#"
        const int *p1;
        int * const p2;
        const int * const p3;
    "#;
    let decls = parse_source(source);
    assert_eq!(decls.len(), 3);
}
