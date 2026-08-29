//! Document and workspace symbol discovery.

use arandu_base::LineIndex;
use arandu_middle::SymbolKind;
use arandu_query::{AnalysisSnapshot, SourceFile};
use lsp_types::{Location, SymbolInformation, SymbolKind as LspSymbolKind, Uri};

use super::presentation::typecheck;
use super::types::DocSnap;
use crate::conv::span_to_range;

#[must_use]
#[allow(deprecated)] // SymbolInformation::deprecated field in lsp-types 0.94
pub fn document_symbols(
    snap: &AnalysisSnapshot,
    source: SourceFile,
    text: &str,
    uri: &Uri,
) -> Vec<SymbolInformation> {
    let index = LineIndex::new(text);
    let tc = typecheck(snap, source);
    let mut out = Vec::new();
    for symbol in tc.symbols.iter() {
        // Top-level-ish: global scope or methods
        let kind = match symbol.kind {
            SymbolKind::Func | SymbolKind::AssociatedFunc | SymbolKind::ExternFunc => {
                LspSymbolKind::FUNCTION
            }
            SymbolKind::Struct => LspSymbolKind::STRUCT,
            SymbolKind::Enum => LspSymbolKind::ENUM,
            SymbolKind::Interface => LspSymbolKind::INTERFACE,
            SymbolKind::Const => LspSymbolKind::CONSTANT,
            SymbolKind::TypeAlias => LspSymbolKind::TYPE_PARAMETER,
            SymbolKind::Field => LspSymbolKind::FIELD,
            SymbolKind::EnumVariant => LspSymbolKind::ENUM_MEMBER,
            _ => continue,
        };
        let range = span_to_range(&index, symbol.span);
        out.push(SymbolInformation {
            name: symbol.name.to_string(),
            kind,
            tags: None,
            deprecated: None,
            location: Location {
                uri: uri.clone(),
                range,
            },
            container_name: None,
        });
    }
    out
}

#[must_use]
#[allow(deprecated)]
pub fn workspace_symbols(
    snap: &AnalysisSnapshot,
    docs: &[DocSnap],
    query: &str,
) -> Vec<SymbolInformation> {
    let q = query.to_ascii_lowercase();
    let mut out = Vec::new();
    for doc in docs {
        let text = doc.source.text(&snap.db);
        let index = LineIndex::new(text);
        let tc = typecheck(snap, doc.source);
        for symbol in tc.symbols.iter() {
            let name = symbol.name.to_string();
            if !q.is_empty() && !name.to_ascii_lowercase().contains(&q) {
                continue;
            }
            let kind = match symbol.kind {
                SymbolKind::Func | SymbolKind::AssociatedFunc | SymbolKind::ExternFunc => {
                    LspSymbolKind::FUNCTION
                }
                SymbolKind::Struct => LspSymbolKind::STRUCT,
                SymbolKind::Enum => LspSymbolKind::ENUM,
                SymbolKind::Interface => LspSymbolKind::INTERFACE,
                SymbolKind::Const => LspSymbolKind::CONSTANT,
                _ => continue,
            };
            out.push(SymbolInformation {
                name,
                kind,
                tags: None,
                deprecated: None,
                location: Location {
                    uri: doc.uri.clone(),
                    range: span_to_range(&index, symbol.span),
                },
                container_name: Some(doc.path.display().to_string()),
            });
        }
    }
    out.truncate(200);
    out
}
