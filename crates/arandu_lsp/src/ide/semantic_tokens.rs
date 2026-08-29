//! Semantic token generation and delta encoding.

use arandu_base::LineIndex;
use arandu_query::{AnalysisSnapshot, SourceFile};
use lsp_types::{
    Position, SemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokens,
    SemanticTokensLegend,
};

use crate::conv::{offset_to_position, utf16_len};

/// Legend order for semantic tokens (must match [`arandu_query::HlKind`] discriminant).
///
/// Modifiers bit order: declaration=0, modification/mutable=1, definition=2 (F2b).
#[must_use]
pub fn semantic_tokens_legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: vec![
            SemanticTokenType::KEYWORD,     // 0
            SemanticTokenType::FUNCTION,    // 1
            SemanticTokenType::VARIABLE,    // 2
            SemanticTokenType::PARAMETER,   // 3
            SemanticTokenType::TYPE,        // 4
            SemanticTokenType::STRUCT,      // 5
            SemanticTokenType::ENUM,        // 6
            SemanticTokenType::INTERFACE,   // 7
            SemanticTokenType::NAMESPACE,   // 8
            SemanticTokenType::NUMBER,      // 9
            SemanticTokenType::STRING,      // 10
            SemanticTokenType::COMMENT,     // 11
            SemanticTokenType::OPERATOR,    // 12
            SemanticTokenType::PROPERTY,    // 13
            SemanticTokenType::ENUM_MEMBER, // 14 Constant
            SemanticTokenType::DECORATOR,   // 15
        ],
        token_modifiers: vec![
            SemanticTokenModifier::DECLARATION,  // bit 0 MOD_DECLARATION
            SemanticTokenModifier::MODIFICATION, // bit 1 MOD_MUTABLE (closest to mut)
            SemanticTokenModifier::DEFINITION,   // bit 2 MOD_DEFINITION
        ],
    }
}

/// Build LSP semantic tokens from type-aware [`arandu_query::file_highlights()`].
#[must_use]
pub fn semantic_tokens(snap: &AnalysisSnapshot, source: SourceFile) -> SemanticTokens {
    encode_highlights(
        arandu_query::file_highlights(&snap.db, source),
        source.text(&snap.db),
    )
}

/// Range semantic tokens (F2b).
#[must_use]
pub fn semantic_tokens_range(
    snap: &AnalysisSnapshot,
    source: SourceFile,
    range_start: u32,
    range_end: u32,
) -> SemanticTokens {
    let all = arandu_query::file_highlights(&snap.db, source);
    let slice = arandu_query::highlights_in_range(all, range_start, range_end);
    encode_highlights(&slice, source.text(&snap.db))
}

pub(crate) fn encode_highlights(
    highlights: &[arandu_query::HlToken],
    text: &str,
) -> SemanticTokens {
    let index = LineIndex::new(text);
    let mut absolute = Vec::with_capacity(highlights.len());
    for hl in highlights {
        split_highlight_lines(hl, text, &index, &mut absolute);
    }
    absolute.sort_by_key(|token| (token.0.line, token.0.character));

    let mut data = Vec::with_capacity(absolute.len());
    let mut prev_line = 0u32;
    let mut prev_start = 0u32;
    for (start_pos, length, token_type, mods) in absolute {
        let delta_line = start_pos.line.saturating_sub(prev_line);
        let delta_start = if delta_line == 0 {
            start_pos.character.saturating_sub(prev_start)
        } else {
            start_pos.character
        };
        data.push(SemanticToken {
            delta_line,
            delta_start,
            length,
            token_type,
            token_modifiers_bitset: mods,
        });
        prev_line = start_pos.line;
        prev_start = start_pos.character;
    }
    SemanticTokens {
        result_id: None,
        data,
    }
}

fn split_highlight_lines(
    hl: &arandu_query::HlToken,
    text: &str,
    index: &LineIndex,
    out: &mut Vec<(Position, u32, u32, u32)>,
) {
    let mut start = usize::try_from(hl.start)
        .unwrap_or(text.len())
        .min(text.len());
    let end = usize::try_from(hl.end)
        .unwrap_or(text.len())
        .min(text.len());
    while start < end {
        while start < end && matches!(text.as_bytes()[start], b'\r' | b'\n') {
            start += 1;
        }
        if start >= end {
            break;
        }
        let line = index
            .line_starts
            .partition_point(|&line_start| usize::try_from(line_start).is_ok_and(|s| s <= start))
            .saturating_sub(1);
        let next_line = index
            .line_starts
            .get(line + 1)
            .and_then(|&offset| usize::try_from(offset).ok())
            .unwrap_or(text.len());
        let mut content_end = next_line.min(text.len());
        while content_end > start && matches!(text.as_bytes()[content_end - 1], b'\r' | b'\n') {
            content_end -= 1;
        }
        let segment_end = end.min(content_end);
        if segment_end > start {
            let start_u32 = u32::try_from(start).unwrap_or(u32::MAX);
            out.push((
                offset_to_position(index, start_u32),
                utf16_len(&text[start..segment_end]),
                hl.kind.legend_index(),
                u32::from(hl.mods),
            ));
        }
        start = next_line.max(segment_end);
    }
}
