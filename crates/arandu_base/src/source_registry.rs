use crate::line_index::LineIndex;
use rustc_hash::FxHashMap;
use std::sync::Arc;

/// An entry in the `SourceRegistry` representing a loaded source file.
/// Uses `Arc<str>` for zero-copy memory interning and lock-free thread safety across compilation sessions.
#[derive(Clone, Debug)]
pub struct SourceFile {
    pub path: Arc<str>,
    pub source: Arc<str>,
    pub line_index: LineIndex,
}

/// A registry mapping file identifiers to file paths, contents, and line indices.
#[derive(Default, Clone, Debug)]
pub struct SourceRegistry {
    files: Vec<SourceFile>,
    path_to_id: FxHashMap<Arc<str>, u32>,
}

impl SourceRegistry {
    /// Creates a new `SourceRegistry`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            files: Vec::new(),
            path_to_id: FxHashMap::default(),
        }
    }

    /// Registers a file path and content, returning a unique file identifier.
    /// If the path is already registered, returns the existing identifier.
    pub fn register(&mut self, path: &str, source: &str) -> u32 {
        if let Some(&id) = self.path_to_id.get(path) {
            return id;
        }
        let id = self.files.len() as u32;
        let path_arc: Arc<str> = Arc::from(path);
        let source_arc: Arc<str> = Arc::from(source);
        let line_index = LineIndex::from_arc(source_arc.clone());
        self.files.push(SourceFile {
            path: path_arc.clone(),
            source: source_arc,
            line_index,
        });
        self.path_to_id.insert(path_arc, id);
        id
    }

    /// Resolves a file identifier back to its `SourceFile`.
    #[must_use]
    pub fn get_file(&self, id: u32) -> Option<&SourceFile> {
        self.files.get(id as usize)
    }

    /// Resolves a file path back to its identifier.
    #[must_use]
    pub fn get_id_by_path(&self, path: &str) -> Option<u32> {
        self.path_to_id.get(path).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_source_registry_operations() {
        let mut registry = SourceRegistry::new();

        assert_eq!(registry.get_id_by_path("file1.aru"), None);
        assert!(registry.get_file(0).is_none());

        let id1 = registry.register("file1.aru", "fn main() {}");
        assert_eq!(registry.get_id_by_path("file1.aru"), Some(id1));

        let file1 = registry.get_file(id1).expect("file should be registered");
        assert_eq!(&*file1.path, "file1.aru");
        assert_eq!(&*file1.source, "fn main() {}");

        let id1_again = registry.register("file1.aru", "ignored");
        assert_eq!(id1, id1_again);

        let id2 = registry.register("file2.aru", "const X = 42;");
        assert_ne!(id1, id2);
        assert_eq!(registry.get_id_by_path("file2.aru"), Some(id2));

        let file2 = registry.get_file(id2).expect("file should be registered");
        assert_eq!(&*file2.path, "file2.aru");
        assert_eq!(&*file2.source, "const X = 42;");
    }
}
