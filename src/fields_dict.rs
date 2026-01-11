//! 構造体フィールド名辞書
//!
//! マクロの引数型推論のため、フィールド名から構造体名への
//! マッピングと、フィールドの型情報を記録する。

use std::collections::{HashMap, HashSet};

use crate::ast::{Declaration, DeclSpecs, Declarator, DerivedDecl, ExternalDecl, StorageClass, StructSpec, TypeSpec};
use crate::intern::{InternedStr, StringInterner};

/// フィールドの型情報
#[derive(Debug, Clone)]
pub struct FieldType {
    /// Rust言語での型表現
    pub rust_type: String,
}

/// フィールド名から構造体名へのマッピング
#[derive(Debug, Default)]
pub struct FieldsDict {
    /// フィールド名 -> 構造体名のセット
    /// (同じフィールド名が複数の構造体で使われる可能性があるためHashSet)
    field_to_structs: HashMap<InternedStr, HashSet<InternedStr>>,
    /// (構造体名, フィールド名) -> フィールド型
    field_types: HashMap<(InternedStr, InternedStr), FieldType>,
    /// typedef名 -> 構造体名のマッピング (例: XPV -> xpv)
    typedef_to_struct: HashMap<InternedStr, InternedStr>,
    /// 一致型キャッシュ: フィールド名 -> 一致型（全バリアントで型が同じ場合のみ Some）
    consistent_type_cache: HashMap<InternedStr, Option<String>>,
}

impl FieldsDict {
    /// 新しい辞書を作成
    pub fn new() -> Self {
        Self::default()
    }

    /// 外部宣言からフィールド情報を収集
    /// is_target: この宣言がターゲットディレクトリ内で定義されたかどうか
    pub fn collect_from_external_decl(
        &mut self,
        decl: &ExternalDecl,
        is_target: bool,
        interner: &StringInterner,
    ) {
        // ターゲットディレクトリ内の宣言のみ収集
        if !is_target {
            return;
        }

        if let ExternalDecl::Declaration(d) = decl {
            self.collect_from_declaration(d, interner);
        }
    }

    /// 宣言からフィールド情報を収集
    fn collect_from_declaration(&mut self, decl: &Declaration, interner: &StringInterner) {
        // typedef struct xpv XPV; のような宣言を検出して typedef マッピングを登録
        if decl.specs.storage == Some(StorageClass::Typedef) {
            self.collect_typedef_aliases(decl, interner);
        }

        for type_spec in &decl.specs.type_specs {
            match type_spec {
                TypeSpec::Struct(spec) => {
                    self.collect_from_struct_spec(spec, interner);
                }
                TypeSpec::Union(spec) => {
                    // 共用体も同様に収集
                    self.collect_from_struct_spec(spec, interner);
                }
                _ => {}
            }
        }
    }

    /// typedef 宣言から typedef名 -> 構造体名 のマッピングを収集
    /// 例: typedef struct xpv XPV; → XPV -> xpv
    fn collect_typedef_aliases(&mut self, decl: &Declaration, _interner: &StringInterner) {
        // 構造体/共用体の名前を取得
        let mut struct_name: Option<InternedStr> = None;
        for type_spec in &decl.specs.type_specs {
            match type_spec {
                TypeSpec::Struct(spec) | TypeSpec::Union(spec) => {
                    if let Some(name) = spec.name {
                        struct_name = Some(name);
                        break;
                    }
                }
                _ => {}
            }
        }

        // 構造体名がある場合、宣言子から typedef 名を取得
        if let Some(s_name) = struct_name {
            for init_decl in &decl.declarators {
                // ポインタなしの直接typedef のみ対象
                // 例: typedef struct xpv XPV; は対象
                //     typedef struct xpv *XPV; は対象外
                if init_decl.declarator.derived.is_empty() {
                    if let Some(typedef_name) = init_decl.declarator.name {
                        self.typedef_to_struct.insert(typedef_name, s_name);
                    }
                }
            }
        }
    }

    /// 構造体指定からフィールド情報を収集
    fn collect_from_struct_spec(&mut self, spec: &StructSpec, interner: &StringInterner) {
        // 名前付き構造体のみ対象
        let struct_name = match spec.name {
            Some(name) => name,
            None => return,
        };

        // メンバーがある場合のみ処理
        let members = match &spec.members {
            Some(m) => m,
            None => return,
        };

        for member in members {
            // メンバーの型指定子にネストした構造体があれば再帰的に処理
            for type_spec in &member.specs.type_specs {
                match type_spec {
                    TypeSpec::Struct(nested) => {
                        self.collect_from_struct_spec(nested, interner);
                    }
                    TypeSpec::Union(nested) => {
                        self.collect_from_struct_spec(nested, interner);
                    }
                    _ => {}
                }
            }

            // フィールド名と型を収集
            for decl in &member.declarators {
                if let Some(ref declarator) = decl.declarator {
                    if let Some(field_name) = declarator.name {
                        // フィールド名 -> 構造体名のマッピング
                        self.field_to_structs
                            .entry(field_name)
                            .or_insert_with(HashSet::new)
                            .insert(struct_name);

                        // フィールド型の収集
                        if let Some(rust_type) = self.extract_field_type(&member.specs, declarator, interner) {
                            self.field_types.insert(
                                (struct_name, field_name),
                                FieldType { rust_type },
                            );
                        }
                    }
                }
            }
        }
    }

    /// DeclSpecs と Declarator からフィールドの Rust 型を抽出
    fn extract_field_type(&self, specs: &DeclSpecs, declarator: &Declarator, interner: &StringInterner) -> Option<String> {
        // 基本型を取得
        let base_type = self.extract_base_type(specs, interner)?;

        // ポインタ等の派生型を適用
        let full_type = self.apply_derived_decls(&base_type, &declarator.derived, &specs.qualifiers);

        Some(full_type)
    }

    /// DeclSpecs から基本型を抽出
    fn extract_base_type(&self, specs: &DeclSpecs, interner: &StringInterner) -> Option<String> {
        let mut has_signed = false;
        let mut has_unsigned = false;
        let mut has_short = false;
        let mut has_long = 0u8;
        let mut base_type: Option<String> = None;

        for type_spec in &specs.type_specs {
            match type_spec {
                TypeSpec::Void => base_type = Some("()".to_string()),
                TypeSpec::Char => base_type = Some("c_char".to_string()),
                TypeSpec::Short => has_short = true,
                TypeSpec::Int => {
                    if base_type.is_none() {
                        base_type = Some("c_int".to_string());
                    }
                }
                TypeSpec::Long => has_long += 1,
                TypeSpec::Float => base_type = Some("c_float".to_string()),
                TypeSpec::Double => base_type = Some("c_double".to_string()),
                TypeSpec::Signed => has_signed = true,
                TypeSpec::Unsigned => has_unsigned = true,
                TypeSpec::Bool => base_type = Some("bool".to_string()),
                TypeSpec::Int128 => {
                    base_type = Some(if has_unsigned { "u128" } else { "i128" }.to_string())
                }
                TypeSpec::Struct(s) | TypeSpec::Union(s) => {
                    if let Some(name) = s.name {
                        base_type = Some(interner.get(name).to_string());
                    }
                }
                TypeSpec::Enum(_) => base_type = Some("c_int".to_string()),
                TypeSpec::TypedefName(name) => {
                    base_type = Some(interner.get(*name).to_string());
                }
                _ => {}
            }
        }

        // signed/unsigned と short/long の組み合わせを処理
        if has_short {
            base_type = Some(if has_unsigned { "c_ushort" } else { "c_short" }.to_string());
        } else if has_long >= 2 {
            base_type = Some(if has_unsigned { "c_ulonglong" } else { "c_longlong" }.to_string());
        } else if has_long == 1 {
            if base_type.as_deref() == Some("c_double") {
                // long double は特別扱い（Rust には直接対応がない）
                base_type = Some("c_double".to_string());
            } else {
                base_type = Some(if has_unsigned { "c_ulong" } else { "c_long" }.to_string());
            }
        } else if has_unsigned && base_type.is_none() {
            base_type = Some("c_uint".to_string());
        } else if has_signed && base_type.is_none() {
            base_type = Some("c_int".to_string());
        } else if has_unsigned && base_type.as_deref() == Some("c_char") {
            base_type = Some("c_uchar".to_string());
        } else if has_unsigned && base_type.as_deref() == Some("c_int") {
            base_type = Some("c_uint".to_string());
        }

        base_type
    }

    /// 派生型（ポインタ、配列など）を適用
    fn apply_derived_decls(
        &self,
        base_type: &str,
        derived: &[DerivedDecl],
        _qualifiers: &crate::ast::TypeQualifiers,
    ) -> String {
        let mut result = base_type.to_string();

        // 派生宣言子は逆順に適用（内側から外側へ）
        for decl in derived.iter().rev() {
            match decl {
                DerivedDecl::Pointer(quals) => {
                    if quals.is_const {
                        result = format!("*const {}", result);
                    } else {
                        result = format!("*mut {}", result);
                    }
                }
                DerivedDecl::Array(_) => {
                    // 配列はポインタとして扱う
                    result = format!("*mut {}", result);
                }
                DerivedDecl::Function(_) => {
                    // 関数ポインタ（簡略化）
                    result = "unsafe extern \"C\" fn()".to_string();
                }
            }
        }

        result
    }

    /// フィールド名と構造体名を手動で登録
    pub fn add_field(&mut self, field_name: InternedStr, struct_name: InternedStr) {
        self.field_to_structs
            .entry(field_name)
            .or_insert_with(HashSet::new)
            .insert(struct_name);
    }

    /// フィールドを一意な構造体型で上書き登録
    /// 既存の登録をすべて破棄し、指定した型のみを設定する
    pub fn set_unique_field_type(&mut self, field_name: InternedStr, struct_name: InternedStr) {
        let mut set = HashSet::new();
        set.insert(struct_name);
        self.field_to_structs.insert(field_name, set);
    }

    /// フィールド名から構造体名を検索
    pub fn lookup(&self, field_name: InternedStr) -> Option<&HashSet<InternedStr>> {
        self.field_to_structs.get(&field_name)
    }

    /// 一意に構造体を特定できるフィールド名から構造体名を取得
    pub fn lookup_unique(&self, field_name: InternedStr) -> Option<InternedStr> {
        self.field_to_structs.get(&field_name).and_then(|structs| {
            if structs.len() == 1 {
                structs.iter().next().copied()
            } else {
                None
            }
        })
    }

    /// (構造体名, フィールド名) からフィールド型を検索
    pub fn get_field_type(
        &self,
        struct_name: InternedStr,
        field_name: InternedStr,
    ) -> Option<&FieldType> {
        self.field_types.get(&(struct_name, field_name))
    }

    /// フィールド名から一意にフィールド型を特定（構造体が1つしかない場合）
    pub fn get_unique_field_type(&self, field_name: InternedStr) -> Option<&FieldType> {
        let struct_name = self.lookup_unique(field_name)?;
        self.field_types.get(&(struct_name, field_name))
    }

    /// 構造体名（文字列）とフィールド名からフィールド型を取得
    /// StringInterner が immutable な場合に使用する
    /// typedef 名でも検索可能（例: XPV で検索すると xpv のフィールドを返す）
    pub fn get_field_type_by_name(
        &self,
        struct_name_str: &str,
        field_name: InternedStr,
        interner: &StringInterner,
    ) -> Option<&FieldType> {
        // まず直接検索
        for ((s_name, f_name), field_type) in &self.field_types {
            if interner.get(*s_name) == struct_name_str && *f_name == field_name {
                return Some(field_type);
            }
        }

        // typedef 名から構造体名を解決して再検索
        if let Some(resolved_struct_name) = self.resolve_typedef_by_name(struct_name_str, interner) {
            let resolved_str = interner.get(resolved_struct_name);
            for ((s_name, f_name), field_type) in &self.field_types {
                if interner.get(*s_name) == resolved_str && *f_name == field_name {
                    return Some(field_type);
                }
            }
        }

        None
    }

    /// typedef 名を登録
    pub fn register_typedef(&mut self, typedef_name: InternedStr, struct_name: InternedStr) {
        self.typedef_to_struct.insert(typedef_name, struct_name);
    }

    /// typedef 名から構造体名を解決（InternedStr ベース）
    pub fn resolve_typedef(&self, typedef_name: InternedStr) -> Option<InternedStr> {
        self.typedef_to_struct.get(&typedef_name).copied()
    }

    /// typedef 名から構造体名を解決（文字列ベース）
    fn resolve_typedef_by_name(&self, typedef_name_str: &str, interner: &StringInterner) -> Option<InternedStr> {
        for (typedef_name, struct_name) in &self.typedef_to_struct {
            if interner.get(*typedef_name) == typedef_name_str {
                return Some(*struct_name);
            }
        }
        None
    }

    /// 登録された typedef の数を取得
    pub fn typedef_count(&self) -> usize {
        self.typedef_to_struct.len()
    }

    /// フィールド型をオーバーライド設定
    /// 自動収集できない場合や、特殊なマッピングが必要な場合に使用
    pub fn set_field_type_override(
        &mut self,
        struct_name: InternedStr,
        field_name: InternedStr,
        rust_type: String,
    ) {
        self.field_types.insert(
            (struct_name, field_name),
            FieldType { rust_type },
        );
    }

    /// 収集されたフィールド型の数を取得
    pub fn field_types_count(&self) -> usize {
        self.field_types.len()
    }

    // ==================== Polymorphic Field Detection ====================

    /// SV ファミリーのメンバー構造体名
    /// Perl の _SV_HEAD マクロにより同一レイアウトを共有する構造体群
    const SV_FAMILY_MEMBERS: &'static [&'static str] = &[
        "sv", "av", "hv", "gv", "cv", "io", "p5rx", "invlist", "STRUCT_SV",
    ];

    /// SV_HEAD マクロで定義される共通フィールド
    /// これらのフィールドは SV ファミリー全体で共有される
    const SV_HEAD_FIELDS: &'static [&'static str] = &[
        "sv_any", "sv_refcnt", "sv_flags", "sv_u",
    ];

    /// フィールドが複数構造体に共有されているか (polymorphic か)
    pub fn is_polymorphic_field(&self, field_name: InternedStr) -> bool {
        self.field_to_structs
            .get(&field_name)
            .map(|structs| structs.len() > 1)
            .unwrap_or(false)
    }

    /// フィールドを持つ全構造体を取得
    pub fn get_structs_with_field(&self, field_name: InternedStr) -> Option<&HashSet<InternedStr>> {
        self.field_to_structs.get(&field_name)
    }

    /// 構造体セットが SV ファミリーか判定
    /// すべての構造体が SV_FAMILY_MEMBERS に含まれていれば true
    pub fn is_sv_family(&self, structs: &HashSet<InternedStr>, interner: &StringInterner) -> bool {
        if structs.is_empty() {
            return false;
        }

        structs.iter().all(|s| {
            let name = interner.get(*s);
            Self::SV_FAMILY_MEMBERS.contains(&name)
        })
    }

    /// SV ファミリーの基底型 ("sv") の InternedStr を取得
    /// interner に "sv" が登録されていない場合は None
    pub fn get_sv_family_base_type(&self, interner: &StringInterner) -> Option<InternedStr> {
        interner.lookup("sv")
    }

    /// フィールドが SV ファミリーの共有フィールドかどうか判定
    ///
    /// 以下のいずれかを満たす場合に true:
    /// 1. フィールドが SV_HEAD_FIELDS に含まれ、"sv" 構造体に属している
    /// 2. フィールドが複数の SV ファミリー構造体に属している
    pub fn is_sv_family_field(&self, field_name: InternedStr, interner: &StringInterner) -> bool {
        let field_str = interner.get(field_name);

        // SV_HEAD フィールドは常に SV ファミリー共有
        if Self::SV_HEAD_FIELDS.contains(&field_str) {
            // "sv" 構造体に属しているか確認
            if let Some(structs) = self.field_to_structs.get(&field_name) {
                return structs.iter().any(|s| {
                    let name = interner.get(*s);
                    name == "sv"
                });
            }
        }

        // 複数の SV ファミリー構造体に属している場合
        if let Some(structs) = self.field_to_structs.get(&field_name) {
            structs.len() > 1 && self.is_sv_family(structs, interner)
        } else {
            false
        }
    }

    // ==================== Consistent Type Cache ====================

    /// 一致型キャッシュを構築
    ///
    /// 全フィールドについて、全バリアントで型が一致するかを事前計算する。
    /// パース完了後、型推論前に1回呼び出す。
    pub fn build_consistent_type_cache(&mut self) {
        self.consistent_type_cache.clear();

        for (&field_name, structs) in &self.field_to_structs {
            let consistent_type = self.compute_consistent_type(field_name, structs);
            self.consistent_type_cache.insert(field_name, consistent_type);
        }
    }

    /// 一致型を計算（内部用）
    fn compute_consistent_type(
        &self,
        field_name: InternedStr,
        structs: &HashSet<InternedStr>,
    ) -> Option<String> {
        if structs.is_empty() {
            return None;
        }

        let mut first_type: Option<&str> = None;

        for struct_name in structs {
            if let Some(ft) = self.field_types.get(&(*struct_name, field_name)) {
                match first_type {
                    None => first_type = Some(&ft.rust_type),
                    Some(t) if t != ft.rust_type => return None, // 不一致
                    Some(_) => {} // 一致、続行
                }
            }
        }

        first_type.map(|s| s.to_string())
    }

    /// キャッシュから一致型を取得（O(1)）
    ///
    /// フィールドが全バリアントで同じ型を持つ場合、その型を返す。
    /// 型が不一致、またはフィールドが存在しない場合は None。
    pub fn get_consistent_field_type(&self, field_name: InternedStr) -> Option<&str> {
        self.consistent_type_cache
            .get(&field_name)
            .and_then(|opt| opt.as_deref())
    }

    // ==================== Dump and Debug ====================

    /// 辞書をダンプ
    pub fn dump(&self, interner: &StringInterner) -> String {
        let mut result = String::new();

        // フィールド名でソートして出力
        let mut entries: Vec<_> = self.field_to_structs.iter().collect();
        entries.sort_by_key(|(field, _)| interner.get(**field));

        for (field_name, struct_names) in entries {
            let field_str = interner.get(*field_name);

            // 構造体名もソート
            let mut struct_strs: Vec<_> = struct_names
                .iter()
                .map(|s| interner.get(*s))
                .collect();
            struct_strs.sort();

            result.push_str(&format!(
                "{} -> {}\n",
                field_str,
                struct_strs.join(", ")
            ));
        }

        result
    }

    /// 一意なフィールドのみをダンプ
    pub fn dump_unique(&self, interner: &StringInterner) -> String {
        let mut result = String::new();

        // フィールド名でソートして出力
        let mut entries: Vec<_> = self.field_to_structs
            .iter()
            .filter(|(_, structs)| structs.len() == 1)
            .collect();
        entries.sort_by_key(|(field, _)| interner.get(**field));

        for (field_name, struct_names) in entries {
            let field_str = interner.get(*field_name);
            let struct_str = interner.get(*struct_names.iter().next().unwrap());
            result.push_str(&format!("{} -> {}\n", field_str, struct_str));
        }

        result
    }

    /// 統計情報を取得
    pub fn stats(&self) -> FieldsDictStats {
        let total_fields = self.field_to_structs.len();
        let unique_fields = self.field_to_structs
            .values()
            .filter(|s| s.len() == 1)
            .count();
        let ambiguous_fields = total_fields - unique_fields;

        FieldsDictStats {
            total_fields,
            unique_fields,
            ambiguous_fields,
        }
    }

    /// フィールド型情報をダンプ（デバッグ用）
    pub fn dump_field_types(&self, interner: &StringInterner) -> String {
        let mut result = String::new();

        // (構造体名, フィールド名) でソートして出力
        let mut entries: Vec<_> = self.field_types.iter().collect();
        entries.sort_by_key(|((struct_name, field_name), _)| {
            (interner.get(*struct_name), interner.get(*field_name))
        });

        for ((struct_name, field_name), field_type) in entries {
            let struct_str = interner.get(*struct_name);
            let field_str = interner.get(*field_name);
            result.push_str(&format!(
                "{}.{}: {}\n",
                struct_str, field_str, field_type.rust_type
            ));
        }

        result
    }
}

/// 辞書の統計情報
#[derive(Debug)]
pub struct FieldsDictStats {
    pub total_fields: usize,
    pub unique_fields: usize,
    pub ambiguous_fields: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fields_dict() {
        // 基本的なテストは実際のパース結果で行う
    }
}
