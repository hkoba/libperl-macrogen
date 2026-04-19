//! Global static const declaration の捕捉
//!
//! C ヘッダの `static const struct X NAME[] = { ... };` 形式の宣言を
//! parse 時に保存し、後段の `static_array_emitter` が Rust の `static`
//! 配列定義に翻訳できるようにする。
//!
//! 典型例: `sv_inline.h` の `bodies_by_type[]`。Rust 側で `Perl_newSV_type`
//! が参照するために必要。

use std::collections::HashMap;

use crate::ast::{Declaration, ExternalDecl, Initializer, StorageClass};
use crate::intern::{InternedStr, StringInterner};
use crate::source::SourceLocation;
use crate::type_repr::TypeRepr;

/// 1 つの global static const declaration の保存形式
#[derive(Debug, Clone)]
pub struct GlobalConstDecl {
    /// 変数名
    pub name: InternedStr,
    /// 配列要素の型 TypeRepr（配列 derived は除外したもの）
    pub element_type: TypeRepr,
    /// 配列サイズ。`[]` の場合は None（initializer から要素数を推定）
    pub array_size: Option<usize>,
    /// 初期化子（Initializer::List 想定）
    pub initializer: Initializer,
    /// 元宣言の出所（デバッグ用）
    pub loc: SourceLocation,
}

/// global const decl の辞書
#[derive(Debug, Default)]
pub struct GlobalConstDict {
    decls: HashMap<InternedStr, GlobalConstDecl>,
}

impl GlobalConstDict {
    pub fn new() -> Self {
        Self::default()
    }

    /// 1 つの宣言を試行的に登録する。`static const` でかつ initializer 付き、
    /// かつ要素型が struct/typedef のもののみ受容する。それ以外は無視。
    /// 同名の宣言は最初のものを保持。
    pub fn try_collect(
        &mut self,
        decl: &ExternalDecl,
        is_target: bool,
        interner: &StringInterner,
    ) {
        if !is_target {
            return;
        }
        let d = match decl {
            ExternalDecl::Declaration(d) => d,
            _ => return,
        };
        // storage = static, qualifier const
        if d.specs.storage != Some(StorageClass::Static) {
            return;
        }
        if !d.specs.qualifiers.is_const {
            return;
        }
        for init_decl in &d.declarators {
            let name = match init_decl.declarator.name {
                Some(n) => n,
                None => continue,
            };
            // initializer 必須
            let init = match &init_decl.init {
                Some(i) => i.clone(),
                None => continue,
            };
            // 配列 derived の解析（[N] の N または [] = None）
            let mut array_size: Option<usize> = None;
            let mut is_array = false;
            for d in &init_decl.declarator.derived {
                if let crate::ast::DerivedDecl::Array(arr) = d {
                    is_array = true;
                    if let Some(sz_expr) = &arr.size {
                        if let crate::ast::ExprKind::IntLit(n) = &sz_expr.kind {
                            array_size = Some(*n as usize);
                        }
                    }
                }
            }
            if !is_array {
                continue; // 当面は配列のみ対応
            }
            // 要素型: derived から Array を取り除いた TypeRepr を構築
            let element_type = build_element_type(d, init_decl, interner);
            let entry = GlobalConstDecl {
                name,
                element_type,
                array_size,
                initializer: init,
                loc: d.loc().clone(),
            };
            self.decls.entry(name).or_insert(entry);
        }
    }

    pub fn get(&self, name: InternedStr) -> Option<&GlobalConstDecl> {
        self.decls.get(&name)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&InternedStr, &GlobalConstDecl)> {
        self.decls.iter()
    }

    pub fn len(&self) -> usize {
        self.decls.len()
    }

    pub fn is_empty(&self) -> bool {
        self.decls.is_empty()
    }
}

/// `Declaration` + `InitDeclarator` から、配列 derived を除外した
/// 要素型を `TypeRepr` として構築する。
fn build_element_type(
    decl: &Declaration,
    init_decl: &crate::ast::InitDeclarator,
    interner: &StringInterner,
) -> TypeRepr {
    use crate::type_repr::CDerivedType;
    // 配列 derived を除いた declarator を仮想的に作る
    let dropped_derived: Vec<crate::ast::DerivedDecl> = init_decl.declarator.derived
        .iter()
        .filter(|d| !matches!(d, crate::ast::DerivedDecl::Array(_)))
        .cloned()
        .collect();
    let synthetic_decl = crate::ast::Declarator {
        name: init_decl.declarator.name,
        derived: dropped_derived,
        loc: init_decl.declarator.loc.clone(),
    };
    let mut ty = TypeRepr::from_decl(&decl.specs, &synthetic_decl, interner);
    // 念のため derived に Array が残っていないことを確認
    if let TypeRepr::CType { derived, .. } = &mut ty {
        derived.retain(|d| !matches!(d, CDerivedType::Array { .. }));
    }
    ty
}
