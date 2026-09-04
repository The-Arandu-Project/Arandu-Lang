use std::sync::Arc;

/// A map of line start byte offsets in a source file.
/// Used to perform binary search lookups of line and column numbers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineIndex {
    pub line_starts: Vec<u32>,
    pub source: Arc<str>,
}

impl Default for LineIndex {
    fn default() -> Self {
        Self {
            line_starts: vec![0],
            source: Arc::from(""),
        }
    }
}

impl LineIndex {
    /// Constructs a `LineIndex` from the given source file contents.
    #[must_use]
    pub fn new(source: &str) -> Self {
        Self::from_arc(Arc::from(source))
    }

    /// Constructs a `LineIndex` reusing an existing `Arc<str>` without re-allocating source text.
    #[must_use]
    pub fn from_arc(source: Arc<str>) -> Self {
        let mut line_starts = vec![0];
        for (offset, byte) in source.bytes().enumerate() {
            if byte == b'\n' {
                line_starts.push((offset + 1) as u32);
            }
        }
        Self {
            line_starts,
            source,
        }
    }

    /// Converts a byte offset into 1-based (line, column) numbers.
    #[must_use]
    pub fn line_col(&self, offset: u32) -> (u32, u32) {
        let line = self
            .line_starts
            .partition_point(|&s| s <= offset)
            .saturating_sub(1);
        let line_start_byte = self.line_starts[line] as usize;
        let original_offset = offset as usize;
        let capped_offset = original_offset.min(self.source.len());
        let mut offset_byte = capped_offset;
        while offset_byte > line_start_byte && !self.source.is_char_boundary(offset_byte) {
            offset_byte -= 1;
        }

        let mut col = 0;
        if offset_byte > line_start_byte {
            let line_slice = &self.source[line_start_byte..offset_byte];
            for c in line_slice.chars() {
                col += c.len_utf16();
            }
        }
        if original_offset > self.source.len() {
            col += original_offset - self.source.len();
        }

        ((line + 1) as u32, (col + 1) as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_line_index_lookups() {
        let source = "abc\ndef\nghi";
        let index = LineIndex::new(source);

        // First line: "abc\n" (offsets 0..=3)
        assert_eq!(index.line_col(0), (1, 1));
        assert_eq!(index.line_col(1), (1, 2));
        assert_eq!(index.line_col(2), (1, 3));
        assert_eq!(index.line_col(3), (1, 4));

        // Second line: "def\n" (offsets 4..=7)
        assert_eq!(index.line_col(4), (2, 1));
        assert_eq!(index.line_col(7), (2, 4));

        // Third line: "ghi" (offsets 8..=10)
        assert_eq!(index.line_col(8), (3, 1));
        assert_eq!(index.line_col(10), (3, 3));

        // Out of bounds: should saturate/cap nicely
        assert_eq!(index.line_col(20), (3, 13));
    }

    #[test]
    fn test_line_index_unicode() {
        // "é" takes 2 bytes, Emoji "😀" takes 4 bytes (2 UTF-16 code units)
        let source = "é\n😀\n";
        let index = LineIndex::new(source);

        // "é\n": "é" starts at byte offset 0, takes 2 bytes. "\n" at 2.
        assert_eq!(index.line_col(0), (1, 1));
        assert_eq!(index.line_col(2), (1, 2));

        // "😀\n": "😀" starts at byte offset 3, takes 4 bytes. "\n" at 7.
        assert_eq!(index.line_col(3), (2, 1));
        // Byte offset 7 is "\n". Its column in UTF-16 from line start is 2 (since Emoji takes 2 UTF-16 code units).
        assert_eq!(index.line_col(7), (2, 3));
    }

    #[test]
    fn offset_inside_utf8_codepoint_clamps_without_panicking() {
        let index = LineIndex::new("é😀z");

        assert_eq!(index.line_col(1), (1, 1));
        assert_eq!(index.line_col(3), (1, 2));
        assert_eq!(index.line_col(4), (1, 2));
        assert_eq!(index.line_col(5), (1, 2));
        assert_eq!(index.line_col(6), (1, 4));
        assert_eq!(index.line_col(7), (1, 5));
    }
}
