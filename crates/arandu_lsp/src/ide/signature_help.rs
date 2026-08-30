//! Interactive parameter and signature help for function/method calls.

use arandu_base::LineIndex;
use arandu_middle::SymbolKind;
use arandu_query::{AnalysisSnapshot, SourceFile};
use lsp_types::{ParameterInformation, ParameterLabel, Position};

use super::presentation::{markdown_documentation, symbol_at, symbol_presentation, typecheck};
use crate::conv::position_to_offset;

#[must_use]
pub fn signature_help(
    snap: &AnalysisSnapshot,
    source: SourceFile,
    text: &str,
    position: Position,
) -> Option<lsp_types::SignatureHelp> {
    let index = LineIndex::new(text);
    let offset = position_to_offset(&index, position, text);
    let context = call_context(snap, source, offset)?;

    let tc = typecheck(snap, source);
    let sym = symbol_at(&tc, context.callee_start).or_else(|| {
        tc.symbols
            .iter()
            .find(|symbol| {
                symbol.name.as_str() == context.name
                    && matches!(
                        symbol.kind,
                        SymbolKind::Func | SymbolKind::AssociatedFunc | SymbolKind::ExternFunc
                    )
            })
            .map(|symbol| symbol.id)
    })?;
    let symbol = tc.symbols.try_get(sym)?;
    if !matches!(
        symbol.kind,
        SymbolKind::Func | SymbolKind::AssociatedFunc | SymbolKind::ExternFunc
    ) {
        return None;
    }
    let presentation = symbol_presentation(snap, source, &tc, symbol);
    let active_parameter = (!presentation.parameters.is_empty()).then(|| {
        context
            .active_parameter
            .min(u32::try_from(presentation.parameters.len() - 1).unwrap_or(u32::MAX))
    });
    let parameters = presentation
        .parameters
        .into_iter()
        .map(|parameter| ParameterInformation {
            label: ParameterLabel::Simple(parameter.label),
            documentation: None,
        })
        .collect();
    Some(lsp_types::SignatureHelp {
        signatures: vec![lsp_types::SignatureInformation {
            label: presentation.signature,
            documentation: presentation.documentation.map(markdown_documentation),
            parameters: Some(parameters),
            active_parameter,
        }],
        active_signature: Some(0),
        active_parameter,
    })
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CallContext {
    pub(crate) name: String,
    pub(crate) callee_start: u32,
    pub(crate) active_parameter: u32,
}

pub(crate) fn call_context(
    snap: &AnalysisSnapshot,
    source: SourceFile,
    cursor_offset: u32,
) -> Option<CallContext> {
    let tree = arandu_query::passes::syntax_tree(&snap.db, source);
    let tokens: Vec<_> = tree
        .root()
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| {
            !token.kind().is_trivia() && u32::from(token.text_range().start()) < cursor_offset
        })
        .collect();

    let mut parenthesis_depth = 0_u32;
    let open_index = tokens.iter().enumerate().rev().find_map(|(index, token)| {
        match token.text() {
            ")" => parenthesis_depth = parenthesis_depth.saturating_add(1),
            "(" if parenthesis_depth == 0 => return Some(index),
            "(" => parenthesis_depth = parenthesis_depth.saturating_sub(1),
            _ => {}
        }
        None
    })?;
    let callee = tokens[..open_index].iter().rev().find(|token| {
        matches!(
            token.kind(),
            arandu_parser::SyntaxKind::IDENT | arandu_parser::SyntaxKind::TYPE_IDENT
        )
    })?;

    let mut delimiter_depth = 0_u32;
    let mut active_parameter = 0_u32;
    for token in &tokens[open_index + 1..] {
        match token.text() {
            "(" | "[" | "{" => delimiter_depth = delimiter_depth.saturating_add(1),
            ")" | "]" | "}" => delimiter_depth = delimiter_depth.saturating_sub(1),
            "," if delimiter_depth == 0 => {
                active_parameter = active_parameter.saturating_add(1);
            }
            _ => {}
        }
    }
    Some(CallContext {
        name: callee.text().to_string(),
        callee_start: u32::from(callee.text_range().start()),
        active_parameter,
    })
}
