//! Parser integration tests

use std::io::Write;
use tempfile::NamedTempFile;
use libperl_macrogen::{
    DerivedDecl, ExternalDecl, PPConfig, Parser, Preprocessor, StorageClass, TypeSpec,
};

/// Helper to parse a source string and return translation unit
fn parse(source: &str) -> Vec<ExternalDecl> {
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
    pp.process_file(file.path()).unwrap();

    let mut parser = Parser::new(&mut pp).unwrap();
    let tu = parser.parse().unwrap();
    tu.decls
}

/// Helper to check if a declaration has typedef storage class
fn is_typedef(decl: &ExternalDecl) -> bool {
    match decl {
        ExternalDecl::Declaration(d) => d.specs.storage == Some(StorageClass::Typedef),
        _ => false,
    }
}

/// Helper to check if a declaration has static storage class
fn is_static(decl: &ExternalDecl) -> bool {
    match decl {
        ExternalDecl::Declaration(d) => d.specs.storage == Some(StorageClass::Static),
        ExternalDecl::FunctionDef(f) => f.specs.storage == Some(StorageClass::Static),
    }
}

/// Helper to check if a declaration has extern storage class
fn is_extern(decl: &ExternalDecl) -> bool {
    match decl {
        ExternalDecl::Declaration(d) => d.specs.storage == Some(StorageClass::Extern),
        ExternalDecl::FunctionDef(f) => f.specs.storage == Some(StorageClass::Extern),
    }
}

/// Helper to check if type specs contain a specific type
fn has_type_spec(decl: &ExternalDecl, check: impl Fn(&TypeSpec) -> bool) -> bool {
    match decl {
        ExternalDecl::Declaration(d) => d.specs.type_specs.iter().any(check),
        ExternalDecl::FunctionDef(f) => f.specs.type_specs.iter().any(check),
    }
}

#[test]
fn test_simple_variable() {
    let decls = parse("int x;");
    assert_eq!(decls.len(), 1);
    assert!(has_type_spec(&decls[0], |t| matches!(t, TypeSpec::Int)));
}

#[test]
fn test_multiple_variables() {
    let decls = parse("int x, y, z;");
    assert_eq!(decls.len(), 1);

    if let ExternalDecl::Declaration(d) = &decls[0] {
        assert_eq!(d.declarators.len(), 3);
    } else {
        panic!("Expected declaration");
    }
}

#[test]
fn test_typedef() {
    let decls = parse("typedef int MyInt;");
    assert_eq!(decls.len(), 1);
    assert!(is_typedef(&decls[0]));
}

#[test]
fn test_typedef_usage() {
    let decls = parse("typedef int MyInt;\nMyInt x;");
    assert_eq!(decls.len(), 2);
    assert!(is_typedef(&decls[0]));
}

#[test]
fn test_struct_declaration() {
    let decls = parse("struct Point { int x; int y; };");
    assert_eq!(decls.len(), 1);
    assert!(has_type_spec(&decls[0], |t| matches!(t, TypeSpec::Struct(_))));
}

#[test]
fn test_struct_variable() {
    let decls = parse("struct Point { int x; int y; } p;");
    assert_eq!(decls.len(), 1);

    if let ExternalDecl::Declaration(d) = &decls[0] {
        assert_eq!(d.declarators.len(), 1);
    } else {
        panic!("Expected declaration");
    }
}

#[test]
fn test_union_declaration() {
    let decls = parse("union Value { int i; float f; };");
    assert_eq!(decls.len(), 1);
    assert!(has_type_spec(&decls[0], |t| matches!(t, TypeSpec::Union(_))));
}

#[test]
fn test_enum_declaration() {
    let decls = parse("enum Color { RED, GREEN, BLUE };");
    assert_eq!(decls.len(), 1);
    assert!(has_type_spec(&decls[0], |t| matches!(t, TypeSpec::Enum(_))));
}

#[test]
fn test_enum_with_values() {
    let decls = parse("enum Status { OK = 0, ERROR = 1 };");
    assert_eq!(decls.len(), 1);
}

#[test]
fn test_function_declaration() {
    let decls = parse("int foo(int x, int y);");
    assert_eq!(decls.len(), 1);

    if let ExternalDecl::Declaration(d) = &decls[0] {
        assert_eq!(d.declarators.len(), 1);
    } else {
        panic!("Expected declaration");
    }
}

#[test]
fn test_function_definition() {
    let decls = parse("int foo(int x) { return x + 1; }");
    assert_eq!(decls.len(), 1);
    assert!(matches!(decls[0], ExternalDecl::FunctionDef(_)));
}

#[test]
fn test_static_function() {
    let decls = parse("static int foo(void) { return 0; }");
    assert_eq!(decls.len(), 1);
    assert!(is_static(&decls[0]));
}

#[test]
fn test_extern_variable() {
    let decls = parse("extern int global_var;");
    assert_eq!(decls.len(), 1);
    assert!(is_extern(&decls[0]));
}

#[test]
fn test_const_qualifier() {
    let decls = parse("const int x = 5;");
    assert_eq!(decls.len(), 1);

    if let ExternalDecl::Declaration(d) = &decls[0] {
        assert!(d.specs.qualifiers.is_const);
    } else {
        panic!("Expected declaration");
    }
}

#[test]
fn test_volatile_qualifier() {
    let decls = parse("volatile int x;");
    assert_eq!(decls.len(), 1);

    if let ExternalDecl::Declaration(d) = &decls[0] {
        assert!(d.specs.qualifiers.is_volatile);
    } else {
        panic!("Expected declaration");
    }
}

#[test]
fn test_pointer_declaration() {
    let decls = parse("int *p;");
    assert_eq!(decls.len(), 1);

    if let ExternalDecl::Declaration(d) = &decls[0] {
        assert!(d.declarators[0].declarator.derived.iter().any(|d| matches!(d, DerivedDecl::Pointer(_))));
    } else {
        panic!("Expected declaration");
    }
}

#[test]
fn test_array_declaration() {
    let decls = parse("int arr[10];");
    assert_eq!(decls.len(), 1);

    if let ExternalDecl::Declaration(d) = &decls[0] {
        assert!(d.declarators[0].declarator.derived.iter().any(|d| matches!(d, DerivedDecl::Array(_))));
    } else {
        panic!("Expected declaration");
    }
}

#[test]
fn test_function_pointer() {
    let decls = parse("int (*fp)(int, int);");
    assert_eq!(decls.len(), 1);
}

#[test]
fn test_inline_function() {
    let decls = parse("inline int foo(void) { return 0; }");
    assert_eq!(decls.len(), 1);

    if let ExternalDecl::FunctionDef(f) = &decls[0] {
        assert!(f.specs.is_inline);
    } else {
        panic!("Expected function definition");
    }
}

#[test]
fn test_static_inline() {
    let decls = parse("static inline int foo(void) { return 0; }");
    assert_eq!(decls.len(), 1);
    assert!(is_static(&decls[0]));

    if let ExternalDecl::FunctionDef(f) = &decls[0] {
        assert!(f.specs.is_inline);
    } else {
        panic!("Expected function definition");
    }
}

#[test]
fn test_void_function() {
    let decls = parse("void foo(void);");
    assert_eq!(decls.len(), 1);
    assert!(has_type_spec(&decls[0], |t| matches!(t, TypeSpec::Void)));
}

#[test]
fn test_variadic_function() {
    let decls = parse("int printf(const char *fmt, ...);");
    assert_eq!(decls.len(), 1);
}

#[test]
fn test_long_long() {
    let decls = parse("long long x;");
    assert_eq!(decls.len(), 1);

    if let ExternalDecl::Declaration(d) = &decls[0] {
        let long_count = d.specs.type_specs.iter().filter(|t| matches!(t, TypeSpec::Long)).count();
        assert_eq!(long_count, 2);
    } else {
        panic!("Expected declaration");
    }
}

#[test]
fn test_unsigned_int() {
    let decls = parse("unsigned int x;");
    assert_eq!(decls.len(), 1);
    assert!(has_type_spec(&decls[0], |t| matches!(t, TypeSpec::Unsigned)));
    assert!(has_type_spec(&decls[0], |t| matches!(t, TypeSpec::Int)));
}

#[test]
fn test_short_int() {
    let decls = parse("short int x;");
    assert_eq!(decls.len(), 1);
    assert!(has_type_spec(&decls[0], |t| matches!(t, TypeSpec::Short)));
}

#[test]
fn test_initializer() {
    let decls = parse("int x = 42;");
    assert_eq!(decls.len(), 1);

    if let ExternalDecl::Declaration(d) = &decls[0] {
        assert!(d.declarators[0].init.is_some());
    } else {
        panic!("Expected declaration");
    }
}

#[test]
fn test_array_initializer() {
    let decls = parse("int arr[] = {1, 2, 3};");
    assert_eq!(decls.len(), 1);

    if let ExternalDecl::Declaration(d) = &decls[0] {
        assert!(d.declarators[0].init.is_some());
    } else {
        panic!("Expected declaration");
    }
}

#[test]
fn test_struct_initializer() {
    let decls = parse("struct Point p = {1, 2};");
    assert_eq!(decls.len(), 1);
}

#[test]
fn test_if_statement() {
    let decls = parse("void foo(void) { if (1) { } }");
    assert_eq!(decls.len(), 1);
}

#[test]
fn test_if_else_statement() {
    let decls = parse("void foo(void) { if (1) { } else { } }");
    assert_eq!(decls.len(), 1);
}

#[test]
fn test_while_statement() {
    let decls = parse("void foo(void) { while (1) { } }");
    assert_eq!(decls.len(), 1);
}

#[test]
fn test_for_statement() {
    let decls = parse("void foo(void) { for (int i = 0; i < 10; i++) { } }");
    assert_eq!(decls.len(), 1);
}

#[test]
fn test_do_while_statement() {
    let decls = parse("void foo(void) { do { } while (1); }");
    assert_eq!(decls.len(), 1);
}

#[test]
fn test_switch_statement() {
    let decls = parse("void foo(int x) { switch (x) { case 1: break; default: break; } }");
    assert_eq!(decls.len(), 1);
}

#[test]
fn test_return_statement() {
    let decls = parse("int foo(void) { return 42; }");
    assert_eq!(decls.len(), 1);
}

#[test]
fn test_break_continue() {
    let decls = parse("void foo(void) { while(1) { break; continue; } }");
    assert_eq!(decls.len(), 1);
}

#[test]
fn test_goto_label() {
    let decls = parse("void foo(void) { start: goto start; }");
    assert_eq!(decls.len(), 1);
}

#[test]
fn test_attribute_on_function() {
    let decls = parse("void foo(void) __attribute__((unused));");
    assert_eq!(decls.len(), 1);
}

#[test]
fn test_attribute_on_parameter() {
    let decls = parse("void foo(int x __attribute__((unused)));");
    assert_eq!(decls.len(), 1);
}

#[test]
fn test_attribute_on_variable() {
    let decls = parse("int x __attribute__((unused));");
    assert_eq!(decls.len(), 1);
}

#[test]
fn test_attribute_on_struct() {
    let decls = parse("struct __attribute__((packed)) S { int x; };");
    assert_eq!(decls.len(), 1);
}

#[test]
fn test_typeof() {
    let decls = parse("__typeof__(1 + 2) x;");
    assert_eq!(decls.len(), 1);
}

#[test]
fn test_statement_expression() {
    let decls = parse("int x = ({ int y = 1; y + 1; });");
    assert_eq!(decls.len(), 1);
}

#[test]
fn test_bool_type() {
    let decls = parse("_Bool b;");
    assert_eq!(decls.len(), 1);
    assert!(has_type_spec(&decls[0], |t| matches!(t, TypeSpec::Bool)));
}

#[test]
fn test_complex_type() {
    let decls = parse("_Complex double z;");
    assert_eq!(decls.len(), 1);
    assert!(has_type_spec(&decls[0], |t| matches!(t, TypeSpec::Complex)));
}

#[test]
fn test_int128() {
    let decls = parse("__int128 big;");
    assert_eq!(decls.len(), 1);
    assert!(has_type_spec(&decls[0], |t| matches!(t, TypeSpec::Int128)));
}

#[test]
fn test_extension_keyword() {
    let decls = parse("__extension__ int x;");
    assert_eq!(decls.len(), 1);
}

#[test]
fn test_asm_in_function() {
    let decls = parse("void foo(void) { __asm__(\"nop\"); }");
    assert_eq!(decls.len(), 1);
}

#[test]
fn test_local_variable_with_attribute() {
    let decls = parse("void foo(void) { int x __attribute__((unused)) = 1; }");
    assert_eq!(decls.len(), 1);
}
