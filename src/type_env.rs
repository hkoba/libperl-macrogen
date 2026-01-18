//! 型環境・制約管理モジュール
//!
//! マクロ型推論のための型制約を収集・管理する。
//! ExprId に紐づく型制約を複数ソースから収集し、
//! 簡約せずにそのまま保持して観察可能にする。

use std::collections::HashMap;

use crate::ast::ExprId;
use crate::intern::InternedStr;
use crate::type_repr::TypeRepr;

/// 型制約の出所を区別するための列挙型
///
/// 注意: unified_type::TypeSource とは異なる（こちらは制約の出所分類用）
///
/// 非推奨: TypeRepr に統合されました。段階的移行のために残されています。
#[deprecated(note = "Use TypeRepr variants instead which include source information")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConstraintSource {
    /// C ヘッダーのパース結果から取得
    CHeader,
    /// bindings.rs（Rust バインディング）から取得
    RustBindings,
    /// apidoc（embed.fnc 等）から取得
    Apidoc,
    /// inline 関数の AST から取得
    InlineFn,
    /// 推論で導出
    Inferred,
}

#[allow(deprecated)]
impl std::fmt::Display for ConstraintSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CHeader => write!(f, "c-header"),
            Self::RustBindings => write!(f, "rust-bindings"),
            Self::Apidoc => write!(f, "apidoc"),
            Self::InlineFn => write!(f, "inline-fn"),
            Self::Inferred => write!(f, "inferred"),
        }
    }
}

/// 型制約
///
/// 簡約せずにそのまま保持し、デバッグ・観察可能にする。
/// 出所情報は TypeRepr 内に含まれる。
#[derive(Debug, Clone)]
pub struct TypeConstraint {
    /// 対象となる式の ID
    pub expr_id: ExprId,
    /// 構造化された型表現（出所情報を含む）
    pub ty: TypeRepr,
    /// デバッグ用コンテキスト（どこで取得したか）
    pub context: String,
}

impl TypeConstraint {
    /// 新しい型制約を作成
    pub fn new(expr_id: ExprId, ty: TypeRepr, context: impl Into<String>) -> Self {
        Self {
            expr_id,
            ty,
            context: context.into(),
        }
    }

    /// 後方互換: 旧シグネチャで型制約を作成
    ///
    /// 段階的移行用。新規コードでは `new()` を使用すること。
    #[deprecated(note = "Use new() with TypeRepr instead")]
    #[allow(deprecated)]
    pub fn from_legacy(
        expr_id: ExprId,
        ty: impl Into<String>,
        source: ConstraintSource,
        context: impl Into<String>,
    ) -> Self {
        let ty_str = ty.into();
        let source_str = match source {
            ConstraintSource::CHeader => "c-header",
            ConstraintSource::RustBindings => "rust-bindings",
            ConstraintSource::Apidoc => "apidoc",
            ConstraintSource::InlineFn => "inline-fn",
            ConstraintSource::Inferred => "inferred",
        };
        #[allow(deprecated)]
        let type_repr = TypeRepr::from_legacy_string(&ty_str, source_str);
        Self {
            expr_id,
            ty: type_repr,
            context: context.into(),
        }
    }

    /// 出所の表示用文字列を取得
    pub fn source_display(&self) -> &'static str {
        self.ty.source_display()
    }
}

/// ExprId とパラメータ名のリンク情報
#[derive(Debug, Clone)]
pub struct ParamLink {
    /// 式 ID
    pub expr_id: ExprId,
    /// パラメータ名
    pub param_name: InternedStr,
    /// リンクのコンテキスト
    pub context: String,
}

/// 型環境
///
/// マクロの型推論に使用する型制約を収集・管理する。
/// パラメータ、式、戻り値それぞれに対する制約を保持する。
#[derive(Debug, Clone, Default)]
pub struct TypeEnv {
    /// パラメータ名 → 型制約リスト
    pub param_constraints: HashMap<InternedStr, Vec<TypeConstraint>>,

    /// ExprId → 型制約リスト
    pub expr_constraints: HashMap<ExprId, Vec<TypeConstraint>>,

    /// 戻り値の型制約
    pub return_constraints: Vec<TypeConstraint>,

    /// ExprId → パラメータ名のリンク（正引き）
    pub expr_to_param: Vec<ParamLink>,

    /// パラメータ名 → ExprId リスト（逆引き）
    ///
    /// パラメータを参照する全ての式の ExprId を保持。
    /// 引数の型推論時に、関連する式の型制約を探すために使用。
    pub param_to_exprs: HashMap<InternedStr, Vec<ExprId>>,
}

impl TypeEnv {
    /// 新しい型環境を作成
    pub fn new() -> Self {
        Self::default()
    }

    /// パラメータに型制約を追加
    pub fn add_param_constraint(&mut self, param: InternedStr, constraint: TypeConstraint) {
        self.param_constraints
            .entry(param)
            .or_default()
            .push(constraint);
    }

    /// 式に型制約を追加
    pub fn add_expr_constraint(&mut self, constraint: TypeConstraint) {
        self.expr_constraints
            .entry(constraint.expr_id)
            .or_default()
            .push(constraint);
    }

    /// 汎用的な制約追加メソッド
    pub fn add_constraint(&mut self, constraint: TypeConstraint) {
        self.add_expr_constraint(constraint);
    }

    /// 戻り値に型制約を追加
    pub fn add_return_constraint(&mut self, constraint: TypeConstraint) {
        self.return_constraints.push(constraint);
    }

    /// 式をパラメータにリンク
    ///
    /// 正引き（expr_to_param）と逆引き（param_to_exprs）の両方を更新する。
    pub fn link_expr_to_param(&mut self, expr_id: ExprId, param_name: InternedStr, context: impl Into<String>) {
        // 正引き: ExprId → パラメータ名
        self.expr_to_param.push(ParamLink {
            expr_id,
            param_name,
            context: context.into(),
        });

        // 逆引き: パラメータ名 → ExprId リスト
        self.param_to_exprs
            .entry(param_name)
            .or_default()
            .push(expr_id);
    }

    /// パラメータの制約を取得
    pub fn get_param_constraints(&self, param: InternedStr) -> Option<&Vec<TypeConstraint>> {
        self.param_constraints.get(&param)
    }

    /// 式の制約を取得
    pub fn get_expr_constraints(&self, expr_id: ExprId) -> Option<&Vec<TypeConstraint>> {
        self.expr_constraints.get(&expr_id)
    }

    /// 式に紐づくパラメータ名を取得
    pub fn get_linked_param(&self, expr_id: ExprId) -> Option<InternedStr> {
        self.expr_to_param
            .iter()
            .find(|link| link.expr_id == expr_id)
            .map(|link| link.param_name)
    }

    /// パラメータ制約の総数
    pub fn param_constraint_count(&self) -> usize {
        self.param_constraints.values().map(|v| v.len()).sum()
    }

    /// 式制約の総数
    pub fn expr_constraint_count(&self) -> usize {
        self.expr_constraints.values().map(|v| v.len()).sum()
    }

    /// 戻り値制約の数
    pub fn return_constraint_count(&self) -> usize {
        self.return_constraints.len()
    }

    /// 戻り値の型を取得（最初の制約から）
    pub fn get_return_type(&self) -> Option<&TypeRepr> {
        self.return_constraints.first().map(|c| &c.ty)
    }

    /// 全制約の総数
    pub fn total_constraint_count(&self) -> usize {
        self.param_constraint_count() + self.expr_constraint_count() + self.return_constraint_count()
    }

    /// 環境が空かどうか
    pub fn is_empty(&self) -> bool {
        self.param_constraints.is_empty()
            && self.expr_constraints.is_empty()
            && self.return_constraints.is_empty()
    }

    /// 他の型環境をマージ
    pub fn merge(&mut self, other: TypeEnv) {
        for (param, constraints) in other.param_constraints {
            self.param_constraints
                .entry(param)
                .or_default()
                .extend(constraints);
        }
        for (expr_id, constraints) in other.expr_constraints {
            self.expr_constraints
                .entry(expr_id)
                .or_default()
                .extend(constraints);
        }
        self.return_constraints.extend(other.return_constraints);
        self.expr_to_param.extend(other.expr_to_param);

        // 逆引き辞書もマージ
        for (param, expr_ids) in other.param_to_exprs {
            self.param_to_exprs
                .entry(param)
                .or_default()
                .extend(expr_ids);
        }
    }

    /// デバッグ用: 制約のサマリを文字列で取得
    pub fn summary(&self) -> String {
        format!(
            "TypeEnv {{ params: {}, exprs: {}, returns: {}, links: {} }}",
            self.param_constraints.len(),
            self.expr_constraints.len(),
            self.return_constraints.len(),
            self.expr_to_param.len(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intern::StringInterner;
    use crate::type_repr::{CTypeSource, CTypeSpecs, InferredType, IntSize, RustTypeRepr, RustTypeSource};

    /// テスト用: C ヘッダー由来の int 型を作成
    fn c_int_type() -> TypeRepr {
        TypeRepr::CType {
            specs: CTypeSpecs::Int { signed: true, size: IntSize::Int },
            derived: vec![],
            source: CTypeSource::Header,
        }
    }

    /// テスト用: Rust bindings 由来の c_int 型を作成
    fn rust_c_int_type() -> TypeRepr {
        TypeRepr::RustType {
            repr: RustTypeRepr::from_type_string("c_int"),
            source: RustTypeSource::FnParam { func_name: "test".to_string(), param_index: 0 },
        }
    }

    /// テスト用: Apidoc 由来の SV * 型を作成
    fn apidoc_sv_ptr_type() -> TypeRepr {
        let interner = StringInterner::new();
        TypeRepr::from_apidoc_string("SV *", &interner)
    }

    #[test]
    fn test_type_env_new() {
        let env = TypeEnv::new();
        assert!(env.is_empty());
        assert_eq!(env.total_constraint_count(), 0);
    }

    #[test]
    fn test_add_expr_constraint() {
        let mut env = TypeEnv::new();
        let expr_id = ExprId::next();

        let constraint = TypeConstraint::new(
            expr_id,
            c_int_type(),
            "test context",
        );

        env.add_expr_constraint(constraint);

        assert!(!env.is_empty());
        assert_eq!(env.expr_constraint_count(), 1);
        assert_eq!(env.get_expr_constraints(expr_id).unwrap().len(), 1);
    }

    #[test]
    fn test_add_multiple_constraints() {
        let mut env = TypeEnv::new();
        let expr_id = ExprId::next();

        // 同じ式に複数の制約
        env.add_constraint(TypeConstraint::new(
            expr_id,
            c_int_type(),
            "from C header",
        ));
        env.add_constraint(TypeConstraint::new(
            expr_id,
            rust_c_int_type(),
            "from bindings",
        ));

        let constraints = env.get_expr_constraints(expr_id).unwrap();
        assert_eq!(constraints.len(), 2);
        assert_eq!(constraints[0].source_display(), "c-header");
        assert_eq!(constraints[1].source_display(), "rust-bindings");
    }

    #[test]
    fn test_link_expr_to_param() {
        let mut env = TypeEnv::new();
        let expr_id = ExprId::next();

        // パラメータ名を StringInterner で作成
        let mut interner = StringInterner::new();
        let param_name = interner.intern("x");

        env.link_expr_to_param(expr_id, param_name, "parameter reference");

        assert_eq!(env.get_linked_param(expr_id), Some(param_name));
    }

    #[test]
    fn test_merge() {
        let mut env1 = TypeEnv::new();
        let mut env2 = TypeEnv::new();

        let expr1 = ExprId::next();
        let expr2 = ExprId::next();

        env1.add_constraint(TypeConstraint::new(
            expr1,
            c_int_type(),
            "env1",
        ));

        env2.add_constraint(TypeConstraint::new(
            expr2,
            TypeRepr::CType {
                specs: CTypeSpecs::Char { signed: None },
                derived: vec![],
                source: CTypeSource::Apidoc { raw: "char".to_string() },
            },
            "env2",
        ));

        env1.merge(env2);

        assert_eq!(env1.expr_constraint_count(), 2);
        assert!(env1.get_expr_constraints(expr1).is_some());
        assert!(env1.get_expr_constraints(expr2).is_some());
    }

    #[test]
    #[allow(deprecated)]
    fn test_constraint_source_display() {
        assert_eq!(format!("{}", ConstraintSource::CHeader), "c-header");
        assert_eq!(format!("{}", ConstraintSource::RustBindings), "rust-bindings");
        assert_eq!(format!("{}", ConstraintSource::Apidoc), "apidoc");
        assert_eq!(format!("{}", ConstraintSource::Inferred), "inferred");
    }

    #[test]
    fn test_return_constraints() {
        let mut env = TypeEnv::new();
        let expr_id = ExprId::next();

        env.add_return_constraint(TypeConstraint::new(
            expr_id,
            apidoc_sv_ptr_type(),
            "return type from apidoc",
        ));

        assert_eq!(env.return_constraint_count(), 1);
        assert_eq!(env.return_constraints[0].source_display(), "apidoc");
    }

    #[test]
    fn test_type_repr_source_display() {
        // CType sources
        assert_eq!(c_int_type().source_display(), "c-header");
        assert_eq!(apidoc_sv_ptr_type().source_display(), "apidoc");

        // RustType
        assert_eq!(rust_c_int_type().source_display(), "rust-bindings");

        // Inferred
        let inferred = TypeRepr::Inferred(InferredType::IntLiteral);
        assert_eq!(inferred.source_display(), "inferred");
    }
}
