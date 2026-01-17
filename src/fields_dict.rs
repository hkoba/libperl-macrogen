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
    /// SV ファミリーメンバー（_SV_HEAD マクロを使用する構造体）
    /// 動的に検出される
    sv_family_members: HashSet<InternedStr>,
    /// _SV_HEAD(typeName) の typeName → 構造体名のマッピング
    /// 例: "XPVAV" → av, "XPVCV" → cv
    /// SvANY キャストパターンによる型推論に使用
    sv_head_type_to_struct: HashMap<String, InternedStr>,
    /// sv_u ユニオンフィールドの型マッピング
    /// key: フィールド名 (例: svu_pv, svu_hash)
    /// value: フィールドの C 型文字列 (例: "char*", "HE**")
    sv_u_field_types: HashMap<InternedStr, String>,
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
                        // sv_u union を検出して C 型を収集
                        if self.is_sv_u_union_member(member, interner) {
                            self.collect_sv_u_union_fields(nested, interner);
                        }
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

    /// メンバーが sv_u という名前の union かどうかを判定
    fn is_sv_u_union_member(&self, member: &crate::ast::StructMember, interner: &StringInterner) -> bool {
        // declarators に sv_u という名前があるか確認
        for decl in &member.declarators {
            if let Some(ref declarator) = decl.declarator {
                if let Some(name) = declarator.name {
                    if interner.get(name) == "sv_u" {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// sv_u union の内部フィールドから C 型を収集
    ///
    /// union のメンバーを走査し、フィールド名と C 型を sv_u_field_types に登録する。
    fn collect_sv_u_union_fields(&mut self, union_spec: &StructSpec, interner: &StringInterner) {
        let members = match &union_spec.members {
            Some(m) => m,
            None => return,
        };

        for member in members {
            for struct_decl in &member.declarators {
                if let Some(ref declarator) = struct_decl.declarator {
                    if let Some(field_name) = declarator.name {
                        // C 型を抽出
                        if let Some(c_type) = self.extract_c_type(&member.specs, declarator, interner) {
                            self.register_sv_u_field(field_name, c_type);
                        }
                    }
                }
            }
        }
    }

    /// DeclSpecs と Declarator から C 型文字列を抽出
    fn extract_c_type(&self, specs: &DeclSpecs, declarator: &Declarator, interner: &StringInterner) -> Option<String> {
        // 基本型を取得
        let base_type = self.extract_c_base_type(specs, interner)?;

        // ポインタを適用
        let mut result = base_type;
        for derived in &declarator.derived {
            if let DerivedDecl::Pointer { .. } = derived {
                result = format!("{}*", result);
            }
        }

        Some(result)
    }

    /// DeclSpecs から C 基本型を抽出
    fn extract_c_base_type(&self, specs: &DeclSpecs, interner: &StringInterner) -> Option<String> {
        for type_spec in &specs.type_specs {
            match type_spec {
                TypeSpec::Void => return Some("void".to_string()),
                TypeSpec::Char => return Some("char".to_string()),
                TypeSpec::Int => return Some("int".to_string()),
                TypeSpec::Short => return Some("short".to_string()),
                TypeSpec::Long => return Some("long".to_string()),
                TypeSpec::Float => return Some("float".to_string()),
                TypeSpec::Double => return Some("double".to_string()),
                TypeSpec::Unsigned => continue, // 修飾子、次のループで処理
                TypeSpec::Signed => continue,
                TypeSpec::Struct(s) | TypeSpec::Union(s) => {
                    if let Some(name) = s.name {
                        return Some(interner.get(name).to_string());
                    }
                }
                TypeSpec::TypedefName(name) => {
                    return Some(interner.get(*name).to_string());
                }
                _ => {}
            }
        }
        None
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

    /// 収集されたフィールド型の数を取得
    pub fn field_types_count(&self) -> usize {
        self.field_types.len()
    }

    // ==================== SV Family Detection ====================

    /// SV ファミリーメンバーと typeName を同時に登録
    ///
    /// _SV_HEAD(typeName) マクロの引数から、typeName → 構造体名のマッピングを構築する。
    /// 例: add_sv_family_member_with_type(av, "XPVAV*") → "XPVAV" → av
    ///
    /// void* は汎用型のためマッピングには含めない。
    pub fn add_sv_family_member_with_type(&mut self, struct_name: InternedStr, type_name: &str) {
        self.sv_family_members.insert(struct_name);

        // ポインタ記号を除去して正規化
        let normalized = type_name.trim().trim_end_matches('*').trim();
        if !normalized.is_empty() && normalized != "void" {
            self.sv_head_type_to_struct.insert(normalized.to_string(), struct_name);
        }
    }

    /// typeName から構造体名を取得
    ///
    /// _SV_HEAD(typeName) で登録された typeName から対応する構造体名を取得する。
    /// SvANY キャストパターン (例: (XPVAV*) SvANY(av)) の型推論に使用。
    pub fn get_struct_for_sv_head_type(&self, type_name: &str) -> Option<InternedStr> {
        let normalized = type_name.trim().trim_end_matches('*').trim();
        self.sv_head_type_to_struct.get(normalized).copied()
    }

    /// 動的に検出された SV ファミリーメンバーの数を取得
    pub fn sv_family_members_count(&self) -> usize {
        self.sv_family_members.len()
    }

    /// typeName → 構造体名マッピングの数を取得
    pub fn sv_head_type_mapping_count(&self) -> usize {
        self.sv_head_type_to_struct.len()
    }

    /// typeName → 構造体名マッピングをイテレート
    pub fn sv_head_type_to_struct_iter(&self) -> impl Iterator<Item = (&String, &InternedStr)> {
        self.sv_head_type_to_struct.iter()
    }

    // ==================== sv_u Union Field Types ====================

    /// sv_u ユニオンフィールドの型を登録
    ///
    /// SV ファミリー構造体に共通の sv_u union のフィールド型を登録する。
    /// 例: svu_pv → "char*", svu_hash → "HE**"
    pub fn register_sv_u_field(&mut self, field_name: InternedStr, c_type: String) {
        self.sv_u_field_types.insert(field_name, c_type);
    }

    /// sv_u ユニオンフィールドの型を取得
    ///
    /// フィールド名から対応する C 型を返す。
    /// 登録されていないフィールドの場合は None。
    pub fn get_sv_u_field_type(&self, field_name: InternedStr) -> Option<&str> {
        self.sv_u_field_types.get(&field_name).map(|s| s.as_str())
    }

    /// sv_u フィールド型の登録数を取得
    pub fn sv_u_field_types_count(&self) -> usize {
        self.sv_u_field_types.len()
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
