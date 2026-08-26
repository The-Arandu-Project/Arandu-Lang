//! Thin abstraction over multi-file module loading for name resolution.
//!
//! Production uses the Salsa [`arandu_middle::db::SourceDatabase`]; tests use [`EmptyModuleLoader`]
//! so they exercise the **same** import pipeline without on-disk modules
//! (RC-DUAL-RESOLVE).

use std::sync::Arc;

use arandu_middle::ExportedSymbolTable;
use arandu_middle::db::SourceFile;

/// Capability needed by [`crate::resolve_imports_and_bodies`] to load imports.
pub trait ModuleLoader {
    fn resolve_module_path(&self, path: &str) -> Option<SourceFile>;
    fn exported_symbols(&self, file: SourceFile) -> Arc<ExportedSymbolTable>;

    /// When `false`, a missing module path is not reported as M001.
    /// Used by unit tests that invent namespaces without multi-file fixtures.
    fn missing_import_is_error(&self) -> bool {
        true
    }

    fn package_mode(&self) -> bool {
        false
    }
}

/// Never finds modules. Prelude short-circuit and local symbols still work.
/// Missing non-prelude imports do **not** produce M001 (single-file unit tests).
#[derive(Debug, Default, Clone, Copy)]
pub struct EmptyModuleLoader;

impl ModuleLoader for EmptyModuleLoader {
    fn resolve_module_path(&self, _path: &str) -> Option<SourceFile> {
        None
    }

    fn exported_symbols(&self, _file: SourceFile) -> Arc<ExportedSymbolTable> {
        Arc::new(ExportedSymbolTable {
            symbols: std::collections::BTreeMap::new(),
        })
    }

    fn missing_import_is_error(&self) -> bool {
        false
    }
}

/// Adapter from the full Salsa source database.
pub struct SourceDbLoader<'a>(pub &'a dyn arandu_middle::db::SourceDatabase);

impl ModuleLoader for SourceDbLoader<'_> {
    fn resolve_module_path(&self, path: &str) -> Option<SourceFile> {
        self.0.resolve_module_path(path)
    }

    fn exported_symbols(&self, file: SourceFile) -> Arc<ExportedSymbolTable> {
        self.0.exported_symbols(file)
    }

    fn package_mode(&self) -> bool {
        self.0.package_mode()
    }
}
