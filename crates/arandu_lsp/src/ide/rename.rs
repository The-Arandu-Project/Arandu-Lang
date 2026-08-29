//! Rename symbol validation, occurrence collection, and workspace edits.

use std::collections::HashMap;

use arandu_base::LineIndex;
use arandu_query::{AnalysisSnapshot, SourceFile};
use lsp_types::{Position, Uri};

use super::types::DocSnap;
use crate::conv::{position_to_offset, span_to_range};

pub fn prepare_rename(
    snap: &AnalysisSnapshot,
    source: SourceFile,
    text: &str,
    position: Position,
) -> Result<lsp_types::PrepareRenameResponse, arandu_query::RenameError> {
    let index = LineIndex::new(text);
    let offset = position_to_offset(&index, position, text);
    let target = arandu_query::prepare_rename(&snap.db, source, offset)?;
    Ok(lsp_types::PrepareRenameResponse::RangeWithPlaceholder {
        range: span_to_range(&index, target.occurrence),
        placeholder: target.placeholder,
    })
}

// lsp-types 0.97 Uri wraps fluent_uri with interior mutability; protocol map keys are still Uri.
#[allow(clippy::mutable_key_type)]
pub fn rename_edits(
    snap: &AnalysisSnapshot,
    source: SourceFile,
    text: &str,
    position: Position,
    documents: &[DocSnap],
    new_name: &str,
) -> Result<lsp_types::WorkspaceEdit, arandu_query::RenameError> {
    let index = LineIndex::new(text);
    let offset = position_to_offset(&index, position, text);
    let target = arandu_query::validate_rename(&snap.db, source, offset, new_name)?;
    // Uri is not a plain Hash key (fluent_uri interior mutability); key by string.
    let mut changes: HashMap<String, Vec<lsp_types::TextEdit>> = HashMap::new();
    for document in documents {
        let document_text = document.source.text(&snap.db);
        let document_index = LineIndex::new(document_text);
        for span in arandu_query::rename_occurrences(&snap.db, document.source, target.symbol) {
            changes
                .entry(document.uri.as_str().to_string())
                .or_default()
                .push(lsp_types::TextEdit {
                    range: span_to_range(&document_index, span),
                    new_text: new_name.to_string(),
                });
        }
    }
    if changes.is_empty() {
        return Err(arandu_query::RenameError::NotRenameable);
    }
    for edits in changes.values_mut() {
        edits.sort_by_key(|edit| {
            (
                edit.range.start.line,
                edit.range.start.character,
                edit.range.end.line,
                edit.range.end.character,
            )
        });
        edits.dedup_by(|left, right| left.range == right.range);
    }
    let changes: HashMap<Uri, Vec<lsp_types::TextEdit>> = changes
        .into_iter()
        .filter_map(|(s, edits)| crate::uri_util::parse_uri(&s).map(|u| (u, edits)))
        .collect();
    Ok(lsp_types::WorkspaceEdit {
        changes: Some(changes),
        ..lsp_types::WorkspaceEdit::default()
    })
}
