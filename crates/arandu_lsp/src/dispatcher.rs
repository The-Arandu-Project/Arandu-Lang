//! Server event loop and job-result plumbing.
//!
//! Owns the explicit `select_biased!` loop (protocol messages, worker job
//! results, workspace discovery events, VFS debounce deadline) and the typed
//! [`JobResult`] channel that carries outcomes from worker threads back to the
//! main thread. Stale results are dropped here: publication happens only when
//! the document is alive and the analysis revision still matches.
//!
//! Interactive scheduling lives here too: semantic requests are spawned at
//! [`Priority::Interactive`] with a per-request [`JobKey`] so clients can
//! cancel via `$/cancelRequest`, and saturation is answered with
//! `ServerCancelled` instead of unbounded backlog.

use crate::diagnostics::{publish_diagnostics, spawn_diagnostics, spawn_open_diagnostics};
use crate::handlers;
use crate::pool::{CancellationToken, JobKey, Priority, WorkerPool};
use crate::state::{DocInfo, ServerState};
use crate::workspace::WorkspaceEvent;
use arandu_query::{AnalysisRevision, AnalysisSnapshot, DocumentId};
use crossbeam_channel::{never, select_biased, Receiver, Sender};
use lsp_server::{Connection, Message, Notification, RequestId, Response};
use lsp_types::notification::{Notification as _, Progress};
use lsp_types::{ProgressParams, ProgressParamsValue, ProgressToken, WorkDoneProgress};
use rustc_hash::FxHashMap;
use std::error::Error;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Duration;

pub(crate) const WORKSPACE_PROGRESS_TOKEN: &str = "arandu-workspace-index";
pub(crate) const WORKSPACE_PROGRESS_REQUEST_ID: &str = "arandu-workspace-progress-create";

pub(crate) const LSP_CONTENT_MODIFIED: i32 = -32801;
pub(crate) const LSP_REQUEST_CANCELLED: i32 = -32800;
pub(crate) const LSP_SERVER_CANCELLED: i32 = -32802;
pub(crate) const JSON_RPC_INTERNAL_ERROR: i32 = -32603;

/// Outcomes sent from worker threads back to the main loop.
pub(crate) enum JobResult {
    Diagnostics {
        uri: lsp_types::Uri,
        doc_id: DocumentId,
        version: Option<i32>,
        revision: AnalysisRevision,
        fingerprint: [u8; 32],
        diags: Vec<lsp_types::Diagnostic>,
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

pub(crate) fn event_loop(
    connection: &Connection,
    state: &mut ServerState,
    pool: &WorkerPool,
    job_tx: Sender<JobResult>,
    job_rx: Receiver<JobResult>,
    mut workspace_rx: Receiver<WorkspaceEvent>,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    let mut workspace_progress_started = false;
    let mut workspace_done = false;
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
                        let mut ctx = handlers::HandlerCtx {
                            connection,
                            state,
                            pool,
                            job_tx: &job_tx,
                        };
                        handlers::dispatch_request(&mut ctx, req)?;
                    }
                    Message::Notification(not) => {
                        let mut ctx = handlers::HandlerCtx {
                            connection,
                            state,
                            pool,
                            job_tx: &job_tx,
                        };
                        handlers::dispatch_notification(&mut ctx, not)?;
                    }
                    Message::Response(response)
                        if response.id == RequestId::from(WORKSPACE_PROGRESS_REQUEST_ID.to_owned())
                            && response.response_result.is_ok() => {
                            send_workspace_progress(connection, WorkDoneProgress::Begin(
                                lsp_types::WorkDoneProgressBegin {
                                    title: "Indexing Arandu workspace".into(),
                                    cancellable: Some(false),
                                    message: Some("Discovering packages and source files".into()),
                                    percentage: None,
                                },
                            ))?;
                            workspace_progress_started = true;
                            if workspace_done {
                                finish_workspace_progress(connection)?;
                                workspace_progress_started = false;
                            }
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
                        let project = *project;
                        state.configure_package(
                            project.manifest_path,
                            project.manifest_data,
                            project.manifest_hash,
                            project.package_src,
                            project.entries,
                            project.stdlib_root,
                        );
                    }
                    Ok(WorkspaceEvent::File(file)) => {
                        crate::workspace::register_workspace_file(state, file);
                    }
                    Ok(WorkspaceEvent::Error(error)) => {
                        send_server_status(connection, "error", &error)?;
                    }
                    Ok(WorkspaceEvent::Done) => {
                        spawn_open_diagnostics(state, pool, &job_tx);
                        workspace_done = true;
                        if workspace_progress_started {
                            finish_workspace_progress(connection)?;
                            workspace_progress_started = false;
                        }
                        send_server_status(connection, "ready", "Workspace ready")?;
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

pub(crate) fn send_workspace_progress(
    connection: &Connection,
    progress: WorkDoneProgress,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    let params = ProgressParams {
        token: ProgressToken::String(WORKSPACE_PROGRESS_TOKEN.into()),
        value: ProgressParamsValue::WorkDone(progress),
    };
    connection
        .sender
        .send(Message::Notification(Notification::new(
            Progress::METHOD.into(),
            serde_json::to_value(params)?,
        )))?;
    Ok(())
}

pub(crate) fn send_server_status(
    connection: &Connection,
    state: &str,
    message: &str,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    connection
        .sender
        .send(Message::Notification(Notification::new(
            "arandu/status".into(),
            serde_json::json!({ "state": state, "message": message }),
        )))?;
    Ok(())
}

pub(crate) fn finish_workspace_progress(
    connection: &Connection,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    send_workspace_progress(
        connection,
        WorkDoneProgress::End(lsp_types::WorkDoneProgressEnd {
            message: Some("Workspace ready".into()),
        }),
    )
}

/// Makes pending VFS edits visible before a semantic request is scheduled.
///
/// If anything commits, diagnostics are re-spawned for those documents so a
/// request never analyzes a buffer older than the client's view.
pub(crate) fn flush_for_request(
    state: &mut ServerState,
    pool: &WorkerPool,
    job_tx: &Sender<JobResult>,
) {
    if state.vfs.has_pending() {
        pool.cancel_requests();
    }
    let committed = state.flush_all();
    for (uri, doc_id) in committed {
        spawn_diagnostics(state, pool, job_tx, uri, doc_id);
    }
}

/// Schedules goto-definition on an interactive worker.
///
/// Clones the URI/FileId maps up front; the closure only touches the captured
/// snapshot, never live server state.
pub(crate) fn spawn_goto(
    state: &ServerState,
    pool: &WorkerPool,
    job_tx: &Sender<JobResult>,
    req_id: RequestId,
    uri: lsp_types::Uri,
    pos: lsp_types::Position,
) {
    let snap = state.snapshot();
    let revision = snap.revision;
    let by_uri = state.by_uri.clone();
    let by_file_id = state.by_file_id.clone();
    let docs = state.doc_infos_by_id();
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
                    let location =
                        handlers::goto_on_snapshot(&snap, &by_uri, &by_file_id, &docs, &uri, pos);
                    match location {
                        Some(loc) => {
                            serde_json::to_value(lsp_types::GotoDefinitionResponse::Scalar(loc))
                                .unwrap_or(serde_json::Value::Null)
                        }
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

/// Runs `f` over the snapshot on an interactive worker and replies with JSON.
pub(crate) fn spawn_json<F>(
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
    let docs = state.doc_info_map();
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

/// Like [`spawn_json`], but `f` may produce a protocol error response.
pub(crate) fn spawn_json_result<F>(
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
    let docs = state.doc_info_map();
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

/// On cancellation, reports `RequestCancelled` to the client and returns true.
pub(crate) fn send_cancelled_if_needed(
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
