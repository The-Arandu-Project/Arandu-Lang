use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// Functional protocol tests share the host with the whole workspace suite and
// must tolerate scheduler contention. Latency regressions are enforced by the
// explicit initialize/interactive p95 budgets below, not by this watchdog.
const MESSAGE_TIMEOUT: Duration = Duration::from_secs(10);
const INITIALIZE_P95_BUDGET: Duration = Duration::from_millis(250);
const INTERACTIVE_BUDGET: Duration = Duration::from_millis(250);
const COLD_SAMPLES: usize = 7;

struct FixtureDir(PathBuf);

impl FixtureDir {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("arandu-lsp-stdio-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&path).expect("create LSP fixture directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for FixtureDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct LspProcess {
    child: Child,
    stdin: ChildStdin,
    messages: Receiver<Value>,
}

impl LspProcess {
    fn spawn() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_arandu-lsp"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn arandu-lsp");
        let stdin = child.stdin.take().expect("capture arandu-lsp stdin");
        let stdout = child.stdout.take().expect("capture arandu-lsp stdout");
        let (tx, messages) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            while let Ok(Some(message)) = read_message(&mut reader) {
                if tx.send(message).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            stdin,
            messages,
        }
    }

    fn send(&mut self, message: &Value) {
        let payload = serde_json::to_vec(message).expect("serialize LSP message");
        write!(self.stdin, "Content-Length: {}\r\n\r\n", payload.len()).expect("write LSP header");
        self.stdin.write_all(&payload).expect("write LSP payload");
        self.stdin.flush().expect("flush LSP payload");
    }

    fn wait_for(&self, mut predicate: impl FnMut(&Value) -> bool) -> Value {
        let deadline = Instant::now() + MESSAGE_TIMEOUT;
        let mut observed = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let message = self
                .messages
                .recv_timeout(remaining)
                .unwrap_or_else(|error| {
                    panic!("timed out waiting for LSP message ({error:?}); observed: {observed:?}")
                });
            if predicate(&message) {
                return message;
            }
            observed.push(message);
        }
    }

    fn wait_for_response(&self, id: i64) -> Value {
        self.wait_for(|message| message.get("id").and_then(Value::as_i64) == Some(id))
    }

    fn wait_for_responses(&self, ids: impl IntoIterator<Item = i64>) -> BTreeMap<i64, Value> {
        let mut pending = ids.into_iter().collect::<BTreeSet<_>>();
        let mut responses = BTreeMap::new();
        let deadline = Instant::now() + MESSAGE_TIMEOUT;
        while !pending.is_empty() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let message = self
                .messages
                .recv_timeout(remaining)
                .expect("timed out waiting for concurrent LSP responses");
            let Some(id) = message.get("id").and_then(Value::as_i64) else {
                continue;
            };
            if pending.remove(&id) {
                responses.insert(id, message);
            }
        }
        responses
    }

    fn initialize(&mut self, root: &Path, id: i64) -> Duration {
        let started = Instant::now();
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "processId": null,
                "capabilities": {
                    "general": {
                        "positionEncodings": ["utf-8", "utf-16"]
                    }
                },
                "workspaceFolders": [{
                    "uri": file_uri(root),
                    "name": "stdio-fixture"
                }]
            }
        }));
        let response = self.wait_for_response(id);
        assert!(
            response.get("error").is_none(),
            "initialize failed: {response}"
        );
        assert_eq!(
            response
                .pointer("/result/capabilities/positionEncoding")
                .and_then(Value::as_str),
            Some("utf-16"),
            "server must explicitly negotiate its mandatory UTF-16 support: {response}"
        );
        for operation in ["didCreate", "didRename", "didDelete"] {
            assert_eq!(
                response
                    .pointer(&format!(
                        "/result/capabilities/workspace/fileOperations/{operation}/filters/0/pattern/glob"
                    ))
                    .and_then(Value::as_str),
                Some("**/*.aru"),
                "server must scope {operation} notifications to Arandu files: {response}"
            );
        }
        assert_eq!(
            response
                .pointer("/result/capabilities/renameProvider/prepareProvider")
                .and_then(Value::as_bool),
            Some(true),
            "safe rename requires the prepareRename handshake: {response}"
        );
        for capability in [
            "definitionProvider",
            "referencesProvider",
            "documentSymbolProvider",
            "workspaceSymbolProvider",
            "documentFormattingProvider",
            "foldingRangeProvider",
            "selectionRangeProvider",
            "documentHighlightProvider",
        ] {
            assert_eq!(
                response
                    .pointer(&format!("/result/capabilities/{capability}"))
                    .and_then(Value::as_bool),
                Some(true),
                "initialize must advertise {capability}: {response}"
            );
        }
        assert_eq!(
            response
                .pointer("/result/capabilities/textDocumentSync")
                .and_then(Value::as_i64),
            Some(2),
            "the public contract requires incremental text sync: {response}"
        );
        assert_eq!(
            response
                .pointer("/result/capabilities/hoverProvider")
                .and_then(Value::as_bool),
            Some(true),
            "hover must remain advertised: {response}"
        );
        for capability in [
            "completionProvider",
            "signatureHelpProvider",
            "semanticTokensProvider",
            "codeActionProvider",
        ] {
            assert!(
                response
                    .pointer(&format!("/result/capabilities/{capability}"))
                    .is_some_and(Value::is_object),
                "initialize must advertise structured {capability}: {response}"
            );
        }
        assert_eq!(
            response
                .pointer("/result/capabilities/semanticTokensProvider/full")
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            response
                .pointer("/result/capabilities/semanticTokensProvider/range")
                .and_then(Value::as_bool),
            Some(true)
        );
        for unsupported in [
            "declarationProvider",
            "typeDefinitionProvider",
            "implementationProvider",
            "documentRangeFormattingProvider",
            "documentOnTypeFormattingProvider",
            "inlayHintProvider",
            "codeLensProvider",
            "callHierarchyProvider",
            "typeHierarchyProvider",
        ] {
            assert!(
                response
                    .pointer(&format!("/result/capabilities/{unsupported}"))
                    .is_none(),
                "unimplemented capability must not be advertised: {unsupported}: {response}"
            );
        }
        let elapsed = started.elapsed();
        self.send(&json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        }));
        elapsed
    }

    fn shutdown(mut self, id: i64) {
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "shutdown",
            "params": null
        }));
        let response = self.wait_for_response(id);
        assert!(
            response.get("error").is_none(),
            "shutdown failed: {response}"
        );
        self.send(&json!({
            "jsonrpc": "2.0",
            "method": "exit",
            "params": null
        }));
        let status = self.child.wait().expect("wait for arandu-lsp exit");
        assert!(status.success(), "arandu-lsp exited with {status}");
    }
}

impl Drop for LspProcess {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[test]
fn stdio_workspace_index_reports_standard_progress_and_status() {
    let fixture = FixtureDir::new();
    let mut lsp = LspProcess::spawn();
    lsp.send(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "processId": null,
            "capabilities": { "window": { "workDoneProgress": true } },
            "workspaceFolders": [{
                "uri": file_uri(fixture.path()),
                "name": "progress-fixture"
            }]
        }
    }));
    assert!(lsp.wait_for_response(1).get("error").is_none());
    lsp.send(&json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }));

    let create = lsp.wait_for(|message| {
        message.get("method").and_then(Value::as_str) == Some("window/workDoneProgress/create")
    });
    assert_eq!(
        create.pointer("/params/token").and_then(Value::as_str),
        Some("arandu-workspace-index")
    );
    lsp.send(&json!({
        "jsonrpc": "2.0",
        "id": create.get("id").expect("progress request id"),
        "result": null
    }));

    let indexing = lsp.wait_for(|message| {
        message.get("method").and_then(Value::as_str) == Some("arandu/status")
            && message.pointer("/params/state").and_then(Value::as_str) == Some("indexing")
    });
    assert_eq!(
        indexing.pointer("/params/message").and_then(Value::as_str),
        Some("Indexing workspace")
    );
    let begin = lsp.wait_for(|message| {
        message.get("method").and_then(Value::as_str) == Some("$/progress")
            && message
                .pointer("/params/value/kind")
                .and_then(Value::as_str)
                == Some("begin")
    });
    assert_eq!(
        begin.pointer("/params/token").and_then(Value::as_str),
        Some("arandu-workspace-index")
    );
    let end = lsp.wait_for(|message| {
        message.get("method").and_then(Value::as_str) == Some("$/progress")
            && message
                .pointer("/params/value/kind")
                .and_then(Value::as_str)
                == Some("end")
    });
    assert_eq!(
        end.pointer("/params/value/message").and_then(Value::as_str),
        Some("Workspace ready")
    );
    let ready = lsp.wait_for(|message| {
        message.get("method").and_then(Value::as_str) == Some("arandu/status")
            && message.pointer("/params/state").and_then(Value::as_str) == Some("ready")
    });
    assert_eq!(
        ready.pointer("/params/message").and_then(Value::as_str),
        Some("Workspace ready")
    );
    lsp.shutdown(2);
}

#[test]
fn stdio_cold_initialize_p95_stays_within_budget() {
    let fixture = FixtureDir::new();
    populate_adversarial_workspace(fixture.path());

    let mut samples = Vec::with_capacity(COLD_SAMPLES);
    for sample in 0..COLD_SAMPLES {
        let mut lsp = LspProcess::spawn();
        samples.push(lsp.initialize(fixture.path(), 1));
        lsp.shutdown(2 + i64::try_from(sample).expect("sample id fits i64"));
    }
    samples.sort_unstable();
    let p50 = percentile(&samples, 50);
    let p95 = percentile(&samples, 95);
    eprintln!("arandu-lsp cold initialize: p50={p50:?}, p95={p95:?}");
    assert!(
        p95 <= INITIALIZE_P95_BUDGET,
        "cold initialize p95 {p95:?} exceeded {INITIALIZE_P95_BUDGET:?}; workspace I/O may have returned to the handshake"
    );
}

#[test]
fn stdio_open_document_stays_interactive_during_discovery() {
    let fixture = FixtureDir::new();
    populate_adversarial_workspace(fixture.path());
    let document = fixture.path().join("open-buffer.aru");
    let uri = file_uri(&document);
    let mut lsp = LspProcess::spawn();
    lsp.initialize(fixture.path(), 1);

    lsp.send(&json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": uri,
                "languageId": "arandu",
                "version": 1,
                "text": "func main() {}"
            }
        }
    }));
    let started = Instant::now();
    lsp.send(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "textDocument/completion",
        "params": {
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 4 }
        }
    }));
    let completion = lsp.wait_for_response(2);
    let latency = started.elapsed();
    assert!(
        completion.get("error").is_none(),
        "completion failed: {completion}"
    );
    assert!(
        latency <= INTERACTIVE_BUDGET,
        "completion took {latency:?} while workspace discovery was active"
    );

    lsp.shutdown(3);
}

#[test]
fn stdio_lifecycle_publishes_diagnostics_and_answers_completion() {
    let fixture = FixtureDir::new();
    let document = fixture.path().join("main.aru");
    let uri = file_uri(&document);
    let mut lsp = LspProcess::spawn();
    let initialize = lsp.initialize(fixture.path(), 1);

    lsp.send(&json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": uri,
                "languageId": "arandu",
                "version": 1,
                "text": "func main() { missing_name; }"
            }
        }
    }));
    let diagnostic_started = Instant::now();
    let diagnostics = lsp.wait_for(|message| {
        message.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics")
            && message.pointer("/params/uri").and_then(Value::as_str) == Some(uri.as_str())
    });
    let diagnostic = diagnostic_started.elapsed();
    assert!(
        diagnostics
            .pointer("/params/diagnostics")
            .and_then(Value::as_array)
            .is_some_and(|items| !items.is_empty()),
        "expected an undefined-name diagnostic: {diagnostics}"
    );

    let completion_started = Instant::now();
    lsp.send(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "textDocument/completion",
        "params": {
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 4 }
        }
    }));
    let completion = lsp.wait_for_response(2);
    let completion_latency = completion_started.elapsed();
    assert!(
        completion.get("error").is_none(),
        "completion failed: {completion}"
    );
    assert!(
        completion.get("result").is_some(),
        "completion has no result"
    );
    eprintln!(
        "arandu-lsp lifecycle: initialize={initialize:?}, diagnostic={diagnostic:?}, completion={completion_latency:?}"
    );

    lsp.shutdown(3);
}

#[test]
fn stdio_incremental_unicode_edits_compose_before_debounce() {
    let fixture = FixtureDir::new();
    let document = fixture.path().join("unicode-edits.aru");
    let uri = file_uri(&document);
    let mut lsp = LspProcess::spawn();
    lsp.initialize(fixture.path(), 1);

    lsp.send(&json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": uri,
                "languageId": "arandu",
                "version": 1,
                "text": "func main() {\n    missing;\n    let nome = \"😀ação\";\n}\n"
            }
        }
    }));
    let initial = lsp.wait_for(|message| {
        message.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics")
            && message.pointer("/params/uri").and_then(Value::as_str) == Some(uri.as_str())
    });
    assert!(
        initial
            .pointer("/params/diagnostics")
            .and_then(Value::as_array)
            .is_some_and(|items| !items.is_empty()),
        "fixture must begin invalid: {initial}"
    );

    // Two notifications arrive inside the debounce window. The second one
    // must use the first notification's pending buffer, not the committed DB.
    lsp.send(&json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didChange",
        "params": {
            "textDocument": { "uri": uri, "version": 2 },
            "contentChanges": [{
                "range": {
                    "start": { "line": 1, "character": 4 },
                    "end": { "line": 1, "character": 11 }
                },
                "text": "let fixed: int = 1"
            }]
        }
    }));
    lsp.send(&json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didChange",
        "params": {
            "textDocument": { "uri": uri, "version": 3 },
            "contentChanges": [
                {
                    "range": {
                        "start": { "line": 2, "character": 18 },
                        "end": { "line": 2, "character": 22 }
                    },
                    "text": "ok"
                },
                {
                    "range": {
                        "start": { "line": 3, "character": 1 },
                        "end": { "line": 3, "character": 1 }
                    },
                    "text": " // fim"
                }
            ]
        }
    }));

    // Any semantic request is a consistency barrier and flushes the VFS.
    lsp.send(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "textDocument/completion",
        "params": {
            "textDocument": { "uri": uri },
            "position": { "line": 2, "character": 12 }
        }
    }));
    let response = lsp.wait_for_response(2);
    assert!(
        response.get("error").is_none(),
        "completion failed: {response}"
    );

    let updated = lsp.wait_for(|message| {
        message.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics")
            && message.pointer("/params/uri").and_then(Value::as_str) == Some(uri.as_str())
    });
    assert_eq!(
        updated
            .pointer("/params/diagnostics")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(0),
        "the second edit must retain the first edit and preserve UTF-16 coordinates: {updated}"
    );

    lsp.shutdown(3);
}

#[test]
fn stdio_rename_prepares_and_rejects_unsafe_names() {
    let fixture = FixtureDir::new();
    let document = fixture.path().join("rename.aru");
    let uri = file_uri(&document);
    let source = concat!(
        "func choose(value: int): int {\n",
        "    let taken: int = 1\n",
        "    return value\n",
        "}\n"
    );
    let position = utf16_position(source, source.find("value").expect("parameter") + 1);
    let mut lsp = LspProcess::spawn();
    lsp.initialize(fixture.path(), 1);
    lsp.send(&json!({
        "jsonrpc": "2.0", "method": "textDocument/didOpen",
        "params": { "textDocument": {
            "uri": uri, "languageId": "arandu", "version": 1, "text": source
        }}
    }));

    lsp.send(&json!({
        "jsonrpc": "2.0", "id": 2, "method": "textDocument/prepareRename",
        "params": { "textDocument": { "uri": uri }, "position": position }
    }));
    let prepared = lsp.wait_for_response(2);
    assert_eq!(
        prepared
            .pointer("/result/placeholder")
            .and_then(Value::as_str),
        Some("value"),
        "prepareRename must return the source spelling: {prepared}"
    );
    assert_eq!(
        prepared.pointer("/result/range/start/character"),
        Some(&json!(12))
    );
    assert_eq!(
        prepared.pointer("/result/range/end/character"),
        Some(&json!(17))
    );

    for (id, new_name, message_fragment) in [
        (3, "return", "non-reserved"),
        (4, "Value", "capitalization"),
        (5, "taken", "shadow"),
    ] {
        lsp.send(&json!({
            "jsonrpc": "2.0", "id": id, "method": "textDocument/rename",
            "params": {
                "textDocument": { "uri": uri }, "position": position, "newName": new_name
            }
        }));
        let rejected = lsp.wait_for_response(id);
        assert_eq!(rejected.pointer("/error/code"), Some(&json!(-32602)));
        assert!(
            rejected
                .pointer("/error/message")
                .and_then(Value::as_str)
                .is_some_and(|message| message.contains(message_fragment)),
            "rename `{new_name}` should explain its rejection: {rejected}"
        );
    }

    lsp.send(&json!({
        "jsonrpc": "2.0", "id": 6, "method": "textDocument/rename",
        "params": {
            "textDocument": { "uri": uri }, "position": position, "newName": "amount"
        }
    }));
    let renamed = lsp.wait_for_response(6);
    let edits = renamed
        .pointer(&format!(
            "/result/changes/{}",
            uri.replace('~', "~0").replace('/', "~1")
        ))
        .and_then(Value::as_array)
        .expect("workspace edit for document");
    assert_eq!(
        edits.len(),
        2,
        "declaration and use must both change: {renamed}"
    );
    assert!(edits
        .iter()
        .all(|edit| edit.pointer("/newText") == Some(&json!("amount"))));

    lsp.shutdown(7);
}

#[test]
fn stdio_formatting_is_local_and_idempotent() {
    let fixture = FixtureDir::new();
    let document = fixture.path().join("format.aru");
    let uri = file_uri(&document);
    let source = "func main(): int {\nreturn 1   \n}\n";
    let formatted = "func main(): int {\n    return 1\n}\n";
    let mut lsp = LspProcess::spawn();
    lsp.initialize(fixture.path(), 1);
    lsp.send(&json!({
        "jsonrpc": "2.0", "method": "textDocument/didOpen",
        "params": { "textDocument": {
            "uri": uri, "languageId": "arandu", "version": 1, "text": source
        }}
    }));
    lsp.send(&json!({
        "jsonrpc": "2.0", "id": 2, "method": "textDocument/formatting",
        "params": {
            "textDocument": { "uri": uri },
            "options": { "tabSize": 2, "insertSpaces": false }
        }
    }));
    let response = lsp.wait_for_response(2);
    let edits = response
        .pointer("/result")
        .and_then(Value::as_array)
        .expect("formatting edits");
    assert_eq!(
        edits.len(),
        1,
        "one changed line should produce one edit: {response}"
    );
    assert_eq!(edits[0].pointer("/range/start/line"), Some(&json!(1)));
    assert_eq!(edits[0].pointer("/range/end/line"), Some(&json!(1)));
    assert_eq!(edits[0].pointer("/newText"), Some(&json!("    return 1")));

    lsp.send(&json!({
        "jsonrpc": "2.0", "method": "textDocument/didChange",
        "params": {
            "textDocument": { "uri": uri, "version": 2 },
            "contentChanges": [{ "text": formatted }]
        }
    }));
    lsp.send(&json!({
        "jsonrpc": "2.0", "id": 3, "method": "textDocument/formatting",
        "params": {
            "textDocument": { "uri": uri },
            "options": { "tabSize": 4, "insertSpaces": true }
        }
    }));
    let stable = lsp.wait_for_response(3);
    assert_eq!(stable.pointer("/result"), Some(&json!([])));
    lsp.shutdown(4);
}

#[test]
fn stdio_semantic_requests_use_utf16_around_unicode() {
    let fixture = FixtureDir::new();
    let document = fixture.path().join("unicode-requests.aru");
    let uri = file_uri(&document);
    let source = concat!(
        "/* 😀 */ func soma(value: int): int { return value } // ação\n",
        "func main(): int { return soma(1) }\n"
    );
    let definition_start = source.find("soma").expect("soma definition");
    let call_start = source.rmatch_indices("soma").next().expect("soma call").0;
    let hover_position = utf16_position(source, definition_start + 1);
    let completion_position = utf16_position(source, definition_start + 2);
    let signature_position = utf16_position(source, call_start + "soma(".len());

    let mut lsp = LspProcess::spawn();
    lsp.initialize(fixture.path(), 1);
    lsp.send(&json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": uri,
                "languageId": "arandu",
                "version": 1,
                "text": source
            }
        }
    }));
    let initial_diagnostics = lsp.wait_for(|message| {
        message.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics")
            && message.pointer("/params/uri").and_then(Value::as_str) == Some(uri.as_str())
    });
    assert_eq!(
        initial_diagnostics
            .pointer("/params/diagnostics")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(0),
        "Unicode semantic fixture must be valid: {initial_diagnostics}"
    );

    let requests = [
        (
            2,
            "textDocument/hover",
            json!({
                "textDocument": { "uri": uri }, "position": hover_position
            }),
        ),
        (
            3,
            "textDocument/definition",
            json!({
                "textDocument": { "uri": uri }, "position": hover_position
            }),
        ),
        (
            4,
            "textDocument/references",
            json!({
                "textDocument": { "uri": uri }, "position": hover_position,
                "context": { "includeDeclaration": true }
            }),
        ),
        (
            5,
            "textDocument/rename",
            json!({
                "textDocument": { "uri": uri }, "position": hover_position,
                "newName": "somar"
            }),
        ),
        (
            6,
            "textDocument/completion",
            json!({
                "textDocument": { "uri": uri }, "position": completion_position
            }),
        ),
        (
            7,
            "textDocument/signatureHelp",
            json!({
                "textDocument": { "uri": uri }, "position": signature_position
            }),
        ),
        (
            8,
            "textDocument/semanticTokens/range",
            json!({
                "textDocument": { "uri": uri },
                "range": {
                    "start": { "line": 0, "character": 0 },
                    "end": { "line": 0, "character": 70 }
                }
            }),
        ),
    ];
    for (id, method, params) in requests {
        lsp.send(&json!({
            "jsonrpc": "2.0", "id": id, "method": method, "params": params
        }));
        let response = lsp.wait_for_response(id);
        assert!(
            response.get("error").is_none(),
            "{method} failed: {response}"
        );
        assert!(
            response
                .get("result")
                .is_some_and(|result| !result.is_null()),
            "{method} returned no semantic result at a UTF-16 position: {response}"
        );
    }

    lsp.shutdown(9);
}

#[test]
fn stdio_presents_one_signature_and_documentation_contract() {
    let fixture = FixtureDir::new();
    let document = fixture.path().join("presentation.aru");
    let uri = file_uri(&document);
    let source = concat!(
        "/// Adds two values.\n",
        "/// Keeps integer precision.\n",
        "func add(left: int, right: int): int { return left + right }\n",
        "func main(): int { return add(1, 2) }\n",
    );
    let expected_signature = "func add(left: int, right: int): int";
    let definition = source.find("add").expect("add definition");
    let call = source.rfind("add").expect("add call");

    let mut lsp = LspProcess::spawn();
    lsp.initialize(fixture.path(), 1);
    lsp.send(&json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": uri,
                "languageId": "arandu",
                "version": 1,
                "text": source
            }
        }
    }));
    let diagnostics = lsp.wait_for(|message| {
        message.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics")
            && message.pointer("/params/uri").and_then(Value::as_str) == Some(uri.as_str())
    });
    assert_eq!(
        diagnostics
            .pointer("/params/diagnostics")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(0),
        "presentation fixture must be valid: {diagnostics}"
    );

    lsp.send(&json!({
        "jsonrpc": "2.0", "id": 2, "method": "textDocument/hover",
        "params": {
            "textDocument": { "uri": uri },
            "position": utf16_position(source, definition + 1)
        }
    }));
    let hover = lsp.wait_for_response(2);
    let hover_markdown = hover
        .pointer("/result/contents/value")
        .and_then(Value::as_str)
        .expect("Markdown hover");
    assert!(hover_markdown.contains(expected_signature), "{hover}");
    assert!(hover_markdown.contains("Adds two values."), "{hover}");
    assert!(!hover_markdown.contains("SymbolId"), "{hover}");

    lsp.send(&json!({
        "jsonrpc": "2.0", "id": 3, "method": "textDocument/completion",
        "params": {
            "textDocument": { "uri": uri },
            "position": utf16_position(source, definition + 2)
        }
    }));
    let completion = lsp.wait_for_response(3);
    let add_item = completion
        .pointer("/result")
        .and_then(Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .find(|item| item.get("label") == Some(&json!("add")))
        })
        .expect("add completion item");
    assert_eq!(
        add_item.get("detail").and_then(Value::as_str),
        Some(expected_signature)
    );
    assert_eq!(
        add_item
            .pointer("/documentation/value")
            .and_then(Value::as_str),
        Some("Adds two values.\nKeeps integer precision.")
    );

    lsp.send(&json!({
        "jsonrpc": "2.0", "id": 4, "method": "textDocument/signatureHelp",
        "params": {
            "textDocument": { "uri": uri },
            "position": utf16_position(source, call + "add(1, ".len())
        }
    }));
    let signature = lsp.wait_for_response(4);
    assert_eq!(
        signature
            .pointer("/result/signatures/0/label")
            .and_then(Value::as_str),
        Some(expected_signature),
        "{signature}"
    );
    assert_eq!(
        signature
            .pointer("/result/activeParameter")
            .and_then(Value::as_u64),
        Some(1),
        "{signature}"
    );
    assert_eq!(
        signature
            .pointer("/result/signatures/0/documentation/value")
            .and_then(Value::as_str),
        Some("Adds two values.\nKeeps integer precision."),
        "{signature}"
    );

    lsp.shutdown(5);
}

#[test]
fn stdio_workspace_files_follow_open_close_create_rename_and_delete() {
    let fixture = FixtureDir::new();
    let base = fixture.path().join("base.aru");
    let base_uri = file_uri(&base);
    let mut lsp = LspProcess::spawn();
    lsp.initialize(fixture.path(), 1);
    fs::write(&base, "func disk_symbol(): int { return 1 }\n").expect("write base file");
    lsp.send(&json!({
        "jsonrpc": "2.0", "method": "workspace/didCreateFiles",
        "params": { "files": [{ "uri": base_uri }] }
    }));
    request_workspace_symbol(&mut lsp, 2, "disk_symbol", true);

    lsp.send(&json!({
        "jsonrpc": "2.0", "method": "textDocument/didOpen",
        "params": { "textDocument": {
            "uri": base_uri, "languageId": "arandu", "version": 1,
            "text": "func overlay_symbol(): int { return 2 }\n"
        }}
    }));
    let diagnostics = lsp.wait_for(|message| {
        message.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics")
            && message.pointer("/params/uri").and_then(Value::as_str) == Some(base_uri.as_str())
    });
    assert_eq!(
        diagnostics
            .pointer("/params/diagnostics")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(0),
        "overlay must be valid: {diagnostics}"
    );
    request_workspace_symbol(&mut lsp, 3, "overlay_symbol", true);

    lsp.send(&json!({
        "jsonrpc": "2.0", "method": "textDocument/didClose",
        "params": { "textDocument": { "uri": base_uri }}
    }));
    let _clear = lsp.wait_for(|message| {
        message.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics")
            && message.pointer("/params/uri").and_then(Value::as_str) == Some(base_uri.as_str())
    });
    request_workspace_symbol(&mut lsp, 4, "overlay_symbol", false);
    request_workspace_symbol(&mut lsp, 5, "disk_symbol", true);

    let created = fixture.path().join("created.aru");
    fs::write(&created, "func created_symbol(): int { return 3 }\n").expect("create source");
    let created_uri = file_uri(&created);
    lsp.send(&json!({
        "jsonrpc": "2.0", "method": "workspace/didCreateFiles",
        "params": { "files": [{ "uri": created_uri }] }
    }));
    request_workspace_symbol(&mut lsp, 6, "created_symbol", true);

    let renamed = fixture.path().join("renamed.aru");
    fs::rename(&created, &renamed).expect("rename source");
    let renamed_uri = file_uri(&renamed);
    lsp.send(&json!({
        "jsonrpc": "2.0", "method": "workspace/didRenameFiles",
        "params": { "files": [{ "oldUri": created_uri, "newUri": renamed_uri }] }
    }));
    let renamed_response = request_workspace_symbol(&mut lsp, 7, "created_symbol", true);
    assert_eq!(
        renamed_response
            .pointer("/result/0/location/uri")
            .and_then(Value::as_str),
        Some(renamed_uri.as_str()),
        "renamed symbol must point only at the new URI: {renamed_response}"
    );

    fs::remove_file(&renamed).expect("delete source");
    lsp.send(&json!({
        "jsonrpc": "2.0", "method": "workspace/didDeleteFiles",
        "params": { "files": [{ "uri": renamed_uri }] }
    }));
    request_workspace_symbol(&mut lsp, 8, "created_symbol", false);

    lsp.shutdown(9);
}

#[test]
fn stdio_package_imports_refresh_completion_goto_and_diagnostics() {
    let fixture = FixtureDir::new();
    let src = fixture.path().join("src");
    fs::create_dir_all(&src).expect("create package source directory");
    fs::write(
        fixture.path().join("Arandu.toml"),
        "name = \"editor_gold\"\nversion = \"0.1.0\"\nentry = \"src/main.aru\"\n",
    )
    .expect("write package manifest");
    let main = src.join("main.aru");
    let main_uri = file_uri(&main);
    let missing_source = concat!(
        "module editor_gold\n",
        "import editor_gold.util as util\n",
        "import std.path as path\n",
        "func main(): int {\n",
        "    if path.is_empty(\"\") { return util.answer() }\n",
        "    return 0\n",
        "}\n",
    );
    fs::write(&main, missing_source).expect("write package entry");

    let mut lsp = LspProcess::spawn();
    lsp.initialize(fixture.path(), 1);
    // Wait until background discovery has installed package metadata and the
    // entry source. Initialization itself remains non-blocking.
    request_workspace_symbol(&mut lsp, 2, "main", true);
    lsp.send(&json!({
        "jsonrpc": "2.0", "method": "textDocument/didOpen",
        "params": { "textDocument": {
            "uri": main_uri, "languageId": "arandu", "version": 1,
            "text": missing_source
        }}
    }));
    let missing = lsp.wait_for(|message| {
        message.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics")
            && message.pointer("/params/uri").and_then(Value::as_str) == Some(main_uri.as_str())
            && message
                .pointer("/params/diagnostics")
                .and_then(Value::as_array)
                .is_some_and(|diagnostics| {
                    diagnostics
                        .iter()
                        .any(|d| d.get("code") == Some(&json!("M001")))
                })
    });
    assert!(
        missing
            .pointer("/params/diagnostics")
            .and_then(Value::as_array)
            .is_some_and(|diagnostics| diagnostics
                .iter()
                .all(|d| d.get("message") != Some(&json!("module not found: std.path")))),
        "installed stdlib must resolve while the local module is missing: {missing}"
    );

    let util = src.join("util.aru");
    let util_uri = file_uri(&util);
    fs::write(
        &util,
        "/// Package answer.\npublic func answer(): int { return 42 }\n",
    )
    .expect("create imported module");
    lsp.send(&json!({
        "jsonrpc": "2.0", "method": "workspace/didCreateFiles",
        "params": { "files": [{ "uri": util_uri }] }
    }));
    request_workspace_symbol(&mut lsp, 3, "answer", true);
    let util_call = missing_source.find("util.answer").expect("util call");
    lsp.send(&json!({
        "jsonrpc": "2.0", "id": 4, "method": "textDocument/completion",
        "params": {
            "textDocument": { "uri": main_uri },
            "position": utf16_position(missing_source, util_call + "util.".len())
        }
    }));
    let completion = lsp.wait_for_response(4);
    assert!(
        completion
            .pointer("/result")
            .and_then(Value::as_array)
            .is_some_and(|items| items
                .iter()
                .any(|item| item.get("label") == Some(&json!("answer")))),
        "new module must update member completion without restart: {completion}"
    );

    lsp.send(&json!({
        "jsonrpc": "2.0", "id": 5, "method": "textDocument/definition",
        "params": {
            "textDocument": { "uri": main_uri },
            "position": utf16_position(missing_source, util_call + "util.".len() + 2)
        }
    }));
    let goto = lsp.wait_for_response(5);
    assert_eq!(
        goto.pointer("/result/uri").and_then(Value::as_str),
        Some(util_uri.as_str()),
        "goto must target the newly created module: {goto}"
    );

    let std_call = missing_source.find("path.is_empty").expect("stdlib call");
    lsp.send(&json!({
        "jsonrpc": "2.0", "id": 6, "method": "textDocument/definition",
        "params": {
            "textDocument": { "uri": main_uri },
            "position": utf16_position(missing_source, std_call + "path.".len() + 2)
        }
    }));
    let std_goto = lsp.wait_for_response(6);
    assert!(
        std_goto
            .pointer("/result/uri")
            .and_then(Value::as_str)
            .is_some_and(|uri| uri.replace('\\', "/").ends_with("/stdlib/std/path.aru")),
        "goto must use the installed stdlib root: {std_goto}"
    );

    let helper = src.join("helper.aru");
    let helper_uri = file_uri(&helper);
    fs::rename(&util, &helper).expect("rename imported module");
    lsp.send(&json!({
        "jsonrpc": "2.0", "method": "workspace/didRenameFiles",
        "params": { "files": [{ "oldUri": util_uri, "newUri": helper_uri }] }
    }));
    let renamed_missing = lsp.wait_for(|message| {
        message.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics")
            && message.pointer("/params/uri").and_then(Value::as_str) == Some(main_uri.as_str())
            && message
                .pointer("/params/diagnostics")
                .and_then(Value::as_array)
                .is_some_and(|diagnostics| {
                    diagnostics
                        .iter()
                        .any(|d| d.get("code") == Some(&json!("M001")))
                })
    });
    assert!(renamed_missing.get("params").is_some());

    let renamed_source = missing_source
        .replace("editor_gold.util as util", "editor_gold.helper as helper")
        .replace("util.answer", "helper.answer");
    lsp.send(&json!({
        "jsonrpc": "2.0", "method": "textDocument/didChange",
        "params": {
            "textDocument": { "uri": main_uri, "version": 2 },
            "contentChanges": [{ "text": renamed_source }]
        }
    }));
    lsp.send(&json!({
        "jsonrpc": "2.0", "id": 7, "method": "textDocument/completion",
        "params": { "textDocument": { "uri": main_uri }, "position": { "line": 4, "character": 47 } }
    }));
    let _ = lsp.wait_for_response(7);
    let renamed_valid = lsp.wait_for(|message| {
        message.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics")
            && message.pointer("/params/uri").and_then(Value::as_str) == Some(main_uri.as_str())
            && message.pointer("/params/version") == Some(&json!(2))
            && message
                .pointer("/params/diagnostics")
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty)
    });
    assert_eq!(renamed_valid.pointer("/params/version"), Some(&json!(2)));

    fs::remove_file(&helper).expect("delete renamed module");
    lsp.send(&json!({
        "jsonrpc": "2.0", "method": "workspace/didDeleteFiles",
        "params": { "files": [{ "uri": helper_uri }] }
    }));
    let deleted = lsp.wait_for(|message| {
        message.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics")
            && message.pointer("/params/uri").and_then(Value::as_str) == Some(main_uri.as_str())
            && message
                .pointer("/params/diagnostics")
                .and_then(Value::as_array)
                .is_some_and(|diagnostics| {
                    diagnostics
                        .iter()
                        .any(|d| d.get("code") == Some(&json!("M001")))
                })
    });
    assert_eq!(deleted.pointer("/params/version"), Some(&json!(2)));

    lsp.shutdown(8);
}

fn request_workspace_symbol(lsp: &mut LspProcess, id: i64, query: &str, expected: bool) -> Value {
    let deadline = Instant::now() + MESSAGE_TIMEOUT;
    let response = loop {
        lsp.send(&json!({
            "jsonrpc": "2.0", "id": id, "method": "workspace/symbol",
            "params": { "query": query }
        }));
        let response = lsp.wait_for_response(id);
        if response.pointer("/error/code").and_then(Value::as_i64) == Some(-32801) {
            continue;
        }
        let found = response
            .pointer("/result")
            .and_then(Value::as_array)
            .is_some_and(|symbols| !symbols.is_empty());
        if expected && !found && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
            continue;
        }
        break response;
    };
    assert!(
        response.get("error").is_none(),
        "workspace/symbol failed: {response}"
    );
    let symbols = response
        .pointer("/result")
        .and_then(Value::as_array)
        .expect("workspace symbol array");
    assert_eq!(
        !symbols.is_empty(),
        expected,
        "workspace symbol presence mismatch for {query}: {response}"
    );
    response
}

#[test]
fn stdio_diagnostics_preserve_context_and_structured_quick_fix() {
    let fixture = FixtureDir::new();
    let document = fixture.path().join("rich-diagnostic.aru");
    let uri = file_uri(&document);
    let mut lsp = LspProcess::spawn();
    lsp.initialize(fixture.path(), 1);
    lsp.send(&json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": uri,
                "languageId": "arandu",
                "version": 7,
                "text": "func main() { let value: int = 1; set value = 2; }\n"
            }
        }
    }));
    let published = lsp.wait_for(|message| {
        message.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics")
            && message.pointer("/params/uri").and_then(Value::as_str) == Some(uri.as_str())
            && message
                .pointer("/params/diagnostics")
                .and_then(Value::as_array)
                .is_some_and(|diagnostics| {
                    diagnostics
                        .iter()
                        .any(|diagnostic| diagnostic.get("code") == Some(&json!("T026")))
                })
    });
    assert_eq!(published.pointer("/params/version"), Some(&json!(7)));
    let diagnostic = published
        .pointer("/params/diagnostics")
        .and_then(Value::as_array)
        .and_then(|diagnostics| {
            diagnostics
                .iter()
                .find(|diagnostic| diagnostic.get("code") == Some(&json!("T026")))
        })
        .expect("T026 diagnostic")
        .clone();
    assert!(
        diagnostic
            .pointer("/codeDescription/href")
            .and_then(Value::as_str)
            .is_some_and(|href| href.ends_with("/docs/errors/T026.md")),
        "diagnostic must link its documentation: {diagnostic}"
    );
    assert!(
        diagnostic
            .get("relatedInformation")
            .and_then(Value::as_array)
            .is_some_and(|labels| labels.len() >= 2),
        "compiler labels must remain structured: {diagnostic}"
    );
    assert!(
        diagnostic
            .pointer("/data/fixes/0/newText")
            .and_then(Value::as_str)
            .is_some_and(|text| text == "mut value"),
        "compiler replacement must remain structured: {diagnostic}"
    );

    // Prove the quick fix does not infer anything from presentation text.
    let mut action_diagnostic = diagnostic;
    action_diagnostic["message"] = json!("localized presentation text");
    let range = action_diagnostic["range"].clone();
    lsp.send(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "textDocument/codeAction",
        "params": {
            "textDocument": { "uri": uri },
            "range": range,
            "context": { "diagnostics": [action_diagnostic], "only": ["quickfix"] }
        }
    }));
    let actions = lsp.wait_for_response(2);
    assert_eq!(
        actions
            .pointer("/result/0/edit/changes")
            .and_then(Value::as_object)
            .and_then(|changes| changes.values().next())
            .and_then(Value::as_array)
            .and_then(|edits| edits.first())
            .and_then(|edit| edit.get("newText"))
            .and_then(Value::as_str),
        Some("mut value"),
        "quick fix must consume Diagnostic.data instead of parsing its message: {actions}"
    );

    // A new document version must be published even when diagnostics have the
    // same semantic fingerprint, otherwise clients may reject them as stale.
    lsp.send(&json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didChange",
        "params": {
            "textDocument": { "uri": uri, "version": 8 },
            "contentChanges": [{
                "text": "func main() { let value: int = 1; set value = 2; }\n"
            }]
        }
    }));
    lsp.send(&json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "textDocument/completion",
        "params": {
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 4 }
        }
    }));
    // The diagnostics worker and completion worker are deliberately
    // concurrent. Do not consume and discard a valid diagnostic merely because
    // it overtook the response that forced the pending edit to flush.
    let republished = lsp.wait_for(|message| {
        message.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics")
            && message.pointer("/params/uri").and_then(Value::as_str) == Some(uri.as_str())
            && message.pointer("/params/version") == Some(&json!(8))
    });
    assert_eq!(republished.pointer("/params/version"), Some(&json!(8)));

    lsp.shutdown(4);
}

#[test]
fn stdio_cancel_request_returns_request_cancelled() {
    let fixture = FixtureDir::new();
    let document = fixture.path().join("cancel.aru");
    let uri = file_uri(&document);
    let mut source = String::new();
    for index in 0..8_000 {
        use std::fmt::Write as _;
        writeln!(
            source,
            "func item{index}() {{ let value{index}: int = {index}; }}"
        )
        .expect("write cancellation fixture");
    }

    let mut lsp = LspProcess::spawn();
    lsp.initialize(fixture.path(), 1);
    lsp.send(&json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": uri,
                "languageId": "arandu",
                "version": 1,
                "text": source
            }
        }
    }));

    for id in 2..=5 {
        lsp.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "textDocument/semanticTokens/full",
            "params": { "textDocument": { "uri": uri } }
        }));
    }
    lsp.send(&json!({
        "jsonrpc": "2.0",
        "id": 6,
        "method": "textDocument/completion",
        "params": {
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 4 }
        }
    }));
    lsp.send(&json!({
        "jsonrpc": "2.0",
        "method": "$/cancelRequest",
        "params": { "id": 6 }
    }));

    let response = lsp.wait_for_response(6);
    assert_eq!(
        response.pointer("/error/code").and_then(Value::as_i64),
        Some(-32800),
        "cancelled request must receive the LSP RequestCancelled error: {response}"
    );
    lsp.shutdown(7);
}

#[test]
fn stdio_concurrent_requests_keep_open_documents_isolated() {
    let fixture = FixtureDir::new();
    let alpha_uri = file_uri(&fixture.path().join("alpha.aru"));
    let beta_uri = file_uri(&fixture.path().join("beta.aru"));
    let mut lsp = LspProcess::spawn();
    lsp.initialize(fixture.path(), 1);

    for (uri, text) in [
        (&alpha_uri, "func alpha_value(): int { return 1 }\n"),
        (&beta_uri, "func beta_value(): int { return 2 }\n"),
    ] {
        lsp.send(&json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": uri,
                    "languageId": "arandu",
                    "version": 1,
                    "text": text
                }
            }
        }));
    }

    for id in 2..=17 {
        let uri = if id % 2 == 0 { &alpha_uri } else { &beta_uri };
        lsp.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "textDocument/documentSymbol",
            "params": { "textDocument": { "uri": uri } }
        }));
    }

    let responses = lsp.wait_for_responses(2..=17);
    for (id, response) in responses {
        assert!(
            response.get("error").is_none(),
            "request {id} failed: {response}"
        );
        let names = response
            .get("result")
            .and_then(Value::as_array)
            .expect("documentSymbol must return an array")
            .iter()
            .filter_map(|symbol| symbol.get("name").and_then(Value::as_str))
            .collect::<Vec<_>>();
        let expected = if id % 2 == 0 {
            "alpha_value"
        } else {
            "beta_value"
        };
        let foreign = if id % 2 == 0 {
            "beta_value"
        } else {
            "alpha_value"
        };
        assert!(
            names.contains(&expected),
            "request {id} lost its document snapshot: {response}"
        );
        assert!(
            !names.contains(&foreign),
            "request {id} leaked symbols from another document: {response}"
        );
    }

    lsp.shutdown(18);
}

#[test]
fn stdio_cst_navigation_features_are_advertised_and_structured() {
    let fixture = FixtureDir::new();
    let uri = file_uri(&fixture.path().join("structure.aru"));
    let source = concat!(
        "/** heading\ncontinued */\n",
        "func add(value: int): int {\n",
        "    return value\n",
        "}\n",
        "func main(): int {\n",
        "    return add(1)\n",
        "}\n",
    );
    let mut lsp = LspProcess::spawn();
    lsp.initialize(fixture.path(), 1);
    lsp.send(&json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": uri,
                "languageId": "arandu",
                "version": 1,
                "text": source
            }
        }
    }));
    let _ = lsp.wait_for(|message| {
        message.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics")
            && message.pointer("/params/uri").and_then(Value::as_str) == Some(uri.as_str())
    });

    lsp.send(&json!({
        "jsonrpc": "2.0", "id": 2, "method": "textDocument/foldingRange",
        "params": { "textDocument": { "uri": uri } }
    }));
    let folding = lsp.wait_for_response(2);
    assert!(
        folding
            .pointer("/result")
            .and_then(Value::as_array)
            .is_some_and(|ranges| ranges
                .iter()
                .any(|range| range.get("kind") == Some(&json!("comment")))
                && ranges
                    .iter()
                    .filter(|range| range.get("kind").is_none())
                    .count()
                    >= 2),
        "folding must expose multiline comments and CST blocks: {folding}"
    );

    lsp.send(&json!({
        "jsonrpc": "2.0", "id": 3, "method": "textDocument/selectionRange",
        "params": {
            "textDocument": { "uri": uri },
            "positions": [{ "line": 6, "character": 12 }]
        }
    }));
    let selection = lsp.wait_for_response(3);
    assert!(
        selection.pointer("/result/0/parent/parent").is_some(),
        "selection must return an enclosing CST chain: {selection}"
    );

    lsp.send(&json!({
        "jsonrpc": "2.0", "id": 4, "method": "textDocument/documentHighlight",
        "params": {
            "textDocument": { "uri": uri },
            "position": { "line": 2, "character": 7 }
        }
    }));
    let highlights = lsp.wait_for_response(4);
    assert_eq!(
        highlights
            .pointer("/result")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(2),
        "document highlights must use resolved identity: {highlights}"
    );
    lsp.shutdown(5);
}

#[derive(Debug)]
struct LspPerfBudget {
    samples: usize,
    warmup: usize,
    diagnostics_p95: Duration,
    completion_p95: Duration,
    goto_p95: Duration,
    rename_p95: Duration,
}

#[test]
#[ignore = "native performance campaign; run explicitly in the L2 CI job"]
fn stdio_l2_corpus_performance_p50_p95() {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("arandu_lsp crate must be inside the workspace")
        .to_path_buf();
    let budget = load_lsp_perf_budget(&workspace.join("tests/perf/lsp-l2-baseline.txt"));
    let main_text = fs::read_to_string(workspace.join("tests/perf/lsp-l2.aru"))
        .expect("read versioned LSP corpus");

    let fixture = FixtureDir::new();
    let main_uri = file_uri(&fixture.path().join("lsp-l2.aru"));

    let mut lsp = LspProcess::spawn();
    lsp.initialize(fixture.path(), 1);
    lsp.send(&json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": main_uri,
                "languageId": "arandu",
                "version": 1,
                "text": main_text
            }
        }
    }));
    let initial_diagnostics = lsp.wait_for(|message| {
        message.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics")
            && message.pointer("/params/uri").and_then(Value::as_str) == Some(main_uri.as_str())
            && message.pointer("/params/version") == Some(&json!(1))
    });
    assert_eq!(
        initial_diagnostics
            .pointer("/params/diagnostics")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(0),
        "versioned performance corpus must be valid: {initial_diagnostics}"
    );

    let definition = main_text.find("add").expect("function definition");
    let completion_position = utf16_position(&main_text, definition + 2);
    let symbol_position = utf16_position(&main_text, definition + 2);

    let mut next_id = 2_i64;
    for _ in 0..budget.warmup {
        let _ = timed_request(
            &mut lsp,
            next_id,
            "textDocument/completion",
            json!({
                "textDocument": { "uri": main_uri },
                "position": completion_position
            }),
        );
        next_id += 1;
    }

    let mut completion = Vec::with_capacity(budget.samples);
    let mut goto = Vec::with_capacity(budget.samples);
    let mut rename = Vec::with_capacity(budget.samples);
    for sample in 0..budget.samples {
        let (elapsed, response) = timed_request(
            &mut lsp,
            next_id,
            "textDocument/completion",
            json!({
                "textDocument": { "uri": main_uri },
                "position": completion_position
            }),
        );
        assert!(
            response.get("error").is_none(),
            "completion failed: {response}"
        );
        completion.push(elapsed);
        next_id += 1;

        let (elapsed, response) = timed_request(
            &mut lsp,
            next_id,
            "textDocument/definition",
            json!({
                "textDocument": { "uri": main_uri },
                "position": symbol_position
            }),
        );
        assert_eq!(
            response.pointer("/result/uri").and_then(Value::as_str),
            Some(main_uri.as_str()),
            "goto must resolve the function from the measured snapshot: {response}"
        );
        goto.push(elapsed);
        next_id += 1;

        let new_name = format!("measured_boxed_{sample}");
        let (elapsed, response) = timed_request(
            &mut lsp,
            next_id,
            "textDocument/rename",
            json!({
                "textDocument": { "uri": main_uri },
                "position": symbol_position,
                "newName": new_name
            }),
        );
        assert!(
            response
                .pointer("/result/changes")
                .and_then(Value::as_object)
                .is_some_and(|changes| changes.contains_key(&main_uri)),
            "rename must produce a workspace edit for the measured document: {response}"
        );
        rename.push(elapsed);
        next_id += 1;
    }

    let invalid_text = main_text.replace("return add(20, 22)", "return missing_performance_symbol");
    let mut diagnostics = Vec::with_capacity(budget.samples);
    for (sample, version) in (0..budget.samples).zip(2_i32..) {
        let text = if sample.is_multiple_of(2) {
            &invalid_text
        } else {
            &main_text
        };
        let started = Instant::now();
        lsp.send(&json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": main_uri, "version": version },
                "contentChanges": [{ "text": text }]
            }
        }));
        lsp.send(&json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didSave",
            "params": { "textDocument": { "uri": main_uri } }
        }));
        let published = lsp.wait_for(|message| {
            message.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics")
                && message.pointer("/params/uri").and_then(Value::as_str) == Some(main_uri.as_str())
                && message.pointer("/params/version") == Some(&json!(version))
        });
        let count = published
            .pointer("/params/diagnostics")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        assert_eq!(
            count > 0,
            sample.is_multiple_of(2),
            "diagnostic result must match the measured revision: {published}"
        );
        diagnostics.push(started.elapsed());
    }

    let diagnostics_p50 = percentile(&diagnostics, 50);
    let diagnostics_p95 = percentile(&diagnostics, 95);
    let completion_p50 = percentile(&completion, 50);
    let completion_p95 = percentile(&completion, 95);
    let goto_p50 = percentile(&goto, 50);
    let goto_p95 = percentile(&goto, 95);
    let rename_p50 = percentile(&rename, 50);
    let rename_p95 = percentile(&rename, 95);
    assert!(
        diagnostics_p95 <= budget.diagnostics_p95,
        "diagnostics p95 {diagnostics_p95:?} exceeds {:?}",
        budget.diagnostics_p95
    );
    assert!(
        completion_p95 <= budget.completion_p95,
        "completion p95 {completion_p95:?} exceeds {:?}",
        budget.completion_p95
    );
    assert!(
        goto_p95 <= budget.goto_p95,
        "goto p95 {goto_p95:?} exceeds {:?}",
        budget.goto_p95
    );
    assert!(
        rename_p95 <= budget.rename_p95,
        "rename p95 {rename_p95:?} exceeds {:?}",
        budget.rename_p95
    );

    let report = format!(
        "check-lsp-performance: ok\nprotocol=1 corpus=tests/perf/lsp-l2.aru samples={} warmup={}\nplatform={} arch={} commit={}\ndiagnostics_p50_us={} diagnostics_p95_us={}\ncompletion_p50_us={} completion_p95_us={}\ngoto_p50_us={} goto_p95_us={}\nrename_p50_us={} rename_p95_us={}\n",
        budget.samples,
        budget.warmup,
        std::env::consts::OS,
        std::env::consts::ARCH,
        std::env::var("GITHUB_SHA").unwrap_or_else(|_| "local-working-tree".into()),
        diagnostics_p50.as_micros(),
        diagnostics_p95.as_micros(),
        completion_p50.as_micros(),
        completion_p95.as_micros(),
        goto_p50.as_micros(),
        goto_p95.as_micros(),
        rename_p50.as_micros(),
        rename_p95.as_micros(),
    );
    let report_path = workspace.join("target/l2-lsp-performance-report.txt");
    fs::write(&report_path, &report).expect("write L2 LSP performance report");
    eprintln!("{report}");
    lsp.shutdown(next_id);
}

fn timed_request(lsp: &mut LspProcess, id: i64, method: &str, params: Value) -> (Duration, Value) {
    let started = Instant::now();
    lsp.send(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params
    }));
    let response = lsp.wait_for_response(id);
    (started.elapsed(), response)
}

fn load_lsp_perf_budget(path: &Path) -> LspPerfBudget {
    let text = fs::read_to_string(path).expect("read L2 LSP performance budget");
    let values = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            line.split_once('=')
                .expect("performance budget uses key=value")
        })
        .collect::<BTreeMap<_, _>>();
    let number = |key: &str| {
        values
            .get(key)
            .unwrap_or_else(|| panic!("missing LSP performance budget {key}"))
            .parse::<u64>()
            .unwrap_or_else(|error| panic!("invalid LSP performance budget {key}: {error}"))
    };
    LspPerfBudget {
        samples: usize::try_from(number("samples")).expect("samples fit usize"),
        warmup: usize::try_from(number("warmup")).expect("warmup fits usize"),
        diagnostics_p95: Duration::from_micros(number("max_diagnostics_p95_us")),
        completion_p95: Duration::from_micros(number("max_completion_p95_us")),
        goto_p95: Duration::from_micros(number("max_goto_p95_us")),
        rename_p95: Duration::from_micros(number("max_rename_p95_us")),
    }
}

fn read_message(reader: &mut impl BufRead) -> std::io::Result<Option<Value>> {
    let mut content_length = None;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 {
            return Ok(None);
        }
        if header == "\r\n" || header == "\n" {
            break;
        }
        if let Some(value) = header
            .strip_prefix("Content-Length:")
            .and_then(|value| value.trim().parse::<usize>().ok())
        {
            content_length = Some(value);
        }
    }
    let length = content_length.ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "missing Content-Length")
    })?;
    let mut payload = vec![0; length];
    reader.read_exact(&mut payload)?;
    serde_json::from_slice(&payload)
        .map(Some)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

fn file_uri(path: &Path) -> String {
    let absolute = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let normalized = absolute.to_string_lossy().replace('\\', "/");
    let encoded = encode_uri_path(&normalized);
    if normalized.starts_with('/') {
        format!("file://{encoded}")
    } else {
        format!("file:///{encoded}")
    }
}

fn populate_adversarial_workspace(root: &Path) {
    let source = "// discovery must remain background work\n".repeat(1_024);
    for index in 0..256 {
        fs::write(root.join(format!("module-{index:03}.aru")), &source)
            .expect("write startup fixture");
    }
}

fn encode_uri_path(path: &str) -> String {
    let mut encoded = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/' | b':') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            write!(encoded, "%{byte:02X}").expect("writing to String cannot fail");
        }
    }
    encoded
}

fn utf16_position(text: &str, byte_offset: usize) -> Value {
    assert!(text.is_char_boundary(byte_offset));
    let prefix = &text[..byte_offset];
    let line = prefix.bytes().filter(|&byte| byte == b'\n').count();
    let line_start = prefix.rfind('\n').map_or(0, |offset| offset + 1);
    json!({
        "line": u32::try_from(line).expect("fixture line fits u32"),
        "character": u32::try_from(text[line_start..byte_offset].encode_utf16().count())
            .expect("fixture UTF-16 column fits u32")
    })
}

fn percentile(samples: &[Duration], percentile: usize) -> Duration {
    assert!(!samples.is_empty());
    let mut ordered = samples.to_vec();
    ordered.sort_unstable();
    let rank = percentile
        .saturating_mul(ordered.len())
        .div_ceil(100)
        .saturating_sub(1)
        .min(ordered.len() - 1);
    ordered[rank]
}

#[test]
fn percentile_is_nearest_rank_and_order_independent() {
    let samples = [9, 1, 5, 3, 7].map(Duration::from_micros);
    assert_eq!(percentile(&samples, 50), Duration::from_micros(5));
    assert_eq!(percentile(&samples, 95), Duration::from_micros(9));
}

#[test]
fn file_uri_encodes_spaces_and_unicode_as_utf8() {
    let uri = file_uri(Path::new("/tmp/Arandu Gold/ação.aru"));
    assert!(uri.ends_with("/Arandu%20Gold/a%C3%A7%C3%A3o.aru"), "{uri}");
}
