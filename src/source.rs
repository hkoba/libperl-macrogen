use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// ファイル識別子
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, Default)]
pub struct FileId(u32);

impl FileId {
    /// 内部IDを取得（デバッグ用）
    pub fn as_u32(self) -> u32 {
        self.0
    }
}

/// ソース位置
#[derive(Debug, Clone, Default)]
pub struct SourceLocation {
    pub file_id: FileId,
    pub line: u32,
    pub column: u32,
}

impl SourceLocation {
    /// 新しいソース位置を作成
    pub fn new(file_id: FileId, line: u32, column: u32) -> Self {
        Self {
            file_id,
            line,
            column,
        }
    }
}

/// ファイルレジストリ
#[derive(Debug, Default, Clone)]
pub struct FileRegistry {
    paths: Vec<PathBuf>,
    path_to_id: HashMap<PathBuf, FileId>,
}

impl FileRegistry {
    /// 新しいレジストリを作成
    pub fn new() -> Self {
        Self {
            paths: Vec::new(),
            path_to_id: HashMap::new(),
        }
    }

    /// パスを登録してIDを返す
    pub fn register(&mut self, path: PathBuf) -> FileId {
        if let Some(&id) = self.path_to_id.get(&path) {
            return id;
        }
        let id = FileId(self.paths.len() as u32);
        self.path_to_id.insert(path.clone(), id);
        self.paths.push(path);
        id
    }

    /// IDからパスを取得
    pub fn get_path(&self, id: FileId) -> &Path {
        &self.paths[id.0 as usize]
    }

    /// 登録されているファイル数を返す
    pub fn len(&self) -> usize {
        self.paths.len()
    }

    /// レジストリが空かどうか
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// 登録されたファイルをイテレート
    pub fn iter(&self) -> impl Iterator<Item = (FileId, &Path)> {
        self.paths
            .iter()
            .enumerate()
            .map(|(i, p)| (FileId(i as u32), p.as_path()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_registry_register() {
        let mut registry = FileRegistry::new();
        let id1 = registry.register(PathBuf::from("/path/to/file1.c"));
        let id2 = registry.register(PathBuf::from("/path/to/file2.c"));

        assert_ne!(id1, id2);
        assert_eq!(registry.get_path(id1), Path::new("/path/to/file1.c"));
        assert_eq!(registry.get_path(id2), Path::new("/path/to/file2.c"));
    }

    #[test]
    fn test_file_registry_same_path() {
        let mut registry = FileRegistry::new();
        let id1 = registry.register(PathBuf::from("/path/to/file.c"));
        let id2 = registry.register(PathBuf::from("/path/to/file.c"));

        assert_eq!(id1, id2);
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn test_source_location() {
        let loc = SourceLocation::new(FileId(0), 10, 5);
        assert_eq!(loc.line, 10);
        assert_eq!(loc.column, 5);
    }
}
