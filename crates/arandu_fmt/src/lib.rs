//! Arandu source formatter — pretty-print from CST structure when possible.
//!
//! Pure (no Salsa/LSP). On unparseable input, falls back to whitespace hygiene.

use arandu_parser::{lower_syntax_to_program, parse_syntax, SyntaxTree};

/// UTF-8 byte range edit (for LSP full-document replace helpers).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEdit {
    pub start: u32,
    pub end: u32,
    pub new_text: String,
}

/// Format source with real structure when the CST parses cleanly.
///
/// Rules (v0.2):
/// - Prefer CST top-level items: blank line between items, reindent inside `{}`
/// - Indent 4 spaces per brace depth
/// - Normalize `\n`, strip trailing whitespace, single trailing newline
/// - Fallback: whitespace hygiene only
#[must_use]
pub fn format_source(source: &str) -> String {
    let tree = parse_syntax(source);
    if tree.lex_diagnostics().is_empty() {
        if let Ok(_prog) = lower_syntax_to_program(&tree, 0) {
            let pretty = format_from_tree(&tree);
            if parses_clean(&pretty) {
                return pretty;
            }
        }
    }
    normalize_whitespace(source)
}

/// Minimal, non-overlapping edits that transform `source` into canonical form.
///
/// When line structure is unchanged, each changed line gets its own smallest
/// UTF-8-boundary edit. If formatting inserts/removes lines, one smallest
/// contiguous hunk is returned. This keeps editor cursors outside the changed
/// region stable instead of replacing the entire document.
#[must_use]
pub fn format_edits(source: &str) -> Vec<TextEdit> {
    let formatted = format_source(source);
    if formatted == source {
        return Vec::new();
    }
    let source_lines = source.split_inclusive('\n').collect::<Vec<_>>();
    let formatted_lines = formatted.split_inclusive('\n').collect::<Vec<_>>();
    if source_lines.len() != formatted_lines.len() {
        return minimal_edit(source, &formatted, 0).into_iter().collect();
    }

    let mut edits = Vec::new();
    let mut base = 0usize;
    for (old_line, new_line) in source_lines.into_iter().zip(formatted_lines) {
        if let Some(edit) = minimal_edit(old_line, new_line, base) {
            edits.push(edit);
        }
        let Some(next) = base.checked_add(old_line.len()) else {
            return Vec::new();
        };
        base = next;
    }
    edits
}

fn minimal_edit(old: &str, new: &str, base: usize) -> Option<TextEdit> {
    if old == new {
        return None;
    }
    let mut prefix = 0usize;
    for (old_char, new_char) in old.chars().zip(new.chars()) {
        if old_char != new_char {
            break;
        }
        prefix += old_char.len_utf8();
    }

    let old_tail = &old[prefix..];
    let new_tail = &new[prefix..];
    let mut suffix = 0usize;
    for (old_char, new_char) in old_tail.chars().rev().zip(new_tail.chars().rev()) {
        if old_char != new_char {
            break;
        }
        suffix += old_char.len_utf8();
    }
    let old_end = old.len().saturating_sub(suffix);
    let new_end = new.len().saturating_sub(suffix);
    let start = u32::try_from(base.checked_add(prefix)?).ok()?;
    let end = u32::try_from(base.checked_add(old_end)?).ok()?;
    Some(TextEdit {
        start,
        end,
        new_text: new[prefix..new_end].to_string(),
    })
}

fn parses_clean(source: &str) -> bool {
    let tree = parse_syntax(source);
    lower_syntax_to_program(&tree, 0).is_ok() && tree.lex_diagnostics().is_empty()
}

/// Pretty-print from green top-level items.
fn format_from_tree(tree: &SyntaxTree) -> String {
    let source = tree.text();
    let items = tree.items();
    if items.is_empty() {
        return normalize_whitespace(source);
    }
    let mut out = String::with_capacity(source.len() + 32);
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let r = item.text_range();
        let s = u32::from(r.start()) as usize;
        let e = (u32::from(r.end()) as usize).min(source.len()).max(s);
        let item_src = &source[s..e];
        out.push_str(&reindent_item(item_src));
        if !out.ends_with('\n') {
            out.push('\n');
        }
    }
    // Preserve leading module-less files; strip excess leading blanks.
    if out.starts_with('\n') && out.len() > 1 {
        let non_nl = out
            .find(|c| c != '\n')
            .unwrap_or(out.len().saturating_sub(1));
        out.drain(..non_nl);
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Reindent a top-level item: 4 spaces per `{}` depth; trim line ends.
fn reindent_item(item_src: &str) -> String {
    let unified = item_src.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = unified
        .strip_suffix('\n')
        .unwrap_or(&unified)
        .split('\n')
        .collect();
    let mut out = String::with_capacity(item_src.len() + 16);
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut in_char = false;
    let mut escaped = false;

    for (li, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if li + 1 < lines.len() {
                out.push('\n');
            }
            continue;
        }

        // Decrease depth for lines that start with `}` before indenting.
        let mut leading_closes = 0;
        if !in_string && !in_char {
            for c in trimmed.chars() {
                if c == '}' {
                    leading_closes += 1;
                } else if c.is_whitespace() {
                    continue;
                } else {
                    break;
                }
            }
        }

        let indent_depth = (depth - leading_closes).max(0) as usize;
        for _ in 0..indent_depth {
            out.push_str("    ");
        }
        out.push_str(trimmed);
        out.push('\n');

        // Update depth from full line braces, ignoring strings, characters, and comments.
        let mut chars = trimmed.chars().peekable();
        while let Some(ch) = chars.next() {
            if escaped {
                escaped = false;
                continue;
            }
            if !in_string && !in_char && ch == '/' && chars.peek() == Some(&'/') {
                break;
            }
            match ch {
                '\\' => {
                    if in_string || in_char {
                        escaped = true;
                    }
                }
                '"' => {
                    if !in_char {
                        in_string = !in_string;
                    }
                }
                '\'' => {
                    if !in_string {
                        in_char = !in_char;
                    }
                }
                '{' if !in_string && !in_char => {
                    depth += 1;
                }
                '}' if !in_string && !in_char => {
                    depth = depth.saturating_sub(1);
                }
                _ => {}
            }
        }
    }
    // Trim final newline for join logic of caller (caller adds one).
    if out.ends_with('\n') {
        out.pop();
    }
    out
}

fn normalize_whitespace(source: &str) -> String {
    let unified = source.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = if unified.is_empty() {
        Vec::new()
    } else {
        unified
            .strip_suffix('\n')
            .unwrap_or(&unified)
            .split('\n')
            .collect()
    };
    let mut out = String::with_capacity(unified.len() + 1);
    let mut blank_run = 0u32;
    for line in lines {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            blank_run += 1;
            if blank_run <= 1 {
                out.push('\n');
            }
        } else {
            blank_run = 0;
            out.push_str(trimmed);
            out.push('\n');
        }
    }
    if out.is_empty() {
        out.push('\n');
    }
    if out.starts_with('\n') && out.len() > 1 {
        let non_nl = out
            .find(|c| c != '\n')
            .unwrap_or(out.len().saturating_sub(1));
        out.drain(..non_nl);
    }
    out
}

/// Quick-fix style actions from diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeAction {
    pub title: &'static str,
    pub edits: Vec<TextEdit>,
}

/// Constructs a structured single-character insertion code action at `offset`.
#[must_use]
pub fn actions_for_missing_char(insert_at: u32, ch: char) -> Option<CodeAction> {
    let (title, new_text) = match ch {
        ';' => ("Insert `;`", ";"),
        '{' => ("Insert `{`", " {"),
        '}' => ("Insert `}`", "}"),
        ')' => ("Insert `)`", ")"),
        ']' => ("Insert `]`", "]"),
        _ => return None,
    };
    Some(CodeAction {
        title,
        edits: vec![TextEdit {
            start: insert_at,
            end: insert_at,
            new_text: new_text.into(),
        }],
    })
}

/// Collect applicable quick-fixes for a single diagnostic.
///
/// Prefer [`actions_for_missing_char`] or structured replacements from compiler diagnostics
/// when AST/CST error details are available.
#[must_use]
pub fn actions_for_diagnostic(start: u32, end: u32, message: &str) -> Vec<CodeAction> {
    let mut out = Vec::new();
    let msg = message.to_ascii_lowercase();
    let insert_at = start.min(end);

    if msg.contains("semicolon") || msg.contains("statement terminator") || msg.contains("semi") {
        if let Some(act) = actions_for_missing_char(insert_at, ';') {
            out.push(act);
        }
    }
    if msg.contains("expected '{'")
        || msg.contains("expected lbrace")
        || msg.contains("expected \"{\"")
    {
        if let Some(act) = actions_for_missing_char(insert_at, '{') {
            out.push(act);
        }
    }
    if msg.contains("expected '}'")
        || msg.contains("expected rbrace")
        || msg.contains("expected \"}\"")
    {
        if let Some(act) = actions_for_missing_char(insert_at, '}') {
            out.push(act);
        }
    }
    if msg.contains("expected ')'") || msg.contains("expected rparen") {
        if let Some(act) = actions_for_missing_char(insert_at, ')') {
            out.push(act);
        }
    }
    out
}

#[deprecated(since = "0.1.0", note = "use actions_for_diagnostic instead")]
#[must_use]
pub fn actions_for_expected_semicolon(start: u32, end: u32, message: &str) -> Option<CodeAction> {
    actions_for_diagnostic(start, end, message)
        .into_iter()
        .find(|a| a.title.contains(';'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pretty_indents_func_body() {
        let src = "func main(): int {\nreturn 1\n}\n";
        let out = format_source(src);
        assert!(
            out.contains("    return 1"),
            "expected indented return, got:\n{out}"
        );
        assert!(parses_clean(&out), "formatted must parse");
    }

    #[test]
    fn blank_line_between_items() {
        let src = "func a(): int { return 1 }\nfunc b(): int { return 2 }\n";
        let out = format_source(src);
        assert!(out.contains("func a") && out.contains("func b"));
        // At least one blank line between the two items.
        let a = out.find("func a").unwrap();
        let b = out.find("func b").unwrap();
        assert!(b > a);
        assert!(
            out[a..b].contains("\n\n"),
            "expected blank line between funcs:\n{out}"
        );
    }

    #[test]
    fn format_edits_empty_when_stable() {
        let src = "func main(): int {\n    return 1\n}\n";
        assert!(format_edits(src).is_empty());
    }

    #[test]
    fn formatting_is_idempotent_with_unicode_crlf_and_braces_in_strings() {
        let src = "func main(): str {\r\nlet greeting = \"olá 😀 {literal}\"   \r\nreturn greeting\r\n}\r\n";
        let once = format_source(src);
        let twice = format_source(&once);

        assert_eq!(once, twice);
        assert!(once.contains("olá 😀 {literal}"));
        assert!(!once.contains('\r'));
        assert!(parses_clean(&once));
    }

    #[test]
    fn fallback_formatting_is_idempotent_for_invalid_input() {
        let src = "func broken( {\r\n  let x = \"😀\"   \r\n\r\n\r\n";
        let once = format_source(src);
        assert_eq!(once, format_source(&once));
        assert!(!once.contains('\r'));
    }

    #[test]
    fn formatting_returns_line_local_edits_instead_of_full_document_replace() {
        let src = "func main(): int {\nreturn 1   \n}\n";
        let edits = format_edits(src);
        assert_eq!(edits.len(), 1);
        let return_start = u32::try_from(src.find("return").unwrap()).unwrap();
        assert!(edits[0].start <= return_start);
        assert!(edits[0].start > 0, "must preserve the unchanged header");
        assert!(edits[0].end < u32::try_from(src.len()).unwrap());
        assert_eq!(apply_edits(src, &edits), format_source(src));
    }

    #[test]
    fn formatting_edits_are_utf8_safe_and_idempotent_after_application() {
        let src = "func main(): str {\nlet greeting = \"olá 😀\"\nreturn greeting\n}\n";
        let edits = format_edits(src);
        assert!(edits.len() >= 2, "distant indentation should remain local");
        for edit in &edits {
            assert!(src.is_char_boundary(edit.start as usize));
            assert!(src.is_char_boundary(edit.end as usize));
        }
        let applied = apply_edits(src, &edits);
        assert_eq!(applied, format_source(src));
        assert!(format_edits(&applied).is_empty());
    }

    #[test]
    fn formatting_preserves_annotation_spelling() {
        let canonical = "@NoFallback\nfunc main() {}\n";
        let legacy = "@no_fallback\nfunc main() {}\n";
        assert!(format_source(canonical).contains("@NoFallback"));
        assert!(format_source(legacy).contains("@no_fallback"));
    }

    fn apply_edits(source: &str, edits: &[TextEdit]) -> String {
        let mut out = source.to_string();
        for edit in edits.iter().rev() {
            out.replace_range(edit.start as usize..edit.end as usize, &edit.new_text);
        }
        out
    }

    #[test]
    fn multiple_actions() {
        let a = actions_for_diagnostic(0, 0, "expected '}'");
        assert!(a.iter().any(|x| x.title.contains('}')));
    }

    #[test]
    #[allow(deprecated)]
    fn semicolon_action() {
        let a = actions_for_expected_semicolon(10, 11, "expected SEMICOLON").unwrap();
        assert_eq!(a.edits[0].new_text, ";");
    }
}
