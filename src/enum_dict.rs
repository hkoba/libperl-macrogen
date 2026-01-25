//! Enum バリアント辞書
//!
//! マクロのコード生成のため、enum バリアント名から enum 名への
//! マッピングを記録する。

use std::collections::{HashMap, HashSet};

use crate::ast::{Declaration, ExternalDecl, EnumSpec, TypeSpec, StorageClass};
use crate::intern::{InternedStr, StringInterner};

/// Enum バリアント名 → Enum 名のマッピング
#[derive(Debug, Default)]
pub struct EnumDict {
    /// バリアント名 → enum 名
    /// 同じバリアント名が複数の enum で使われる可能性は低いが、念のため HashSet
    variant_to_enum: HashMap<InternedStr, HashSet<InternedStr>>,

    /// enum 名 → バリアント名リスト
    enum_to_variants: HashMap<InternedStr, Vec<InternedStr>>,

    /// target ディレクトリで定義された enum 名のセット
    target_enums: HashSet<InternedStr>,
}

impl EnumDict {
    /// 新しい辞書を作成
    pub fn new() -> Self {
        Self::default()
    }

    /// 外部宣言から enum 情報を収集
    /// is_target: この宣言がターゲットディレクトリ内で定義されたかどうか
    pub fn collect_from_external_decl(
        &mut self,
        decl: &ExternalDecl,
        is_target: bool,
        _interner: &StringInterner,
    ) {
        if let ExternalDecl::Declaration(d) = decl {
            self.collect_from_declaration(d, is_target);
        }
    }

    /// 宣言から enum 情報を収集
    fn collect_from_declaration(&mut self, decl: &Declaration, is_target: bool) {
        // typedef enum { ... } Name; の場合、typedef 名を取得
        let typedef_name = if decl.specs.storage == Some(StorageClass::Typedef) {
            decl.declarators.first().and_then(|init_decl| {
                init_decl.declarator.name
            })
        } else {
            None
        };

        for type_spec in &decl.specs.type_specs {
            if let TypeSpec::Enum(spec) = type_spec {
                self.collect_from_enum_spec(spec, typedef_name, is_target);
            }
        }
    }

    /// EnumSpec から情報を収集
    fn collect_from_enum_spec(
        &mut self,
        spec: &EnumSpec,
        typedef_name: Option<InternedStr>,
        is_target: bool,
    ) {
        // enum 名を決定: typedef 名があればそれを優先、なければ enum タグ名
        let enum_name = typedef_name.or(spec.name);

        // enum 名がない場合（anonymous enum）はスキップ
        let enum_name = match enum_name {
            Some(name) => name,
            None => return,
        };

        // enumerators がない場合（前方宣言など）はスキップ
        let enumerators = match &spec.enumerators {
            Some(e) => e,
            None => return,
        };

        // バリアント名を収集
        let variants: Vec<InternedStr> = enumerators.iter().map(|e| e.name).collect();

        // 登録
        self.collect_enum(enum_name, &variants, is_target);
    }

    /// enum 定義を登録
    fn collect_enum(
        &mut self,
        enum_name: InternedStr,
        variants: &[InternedStr],
        is_target: bool,
    ) {
        for &variant in variants {
            self.variant_to_enum
                .entry(variant)
                .or_default()
                .insert(enum_name);
        }
        self.enum_to_variants.insert(enum_name, variants.to_vec());
        if is_target {
            self.target_enums.insert(enum_name);
        }
    }

    /// バリアント名から enum 名を取得（一意の場合のみ Some）
    pub fn get_enum_for_variant(&self, variant: InternedStr) -> Option<InternedStr> {
        self.variant_to_enum.get(&variant).and_then(|enums| {
            if enums.len() == 1 {
                enums.iter().next().copied()
            } else {
                None // 複数の enum で同名バリアントがある場合は None
            }
        })
    }

    /// バリアント名かどうかをチェック
    pub fn is_enum_variant(&self, name: InternedStr) -> bool {
        self.variant_to_enum.contains_key(&name)
    }

    /// target ディレクトリで定義された enum のイテレータ
    pub fn target_enums(&self) -> impl Iterator<Item = InternedStr> + '_ {
        self.target_enums.iter().copied()
    }

    /// enum 名のリストを取得（ソート済み）
    pub fn target_enum_names<'a>(&'a self, interner: &'a StringInterner) -> Vec<&'a str> {
        let mut names: Vec<&str> = self.target_enums
            .iter()
            .map(|&name| interner.get(name))
            .collect();
        names.sort();
        names
    }
}
