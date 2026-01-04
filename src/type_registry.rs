//! 型レジストリモジュール
//!
//! bindings.rs とマクロ定義から型エイリアス情報を収集し、
//! 型の解決と比較を行う。

use std::collections::{HashMap, HashSet};

use crate::intern::StringInterner;
use crate::macro_def::MacroTable;
use crate::rust_decl::RustDeclDict;
use crate::token::TokenKind;
use crate::unified_type::UnifiedType;

/// 型の同等性レベル
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TypeEquality {
    /// 完全一致（正規化後）
    Exact,
    /// const/mut の差のみ
    ConstMutDiff,
    /// 大文字小文字の差のみ
    CaseDiff,
    /// 互換性なし
    Incompatible,
}

impl TypeEquality {
    /// 互換性があるかどうか（Incompatible 以外）
    pub fn is_compatible(&self) -> bool {
        *self != TypeEquality::Incompatible
    }

    /// 完全一致かどうか
    pub fn is_exact(&self) -> bool {
        *self == TypeEquality::Exact
    }
}

/// 型レジストリ
///
/// bindings.rs とマクロ定義から型情報を収集し、
/// 型の解決と比較を行う。
pub struct TypeRegistry {
    /// bindings.rs の型エイリアス (STRLEN -> usize など)
    rust_aliases: HashMap<String, UnifiedType>,
    /// bindings.rs の構造体名
    rust_structs: HashSet<String>,
    /// マクロ定義の型エイリアス (Size_t -> size_t など)
    macro_aliases: HashMap<String, String>,
}

impl TypeRegistry {
    /// 新しい空のレジストリを作成
    pub fn new() -> Self {
        Self {
            rust_aliases: HashMap::new(),
            rust_structs: HashSet::new(),
            macro_aliases: HashMap::new(),
        }
    }

    /// RustDeclDict とマクロテーブルからレジストリを構築
    pub fn from_sources(
        rust_decls: &RustDeclDict,
        macros: &MacroTable,
        interner: &StringInterner,
    ) -> Self {
        let mut registry = Self::new();

        // bindings.rs の型エイリアスを収集
        for (name, alias) in &rust_decls.types {
            let ty = UnifiedType::from_rust_str(&alias.ty);
            registry.rust_aliases.insert(name.clone(), ty);
        }

        // bindings.rs の構造体名を収集
        for name in rust_decls.structs.keys() {
            registry.rust_structs.insert(name.clone());
        }

        // マクロ定義から型エイリアスを抽出
        registry.extract_type_aliases_from_macros(macros, interner);

        registry
    }

    /// マクロ定義から型エイリアスを抽出
    ///
    /// `#define Size_t size_t` のようなマクロを型エイリアスとして解釈する
    fn extract_type_aliases_from_macros(
        &mut self,
        macros: &MacroTable,
        interner: &StringInterner,
    ) {
        for (name_id, def) in macros.iter() {
            // オブジェクトマクロのみ対象
            if def.is_function() {
                continue;
            }

            // 本体が単一トークンの場合、型エイリアスと見なす
            if def.body.len() == 1 {
                let name = interner.get(*name_id);

                // 型名らしいもののみ（大文字で始まる or _t で終わる）
                if !name.chars().next().map(|c| c.is_uppercase()).unwrap_or(false)
                    && !name.ends_with("_t")
                {
                    continue;
                }

                // 識別子または型キーワードを抽出
                let base_name: Option<String> = match def.body[0].kind {
                    TokenKind::Ident(base_id) => Some(interner.get(base_id).to_string()),
                    // 基本型キーワード
                    TokenKind::KwChar => Some("char".to_string()),
                    TokenKind::KwInt => Some("int".to_string()),
                    TokenKind::KwShort => Some("short".to_string()),
                    TokenKind::KwLong => Some("long".to_string()),
                    TokenKind::KwFloat => Some("float".to_string()),
                    TokenKind::KwDouble => Some("double".to_string()),
                    TokenKind::KwVoid => Some("void".to_string()),
                    TokenKind::KwBool => Some("bool".to_string()),
                    TokenKind::KwInt128 => Some("__int128".to_string()),
                    _ => None,
                };

                if let Some(base) = base_name {
                    self.macro_aliases.insert(name.to_string(), base);
                }
            }
        }
    }

    /// 型名が bindings.rs に存在するか（型エイリアスまたは構造体）
    pub fn exists_in_bindings(&self, name: &str) -> bool {
        self.rust_aliases.contains_key(name) || self.rust_structs.contains(name)
    }

    /// 型エイリアスを解決
    ///
    /// Named 型の名前を再帰的に解決し、最終的な型を返す
    pub fn resolve(&self, ty: &UnifiedType) -> UnifiedType {
        match ty {
            UnifiedType::Named(name) => {
                // まず bindings.rs のエイリアスをチェック
                if let Some(resolved) = self.rust_aliases.get(name) {
                    return self.resolve(resolved);
                }

                // マクロ定義の型エイリアスをチェック
                if let Some(base_name) = self.macro_aliases.get(name) {
                    let base_ty = UnifiedType::Named(base_name.clone());
                    return self.resolve(&base_ty);
                }

                // 標準的な型変換
                match name.as_str() {
                    "size_t" => UnifiedType::Int {
                        signed: false,
                        size: crate::unified_type::IntSize::Long,
                    },
                    "ssize_t" | "ptrdiff_t" => UnifiedType::Int {
                        signed: true,
                        size: crate::unified_type::IntSize::Long,
                    },
                    "off_t" | "off64_t" => UnifiedType::Int {
                        signed: true,
                        size: crate::unified_type::IntSize::LongLong,
                    },
                    _ => ty.clone(),
                }
            }

            UnifiedType::Pointer { inner, is_const } => UnifiedType::Pointer {
                inner: Box::new(self.resolve(inner)),
                is_const: *is_const,
            },

            UnifiedType::Array { inner, size } => UnifiedType::Array {
                inner: Box::new(self.resolve(inner)),
                size: *size,
            },

            _ => ty.clone(),
        }
    }

    /// 2つの型を比較
    ///
    /// エイリアスを解決した後、同等性レベルを返す
    pub fn compare(&self, a: &UnifiedType, b: &UnifiedType) -> TypeEquality {
        // 1. エイリアス解決
        let a_resolved = self.resolve(a);
        let b_resolved = self.resolve(b);

        // 2. 完全一致
        if a_resolved == b_resolved {
            return TypeEquality::Exact;
        }

        // 3. const/mut 無視で比較
        if a_resolved.equals_ignoring_const(&b_resolved) {
            return TypeEquality::ConstMutDiff;
        }

        // 4. 大文字小文字無視で比較
        if a_resolved.equals_ignoring_case(&b_resolved) {
            return TypeEquality::CaseDiff;
        }

        TypeEquality::Incompatible
    }

    /// C型文字列とRust型文字列を比較
    ///
    /// 便利メソッド: 文字列をパースしてから比較
    pub fn compare_c_rust(&self, c_type: &str, rust_type: &str) -> TypeEquality {
        let c_ty = UnifiedType::from_c_str(c_type);
        let rust_ty = UnifiedType::from_rust_str(rust_type);
        self.compare(&c_ty, &rust_ty)
    }

    /// 型が Rust コード生成で使用可能か
    ///
    /// bindings.rs に存在するか、基本型かをチェック
    pub fn is_usable_in_rust(&self, ty: &UnifiedType) -> bool {
        match ty {
            UnifiedType::Void
            | UnifiedType::Bool
            | UnifiedType::Char { .. }
            | UnifiedType::Int { .. }
            | UnifiedType::Float
            | UnifiedType::Double
            | UnifiedType::LongDouble => true,

            UnifiedType::Named(name) => self.exists_in_bindings(name),

            UnifiedType::Pointer { inner, .. } => self.is_usable_in_rust(inner),

            UnifiedType::Array { inner, .. } => self.is_usable_in_rust(inner),

            UnifiedType::Unknown => false,
        }
    }

    /// 解決パスを取得（デバッグ用）
    ///
    /// 型がどのようにエイリアス解決されたかのパスを返す
    pub fn resolution_path(&self, ty: &UnifiedType) -> Vec<String> {
        let mut path = Vec::new();
        self.collect_resolution_path(ty, &mut path);
        path
    }

    fn collect_resolution_path(&self, ty: &UnifiedType, path: &mut Vec<String>) {
        match ty {
            UnifiedType::Named(name) => {
                path.push(name.clone());

                // bindings.rs のエイリアスをチェック
                if let Some(resolved) = self.rust_aliases.get(name) {
                    self.collect_resolution_path(resolved, path);
                    return;
                }

                // マクロ定義の型エイリアスをチェック
                if let Some(base_name) = self.macro_aliases.get(name) {
                    let base_ty = UnifiedType::Named(base_name.clone());
                    self.collect_resolution_path(&base_ty, path);
                }
            }
            _ => {
                path.push(ty.to_rust_string());
            }
        }
    }

    /// 統計情報を取得
    pub fn stats(&self) -> TypeRegistryStats {
        TypeRegistryStats {
            rust_alias_count: self.rust_aliases.len(),
            rust_struct_count: self.rust_structs.len(),
            macro_alias_count: self.macro_aliases.len(),
        }
    }

    /// マクロエイリアスを取得（デバッグ用）
    pub fn get_macro_alias(&self, name: &str) -> Option<&str> {
        self.macro_aliases.get(name).map(|s| s.as_str())
    }

    /// 全マクロエイリアスのイテレータ（デバッグ用）
    pub fn macro_aliases_iter(&self) -> impl Iterator<Item = (&String, &String)> {
        self.macro_aliases.iter()
    }
}

impl Default for TypeRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// 型レジストリの統計情報
#[derive(Debug, Clone)]
pub struct TypeRegistryStats {
    pub rust_alias_count: usize,
    pub rust_struct_count: usize,
    pub macro_alias_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_registry() -> TypeRegistry {
        let mut registry = TypeRegistry::new();

        // STRLEN -> usize のエイリアスを追加
        registry.rust_aliases.insert(
            "STRLEN".to_string(),
            UnifiedType::Int {
                signed: false,
                size: crate::unified_type::IntSize::Long,
            },
        );

        // SV 構造体を追加
        registry.rust_structs.insert("SV".to_string());
        registry.rust_structs.insert("AV".to_string());
        registry.rust_structs.insert("HV".to_string());

        // Size_t -> size_t のマクロエイリアスを追加
        registry
            .macro_aliases
            .insert("Size_t".to_string(), "size_t".to_string());

        registry
    }

    #[test]
    fn test_exists_in_bindings() {
        let registry = create_test_registry();

        assert!(registry.exists_in_bindings("STRLEN"));
        assert!(registry.exists_in_bindings("SV"));
        assert!(!registry.exists_in_bindings("UnknownType"));
    }

    #[test]
    fn test_resolve_alias() {
        let registry = create_test_registry();

        // STRLEN -> usize
        let strlen = UnifiedType::Named("STRLEN".to_string());
        let resolved = registry.resolve(&strlen);
        assert_eq!(
            resolved,
            UnifiedType::Int {
                signed: false,
                size: crate::unified_type::IntSize::Long
            }
        );

        // Size_t -> size_t -> usize
        let size_t = UnifiedType::Named("Size_t".to_string());
        let resolved = registry.resolve(&size_t);
        assert_eq!(
            resolved,
            UnifiedType::Int {
                signed: false,
                size: crate::unified_type::IntSize::Long
            }
        );
    }

    #[test]
    fn test_resolve_pointer() {
        let registry = create_test_registry();

        // *mut STRLEN -> *mut usize
        let ptr_strlen = UnifiedType::Pointer {
            inner: Box::new(UnifiedType::Named("STRLEN".to_string())),
            is_const: false,
        };
        let resolved = registry.resolve(&ptr_strlen);
        assert_eq!(
            resolved,
            UnifiedType::Pointer {
                inner: Box::new(UnifiedType::Int {
                    signed: false,
                    size: crate::unified_type::IntSize::Long
                }),
                is_const: false,
            }
        );
    }

    #[test]
    fn test_compare_exact() {
        let registry = create_test_registry();

        // STRLEN vs usize (エイリアス解決後に一致)
        let strlen = UnifiedType::Named("STRLEN".to_string());
        let usize_ty = UnifiedType::Int {
            signed: false,
            size: crate::unified_type::IntSize::Long,
        };

        assert_eq!(registry.compare(&strlen, &usize_ty), TypeEquality::Exact);
    }

    #[test]
    fn test_compare_const_mut_diff() {
        let registry = create_test_registry();

        let mut_sv = UnifiedType::Pointer {
            inner: Box::new(UnifiedType::Named("SV".to_string())),
            is_const: false,
        };
        let const_sv = UnifiedType::Pointer {
            inner: Box::new(UnifiedType::Named("SV".to_string())),
            is_const: true,
        };

        assert_eq!(
            registry.compare(&mut_sv, &const_sv),
            TypeEquality::ConstMutDiff
        );
    }

    #[test]
    fn test_compare_case_diff() {
        let registry = create_test_registry();

        let sv_upper = UnifiedType::Named("SV".to_string());
        let sv_lower = UnifiedType::Named("sv".to_string());

        assert_eq!(registry.compare(&sv_upper, &sv_lower), TypeEquality::CaseDiff);
    }

    #[test]
    fn test_compare_incompatible() {
        let registry = create_test_registry();

        let sv = UnifiedType::Named("SV".to_string());
        let av = UnifiedType::Named("AV".to_string());

        assert_eq!(registry.compare(&sv, &av), TypeEquality::Incompatible);
    }

    #[test]
    fn test_compare_c_rust() {
        let registry = create_test_registry();

        // "SV *" vs "*mut SV" -> Exact
        assert_eq!(
            registry.compare_c_rust("SV *", "*mut SV"),
            TypeEquality::Exact
        );

        // "const SV *" vs "*mut SV" -> ConstMutDiff
        assert_eq!(
            registry.compare_c_rust("const SV *", "*mut SV"),
            TypeEquality::ConstMutDiff
        );
    }

    #[test]
    fn test_is_usable_in_rust() {
        let registry = create_test_registry();

        // 基本型は使用可能
        assert!(registry.is_usable_in_rust(&UnifiedType::Int {
            signed: true,
            size: crate::unified_type::IntSize::Int
        }));

        // bindings.rs に存在する型は使用可能
        assert!(registry.is_usable_in_rust(&UnifiedType::Named("SV".to_string())));

        // 存在しない型は使用不可
        assert!(!registry.is_usable_in_rust(&UnifiedType::Named("Unknown".to_string())));

        // ポインタの内部型もチェック
        assert!(registry.is_usable_in_rust(&UnifiedType::Pointer {
            inner: Box::new(UnifiedType::Named("SV".to_string())),
            is_const: false,
        }));
    }

    #[test]
    fn test_resolution_path() {
        let registry = create_test_registry();

        // Size_t -> size_t -> usize
        let size_t = UnifiedType::Named("Size_t".to_string());
        let path = registry.resolution_path(&size_t);

        assert!(path.contains(&"Size_t".to_string()));
        assert!(path.contains(&"size_t".to_string()));
    }
}
