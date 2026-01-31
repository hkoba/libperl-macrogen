//! C 関数宣言辞書
//!
//! C ヘッダファイルから収集した関数宣言を管理する。
//! `RustDeclDict` と対になる構造で、将来的な比較や検証に活用できる。

use std::collections::HashMap;

use crate::intern::InternedStr;

/// C 関数パラメータ
#[derive(Debug, Clone)]
pub struct CParam {
    /// パラメータ名（匿名の場合は None）
    pub name: Option<InternedStr>,
    /// パラメータの型（文字列表現）
    pub ty: String,
}

/// C 関数宣言
#[derive(Debug, Clone)]
pub struct CFnDecl {
    /// 関数名
    pub name: InternedStr,
    /// パラメータリスト
    pub params: Vec<CParam>,
    /// 戻り値の型（文字列表現）
    pub ret_ty: String,
    /// THX 依存性（pTHX_ または pTHX がパラメータに含まれる）
    pub is_thx: bool,
    /// ターゲットディレクトリで宣言されたか
    pub is_target: bool,
    /// 宣言の場所（ファイルパス:行番号）
    pub location: Option<String>,
}

/// C 関数宣言辞書
#[derive(Debug, Default)]
pub struct CFnDeclDict {
    /// 関数名 → 関数宣言のマッピング
    pub fns: HashMap<InternedStr, CFnDecl>,
}

impl CFnDeclDict {
    /// 新しい辞書を作成
    pub fn new() -> Self {
        Self::default()
    }

    /// 関数宣言を追加
    pub fn insert(&mut self, decl: CFnDecl) {
        self.fns.insert(decl.name, decl);
    }

    /// 関数が存在するか
    pub fn contains(&self, name: InternedStr) -> bool {
        self.fns.contains_key(&name)
    }

    /// 関数宣言を取得
    pub fn get(&self, name: InternedStr) -> Option<&CFnDecl> {
        self.fns.get(&name)
    }

    /// 関数が THX 依存かどうか
    pub fn is_thx_dependent(&self, name: InternedStr) -> bool {
        self.fns.get(&name).is_some_and(|d| d.is_thx)
    }

    /// THX 依存関数の数
    pub fn thx_count(&self) -> usize {
        self.fns.values().filter(|d| d.is_thx).count()
    }

    /// 登録された関数数
    pub fn len(&self) -> usize {
        self.fns.len()
    }

    /// 空かどうか
    pub fn is_empty(&self) -> bool {
        self.fns.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intern::StringInterner;

    #[test]
    fn test_c_fn_decl_dict_basic() {
        let mut interner = StringInterner::new();
        let mut dict = CFnDeclDict::new();

        let name = interner.intern("Perl_foo");
        let decl = CFnDecl {
            name,
            params: vec![
                CParam { name: None, ty: "PerlInterpreter *".to_string() },
                CParam { name: Some(interner.intern("x")), ty: "int".to_string() },
            ],
            ret_ty: "void".to_string(),
            is_thx: true,
            is_target: true,
            location: Some("proto.h:123".to_string()),
        };

        dict.insert(decl);

        assert!(dict.contains(name));
        assert!(dict.is_thx_dependent(name));
        assert_eq!(dict.len(), 1);
        assert_eq!(dict.thx_count(), 1);

        let retrieved = dict.get(name).unwrap();
        assert_eq!(retrieved.params.len(), 2);
        assert_eq!(retrieved.ret_ty, "void");
    }

    #[test]
    fn test_c_fn_decl_dict_non_thx() {
        let mut interner = StringInterner::new();
        let mut dict = CFnDeclDict::new();

        let name = interner.intern("strlen");
        let decl = CFnDecl {
            name,
            params: vec![
                CParam { name: Some(interner.intern("s")), ty: "const char *".to_string() },
            ],
            ret_ty: "size_t".to_string(),
            is_thx: false,
            is_target: false,
            location: Some("string.h:100".to_string()),
        };

        dict.insert(decl);

        assert!(dict.contains(name));
        assert!(!dict.is_thx_dependent(name));
        assert_eq!(dict.thx_count(), 0);
    }
}
