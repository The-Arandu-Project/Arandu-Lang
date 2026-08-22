use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MESSAGE_TIMEOUT: Duration = Duration::from_secs(3);
const INITIALIZE_P95_BUDGET: Duration = Duration::from_millis(250);
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
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let message = self
                .messages
                .recv_timeout(remaining)
                .expect("timed out waiting for LSP message");
            if predicate(&message) {
                return message;
            }
        }
    }

    fn wait_for_response(&self, id: i64) -> Value {
        self.wait_for(|message| message.get("id").and_then(Value::as_i64) == Some(id))
    }

    fn initialize(&mut self, root: &Path, id: i64) -> Duration {
        let started = Instant::now();
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "processId": null,
                "capabilities": {},
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
fn stdio_cold_initialize_p95_stays_within_budget() {
    let fixture = FixtureDir::new();
    let source = "// startup must not read this file\n".repeat(1_024);
    for index in 0..256 {
        fs::write(
            fixture.path().join(format!("module-{index:03}.aru")),
            &source,
        )
        .expect("write startup fixture");
    }

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

fn percentile(samples: &[Duration], percentile: usize) -> Duration {
    assert!(!samples.is_empty());
    let rank = percentile
        .saturating_mul(samples.len())
        .div_ceil(100)
        .saturating_sub(1)
        .min(samples.len() - 1);
    samples[rank]
}

#[test]
fn file_uri_encodes_spaces_and_unicode_as_utf8() {
    let uri = file_uri(Path::new("/tmp/Arandu Gold/ação.aru"));
    assert!(uri.ends_with("/Arandu%20Gold/a%C3%A7%C3%A3o.aru"), "{uri}");
}
