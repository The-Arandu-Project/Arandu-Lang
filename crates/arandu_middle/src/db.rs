use std::path::PathBuf;
use std::sync::Arc;

pub type FileId = u32;

#[salsa::input]
pub struct SourceFile {
    pub file_id: FileId,
    pub text: Arc<str>,
    pub path: Arc<PathBuf>,
}

/// The common database trait used by middle-end crates (resolve, typeck)
/// to request data from the Salsa database without knowing about `arandu_query`.
pub trait SourceDatabase: salsa::Database {
    fn exported_symbols(&self, file: SourceFile) -> Arc<crate::ExportedSymbolTable>;

    /// Retrieves the exact lexical span of a symbol for diagnostics (prevents Span from breaking early cutoff).
    fn symbol_span(&self, symbol_id: crate::SymbolId) -> arandu_lexer::Span;

    /// Parses a file and returns its AST.
    fn parse_file(
        &self,
        file: SourceFile,
    ) -> Result<Arc<arandu_parser::Program>, arandu_parser::ParseError>;

    /// Resolves all symbols (public and private) within a file.
    fn resolve_file(&self, file: SourceFile) -> Arc<crate::ResolutionResult>;

    /// Maps a module import path to a Salsa SourceFile.
    fn resolve_module_path(&self, path: &str) -> Option<SourceFile>;

    /// Whether imports are interpreted under a discovered package manifest.
    fn package_mode(&self) -> bool {
        false
    }
}

/// Diagnostic accumulator for Salsa.
#[salsa::accumulator]
pub struct DiagnosticsAccumulator(pub crate::Diagnostic);
