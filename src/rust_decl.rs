//! Rust宣言パーサー
//!
//! bindgenが生成したRustコードから宣言を抽出する。
//! syn crateを使用して正確にパースする。

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::Path;

use syn::{Item, Type, FnArg, Pat, ReturnType, Fields, Visibility};
use quote::ToTokens;

/// Rust定数
#[derive(Debug, Clone)]
pub struct RustConst {
    pub name: String,
    pub ty: String,
}

/// Rust関数パラメータ
#[derive(Debug, Clone)]
pub struct RustParam {
    pub name: String,
    pub ty: String,
}

/// Rust関数
#[derive(Debug, Clone)]
pub struct RustFn {
    pub name: String,
    pub params: Vec<RustParam>,
    pub ret_ty: Option<String>,
}

/// Rust構造体フィールド
#[derive(Debug, Clone)]
pub struct RustField {
    pub name: String,
    pub ty: String,
}

/// Rust構造体
#[derive(Debug, Clone)]
pub struct RustStruct {
    pub name: String,
    pub fields: Vec<RustField>,
}

/// Rust型エイリアス
#[derive(Debug, Clone)]
pub struct RustTypeAlias {
    pub name: String,
    pub ty: String,
}

/// Rust宣言辞書
#[derive(Debug, Default)]
pub struct RustDeclDict {
    pub consts: HashMap<String, RustConst>,
    pub fns: HashMap<String, RustFn>,
    pub structs: HashMap<String, RustStruct>,
    pub types: HashMap<String, RustTypeAlias>,
    pub enums: HashSet<String>,
}

impl RustDeclDict {
    /// 新しい辞書を作成
    pub fn new() -> Self {
        Self::default()
    }

    /// ファイルからパース
    pub fn parse_file<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let content = fs::read_to_string(path)?;
        Ok(Self::parse(&content))
    }

    /// 文字列からパース
    pub fn parse(content: &str) -> Self {
        let mut dict = Self::new();

        // synでパース
        let file = match syn::parse_file(content) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("Warning: Failed to parse Rust file: {}", e);
                return dict;
            }
        };

        // 各アイテムを処理
        for item in file.items {
            dict.process_item(&item);
        }

        dict
    }

    /// アイテムを処理
    fn process_item(&mut self, item: &Item) {
        match item {
            Item::Const(item_const) => {
                if Self::is_pub(&item_const.vis) {
                    let name = item_const.ident.to_string();
                    let ty = Self::type_to_string(&item_const.ty);
                    self.consts.insert(name.clone(), RustConst { name, ty });
                }
            }
            Item::Type(item_type) => {
                if Self::is_pub(&item_type.vis) {
                    let name = item_type.ident.to_string();
                    let ty = Self::type_to_string(&item_type.ty);
                    self.types.insert(name.clone(), RustTypeAlias { name, ty });
                }
            }
            Item::Struct(item_struct) => {
                if Self::is_pub(&item_struct.vis) {
                    let name = item_struct.ident.to_string();
                    let fields = Self::extract_fields(&item_struct.fields);
                    self.structs.insert(name.clone(), RustStruct { name, fields });
                }
            }
            Item::Fn(item_fn) => {
                if Self::is_pub(&item_fn.vis) {
                    if let Some(rust_fn) = Self::extract_fn(&item_fn.sig) {
                        self.fns.insert(rust_fn.name.clone(), rust_fn);
                    }
                }
            }
            Item::ForeignMod(foreign_mod) => {
                // extern "C" { ... } ブロック内の関数
                for foreign_item in &foreign_mod.items {
                    if let syn::ForeignItem::Fn(fn_item) = foreign_item {
                        if Self::is_pub(&fn_item.vis) {
                            if let Some(rust_fn) = Self::extract_fn(&fn_item.sig) {
                                self.fns.insert(rust_fn.name.clone(), rust_fn);
                            }
                        }
                    }
                }
            }
            Item::Enum(item_enum) => {
                if Self::is_pub(&item_enum.vis) {
                    self.enums.insert(item_enum.ident.to_string());
                }
            }
            Item::Impl(_) => {
                // impl ブロック内のpubメソッドも収集可能（必要なら）
                // 今回はスキップ
            }
            _ => {}
        }
    }

    /// 可視性がpubかどうか
    fn is_pub(vis: &Visibility) -> bool {
        matches!(vis, Visibility::Public(_))
    }

    /// 型を文字列に変換
    fn type_to_string(ty: &Type) -> String {
        ty.to_token_stream().to_string()
    }

    /// 構造体フィールドを抽出
    fn extract_fields(fields: &Fields) -> Vec<RustField> {
        let mut result = Vec::new();

        match fields {
            Fields::Named(named) => {
                for field in &named.named {
                    if Self::is_pub(&field.vis) {
                        if let Some(ident) = &field.ident {
                            result.push(RustField {
                                name: ident.to_string(),
                                ty: Self::type_to_string(&field.ty),
                            });
                        }
                    }
                }
            }
            Fields::Unnamed(unnamed) => {
                for (i, field) in unnamed.unnamed.iter().enumerate() {
                    if Self::is_pub(&field.vis) {
                        result.push(RustField {
                            name: format!("{}", i),
                            ty: Self::type_to_string(&field.ty),
                        });
                    }
                }
            }
            Fields::Unit => {}
        }

        result
    }

    /// 関数シグネチャを抽出
    fn extract_fn(sig: &syn::Signature) -> Option<RustFn> {
        let name = sig.ident.to_string();

        let mut params = Vec::new();
        for arg in &sig.inputs {
            match arg {
                FnArg::Receiver(_) => {
                    // self, &self, &mut self はスキップ
                }
                FnArg::Typed(pat_type) => {
                    let param_name = match pat_type.pat.as_ref() {
                        Pat::Ident(pat_ident) => pat_ident.ident.to_string(),
                        _ => "_".to_string(),
                    };
                    let param_ty = Self::type_to_string(&pat_type.ty);
                    params.push(RustParam {
                        name: param_name,
                        ty: param_ty,
                    });
                }
            }
        }

        let ret_ty = match &sig.output {
            ReturnType::Default => None,
            ReturnType::Type(_, ty) => Some(Self::type_to_string(ty)),
        };

        Some(RustFn {
            name,
            params,
            ret_ty,
        })
    }

    /// 統計情報を取得
    pub fn stats(&self) -> RustDeclStats {
        RustDeclStats {
            const_count: self.consts.len(),
            fn_count: self.fns.len(),
            struct_count: self.structs.len(),
            type_count: self.types.len(),
        }
    }

    /// THX依存関数の名前を取得
    ///
    /// 第一引数が *mut PerlInterpreter を含む関数を返す
    pub fn thx_functions(&self) -> std::collections::HashSet<String> {
        self.fns.iter()
            .filter(|(_, f)| {
                f.params.first()
                    .map(|p| p.ty.contains("PerlInterpreter"))
                    .unwrap_or(false)
            })
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// 辞書をダンプ
    pub fn dump(&self) -> String {
        let mut result = String::new();

        result.push_str("=== Constants ===\n");
        let mut consts: Vec<_> = self.consts.values().collect();
        consts.sort_by_key(|c| &c.name);
        for c in consts {
            result.push_str(&format!("  {}: {}\n", c.name, c.ty));
        }

        result.push_str("\n=== Type Aliases ===\n");
        let mut types: Vec<_> = self.types.values().collect();
        types.sort_by_key(|t| &t.name);
        for t in types {
            result.push_str(&format!("  {} = {}\n", t.name, t.ty));
        }

        result.push_str("\n=== Functions ===\n");
        let mut fns: Vec<_> = self.fns.values().collect();
        fns.sort_by_key(|f| &f.name);
        for f in fns {
            let params: Vec<_> = f.params.iter().map(|p| format!("{}: {}", p.name, p.ty)).collect();
            let ret = f.ret_ty.as_deref().unwrap_or("()");
            result.push_str(&format!("  fn {}({}) -> {}\n", f.name, params.join(", "), ret));
        }

        result.push_str("\n=== Structs ===\n");
        let mut structs: Vec<_> = self.structs.values().collect();
        structs.sort_by_key(|s| &s.name);
        for s in structs {
            result.push_str(&format!("  struct {} {{\n", s.name));
            for f in &s.fields {
                result.push_str(&format!("    {}: {},\n", f.name, f.ty));
            }
            result.push_str("  }\n");
        }

        result
    }

    /// 名前で定数を検索
    pub fn lookup_const(&self, name: &str) -> Option<&RustConst> {
        self.consts.get(name)
    }

    /// 名前で関数を検索
    pub fn lookup_fn(&self, name: &str) -> Option<&RustFn> {
        self.fns.get(name)
    }

    /// 名前で構造体を検索
    pub fn lookup_struct(&self, name: &str) -> Option<&RustStruct> {
        self.structs.get(name)
    }

    /// 名前で型エイリアスを検索
    pub fn lookup_type(&self, name: &str) -> Option<&RustTypeAlias> {
        self.types.get(name)
    }
}

/// 統計情報
#[derive(Debug)]
pub struct RustDeclStats {
    pub const_count: usize,
    pub fn_count: usize,
    pub struct_count: usize,
    pub type_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_const() {
        let dict = RustDeclDict::parse("pub const FOO: u32 = 42;");
        assert_eq!(dict.consts.len(), 1);
        let c = dict.consts.get("FOO").unwrap();
        assert_eq!(c.name, "FOO");
        assert_eq!(c.ty, "u32");
    }

    #[test]
    fn test_parse_const_array() {
        let dict = RustDeclDict::parse("pub const MSG: &[u8; 10] = b\"(unknown)\\0\";");
        let c = dict.consts.get("MSG").unwrap();
        assert_eq!(c.name, "MSG");
        assert_eq!(c.ty, "& [u8 ; 10]");
    }

    #[test]
    fn test_parse_type_alias() {
        let dict = RustDeclDict::parse("pub type Size = ::std::os::raw::c_ulong;");
        let t = dict.types.get("Size").unwrap();
        assert_eq!(t.name, "Size");
        assert_eq!(t.ty, ":: std :: os :: raw :: c_ulong");
    }

    #[test]
    fn test_parse_fn() {
        let dict = RustDeclDict::parse(r#"
            extern "C" {
                pub fn foo(x: i32, y: *mut u8) -> bool;
            }
        "#);
        let f = dict.fns.get("foo").unwrap();
        assert_eq!(f.name, "foo");
        assert_eq!(f.params.len(), 2);
        assert_eq!(f.params[0].name, "x");
        assert_eq!(f.params[0].ty, "i32");
        assert_eq!(f.params[1].name, "y");
        assert_eq!(f.params[1].ty, "* mut u8");
        assert_eq!(f.ret_ty, Some("bool".to_string()));
    }

    #[test]
    fn test_parse_fn_no_return() {
        let dict = RustDeclDict::parse(r#"
            extern "C" {
                pub fn bar(x: i32);
            }
        "#);
        let f = dict.fns.get("bar").unwrap();
        assert_eq!(f.name, "bar");
        assert_eq!(f.ret_ty, None);
    }

    #[test]
    fn test_parse_struct() {
        let content = r#"
pub struct Point {
    pub x: i32,
    pub y: i32,
}
"#;
        let dict = RustDeclDict::parse(content);
        let s = dict.structs.get("Point").unwrap();
        assert_eq!(s.name, "Point");
        assert_eq!(s.fields.len(), 2);
        assert_eq!(s.fields[0].name, "x");
        assert_eq!(s.fields[0].ty, "i32");
    }

    #[test]
    fn test_parse_struct_with_option() {
        let content = r#"
pub struct Test {
    pub callback: ::std::option::Option<
        unsafe extern "C" fn(x: i32) -> i32,
    >,
}
"#;
        let dict = RustDeclDict::parse(content);
        let s = dict.structs.get("Test").unwrap();
        assert_eq!(s.name, "Test");
        assert_eq!(s.fields.len(), 1);
        assert_eq!(s.fields[0].name, "callback");
        // synのto_token_stream()は型を正しくパースする
        assert!(s.fields[0].ty.contains("Option"));
        assert!(s.fields[0].ty.contains("fn"));
        assert!(s.fields[0].ty.ends_with(">"), "Type should end with >");
    }

    #[test]
    fn test_parse_struct_with_generics() {
        let content = r#"
pub struct Wrapper<T> {
    pub value: T,
}
"#;
        let dict = RustDeclDict::parse(content);
        let s = dict.structs.get("Wrapper").unwrap();
        assert_eq!(s.name, "Wrapper");
    }
}
