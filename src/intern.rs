use std::collections::HashMap;

/// インターン済み文字列の識別子
#[derive(Clone, Copy, Eq, PartialEq, Hash, Debug, Default)]
pub struct InternedStr(u32);

impl InternedStr {
    /// 内部IDを取得（デバッグ用）
    pub fn as_u32(self) -> u32 {
        self.0
    }
}

/// 文字列インターナー
#[derive(Clone, Debug, Default)]
pub struct StringInterner {
    strings: Vec<String>,
    map: HashMap<String, InternedStr>,
}

impl StringInterner {
    /// 新しいインターナーを作成
    pub fn new() -> Self {
        Self {
            strings: Vec::new(),
            map: HashMap::new(),
        }
    }

    /// 文字列をインターンし、IDを返す
    pub fn intern(&mut self, s: &str) -> InternedStr {
        if let Some(&id) = self.map.get(s) {
            return id;
        }
        let id = InternedStr(self.strings.len() as u32);
        self.strings.push(s.to_owned());
        self.map.insert(s.to_owned(), id);
        id
    }

    /// IDから文字列を取得
    pub fn get(&self, id: InternedStr) -> &str {
        &self.strings[id.0 as usize]
    }

    /// 文字列がインターン済みか検索（新規登録しない）
    pub fn lookup(&self, s: &str) -> Option<InternedStr> {
        self.map.get(s).copied()
    }

    /// インターン済み文字列の数を返す
    pub fn len(&self) -> usize {
        self.strings.len()
    }

    /// インターナーが空かどうか
    pub fn is_empty(&self) -> bool {
        self.strings.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_intern_new_string() {
        let mut interner = StringInterner::new();
        let id1 = interner.intern("hello");
        let id2 = interner.intern("world");

        assert_ne!(id1, id2);
        assert_eq!(interner.get(id1), "hello");
        assert_eq!(interner.get(id2), "world");
    }

    #[test]
    fn test_intern_same_string() {
        let mut interner = StringInterner::new();
        let id1 = interner.intern("hello");
        let id2 = interner.intern("hello");

        assert_eq!(id1, id2);
        assert_eq!(interner.len(), 1);
    }

    #[test]
    fn test_intern_empty_string() {
        let mut interner = StringInterner::new();
        let id = interner.intern("");
        assert_eq!(interner.get(id), "");
    }
}
