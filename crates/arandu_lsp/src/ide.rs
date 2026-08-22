//! IDE analysis helpers over a frozen [`AnalysisSnapshot`] (P4).
//!
//! Pure queries on typeck/resolve results — no Salsa writes.

use arandu_base::LineIndex;
use arandu_middle::types::ArType;
use arandu_middle::{NodeKey, Symbol, SymbolId, SymbolKind};
use arandu_query::{AnalysisSnapshot, SourceFile};
use arandu_semantics::TypeCheckResult;
use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionResponse, CompletionItem,
    CompletionItemKind, Documentation, Hover, HoverContents, Location, MarkupContent, MarkupKind,
    ParameterInformation, ParameterLabel, Position, SemanticToken, SemanticTokenModifier,
    SemanticTokenType, SemanticTokens, SemanticTokensLegend, SymbolInformation,
    SymbolKind as LspSymbolKind, TextEdit as LspTextEdit, Uri, WorkspaceEdit,
};
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::conv::{offset_to_position, position_to_offset, span_to_range, utf16_len};

/// Snapshot of open docs for multi-file IDE features.
#[derive(Clone)]
pub struct DocSnap {
    pub source: SourceFile,
    pub path: Arc<PathBuf>,
    pub uri: Uri,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticFixData {
    pub title: String,
    pub uri: Uri,
    pub range: lsp_types::Range,
    pub new_text: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticData {
    pub notes: Vec<String>,
    pub hints: Vec<String>,
    pub fixes: Vec<DiagnosticFixData>,
}

/// Type-check the file (composed P1/P2 view).
#[must_use]
pub fn typecheck(
    snap: &AnalysisSnapshot,
    source: SourceFile,
) -> arandu_query::db::HashEq<TypeCheckResult> {
    arandu_query::passes::type_check(&snap.db, source).clone()
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ParameterPresentation {
    label: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SymbolPresentation {
    signature: String,
    documentation: Option<String>,
    parameters: Vec<ParameterPresentation>,
}

fn display_type(tc: &TypeCheckResult, ty: &ArType) -> String {
    ty.display(&tc.symbols, &tc.type_info.type_interner)
}

fn symbol_presentation(
    snap: &AnalysisSnapshot,
    source: SourceFile,
    tc: &TypeCheckResult,
    symbol: &Symbol,
) -> SymbolPresentation {
    let ty = tc.type_info.decl_type(symbol.id);
    let parameter_names = function_parameter_names(snap, source, symbol);
    let (signature, parameters) = match ty.as_ref() {
        Some(ArType::Func(param_types, return_type)) => {
            let labels: Vec<_> = param_types
                .iter()
                .enumerate()
                .map(|(index, type_id)| {
                    let ty = tc.type_info.type_interner.resolve(*type_id);
                    let ty = display_type(tc, &ty);
                    parameter_names
                        .get(index)
                        .filter(|name| !name.is_empty())
                        .map_or_else(|| ty.clone(), |name| format!("{name}: {ty}"))
                })
                .collect();
            let return_ty = tc.type_info.type_interner.resolve(*return_type);
            let return_suffix = if matches!(return_ty, ArType::Void) {
                String::new()
            } else {
                format!(": {}", display_type(tc, &return_ty))
            };
            (
                format!("func {}({}){return_suffix}", symbol.name, labels.join(", ")),
                labels
                    .into_iter()
                    .map(|label| ParameterPresentation { label })
                    .collect(),
            )
        }
        Some(ty) => {
            let prefix = match symbol.kind {
                SymbolKind::Const => "const",
                SymbolKind::Field => "field",
                SymbolKind::Param => "param",
                SymbolKind::Local => "let",
                _ => "type",
            };
            (
                format!("{prefix} {}: {}", symbol.name, display_type(tc, ty)),
                Vec::new(),
            )
        }
        None => (
            format!("{} {}", symbol_kind_name(symbol.kind), symbol.name),
            Vec::new(),
        ),
    };
    SymbolPresentation {
        signature,
        documentation: symbol_documentation(snap, source, symbol),
        parameters,
    }
}

fn markdown_documentation(value: String) -> Documentation {
    Documentation::MarkupContent(MarkupContent {
        kind: MarkupKind::Markdown,
        value,
    })
}

fn symbol_kind_name(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Func | SymbolKind::AssociatedFunc | SymbolKind::ExternFunc => "func",
        SymbolKind::Struct => "struct",
        SymbolKind::Enum => "enum",
        SymbolKind::Interface => "interface",
        SymbolKind::TypeAlias => "type",
        SymbolKind::EnumVariant => "variant",
        SymbolKind::Module => "module",
        SymbolKind::NamespaceMember => "member",
        SymbolKind::ImportValue | SymbolKind::ImportType => "import",
        SymbolKind::TypeParam => "type parameter",
        SymbolKind::Const => "const",
        SymbolKind::Field => "field",
        SymbolKind::Param => "param",
        SymbolKind::Local => "let",
    }
}

fn symbol_documentation(
    snap: &AnalysisSnapshot,
    source: SourceFile,
    symbol: &Symbol,
) -> Option<String> {
    let resolved = arandu_query::passes::resolve(&snap.db, source);
    let docs = resolved
        .docs
        .iter()
        .filter(|(target, _)| target.start <= symbol.span.start && symbol.span.end <= target.end)
        .min_by_key(|(target, _)| target.end.saturating_sub(target.start))?
        .1;
    let text = docs
        .iter()
        .map(|line| line.strip_prefix("///").unwrap_or(line).trim_start())
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}

fn function_parameter_names(
    snap: &AnalysisSnapshot,
    source: SourceFile,
    symbol: &Symbol,
) -> Vec<String> {
    let program = arandu_query::passes::parse(&snap.db, source);
    let Ok(program) = &**program else {
        return Vec::new();
    };
    for &decl_id in &program.decls {
        match program.pool.decl(decl_id) {
            arandu_parser::TopLevelDecl::Func(func) => {
                let (span, name) = match &func.name {
                    arandu_parser::FuncName::Free { span, name }
                    | arandu_parser::FuncName::Method { span, name, .. } => (*span, name),
                };
                if span == symbol.span && name.as_str() == symbol.name.as_str() {
                    return func
                        .params
                        .iter()
                        .map(|param| param.name.to_string())
                        .collect();
                }
            }
            arandu_parser::TopLevelDecl::Extern(extern_decl) => {
                if let Some(member) = extern_decl.members.iter().find(|member| {
                    member.name.as_str() == symbol.name.as_str()
                        && member.span.start <= symbol.span.start
                        && symbol.span.end <= member.span.end
                }) {
                    return member
                        .params
                        .iter()
                        .map(|param| param.name.to_string())
                        .collect();
                }
            }
            _ => {}
        }
    }
    Vec::new()
}

/// Tightest name/ref/definition containing `offset`.
#[must_use]
pub fn symbol_at(tc: &TypeCheckResult, offset: u32) -> Option<SymbolId> {
    let mut best: Option<(u32, SymbolId)> = None;
    let consider = |map: &FxHashMap<NodeKey, SymbolId>, best: &mut Option<(u32, SymbolId)>| {
        for (key, &sym) in map {
            if key.start <= offset && offset < key.end {
                let w = key.end.saturating_sub(key.start);
                if best.is_none_or(|(bw, _)| w < bw) {
                    *best = Some((w, sym));
                }
            }
        }
    };
    consider(&tc.resolved.value_refs, &mut best);
    consider(&tc.resolved.type_refs, &mut best);
    consider(&tc.resolved.definitions, &mut best);
    best.map(|(_, s)| s)
}

/// Tightest resolved expression containing `offset`.
///
/// Namespace members such as `util.answer` are recorded by the resolver in
/// the dense `expr_symbols` table rather than in `value_refs`; navigation must
/// consume that existing semantic identity instead of reparsing the text.
#[must_use]
pub fn expr_symbol_at(
    program: &arandu_parser::Program,
    tc: &TypeCheckResult,
    offset: u32,
) -> Option<SymbolId> {
    let mut best: Option<(u32, SymbolId)> = None;
    for (index, symbol) in tc.resolved.expr_symbols.iter().enumerate() {
        let Some(symbol) = *symbol else {
            continue;
        };
        let Some(span) = program.pool.expr_spans.get(index) else {
            continue;
        };
        if span.start <= offset && offset < span.end {
            let width = span.end.saturating_sub(span.start);
            if best.is_none_or(|(best_width, _)| width < best_width) {
                best = Some((width, symbol));
            }
        }
    }
    best.map(|(_, symbol)| symbol)
}

/// Word prefix before `offset` for completion filtering.
#[must_use]
pub fn prefix_at(text: &str, offset: u32) -> String {
    let off = (offset as usize).min(text.len());
    let bytes = text.as_bytes();
    let mut i = off;
    while i > 0 {
        let c = bytes[i - 1];
        if c.is_ascii_alphanumeric() || c == b'_' {
            i -= 1;
        } else {
            break;
        }
    }
    text[i..off].to_string()
}

#[must_use]
pub fn hover(
    snap: &AnalysisSnapshot,
    source: SourceFile,
    text: &str,
    position: Position,
) -> Option<Hover> {
    let index = LineIndex::new(text);
    let offset = position_to_offset(&index, position, text);
    let tc = typecheck(snap, source);
    let sym = symbol_at(&tc, offset)?;
    let symbol = tc.symbols.try_get(sym)?;
    let presentation = symbol_presentation(snap, source, &tc, symbol);
    let mut md = format!("```arandu\n{}\n```", presentation.signature);
    if let Some(documentation) = presentation.documentation {
        md.push_str("\n\n");
        md.push_str(&documentation);
    }
    let range = span_to_range(&index, symbol.span);
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: md,
        }),
        range: Some(range),
    })
}

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
            "ptr",
            "pointer",
        ],
    ),
    ("std.alloc", &["vec", "allocator_api", "gen_arena"]),
];

/// If the cursor is inside an `import …` path (not after `as`), suggest next segments.
fn import_path_completions(text: &str, offset: u32, prefix: &str) -> Option<Vec<CompletionItem>> {
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

#[must_use]
pub fn references(
    snap: &AnalysisSnapshot,
    source: SourceFile,
    text: &str,
    position: Position,
    uri: &Uri,
) -> Vec<Location> {
    let index = LineIndex::new(text);
    let offset = position_to_offset(&index, position, text);
    let tc = typecheck(snap, source);
    let Some(sym) = symbol_at(&tc, offset) else {
        return Vec::new();
    };
    let mut locs = Vec::new();
    let push_key = |key: &NodeKey, locs: &mut Vec<Location>| {
        let span = arandu_base::Span::new(sym.file_id, key.start, key.end);
        locs.push(Location {
            uri: uri.clone(),
            range: span_to_range(&index, span),
        });
    };
    for (key, &s) in &tc.resolved.definitions {
        if s == sym {
            push_key(key, &mut locs);
        }
    }
    for (key, &s) in &tc.resolved.value_refs {
        if s == sym {
            push_key(key, &mut locs);
        }
    }
    for (key, &s) in &tc.resolved.type_refs {
        if s == sym {
            push_key(key, &mut locs);
        }
    }
    locs.sort_by_key(|l| (l.range.start.line, l.range.start.character));
    locs.dedup_by(|a, b| a.range == b.range);
    locs
}

pub fn prepare_rename(
    snap: &AnalysisSnapshot,
    source: SourceFile,
    text: &str,
    position: Position,
) -> Result<lsp_types::PrepareRenameResponse, arandu_query::RenameError> {
    let index = LineIndex::new(text);
    let offset = position_to_offset(&index, position, text);
    let target = arandu_query::prepare_rename(&snap.db, source, offset)?;
    Ok(lsp_types::PrepareRenameResponse::RangeWithPlaceholder {
        range: span_to_range(&index, target.occurrence),
        placeholder: target.placeholder,
    })
}

// lsp-types 0.97 Uri wraps fluent_uri with interior mutability; protocol map keys are still Uri.
#[allow(clippy::mutable_key_type)]
pub fn rename_edits(
    snap: &AnalysisSnapshot,
    source: SourceFile,
    text: &str,
    position: Position,
    documents: &[DocSnap],
    new_name: &str,
) -> Result<lsp_types::WorkspaceEdit, arandu_query::RenameError> {
    let index = LineIndex::new(text);
    let offset = position_to_offset(&index, position, text);
    let target = arandu_query::validate_rename(&snap.db, source, offset, new_name)?;
    // Uri is not a plain Hash key (fluent_uri interior mutability); key by string.
    let mut changes: HashMap<String, Vec<lsp_types::TextEdit>> = HashMap::new();
    for document in documents {
        let document_text = document.source.text(&snap.db);
        let document_index = LineIndex::new(document_text);
        for span in arandu_query::rename_occurrences(&snap.db, document.source, target.symbol) {
            changes
                .entry(document.uri.as_str().to_string())
                .or_default()
                .push(lsp_types::TextEdit {
                    range: span_to_range(&document_index, span),
                    new_text: new_name.to_string(),
                });
        }
    }
    if changes.is_empty() {
        return Err(arandu_query::RenameError::NotRenameable);
    }
    for edits in changes.values_mut() {
        edits.sort_by_key(|edit| {
            (
                edit.range.start.line,
                edit.range.start.character,
                edit.range.end.line,
                edit.range.end.character,
            )
        });
        edits.dedup_by(|left, right| left.range == right.range);
    }
    let changes: HashMap<Uri, Vec<lsp_types::TextEdit>> = changes
        .into_iter()
        .filter_map(|(s, edits)| crate::uri_util::parse_uri(&s).map(|u| (u, edits)))
        .collect();
    Ok(lsp_types::WorkspaceEdit {
        changes: Some(changes),
        ..lsp_types::WorkspaceEdit::default()
    })
}

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
struct CallContext {
    name: String,
    callee_start: u32,
    active_parameter: u32,
}

fn call_context(
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
        ],
        token_modifiers: vec![
            SemanticTokenModifier::DECLARATION,  // bit 0 MOD_DECLARATION
            SemanticTokenModifier::MODIFICATION, // bit 1 MOD_MUTABLE (closest to mut)
            SemanticTokenModifier::DEFINITION,   // bit 2 MOD_DEFINITION
        ],
    }
}

/// Format entire document (F3a) → LSP text edits (usually one full replace).
#[must_use]
pub fn format_document(text: &str) -> Vec<LspTextEdit> {
    let edits = arandu_fmt::format_edits(text);
    if edits.is_empty() {
        return Vec::new();
    }
    let index = LineIndex::new(text);
    edits
        .into_iter()
        .map(|e| {
            let start = offset_to_position(&index, e.start);
            let end = offset_to_position(&index, e.end);
            LspTextEdit {
                range: lsp_types::Range { start, end },
                new_text: e.new_text,
            }
        })
        .collect()
}

/// Code actions backed by structured compiler replacements carried in
/// `Diagnostic.data`; messages are presentation only and are never parsed.
#[must_use]
#[allow(clippy::mutable_key_type)] // WorkspaceEdit keys are `Uri` in lsp-types 0.97
pub fn code_actions(_uri: &Uri, context: &lsp_types::CodeActionContext) -> CodeActionResponse {
    let mut out = Vec::new();
    for d in &context.diagnostics {
        let Some(data) = d
            .data
            .clone()
            .and_then(|value| serde_json::from_value::<DiagnosticData>(value).ok())
        else {
            continue;
        };
        for fix in data.fixes {
            let mut by_str: HashMap<String, Vec<LspTextEdit>> = HashMap::new();
            by_str.insert(
                fix.uri.as_str().to_string(),
                vec![LspTextEdit {
                    range: fix.range,
                    new_text: fix.new_text,
                }],
            );
            let changes: HashMap<Uri, Vec<LspTextEdit>> = by_str
                .into_iter()
                .filter_map(|(s, edits)| crate::uri_util::parse_uri(&s).map(|u| (u, edits)))
                .collect();
            out.push(CodeActionOrCommand::CodeAction(CodeAction {
                title: fix.title,
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: Some(vec![d.clone()]),
                edit: Some(WorkspaceEdit {
                    changes: Some(changes),
                    ..WorkspaceEdit::default()
                }),
                ..CodeAction::default()
            }));
        }
    }
    out
}

/// Build LSP semantic tokens from type-aware [`arandu_query::file_highlights`].
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

fn encode_highlights(highlights: &[arandu_query::HlToken], text: &str) -> SemanticTokens {
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

#[cfg(test)]
mod tests {
    use super::*;
    use arandu_query::AnalysisHost;

    #[test]
    fn prefix_at_identifier() {
        assert_eq!(prefix_at("let foo_bar = 1", 11), "foo_bar");
        assert_eq!(prefix_at("io.", 3), "");
    }

    #[test]
    fn completions_include_func_and_keyword() {
        let mut host = AnalysisHost::new();
        let file = host.new_file("h.aru".into(), "func main(): int { return 1 }\n".into());
        let snap = host.snapshot();
        let text = file.text(&snap.db);
        let items = completions(
            &snap,
            file,
            text,
            Position {
                line: 0,
                character: text.len() as u32,
            },
        );
        assert!(
            items.iter().any(|i| i.label == "func" || i.label == "main"),
            "expected keyword or main in completions, got {} items",
            items.len()
        );
    }

    #[test]
    fn unicode_position_resolves_symbol_after_astral_character() {
        let text = "/* 😀 */ func soma(value: int): int { return value } // ação\n";
        let mut host = AnalysisHost::new();
        let file = host.new_file("unicode.aru".into(), text.into());
        let snap = host.snapshot();
        let byte_offset = text.find("soma").expect("soma definition") + 1;
        let position = offset_to_position(
            &LineIndex::new(text),
            u32::try_from(byte_offset).expect("fixture offset fits u32"),
        );
        let tc = typecheck(&snap, file);
        assert!(
            symbol_at(
                &tc,
                u32::try_from(byte_offset).expect("fixture offset fits u32")
            )
            .is_some(),
            "resolved maps: definitions={:?}, values={:?}",
            tc.resolved.definitions,
            tc.resolved.value_refs
        );
        assert!(hover(&snap, file, text, position).is_some());
    }

    #[test]
    fn signature_help_is_safe_for_an_empty_document() {
        let mut host = AnalysisHost::new();
        let file = host.new_file("empty.aru".into(), String::new());
        let snap = host.snapshot();
        assert!(signature_help(&snap, file, "", Position::new(0, 0)).is_none());
    }

    #[test]
    fn hover_completion_and_signature_share_user_facing_presentation() {
        let text = concat!(
            "/// Adds two values.\n",
            "/// Keeps integer precision.\n",
            "func add(left: int, right: int): int { return left + right }\n",
            "func main(): int { return add(1, 2) }\n",
        );
        let mut host = AnalysisHost::new();
        let file = host.new_file("presentation.aru".into(), text.into());
        let snap = host.snapshot();
        let signature = "func add(left: int, right: int): int";
        let documentation = "Adds two values.\nKeeps integer precision.";
        let definition = text.find("add").expect("add definition");
        let call = text.rfind("add").expect("add call");

        let hover = hover(
            &snap,
            file,
            text,
            offset_to_position(&LineIndex::new(text), definition as u32),
        )
        .expect("hover for add");
        let HoverContents::Markup(hover) = hover.contents else {
            panic!("hover must use Markdown markup");
        };
        assert!(hover.value.contains(signature));
        assert!(hover.value.contains(documentation));
        assert!(!hover.value.contains("SymbolId"));

        let completion = completions(
            &snap,
            file,
            text,
            offset_to_position(&LineIndex::new(text), (definition + 2) as u32),
        )
        .into_iter()
        .find(|item| item.label == "add")
        .expect("add completion");
        assert_eq!(completion.detail.as_deref(), Some(signature));
        assert!(matches!(
            completion.documentation,
            Some(Documentation::MarkupContent(MarkupContent { value, .. }))
                if value == documentation
        ));

        let second_argument = call + "add(1, ".len();
        let help = signature_help(
            &snap,
            file,
            text,
            offset_to_position(&LineIndex::new(text), second_argument as u32),
        )
        .expect("signature help for add");
        let shown = &help.signatures[0];
        assert_eq!(shown.label, signature);
        assert_eq!(help.active_parameter, Some(1));
        assert_eq!(shown.active_parameter, Some(1));
        assert_eq!(
            shown.parameters.as_ref().expect("parameter labels")[1].label,
            ParameterLabel::Simple("right: int".into())
        );
        assert!(matches!(
            shown.documentation,
            Some(Documentation::MarkupContent(MarkupContent { ref value, .. }))
                if value == documentation
        ));
    }

    #[test]
    fn signature_context_ignores_nested_argument_commas() {
        let text = concat!(
            "func add(left: int, right: int): int { return left + right }\n",
            "func main(): int { return add(add(1, 2), 3) }\n",
        );
        let mut host = AnalysisHost::new();
        let file = host.new_file("nested-signature.aru".into(), text.into());
        let snap = host.snapshot();
        let outer_second = text.rfind(", 3").expect("outer second argument") + 2;
        let context = call_context(&snap, file, outer_second as u32).expect("outer call context");
        assert_eq!(context.name, "add");
        assert_eq!(context.active_parameter, 1);
    }

    #[test]
    fn import_path_completions_std() {
        let mut host = AnalysisHost::new();
        // Cursor after `import std.`
        let text = "import std.\n";
        let file = host.new_file("imp.aru".into(), text.into());
        let snap = host.snapshot();
        let items = completions(
            &snap,
            file,
            text,
            Position {
                line: 0,
                character: "import std.".len() as u32,
            },
        );
        assert!(
            items
                .iter()
                .any(|i| i.label == "core" || i.label == "alloc"),
            "expected std.core/alloc path segments, got {:?}",
            items.iter().map(|i| &i.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn import_path_completions_root() {
        let text = "import \n";
        let items = import_path_completions(text, text.len() as u32 - 1, "")
            .expect("import root completions");
        assert!(
            items.iter().any(|i| i.label == "std"),
            "expected std root, got {:?}",
            items.iter().map(|i| &i.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn semantic_tokens_from_cst_nonempty() {
        let mut host = AnalysisHost::new();
        let file = host.new_file("st.aru".into(), "func main(): int { return 1 }\n".into());
        let snap = host.snapshot();
        let tokens = semantic_tokens(&snap, file);
        assert!(
            !tokens.data.is_empty(),
            "expected semantic tokens from CST keywords/idents"
        );
    }

    #[test]
    fn test_semantic_tokens_exact_deltas() {
        // Resolve via CARGO_MANIFEST_DIR so CI/macOS/other checkouts work
        // (never hard-code a developer machine path).
        let filepath = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../stdlib/std/runtime.aru")
            .canonicalize()
            .expect("resolve stdlib/std/runtime.aru from workspace");
        let content = std::fs::read_to_string(&filepath).expect("read runtime.aru");
        let path_key = filepath.to_string_lossy().into_owned();

        let mut host = AnalysisHost::new();
        let file = host.new_file(path_key, content.clone());
        let snap = host.snapshot();

        let tokens = semantic_tokens(&snap, file);

        // Reconstruct absolute character offsets from LSP deltas and verify
        // they match the highlight spans from the query layer.
        let mut current_line = 0u32;
        let hls = arandu_query::file_highlights(&snap.db, file);
        assert_eq!(tokens.data.len(), hls.len());

        for (i, tok) in tokens.data.iter().enumerate() {
            if tok.delta_line > 0 {
                current_line += tok.delta_line;
            }

            let hl = hls[i];
            assert!(hl.end <= content.len() as u32);
            let substring = &content[hl.start as usize..hl.end as usize];

            // Semantic token lengths use negotiated UTF-16 code units, not bytes.
            assert_eq!(tok.length, utf16_len(substring));

            // Spot-check: `tcp_listen` public decl in stdlib is a FUNCTION token.
            // Line is 0-based (LSP semantic tokens); file line 285 → index 284.
            if substring == "tcp_listen" && current_line == 284 {
                assert_eq!(tok.token_type, 1); // FUNCTION
            }
        }
    }

    #[test]
    fn semantic_tokens_split_multiline_unicode_without_newlines() {
        let text = "/* ação\r\n😀 fim */";
        let highlight = arandu_query::HlToken {
            start: 0,
            end: u32::try_from(text.len()).expect("fixture length fits u32"),
            kind: arandu_query::HlKind::Comment,
            mods: 0,
        };

        let tokens = encode_highlights(&[highlight], text);
        assert_eq!(tokens.data.len(), 2);
        assert_eq!(tokens.data[0].delta_line, 0);
        assert_eq!(tokens.data[0].delta_start, 0);
        assert_eq!(tokens.data[0].length, utf16_len("/* ação"));
        assert_eq!(tokens.data[1].delta_line, 1);
        assert_eq!(tokens.data[1].delta_start, 0);
        assert_eq!(tokens.data[1].length, utf16_len("😀 fim */"));
    }

    #[test]
    fn document_symbols_does_not_panic() {
        let mut host = AnalysisHost::new();
        let file = host.new_file("h.aru".into(), "func main(): int { return 1 }\n".into());
        let snap = host.snapshot();
        let text = file.text(&snap.db);
        let uri = crate::uri_util::parse_uri("file:///h.aru").expect("uri");
        let _syms = document_symbols(&snap, file, text, &uri);
    }
}
