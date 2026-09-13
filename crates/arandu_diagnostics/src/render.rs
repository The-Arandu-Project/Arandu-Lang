//! Diagnostic formatting and rendering (CLI output and miette integration).

use crate::{Diagnostic, Severity};
use arandu_base::source_registry::SourceRegistry;
use std::fmt;

impl Diagnostic {
    #[must_use]
    pub fn format_for_cli(&self, registry: &SourceRegistry) -> String {
        use std::fmt::Write;
        let mut out = String::new();

        let (filepath, start_line, start_col) =
            if let Some(file) = registry.get_file(self.span.file_id) {
                let (line, col) = file.line_index.line_col(self.span.start);
                (&file.path[..], line, col)
            } else {
                ("", 1, 1)
            };

        let file_prefix = if filepath.is_empty() {
            String::new()
        } else {
            format!("{filepath}:")
        };

        // Format code prefix based on ICE vs regular error
        let code_prefix = self.code.as_str();

        let _ = writeln!(out, "{}: {}", code_prefix, self.message);
        let _ = writeln!(out, "  --> {}{}:{}", file_prefix, start_line, start_col);

        for label in &self.labels {
            let (l_start_line, l_start_col, l_end_line, l_end_col) =
                if let Some(file) = registry.get_file(label.span.file_id) {
                    let (s_line, s_col) = file.line_index.line_col(label.span.start);
                    let (e_line, e_col) = file.line_index.line_col(label.span.end);
                    (s_line, s_col, e_line, e_col)
                } else {
                    (1, 1, 1, 1)
                };
            let _ = writeln!(
                out,
                "  label: {}:{}-{}:{} {}",
                l_start_line, l_start_col, l_end_line, l_end_col, label.message
            );
        }
        for note in &self.notes {
            let _ = writeln!(out, "  note: {note}");
        }
        for hint in &self.hints {
            let _ = writeln!(out, "  hint: {}", hint.message);
            if let Some(ref rep) = hint.replacement {
                let (r_start_line, r_start_col, r_end_line, r_end_col) =
                    if let Some(file) = registry.get_file(rep.span.file_id) {
                        let (s_line, s_col) = file.line_index.line_col(rep.span.start);
                        let (e_line, e_col) = file.line_index.line_col(rep.span.end);
                        (s_line, s_col, e_line, e_col)
                    } else {
                        (1, 1, 1, 1)
                    };
                let _ = writeln!(
                    out,
                    "  replacement: at {}:{}-{}:{} with {:?}",
                    r_start_line, r_start_col, r_end_line, r_end_col, rep.new_text
                );
            }
        }

        // Remove trailing newline
        if out.ends_with('\n') {
            out.pop();
        }
        if out.ends_with('\r') {
            out.pop();
        }

        out
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Diagnostic {}

impl miette::Diagnostic for Diagnostic {
    fn code<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        Some(Box::new(self.code.as_str()))
    }

    fn severity(&self) -> Option<miette::Severity> {
        match self.severity {
            Severity::Error => Some(miette::Severity::Error),
            Severity::Warning => Some(miette::Severity::Warning),
            Severity::Note => Some(miette::Severity::Advice),
            Severity::Hint => Some(miette::Severity::Advice),
        }
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = miette::LabeledSpan> + '_>> {
        let primary = std::iter::once(miette::LabeledSpan::new_primary_with_span(
            None,
            miette::SourceSpan::new(
                (self.span.start as usize).into(),
                (self.span.end.saturating_sub(self.span.start)) as usize,
            ),
        ));
        let secondary = self.labels.iter().map(|label| {
            miette::LabeledSpan::new_with_span(
                Some(label.message.clone()),
                miette::SourceSpan::new(
                    (label.span.start as usize).into(),
                    (label.span.end.saturating_sub(label.span.start)) as usize,
                ),
            )
        });
        Some(Box::new(primary.chain(secondary)))
    }

    fn help<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        if self.notes.is_empty() && self.hints.is_empty() {
            None
        } else {
            let mut parts = Vec::new();
            for note in &self.notes {
                parts.push(format!("note: {note}"));
            }
            for hint in &self.hints {
                parts.push(hint.message.clone());
            }
            Some(Box::new(parts.join("\n")))
        }
    }

    fn url<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        if self.is_ice() {
            None
        } else {
            Some(Box::new(format!(
                "https://arandu-lang.dev/docs/errors/{}",
                self.code.as_str()
            )))
        }
    }
}
