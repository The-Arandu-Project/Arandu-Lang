//! Type-aware semantic highlights (F2): CST tokens reclassified via resolve.
//!
//! Pure data for LSP encoding — no `lsp_types` here.

use crate::db::HashEq;
use crate::passes::{resolve, syntax_tree};
use crate::{ArandCompilerDb, SourceFile};
use arandu_middle::{NodeKey, SymbolId, SymbolKind};
use arandu_parser::SyntaxKind;
use rustc_hash::FxHashMap;
use std::sync::Arc;

/// Semantic class for a highlight span (stable `u8` for HashEq / LSP legend index).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum HlKind {
    Keyword = 0,
    Function = 1,
    Variable = 2,
    Parameter = 3,
    Type = 4,
    Struct = 5,
    Enum = 6,
    Interface = 7,
    Namespace = 8,
    Number = 9,
    String = 10,
    Comment = 11,
    Operator = 12,
    Property = 13,
    Constant = 14,
    Decorator = 15,
}

impl HlKind {
    /// Legend index for LSP (must match `arandu_lsp::ide::semantic_tokens_legend`).
    #[must_use]
    pub const fn legend_index(self) -> u32 {
        self as u32
    }

    #[must_use]
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Keyword),
            1 => Some(Self::Function),
            2 => Some(Self::Variable),
            3 => Some(Self::Parameter),
            4 => Some(Self::Type),
            5 => Some(Self::Struct),
            6 => Some(Self::Enum),
            7 => Some(Self::Interface),
            8 => Some(Self::Namespace),
            9 => Some(Self::Number),
            10 => Some(Self::String),
            11 => Some(Self::Comment),
            12 => Some(Self::Operator),
            13 => Some(Self::Property),
            14 => Some(Self::Constant),
            15 => Some(Self::Decorator),
            _ => None,
        }
    }
}

/// Bitflags for semantic token modifiers (F2b).
pub type HlMods = u16;

/// Token is a definition / binding site.
pub const MOD_DECLARATION: HlMods = 1 << 0;
/// Token is a mutable binding (`mut` / assigned).
pub const MOD_MUTABLE: HlMods = 1 << 1;
/// Token is the defining occurrence of a symbol.
pub const MOD_DEFINITION: HlMods = 1 << 2;

/// One highlighted range in file byte offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HlToken {
    pub start: u32,
    pub end: u32,
    pub kind: HlKind,
    pub mods: HlMods,
}

fn lexical_kind(class: &str) -> Option<HlKind> {
    Some(match class {
        "keyword" => HlKind::Keyword,
        "variable" => HlKind::Variable,
        "type" => HlKind::Type,
        "number" => HlKind::Number,
        "string" => HlKind::String,
        "comment" => HlKind::Comment,
        "operator" => HlKind::Operator,
        _ => return None,
    })
}

fn symbol_kind_to_hl(kind: SymbolKind) -> HlKind {
    match kind {
        SymbolKind::Func | SymbolKind::ExternFunc | SymbolKind::AssociatedFunc => HlKind::Function,
        SymbolKind::Param => HlKind::Parameter,
        SymbolKind::Local | SymbolKind::ImportValue => HlKind::Variable,
        SymbolKind::Const => HlKind::Constant,
        SymbolKind::Struct => HlKind::Struct,
        SymbolKind::Enum | SymbolKind::EnumVariant => HlKind::Enum,
        SymbolKind::Interface => HlKind::Interface,
        SymbolKind::TypeAlias | SymbolKind::TypeParam | SymbolKind::ImportType => HlKind::Type,
        SymbolKind::Field => HlKind::Property,
        SymbolKind::Module | SymbolKind::NamespaceMember => HlKind::Namespace,
    }
}

/// Exact or tightest enclosing symbol for token `[start, end)`.
fn symbol_for_span(
    start: u32,
    end: u32,
    maps: &[&FxHashMap<NodeKey, SymbolId>],
) -> Option<SymbolId> {
    let exact = NodeKey { start, end };
    for map in maps {
        if let Some(&id) = map.get(&exact) {
            return Some(id);
        }
    }
    // Fallback: tightest containing key (definition/ref may be slightly wider).
    let mid = start.saturating_add(end.saturating_sub(start) / 2);
    let mut best: Option<(u32, SymbolId)> = None;
    for map in maps {
        for (key, &sym) in *map {
            if key.start <= mid && mid < key.end && key.start <= start && end <= key.end {
                let w = key.end.saturating_sub(key.start);
                if best.is_none_or(|(bw, _)| w < bw) {
                    best = Some((w, sym));
                }
            }
        }
    }
    best.map(|(_, s)| s)
}

/// Build highlights from CST + resolve (no Salsa; used by the tracked query and tests).
#[must_use]
pub fn compute_highlights(
    tree: &arandu_parser::SyntaxTree,
    resolved: &arandu_middle::ResolutionResult,
) -> Arc<[HlToken]> {
    let maps: [&FxHashMap<NodeKey, SymbolId>; 3] = [
        &resolved.resolved.value_refs,
        &resolved.resolved.type_refs,
        &resolved.resolved.definitions,
    ];
    let mut out: Vec<HlToken> = Vec::with_capacity(64);
    let source = tree.text();
    arandu_parser::for_each_highlight_token(tree, |tok, class| {
        let r = tok.text_range();
        let start = u32::from(r.start());
        let end = u32::from(r.end());

        let Some(lex) = lexical_kind(class) else {
            return;
        };
        if end <= start {
            return;
        }
        let mut mods = 0u16;
        let is_annotation_name = matches!(tok.kind(), SyntaxKind::IDENT | SyntaxKind::TYPE_IDENT)
            && usize::try_from(start)
                .ok()
                .and_then(|start| start.checked_sub(1))
                .is_some_and(|at| source.as_bytes().get(at) == Some(&b'@'));
        let kind = if is_annotation_name {
            HlKind::Decorator
        } else if matches!(tok.kind(), SyntaxKind::IDENT | SyntaxKind::TYPE_IDENT) {
            if let Some(sid) = symbol_for_span(start, end, &maps) {
                // Definition site?
                let key = NodeKey { start, end };
                if resolved.resolved.definitions.contains_key(&key)
                    || resolved
                        .resolved
                        .definitions
                        .iter()
                        .any(|(k, s)| *s == sid && k.start == start && k.end == end)
                {
                    mods |= MOD_DECLARATION | MOD_DEFINITION;
                }
                if resolved.resolved.mutable_symbols.contains(&sid) {
                    mods |= MOD_MUTABLE;
                }
                if let Some(sym) = resolved.symbols.try_get(sid) {
                    // Defining occurrence often equals symbol.span
                    if sym.span.start == start && sym.span.end == end {
                        mods |= MOD_DECLARATION | MOD_DEFINITION;
                    }
                    symbol_kind_to_hl(sym.kind)
                } else {
                    lex
                }
            } else {
                lex
            }
        } else {
            lex
        };
        out.push(HlToken {
            start,
            end,
            kind,
            mods,
        });
    });
    Arc::from(out)
}

/// Highlights restricted to `[range_start, range_end)` (F2b range request).
#[must_use]
pub fn highlights_in_range(tokens: &[HlToken], range_start: u32, range_end: u32) -> Vec<HlToken> {
    tokens
        .iter()
        .copied()
        .filter(|t| t.end > range_start && t.start < range_end)
        .collect()
}

/// Salsa memo: type-aware file highlights for IDE semantic tokens.
#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "file_highlights",
    file = ?file.file_id(db),
))]
pub fn file_highlights(db: &dyn ArandCompilerDb, file: SourceFile) -> HashEq<Arc<[HlToken]>> {
    let tree = syntax_tree(db, file);
    let resolved = resolve(db, file);
    let tokens = compute_highlights(tree, resolved);
    HashEq::new(tokens)
}
