//! Pure semantic validation for editor rename operations.
//!
//! Protocol presentation and preview remain client/LSP responsibilities. This
//! module only answers whether a source occurrence denotes a renameable symbol
//! and whether a proposed spelling preserves Arandu's lexical/scope contracts.

use crate::{passes, ArandCompilerDb, SourceFile};
use arandu_base::Span;
use arandu_lexer::{identifier_kind, TokenKind};
use arandu_middle::{NodeKey, SymbolId, SymbolKind};
use rustc_hash::FxHashMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenameTarget {
    pub symbol: SymbolId,
    pub occurrence: Span,
    pub placeholder: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenameError {
    NotRenameable,
    InvalidIdentifier,
    WrongIdentifierKind,
    Conflict { existing: String, span: Span },
}

impl RenameError {
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::NotRenameable => "no renameable Arandu symbol at this position".into(),
            Self::InvalidIdentifier => {
                "the new name must be one non-reserved Arandu identifier".into()
            }
            Self::WrongIdentifierKind => {
                "the new name must preserve the symbol's value/type capitalization".into()
            }
            Self::Conflict { existing, .. } => {
                format!("renaming would conflict with or shadow the existing symbol `{existing}`")
            }
        }
    }
}

pub fn prepare_rename(
    db: &dyn ArandCompilerDb,
    source: SourceFile,
    offset: u32,
) -> Result<RenameTarget, RenameError> {
    let checked = passes::type_check(db, source);
    let symbol = occurrence_at(&checked.resolved.definitions, offset)
        .or_else(|| occurrence_at(&checked.resolved.value_refs, offset))
        .or_else(|| occurrence_at(&checked.resolved.type_refs, offset))
        .map(|(_, symbol)| symbol)
        .or_else(|| expression_symbol_at(db, source, checked, offset))
        .ok_or(RenameError::NotRenameable)?;
    let declaration = checked
        .symbols
        .try_get(symbol)
        .ok_or(RenameError::NotRenameable)?;
    if matches!(
        declaration.kind,
        SymbolKind::Module | SymbolKind::ImportValue | SymbolKind::ImportType
    ) {
        return Err(RenameError::NotRenameable);
    }
    let tree = passes::syntax_tree(db, source);
    let token = tree
        .tokens()
        .iter()
        .find(|token| token.start <= offset && offset < token.start.saturating_add(token.len))
        .filter(|token| {
            matches!(token.kind, TokenKind::IdentValue | TokenKind::IdentType)
                && token.lexeme(tree.text())
                    == declaration
                        .name
                        .split('.')
                        .next_back()
                        .unwrap_or(&declaration.name)
        })
        .ok_or(RenameError::NotRenameable)?;
    Ok(RenameTarget {
        symbol,
        occurrence: token.span(*source.file_id(db)),
        placeholder: declaration
            .name
            .split('.')
            .next_back()
            .unwrap_or(&declaration.name)
            .to_string(),
    })
}

pub fn validate_rename(
    db: &dyn ArandCompilerDb,
    source: SourceFile,
    offset: u32,
    new_name: &str,
) -> Result<RenameTarget, RenameError> {
    let target = prepare_rename(db, source, offset)?;
    let new_kind = identifier_kind(new_name).ok_or(RenameError::InvalidIdentifier)?;
    let old_kind = identifier_kind(&target.placeholder).ok_or(RenameError::NotRenameable)?;
    if new_kind != old_kind {
        return Err(RenameError::WrongIdentifierKind);
    }

    let checked = passes::type_check(db, source);
    let symbol = checked
        .symbols
        .try_get(target.symbol)
        .ok_or(RenameError::NotRenameable)?;
    for existing in checked.symbols.iter() {
        if existing.id == target.symbol
            || existing.id.file_id != symbol.id.file_id
            || existing.name.as_str() != new_name
            || !checked
                .symbols
                .scopes_are_related(symbol.scope, existing.scope)
        {
            continue;
        }
        return Err(RenameError::Conflict {
            existing: existing.name.to_string(),
            span: existing.span,
        });
    }
    Ok(target)
}

/// Exact identifier-token spans bound to `symbol`, in deterministic source order.
#[must_use]
pub fn rename_occurrences(
    db: &dyn ArandCompilerDb,
    source: SourceFile,
    symbol: SymbolId,
) -> Vec<Span> {
    let checked = passes::type_check(db, source);
    let Some(declaration) = checked.symbols.try_get(symbol) else {
        return Vec::new();
    };
    let spelling = declaration
        .name
        .split('.')
        .next_back()
        .unwrap_or(&declaration.name);
    let tree = passes::syntax_tree(db, source);
    let mut spans = tree
        .tokens()
        .iter()
        .filter(|token| {
            matches!(token.kind, TokenKind::IdentValue | TokenKind::IdentType)
                && token.lexeme(tree.text()) == spelling
                && semantic_symbol_at(db, source, checked, token.start) == Some(symbol)
        })
        .map(|token| token.span(*source.file_id(db)))
        .collect::<Vec<_>>();
    spans.sort_by_key(|span| (span.start, span.end));
    spans.dedup();
    spans
}

fn semantic_symbol_at(
    db: &dyn ArandCompilerDb,
    source: SourceFile,
    checked: &arandu_semantics::TypeCheckResult,
    offset: u32,
) -> Option<SymbolId> {
    occurrence_at(&checked.resolved.definitions, offset)
        .or_else(|| occurrence_at(&checked.resolved.value_refs, offset))
        .or_else(|| occurrence_at(&checked.resolved.type_refs, offset))
        .map(|(_, symbol)| symbol)
        .or_else(|| expression_symbol_at(db, source, checked, offset))
}

fn occurrence_at(
    occurrences: &FxHashMap<NodeKey, SymbolId>,
    offset: u32,
) -> Option<(NodeKey, SymbolId)> {
    occurrences
        .iter()
        .filter(|(key, _)| key.start <= offset && offset < key.end)
        .min_by_key(|(key, _)| key.end.saturating_sub(key.start))
        .map(|(key, symbol)| (*key, *symbol))
}

fn expression_symbol_at(
    db: &dyn ArandCompilerDb,
    source: SourceFile,
    checked: &arandu_semantics::TypeCheckResult,
    offset: u32,
) -> Option<SymbolId> {
    let program = passes::parse(db, source);
    let Ok(program) = &**program else {
        return None;
    };
    checked
        .resolved
        .expr_symbols
        .iter()
        .enumerate()
        .filter_map(|(index, symbol)| {
            let symbol = (*symbol)?;
            let span = program.pool.expr_spans.get(index)?;
            (span.start <= offset && offset < span.end)
                .then_some((span.end.saturating_sub(span.start), symbol))
        })
        .min_by_key(|(width, _)| *width)
        .map(|(_, symbol)| symbol)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DatabaseImpl;

    fn source(db: &mut DatabaseImpl, text: &str) -> SourceFile {
        db.new_file("rename.aru".into(), text.into())
    }

    fn offset(text: &str, needle: &str) -> u32 {
        u32::try_from(text.find(needle).expect("fixture needle")).expect("fixture offset")
    }

    #[test]
    fn prepare_returns_exact_occurrence_and_placeholder() {
        let mut db = DatabaseImpl::default();
        let text = "func main(): int {\n    let total: int = 1\n    return total\n}\n";
        let file = source(&mut db, text);
        let use_offset = offset(text, "return total") + 7;
        let target = prepare_rename(&db, file, use_offset).expect("rename target");
        assert_eq!(target.placeholder, "total");
        assert_eq!(
            &text[target.occurrence.start as usize..target.occurrence.end as usize],
            "total"
        );
    }

    #[test]
    fn rename_rejects_keywords_malformed_names_and_case_class_changes() {
        let mut db = DatabaseImpl::default();
        let text = "func main(): int {\n    let value: int = 1\n    return value\n}\n";
        let file = source(&mut db, text);
        let at = offset(text, "return value") + 7;
        assert_eq!(
            validate_rename(&db, file, at, "return"),
            Err(RenameError::InvalidIdentifier)
        );
        assert_eq!(
            validate_rename(&db, file, at, "two words"),
            Err(RenameError::InvalidIdentifier)
        );
        assert_eq!(
            validate_rename(&db, file, at, "Value"),
            Err(RenameError::WrongIdentifierKind)
        );
    }

    #[test]
    fn rename_rejects_related_scope_conflicts_but_allows_disjoint_scopes() {
        let mut db = DatabaseImpl::default();
        let text = "func first(target: int): int {\n    let taken: int = 1\n    return target\n}\nfunc second(): int {\n    let taken: int = 2\n    return taken\n}\n";
        let file = source(&mut db, text);
        let at = offset(text, "return target") + 7;
        assert!(matches!(
            validate_rename(&db, file, at, "taken"),
            Err(RenameError::Conflict { .. })
        ));

        let text = "func first(target: int): int {\n    return target\n}\nfunc second(): int {\n    let spare: int = 2\n    return spare\n}\n";
        let file = source(&mut db, text);
        let at = offset(text, "return target") + 7;
        assert!(validate_rename(&db, file, at, "spare").is_ok());
    }

    #[test]
    fn occurrences_are_exact_identifier_tokens_not_declaration_spans() {
        let mut db = DatabaseImpl::default();
        let text = "func main(value: int): int {\n    return value\n}\n";
        let file = source(&mut db, text);
        let at = offset(text, "return value") + 7;
        let target = prepare_rename(&db, file, at).expect("rename target");
        let spans = rename_occurrences(&db, file, target.symbol);
        assert_eq!(spans.len(), 2);
        assert!(spans
            .iter()
            .all(|span| { &text[span.start as usize..span.end as usize] == "value" }));
    }
}
