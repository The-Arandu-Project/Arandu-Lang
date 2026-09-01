//! CST-first green tree: lex once → ITEM green + token cache; lower AST without re-lex.

use super::kind::{SyntaxKind, SyntaxNode};
use arandu_lexer::{Token, TokenKind, lex_recovering};
use rowan::{GreenNode, GreenNodeBuilder, TextRange, TextSize};
use std::sync::Arc;

/// Result of building a CST (authoritative text + green tree + token stream).
///
/// Tokens are produced by the **same** lex that builds the green tree. Lower
/// reuses them so typeck never pays a second full-file lex.
///
/// Cheap to clone: all heavy fields are `Arc` (or green Arc).
#[derive(Debug, Clone)]
pub struct SyntaxTree {
    pub(crate) green: GreenNode,
    pub(crate) text: Arc<str>,
    pub(crate) tokens: Arc<Vec<Token>>,
    /// Lex diagnostics from the same pass that produced [`Self::tokens`].
    pub(crate) lex_diagnostics: Arc<Vec<arandu_lexer::LexError>>,
}

impl SyntaxTree {
    #[must_use]
    pub fn green(&self) -> &GreenNode {
        &self.green
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Shared text buffer (can alias `SourceFile.text`).
    #[must_use]
    pub fn text_arc(&self) -> &Arc<str> {
        &self.text
    }

    /// Cached token stream from CST construction (includes ASI-inserted `;`).
    #[must_use]
    pub fn tokens(&self) -> &[Token] {
        &self.tokens
    }

    /// Shared token buffer for zero-copy lower.
    #[must_use]
    pub fn tokens_arc(&self) -> &Arc<Vec<Token>> {
        &self.tokens
    }

    /// Lex errors captured while building this tree (propagated by lower).
    #[must_use]
    pub fn lex_diagnostics(&self) -> &[arandu_lexer::LexError] {
        &self.lex_diagnostics
    }

    #[must_use]
    pub fn root(&self) -> SyntaxNode {
        SyntaxNode::new_root(self.green.clone())
    }

    /// Top-level item nodes (`FUNC_ITEM`, `STRUCT_ITEM`, … or generic `ITEM`).
    #[must_use]
    pub fn items(&self) -> Vec<SyntaxNode> {
        self.root()
            .children()
            .filter(|n| n.kind().is_top_level_item())
            .collect()
    }

    /// First `BLOCK` descendant of each top-level item (if any).
    #[must_use]
    pub fn item_blocks(&self) -> Vec<Option<SyntaxNode>> {
        self.items()
            .into_iter()
            .map(|item| item.descendants().find(|n| n.kind() == SyntaxKind::BLOCK))
            .collect()
    }

    /// Borrow item text as a slice of the shared source (no allocation).
    #[must_use]
    pub fn item_text(&self, index: usize) -> Option<&str> {
        let (s, e) = self.item_ranges().get(index).copied()?;
        let s = s as usize;
        let e = (e as usize).min(self.text.len()).max(s);
        Some(&self.text[s..e])
    }

    #[must_use]
    pub fn item_texts(&self) -> Vec<String> {
        self.item_ranges()
            .into_iter()
            .map(|(s, e)| {
                let s = s as usize;
                let e = (e as usize).min(self.text.len()).max(s);
                self.text[s..e].to_string()
            })
            .collect()
    }

    /// Byte range of each ITEM in source order.
    #[must_use]
    pub fn item_ranges(&self) -> Vec<(u32, u32)> {
        self.items()
            .into_iter()
            .map(|n| {
                let r = n.text_range();
                (u32::from(r.start()), u32::from(r.end()))
            })
            .collect()
    }

    /// Index of the ITEM covering `offset` (byte), if any.
    #[must_use]
    pub fn item_index_at(&self, offset: u32) -> Option<usize> {
        let off = TextSize::from(offset);
        self.items().into_iter().position(|n| {
            let r = n.text_range();
            r.start() <= off && off < r.end() || (off == r.end() && r.start() < r.end())
        })
    }
}

fn tree_from_lexed(
    source: &str,
    lexed: arandu_lexer::Lexed<'_>,
    spans: &[(u32, u32)],
) -> SyntaxTree {
    let green = build_green(source, &lexed.tokens, spans);
    SyntaxTree {
        green,
        text: Arc::from(source),
        tokens: Arc::new(lexed.tokens),
        lex_diagnostics: Arc::new(lexed.diagnostics),
    }
}

/// Build a tree from an existing shared text buffer (Salsa path shares `SourceFile.text`).
///
/// Primary path: RD parse with [`crate::syntax::events`] → green.
/// Fallback: brace-aware heuristic items (same as pre-event sink).
#[must_use]
pub fn parse_syntax_arc(text: Arc<str>) -> SyntaxTree {
    let lexed = lex_recovering(text.as_ref());
    let tokens = Arc::new(lexed.tokens);
    let diags = Arc::new(lexed.diagnostics);

    let mut parser = crate::parser::Parser::new(text.as_ref(), Arc::clone(&tokens)).with_events();
    let _ = parser.parse_program();
    let events = parser.take_events();

    let green =
        super::events::build_green_from_events(text.as_ref(), &events).unwrap_or_else(|| {
            let spans = find_top_level_item_spans(&tokens, text.len() as u32);
            build_green(text.as_ref(), &tokens, &spans)
        });

    #[cfg(debug_assertions)]
    {
        if diags.is_empty() && parser.diagnostics.is_empty() {
            let root = SyntaxNode::new_root(green.clone());
            let tree_text = root.text().to_string();
            debug_assert_eq!(
                tree_text,
                text.as_ref(),
                "CST is not lossless: the reconstructed CST tree diverged from the original source text without syntax errors. Likely a leak or duplication of parser events!"
            );
        }
    }

    SyntaxTree {
        green,
        text,
        tokens,
        lex_diagnostics: diags,
    }
}

/// CST-first parse: one lex → **RD with event sink** → green + token cache.
///
/// Falls back to heuristic ITEM spans if events are unbalanced (severe recovery).
#[must_use]
pub fn parse_syntax(source: &str) -> SyntaxTree {
    parse_syntax_arc(Arc::from(source))
}

/// Build CST with explicit item spans (advanced / tests) — heuristic builder.
#[must_use]
pub fn parse_syntax_with_item_spans(source: &str, item_spans: &[(u32, u32)]) -> SyntaxTree {
    let lexed = lex_recovering(source);
    tree_from_lexed(source, lexed, item_spans)
}

pub(crate) fn lex_diags_as_parse(tree: &SyntaxTree, file_id: u32) -> Vec<crate::ParseError> {
    tree.lex_diagnostics()
        .iter()
        .copied()
        .map(|err| crate::ParseError::from_lex(err, file_id))
        .collect()
}

/// Lower CST → AST: **green-guided walk** (F1b) with RD at each top-level item.
///
/// Falls back to a full linear RD pass when the green tree has no items or the walk
/// cannot recover a complete program. No re-lex; tokens come from the CST cache.
pub fn lower_syntax_to_program(
    tree: &SyntaxTree,
    file_id: u32,
) -> Result<crate::Program, crate::ParseError> {
    super::lower::lower_from_green(tree, file_id)
}

/// Linear RD lower (no green walk) — tests / explicit fallback.
pub fn lower_syntax_to_program_rd_only(
    tree: &SyntaxTree,
    file_id: u32,
) -> Result<crate::Program, crate::ParseError> {
    let output = lower_syntax_to_program_recovering_rd_only(tree, file_id);
    if let Some(err) = output.diagnostics.into_iter().next() {
        Err(err)
    } else {
        Ok(output.program)
    }
}

/// Recovering lower via green-guided walk (keeps diagnostics).
#[must_use]
pub fn lower_syntax_to_program_recovering(tree: &SyntaxTree, file_id: u32) -> crate::ParseOutput {
    super::lower::lower_from_green_recovering(tree, file_id)
}

/// Recovering linear RD lower (no green walk).
#[must_use]
pub fn lower_syntax_to_program_recovering_rd_only(
    tree: &SyntaxTree,
    file_id: u32,
) -> crate::ParseOutput {
    crate::parser::parse_token_stream(
        tree.text(),
        Arc::clone(tree.tokens_arc()),
        file_id,
        lex_diags_as_parse(tree, file_id),
    )
}

fn build_green(source: &str, tokens: &[Token], item_spans: &[(u32, u32)]) -> GreenNode {
    let mut builder = GreenNodeBuilder::new();
    builder.start_node(SyntaxKind::SOURCE_FILE.into());

    if item_spans.is_empty() {
        emit_tokens(&mut builder, source, tokens, 0, source.len() as u32);
    } else {
        let mut spans: Vec<(u32, u32)> = item_spans
            .iter()
            .copied()
            .filter(|(s, e)| e > s && (*s as usize) <= source.len())
            .collect();
        spans.sort_by_key(|(s, _)| *s);

        let mut cursor = 0u32;
        for (start, end) in spans {
            let start = start.max(cursor);
            let end = end.min(source.len() as u32).max(start);
            if start > cursor {
                emit_tokens(&mut builder, source, tokens, cursor, start);
            }
            if end > start {
                emit_structured_item(&mut builder, source, tokens, start, end);
            }
            cursor = end;
        }
        if (cursor as usize) < source.len() {
            emit_tokens(&mut builder, source, tokens, cursor, source.len() as u32);
        }
    }

    builder.finish_node();
    builder.finish()
}

/// Classify top-level item from the first item-start keyword in range.
#[must_use]
pub fn classify_item_kind(tokens: &[Token], range_start: u32, range_end: u32) -> SyntaxKind {
    for tok in tokens {
        if matches!(tok.kind, TokenKind::Eof | TokenKind::Error(_)) {
            continue;
        }
        let te = tok.start.saturating_add(tok.len);
        if te <= range_start || tok.start >= range_end {
            continue;
        }
        if is_item_start_keyword(tok.kind) {
            return match tok.kind {
                TokenKind::KwModule => SyntaxKind::MODULE_ITEM,
                TokenKind::KwImport | TokenKind::KwFrom => SyntaxKind::IMPORT_ITEM,
                TokenKind::KwFunc | TokenKind::KwAsync => SyntaxKind::FUNC_ITEM,
                TokenKind::KwStruct => SyntaxKind::STRUCT_ITEM,
                TokenKind::KwEnum => SyntaxKind::ENUM_ITEM,
                TokenKind::KwInterface => SyntaxKind::INTERFACE_ITEM,
                TokenKind::KwConst => SyntaxKind::CONST_ITEM,
                TokenKind::KwType => SyntaxKind::TYPE_ALIAS_ITEM,
                TokenKind::KwExtern => SyntaxKind::EXTERN_ITEM,
                _ => SyntaxKind::ITEM,
            };
        }
    }
    SyntaxKind::ITEM
}

/// Emit one top-level item with optional nested [`SyntaxKind::BLOCK`] for `{…}`.
fn emit_structured_item(
    builder: &mut GreenNodeBuilder<'_>,
    source: &str,
    tokens: &[Token],
    range_start: u32,
    range_end: u32,
) {
    let item_kind = classify_item_kind(tokens, range_start, range_end);
    builder.start_node(item_kind.into());

    // First `{` in this item range opens the body BLOCK (func/struct/enum/…).
    let brace_start = tokens.iter().find_map(|tok| {
        if matches!(tok.kind, TokenKind::LBrace)
            && tok.start >= range_start
            && tok.start < range_end
        {
            Some(tok.start)
        } else {
            None
        }
    });

    let Some(open_brace) = brace_start else {
        emit_tokens(builder, source, tokens, range_start, range_end);
        builder.finish_node();
        return;
    };

    // Header (before body `{`).
    if open_brace > range_start {
        emit_tokens(builder, source, tokens, range_start, open_brace);
    }

    // Match closing `}` at depth 0 relative to this brace.
    let mut depth = 0i32;
    let mut close_end = range_end;
    for tok in tokens {
        if tok.start < open_brace || tok.start >= range_end {
            continue;
        }
        match tok.kind {
            TokenKind::LBrace => depth += 1,
            TokenKind::RBrace => {
                depth -= 1;
                if depth == 0 {
                    close_end = tok.start.saturating_add(tok.len).min(range_end);
                    break;
                }
            }
            _ => {}
        }
    }

    emit_block_with_stmts(builder, source, tokens, open_brace, close_end);

    if close_end < range_end {
        emit_tokens(builder, source, tokens, close_end, range_end);
    }
    builder.finish_node();
}

/// `BLOCK` with `{` / `STMT`* / `}` (statements split on `;` at brace-depth 1).
fn emit_block_with_stmts(
    builder: &mut GreenNodeBuilder<'_>,
    source: &str,
    tokens: &[Token],
    open_brace: u32,
    close_end: u32,
) {
    builder.start_node(SyntaxKind::BLOCK.into());

    // Tokens strictly inside the block range.
    let in_block: Vec<&Token> = tokens
        .iter()
        .filter(|t| {
            !matches!(t.kind, TokenKind::Eof | TokenKind::Error(_))
                && t.start >= open_brace
                && t.start < close_end
        })
        .collect();

    if in_block.is_empty() {
        builder.finish_node();
        return;
    }

    // Opening `{`
    let first = in_block[0];
    if matches!(first.kind, TokenKind::LBrace) {
        emit_tokens(
            builder,
            source,
            tokens,
            first.start,
            first.start.saturating_add(first.len),
        );
    }

    let close_tok = in_block
        .last()
        .filter(|t| matches!(t.kind, TokenKind::RBrace));
    let inner_end = close_tok.map_or(close_end, |t| t.start);

    // Split interior into STMTs on `;` at depth==1 (inside outer braces).
    let mut depth = 0i32;
    let mut stmt_start: Option<u32> = None;
    for t in &in_block {
        match t.kind {
            TokenKind::LBrace => {
                depth += 1;
                if depth == 1 {
                    // just opened outer `{` — next content starts after this token
                    stmt_start = Some(t.start.saturating_add(t.len));
                }
            }
            TokenKind::RBrace => {
                if depth == 1 {
                    // flush last stmt before closing `}`
                    if let Some(ss) = stmt_start
                        && ss < t.start
                    {
                        builder.start_node(SyntaxKind::STMT.into());
                        emit_tokens(builder, source, tokens, ss, t.start);
                        builder.finish_node();
                    }
                    stmt_start = None;
                }
                depth -= 1;
            }
            TokenKind::Semicolon if depth == 1 => {
                let te = t.start.saturating_add(t.len);
                if let Some(ss) = stmt_start
                    && ss < te
                {
                    builder.start_node(SyntaxKind::STMT.into());
                    emit_tokens(builder, source, tokens, ss, te);
                    builder.finish_node();
                }
                stmt_start = Some(te);
            }
            _ => {
                if depth == 1 && stmt_start.is_none() {
                    stmt_start = Some(t.start);
                }
            }
        }
    }

    // Closing `}`
    if let Some(t) = close_tok {
        emit_tokens(
            builder,
            source,
            tokens,
            t.start,
            t.start.saturating_add(t.len),
        );
    } else if inner_end < close_end {
        emit_tokens(builder, source, tokens, inner_end, close_end);
    }

    builder.finish_node();
}

/// Build a single item green node (for [`crate::syntax::incremental::reparse_subtree`]).
#[must_use]
pub fn build_item_green(item_text: &str, tokens: &[Token]) -> GreenNode {
    let mut builder = GreenNodeBuilder::new();
    let end = item_text.len() as u32;
    emit_structured_item(&mut builder, item_text, tokens, 0, end);
    // emit_structured_item already finished the item node; wrap is not needed.
    // GreenNodeBuilder::finish requires exactly one root — the item node is the only child of empty parents.
    // Actually start_node ITEM was finished — stack should have one element. finish() returns it.
    builder.finish()
}

/// Heuristic top-level item ranges from tokens (**brace-depth aware**).
///
/// Starts an item at module/import/decl keywords (optionally after `public`)
/// only when `{}` depth is 0 — so `func` methods inside `interface` / `extern`
/// / `struct` are not treated as new top-level items.
#[must_use]
pub fn find_top_level_item_spans(tokens: &[Token], source_len: u32) -> Vec<(u32, u32)> {
    let mut starts: Vec<u32> = Vec::new();
    let mut depth = 0i32;
    let mut i = 0;
    // Last item-start keyword seen at depth 0 (to suppress `import` after `from`).
    let mut last_item_kw: Option<TokenKind> = None;
    while i < tokens.len() {
        let tok = &tokens[i];
        if matches!(tok.kind, TokenKind::Eof | TokenKind::Error(_)) {
            i += 1;
            continue;
        }

        // Keyword check at current depth (before this token modifies depth).
        // Item openers: module/import/from/func/struct/… — not `async`/`public` alone
        // (those are prefixes). Leading `@attr` / `public` / `async` are folded left.
        if depth == 0 && is_item_start_keyword(tok.kind) {
            if matches!(tok.kind, TokenKind::KwImport)
                && matches!(last_item_kw, Some(TokenKind::KwFrom))
            {
                // `from path import {…}` — do not split at `import`.
            } else {
                let start = expand_item_start_left(tokens, i);
                starts.push(start);
                last_item_kw = Some(tok.kind);
                i += 1;
                continue;
            }
        }

        match tok.kind {
            TokenKind::LBrace => depth += 1,
            TokenKind::RBrace => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    last_item_kw = None;
                }
            }
            _ => {}
        }
        i += 1;
    }

    if starts.is_empty() {
        return Vec::new();
    }

    let mut spans = Vec::with_capacity(starts.len());
    for (idx, &start) in starts.iter().enumerate() {
        let end = if idx + 1 < starts.len() {
            starts[idx + 1]
        } else {
            // End of last item: last non-eof token end or source_len
            tokens
                .iter()
                .rev()
                .find(|t| !matches!(t.kind, TokenKind::Eof))
                .map(|t| t.start.saturating_add(t.len))
                .unwrap_or(source_len)
                .min(source_len)
        };
        if end > start {
            spans.push((start, end));
        }
    }
    spans
}

fn is_item_start_keyword(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::KwModule
            | TokenKind::KwImport
            | TokenKind::KwFrom
            | TokenKind::KwFunc
            | TokenKind::KwStruct
            | TokenKind::KwEnum
            | TokenKind::KwInterface
            | TokenKind::KwConst
            | TokenKind::KwType
            | TokenKind::KwExtern // Note: KwAsync / KwPublic are prefixes — folded via `expand_item_start_left`.
    )
}

/// Include leading `@attr…`, `public`, `async`, and doc comments before an item keyword.
///
/// Doc comments must attach to the **following** item. Otherwise the previous
/// item's span absorbs `/// …` (end = next start), hand-lower sees leftover
/// tokens after `}`, falls back to RD, and fails mid-field (e.g. gen_arena.aru).
fn expand_item_start_left(tokens: &[Token], kw_index: usize) -> u32 {
    let mut idx = kw_index;
    let mut start = tokens[kw_index].start;
    while idx > 0 {
        let prev = &tokens[idx - 1];
        match prev.kind {
            // ASI often inserts `;` after bare `@Attr` / names before `public`/`func`.
            TokenKind::Semicolon => {
                idx -= 1;
            }
            TokenKind::DocComment => {
                start = prev.start;
                idx -= 1;
            }
            TokenKind::KwAsync | TokenKind::KwPublic => {
                start = prev.start;
                idx -= 1;
            }
            TokenKind::RParen => {
                // Walk back a `(…)` group (attribute args), then name + `@`.
                let mut depth = 1i32;
                start = prev.start;
                idx -= 1;
                while idx > 0 && depth > 0 {
                    match tokens[idx - 1].kind {
                        TokenKind::RParen => depth += 1,
                        TokenKind::LParen => depth -= 1,
                        _ => {}
                    }
                    start = tokens[idx - 1].start;
                    idx -= 1;
                }
                // optional attr name
                if idx > 0
                    && matches!(
                        tokens[idx - 1].kind,
                        TokenKind::IdentValue | TokenKind::IdentType
                    )
                {
                    start = tokens[idx - 1].start;
                    idx -= 1;
                }
                if idx > 0 && matches!(tokens[idx - 1].kind, TokenKind::At) {
                    start = tokens[idx - 1].start;
                    idx -= 1;
                } else {
                    break;
                }
            }
            TokenKind::IdentValue | TokenKind::IdentType if idx >= 2 => {
                if matches!(tokens[idx - 2].kind, TokenKind::At) {
                    start = tokens[idx - 2].start;
                    idx -= 2;
                } else {
                    break;
                }
            }
            TokenKind::At => {
                start = prev.start;
                idx -= 1;
            }
            _ => break,
        }
    }
    start
}

fn emit_tokens(
    builder: &mut GreenNodeBuilder<'_>,
    source: &str,
    tokens: &[Token],
    range_start: u32,
    range_end: u32,
) {
    let mut cursor = range_start;
    for tok in tokens {
        if matches!(tok.kind, TokenKind::Eof | TokenKind::Error(_)) {
            continue;
        }
        let ts = tok.start;
        let te = tok.start.saturating_add(tok.len);
        if te <= range_start || ts >= range_end {
            continue;
        }
        if ts > cursor {
            let gs = cursor as usize;
            let ge = ts.min(range_end) as usize;
            if ge > gs {
                let gap = &source[gs..ge];
                if !gap.is_empty() {
                    builder.token(SyntaxKind::WHITESPACE.into(), gap);
                }
            }
            cursor = ts.min(range_end);
        }
        let s = ts.max(range_start) as usize;
        let e = te.min(range_end) as usize;
        if e <= s {
            continue;
        }
        let text = &source[s..e];
        let kind = map_token_kind(tok.kind);
        builder.token(kind.into(), text);
        cursor = te.min(range_end);
    }
    if cursor < range_end {
        let gap = &source[cursor as usize..range_end as usize];
        if !gap.is_empty() {
            builder.token(SyntaxKind::WHITESPACE.into(), gap);
        }
    }
}

/// Map lexer token kinds to CST token kinds (shared with event sink).
#[must_use]
pub fn map_token_kind(kind: TokenKind) -> SyntaxKind {
    use TokenKind::*;
    match kind {
        DocComment => SyntaxKind::COMMENT,
        IdentValue | KwSelf => SyntaxKind::IDENT,
        IdentType => SyntaxKind::TYPE_IDENT,
        IntDec | IntHex | IntBin | IntOct | Float => SyntaxKind::NUMBER,
        StringStart | StringText | StringEscape | InterpStart | InterpEnd | StringEnd
        | RawString | MultilineStringStart | MultilineStringEnd => SyntaxKind::STRING,
        Char => SyntaxKind::CHAR,
        TypeInt | TypeUint | TypeFloat | TypeI8 | TypeI16 | TypeI32 | TypeI64 | TypeU8
        | TypeU16 | TypeU32 | TypeU64 | TypeF32 | TypeF64 | TypeBool | TypeByte | TypeChar
        | TypeStr | TypeAny | TypeErr => SyntaxKind::TYPE_IDENT,
        BoolTrue | BoolFalse | Nil => SyntaxKind::KEYWORD,
        k if is_keyword_kind(k) => SyntaxKind::KEYWORD,
        Error(_) => SyntaxKind::ERROR_TOKEN,
        Eof => SyntaxKind::WHITESPACE,
        _ => SyntaxKind::PUNCT,
    }
}

fn is_keyword_kind(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::KwIf
            | TokenKind::KwElse
            | TokenKind::KwFor
            | TokenKind::KwIn
            | TokenKind::KwWhile
            | TokenKind::KwMatch
            | TokenKind::KwReturn
            | TokenKind::KwBreak
            | TokenKind::KwContinue
            | TokenKind::KwFunc
            | TokenKind::KwAsync
            | TokenKind::KwAwait
            | TokenKind::KwStruct
            | TokenKind::KwEnum
            | TokenKind::KwInterface
            | TokenKind::KwConst
            | TokenKind::KwType
            | TokenKind::KwModule
            | TokenKind::KwImport
            | TokenKind::KwFrom
            | TokenKind::KwAs
            | TokenKind::KwPublic
            | TokenKind::KwExtern
            | TokenKind::KwUnsafe
            | TokenKind::KwWhere
            | TokenKind::KwCatch
            | TokenKind::KwIs
            | TokenKind::KwSet
            | TokenKind::KwOwn
            | TokenKind::KwMut
            | TokenKind::KwShared
            | TokenKind::KwRef
            | TokenKind::KwPtr
            | TokenKind::KwAlloc
            | TokenKind::KwFree
            | TokenKind::KwDefer
            | TokenKind::KwErrdefer
            | TokenKind::KwLet
    )
}

#[must_use]
pub fn text_range(start: u32, end: u32) -> TextRange {
    TextRange::new(TextSize::from(start), TextSize::from(end))
}
