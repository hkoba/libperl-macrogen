//! 構造体フィールド名辞書
//!
//! マクロの引数型推論のため、フィールド名から構造体名への
//! マッピングを記録する。

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::ast::{Declaration, ExternalDecl, StructSpec, TypeSpec};
use crate::intern::{InternedStr, StringInterner};

/// フィールド名から構造体名へのマッピング
#[derive(Debug, Default)]
pub struct FieldsDict {
    /// フィールド名 -> 構造体名のセット
    /// (同じフィールド名が複数の構造体で使われる可能性があるためHashSet)
    field_to_structs: HashMap<InternedStr, HashSet<InternedStr>>,
    /// 収集対象のディレクトリパス
    target_dirs: Vec<String>,
}

impl FieldsDict {
    /// 新しい辞書を作成
    pub fn new() -> Self {
        Self::default()
    }

    /// 収集対象ディレクトリを追加
    pub fn add_target_dir(&mut self, dir: &str) {
        self.target_dirs.push(dir.to_string());
    }

    /// 指定されたパスが収集対象かどうかを判定
    fn is_target_path(&self, path: &Path) -> bool {
        if self.target_dirs.is_empty() {
            return true; // ターゲットが指定されていなければ全て対象
        }
        let path_str = path.to_string_lossy();
        self.target_dirs.iter().any(|dir| path_str.starts_with(dir))
    }

    /// 外部宣言からフィールド情報を収集
    pub fn collect_from_external_decl(
        &mut self,
        decl: &ExternalDecl,
        path: &Path,
    ) {
        // パスが収集対象かチェック
        if !self.is_target_path(path) {
            return;
        }

        if let ExternalDecl::Declaration(d) = decl {
            self.collect_from_declaration(d);
        }
    }

    /// 宣言からフィールド情報を収集
    fn collect_from_declaration(&mut self, decl: &Declaration) {
        for type_spec in &decl.specs.type_specs {
            match type_spec {
                TypeSpec::Struct(spec) => {
                    self.collect_from_struct_spec(spec);
                }
                TypeSpec::Union(spec) => {
                    // 共用体も同様に収集
                    self.collect_from_struct_spec(spec);
                }
                _ => {}
            }
        }
    }

    /// 構造体指定からフィールド情報を収集
    fn collect_from_struct_spec(&mut self, spec: &StructSpec) {
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
                        self.collect_from_struct_spec(nested);
                    }
                    TypeSpec::Union(nested) => {
                        self.collect_from_struct_spec(nested);
                    }
                    _ => {}
                }
            }

            // フィールド名を収集
            for decl in &member.declarators {
                if let Some(ref declarator) = decl.declarator {
                    if let Some(field_name) = declarator.name {
                        self.field_to_structs
                            .entry(field_name)
                            .or_insert_with(HashSet::new)
                            .insert(struct_name);
                    }
                }
            }
        }
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
