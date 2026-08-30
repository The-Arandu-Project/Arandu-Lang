//! Intelligent completions for keywords, symbols, annotations, import paths, and module members.

use arandu_base::LineIndex;
use arandu_middle::SymbolKind;
use arandu_query::{AnalysisSnapshot, SourceFile};
use lsp_types::{CompletionItem, CompletionItemKind, InsertTextFormat, Position};

use super::presentation::{markdown_documentation, prefix_at, symbol_presentation, typecheck};
use crate::conv::position_to_offset;

#[must_use]
pub fn completions(
    snap: &AnalysisSnapshot,
    source: SourceFile,
    text: &str,
    position: Position,
) -> Vec<CompletionItem> {
    let index = LineIndex::new(text);
    let offset = position_to_offset(&index, position, text);
    let prefix = prefix_at(text, offset);
    let prefix_l = prefix.to_ascii_lowercase();

    if is_annotation_completion(text, offset, &prefix) {
        return annotation_completions(text, offset, &prefix);
    }

    // W4 / T3.6: import path completion (`import std.core.▮` / `import std.▮`).
    if let Some(items) = import_path_completions(text, offset, &prefix) {
        return items;
    }

    // Module member completion after `alias.` when alias is a Module import.
    if let Some(items) = module_member_completions(snap, source, text, offset, &prefix) {
        return items;
    }

    let tc = typecheck(snap, source);

    let mut items = Vec::new();
    // Keywords
    for kw in [
        "func",
        "struct",
        "enum",
        "const",
        "let",
        "mut",
        "set",
        "return",
        "if",
        "else",
        "match",
        "import",
        "module",
        "true",
        "false",
        "nil",
        "err",
        "interface",
        "extern",
    ] {
        if prefix.is_empty() || kw.starts_with(&prefix_l) || kw.starts_with(&prefix) {
            items.push(CompletionItem {
                label: kw.into(),
                kind: Some(CompletionItemKind::KEYWORD),
                ..CompletionItem::default()
            });
        }
    }
    // Symbols from the file's table
    for symbol in tc.symbols.iter() {
        let name = symbol.name.to_string();
        if !prefix.is_empty()
            && !name.to_ascii_lowercase().starts_with(&prefix_l)
            && !name.starts_with(&prefix)
        {
            continue;
        }
        let kind = match symbol.kind {
            SymbolKind::Func | SymbolKind::AssociatedFunc | SymbolKind::ExternFunc => {
                CompletionItemKind::FUNCTION
            }
            SymbolKind::Struct
            | SymbolKind::Enum
            | SymbolKind::Interface
            | SymbolKind::TypeAlias => CompletionItemKind::STRUCT,
            SymbolKind::Const => CompletionItemKind::CONSTANT,
            SymbolKind::Field | SymbolKind::EnumVariant => CompletionItemKind::FIELD,
            SymbolKind::Param | SymbolKind::Local => CompletionItemKind::VARIABLE,
            _ => CompletionItemKind::TEXT,
        };
        let presentation = symbol_presentation(snap, source, &tc, symbol);
        items.push(CompletionItem {
            label: name,
            kind: Some(kind),
            detail: Some(presentation.signature),
            documentation: presentation.documentation.map(markdown_documentation),
            ..CompletionItem::default()
        });
    }
    items.sort_by(|a, b| a.label.cmp(&b.label));
    items.dedup_by(|a, b| a.label == b.label);
    items.truncate(200);
    items
}

fn is_annotation_completion(text: &str, offset: u32, prefix: &str) -> bool {
    usize::try_from(offset)
        .ok()
        .and_then(|offset| offset.checked_sub(prefix.len()))
        .and_then(|name_start| name_start.checked_sub(1))
        .is_some_and(|at| text.as_bytes().get(at) == Some(&b'@'))
}

fn annotation_target_after(
    text: &str,
    offset: u32,
) -> Option<arandu_semantics::attributes::AnnotationTarget> {
    let offset = usize::try_from(offset).ok()?.min(text.len());
    let tail = &text[offset..];
    let keyword = tail
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .filter(|part| !part.is_empty())
        .find(|part| !matches!(*part, "public" | "async"))?;
    match keyword {
        "func" => Some(arandu_semantics::attributes::AnnotationTarget::Function),
        "extern" => Some(arandu_semantics::attributes::AnnotationTarget::ExternBlock),
        "struct" => Some(arandu_semantics::attributes::AnnotationTarget::Struct),
        "enum" => Some(arandu_semantics::attributes::AnnotationTarget::Enum),
        "interface" => Some(arandu_semantics::attributes::AnnotationTarget::Interface),
        "const" => Some(arandu_semantics::attributes::AnnotationTarget::Const),
        "type" => Some(arandu_semantics::attributes::AnnotationTarget::TypeAlias),
        _ => None,
    }
}

fn annotation_completions(text: &str, offset: u32, prefix: &str) -> Vec<CompletionItem> {
    let target = annotation_target_after(text, offset);
    let mut items = arandu_semantics::attributes::BUILTIN_ANNOTATIONS
        .iter()
        .filter(|spec| {
            spec.availability == arandu_semantics::attributes::AnnotationAvailability::Implemented
                && target.is_none_or(|target| spec.targets.contains(&target))
                && (prefix.is_empty()
                    || spec
                        .canonical_name
                        .to_ascii_lowercase()
                        .starts_with(&prefix.to_ascii_lowercase()))
        })
        .map(|spec| {
            let (insert_text, insert_text_format) = match spec.arguments {
                arandu_semantics::attributes::AnnotationArguments::None => {
                    (Some(spec.canonical_name.to_string()), None)
                }
                arandu_semantics::attributes::AnnotationArguments::OneString => (
                    Some(format!("{}(\"${{1:library}}\")", spec.canonical_name)),
                    Some(InsertTextFormat::SNIPPET),
                ),
            };
            CompletionItem {
                label: spec.canonical_name.to_string(),
                kind: Some(CompletionItemKind::PROPERTY),
                detail: Some(format!("@{} — {}", spec.canonical_name, spec.summary)),
                documentation: Some(markdown_documentation(spec.summary.to_string())),
                insert_text,
                insert_text_format,
                ..CompletionItem::default()
            }
        })
        .collect::<Vec<_>>();
    items.sort_by(|left, right| left.label.cmp(&right.label));
    items
}

/// Known top-level stdlib path roots for import completion (T3 tokens).
const IMPORT_ROOTS: &[&str] = &["std", "io", "err"];

/// Segments under `std.` that exist in the tree.
const STD_CHILDREN: &[(&str, &[&str])] = &[
    ("std", &["core", "alloc"]),
    (
        "std.core",
        &[
            "mem",
            "option",
            "result",
            "prelude",
            "intrinsics",
            "future",
            "pointer",
            "str",
            "cmp",
            "char",
            "ascii",
            "num",
            "slice",
        ],
    ),
    (
        "std.alloc",
        &["vec", "allocator_api", "gen_arena", "string"],
    ),
];

/// If the cursor is inside an `import …` path (not after `as`), suggest next segments.
pub(crate) fn import_path_completions(
    text: &str,
    offset: u32,
    prefix: &str,
) -> Option<Vec<CompletionItem>> {
    let off = (offset as usize).min(text.len());
    // Find start of current line.
    let line_start = text[..off].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let line = &text[line_start..off];
    let trimmed = line.trim_start();
    if !trimmed.starts_with("import ") && !trimmed.starts_with("import\t") {
        return None;
    }
    // After `as` → alias name, not path.
    if trimmed.contains(" as ") {
        return None;
    }
    // Path after `import `
    let after_import = trimmed.strip_prefix("import")?.trim_start();
    // Quoted imports are free-form paths; skip special completion.
    if after_import.starts_with('"') {
        return None;
    }
    // Segments before the incomplete last token (prefix).
    let path_so_far = after_import;
    let parent = if path_so_far.is_empty() || path_so_far.ends_with('.') {
        path_so_far.trim_end_matches('.').to_string()
    } else if let Some(dot) = path_so_far.rfind('.') {
        path_so_far[..dot].to_string()
    } else {
        String::new()
    };
    let prefix_l = prefix.to_ascii_lowercase();
    let mut labels: Vec<&str> = Vec::new();
    if parent.is_empty() {
        labels.extend(IMPORT_ROOTS.iter().copied());
    } else {
        for (key, kids) in STD_CHILDREN {
            if *key == parent.as_str() {
                labels.extend(kids.iter().copied());
            }
        }
    }
    if labels.is_empty() {
        return None;
    }
    let mut items: Vec<CompletionItem> = labels
        .into_iter()
        .filter(|lab| {
            prefix.is_empty()
                || lab.starts_with(prefix)
                || lab.to_ascii_lowercase().starts_with(&prefix_l)
        })
        .map(|lab| CompletionItem {
            label: lab.into(),
            kind: Some(CompletionItemKind::MODULE),
            detail: Some("module path".into()),
            ..CompletionItem::default()
        })
        .collect();
    items.sort_by(|a, b| a.label.cmp(&b.label));
    items.dedup_by(|a, b| a.label == b.label);
    if items.is_empty() {
        None
    } else {
        Some(items)
    }
}

/// Completions for `alias.▮` when `alias` is an imported module.
fn module_member_completions(
    snap: &AnalysisSnapshot,
    source: SourceFile,
    text: &str,
    offset: u32,
    prefix: &str,
) -> Option<Vec<CompletionItem>> {
    let off = (offset as usize).min(text.len());
    // Look for `ident.` immediately before prefix.
    let before = &text[..off.saturating_sub(prefix.len())];
    let before = before.trim_end();
    if !before.ends_with('.') {
        return None;
    }
    let without_dot = &before[..before.len() - 1];
    let mut i = without_dot.len();
    let bytes = without_dot.as_bytes();
    while i > 0 {
        let c = bytes[i - 1];
        if c.is_ascii_alphanumeric() || c == b'_' {
            i -= 1;
        } else {
            break;
        }
    }
    let alias = &without_dot[i..];
    if alias.is_empty() {
        return None;
    }

    let tc = typecheck(snap, source);
    // Resolve alias as Module in the file's symbol table.
    let global = tc.symbols.global_scope();
    let module_sym = tc.symbols.lookup_module(global, alias)?;
    let _ = module_sym;
    let members = tc.symbols.module_members.get(alias)?;
    let prefix_l = prefix.to_ascii_lowercase();
    let mut items = Vec::new();
    for (name, &sym_id) in members {
        let name_s = name.as_str();
        // Skip associated method keys `Type.method` at top-level complete after alias.
        if name_s.contains('.') {
            continue;
        }
        if !prefix.is_empty()
            && !name_s.starts_with(prefix)
            && !name_s.to_ascii_lowercase().starts_with(&prefix_l)
        {
            continue;
        }
        let symbol = tc.symbols.try_get(sym_id);
        let kind = symbol
            .map(|symbol| match symbol.kind {
                SymbolKind::Func | SymbolKind::AssociatedFunc | SymbolKind::ExternFunc => {
                    CompletionItemKind::FUNCTION
                }
                SymbolKind::Struct
                | SymbolKind::Enum
                | SymbolKind::Interface
                | SymbolKind::TypeAlias => CompletionItemKind::STRUCT,
                SymbolKind::Const => CompletionItemKind::CONSTANT,
                _ => CompletionItemKind::TEXT,
            })
            .unwrap_or(CompletionItemKind::TEXT);
        let presentation = symbol.and_then(|symbol| {
            let symbol_source = snap.db.source_file_by_id(symbol.id.file_id)?;
            Some(symbol_presentation(snap, symbol_source, &tc, symbol))
        });
        items.push(CompletionItem {
            label: name_s.into(),
            kind: Some(kind),
            detail: Some(presentation.as_ref().map_or_else(
                || format!("from `{alias}`"),
                |presentation| format!("{} — from `{alias}`", presentation.signature),
            )),
            documentation: presentation
                .and_then(|presentation| presentation.documentation)
                .map(markdown_documentation),
            ..CompletionItem::default()
        });
    }
    items.sort_by(|a, b| a.label.cmp(&b.label));
    items.dedup_by(|a, b| a.label == b.label);
    if items.is_empty() {
        None
    } else {
        Some(items)
    }
}
