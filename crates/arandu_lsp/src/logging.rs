//! Operator diagnostics go to stderr, never the LSP protocol stream.

pub(crate) fn log_panic(context: &str, payload: &(dyn std::any::Any + Send)) {
    let message = if let Some(message) = payload.downcast_ref::<&str>() {
        *message
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.as_str()
    } else {
        "non-string panic payload"
    };
    eprintln!("arandu-lsp: {context} panicked: {message}");
}
