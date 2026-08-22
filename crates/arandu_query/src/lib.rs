pub mod analysis;
pub mod dataflow;
pub mod db;
pub mod debounce;
pub mod doc_store;
pub mod edit_vfs;
pub mod explain;
pub mod highlight;
pub mod manifest;
pub mod passes;
pub mod stable_hash;
pub mod stdlib;
pub mod vfs;
pub mod watch_buf;

pub use analysis::{AnalysisHost, AnalysisRevision, AnalysisSnapshot, LspSymbolId};
pub use dataflow::{
    block_borrow_facts, block_dataflow_facts, block_diagnostics, file_func_symbols,
    file_ide_diagnostics, file_signature_ide_diagnostics, func_amir, func_analysis_diags,
    func_borrow_summaries, ide_diags_fingerprint, item_ide_diagnostics, item_ide_diags_fingerprint,
    liveness_facts, BorrowFacts, DataflowFacts, IdeDiagnostic, IdeHint, IdeLabel, IdeReplacement,
    LivenessMap,
};
pub use db::{ArandCompilerDb, DatabaseImpl, RegistryMetrics, SourceFile};
pub use doc_store::{DocumentId, DocumentStore, OpenDocument};
pub use explain::{any_execute, RebuildCounts, RebuildEvent, RebuildLog};
pub use highlight::{
    compute_highlights, file_highlights, highlights_in_range, HlKind, HlToken, MOD_DECLARATION,
    MOD_DEFINITION, MOD_MUTABLE,
};
pub use manifest::{
    find_manifest, hash_manifest_bytes, load_manifest, manifest_fingerprint, parse_manifest_str,
    register_manifest, ManifestData, ManifestError, ProjectManifest, MANIFEST_FILENAME,
};
// re-export for tests/CLI convenience
pub use debounce::{DebouncedMap, DEFAULT_DEBOUNCE};
pub use edit_vfs::{EditVfs, Vfs};
pub use passes::{file_typeck_view, item_body_typeck, lower_amir, syntax_tree, LowerAmirArtifacts};
pub use stable_hash::StableHash;
pub use stdlib::{
    import_path_on_disk, is_stdlib_root, resolve_exe_path, resolve_stdlib_root, StdlibNotFound,
    StdlibResolveOpts, StdlibRoot, StdlibSource, INSTALL_RELATIVE, STDLIB_ENV,
};
pub use vfs::{
    listing_contains, map_import_key, scan_aru_entries, validate_package_name, DirectoryListing,
    ModuleRoots, ReservedNameError, RESERVED_PACKAGE_ROOTS,
};
pub use watch_buf::{
    abs_path, FsChange, PackageWatchConfig, PackageWatchSession, WatchBuffer, WatchCommitSummary,
};
