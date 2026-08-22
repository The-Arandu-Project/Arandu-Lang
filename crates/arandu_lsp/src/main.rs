//! DX.6 / P4 — synchronous LSP: main + VFS debounce + worker pool + full IDE caps.
//!
//! Protocol: `lsp-server` + `lsp-types` (no async on the analysis path).

mod conv;
mod ide;
mod pool;
mod state;
mod uri_util;
mod vfs;

use arandu_base::LineIndex;
use arandu_query::{
    find_manifest, load_manifest, resolve_stdlib_root, scan_aru_entries, AnalysisRevision,
    AnalysisSnapshot, ArandCompilerDb, DocumentId, ManifestData, SourceFile, StdlibResolveOpts,
};
use conv::{apply_lsp_range_edit, position_to_offset, span_to_range};
use crossbeam_channel::{bounded, never, select_biased, Receiver, Sender};
use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use lsp_types::notification::{
    Cancel, DidChangeTextDocument, DidChangeWatchedFiles, DidCloseTextDocument, DidCreateFiles,
    DidDeleteFiles, DidOpenTextDocument, DidRenameFiles, DidSaveTextDocument, Notification as _,
    PublishDiagnostics,
};
use lsp_types::request::{
    CodeActionRequest, Completion, DocumentHighlightRequest, DocumentSymbolRequest,
    FoldingRangeRequest, Formatting, GotoDefinition, HoverRequest, PrepareRenameRequest,
    References, Rename, Request as _, SelectionRangeRequest, SemanticTokensFullRequest,
    SemanticTokensRangeRequest, SignatureHelpRequest, WorkspaceSymbolRequest,
};
use lsp_types::{
    CancelParams, CodeActionOptions, CodeActionProviderCapability, CodeDescription,
    CompletionOptions, CompletionResponse, Diagnostic, DiagnosticRelatedInformation,
    DiagnosticSeverity, DiagnosticTag, DocumentSymbolResponse, FileChangeType, FileOperationFilter,
    FileOperationPattern, FileOperationPatternKind, FileOperationRegistrationOptions,
    FoldingRangeProviderCapability, GotoDefinitionResponse, HoverProviderCapability,
    InitializeResult, Location, NumberOrString, OneOf, Position, PositionEncodingKind,
    PublishDiagnosticsParams, RenameOptions, SelectionRangeProviderCapability,
    SemanticTokensFullOptions, SemanticTokensOptions, SemanticTokensServerCapabilities,
    ServerCapabilities, ServerInfo, SignatureHelpOptions, TextDocumentSyncCapability,
    TextDocumentSyncKind, Uri, WorkDoneProgressOptions, WorkspaceFileOperationsServerCapabilities,
    WorkspaceServerCapabilities, WorkspaceSymbolParams,
};
use pool::{CancellationToken, JobKey, Priority, WorkerPool};
use rustc_hash::FxHashMap;
use state::{discover_aru_files, ServerState};
use std::error::Error;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use uri_util::{parse_uri, path_from_uri, uri_from_path};

enum JobResult {
    Diagnostics {
        uri: Uri,
        doc_id: DocumentId,
        version: Option<i32>,
        revision: AnalysisRevision,
        fingerprint: [u8; 32],
        diags: Vec<Diagnostic>,
    },
    JsonResponse {
        id: RequestId,
        revision: AnalysisRevision,
        value: serde_json::Value,
    },
    JsonError {
        id: RequestId,
        revision: AnalysisRevision,
        code: i32,
        message: String,
    },
    Failed {
        id: Option<RequestId>,
        revision: AnalysisRevision,
    },
    Cancelled {
        id: RequestId,
    },
    Rejected {
        id: RequestId,
    },
}

struct WorkspaceFile {
    path: PathBuf,
    text: String,
}

struct WorkspaceProject {
    manifest_path: PathBuf,
    manifest_data: ManifestData,
    manifest_hash: String,
    package_src: PathBuf,
    entries: Vec<String>,
    stdlib_root: Option<PathBuf>,
}

enum WorkspaceEvent {
    Project(WorkspaceProject),
    File(WorkspaceFile),
    Done,
}

const LSP_CONTENT_MODIFIED: i32 = -32801;
const LSP_REQUEST_CANCELLED: i32 = -32800;
const LSP_SERVER_CANCELLED: i32 = -32802;
const JSON_RPC_INTERNAL_ERROR: i32 = -32603;

#[derive(Clone)]
struct DocInfo {
    source: SourceFile,
    path: Arc<PathBuf>,
}

fn main() -> Result<(), Box<dyn Error + Sync + Send>> {
    let (connection, io_threads) = Connection::stdio();
    run(connection)?;
    io_threads.join()?;
    Ok(())
}

fn run(connection: Connection) -> Result<(), Box<dyn Error + Sync + Send>> {
    let workspace_roots = initialize_connection(&connection)?;
    let mut state = ServerState::new();
    let pool = WorkerPool::new(4)?;
    let (job_tx, job_rx) = crossbeam_channel::unbounded::<JobResult>();
    let workspace_rx = spawn_workspace_discovery(&pool, workspace_roots);
    event_loop(&connection, &mut state, &pool, job_tx, job_rx, workspace_rx)?;
    // Close lsp-server's sender before the stdio owner joins its writer thread.
    drop(connection);
    Ok(())
}

fn initialize_connection(
    connection: &Connection,
) -> Result<Vec<PathBuf>, Box<dyn Error + Sync + Send>> {
    let (initialize_id, initialize_params) = connection.initialize_start()?;
    let init: lsp_types::InitializeParams = serde_json::from_value(initialize_params)?;
    let mut workspace_roots = Vec::new();
    if let Some(folders) = init.workspace_folders.as_ref() {
        workspace_roots.extend(folders.iter().filter_map(|folder| {
            let root = path_from_uri(&folder.uri);
            (!root.as_os_str().is_empty()).then_some(root)
        }));
    } else {
        // root_uri deprecated in favor of workspace_folders; still used by older clients.
        #[allow(deprecated)]
        if let Some(root_uri) = init.root_uri.as_ref() {
            let root = path_from_uri(root_uri);
            if !root.as_os_str().is_empty() {
                workspace_roots.push(root);
            }
        }
    }
    workspace_roots.sort();
    workspace_roots.dedup();

    let aru_file_operations = || FileOperationRegistrationOptions {
        filters: vec![FileOperationFilter {
            scheme: Some("file".into()),
            pattern: FileOperationPattern {
                glob: "**/*.aru".into(),
                matches: Some(FileOperationPatternKind::File),
                options: None,
            },
        }],
    };
    let server_caps = ServerCapabilities {
        // UTF-16 is the mandatory LSP encoding. Advertise it explicitly even
        // when a client prefers optional encodings we do not implement yet.
        position_encoding: Some(PositionEncodingKind::UTF16),
        text_document_sync: Some(TextDocumentSyncCapability::Kind(
            TextDocumentSyncKind::INCREMENTAL,
        )),
        definition_provider: Some(OneOf::Left(true)),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        completion_provider: Some(CompletionOptions {
            resolve_provider: Some(false),
            trigger_characters: Some(vec![".".into(), ":".into()]),
            ..CompletionOptions::default()
        }),
        signature_help_provider: Some(SignatureHelpOptions {
            trigger_characters: Some(vec!["(".into(), ",".into()]),
            retrigger_characters: Some(vec![",".into()]),
            work_done_progress_options: WorkDoneProgressOptions::default(),
        }),
        references_provider: Some(OneOf::Left(true)),
        document_highlight_provider: Some(OneOf::Left(true)),
        folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
        selection_range_provider: Some(SelectionRangeProviderCapability::Simple(true)),
        rename_provider: Some(OneOf::Right(RenameOptions {
            prepare_provider: Some(true),
            work_done_progress_options: WorkDoneProgressOptions::default(),
        })),
        document_symbol_provider: Some(OneOf::Left(true)),
        workspace_symbol_provider: Some(OneOf::Left(true)),
        semantic_tokens_provider: Some(SemanticTokensServerCapabilities::SemanticTokensOptions(
            SemanticTokensOptions {
                legend: ide::semantic_tokens_legend(),
                range: Some(true),
                full: Some(SemanticTokensFullOptions::Bool(true)),
                work_done_progress_options: WorkDoneProgressOptions::default(),
            },
        )),
        document_formatting_provider: Some(OneOf::Left(true)),
        code_action_provider: Some(CodeActionProviderCapability::Options(CodeActionOptions {
            code_action_kinds: Some(vec![lsp_types::CodeActionKind::QUICKFIX]),
            work_done_progress_options: WorkDoneProgressOptions::default(),
            resolve_provider: None,
        })),
        workspace: Some(WorkspaceServerCapabilities {
            workspace_folders: None,
            file_operations: Some(WorkspaceFileOperationsServerCapabilities {
                did_create: Some(aru_file_operations()),
                did_rename: Some(aru_file_operations()),
                did_delete: Some(aru_file_operations()),
                ..WorkspaceFileOperationsServerCapabilities::default()
            }),
        }),
        ..ServerCapabilities::default()
    };
    let init_result = InitializeResult {
        capabilities: server_caps,
        server_info: Some(ServerInfo {
            name: "arandu-lsp".into(),
            version: Some(env!("CARGO_PKG_VERSION").into()),
        }),
    };
    connection.initialize_finish(initialize_id, serde_json::to_value(init_result)?)?;
    Ok(workspace_roots)
}

fn spawn_workspace_discovery(pool: &WorkerPool, roots: Vec<PathBuf>) -> Receiver<WorkspaceEvent> {
    const DISCOVERY_BACKLOG: usize = 8;
    let (tx, rx) = bounded(DISCOVERY_BACKLOG);
    if roots.is_empty() {
        return never();
    }
    let _ = pool.spawn(Priority::Background, None, move |cancellation| {
        // The compiler DB currently owns one ModuleRoots input. Select the
        // first workspace package deterministically; multi-root ownership is a
        // separate protocol capability, not an order-dependent overwrite.
        if let Some(project) = discover_workspace_project(&roots) {
            if tx.send(WorkspaceEvent::Project(project)).is_err() {
                return;
            }
        }
        for (path, text) in discover_aru_files(&roots) {
            if cancellation.is_cancelled() {
                break;
            }
            if tx
                .send(WorkspaceEvent::File(WorkspaceFile { path, text }))
                .is_err()
            {
                break;
            }
        }
        let _ = tx.send(WorkspaceEvent::Done);
    });
    rx
}

fn discover_workspace_project(roots: &[PathBuf]) -> Option<WorkspaceProject> {
    for root in roots {
        let Some(manifest_path) = find_manifest(root) else {
            continue;
        };
        let Ok((manifest_data, manifest_hash, _)) = load_manifest(&manifest_path) else {
            continue;
        };
        let package_root = manifest_path
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| root.clone());
        let entry_path = package_root.join(&manifest_data.entry);
        let package_src = entry_path
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or(package_root);
        let entries = scan_aru_entries(&package_src);
        let stdlib_root = resolve_stdlib_root(StdlibResolveOpts::default())
            .ok()
            .map(|stdlib| stdlib.path);
        return Some(WorkspaceProject {
            manifest_path,
            manifest_data,
            manifest_hash,
            package_src,
            entries,
            stdlib_root,
        });
    }
    None
}

fn event_loop(
    connection: &Connection,
    state: &mut ServerState,
    pool: &WorkerPool,
    job_tx: Sender<JobResult>,
    job_rx: Receiver<JobResult>,
    mut workspace_rx: Receiver<WorkspaceEvent>,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    loop {
        let timeout = state
            .vfs
            .next_deadline()
            .unwrap_or(Duration::from_secs(3600));

        select_biased! {
            recv(connection.receiver) -> msg => {
                let Ok(msg) = msg else { break };
                match msg {
                    Message::Request(req) => {
                        if connection.handle_shutdown(&req)? {
                            return Ok(());
                        }
                        on_request(connection, state, pool, &job_tx, req)?;
                    }
                    Message::Notification(not) => {
                        on_notification(connection, state, pool, &job_tx, not)?;
                    }
                    Message::Response(_) => {}
                }
            }
            recv(job_rx) -> job => {
                if let Ok(job) = job {
                    handle_job_result(connection, state, job)?;
                }
            }
            recv(workspace_rx) -> event => {
                match event {
                    Ok(WorkspaceEvent::Project(project)) => {
                        state.configure_package(
                            project.manifest_path,
                            project.manifest_data,
                            project.manifest_hash,
                            project.package_src,
                            project.entries,
                            project.stdlib_root,
                        );
                    }
                    Ok(WorkspaceEvent::File(file)) => register_workspace_file(state, file),
                    Ok(WorkspaceEvent::Done) => {
                        spawn_open_diagnostics(state, pool, &job_tx);
                        workspace_rx = never();
                    }
                    Err(_) => workspace_rx = never(),
                }
            }
            default(timeout) => {
                if state.vfs.has_pending() {
                    pool.cancel_requests();
                }
                let committed = state.flush_due();
                for (uri, doc_id) in committed {
                    spawn_diagnostics(state, pool, &job_tx, uri, doc_id);
                }
            }
        }
    }
    Ok(())
}

fn on_notification(
    connection: &Connection,
    state: &mut ServerState,
    pool: &WorkerPool,
    job_tx: &Sender<JobResult>,
    not: Notification,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    match not.method.as_str() {
        Cancel::METHOD => {
            let params: CancelParams = not.extract(Cancel::METHOD)?;
            let id = match params.id {
                NumberOrString::Number(id) => RequestId::from(id),
                NumberOrString::String(id) => RequestId::from(id),
            };
            let _ = pool.cancel(&JobKey::Request(id));
        }
        DidOpenTextDocument::METHOD => {
            let params: lsp_types::DidOpenTextDocumentParams =
                not.extract(DidOpenTextDocument::METHOD)?;
            let uri = params.text_document.uri;
            let version = params.text_document.version;
            pool.cancel_requests();
            let id = state.open_or_commit(&uri, params.text_document.text);
            state.mark_open(&uri);
            state.set_version(id, version);
            spawn_diagnostics(state, pool, job_tx, uri, id);
        }
        DidChangeTextDocument::METHOD => {
            let params: lsp_types::DidChangeTextDocumentParams =
                not.extract(DidChangeTextDocument::METHOD)?;
            let uri = params.text_document.uri;
            let version = params.text_document.version;
            // Incremental: apply each range change onto the current buffer text.
            let mut text = state.text_for_change(&uri);
            for change in params.content_changes {
                if let Some(range) = change.range {
                    text = apply_lsp_range_edit(&text, range, &change.text);
                } else {
                    // Full replace (clients may still send this).
                    text = change.text;
                }
            }
            let id = state
                .by_uri
                .get(uri.as_str())
                .copied()
                .unwrap_or_else(|| state.open_or_commit(&uri, text.clone()));
            state.set_version(id, version);
            state.queue_change(&uri, text);
        }
        DidSaveTextDocument::METHOD => {
            let params: lsp_types::DidSaveTextDocumentParams =
                not.extract(DidSaveTextDocument::METHOD)?;
            if let Some(text) = params.text {
                state.queue_change(&params.text_document.uri, text);
            }
            if state.vfs.has_pending() {
                pool.cancel_requests();
            }
            let committed = state.flush_all();
            if committed.is_empty() {
                if let Some(&id) = state.by_uri.get(params.text_document.uri.as_str()) {
                    spawn_diagnostics(state, pool, job_tx, params.text_document.uri, id);
                }
            } else {
                for (uri, doc_id) in committed {
                    spawn_diagnostics(state, pool, job_tx, uri, doc_id);
                }
            }
        }
        DidCloseTextDocument::METHOD => {
            let params: lsp_types::DidCloseTextDocumentParams =
                not.extract(DidCloseTextDocument::METHOD)?;
            let uri = params.text_document.uri;
            state.close_uri(&uri);
            publish_diagnostics(connection, uri, Vec::new(), None)?;
        }
        DidCreateFiles::METHOD => {
            let params: lsp_types::CreateFilesParams = not.extract(DidCreateFiles::METHOD)?;
            pool.cancel_requests();
            for file in params.files {
                let Some(uri) = parse_uri(&file.uri) else {
                    continue;
                };
                let _ = state.reload_uri_from_disk(&uri);
            }
            state.refresh_package_listing();
            spawn_open_diagnostics(state, pool, job_tx);
        }
        DidRenameFiles::METHOD => {
            let params: lsp_types::RenameFilesParams = not.extract(DidRenameFiles::METHOD)?;
            pool.cancel_requests();
            for file in params.files {
                let (Some(old_uri), Some(new_uri)) =
                    (parse_uri(&file.old_uri), parse_uri(&file.new_uri))
                else {
                    continue;
                };
                let was_open = state.is_open(&old_uri);
                let renamed = state.rename_uri(&old_uri, &new_uri);
                publish_diagnostics(connection, old_uri, Vec::new(), None)?;
                if was_open {
                    if let Some(id) = renamed {
                        spawn_diagnostics(state, pool, job_tx, new_uri, id);
                    }
                }
            }
            state.refresh_package_listing();
            spawn_open_diagnostics(state, pool, job_tx);
        }
        DidDeleteFiles::METHOD => {
            let params: lsp_types::DeleteFilesParams = not.extract(DidDeleteFiles::METHOD)?;
            pool.cancel_requests();
            for file in params.files {
                let Some(uri) = parse_uri(&file.uri) else {
                    continue;
                };
                state.remove_uri(&uri);
                publish_diagnostics(connection, uri, Vec::new(), None)?;
            }
            state.refresh_package_listing();
            spawn_open_diagnostics(state, pool, job_tx);
        }
        DidChangeWatchedFiles::METHOD => {
            let params: lsp_types::DidChangeWatchedFilesParams =
                not.extract(DidChangeWatchedFiles::METHOD)?;
            pool.cancel_requests();
            for change in params.changes {
                if change.typ == FileChangeType::DELETED {
                    state.remove_uri(&change.uri);
                    publish_diagnostics(connection, change.uri, Vec::new(), None)?;
                } else {
                    let _ = state.reload_uri_from_disk(&change.uri);
                }
            }
            state.refresh_package_listing();
            spawn_open_diagnostics(state, pool, job_tx);
        }
        _ => {}
    }
    Ok(())
}

fn spawn_open_diagnostics(state: &ServerState, pool: &WorkerPool, job_tx: &Sender<JobResult>) {
    let open: Vec<_> = state
        .open_uris
        .iter()
        .filter_map(|uri| {
            let parsed = parse_uri(uri)?;
            let id = state.by_uri.get(uri).copied()?;
            Some((parsed, id))
        })
        .collect();
    for (uri, id) in open {
        spawn_diagnostics(state, pool, job_tx, uri, id);
    }
}

fn flush_for_request(state: &mut ServerState, pool: &WorkerPool, job_tx: &Sender<JobResult>) {
    if state.vfs.has_pending() {
        pool.cancel_requests();
    }
    let committed = state.flush_all();
    for (uri, doc_id) in committed {
        spawn_diagnostics(state, pool, job_tx, uri, doc_id);
    }
}

fn on_request(
    connection: &Connection,
    state: &mut ServerState,
    pool: &WorkerPool,
    job_tx: &Sender<JobResult>,
    req: Request,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    // Ensure pending edits are visible for semantic requests.
    match req.method.as_str() {
        GotoDefinition::METHOD
        | HoverRequest::METHOD
        | Completion::METHOD
        | References::METHOD
        | DocumentHighlightRequest::METHOD
        | FoldingRangeRequest::METHOD
        | SelectionRangeRequest::METHOD
        | PrepareRenameRequest::METHOD
        | Rename::METHOD
        | SignatureHelpRequest::METHOD
        | DocumentSymbolRequest::METHOD
        | WorkspaceSymbolRequest::METHOD
        | SemanticTokensFullRequest::METHOD
        | SemanticTokensRangeRequest::METHOD
        | Formatting::METHOD
        | CodeActionRequest::METHOD => {
            flush_for_request(state, pool, job_tx);
        }
        _ => {}
    }

    match req.method.as_str() {
        GotoDefinition::METHOD => {
            let (id, params) =
                req.extract::<lsp_types::GotoDefinitionParams>(GotoDefinition::METHOD)?;
            let uri = params.text_document_position_params.text_document.uri;
            let pos = params.text_document_position_params.position;
            spawn_goto(state, pool, job_tx, id, uri, pos);
        }
        HoverRequest::METHOD => {
            let (id, params) = req.extract::<lsp_types::HoverParams>(HoverRequest::METHOD)?;
            let uri = params.text_document_position_params.text_document.uri;
            let pos = params.text_document_position_params.position;
            spawn_json(state, pool, job_tx, id, move |snap, docs| {
                let Some(info) = docs.get(uri.as_str()) else {
                    return serde_json::Value::Null;
                };
                let text = info.source.text(&snap.db);
                match ide::hover(snap, info.source, text, pos) {
                    Some(h) => serde_json::to_value(h).unwrap_or(serde_json::Value::Null),
                    None => serde_json::Value::Null,
                }
            });
        }
        Completion::METHOD => {
            let (id, params) = req.extract::<lsp_types::CompletionParams>(Completion::METHOD)?;
            let uri = params.text_document_position.text_document.uri;
            let pos = params.text_document_position.position;
            spawn_json(state, pool, job_tx, id, move |snap, docs| {
                let Some(info) = docs.get(uri.as_str()) else {
                    return serde_json::Value::Null;
                };
                let text = info.source.text(&snap.db);
                let items = ide::completions(snap, info.source, text, pos);
                serde_json::to_value(CompletionResponse::Array(items))
                    .unwrap_or(serde_json::Value::Null)
            });
        }
        SignatureHelpRequest::METHOD => {
            let (id, params) =
                req.extract::<lsp_types::SignatureHelpParams>(SignatureHelpRequest::METHOD)?;
            let uri = params.text_document_position_params.text_document.uri;
            let pos = params.text_document_position_params.position;
            spawn_json(state, pool, job_tx, id, move |snap, docs| {
                let Some(info) = docs.get(uri.as_str()) else {
                    return serde_json::Value::Null;
                };
                let text = info.source.text(&snap.db);
                match ide::signature_help(snap, info.source, text, pos) {
                    Some(h) => serde_json::to_value(h).unwrap_or(serde_json::Value::Null),
                    None => serde_json::Value::Null,
                }
            });
        }
        References::METHOD => {
            let (id, params) = req.extract::<lsp_types::ReferenceParams>(References::METHOD)?;
            let uri = params.text_document_position.text_document.uri;
            let pos = params.text_document_position.position;
            spawn_json(state, pool, job_tx, id, move |snap, docs| {
                let Some(info) = docs.get(uri.as_str()) else {
                    return serde_json::Value::Null;
                };
                let text = info.source.text(&snap.db);
                let locs = ide::references(snap, info.source, text, pos, &uri);
                serde_json::to_value(locs).unwrap_or(serde_json::Value::Null)
            });
        }
        DocumentHighlightRequest::METHOD => {
            let (id, params) = req
                .extract::<lsp_types::DocumentHighlightParams>(DocumentHighlightRequest::METHOD)?;
            let uri = params.text_document_position_params.text_document.uri;
            let pos = params.text_document_position_params.position;
            spawn_json(state, pool, job_tx, id, move |snap, docs| {
                let Some(info) = docs.get(uri.as_str()) else {
                    return serde_json::Value::Null;
                };
                let text = info.source.text(&snap.db);
                let highlights = ide::document_highlights(snap, info.source, text, pos);
                serde_json::to_value(highlights).unwrap_or(serde_json::Value::Null)
            });
        }
        FoldingRangeRequest::METHOD => {
            let (id, params) =
                req.extract::<lsp_types::FoldingRangeParams>(FoldingRangeRequest::METHOD)?;
            let uri = params.text_document.uri;
            spawn_json(state, pool, job_tx, id, move |snap, docs| {
                let Some(info) = docs.get(uri.as_str()) else {
                    return serde_json::Value::Null;
                };
                let text = info.source.text(&snap.db);
                serde_json::to_value(ide::folding_ranges(snap, info.source, text))
                    .unwrap_or(serde_json::Value::Null)
            });
        }
        SelectionRangeRequest::METHOD => {
            let (id, params) =
                req.extract::<lsp_types::SelectionRangeParams>(SelectionRangeRequest::METHOD)?;
            let uri = params.text_document.uri;
            let positions = params.positions;
            spawn_json(state, pool, job_tx, id, move |snap, docs| {
                let Some(info) = docs.get(uri.as_str()) else {
                    return serde_json::Value::Null;
                };
                let text = info.source.text(&snap.db);
                serde_json::to_value(ide::selection_ranges(snap, info.source, text, &positions))
                    .unwrap_or(serde_json::Value::Null)
            });
        }
        Rename::METHOD => {
            let (id, params) = req.extract::<lsp_types::RenameParams>(Rename::METHOD)?;
            let uri = params.text_document_position.text_document.uri;
            let pos = params.text_document_position.position;
            let new_name = params.new_name;
            spawn_json_result(state, pool, job_tx, id, move |snap, docs| {
                let Some(info) = docs.get(uri.as_str()) else {
                    return Ok(serde_json::Value::Null);
                };
                let text = info.source.text(&snap.db);
                let documents = docs
                    .iter()
                    .filter_map(|(uri, info)| {
                        Some(ide::DocSnap {
                            source: info.source,
                            path: Arc::clone(&info.path),
                            uri: parse_uri(uri)?,
                        })
                    })
                    .collect::<Vec<_>>();
                match ide::rename_edits(snap, info.source, text, pos, &documents, &new_name) {
                    Ok(edit) => Ok(serde_json::to_value(edit).unwrap_or(serde_json::Value::Null)),
                    Err(error) => {
                        Err((lsp_server::ErrorCode::InvalidParams as i32, error.message()))
                    }
                }
            });
        }
        PrepareRenameRequest::METHOD => {
            let (id, params) =
                req.extract::<lsp_types::TextDocumentPositionParams>(PrepareRenameRequest::METHOD)?;
            let uri = params.text_document.uri;
            let pos = params.position;
            spawn_json_result(state, pool, job_tx, id, move |snap, docs| {
                let Some(info) = docs.get(uri.as_str()) else {
                    return Ok(serde_json::Value::Null);
                };
                let text = info.source.text(&snap.db);
                match ide::prepare_rename(snap, info.source, text, pos) {
                    Ok(prepared) => {
                        Ok(serde_json::to_value(prepared).unwrap_or(serde_json::Value::Null))
                    }
                    Err(error) => {
                        Err((lsp_server::ErrorCode::InvalidParams as i32, error.message()))
                    }
                }
            });
        }
        DocumentSymbolRequest::METHOD => {
            let (id, params) =
                req.extract::<lsp_types::DocumentSymbolParams>(DocumentSymbolRequest::METHOD)?;
            let uri = params.text_document.uri;
            spawn_json(state, pool, job_tx, id, move |snap, docs| {
                let Some(info) = docs.get(uri.as_str()) else {
                    return serde_json::Value::Null;
                };
                let text = info.source.text(&snap.db);
                let syms = ide::document_symbols(snap, info.source, text, &uri);
                serde_json::to_value(DocumentSymbolResponse::Flat(syms))
                    .unwrap_or(serde_json::Value::Null)
            });
        }
        WorkspaceSymbolRequest::METHOD => {
            let (id, params) =
                req.extract::<WorkspaceSymbolParams>(WorkspaceSymbolRequest::METHOD)?;
            let query = params.query;
            spawn_json(state, pool, job_tx, id, move |snap, docs| {
                let list: Vec<ide::DocSnap> = docs
                    .iter()
                    .map(|(uri_s, info)| ide::DocSnap {
                        source: info.source,
                        path: Arc::clone(&info.path),
                        uri: parse_uri(uri_s)
                            .unwrap_or_else(|| parse_uri("file:///").expect("file root")),
                    })
                    .collect();
                let syms = ide::workspace_symbols(snap, &list, &query);
                serde_json::to_value(syms).unwrap_or(serde_json::Value::Null)
            });
        }
        SemanticTokensFullRequest::METHOD => {
            let (id, params) =
                req.extract::<lsp_types::SemanticTokensParams>(SemanticTokensFullRequest::METHOD)?;
            let uri = params.text_document.uri;
            spawn_json(state, pool, job_tx, id, move |snap, docs| {
                let Some(info) = docs.get(uri.as_str()) else {
                    return serde_json::Value::Null;
                };
                let tokens = ide::semantic_tokens(snap, info.source);
                serde_json::to_value(tokens).unwrap_or(serde_json::Value::Null)
            });
        }
        SemanticTokensRangeRequest::METHOD => {
            let (id, params) = req.extract::<lsp_types::SemanticTokensRangeParams>(
                SemanticTokensRangeRequest::METHOD,
            )?;
            let uri = params.text_document.uri;
            let range = params.range;
            spawn_json(state, pool, job_tx, id, move |snap, docs| {
                let Some(info) = docs.get(uri.as_str()) else {
                    return serde_json::Value::Null;
                };
                let text = info.source.text(&snap.db);
                let index = LineIndex::new(text);
                let start = position_to_offset(&index, range.start, text);
                let end = position_to_offset(&index, range.end, text);
                let tokens = ide::semantic_tokens_range(snap, info.source, start, end);
                serde_json::to_value(tokens).unwrap_or(serde_json::Value::Null)
            });
        }
        Formatting::METHOD => {
            let (id, params) =
                req.extract::<lsp_types::DocumentFormattingParams>(Formatting::METHOD)?;
            let uri = params.text_document.uri;
            spawn_json(state, pool, job_tx, id, move |snap, docs| {
                let Some(info) = docs.get(uri.as_str()) else {
                    return serde_json::Value::Null;
                };
                let text = info.source.text(&snap.db);
                let edits = ide::format_document(text);
                serde_json::to_value(edits).unwrap_or(serde_json::Value::Null)
            });
        }
        CodeActionRequest::METHOD => {
            let (id, params) =
                req.extract::<lsp_types::CodeActionParams>(CodeActionRequest::METHOD)?;
            let uri = params.text_document.uri;
            let context = params.context;
            spawn_json(state, pool, job_tx, id, move |_snap, docs| {
                let Some(info) = docs.get(uri.as_str()) else {
                    return serde_json::Value::Null;
                };
                let actions = ide::code_actions(&uri, &context);
                let _ = info;
                serde_json::to_value(actions).unwrap_or(serde_json::Value::Null)
            });
        }
        _ => {
            let resp = Response::new_err(
                req.id,
                lsp_server::ErrorCode::MethodNotFound as i32,
                format!("unknown request {}", req.method),
            );
            connection.sender.send(Message::Response(resp))?;
        }
    }
    Ok(())
}

fn collect_docs_map(state: &ServerState) -> FxHashMap<String, DocInfo> {
    let mut map = FxHashMap::default();
    for (uri, &id) in &state.by_uri {
        if let Some(doc) = state.docs.get(id) {
            map.insert(
                uri.clone(),
                DocInfo {
                    source: doc.source,
                    path: Arc::clone(&doc.path),
                },
            );
        }
    }
    map
}

fn spawn_diagnostics(
    state: &ServerState,
    pool: &WorkerPool,
    job_tx: &Sender<JobResult>,
    uri: Uri,
    doc_id: DocumentId,
) {
    let Some(doc) = state.docs.get(doc_id) else {
        return;
    };
    let source = doc.source;
    let version = state.version(doc_id);
    let snap = state.snapshot();
    let revision = snap.revision;
    let tx = job_tx.clone();
    let _ = pool.spawn(
        Priority::Background,
        Some(JobKey::Diagnostics(doc_id)),
        move |cancellation| {
            if cancellation.is_cancelled() {
                return;
            }
            match catch_unwind(AssertUnwindSafe(|| compute_diagnostics(&snap, source))) {
                Ok((diags, fingerprint)) => {
                    if cancellation.is_cancelled() {
                        return;
                    }
                    let _ = tx.send(JobResult::Diagnostics {
                        uri,
                        doc_id,
                        version,
                        revision,
                        fingerprint,
                        diags,
                    });
                }
                Err(_) => {
                    let _ = tx.send(JobResult::Failed { id: None, revision });
                }
            }
        },
    );
}

fn spawn_goto(
    state: &ServerState,
    pool: &WorkerPool,
    job_tx: &Sender<JobResult>,
    req_id: RequestId,
    uri: Uri,
    pos: Position,
) {
    let snap = state.snapshot();
    let revision = snap.revision;
    let by_uri = state.by_uri.clone();
    let by_file_id = state.by_file_id.clone();
    let docs = collect_doc_infos(state);
    let tx = job_tx.clone();
    let request_key = JobKey::Request(req_id.clone());
    let rejected_id = req_id.clone();
    if pool
        .spawn(
            Priority::Interactive,
            Some(request_key),
            move |cancellation| {
                if send_cancelled_if_needed(&tx, &req_id, &cancellation) {
                    return;
                }
                match catch_unwind(AssertUnwindSafe(|| {
                    let location = goto_on_snapshot(&snap, &by_uri, &by_file_id, &docs, &uri, pos);
                    match location {
                        Some(loc) => serde_json::to_value(GotoDefinitionResponse::Scalar(loc))
                            .unwrap_or(serde_json::Value::Null),
                        None => serde_json::Value::Null,
                    }
                })) {
                    Ok(value) => {
                        if send_cancelled_if_needed(&tx, &req_id, &cancellation) {
                            return;
                        }
                        let _ = tx.send(JobResult::JsonResponse {
                            id: req_id,
                            revision,
                            value,
                        });
                    }
                    Err(_) => {
                        let _ = tx.send(JobResult::Failed {
                            id: Some(req_id),
                            revision,
                        });
                    }
                }
            },
        )
        .is_err()
    {
        let _ = job_tx.send(JobResult::Rejected { id: rejected_id });
    }
}

fn spawn_json<F>(
    state: &ServerState,
    pool: &WorkerPool,
    job_tx: &Sender<JobResult>,
    req_id: RequestId,
    f: F,
) where
    F: FnOnce(&AnalysisSnapshot, &FxHashMap<String, DocInfo>) -> serde_json::Value + Send + 'static,
{
    let snap = state.snapshot();
    let revision = snap.revision;
    let docs = collect_docs_map(state);
    let tx = job_tx.clone();
    let request_key = JobKey::Request(req_id.clone());
    let rejected_id = req_id.clone();
    if pool
        .spawn(
            Priority::Interactive,
            Some(request_key),
            move |cancellation| {
                if send_cancelled_if_needed(&tx, &req_id, &cancellation) {
                    return;
                }
                match catch_unwind(AssertUnwindSafe(|| f(&snap, &docs))) {
                    Ok(value) => {
                        if send_cancelled_if_needed(&tx, &req_id, &cancellation) {
                            return;
                        }
                        let _ = tx.send(JobResult::JsonResponse {
                            id: req_id,
                            revision,
                            value,
                        });
                    }
                    Err(_) => {
                        let _ = tx.send(JobResult::Failed {
                            id: Some(req_id),
                            revision,
                        });
                    }
                }
            },
        )
        .is_err()
    {
        let _ = job_tx.send(JobResult::Rejected { id: rejected_id });
    }
}

fn spawn_json_result<F>(
    state: &ServerState,
    pool: &WorkerPool,
    job_tx: &Sender<JobResult>,
    req_id: RequestId,
    f: F,
) where
    F: FnOnce(
            &AnalysisSnapshot,
            &FxHashMap<String, DocInfo>,
        ) -> Result<serde_json::Value, (i32, String)>
        + Send
        + 'static,
{
    let snap = state.snapshot();
    let revision = snap.revision;
    let docs = collect_docs_map(state);
    let tx = job_tx.clone();
    let request_key = JobKey::Request(req_id.clone());
    let rejected_id = req_id.clone();
    if pool
        .spawn(
            Priority::Interactive,
            Some(request_key),
            move |cancellation| {
                if send_cancelled_if_needed(&tx, &req_id, &cancellation) {
                    return;
                }
                match catch_unwind(AssertUnwindSafe(|| f(&snap, &docs))) {
                    Ok(Ok(value)) => {
                        if send_cancelled_if_needed(&tx, &req_id, &cancellation) {
                            return;
                        }
                        let _ = tx.send(JobResult::JsonResponse {
                            id: req_id,
                            revision,
                            value,
                        });
                    }
                    Ok(Err((code, message))) => {
                        let _ = tx.send(JobResult::JsonError {
                            id: req_id,
                            revision,
                            code,
                            message,
                        });
                    }
                    Err(_) => {
                        let _ = tx.send(JobResult::Failed {
                            id: Some(req_id),
                            revision,
                        });
                    }
                }
            },
        )
        .is_err()
    {
        let _ = job_tx.send(JobResult::Rejected { id: rejected_id });
    }
}

fn send_cancelled_if_needed(
    tx: &Sender<JobResult>,
    id: &RequestId,
    cancellation: &CancellationToken,
) -> bool {
    if !cancellation.is_cancelled() {
        return false;
    }
    let _ = tx.send(JobResult::Cancelled { id: id.clone() });
    true
}

fn compute_diagnostics(snap: &AnalysisSnapshot, source: SourceFile) -> (Vec<Diagnostic>, [u8; 32]) {
    let text = source.text(&snap.db);
    let index = LineIndex::new(text);
    let source_uri = uri_from_path(source.path(&snap.db).as_ref());
    let ide = arandu_query::file_ide_diagnostics(&snap.db, source);
    let fp = arandu_query::ide_diags_fingerprint(ide);
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(fp.as_bytes());
    let diags = ide
        .iter()
        .map(|d| {
            let span = arandu_base::Span::new(d.file_id, d.start, d.end);
            let primary_location = diagnostic_location(snap, span).or_else(|| {
                source_uri.clone().map(|uri| Location {
                    uri,
                    range: span_to_range(&index, span),
                })
            });
            let range = primary_location
                .as_ref()
                .map_or_else(lsp_types::Range::default, |location| location.range);
            let severity = match d.severity {
                0 => DiagnosticSeverity::ERROR,
                1 => DiagnosticSeverity::WARNING,
                2 => DiagnosticSeverity::INFORMATION,
                _ => DiagnosticSeverity::HINT,
            };
            let related_information: Vec<_> = d
                .labels
                .iter()
                .filter_map(|label| {
                    diagnostic_location(
                        snap,
                        arandu_base::Span::new(label.file_id, label.start, label.end),
                    )
                    .map(|location| DiagnosticRelatedInformation {
                        location,
                        message: label.message.clone(),
                    })
                })
                .collect();
            let fixes = d
                .hints
                .iter()
                .filter_map(|hint| {
                    let replacement = hint.replacement.as_ref()?;
                    let location = diagnostic_location(
                        snap,
                        arandu_base::Span::new(
                            replacement.file_id,
                            replacement.start,
                            replacement.end,
                        ),
                    )?;
                    Some(ide::DiagnosticFixData {
                        title: hint.message.clone(),
                        uri: location.uri,
                        range: location.range,
                        new_text: replacement.new_text.clone(),
                    })
                })
                .collect();
            let data = ide::DiagnosticData {
                notes: d.notes.clone(),
                hints: d.hints.iter().map(|hint| hint.message.clone()).collect(),
                fixes,
            };
            let mut message = d.message.clone();
            for note in &d.notes {
                message.push_str("\n\nnote: ");
                message.push_str(note);
            }
            for hint in &d.hints {
                message.push_str("\n\nhint: ");
                message.push_str(&hint.message);
            }
            let code_description = (!d.code.starts_with("ICE"))
                .then(|| {
                    parse_uri(&format!(
                        "https://github.com/BrunoF2P/Arandu-Lang/blob/main/docs/errors/{}.md",
                        d.code
                    ))
                    .map(|href| CodeDescription { href })
                })
                .flatten();
            let tags = matches!(d.code.as_str(), "W001" | "W002" | "W003" | "W005" | "W007")
                .then(|| vec![DiagnosticTag::UNNECESSARY]);
            Diagnostic {
                range,
                severity: Some(severity),
                code: Some(NumberOrString::String(d.code.clone())),
                code_description,
                message,
                source: Some("arandu".into()),
                related_information: (!related_information.is_empty())
                    .then_some(related_information),
                tags,
                data: serde_json::to_value(data).ok(),
            }
        })
        .collect();
    (diags, bytes)
}

fn diagnostic_location(snap: &AnalysisSnapshot, span: arandu_base::Span) -> Option<Location> {
    let source = snap.db.source_file_by_id(span.file_id)?;
    let uri = uri_from_path(source.path(&snap.db).as_ref())?;
    let index = LineIndex::new(source.text(&snap.db));
    Some(Location {
        uri,
        range: span_to_range(&index, span),
    })
}

fn collect_doc_infos(state: &ServerState) -> FxHashMap<DocumentId, DocInfo> {
    let mut by_id = FxHashMap::default();
    for &id in state.by_uri.values() {
        if let Some(doc) = state.docs.get(id) {
            by_id.insert(
                id,
                DocInfo {
                    source: doc.source,
                    path: Arc::clone(&doc.path),
                },
            );
        }
    }
    by_id
}

fn goto_on_snapshot(
    snap: &AnalysisSnapshot,
    by_uri: &FxHashMap<String, DocumentId>,
    by_file_id: &FxHashMap<u32, DocumentId>,
    docs: &FxHashMap<DocumentId, DocInfo>,
    uri: &Uri,
    position: Position,
) -> Option<Location> {
    use arandu_base::{LineIndex, Span};
    use arandu_query::LspSymbolId;

    let id = *by_uri.get(uri.as_str())?;
    let info = docs.get(&id)?;
    let text = info.source.text(&snap.db);
    let index = LineIndex::new(text);
    let offset = position_to_offset(&index, position, text);
    let tc = arandu_query::passes::type_check(&snap.db, info.source);
    let program = arandu_query::passes::parse(&snap.db, info.source);
    let sym_id = state::ServerState::symbol_at(tc, offset).or_else(|| {
        program
            .as_ref()
            .as_ref()
            .ok()
            .and_then(|program| ide::expr_symbol_at(program, tc, offset))
    })?;
    let lsp_sym = LspSymbolId::new(sym_id, snap.revision);
    let sym_id = lsp_sym.resolve(snap)?;
    let _symbol = tc.symbols.try_get(sym_id)?;
    let def_span: Span = arandu_query::passes::symbol_span(&snap.db, sym_id);
    let def_uri = uri_for_file_id(by_file_id, docs, &snap.db, def_span.file_id)?;
    let def_text = if def_span.file_id == *info.source.file_id(&snap.db) {
        text.clone()
    } else if let Some(&def_id) = by_file_id.get(&def_span.file_id) {
        let d = docs.get(&def_id)?;
        d.source.text(&snap.db).clone()
    } else {
        let p = snap.db.file_path(def_span.file_id);
        Arc::from(std::fs::read_to_string(p.as_ref()).ok()?.as_str())
    };
    let def_index = LineIndex::new(&def_text);
    Some(Location {
        uri: def_uri,
        range: span_to_range(&def_index, def_span),
    })
}

fn uri_for_file_id(
    by_file_id: &FxHashMap<u32, DocumentId>,
    docs: &FxHashMap<DocumentId, DocInfo>,
    db: &arandu_query::DatabaseImpl,
    file_id: u32,
) -> Option<Uri> {
    if let Some(&id) = by_file_id.get(&file_id) {
        if let Some(doc) = docs.get(&id) {
            return uri_from_path(doc.path.as_ref());
        }
    }
    let path = db.file_path(file_id);
    if path.as_os_str().is_empty() {
        return None;
    }
    uri_from_path(path.as_ref())
}

fn handle_job_result(
    connection: &Connection,
    state: &mut ServerState,
    job: JobResult,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    match job {
        JobResult::Diagnostics {
            uri,
            doc_id,
            version,
            revision,
            fingerprint,
            diags,
        } => {
            if state.docs.get(doc_id).is_none() {
                return Ok(());
            }
            if revision != state.revision() {
                return Ok(());
            }
            if state.last_diag_fp.get(&doc_id) == Some(&(fingerprint, version)) {
                return Ok(());
            }
            if version != state.version(doc_id) {
                return Ok(());
            }
            publish_diagnostics(connection, uri, diags, version)?;
            state.last_diag_fp.insert(doc_id, (fingerprint, version));
        }
        JobResult::JsonResponse {
            id,
            revision,
            value,
        } => {
            if revision != state.revision() {
                connection.sender.send(Message::Response(Response::new_err(
                    id,
                    LSP_CONTENT_MODIFIED,
                    "document changed while the request was running".into(),
                )))?;
                return Ok(());
            }
            connection
                .sender
                .send(Message::Response(Response::new_ok(id, value)))?;
        }
        JobResult::JsonError {
            id,
            revision,
            code,
            message,
        } => {
            let (code, message) = if revision == state.revision() {
                (code, message)
            } else {
                (
                    LSP_CONTENT_MODIFIED,
                    "document changed while the request was running".into(),
                )
            };
            connection
                .sender
                .send(Message::Response(Response::new_err(id, code, message)))?;
        }
        JobResult::Failed { id, revision } => {
            // The captured snapshot is dropped with the failed job. Never publish a
            // diagnostic or successful response from that analysis.
            if let Some(id) = id {
                let (code, message) = if revision == state.revision() {
                    (
                        JSON_RPC_INTERNAL_ERROR,
                        "analysis worker failed; request snapshot was discarded",
                    )
                } else {
                    (
                        LSP_CONTENT_MODIFIED,
                        "document changed while the failed request was running",
                    )
                };
                connection.sender.send(Message::Response(Response::new_err(
                    id,
                    code,
                    message.into(),
                )))?;
            }
        }
        JobResult::Cancelled { id } => {
            connection.sender.send(Message::Response(Response::new_err(
                id,
                LSP_REQUEST_CANCELLED,
                "request cancelled by client".into(),
            )))?;
        }
        JobResult::Rejected { id } => {
            connection.sender.send(Message::Response(Response::new_err(
                id,
                LSP_SERVER_CANCELLED,
                "interactive scheduler is saturated; retry the request".into(),
            )))?;
        }
    }
    Ok(())
}

fn register_workspace_file(state: &mut ServerState, file: WorkspaceFile) {
    let Some(uri) = uri_from_path(&file.path) else {
        return;
    };
    // Never replace a newer editor buffer with a stale disk snapshot.
    if !state.by_uri.contains_key(uri.as_str()) {
        state.open_or_commit(&uri, file.text);
    }
}

fn publish_diagnostics(
    connection: &Connection,
    uri: Uri,
    diagnostics: Vec<Diagnostic>,
    version: Option<i32>,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    let params = PublishDiagnosticsParams {
        uri,
        diagnostics,
        version,
    };
    let not = Notification::new(
        PublishDiagnostics::METHOD.to_string(),
        serde_json::to_value(params)?,
    );
    connection.sender.send(Message::Notification(not))?;
    Ok(())
}

#[cfg(test)]
mod startup_tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn initialize_finishes_before_workspace_discovery() {
        let (server, client) = Connection::memory();
        let root = uri_from_path(std::path::Path::new("/workspace/with-many-files"))
            .or_else(|| parse_uri("file:///workspace/with-many-files"))
            .expect("workspace URI");
        let request = Request::new(
            RequestId::from(1),
            "initialize".into(),
            serde_json::json!({
                "processId": null,
                "capabilities": {},
                "workspaceFolders": [{ "uri": root, "name": "fixture" }]
            }),
        );

        let server_thread = std::thread::spawn(move || initialize_connection(&server));
        let started = Instant::now();
        client
            .sender
            .send(Message::Request(request))
            .expect("send initialize");
        let response = client
            .receiver
            .recv_timeout(Duration::from_millis(250))
            .expect("initialize must satisfy the cold p95 budget");
        assert!(matches!(
            response,
            Message::Response(Response {
                response_result: Ok(_),
                ..
            })
        ));
        assert!(started.elapsed() < Duration::from_millis(250));
        client
            .sender
            .send(Message::Notification(Notification::new(
                "initialized".into(),
                serde_json::json!({}),
            )))
            .expect("send initialized");

        let roots = server_thread
            .join()
            .expect("initialize thread")
            .expect("initialize result");
        assert_eq!(roots.len(), 1);
    }

    #[test]
    fn workspace_project_discovery_loads_manifest_and_listing() {
        let root = std::env::temp_dir().join(format!(
            "arandu-lsp-project-discovery-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("src")).expect("create fixture");
        std::fs::write(
            root.join("Arandu.toml"),
            "name = \"editor_gold\"\nversion = \"0.1.0\"\nentry = \"src/main.aru\"\n",
        )
        .expect("write manifest");
        std::fs::write(root.join("src/main.aru"), "func main() {}\n").expect("write entry");

        let project = discover_workspace_project(std::slice::from_ref(&root))
            .expect("discover package metadata");
        assert_eq!(project.manifest_data.name, "editor_gold");
        assert_eq!(
            project.package_src,
            std::fs::canonicalize(root.join("src")).expect("canonical fixture source")
        );
        assert_eq!(project.entries, vec!["main.aru"]);

        std::fs::remove_dir_all(root).expect("remove fixture");
    }
}
