//! Syntax kinds for the Arandu CST (rowan) — CST-first pipeline (F1 structured).

/// Token and composite node kinds for the green/red tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
#[allow(non_camel_case_types)]
// `__LAST` is used as a bounds sentinel for `from_raw` (rowan Language).
#[allow(clippy::manual_non_exhaustive)]
pub enum SyntaxKind {
    // --- trivia ---
    WHITESPACE = 0,
    COMMENT,

    // --- tokens (for highlighting) ---
    KEYWORD,
    IDENT,
    TYPE_IDENT,
    NUMBER,
    STRING,
    CHAR,
    PUNCT,
    ERROR_TOKEN,

    // --- composite: file ---
    /// Root covering the entire file.
    SOURCE_FILE,

    /// Generic top-level unit (unknown / recovery). Prefer typed `*_ITEM` kinds.
    ITEM,
    MODULE_ITEM,
    IMPORT_ITEM,
    FUNC_ITEM,
    STRUCT_ITEM,
    ENUM_ITEM,
    INTERFACE_ITEM,
    CONST_ITEM,
    TYPE_ALIAS_ITEM,
    EXTERN_ITEM,
    IMPL_ITEM,

    /// `{ ... }` body (function body, struct body, …).
    BLOCK,
    /// Statement fragment inside a [`Self::BLOCK`] (tokens until `;` / next stmt; F1b).
    STMT,
    /// Expression node (event sink wraps RD `parse_expr`).
    EXPR,
    /// Unparsed / error region.
    ERROR,

    #[doc(hidden)]
    __LAST,
}

impl From<SyntaxKind> for rowan::SyntaxKind {
    fn from(kind: SyntaxKind) -> Self {
        Self(kind as u16)
    }
}

impl SyntaxKind {
    #[must_use]
    pub const fn is_trivia(self) -> bool {
        matches!(self, Self::WHITESPACE | Self::COMMENT)
    }

    /// Top-level declaration / import / module node (any typed or generic ITEM).
    #[must_use]
    pub const fn is_top_level_item(self) -> bool {
        matches!(
            self,
            Self::ITEM
                | Self::MODULE_ITEM
                | Self::IMPORT_ITEM
                | Self::FUNC_ITEM
                | Self::STRUCT_ITEM
                | Self::ENUM_ITEM
                | Self::INTERFACE_ITEM
                | Self::CONST_ITEM
                | Self::TYPE_ALIAS_ITEM
                | Self::EXTERN_ITEM
                | Self::IMPL_ITEM
        )
    }

    /// Convert raw rowan kind (same as `Language::kind_from_raw`).
    #[must_use]
    pub fn from_raw(raw: rowan::SyntaxKind) -> Self {
        assert!(raw.0 < Self::__LAST as u16);
        // SAFETY: raw produced by kind_to_raw from a valid SyntaxKind.
        unsafe { std::mem::transmute::<u16, SyntaxKind>(raw.0) }
    }

    /// LSP semantic-token friendly name (stable strings) for **token** kinds.
    #[must_use]
    pub const fn highlight_class(self) -> Option<&'static str> {
        match self {
            Self::KEYWORD => Some("keyword"),
            Self::IDENT => Some("variable"),
            Self::TYPE_IDENT => Some("type"),
            Self::NUMBER => Some("number"),
            Self::STRING => Some("string"),
            Self::CHAR => Some("string"),
            Self::COMMENT => Some("comment"),
            Self::PUNCT => Some("operator"),
            Self::ERROR_TOKEN | Self::ERROR => Some("error"),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AranduLanguage {}

impl rowan::Language for AranduLanguage {
    type Kind = SyntaxKind;

    fn kind_from_raw(raw: rowan::SyntaxKind) -> Self::Kind {
        SyntaxKind::from_raw(raw)
    }

    fn kind_to_raw(kind: Self::Kind) -> rowan::SyntaxKind {
        kind.into()
    }
}

pub type SyntaxNode = rowan::SyntaxNode<AranduLanguage>;
pub type SyntaxToken = rowan::SyntaxToken<AranduLanguage>;
pub type SyntaxElement = rowan::SyntaxElement<AranduLanguage>;
