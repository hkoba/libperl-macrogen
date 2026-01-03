//! 統一型表現モジュール
//!
//! C型とRust型を統一的に表現し、変換・比較を行う。

use std::fmt;

/// 整数サイズ
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntSize {
    /// char (8-bit)
    Char,
    /// short (16-bit)
    Short,
    /// int (32-bit)
    Int,
    /// long (platform-dependent)
    Long,
    /// long long (64-bit)
    LongLong,
    /// __int128 (128-bit)
    Int128,
}

/// 型の出自情報
#[derive(Debug, Clone)]
pub enum TypeSource {
    /// apidoc の C型
    Apidoc { raw: String, entry_name: String },
    /// bindings.rs の Rust型
    Bindings { raw: String },
    /// マクロ本体からの推論
    Inferred,
    /// AST からのパース
    Parsed,
}

/// 統一型表現
///
/// C型とRust型の両方を表現できる構造化された型。
/// パース時に正規化され、以降は構造的な操作が可能。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum UnifiedType {
    /// void / ()
    Void,
    /// bool / _Bool
    Bool,
    /// char (signed は None = plain char, Some(true) = signed, Some(false) = unsigned)
    Char { signed: Option<bool> },
    /// 整数型
    Int { signed: bool, size: IntSize },
    /// float
    Float,
    /// double
    Double,
    /// long double
    LongDouble,

    /// ポインタ型
    Pointer {
        inner: Box<UnifiedType>,
        is_const: bool,
    },

    /// 配列型
    Array {
        inner: Box<UnifiedType>,
        size: Option<usize>,
    },

    /// 名前付き型 (typedef, struct, union, enum)
    Named(String),

    /// 不明な型
    Unknown,
}

/// 出自情報付き型
#[derive(Debug, Clone)]
pub struct SourcedType {
    pub ty: UnifiedType,
    pub source: TypeSource,
}

impl UnifiedType {
    /// C型文字列からパース
    ///
    /// 例:
    /// - `"SV *"` → `Pointer { inner: Named("SV"), is_const: false }`
    /// - `"const char *"` → `Pointer { inner: Char { signed: None }, is_const: true }`
    /// - `"int"` → `Int { signed: true, size: IntSize::Int }`
    pub fn from_c_str(s: &str) -> Self {
        let trimmed = s.trim();

        if trimmed.is_empty() {
            return Self::Unknown;
        }

        // void
        if trimmed == "void" {
            return Self::Void;
        }

        // bool
        if trimmed == "bool" || trimmed == "_Bool" {
            return Self::Bool;
        }

        // ポインタ型の処理
        if let Some(ptr_type) = Self::parse_c_pointer(trimmed) {
            return ptr_type;
        }

        // 基本型の変換
        Self::parse_c_basic_type(trimmed)
    }

    /// C言語のポインタ型をパース
    fn parse_c_pointer(s: &str) -> Option<Self> {
        let s = s.trim();

        // 末尾の * を探す
        if let Some(star_pos) = s.rfind('*') {
            let before_star = s[..star_pos].trim();
            let after_star = s[star_pos + 1..].trim();

            // after_star が "const" の場合は無視（ポインタ自体のconst）
            let _ptr_const = after_star == "const";

            // before_star から型を解析
            // "const char" -> is_const=true, base="char"
            // "SV" -> is_const=false, base="SV"
            let (is_const, base_type) = if before_star.starts_with("const ") {
                (true, before_star[6..].trim())
            } else if before_star.ends_with(" const") {
                (true, before_star[..before_star.len() - 6].trim())
            } else {
                (false, before_star)
            };

            // 再帰的にポインタをチェック（ダブルポインタなど）
            let inner_type = if base_type.contains('*') {
                Self::parse_c_pointer(base_type).unwrap_or_else(|| Self::parse_c_basic_type(base_type))
            } else {
                Self::parse_c_basic_type(base_type)
            };

            return Some(Self::Pointer {
                inner: Box::new(inner_type),
                is_const,
            });
        }

        None
    }

    /// C言語の基本型をパース
    fn parse_c_basic_type(s: &str) -> Self {
        let s = s.trim();

        // unsigned/signed の処理
        let (is_unsigned, base) = if s.starts_with("unsigned ") {
            (true, s[9..].trim())
        } else if s.starts_with("signed ") {
            (false, s[7..].trim())
        } else {
            (false, s)
        };

        // 型名のマッピング
        match base {
            // char 系
            "char" if is_unsigned => Self::Char { signed: Some(false) },
            "char" => Self::Char { signed: None },

            // short 系
            "short" | "short int" => Self::Int {
                signed: !is_unsigned,
                size: IntSize::Short,
            },

            // int 系
            "int" | "" => Self::Int {
                signed: !is_unsigned,
                size: IntSize::Int,
            },

            // long 系
            "long" | "long int" => Self::Int {
                signed: !is_unsigned,
                size: IntSize::Long,
            },

            // long long 系
            "long long" | "long long int" => Self::Int {
                signed: !is_unsigned,
                size: IntSize::LongLong,
            },

            // __int128
            "__int128" | "__int128_t" => Self::Int {
                signed: !is_unsigned,
                size: IntSize::Int128,
            },

            // 浮動小数点
            "float" => Self::Float,
            "double" => Self::Double,
            "long double" => Self::LongDouble,

            // void
            "void" => Self::Void,

            // bool
            "bool" | "_Bool" => Self::Bool,

            // size_t, ssize_t などの標準型
            "size_t" => Self::Int {
                signed: false,
                size: IntSize::Long,
            },
            "ssize_t" | "ptrdiff_t" => Self::Int {
                signed: true,
                size: IntSize::Long,
            },

            // その他は名前付き型
            _ => {
                if is_unsigned {
                    // "unsigned SomeType" のような場合
                    Self::Named(format!("unsigned {}", base))
                } else {
                    Self::Named(base.to_string())
                }
            }
        }
    }

    /// Rust型文字列からパース
    ///
    /// 例:
    /// - `"*mut SV"` → `Pointer { inner: Named("SV"), is_const: false }`
    /// - `"*const c_char"` → `Pointer { inner: Char { signed: None }, is_const: true }`
    /// - `"c_int"` → `Int { signed: true, size: IntSize::Int }`
    pub fn from_rust_str(s: &str) -> Self {
        // synのto_token_stream().to_string()は "* mut" のようにスペースを入れるため正規化
        let normalized = s
            .replace("* mut", "*mut")
            .replace("* const", "*const");
        let trimmed = normalized.trim();

        if trimmed.is_empty() {
            return Self::Unknown;
        }

        // ポインタ型
        if let Some(rest) = trimmed.strip_prefix("*mut ") {
            return Self::Pointer {
                inner: Box::new(Self::from_rust_str(rest)),
                is_const: false,
            };
        }
        if let Some(rest) = trimmed.strip_prefix("*const ") {
            return Self::Pointer {
                inner: Box::new(Self::from_rust_str(rest)),
                is_const: true,
            };
        }
        // スペースなしのポインタ
        if let Some(rest) = trimmed.strip_prefix("*mut") {
            return Self::Pointer {
                inner: Box::new(Self::from_rust_str(rest.trim())),
                is_const: false,
            };
        }
        if let Some(rest) = trimmed.strip_prefix("*const") {
            return Self::Pointer {
                inner: Box::new(Self::from_rust_str(rest.trim())),
                is_const: true,
            };
        }

        // 基本型
        Self::parse_rust_basic_type(trimmed)
    }

    /// Rust の基本型をパース
    fn parse_rust_basic_type(s: &str) -> Self {
        match s {
            "()" => Self::Void,
            "c_void" => Self::Void,
            "bool" => Self::Bool,

            // char 系
            "c_char" => Self::Char { signed: None },
            "c_schar" => Self::Char { signed: Some(true) },
            "c_uchar" => Self::Char { signed: Some(false) },

            // 整数型
            "c_short" => Self::Int { signed: true, size: IntSize::Short },
            "c_ushort" => Self::Int { signed: false, size: IntSize::Short },
            "c_int" => Self::Int { signed: true, size: IntSize::Int },
            "c_uint" => Self::Int { signed: false, size: IntSize::Int },
            "c_long" => Self::Int { signed: true, size: IntSize::Long },
            "c_ulong" => Self::Int { signed: false, size: IntSize::Long },
            "c_longlong" => Self::Int { signed: true, size: IntSize::LongLong },
            "c_ulonglong" => Self::Int { signed: false, size: IntSize::LongLong },

            // Rust ネイティブ整数
            "i8" => Self::Char { signed: Some(true) },
            "u8" => Self::Char { signed: Some(false) },
            "i16" => Self::Int { signed: true, size: IntSize::Short },
            "u16" => Self::Int { signed: false, size: IntSize::Short },
            "i32" => Self::Int { signed: true, size: IntSize::Int },
            "u32" => Self::Int { signed: false, size: IntSize::Int },
            "i64" => Self::Int { signed: true, size: IntSize::LongLong },
            "u64" => Self::Int { signed: false, size: IntSize::LongLong },
            "i128" => Self::Int { signed: true, size: IntSize::Int128 },
            "u128" => Self::Int { signed: false, size: IntSize::Int128 },
            "isize" => Self::Int { signed: true, size: IntSize::Long },
            "usize" => Self::Int { signed: false, size: IntSize::Long },

            // 浮動小数点
            "c_float" | "f32" => Self::Float,
            "c_double" | "f64" => Self::Double,

            // その他は名前付き型
            _ => Self::Named(s.to_string()),
        }
    }

    /// Rust型文字列に変換
    pub fn to_rust_string(&self) -> String {
        match self {
            Self::Void => "()".to_string(),
            Self::Bool => "bool".to_string(),

            Self::Char { signed: None } => "c_char".to_string(),
            Self::Char { signed: Some(true) } => "c_schar".to_string(),
            Self::Char { signed: Some(false) } => "c_uchar".to_string(),

            Self::Int { signed, size } => {
                match (signed, size) {
                    (true, IntSize::Char) => "c_schar".to_string(),
                    (false, IntSize::Char) => "c_uchar".to_string(),
                    (true, IntSize::Short) => "c_short".to_string(),
                    (false, IntSize::Short) => "c_ushort".to_string(),
                    (true, IntSize::Int) => "c_int".to_string(),
                    (false, IntSize::Int) => "c_uint".to_string(),
                    (true, IntSize::Long) => "c_long".to_string(),
                    (false, IntSize::Long) => "c_ulong".to_string(),
                    (true, IntSize::LongLong) => "c_longlong".to_string(),
                    (false, IntSize::LongLong) => "c_ulonglong".to_string(),
                    (true, IntSize::Int128) => "i128".to_string(),
                    (false, IntSize::Int128) => "u128".to_string(),
                }
            }

            Self::Float => "c_float".to_string(),
            Self::Double => "c_double".to_string(),
            Self::LongDouble => "c_double".to_string(), // Rust には long double がない

            Self::Pointer { inner, is_const } => {
                let inner_str = inner.to_rust_string();
                if *is_const {
                    format!("*const {}", inner_str)
                } else {
                    format!("*mut {}", inner_str)
                }
            }

            Self::Array { inner, size } => {
                let inner_str = inner.to_rust_string();
                match size {
                    Some(n) => format!("[{}; {}]", inner_str, n),
                    None => format!("[{}]", inner_str),
                }
            }

            Self::Named(name) => name.clone(),

            Self::Unknown => "UnknownType".to_string(),
        }
    }

    /// const/mut を無視して比較
    pub fn equals_ignoring_const(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Void, Self::Void) => true,
            (Self::Bool, Self::Bool) => true,
            (Self::Char { signed: s1 }, Self::Char { signed: s2 }) => s1 == s2,
            (
                Self::Int { signed: s1, size: sz1 },
                Self::Int { signed: s2, size: sz2 },
            ) => s1 == s2 && sz1 == sz2,
            (Self::Float, Self::Float) => true,
            (Self::Double, Self::Double) => true,
            (Self::LongDouble, Self::LongDouble) => true,

            // ポインタは const/mut を無視して内部型を比較
            (
                Self::Pointer { inner: i1, .. },
                Self::Pointer { inner: i2, .. },
            ) => i1.equals_ignoring_const(i2),

            (
                Self::Array { inner: i1, size: s1 },
                Self::Array { inner: i2, size: s2 },
            ) => s1 == s2 && i1.equals_ignoring_const(i2),

            (Self::Named(n1), Self::Named(n2)) => n1 == n2,

            (Self::Unknown, Self::Unknown) => true,

            _ => false,
        }
    }

    /// 大文字小文字を無視して比較
    pub fn equals_ignoring_case(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Named(n1), Self::Named(n2)) => n1.eq_ignore_ascii_case(n2),

            (
                Self::Pointer { inner: i1, is_const: c1 },
                Self::Pointer { inner: i2, is_const: c2 },
            ) => c1 == c2 && i1.equals_ignoring_case(i2),

            (
                Self::Array { inner: i1, size: s1 },
                Self::Array { inner: i2, size: s2 },
            ) => s1 == s2 && i1.equals_ignoring_case(i2),

            // 他の型は通常の比較
            _ => self == other,
        }
    }

    /// ポインタ型かどうか
    pub fn is_pointer(&self) -> bool {
        matches!(self, Self::Pointer { .. })
    }

    /// 名前付き型かどうか
    pub fn is_named(&self) -> bool {
        matches!(self, Self::Named(_))
    }

    /// 名前付き型の名前を取得
    pub fn as_named(&self) -> Option<&str> {
        match self {
            Self::Named(name) => Some(name),
            _ => None,
        }
    }

    /// ポインタの内部型を取得
    pub fn inner_type(&self) -> Option<&UnifiedType> {
        match self {
            Self::Pointer { inner, .. } => Some(inner),
            Self::Array { inner, .. } => Some(inner),
            _ => None,
        }
    }
}

impl fmt::Display for UnifiedType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_rust_string())
    }
}

impl SourcedType {
    /// 新しい出自情報付き型を作成
    pub fn new(ty: UnifiedType, source: TypeSource) -> Self {
        Self { ty, source }
    }

    /// apidoc からの型を作成
    pub fn from_apidoc(raw: &str, entry_name: &str) -> Self {
        Self {
            ty: UnifiedType::from_c_str(raw),
            source: TypeSource::Apidoc {
                raw: raw.to_string(),
                entry_name: entry_name.to_string(),
            },
        }
    }

    /// bindings.rs からの型を作成
    pub fn from_bindings(raw: &str) -> Self {
        Self {
            ty: UnifiedType::from_rust_str(raw),
            source: TypeSource::Bindings {
                raw: raw.to_string(),
            },
        }
    }

    /// 元の文字列表現を取得
    pub fn raw_string(&self) -> Option<&str> {
        match &self.source {
            TypeSource::Apidoc { raw, .. } => Some(raw),
            TypeSource::Bindings { raw } => Some(raw),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_c_str_basic_types() {
        assert_eq!(UnifiedType::from_c_str("void"), UnifiedType::Void);
        assert_eq!(UnifiedType::from_c_str("bool"), UnifiedType::Bool);
        assert_eq!(
            UnifiedType::from_c_str("int"),
            UnifiedType::Int { signed: true, size: IntSize::Int }
        );
        assert_eq!(
            UnifiedType::from_c_str("unsigned int"),
            UnifiedType::Int { signed: false, size: IntSize::Int }
        );
        assert_eq!(
            UnifiedType::from_c_str("long long"),
            UnifiedType::Int { signed: true, size: IntSize::LongLong }
        );
    }

    #[test]
    fn test_from_c_str_pointer() {
        assert_eq!(
            UnifiedType::from_c_str("SV *"),
            UnifiedType::Pointer {
                inner: Box::new(UnifiedType::Named("SV".to_string())),
                is_const: false,
            }
        );
        assert_eq!(
            UnifiedType::from_c_str("const char *"),
            UnifiedType::Pointer {
                inner: Box::new(UnifiedType::Char { signed: None }),
                is_const: true,
            }
        );
    }

    #[test]
    fn test_from_c_str_double_pointer() {
        let ty = UnifiedType::from_c_str("SV **");
        assert_eq!(
            ty,
            UnifiedType::Pointer {
                inner: Box::new(UnifiedType::Pointer {
                    inner: Box::new(UnifiedType::Named("SV".to_string())),
                    is_const: false,
                }),
                is_const: false,
            }
        );
    }

    #[test]
    fn test_from_rust_str_basic_types() {
        assert_eq!(UnifiedType::from_rust_str("()"), UnifiedType::Void);
        assert_eq!(UnifiedType::from_rust_str("bool"), UnifiedType::Bool);
        assert_eq!(
            UnifiedType::from_rust_str("c_int"),
            UnifiedType::Int { signed: true, size: IntSize::Int }
        );
        assert_eq!(
            UnifiedType::from_rust_str("c_uint"),
            UnifiedType::Int { signed: false, size: IntSize::Int }
        );
    }

    #[test]
    fn test_from_rust_str_pointer() {
        assert_eq!(
            UnifiedType::from_rust_str("*mut SV"),
            UnifiedType::Pointer {
                inner: Box::new(UnifiedType::Named("SV".to_string())),
                is_const: false,
            }
        );
        assert_eq!(
            UnifiedType::from_rust_str("*const c_char"),
            UnifiedType::Pointer {
                inner: Box::new(UnifiedType::Char { signed: None }),
                is_const: true,
            }
        );
    }

    #[test]
    fn test_from_rust_str_double_pointer() {
        let ty = UnifiedType::from_rust_str("*mut *mut SV");
        assert_eq!(
            ty,
            UnifiedType::Pointer {
                inner: Box::new(UnifiedType::Pointer {
                    inner: Box::new(UnifiedType::Named("SV".to_string())),
                    is_const: false,
                }),
                is_const: false,
            }
        );
    }

    #[test]
    fn test_from_rust_str_with_spaces() {
        // syn の to_token_stream() が生成する "* mut" 形式
        let ty = UnifiedType::from_rust_str("* mut * mut SV");
        assert_eq!(
            ty,
            UnifiedType::Pointer {
                inner: Box::new(UnifiedType::Pointer {
                    inner: Box::new(UnifiedType::Named("SV".to_string())),
                    is_const: false,
                }),
                is_const: false,
            }
        );
    }

    #[test]
    fn test_to_rust_string() {
        assert_eq!(UnifiedType::Void.to_rust_string(), "()");
        assert_eq!(
            UnifiedType::Int { signed: true, size: IntSize::Int }.to_rust_string(),
            "c_int"
        );
        assert_eq!(
            UnifiedType::Pointer {
                inner: Box::new(UnifiedType::Named("SV".to_string())),
                is_const: false,
            }.to_rust_string(),
            "*mut SV"
        );
        assert_eq!(
            UnifiedType::Pointer {
                inner: Box::new(UnifiedType::Char { signed: None }),
                is_const: true,
            }.to_rust_string(),
            "*const c_char"
        );
    }

    #[test]
    fn test_roundtrip_c_to_rust() {
        let cases = [
            ("void", "()"),
            ("int", "c_int"),
            ("unsigned int", "c_uint"),
            ("SV *", "*mut SV"),
            ("const char *", "*const c_char"),
            ("SV **", "*mut *mut SV"),
        ];

        for (c_type, expected_rust) in cases {
            let ty = UnifiedType::from_c_str(c_type);
            assert_eq!(ty.to_rust_string(), expected_rust, "C type: {}", c_type);
        }
    }

    #[test]
    fn test_roundtrip_rust() {
        let cases = [
            "()",
            "bool",
            "c_int",
            "c_uint",
            "*mut SV",
            "*const c_char",
            "*mut *mut SV",
        ];

        for rust_type in cases {
            let ty = UnifiedType::from_rust_str(rust_type);
            assert_eq!(ty.to_rust_string(), rust_type, "Rust type: {}", rust_type);
        }
    }

    #[test]
    fn test_equals_ignoring_const() {
        let mut_sv = UnifiedType::Pointer {
            inner: Box::new(UnifiedType::Named("SV".to_string())),
            is_const: false,
        };
        let const_sv = UnifiedType::Pointer {
            inner: Box::new(UnifiedType::Named("SV".to_string())),
            is_const: true,
        };

        assert!(mut_sv.equals_ignoring_const(&const_sv));
        assert!(!mut_sv.eq(&const_sv)); // 通常の比較では異なる
    }

    #[test]
    fn test_equals_ignoring_case() {
        let sv_upper = UnifiedType::Named("SV".to_string());
        let sv_lower = UnifiedType::Named("sv".to_string());

        assert!(sv_upper.equals_ignoring_case(&sv_lower));
        assert!(!sv_upper.eq(&sv_lower)); // 通常の比較では異なる

        // ポインタ内部でも動作
        let ptr_sv_upper = UnifiedType::Pointer {
            inner: Box::new(sv_upper),
            is_const: false,
        };
        let ptr_sv_lower = UnifiedType::Pointer {
            inner: Box::new(sv_lower),
            is_const: false,
        };
        assert!(ptr_sv_upper.equals_ignoring_case(&ptr_sv_lower));
    }
}
