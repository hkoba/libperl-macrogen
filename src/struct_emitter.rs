//! 構造体・union 定義の Rust ソース生成
//!
//! C ヘッダで宣言されているが bindings.rs に存在しない struct/union を
//! `macro_bindings.rs` 側で `#[repr(C)]` 付き Rust 定義として生成する。
//! 例: `body_details`、`ALIGNED_TYPE_NAME(*)` 系 typedef union。
//!
//! ビットフィールドは連続する bitfield グループを 1 つの packed `u8`/`u16`/`u32`
//! フィールドにまとめる（簡易版）。bindgen 風の getter/setter は付けず、
//! 初期化時の値計算は呼び出し側で行う想定。
//!
//! `flexible array member`（最終メンバーが `T[1]`/`[0]`/`[]`）は
//! `[T; 0]` として出力する（Rust では size 0 配列が flex array 相当）。

use std::collections::HashSet;

use crate::fields_dict::{FieldsDict, StructDef, StructMemberInfo};
use crate::intern::{InternedStr, StringInterner};
use crate::rust_decl::RustDeclDict;
use crate::type_repr::{CDerivedType, TypeRepr};

/// 名前が Rust の予約語で、struct 名として利用すると複雑な escape が必要な場合に
/// 出力をスキップするための集合。`r#name` は struct 自体は valid だが、その struct
/// を参照する全箇所も `r#name` にしないとならず、対応が大きい。初版では諦める。
const SKIP_NAMES_KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn",
    "else", "enum", "extern", "fn", "for", "if", "impl", "in",
    "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return",
    "self", "Self", "static", "struct", "super", "trait", "type",
    "unsafe", "use", "where", "while",
    "abstract", "become", "box", "do", "final", "gen", "macro", "override",
    "priv", "try", "typeof", "unsized", "virtual", "yield",
];

/// `bindings.rs` に無い struct/union を順序保持して列挙する。
///
/// 戻り値は (name, def) のペア。bindings.rs の `structs` HashMap に存在する
/// 名前は除外する。
pub fn missing_struct_defs<'a>(
    fields_dict: &'a FieldsDict,
    rust_decl_dict: Option<&RustDeclDict>,
    interner: &StringInterner,
) -> Vec<(InternedStr, &'a StructDef)> {
    let bindings_struct_names: HashSet<String> = rust_decl_dict
        .map(|d| d.structs.keys().cloned().collect())
        .unwrap_or_default();

    let mut result: Vec<(InternedStr, &StructDef)> = fields_dict
        .iter_struct_defs()
        .filter(|(name, _)| {
            let n = interner.get(**name);
            !bindings_struct_names.contains(n) && !SKIP_NAMES_KEYWORDS.contains(&n)
        })
        .map(|(n, d)| (*n, d))
        .collect();
    // 名前順で安定ソート（決定論的出力）
    result.sort_by(|a, b| interner.get(a.0).cmp(interner.get(b.0)));
    result
}

/// 1 つの struct/union を Rust ソース形式で整形する。
///
/// ビットフィールド群は連続グループを `_bitfield_<n>: u8` に統合する。
/// 1 byte (8 bit) を超えるグループは現状非対応で warning コメントを出力。
/// 第 2 戻り値は当該 struct に生成した bit-field getter メソッド名の集合。
/// 呼出側でこれを `bitfield_methods` 集合にマージすると、codegen 時に
/// フィールド参照を `.name()`、代入を `.set_name(val)` に変換できる。
pub fn format_struct(def: &StructDef, interner: &StringInterner) -> (String, HashSet<String>) {
    let mut buf = String::new();
    let mut bitfield_accessors: HashSet<String> = HashSet::new();
    buf.push_str("#[repr(C)]\n");
    buf.push_str("#[derive(Copy, Clone)]\n");
    buf.push_str(&format!(
        "pub {} {} {{\n",
        if def.is_union { "union" } else { "struct" },
        interner.get(def.name)
    ));

    // bitfield 群ごとの情報（impl 生成用）: (group_idx, pack_ty, [(name, shift, width)])
    let mut bitfield_groups: Vec<(usize, &'static str, Vec<(String, u32, u32)>)> = Vec::new();

    // bitfield 連続グループを検出
    let mut i = 0;
    let mut bitfield_group_idx = 0;
    while i < def.members.len() {
        let m = &def.members[i];
        if m.bitfield_width.is_some() {
            // bitfield グループの終端を探す
            let group_start = i;
            let mut total_width = 0u32;
            while i < def.members.len() && def.members[i].bitfield_width.is_some() {
                total_width += def.members[i].bitfield_width.unwrap();
                i += 1;
            }
            // packed 型を選択
            let pack_ty: &'static str = if total_width <= 8 { "u8" }
                else if total_width <= 16 { "u16" }
                else if total_width <= 32 { "u32" }
                else { "u64" };
            // 含まれる field 名のコメント
            let names: Vec<&str> = def.members[group_start..i]
                .iter().map(|m| interner.get(m.name)).collect();
            buf.push_str(&format!(
                "    /// packed bitfields ({} bit total): {}\n",
                total_width, names.join(", ")
            ));
            buf.push_str(&format!(
                "    pub _bitfield_{}: {},\n",
                bitfield_group_idx, pack_ty
            ));
            // impl 生成用情報を収集
            let mut shift = 0u32;
            let mut entries: Vec<(String, u32, u32)> = Vec::new();
            for bm in &def.members[group_start..i] {
                let w = bm.bitfield_width.unwrap();
                entries.push((interner.get(bm.name).to_string(), shift, w));
                shift += w;
            }
            bitfield_groups.push((bitfield_group_idx, pack_ty, entries));
            bitfield_group_idx += 1;
        } else {
            buf.push_str(&format_member_line(m, interner));
            i += 1;
        }
    }

    buf.push_str("}\n");

    // Bitfield の getter / setter を impl ブロックで emit する。
    // bindgen と同名（`fn <name>(&self)` / `fn set_<name>(&mut self, val)`）にして
    // codegen 側の bitfield-method 判定にそのまま乗せる。
    if !bitfield_groups.is_empty() {
        buf.push_str(&format!("impl {} {{\n", interner.get(def.name)));
        for (gidx, pack_ty, entries) in &bitfield_groups {
            for (name, shift, width) in entries {
                // Rust キーワードは getter 名として使えないため skip
                if SKIP_NAMES_KEYWORDS.contains(&name.as_str()) {
                    continue;
                }
                let mask = if *width >= 64 { u64::MAX } else { (1u64 << width) - 1 };
                // getter: ((self._bitfield_N >> shift) & mask) as pack_ty
                buf.push_str(&format!(
                    "    #[inline]\n    pub const fn {name}(&self) -> {pack_ty} {{\n\
                     \x20       ((self._bitfield_{gidx} >> {shift}) & {mask}) as {pack_ty}\n\
                     \x20   }}\n",
                    name = name, pack_ty = pack_ty, gidx = gidx,
                    shift = shift, mask = mask
                ));
                // setter: clear bits then OR new value
                buf.push_str(&format!(
                    "    #[inline]\n    pub fn set_{name}(&mut self, val: {pack_ty}) {{\n\
                     \x20       self._bitfield_{gidx} = (self._bitfield_{gidx} & !(({mask} as {pack_ty}) << {shift}))\n\
                     \x20           | ((val & {mask} as {pack_ty}) << {shift});\n\
                     \x20   }}\n",
                    name = name, pack_ty = pack_ty, gidx = gidx,
                    shift = shift, mask = mask
                ));
                bitfield_accessors.insert(name.clone());
            }
        }
        buf.push_str("}\n");
    }

    (buf, bitfield_accessors)
}

/// 単一メンバー行を整形（非 bitfield）。flex array は `[T; 0]` に置換。
fn format_member_line(m: &StructMemberInfo, interner: &StringInterner) -> String {
    let ty_str = type_repr_to_rust_struct_field(&m.type_repr, interner);
    format!("    pub {}: {},\n", interner.get(m.name), ty_str)
}

/// `TypeRepr` を struct フィールド型の Rust 文字列にする。
/// 末尾 size 1/0 配列は `[T; 0]` に変換（flex array 慣用句）。
pub fn type_repr_to_rust_struct_field(ty: &TypeRepr, interner: &StringInterner) -> String {
    // flex array 検出: 最後の derived が Array { Some(0|1) | None }
    if let TypeRepr::CType { derived, .. } = ty {
        if let Some(CDerivedType::Array { size }) = derived.last() {
            if matches!(size, None | Some(0) | Some(1)) {
                // flex array → [elem; 0]
                let mut without_last = (*ty).clone();
                if let TypeRepr::CType { derived: d, .. } = &mut without_last {
                    d.pop();
                }
                let elem = without_last.to_rust_string(interner);
                return format!("[{}; 0]", elem);
            }
        }
    }
    ty.to_rust_string(interner)
}

/// 出力結果と実際に出力した名前集合のペア
pub struct EmittedStructs {
    pub source: String,
    /// syn::parse_str を通った struct/union 名
    pub emitted_struct_names: HashSet<String>,
    /// 出力した typedef alias 名（左辺）
    pub emitted_typedef_names: HashSet<String>,
    /// 自動生成した struct ごとの bit-field getter 名集合。codegen 側で
    /// `bitfield_methods` にマージすると、フィールド参照が自動的に
    /// `.name()` / `.set_name(val)` に書き換えられる。
    pub bitfield_methods: std::collections::HashMap<String, HashSet<String>>,
}

/// `missing_struct_defs` 全件を 1 つの Rust ソース文字列にする。
/// セクションヘッダコメントを冒頭に付与。
pub fn emit_missing_structs(
    fields_dict: &FieldsDict,
    rust_decl_dict: Option<&RustDeclDict>,
    interner: &StringInterner,
) -> EmittedStructs {
    let defs = missing_struct_defs(fields_dict, rust_decl_dict, interner);
    let mut emitted_struct_names: HashSet<String> = HashSet::new();
    let mut emitted_typedef_names: HashSet<String> = HashSet::new();
    let mut bitfield_methods: std::collections::HashMap<String, HashSet<String>> =
        std::collections::HashMap::new();
    if defs.is_empty() {
        return EmittedStructs {
            source: String::new(),
            emitted_struct_names,
            emitted_typedef_names,
            bitfield_methods,
        };
    }
    let mut buf = String::new();
    buf.push_str("// === Auto-generated struct definitions ===\n");
    buf.push_str("// Structs/unions declared in C headers but absent from bindings.rs\n");
    buf.push_str("// (typically static-inline-only headers like sv_inline.h).\n\n");
    for (name, def) in defs {
        let (formatted, accessors) = format_struct(def, interner);
        // `struct ... impl ...` 連結は syn::Item 単体では parse できないので
        // File としてパース検証する。
        if syn::parse_str::<syn::File>(&formatted).is_ok() {
            buf.push_str(&formatted);
            buf.push('\n');
            let struct_name = interner.get(name).to_string();
            emitted_struct_names.insert(struct_name.clone());
            if !accessors.is_empty() {
                bitfield_methods.insert(struct_name, accessors);
            }
        } else {
            buf.push_str(&format!(
                "// [SKIPPED] struct/union {} — failed to format as valid Rust\n\n",
                interner.get(name)
            ));
        }
    }

    // typedef alias: bindings.rs に既存の struct を別名で typedef している
    // （例: `typedef struct xpvhv_with_aux XPVHV_WITH_AUX;`）が、
    // typedef 名 (XPVHV_WITH_AUX) が bindings.rs に無い場合に補完する。
    let bindings_struct_names: HashSet<String> = rust_decl_dict
        .map(|d| d.structs.keys().cloned().collect())
        .unwrap_or_default();
    let bindings_type_names: HashSet<String> = rust_decl_dict
        .map(|d| d.types.keys().cloned().collect())
        .unwrap_or_default();
    let mut typedef_aliases: Vec<(String, String)> = fields_dict
        .iter_typedefs()
        .map(|(td, st)| (interner.get(*td).to_string(), interner.get(*st).to_string()))
        .filter(|(td, st)| {
            !bindings_struct_names.contains(td)
                && !bindings_type_names.contains(td)
                && bindings_struct_names.contains(st)
        })
        .collect();
    typedef_aliases.sort();
    if !typedef_aliases.is_empty() {
        buf.push_str("// === Auto-generated typedef aliases ===\n");
        buf.push_str("// `typedef struct foo NAME;` where struct foo is in bindings.rs\n");
        buf.push_str("// but the typedef name NAME is not (e.g. XPVHV_WITH_AUX).\n\n");
        for (td, st) in typedef_aliases {
            buf.push_str(&format!("#[allow(non_camel_case_types)] pub type {} = {};\n", td, st));
            emitted_typedef_names.insert(td);
        }
        buf.push('\n');
    }
    EmittedStructs { source: buf, emitted_struct_names, emitted_typedef_names, bitfield_methods }
}
