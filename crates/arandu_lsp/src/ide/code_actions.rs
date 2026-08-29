//! Code action generation from structured compiler diagnostics.

use std::collections::HashMap;

use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionResponse, TextEdit as LspTextEdit,
    Uri, WorkspaceEdit,
};

use super::types::DiagnosticData;

/// Code actions backed by structured compiler replacements carried in
/// `Diagnostic.data`; messages are presentation only and are never parsed.
#[must_use]
#[allow(clippy::mutable_key_type)] // WorkspaceEdit keys are `Uri` in lsp-types 0.97
pub fn code_actions(_uri: &Uri, context: &lsp_types::CodeActionContext) -> CodeActionResponse {
    let mut out = Vec::new();
    for d in &context.diagnostics {
        let Some(data) = d
            .data
            .clone()
            .and_then(|value| serde_json::from_value::<DiagnosticData>(value).ok())
        else {
            continue;
        };
        for fix in data.fixes {
            let mut by_str: HashMap<String, Vec<LspTextEdit>> = HashMap::new();
            by_str.insert(
                fix.uri.as_str().to_string(),
                vec![LspTextEdit {
                    range: fix.range,
                    new_text: fix.new_text,
                }],
            );
            let changes: HashMap<Uri, Vec<LspTextEdit>> = by_str
                .into_iter()
                .filter_map(|(s, edits)| crate::uri_util::parse_uri(&s).map(|u| (u, edits)))
                .collect();
            out.push(CodeActionOrCommand::CodeAction(CodeAction {
                title: fix.title,
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: Some(vec![d.clone()]),
                edit: Some(WorkspaceEdit {
                    changes: Some(changes),
                    ..WorkspaceEdit::default()
                }),
                ..CodeAction::default()
            }));
        }
    }
    out
}
