//! Byte offset ↔ LSP Position using [`arandu_base::LineIndex`].
//!
//! LSP `character` is a UTF-16 code-unit offset, as required by the protocol.

use arandu_base::{LineIndex, Span};
use lsp_types::{Position, Range};

/// 0-based LSP position from exclusive/inclusive byte offset into `text`.
#[must_use]
pub fn offset_to_position(index: &LineIndex, offset: u32) -> Position {
    let (line1, col1) = index.line_col(offset);
    Position {
        line: line1.saturating_sub(1),
        character: col1.saturating_sub(1),
    }
}

/// Byte offset from LSP position (clamped to text length).
#[must_use]
pub fn position_to_offset(index: &LineIndex, pos: Position, text: &str) -> u32 {
    let len = text.len() as u32;
    let line = pos.line as usize;
    let line_start = match index.line_starts.get(line) {
        Some(&s) => s,
        None => return len,
    };
    let line_end = index.line_starts.get(line + 1).copied().unwrap_or(len);
    // Exclude the complete line ending from the character clamp. Treating the
    // `\r` in CRLF as editable text shifts Windows client edits by one byte.
    let mut end = line_end;
    if end > line_start {
        let last = text.as_bytes().get((end - 1) as usize).copied();
        if last == Some(b'\n') {
            end -= 1;
        }
    }
    if end > line_start && text.as_bytes().get((end - 1) as usize) == Some(&b'\r') {
        end -= 1;
    }

    let mut byte_offset = line_start as usize;
    let mut chars = text[line_start as usize..end as usize].chars();
    let mut utf16_count = 0;
    let target_utf16 = pos.character as usize;

    while utf16_count < target_utf16 {
        if let Some(ch) = chars.next() {
            let ch_utf16 = ch.len_utf16();
            if utf16_count + ch_utf16 > target_utf16 {
                break;
            }
            utf16_count += ch_utf16;
            byte_offset += ch.len_utf8();
        } else {
            break;
        }
    }

    (byte_offset as u32).min(end).min(len)
}

/// Convert a compiler [`Span`] (byte offsets) to an LSP [`Range`].
/// End is exclusive in our spans.
#[must_use]
pub fn span_to_range(index: &LineIndex, span: Span) -> Range {
    let start = offset_to_position(index, span.start);
    let end_off = if span.end > span.start {
        span.end
    } else {
        span.start
    };
    let end = offset_to_position(index, end_off);
    Range { start, end }
}

/// Number of UTF-16 code units in a valid UTF-8 source slice.
#[must_use]
pub fn utf16_len(text: &str) -> u32 {
    u32::try_from(text.encode_utf16().count()).unwrap_or(u32::MAX)
}

/// Apply an incremental LSP text edit (`range` + `new_text`) to a document buffer.
#[must_use]
pub fn apply_lsp_range_edit(text: &str, range: Range, new_text: &str) -> String {
    let index = LineIndex::new(text);
    let start = position_to_offset(&index, range.start, text) as usize;
    let end = position_to_offset(&index, range.end, text) as usize;
    let start = start.min(text.len());
    let end = end.min(text.len()).max(start);
    let mut out = String::with_capacity(text.len() - (end - start) + new_text.len());
    out.push_str(&text[..start]);
    out.push_str(new_text);
    out.push_str(&text[end..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use arandu_base::LineIndex;

    #[test]
    fn roundtrip_ascii() {
        let text = "abc\ndef\n";
        let idx = LineIndex::new(text);
        assert_eq!(
            offset_to_position(&idx, 0),
            Position {
                line: 0,
                character: 0
            }
        );
        assert_eq!(
            offset_to_position(&idx, 4),
            Position {
                line: 1,
                character: 0
            }
        );
        assert_eq!(
            position_to_offset(
                &idx,
                Position {
                    line: 1,
                    character: 1
                },
                text
            ),
            5
        );
        let r = span_to_range(&idx, Span::new(0, 4, 7)); // "def"
        assert_eq!(r.start.line, 1);
        assert_eq!(r.start.character, 0);
        assert_eq!(r.end.line, 1);
        assert_eq!(r.end.character, 3);
    }

    #[test]
    fn roundtrip_utf16_with_bmp_and_astral_characters() {
        let text = "aé😀z\n";
        let idx = LineIndex::new(text);

        for offset in [0, 1, 3, 7, 8] {
            let position = offset_to_position(&idx, offset);
            assert_eq!(position_to_offset(&idx, position, text), offset);
        }
        assert_eq!(offset_to_position(&idx, 7).character, 4);
    }

    #[test]
    fn crlf_clamp_never_targets_carriage_return() {
        let text = "abc\r\ndef";
        let idx = LineIndex::new(text);
        let past_end = Position {
            line: 0,
            character: u32::MAX,
        };

        assert_eq!(position_to_offset(&idx, past_end, text), 3);
        let edited = apply_lsp_range_edit(
            text,
            Range {
                start: past_end,
                end: past_end,
            },
            "!",
        );
        assert_eq!(edited, "abc!\r\ndef");
    }

    #[test]
    fn unicode_incremental_edit_uses_utf16_units() {
        let text = "let nome = \"😀ação\";";
        let edited = apply_lsp_range_edit(
            text,
            Range {
                start: Position::new(0, 14),
                end: Position::new(0, 18),
            },
            "ok",
        );
        assert_eq!(edited, "let nome = \"😀ok\";");
    }

    #[test]
    fn incremental_insert_supports_empty_document_and_eof() {
        let origin = Position::new(0, 0);
        let from_empty = apply_lsp_range_edit(
            "",
            Range {
                start: origin,
                end: origin,
            },
            "abc",
        );
        assert_eq!(from_empty, "abc");

        let past_eof = Position::new(u32::MAX, u32::MAX);
        let at_eof = apply_lsp_range_edit(
            &from_empty,
            Range {
                start: past_eof,
                end: past_eof,
            },
            "!",
        );
        assert_eq!(at_eof, "abc!");
    }
}
