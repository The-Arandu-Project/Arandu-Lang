use super::Lexer;
use super::ident::is_bidi_control;
use crate::{LexError, LexErrorCode, TokenKind};

impl<'a> Lexer<'a> {
    pub(super) fn lex_line_doc_comment(&mut self) -> Result<(), LexError> {
        let start = self.mark();
        while !self.is_at_end() && !matches!(self.peek(), Some('\n' | '\r')) {
            if let Some(ch) = self.peek()
                && is_bidi_control(ch)
            {
                let err_start = self.mark();
                self.bump();
                return Err(self.error_from(
                    err_start,
                    LexErrorCode::BidiTrojanSource,
                    "unescaped bidirectional Unicode control character (Trojan Source, CWE-1307)",
                ));
            }
            self.bump();
        }
        self.push_token(TokenKind::DocComment, self.span_from(start), false);
        Ok(())
    }

    pub(super) fn lex_block_doc_comment(&mut self) -> Result<(), LexError> {
        let start = self.mark();
        self.bump_ascii(3); // /**
        let mut depth = 1;
        while !self.is_at_end() && depth > 0 {
            if let Some(ch) = self.peek()
                && is_bidi_control(ch)
            {
                let err_start = self.mark();
                self.bump();
                return Err(self.error_from(
                    err_start,
                    LexErrorCode::BidiTrojanSource,
                    "unescaped bidirectional Unicode control character (Trojan Source, CWE-1307)",
                ));
            }
            if self.starts_with("/*") {
                self.bump_ascii(2);
                depth += 1;
            } else if self.starts_with("*/") {
                self.bump_ascii(2);
                depth -= 1;
            } else {
                self.bump();
            }
        }
        if depth > 0 {
            return Err(self.error_from(
                start,
                LexErrorCode::UnterminatedBlockComment,
                "unterminated doc block comment",
            ));
        }
        self.push_token(TokenKind::DocComment, self.span_from(start), false);
        Ok(())
    }

    pub(super) fn skip_line_comment(&mut self) -> Result<(), LexError> {
        while !self.is_at_end() && !matches!(self.peek(), Some('\n' | '\r')) {
            if let Some(ch) = self.peek()
                && is_bidi_control(ch)
            {
                let err_start = self.mark();
                self.bump();
                return Err(self.error_from(
                    err_start,
                    LexErrorCode::BidiTrojanSource,
                    "unescaped bidirectional Unicode control character (Trojan Source, CWE-1307)",
                ));
            }
            self.bump();
        }
        Ok(())
    }

    pub(super) fn skip_block_comment(&mut self) -> Result<(), LexError> {
        let start = self.mark();
        self.bump_ascii(2); // /*
        let mut depth = 1;
        while !self.is_at_end() && depth > 0 {
            if let Some(ch) = self.peek()
                && is_bidi_control(ch)
            {
                let err_start = self.mark();
                self.bump();
                return Err(self.error_from(
                    err_start,
                    LexErrorCode::BidiTrojanSource,
                    "unescaped bidirectional Unicode control character (Trojan Source, CWE-1307)",
                ));
            }
            if self.starts_with("/*") {
                self.bump_ascii(2);
                depth += 1;
            } else if self.starts_with("*/") {
                self.bump_ascii(2);
                depth -= 1;
            } else {
                self.bump();
            }
        }
        if depth > 0 {
            return Err(self.error_from(
                start,
                LexErrorCode::UnterminatedBlockComment,
                "unterminated block comment",
            ));
        }
        Ok(())
    }
}
