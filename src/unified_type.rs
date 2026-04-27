//! 統一型表現モジュール
//!
//! C型とRust型を統一的に表現し、変換・比較を行う。

use std::fmt;

use quote::ToTokens;

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

    /// 関数ポインタ型
    ///
    /// C の関数ポインタ (`void (*)(int)`) や Rust の
    /// `extern "C" fn(...) -> ...` に対応する。bindgen は通常
    /// `Option<unsafe extern "C" fn(...)>` 形式で出すため、
    /// `is_optional: true` のケースが事実上の標準。
    FnPtr {
        params: Vec<UnifiedType>,
        ret: Box<UnifiedType>,
        /// ABI 名 (`extern "C"` の `"C"` 等)。`None` はデフォルト ABI。
        abi: Option<String>,
        /// `unsafe fn` なら true
        is_unsafe: bool,
        /// `Option<extern "C" fn(...)>` の Option ラッパを表現
        is_optional: bool,
    },

    /// 構造化未対応の型を syn の正規トークンで保持する escape hatch
    ///
    /// **保持する文字列は必ず `proc_macro2::TokenStream::to_string()`
    /// 出力 (= syn 正規形)** であること。手書き文字列を入れない。
    /// 比較・Hash の安定性は syn の正規化に依存する。
    /// 構造的検査 (`is_pointer` 等) は常に false を返す。emit 時のみ意味を持つ。
    Verbatim(String),

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

        // const を除去（値型では const は無意味）
        let s = s.strip_prefix("const ").unwrap_or(s);
        let s = if s.ends_with(" const") {
            &s[..s.len() - 6]
        } else {
            s
        };
        let s = s.trim();

        // struct/union/enum プレフィックスを除去
        let s = s
            .strip_prefix("struct ")
            .or_else(|| s.strip_prefix("union "))
            .or_else(|| s.strip_prefix("enum "))
            .unwrap_or(s);

        // unsigned/signed の処理
        let (is_unsigned, base) = if s == "unsigned" {
            // "unsigned" alone means "unsigned int"
            (true, "")
        } else if s == "signed" {
            // "signed" alone means "signed int"
            (false, "")
        } else if s.starts_with("unsigned ") {
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

            // size_t, ssize_t などの標準型は Named として保持
            // （to_rust_string で usize/isize に変換される）
            "size_t" | "ssize_t" | "ptrdiff_t" => Self::Named(base.to_string()),

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
        // synのto_token_stream().to_string()は "* mut" や " :: " のようにスペースを入れるため正規化
        let normalized = s
            .replace("* mut", "*mut")
            .replace("* const", "*const")
            .replace(" :: ", "::");
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
        // 配列 `[T; N]` — 要素型を保持して inner_type() で取り出せるようにする。
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            let inner = &trimmed[1..trimmed.len() - 1];
            // "[T; N]" の T と N を分ける (ネスト `[` を考慮)
            let mut depth = 0i32;
            let mut semi_pos: Option<usize> = None;
            for (i, ch) in inner.char_indices() {
                match ch {
                    '[' | '(' | '<' => depth += 1,
                    ']' | ')' | '>' => depth -= 1,
                    ';' if depth == 0 => {
                        semi_pos = Some(i);
                        break;
                    }
                    _ => {}
                }
            }
            if let Some(pos) = semi_pos {
                let elem = inner[..pos].trim();
                let size_str = inner[pos + 1..].trim();
                let size = size_str
                    .split_whitespace()
                    .next()
                    .and_then(|s| s.trim_end_matches("usize").parse::<usize>().ok());
                return Self::Array {
                    inner: Box::new(Self::from_rust_str(elem)),
                    size,
                };
            }
        }

        // 基本型
        Self::parse_rust_basic_type(trimmed)
    }

    /// **構造ベース第一推奨**: `syn::Type` を直接 decompose して構築する。
    ///
    /// `to_token_stream().to_string()` → `from_rust_str` という round-trip
    /// では prefix 剥がしの累積で破綻する型 (`::std::option::Option<extern "C" fn>` 等)
    /// が安全に扱える。bindings.rs (`syn::File`) 由来データはこちらを使うこと。
    ///
    /// 構造的に decompose できない型は `UnifiedType::Verbatim` に格納し、
    /// emit 時に元のトークン列をそのまま吐く (構造的検査は諦める)。
    pub fn from_syn_type(ty: &syn::Type) -> Self {
        match ty {
            // () = Void
            syn::Type::Tuple(t) if t.elems.is_empty() => Self::Void,

            // 透過的に剥がす構造
            syn::Type::Group(g) => Self::from_syn_type(&g.elem),
            syn::Type::Paren(p) => Self::from_syn_type(&p.elem),

            // *mut T / *const T
            syn::Type::Ptr(p) => Self::Pointer {
                inner: Box::new(Self::from_syn_type(&p.elem)),
                is_const: p.const_token.is_some(),
            },

            // [T; N]
            syn::Type::Array(a) => Self::Array {
                inner: Box::new(Self::from_syn_type(&a.elem)),
                size: extract_array_size(&a.len),
            },

            // 裸関数ポインタ (まれ。bindgen は通常 Option ラップ)
            syn::Type::BareFn(f) => from_bare_fn(f, /* is_optional */ false),

            // Path: Option<T>, primitives (c_int 等), 名前付き型 (SV 等)
            syn::Type::Path(tp) => from_type_path(tp),

            // 上記以外は escape hatch
            _ => verbatim_of(ty),
        }
    }

    /// Rust の基本型をパース
    fn parse_rust_basic_type(s: &str) -> Self {
        // std:: プレフィックスを除去
        // syn の to_token_stream() は ":: std" のようにスペースを入れるので trim が必要
        let s = s
            .strip_prefix("::")
            .map(|s| s.trim_start())
            .unwrap_or(s);
        let s = s
            .strip_prefix("std::")
            .unwrap_or(s);
        let s = s
            .strip_prefix("ffi::")
            .or_else(|| s.strip_prefix("os::raw::"))
            .unwrap_or(s);

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
            // isize/usize は c_long/c_ulong と同じ 64bit だが Rust では別型。
            // Int{Long} に詰めると to_rust_string で `c_long`/`c_ulong` に
            // 戻ってしまい、比較・演算で「同じ型」と誤認される。
            // Named にすることでラウンドトリップを保ち区別する。
            "isize" => Self::Named("isize".to_string()),
            "usize" => Self::Named("usize".to_string()),

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
                // void pointer (`Pointer { inner: Void }`) は Rust では
                // `*mut c_void` / `*const c_void` と書く必要がある。
                // `Self::Void` の to_rust_string は `()` を返すので、そのまま
                // ラップすると `*mut ()` という invalid な型になる。
                let inner_str = if matches!(inner.as_ref(), Self::Void) {
                    "c_void".to_string()
                } else {
                    inner.to_rust_string()
                };
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

            Self::Named(name) => {
                // C標準型をRust型に変換
                match name.as_str() {
                    "size_t" => "usize".to_string(),
                    "ssize_t" | "ptrdiff_t" => "isize".to_string(),
                    "off_t" | "off64_t" => "i64".to_string(),
                    _ => name.clone(),
                }
            }

            Self::FnPtr { params, ret, abi, is_unsafe, is_optional } => {
                let mut s = String::new();
                if *is_unsafe {
                    s.push_str("unsafe ");
                }
                if let Some(abi_name) = abi {
                    // `{:?}` で `"C"` のような文字列リテラル形式になる
                    s.push_str(&format!("extern {:?} ", abi_name));
                }
                s.push_str("fn(");
                let param_strs: Vec<String> = params.iter().map(|p| p.to_rust_string()).collect();
                s.push_str(&param_strs.join(", "));
                s.push(')');
                let ret_str = ret.to_rust_string();
                if ret_str != "()" {
                    s.push_str(" -> ");
                    s.push_str(&ret_str);
                }
                if *is_optional {
                    format!("Option<{}>", s)
                } else {
                    s
                }
            }

            Self::Verbatim(s) => s.clone(),

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

    /// void ポインタ (*mut c_void / *const c_void) かどうか
    pub fn is_void_pointer(&self) -> bool {
        match self {
            Self::Pointer { inner, .. } => {
                matches!(**inner, Self::Void) || matches!(**inner, Self::Named(ref n) if n == "c_void")
            }
            _ => false,
        }
    }

    /// 具体的なポインタ（ポインタだが void ではない）かどうか
    pub fn is_concrete_pointer(&self) -> bool {
        self.is_pointer() && !self.is_void_pointer()
    }

    /// const ポインタ型かどうか
    pub fn is_const_pointer(&self) -> bool {
        matches!(self, Self::Pointer { is_const: true, .. })
    }

    /// 浮動小数点型かどうか
    pub fn is_float(&self) -> bool {
        matches!(self, Self::Float | Self::Double | Self::LongDouble)
            || matches!(self, Self::Named(n) if n == "NV")
    }

    /// bool 型かどうか
    pub fn is_bool(&self) -> bool {
        matches!(self, Self::Bool)
    }

    /// void 型かどうか
    pub fn is_void(&self) -> bool {
        matches!(self, Self::Void)
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

    /// 関数ポインタ型かどうか (Option ラップを問わず)
    pub fn is_fn_ptr(&self) -> bool {
        matches!(self, Self::FnPtr { .. })
    }

    /// `Option<fn>` 形式の関数ポインタかどうか
    pub fn is_optional_fn_ptr(&self) -> bool {
        matches!(self, Self::FnPtr { is_optional: true, .. })
    }

    /// Verbatim ハッチ経由かどうか (構造化未対応の指標)
    pub fn is_verbatim(&self) -> bool {
        matches!(self, Self::Verbatim(_))
    }
}

// ============================================================================
// from_syn_type の補助関数 (構造ベースで decompose する内部実装)
// ============================================================================

/// 配列長の `syn::Expr` から `usize` を取り出す。整数リテラル以外は `None`。
fn extract_array_size(expr: &syn::Expr) -> Option<usize> {
    if let syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Int(li), .. }) = expr {
        li.base10_parse::<usize>().ok()
    } else {
        None
    }
}

/// `syn::TypeBareFn` から `UnifiedType::FnPtr` を構築する。
/// `is_optional` は `Option<extern "C" fn(...)>` 形式かどうかの呼出側情報。
fn from_bare_fn(f: &syn::TypeBareFn, is_optional: bool) -> UnifiedType {
    let abi = f.abi.as_ref().map(|abi| {
        abi.name
            .as_ref()
            .map(|n| n.value())
            .unwrap_or_else(|| "C".to_string())
    });
    let params: Vec<UnifiedType> = f
        .inputs
        .iter()
        .map(|arg| UnifiedType::from_syn_type(&arg.ty))
        .collect();
    let ret = match &f.output {
        syn::ReturnType::Default => UnifiedType::Void,
        syn::ReturnType::Type(_, t) => UnifiedType::from_syn_type(t),
    };
    UnifiedType::FnPtr {
        params,
        ret: Box::new(ret),
        abi,
        is_unsafe: f.unsafety.is_some(),
        is_optional,
    }
}

/// `syn::TypePath` を Option<fn> / 基本型 / 名前付き型に振り分ける。
fn from_type_path(tp: &syn::TypePath) -> UnifiedType {
    // qself 付き (`<T as Trait>::U`) は構造化を諦める
    if tp.qself.is_some() {
        return verbatim_of(&syn::Type::Path(tp.clone()));
    }
    let path = &tp.path;

    // Option<T> の検出: 最終セグメントが `Option` で <T> が一つ
    if let Some(last) = path.segments.last() {
        if last.ident == "Option" {
            if let syn::PathArguments::AngleBracketed(ab) = &last.arguments {
                if let Some(syn::GenericArgument::Type(inner)) = ab.args.first() {
                    return from_optional_inner(inner);
                }
            }
        }
    }

    // 単一識別子へ畳めるパス (`SV`, `c_int`, `::std::os::raw::c_int` 等)
    if let Some(ident) = single_ident_of(path) {
        if let Some(prim) = primitive_from_ident(&ident) {
            return prim;
        }
        return UnifiedType::Named(ident);
    }

    // それ以外 (ジェネリック等) は escape hatch
    verbatim_of(&syn::Type::Path(tp.clone()))
}

/// `Option<T>` の中身 `T` を見て、関数ポインタなら `is_optional: true` の
/// `FnPtr` に持ち上げる。それ以外は Verbatim にフォールバック。
fn from_optional_inner(inner: &syn::Type) -> UnifiedType {
    match inner {
        syn::Type::BareFn(f) => from_bare_fn(f, true),
        syn::Type::Group(g) => from_optional_inner(&g.elem),
        syn::Type::Paren(p) => from_optional_inner(&p.elem),
        // Option<*mut T> 等は perl bindings には現れないため Verbatim で残す
        _ => {
            let wrapped: syn::TypePath = syn::parse_quote!(::std::option::Option<#inner>);
            verbatim_of(&syn::Type::Path(wrapped))
        }
    }
}

/// パスを単一識別子に畳めるか試みる:
/// - 最終セグメントに generics が無い
/// - 最終セグメントの ident をそのまま採用 (`::std::os::raw::c_int` → `"c_int"`)
fn single_ident_of(path: &syn::Path) -> Option<String> {
    let last = path.segments.last()?;
    if !matches!(last.arguments, syn::PathArguments::None) {
        return None;
    }
    Some(last.ident.to_string())
}

/// 識別子が C 互換 / Rust 基本型に対応するか調べる。
/// 文字列 prefix の剥がしは行わず、**識別子そのもの** で判定する。
fn primitive_from_ident(name: &str) -> Option<UnifiedType> {
    Some(match name {
        // C 互換 void / bool
        "c_void" => UnifiedType::Void,
        "bool" => UnifiedType::Bool,

        // C 互換 char
        "c_char" => UnifiedType::Char { signed: None },
        "c_schar" => UnifiedType::Char { signed: Some(true) },
        "c_uchar" => UnifiedType::Char { signed: Some(false) },

        // C 互換整数
        "c_short" => UnifiedType::Int { signed: true, size: IntSize::Short },
        "c_ushort" => UnifiedType::Int { signed: false, size: IntSize::Short },
        "c_int" => UnifiedType::Int { signed: true, size: IntSize::Int },
        "c_uint" => UnifiedType::Int { signed: false, size: IntSize::Int },
        "c_long" => UnifiedType::Int { signed: true, size: IntSize::Long },
        "c_ulong" => UnifiedType::Int { signed: false, size: IntSize::Long },
        "c_longlong" => UnifiedType::Int { signed: true, size: IntSize::LongLong },
        "c_ulonglong" => UnifiedType::Int { signed: false, size: IntSize::LongLong },

        // Rust ネイティブ整数 (8/16/32/64/128)
        // i8/u8 は char 系として扱い (バイト列の意味を保つ)
        "i8" => UnifiedType::Char { signed: Some(true) },
        "u8" => UnifiedType::Char { signed: Some(false) },
        "i16" => UnifiedType::Int { signed: true, size: IntSize::Short },
        "u16" => UnifiedType::Int { signed: false, size: IntSize::Short },
        "i32" => UnifiedType::Int { signed: true, size: IntSize::Int },
        "u32" => UnifiedType::Int { signed: false, size: IntSize::Int },
        "i64" => UnifiedType::Int { signed: true, size: IntSize::LongLong },
        "u64" => UnifiedType::Int { signed: false, size: IntSize::LongLong },
        "i128" => UnifiedType::Int { signed: true, size: IntSize::Int128 },
        "u128" => UnifiedType::Int { signed: false, size: IntSize::Int128 },

        // isize/usize は Named 維持 (c_long とサイズが同じでも区別)
        "isize" | "usize" => UnifiedType::Named(name.to_string()),

        // 浮動小数点
        "c_float" | "f32" => UnifiedType::Float,
        "c_double" | "f64" => UnifiedType::Double,

        _ => return None,
    })
}

/// 任意の `syn::Type` を `proc_macro2::TokenStream` の正規形文字列で
/// `UnifiedType::Verbatim` にラップする。
fn verbatim_of(ty: &syn::Type) -> UnifiedType {
    UnifiedType::Verbatim(ty.to_token_stream().to_string())
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

    #[test]
    fn test_const_value_type() {
        // const U32 should be the same as U32
        assert_eq!(
            UnifiedType::from_c_str("const U32"),
            UnifiedType::Named("U32".to_string())
        );
        assert_eq!(
            UnifiedType::from_c_str("const STRLEN"),
            UnifiedType::Named("STRLEN".to_string())
        );
        assert_eq!(
            UnifiedType::from_c_str("const int"),
            UnifiedType::Int { signed: true, size: IntSize::Int }
        );
        assert_eq!(
            UnifiedType::from_c_str("const bool"),
            UnifiedType::Bool
        );
    }

    #[test]
    fn test_struct_prefix() {
        // struct X should be the same as X
        assert_eq!(
            UnifiedType::from_c_str("struct refcounted_he *"),
            UnifiedType::Pointer {
                inner: Box::new(UnifiedType::Named("refcounted_he".to_string())),
                is_const: false,
            }
        );
        assert_eq!(
            UnifiedType::from_c_str("struct SV"),
            UnifiedType::Named("SV".to_string())
        );
    }

    #[test]
    fn test_is_const_pointer() {
        let mut_ptr = UnifiedType::from_rust_str("*mut SV");
        let const_ptr = UnifiedType::from_rust_str("*const c_char");
        let non_ptr = UnifiedType::from_rust_str("c_int");

        assert!(!mut_ptr.is_const_pointer());
        assert!(const_ptr.is_const_pointer());
        assert!(!non_ptr.is_const_pointer());
    }

    #[test]
    fn test_is_float() {
        assert!(UnifiedType::Float.is_float());
        assert!(UnifiedType::Double.is_float());
        assert!(UnifiedType::LongDouble.is_float());
        assert!(UnifiedType::Named("NV".to_string()).is_float());
        assert!(!UnifiedType::Int { signed: true, size: IntSize::Int }.is_float());
        assert!(!UnifiedType::Named("SV".to_string()).is_float());
    }

    #[test]
    fn test_is_bool() {
        assert!(UnifiedType::Bool.is_bool());
        assert!(!UnifiedType::Int { signed: true, size: IntSize::Int }.is_bool());
        assert!(!UnifiedType::Void.is_bool());
    }

    #[test]
    fn test_is_void() {
        assert!(UnifiedType::Void.is_void());
        assert!(!UnifiedType::Bool.is_void());
        assert!(!UnifiedType::Int { signed: true, size: IntSize::Int }.is_void());
    }

    // === Stage 1: FnPtr / Verbatim ===

    #[test]
    fn test_fn_ptr_void_no_args() {
        let ty = UnifiedType::FnPtr {
            params: vec![],
            ret: Box::new(UnifiedType::Void),
            abi: None,
            is_unsafe: false,
            is_optional: false,
        };
        assert!(ty.is_fn_ptr());
        assert!(!ty.is_optional_fn_ptr());
        assert!(!ty.is_pointer());
        assert!(!ty.is_void());
        assert_eq!(ty.to_rust_string(), "fn()");
    }

    #[test]
    fn test_fn_ptr_extern_c_with_args() {
        let ty = UnifiedType::FnPtr {
            params: vec![
                UnifiedType::Pointer {
                    inner: Box::new(UnifiedType::Named("CV".to_string())),
                    is_const: false,
                },
            ],
            ret: Box::new(UnifiedType::Void),
            abi: Some("C".to_string()),
            is_unsafe: true,
            is_optional: false,
        };
        assert_eq!(ty.to_rust_string(), "unsafe extern \"C\" fn(*mut CV)");
    }

    #[test]
    fn test_fn_ptr_optional_with_return() {
        // bindgen が生成する典型形: `Option<unsafe extern "C" fn(arg1: *mut CV) -> *mut SV>`
        let ty = UnifiedType::FnPtr {
            params: vec![
                UnifiedType::Pointer {
                    inner: Box::new(UnifiedType::Named("CV".to_string())),
                    is_const: false,
                },
            ],
            ret: Box::new(UnifiedType::Pointer {
                inner: Box::new(UnifiedType::Named("SV".to_string())),
                is_const: false,
            }),
            abi: Some("C".to_string()),
            is_unsafe: true,
            is_optional: true,
        };
        assert!(ty.is_fn_ptr());
        assert!(ty.is_optional_fn_ptr());
        assert_eq!(
            ty.to_rust_string(),
            "Option<unsafe extern \"C\" fn(*mut CV) -> *mut SV>"
        );
    }

    #[test]
    fn test_verbatim_emits_as_is() {
        let raw = ":: std :: option :: Option < unsafe extern \"C\" fn (arg1 : * mut CV) >";
        let ty = UnifiedType::Verbatim(raw.to_string());
        assert!(ty.is_verbatim());
        assert!(!ty.is_pointer());
        assert!(!ty.is_fn_ptr());
        assert_eq!(ty.to_rust_string(), raw);
    }

    // === Stage 2: from_syn_type ===

    fn parse_ty(s: &str) -> syn::Type {
        syn::parse_str::<syn::Type>(s).expect("syn parse failed")
    }

    #[test]
    fn test_syn_void_tuple() {
        assert_eq!(UnifiedType::from_syn_type(&parse_ty("()")), UnifiedType::Void);
    }

    #[test]
    fn test_syn_pointer_mut() {
        let ty = parse_ty("*mut SV");
        assert_eq!(
            UnifiedType::from_syn_type(&ty),
            UnifiedType::Pointer {
                inner: Box::new(UnifiedType::Named("SV".to_string())),
                is_const: false,
            }
        );
    }

    #[test]
    fn test_syn_pointer_const_char() {
        let ty = parse_ty("*const c_char");
        assert_eq!(
            UnifiedType::from_syn_type(&ty),
            UnifiedType::Pointer {
                inner: Box::new(UnifiedType::Char { signed: None }),
                is_const: true,
            }
        );
    }

    #[test]
    fn test_syn_double_pointer() {
        let ty = parse_ty("*mut *mut OP");
        assert_eq!(
            UnifiedType::from_syn_type(&ty),
            UnifiedType::Pointer {
                inner: Box::new(UnifiedType::Pointer {
                    inner: Box::new(UnifiedType::Named("OP".to_string())),
                    is_const: false,
                }),
                is_const: false,
            }
        );
    }

    #[test]
    fn test_syn_array() {
        let ty = parse_ty("[U32; 8]");
        match UnifiedType::from_syn_type(&ty) {
            UnifiedType::Array { inner, size } => {
                assert_eq!(*inner, UnifiedType::Named("U32".to_string()));
                assert_eq!(size, Some(8));
            }
            other => panic!("expected Array, got {:?}", other),
        }
    }

    #[test]
    fn test_syn_primitive_full_path() {
        // `::std::os::raw::c_int` の最終識別子で判定
        let ty = parse_ty(":: std :: os :: raw :: c_int");
        assert_eq!(
            UnifiedType::from_syn_type(&ty),
            UnifiedType::Int { signed: true, size: IntSize::Int }
        );
    }

    #[test]
    fn test_syn_primitive_short_path() {
        let ty = parse_ty("c_uchar");
        assert_eq!(
            UnifiedType::from_syn_type(&ty),
            UnifiedType::Char { signed: Some(false) }
        );
    }

    #[test]
    fn test_syn_named_type() {
        let ty = parse_ty("PerlInterpreter");
        assert_eq!(
            UnifiedType::from_syn_type(&ty),
            UnifiedType::Named("PerlInterpreter".to_string())
        );
    }

    #[test]
    fn test_syn_option_extern_c_fn_ptr() {
        // bindgen 標準形: `xcv_xsub` の型表現
        let ty = parse_ty(
            ":: std :: option :: Option < unsafe extern \"C\" fn (arg1 : * mut CV) >",
        );
        let ut = UnifiedType::from_syn_type(&ty);
        let expected = UnifiedType::FnPtr {
            params: vec![UnifiedType::Pointer {
                inner: Box::new(UnifiedType::Named("CV".to_string())),
                is_const: false,
            }],
            ret: Box::new(UnifiedType::Void),
            abi: Some("C".to_string()),
            is_unsafe: true,
            is_optional: true,
        };
        assert_eq!(ut, expected);
        // emit が `option::Option<...>` ではなく正しい形に戻ること
        assert_eq!(
            ut.to_rust_string(),
            "Option<unsafe extern \"C\" fn(*mut CV)>"
        );
    }

    #[test]
    fn test_syn_option_fn_with_return() {
        let ty = parse_ty(
            "Option<unsafe extern \"C\" fn(my_perl: *mut PerlInterpreter, rx: *mut REGEXP) -> *mut SV>",
        );
        let ut = UnifiedType::from_syn_type(&ty);
        assert!(ut.is_optional_fn_ptr());
        assert_eq!(
            ut.to_rust_string(),
            "Option<unsafe extern \"C\" fn(*mut PerlInterpreter, *mut REGEXP) -> *mut SV>"
        );
    }

    #[test]
    fn test_syn_unsupported_falls_to_verbatim() {
        // ジェネリック型 (Option<fn> 以外) は Verbatim にフォールバックする
        let ty = parse_ty("Vec<u8>");
        let ut = UnifiedType::from_syn_type(&ty);
        assert!(ut.is_verbatim());
        // Verbatim の文字列は syn 正規形 (空白入り)
        assert_eq!(ut.to_rust_string(), "Vec < u8 >");
    }

    #[test]
    fn test_syn_paren_and_group_unwrap() {
        // 括弧で囲まれた型はそのまま剥がれて中身に等しくなる
        let ty = parse_ty("(*mut SV)");
        assert_eq!(
            UnifiedType::from_syn_type(&ty),
            UnifiedType::Pointer {
                inner: Box::new(UnifiedType::Named("SV".to_string())),
                is_const: false,
            }
        );
    }

    #[test]
    fn test_fn_ptr_eq_hash() {
        // PartialEq / Hash の derive が新 variant でも壊れていないこと
        let a = UnifiedType::FnPtr {
            params: vec![UnifiedType::Void],
            ret: Box::new(UnifiedType::Void),
            abi: Some("C".to_string()),
            is_unsafe: true,
            is_optional: true,
        };
        let b = a.clone();
        assert_eq!(a, b);
        let mut set = std::collections::HashSet::new();
        set.insert(a.clone());
        assert!(set.contains(&b));
    }
}
