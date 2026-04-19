//! 構造体フィールド名辞書
//!
//! マクロの引数型推論のため、フィールド名から構造体名への
//! マッピングと、フィールドの型情報を記録する。

use std::collections::{HashMap, HashSet};

use crate::ast::{Declaration, DeclSpecs, Declarator, DerivedDecl, ExternalDecl, StorageClass, StructMember, StructSpec, TypeSpec};
use crate::intern::{InternedStr, StringInterner};
use crate::type_repr::TypeRepr;

/// 共通フィールドマクロが宣言する 1 つのフィールドの最小情報
#[derive(Debug, Clone)]
pub struct CommonField {
    /// フィールド名（最も内側、例: `xcv_xsub`）
    pub name: InternedStr,
    /// 関数ポインタ（`void (*name)(...)` 等）かどうか
    pub is_fn_pointer: bool,
    /// マクロ本体内での出現位置の素性
    pub origin: CommonFieldOrigin,
}

/// `CommonField` の出現位置素性
#[derive(Debug, Clone)]
pub enum CommonFieldOrigin {
    /// 直接のフィールド
    Direct,
    /// 無名 union/struct の中のフィールド（外側のフィールド名を持つ）
    InsideUnion { union_field: InternedStr },
}

/// 共通フィールドマクロが定義するフィールド集合
#[derive(Debug, Clone)]
pub struct CommonFieldMacro {
    pub name: InternedStr,
    pub fields: Vec<CommonField>,
}

/// `Declarator.derived` から関数ポインタを判定する。
///
/// C の関数ポインタ宣言 `void (*name)(args)` の `derived` は典型的に
/// `[Function(...), Pointer(...)]` の順で並ぶ（最内側が先頭）。
/// 関数自体（fn pointer ではなく fn 型）と区別するため、Pointer と
/// Function の両方が出現することを要求する。
fn is_fn_pointer_declarator(derived: &[DerivedDecl]) -> bool {
    let has_fn = derived.iter().any(|d| matches!(d, DerivedDecl::Function(_)));
    let has_ptr = derived.iter().any(|d| matches!(d, DerivedDecl::Pointer(_)));
    has_fn && has_ptr
}

/// `TypeRepr` が C 慣用句の flexible array member 型（`T[1]`、`T[0]`、または
/// C99 `T[]`）かを判定し、該当すれば配列を剥がした要素型を返す。
///
/// 検出条件: 最も外側の derived が `Array { size: Some(0|1) | None }`。
/// （`Array { size: Some(N>=2) }` は通常の固定長配列なので除外）
fn flexible_array_element_type(ty: &TypeRepr) -> Option<TypeRepr> {
    use crate::type_repr::CDerivedType;
    let TypeRepr::CType { specs, derived, source } = ty else {
        return None;
    };
    let last = derived.last()?;
    let is_flex = match last {
        CDerivedType::Array { size: None } => true,
        CDerivedType::Array { size: Some(0) } | CDerivedType::Array { size: Some(1) } => true,
        _ => false,
    };
    if !is_flex {
        return None;
    }
    let mut new_derived = derived.clone();
    new_derived.pop();
    Some(TypeRepr::CType {
        specs: specs.clone(),
        derived: new_derived,
        source: source.clone(),
    })
}

/// フィールドの型情報
#[derive(Debug, Clone)]
pub struct FieldType {
    /// 型情報（構造化された表現）
    pub type_repr: TypeRepr,
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
    consistent_type_cache: HashMap<InternedStr, Option<TypeRepr>>,
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
    /// 構造体名 → そこで展開された共通フィールドマクロ集合
    /// 例: xpvcv → {_XPV_HEAD, _XPVCV_COMMON}
    struct_to_common_macros: HashMap<InternedStr, Vec<InternedStr>>,
    /// 共通フィールドマクロ → それを使う構造体集合
    /// 例: _XPVCV_COMMON → [xpvcv, xpvfm]
    common_macro_to_structs: HashMap<InternedStr, Vec<InternedStr>>,
    /// 共通フィールドマクロ名 → そこで宣言されるフィールド情報
    /// 例: _XPVCV_COMMON → [{ name: xcv_xsub, is_fn_pointer: true, ... }, ...]
    common_macros: HashMap<InternedStr, CommonFieldMacro>,
    /// フィールド名 → そのフィールドを定義している共通マクロ名
    /// 例: xcv_xsub → _XPVCV_COMMON
    /// （無名 union 内のフィールドも含む）
    field_to_defining_macro: HashMap<InternedStr, InternedStr>,
    /// 共通フィールドマクロが宣言したフィールド名 → 整合性のある Rust 型
    /// （bindings.rs 由来）。`build_common_field_rust_types` で構築。
    common_field_rust_types: HashMap<InternedStr, TypeRepr>,
    /// 共通フィールドマクロ → 一意な SV ファミリー typedef 名
    /// 例: `_XPVCV_COMMON` → `CV` （xpvcv ボディ → struct cv → typedef CV、
    /// xpvfm 側は対応 SV 構造体無しで除外、結果一意）
    /// `build_common_macro_sv_family` で構築。
    common_macro_to_sv_family: HashMap<InternedStr, InternedStr>,
    /// 構造体最終メンバーが flexible array member（`char foo[1]` や `[0]`、`[]`）
    /// である場合の (struct_name, field_name) → 要素型（配列を剥がした TypeRepr）。
    /// C 慣用句で可変長バッファとして使われ、ポインタとして扱うべきもの。
    /// 例: `(struct hek, hek_key)` → `char` の TypeRepr
    flexible_array_fields: HashMap<(InternedStr, InternedStr), TypeRepr>,
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

        let last_idx = members.len().saturating_sub(1);
        for (m_idx, member) in members.iter().enumerate() {
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
            let is_last_member = m_idx == last_idx;
            let last_decl_idx = member.declarators.len().saturating_sub(1);
            for (d_idx, decl) in member.declarators.iter().enumerate() {
                if let Some(ref declarator) = decl.declarator {
                    if let Some(field_name) = declarator.name {
                        // フィールド名 -> 構造体名のマッピング
                        self.field_to_structs
                            .entry(field_name)
                            .or_insert_with(HashSet::new)
                            .insert(struct_name);

                        // フィールド型の収集
                        if let Some(type_repr) = self.extract_field_type(&member.specs, declarator, interner) {
                            // flexible array member の検出: 構造体の真の末尾メンバーで
                            // size 1 / 0 の固定長配列、または C99 [] 構文の可変長配列
                            let is_struct_last = is_last_member && d_idx == last_decl_idx;
                            if is_struct_last {
                                if let Some(elem) = flexible_array_element_type(&type_repr) {
                                    self.flexible_array_fields
                                        .insert((struct_name, field_name), elem);
                                }
                            }
                            self.field_types.insert(
                                (struct_name, field_name),
                                FieldType { type_repr },
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

    /// DeclSpecs と Declarator からフィールドの TypeRepr を抽出
    fn extract_field_type(&self, specs: &DeclSpecs, declarator: &Declarator, interner: &StringInterner) -> Option<TypeRepr> {
        // TypeRepr::from_decl を使用して構造化された型情報を生成
        Some(TypeRepr::from_decl(specs, declarator, interner))
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

    /// フィールドを持つ構造体群の共通親型を取得
    ///
    /// 複数の構造体がフィールドを持つ場合、それらの共通の親型を返す。
    /// 現在は SV ファミリーのみ対応:
    /// - フィールドを持つ全構造体が SV ファミリーメンバーの場合、"sv" を返す
    /// - それ以外の場合は None
    ///
    /// # 例
    ///
    /// `sv_flags` は `sv`, `av`, `hv`, `cv` 等に存在 → 共通親型は "sv"
    pub fn get_consistent_base_type(&self, field_name: InternedStr, interner: &StringInterner) -> Option<InternedStr> {
        let structs = self.field_to_structs.get(&field_name)?;

        // フィールドを持つ構造体がない場合は None
        if structs.is_empty() {
            return None;
        }

        // 全ての構造体が SV ファミリーメンバーかチェック
        let all_sv_family = structs.iter().all(|s| self.sv_family_members.contains(s));

        if all_sv_family {
            // "sv" を intern して返す
            // Note: interner.lookup は既存の文字列のみ返すので、
            // "sv" が未登録の場合は None になる可能性がある
            interner.lookup("sv")
        } else {
            None
        }
    }

    /// フィールド名から一意にフィールド型を特定（構造体が1つしかない場合）
    pub fn get_unique_field_type(&self, field_name: InternedStr) -> Option<&FieldType> {
        let struct_name = self.lookup_unique(field_name)?;
        self.field_types.get(&(struct_name, field_name))
    }

    /// InternedStr で直接フィールド型を取得（typedef 解決付き）
    pub fn get_field_type(
        &self,
        struct_name: InternedStr,
        field_name: InternedStr,
    ) -> Option<&FieldType> {
        // 直接検索
        if let Some(ft) = self.field_types.get(&(struct_name, field_name)) {
            return Some(ft);
        }
        // typedef 解決して再検索
        let resolved = self.resolve_typedef(struct_name)?;
        self.field_types.get(&(resolved, field_name))
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

    /// 構造体名から typedef 名を取得（逆引き）
    ///
    /// 同じ構造体に複数の typedef がある場合は最初に見つかったものを返す。
    /// typedef が登録されていない場合は None。
    pub fn get_typedef_for_struct(&self, struct_name: InternedStr) -> Option<InternedStr> {
        for (typedef_name, s_name) in &self.typedef_to_struct {
            if *s_name == struct_name {
                return Some(*typedef_name);
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

    /// 型名（typedef 名または構造体名）が SV ファミリーかどうかを判定
    pub fn is_sv_family_type(&self, type_name: InternedStr) -> bool {
        // 構造体名で直接チェック
        if self.sv_family_members.contains(&type_name) {
            return true;
        }
        // typedef 名 → 構造体名に解決してチェック
        if let Some(struct_name) = self.typedef_to_struct.get(&type_name) {
            return self.sv_family_members.contains(struct_name);
        }
        false
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

    // ==================== Common Field Macros ====================
    //
    // perl5 の `_XPV_HEAD` / `_XPVCV_COMMON` のような object-like macro を
    // 通じた共通フィールド宣言を構造体↔マクロのレベルで記録する
    // （`_SV_HEAD` の sv_family サポートを一般化したもの）。

    /// 構造体がある共通フィールドマクロを使用していることを記録する。
    pub fn add_struct_uses_common_macro(
        &mut self,
        struct_name: InternedStr,
        macro_name: InternedStr,
    ) {
        let v = self.struct_to_common_macros.entry(struct_name).or_default();
        if !v.contains(&macro_name) {
            v.push(macro_name);
        }
        let v = self.common_macro_to_structs.entry(macro_name).or_default();
        if !v.contains(&struct_name) {
            v.push(struct_name);
        }
    }

    /// あるマクロを使用している構造体一覧
    pub fn structs_using_common_macro(&self, macro_name: InternedStr) -> &[InternedStr] {
        self.common_macro_to_structs
            .get(&macro_name)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// ある構造体が使用している共通フィールドマクロ一覧
    pub fn common_macros_used_by_struct(&self, struct_name: InternedStr) -> &[InternedStr] {
        self.struct_to_common_macros
            .get(&struct_name)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// 観測対象の共通フィールドマクロ数（テスト・デバッグ用）
    pub fn common_macro_count(&self) -> usize {
        self.common_macro_to_structs.len()
    }

    /// あるフィールド名を宣言している共通マクロ（B-2 で構築）
    pub fn defining_macro_of(&self, field_name: InternedStr) -> Option<InternedStr> {
        self.field_to_defining_macro.get(&field_name).copied()
    }

    /// マクロ名から `CommonFieldMacro` を取得
    pub fn common_macro(&self, macro_name: InternedStr) -> Option<&CommonFieldMacro> {
        self.common_macros.get(&macro_name)
    }

    /// フィールド名から定義マクロと該当 `CommonField` をまとめて取得
    pub fn canonical_field(
        &self,
        field_name: InternedStr,
    ) -> Option<(&CommonFieldMacro, &CommonField)> {
        let macro_name = *self.field_to_defining_macro.get(&field_name)?;
        let cm = self.common_macros.get(&macro_name)?;
        let cf = cm.fields.iter().find(|f| f.name == field_name)?;
        Some((cm, cf))
    }

    /// 共通フィールドマクロ宣言フィールドの bindings.rs 由来 Rust 型
    /// （`build_common_field_rust_types` で構築）
    pub fn rust_type_of_common_field(&self, field_name: InternedStr) -> Option<&TypeRepr> {
        self.common_field_rust_types.get(&field_name)
    }

    /// 共通フィールドマクロから一意な SV ファミリー typedef 名を取得
    /// （`build_common_macro_sv_family` で構築）
    pub fn sv_family_of_common_macro(&self, macro_name: InternedStr) -> Option<InternedStr> {
        self.common_macro_to_sv_family.get(&macro_name).copied()
    }

    /// (struct_name, field_name) が flexible array member であれば、
    /// その要素型 (`T[1]` の `T`) を返す。typedef 解決付き
    /// （例: 引数に `HEK` を渡しても `hek` のエントリを返す）。
    /// `collect_from_struct_spec` で構造体最終メンバーが size 1/0 配列、または
    /// C99 `[]` の場合に登録される。
    pub fn flexible_array_element(&self, struct_name: InternedStr, field_name: InternedStr)
        -> Option<&TypeRepr>
    {
        if let Some(t) = self.flexible_array_fields.get(&(struct_name, field_name)) {
            return Some(t);
        }
        let resolved = self.resolve_typedef(struct_name)?;
        self.flexible_array_fields.get(&(resolved, field_name))
    }

    /// `flexible_array_element` の便利版（要素型が不要な場合）
    pub fn is_flexible_array_field(&self, struct_name: InternedStr, field_name: InternedStr) -> bool {
        self.flexible_array_element(struct_name, field_name).is_some()
    }

    /// 共通フィールドマクロ → 対応する SV ファミリー typedef 名の事前マッピング
    /// を構築する。
    ///
    /// 各 xpv ボディ構造体（例 xpvcv）について:
    ///   xpv struct → typedef "XPVCV" → sv_head_type_to_struct で SV 構造体 cv
    ///     → typedef "CV"
    /// と辿り、全ての xpv について typedef が一意に決まる場合のみ登録する。
    pub fn build_common_macro_sv_family(&mut self, interner: &StringInterner) {
        let mut new_map: HashMap<InternedStr, InternedStr> = HashMap::new();
        for (&macro_id, struct_names) in &self.common_macro_to_structs {
            let mut sv_typedefs: HashSet<InternedStr> = HashSet::new();
            for &xpv_struct in struct_names {
                let xpv_typedef = match self.get_typedef_for_struct(xpv_struct) {
                    Some(td) => td,
                    None => continue,
                };
                let xpv_typedef_str = interner.get(xpv_typedef);
                let sv_struct = match self.sv_head_type_to_struct.get(xpv_typedef_str) {
                    Some(&s) => s,
                    None => continue, // 対応する SV ファミリー無し
                };
                if let Some(td) = self.get_typedef_for_struct(sv_struct) {
                    sv_typedefs.insert(td);
                }
            }
            if sv_typedefs.len() == 1 {
                new_map.insert(macro_id, *sv_typedefs.iter().next().unwrap());
            }
        }
        self.common_macro_to_sv_family = new_map;
    }

    /// `RustDeclDict.structs` (bindgen の Item::Struct と Item::Union を含む)
    /// を走査し、共通フィールドマクロ宣言フィールドについて整合性のある Rust 型を
    /// 収集する。
    ///
    /// あるフィールド名が複数の bindings.rs エントリで異なる型を持つ場合、
    /// マップから除外する（`build_field_type_map` と同じポリシー）。
    pub fn build_common_field_rust_types(
        &mut self,
        rust_dict: &crate::rust_decl::RustDeclDict,
        interner: &mut StringInterner,
    ) {
        use std::collections::hash_map::Entry;
        // 関心対象: 共通マクロ宣言フィールド名集合
        let target_names: HashSet<InternedStr> = self
            .common_macros
            .values()
            .flat_map(|m| m.fields.iter().map(|f| f.name))
            .collect();
        if target_names.is_empty() {
            return;
        }

        let mut acc: HashMap<InternedStr, String> = HashMap::new();
        let mut conflicts: HashSet<InternedStr> = HashSet::new();

        for rust_struct in rust_dict.structs.values() {
            for field in &rust_struct.fields {
                let id = interner.intern(&field.name);
                if !target_names.contains(&id) {
                    continue;
                }
                if conflicts.contains(&id) {
                    continue;
                }
                match acc.entry(id) {
                    Entry::Vacant(e) => {
                        e.insert(field.ty.clone());
                    }
                    Entry::Occupied(e) => {
                        if e.get() != &field.ty {
                            conflicts.insert(id);
                            e.remove();
                        }
                    }
                }
            }
        }

        for (id, ty_str) in acc {
            let repr = TypeRepr::RustType {
                repr: crate::type_repr::RustTypeRepr::from_type_string(&ty_str),
                source: crate::type_repr::RustTypeSource::Parsed { raw: ty_str },
            };
            self.common_field_rust_types.insert(id, repr);
        }
    }

    /// 共通フィールドマクロの canonical field set を構築する。
    ///
    /// B-1 で `add_struct_uses_common_macro` により記録された各共通マクロに
    /// ついて、`MacroDef.body` を struct member 列としてパースし、
    /// `CommonFieldMacro` と `field_to_defining_macro` を構築する。
    ///
    /// `parse_struct_members` は呼び出し側に委ねる関数（典型的には
    /// `crate::parser::parse_struct_members_from_tokens_ref` をラップしたもの）。
    /// これは `FieldsDict` が `Preprocessor` への直接依存を避けるため。
    pub fn build_common_macro_fields<F>(
        &mut self,
        macro_bodies: &[(InternedStr, Vec<crate::token::Token>)],
        mut parse_struct_members: F,
    ) where
        F: FnMut(Vec<crate::token::Token>) -> Result<Vec<StructMember>, crate::error::CompileError>,
    {
        for (macro_name, body) in macro_bodies {
            let members = match parse_struct_members(body.clone()) {
                Ok(m) => m,
                Err(_) => continue, // 解析失敗は黙殺
            };
            let mut fields = Vec::new();
            for member in &members {
                Self::collect_common_fields_from_member(member, CommonFieldOrigin::Direct, &mut fields);
            }
            for f in &fields {
                self.field_to_defining_macro.insert(f.name, *macro_name);
            }
            self.common_macros.insert(
                *macro_name,
                CommonFieldMacro { name: *macro_name, fields },
            );
        }
    }

    /// `StructMember` から `CommonField` を抽出（無名 union/struct を再帰）
    fn collect_common_fields_from_member(
        member: &StructMember,
        outer_origin: CommonFieldOrigin,
        out: &mut Vec<CommonField>,
    ) {
        // 直下の declarator を見る
        for decl in &member.declarators {
            let Some(declarator) = decl.declarator.as_ref() else { continue };
            let Some(field_name) = declarator.name else { continue };
            let is_fn_pointer = is_fn_pointer_declarator(&declarator.derived);
            out.push(CommonField {
                name: field_name,
                is_fn_pointer,
                origin: outer_origin.clone(),
            });

            // この declarator が無名 union/struct の名前なら、その中身も収集する
            // （例: `union { ... } xcv_root_u` の `xcv_root_u`）
            for type_spec in &member.specs.type_specs {
                let nested = match type_spec {
                    TypeSpec::Struct(s) | TypeSpec::Union(s) => s,
                    _ => continue,
                };
                if nested.name.is_some() {
                    continue; // 名前付き struct/union（typedef 経由など）はスキップ
                }
                let Some(inner_members) = &nested.members else { continue };
                for inner in inner_members {
                    Self::collect_common_fields_from_member(
                        inner,
                        CommonFieldOrigin::InsideUnion { union_field: field_name },
                        out,
                    );
                }
            }
        }
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
    pub fn build_consistent_type_cache(&mut self, interner: &StringInterner) {
        self.consistent_type_cache.clear();

        // field_to_structs を先にクローンして borrow を分離
        let field_structs: Vec<_> = self.field_to_structs.iter()
            .map(|(&k, v)| (k, v.clone()))
            .collect();

        for (field_name, structs) in field_structs {
            let consistent_type = self.compute_consistent_type(field_name, &structs, interner);
            self.consistent_type_cache.insert(field_name, consistent_type);
        }
    }

    /// 一致型を計算（内部用）
    ///
    /// TypeRepr を保持しつつ、型の比較には to_rust_string() を使用する
    /// （source が異なっても型が同じなら一致とみなす）
    fn compute_consistent_type(
        &self,
        field_name: InternedStr,
        structs: &HashSet<InternedStr>,
        interner: &StringInterner,
    ) -> Option<TypeRepr> {
        if structs.is_empty() {
            return None;
        }

        let mut first_type: Option<(&TypeRepr, String)> = None;

        for struct_name in structs {
            if let Some(ft) = self.field_types.get(&(*struct_name, field_name)) {
                let type_str = ft.type_repr.to_rust_string(interner);
                match &first_type {
                    None => first_type = Some((&ft.type_repr, type_str)),
                    Some((_, first_str)) if first_str != &type_str => return None, // 不一致
                    Some(_) => {} // 一致、続行
                }
            }
        }

        first_type.map(|(tr, _)| tr.clone())
    }

    /// キャッシュから一致型を取得（O(1)）
    ///
    /// フィールドが全バリアントで同じ型を持つ場合、その型を返す。
    /// 型が不一致、またはフィールドが存在しない場合は None。
    pub fn get_consistent_field_type(&self, field_name: InternedStr) -> Option<&TypeRepr> {
        self.consistent_type_cache
            .get(&field_name)
            .and_then(|opt| opt.as_ref())
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
            let type_str = field_type.type_repr.to_rust_string(interner);
            result.push_str(&format!(
                "{}.{}: {}\n",
                struct_str, field_str, type_str
            ));
        }

        result
    }

    /// typedef マッピング情報をダンプ（デバッグ用）
    pub fn dump_typedefs(&self, interner: &StringInterner) -> String {
        let mut result = String::new();

        // typedef 名でソートして出力
        let mut entries: Vec<_> = self.typedef_to_struct.iter().collect();
        entries.sort_by_key(|(typedef_name, _)| interner.get(**typedef_name));

        for (typedef_name, struct_name) in entries {
            let typedef_str = interner.get(*typedef_name);
            let struct_str = interner.get(*struct_name);
            result.push_str(&format!("typedef {} = struct {}\n", typedef_str, struct_str));
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
