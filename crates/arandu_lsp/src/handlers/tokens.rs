//! Semantic tokens handlers (full document and range).

use super::HandlerCtx;
use crate::conv::position_to_offset;
use crate::dispatcher;
use crate::ide;
use arandu_base::LineIndex;
use lsp_server::RequestId;

pub(super) fn semantic_tokens_full(
    ctx: &mut HandlerCtx<'_>,
    id: RequestId,
    params: lsp_types::SemanticTokensParams,
) {
    let uri = params.text_document.uri;
    dispatcher::spawn_json(ctx.state, ctx.pool, ctx.job_tx, id, move |snap, docs| {
        let Some(info) = docs.get(uri.as_str()) else {
            return serde_json::Value::Null;
        };
        let tokens = ide::semantic_tokens(snap, info.source);
        serde_json::to_value(tokens).unwrap_or(serde_json::Value::Null)
    });
}

pub(super) fn semantic_tokens_range(
    ctx: &mut HandlerCtx<'_>,
    id: RequestId,
    params: lsp_types::SemanticTokensRangeParams,
) {
    let uri = params.text_document.uri;
    let range = params.range;
    dispatcher::spawn_json(ctx.state, ctx.pool, ctx.job_tx, id, move |snap, docs| {
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
