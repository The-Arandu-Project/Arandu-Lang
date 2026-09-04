//! Hand-lower expressions (Pratt + postfix + primary forms).

use super::cursor::{Cursor, HandCtx, bin_bp};
use super::pattern::parse_match_arm;
use super::stmt::parse_block_tokens;
use super::ty::{can_start_type_kind, parse_type, primitive_type_token_name};
use crate::ast::ast_pool::{ExprId, ExprKind};
use crate::{
    CatchHandler, Condition, FieldInit, LambdaBody, LambdaParam, StringPart, TypeExpr, TypeName,
    UnaryOp,
};
use arandu_lexer::{Token, TokenKind};
use smallvec::smallvec;
use smol_str::SmolStr;

/// Parse expression from token slice; must consume all tokens.
pub fn try_hand_lower_expr_all(
    pool: &mut crate::ast::ast_pool::AstPool,
    source: &str,
    toks: &[&Token],
    file_id: u32,
) -> Option<ExprId> {
    if toks.is_empty() {
        return None;
    }
    let mut ctx = HandCtx {
        pool,
        source,
        file_id,
    };
    let mut cur = Cursor::new(toks);
    let expr = try_hand_lower_expr(&mut ctx, &mut cur, 0)?;
    cur.skip_semis();
    if !cur.at_end() {
        return None;
    }
    Some(expr)
}

/// Parse expression with minimum binding power.
pub fn try_hand_lower_expr(
    ctx: &mut HandCtx<'_>,
    cur: &mut Cursor<'_>,
    min_bp: u8,
) -> Option<ExprId> {
    let mut left = parse_unary_primary_post(ctx, cur)?;

    loop {
        // catch / as as postfix-infix
        match cur.peek_kind() {
            Some(TokenKind::KwCatch) if min_bp <= 10 => {
                let span_start = ctx.pool.expr_span(left).start;
                cur.bump(); // catch
                let handler = if cur.peek_kind() == Some(TokenKind::Pipe) {
                    let pipe = cur.bump()?;
                    let err_tok = cur.expect(TokenKind::IdentValue)?;
                    let error = SmolStr::new(ctx.text(err_tok)?);
                    cur.expect(TokenKind::Pipe)?;
                    let block = parse_block_tokens(ctx, cur)?;
                    let catch_handler = CatchHandler::Block {
                        span: ctx.span(pipe.start, block.span.end),
                        error,
                        block,
                    };
                    ctx.pool.alloc_catch_handler(catch_handler)
                } else {
                    let expr = try_hand_lower_expr(ctx, cur, 10)?;
                    let catch_handler = CatchHandler::Expr {
                        span: ctx.pool.expr_span(expr),
                        expr,
                    };
                    ctx.pool.alloc_catch_handler(catch_handler)
                };
                let end = match ctx.pool.catch_handler(handler) {
                    CatchHandler::Block { span, .. } | CatchHandler::Expr { span, .. } => span.end,
                };
                left = ctx.pool.alloc_expr(
                    ExprKind::Catch {
                        expr: left,
                        handler,
                    },
                    ctx.span(span_start, end),
                );
                continue;
            }
            Some(TokenKind::KwAs) if min_bp <= 140 => {
                // cast bp 140 matches RD
                let span_start = ctx.pool.expr_span(left).start;
                cur.bump();
                let ty = parse_type(ctx, cur)?;
                let end = ctx.pool.type_expr_span(ty).end;
                left = ctx
                    .pool
                    .alloc_expr(ExprKind::Cast { expr: left, ty }, ctx.span(span_start, end));
                continue;
            }
            _ => {}
        }

        let Some(kind) = cur.peek_kind() else {
            break;
        };
        let Some((l_bp, r_bp, op)) = bin_bp(kind) else {
            break;
        };
        if l_bp < min_bp {
            break;
        }
        // chained ranges need parens
        if matches!(
            op,
            crate::BinaryOp::RangeExclusive | crate::BinaryOp::RangeInclusive
        ) && matches!(
            ctx.pool.expr(left),
            ExprKind::Binary {
                op: crate::BinaryOp::RangeExclusive | crate::BinaryOp::RangeInclusive,
                ..
            }
        ) {
            return None;
        }
        cur.bump();
        let right = try_hand_lower_expr(ctx, cur, r_bp)?;
        let left_span = ctx.pool.expr_span(left);
        let right_span = ctx.pool.expr_span(right);
        let span = ctx.span(left_span.start, right_span.end);
        left = if op == crate::BinaryOp::NullCoalesce {
            ctx.pool
                .alloc_expr(ExprKind::NullCoalesce { left, right }, span)
        } else {
            ctx.pool
                .alloc_expr(ExprKind::Binary { op, left, right }, span)
        };
    }

    Some(left)
}

fn parse_unary_primary_post(ctx: &mut HandCtx<'_>, cur: &mut Cursor<'_>) -> Option<ExprId> {
    let t = cur.peek()?;
    match t.kind {
        TokenKind::Minus | TokenKind::Bang | TokenKind::Tilde | TokenKind::KwAwait => {
            let op = match t.kind {
                TokenKind::Minus => UnaryOp::Neg,
                TokenKind::Bang => UnaryOp::Not,
                TokenKind::Tilde => UnaryOp::BitNot,
                TokenKind::KwAwait => UnaryOp::Await,
                _ => unreachable!(),
            };
            let start = t.start;
            cur.bump();
            // Prefix binds tighter than all binary ops (RD uses 150; Mul is 130).
            // min_bp=100 wrongly absorbed `*a + *b` as `*(a + *b)`.
            let expr = try_hand_lower_expr(ctx, cur, 140)?;
            let end = ctx.pool.expr_span(expr).end;
            Some(
                ctx.pool
                    .alloc_expr(ExprKind::Unary { op, expr }, ctx.span(start, end)),
            )
        }
        // `&mut expr` / `&expr` — address-of (F2.0). Distinct from binary `a & b`.
        TokenKind::Amp => {
            let start = t.start;
            cur.bump();
            let is_mut = cur.eat(TokenKind::KwMut);
            let expr = try_hand_lower_expr(ctx, cur, 140)?;
            let end = ctx.pool.expr_span(expr).end;
            let op = if is_mut {
                UnaryOp::RefMut
            } else {
                UnaryOp::Ref
            };
            Some(
                ctx.pool
                    .alloc_expr(ExprKind::Unary { op, expr }, ctx.span(start, end)),
            )
        }
        // Canonical `ref expr` / `mut ref expr` safe borrows.
        TokenKind::KwRef => {
            let start = t.start;
            cur.bump();
            let expr = try_hand_lower_expr(ctx, cur, 140)?;
            let end = ctx.pool.expr_span(expr).end;
            Some(ctx.pool.alloc_expr(
                ExprKind::Unary {
                    op: UnaryOp::Ref,
                    expr,
                },
                ctx.span(start, end),
            ))
        }
        TokenKind::KwMut if cur.peek_at(1).map(|token| token.kind) == Some(TokenKind::KwRef) => {
            let start = t.start;
            cur.bump();
            cur.bump();
            let expr = try_hand_lower_expr(ctx, cur, 140)?;
            let end = ctx.pool.expr_span(expr).end;
            Some(ctx.pool.alloc_expr(
                ExprKind::Unary {
                    op: UnaryOp::RefMut,
                    expr,
                },
                ctx.span(start, end),
            ))
        }
        // `*expr` — deref (F2.0). Unary binds tighter than binary `*`/`+` (bp ≤ 130).
        TokenKind::Star => {
            let start = t.start;
            cur.bump();
            let expr = try_hand_lower_expr(ctx, cur, 140)?;
            let end = ctx.pool.expr_span(expr).end;
            Some(ctx.pool.alloc_expr(
                ExprKind::Unary {
                    op: UnaryOp::Deref,
                    expr,
                },
                ctx.span(start, end),
            ))
        }
        TokenKind::KwAlloc => {
            let start = t.start;
            cur.bump();
            let expr = try_hand_lower_expr(ctx, cur, 100)?;
            let end = ctx.pool.expr_span(expr).end;
            Some(
                ctx.pool
                    .alloc_expr(ExprKind::Alloc { expr }, ctx.span(start, end)),
            )
        }
        _ => parse_primary_post(ctx, cur),
    }
}

fn parse_primary_post(ctx: &mut HandCtx<'_>, cur: &mut Cursor<'_>) -> Option<ExprId> {
    let mut left = parse_primary(ctx, cur)?;

    loop {
        match cur.peek_kind() {
            Some(TokenKind::LParen) => {
                cur.bump();
                let mut args = Vec::new();
                if cur.peek_kind() != Some(TokenKind::RParen) {
                    loop {
                        args.push(try_hand_lower_expr(ctx, cur, 0)?);
                        if cur.eat(TokenKind::Comma) {
                            continue;
                        }
                        break;
                    }
                }
                let close = cur.expect(TokenKind::RParen)?;
                let args_range = ctx.pool.alloc_expr_list(&args);
                let left_span = ctx.pool.expr_span(left);
                let mut end = close.start + close.len;
                // Trailing-block call `f(args) { ... }` only via allows_trailing_block.
                // Match scrutinees are sliced to exclude `{`, so this is mainly for
                // full-expression hand lower; keep consistent with RD allow_block_calls.
                let trailing = if cur.peek_kind() == Some(TokenKind::LBrace)
                    && allows_trailing_block(ctx, left)
                {
                    let block = parse_block_tokens(ctx, cur)?;
                    end = block.span.end;
                    Some(ctx.pool.alloc_block(block))
                } else {
                    None
                };
                left = ctx.pool.alloc_expr(
                    ExprKind::Call {
                        callee: left,
                        args: args_range,
                        trailing_block: trailing,
                    },
                    ctx.span(left_span.start, end),
                );
            }
            Some(TokenKind::LBrace) if allows_trailing_block(ctx, left) => {
                // Root fix: `lib.Type {}` / `lib.Type { x: 1 }` starts as Path+Field
                // (module is IdentValue). Empty `{}` successfully parses as a trailing
                // *block* call, so the type never resolves (silent Error). Prefer
                // struct-literal when the path ends in a type-like name and `{`
                // looks like field inits (empty or `name:`).
                if looks_like_struct_lit_after_type_path(ctx, cur, left)
                    && let Some(sl) = try_struct_lit_from_type_path(ctx, cur, left)
                {
                    left = sl;
                    continue;
                }
                // bare trailing block call: f { ... }
                let left_span = ctx.pool.expr_span(left);
                let block = parse_block_tokens(ctx, cur)?;
                let end = block.span.end;
                let block_id = ctx.pool.alloc_block(block);
                let empty = ctx.pool.alloc_expr_list(&[]);
                left = ctx.pool.alloc_expr(
                    ExprKind::Call {
                        callee: left,
                        args: empty,
                        trailing_block: Some(block_id),
                    },
                    ctx.span(left_span.start, end),
                );
            }
            Some(TokenKind::Lt) if looks_like_generic_args(cur) => {
                cur.bump();
                let mut type_args = Vec::new();
                if cur.peek_kind() != Some(TokenKind::Gt) {
                    loop {
                        type_args.push(parse_type(ctx, cur)?);
                        if cur.eat(TokenKind::Comma) {
                            continue;
                        }
                        break;
                    }
                }
                let gt = cur.expect(TokenKind::Gt)?;
                let args = ctx.pool.alloc_type_expr_list(&type_args);
                let left_span = ctx.pool.expr_span(left);
                left = ctx.pool.alloc_expr(
                    ExprKind::Generic { callee: left, args },
                    ctx.span(left_span.start, gt.start + gt.len),
                );
                // generic must be followed by call or trailing block
                if cur.peek_kind() == Some(TokenKind::LParen) {
                    continue; // loop will handle call
                }
                if cur.peek_kind() == Some(TokenKind::LBrace) {
                    continue;
                }
                return None;
            }
            Some(TokenKind::Dot) => {
                cur.bump();
                let field_tok = cur.peek()?;
                if !matches!(field_tok.kind, TokenKind::IdentValue | TokenKind::IdentType) {
                    return None;
                }
                let field = SmolStr::new(ctx.text(field_tok)?);
                cur.bump();
                let left_span = ctx.pool.expr_span(left);
                let span = ctx.span(left_span.start, field_tok.start + field_tok.len);
                left = ctx
                    .pool
                    .alloc_expr(ExprKind::Field { base: left, field }, span);
            }
            Some(TokenKind::SafeDot) => {
                cur.bump();
                let field_tok = cur.expect(TokenKind::IdentValue)?;
                let field = SmolStr::new(ctx.text(field_tok)?);
                let left_span = ctx.pool.expr_span(left);
                let span = ctx.span(left_span.start, field_tok.start + field_tok.len);
                left = ctx
                    .pool
                    .alloc_expr(ExprKind::SafeField { base: left, field }, span);
            }
            Some(TokenKind::LBracket) => {
                cur.bump();
                let index = try_hand_lower_expr(ctx, cur, 0)?;
                let close = cur.expect(TokenKind::RBracket)?;
                let left_span = ctx.pool.expr_span(left);
                let span = ctx.span(left_span.start, close.start + close.len);
                left = ctx
                    .pool
                    .alloc_expr(ExprKind::Index { base: left, index }, span);
            }
            Some(TokenKind::SafeIndexStart) => {
                cur.bump();
                let index = try_hand_lower_expr(ctx, cur, 0)?;
                let close = cur.expect(TokenKind::RBracket)?;
                let left_span = ctx.pool.expr_span(left);
                let span = ctx.span(left_span.start, close.start + close.len);
                left = ctx
                    .pool
                    .alloc_expr(ExprKind::SafeIndex { base: left, index }, span);
            }
            Some(TokenKind::Question) => {
                let q = cur.bump()?;
                let left_span = ctx.pool.expr_span(left);
                let span = ctx.span(left_span.start, q.start + q.len);
                left = ctx.pool.alloc_expr(ExprKind::Try { expr: left }, span);
            }
            _ => break,
        }
    }

    Some(left)
}

fn allows_trailing_block(ctx: &HandCtx<'_>, left: ExprId) -> bool {
    matches!(
        ctx.pool.expr(left),
        ExprKind::Path { .. }
            | ExprKind::Field { .. }
            | ExprKind::Generic { .. }
            | ExprKind::TypePath { .. }
    )
}

/// Type-like path segment: starts with uppercase (Arandu IdentType convention).
fn is_type_like_name(name: &str) -> bool {
    name.chars().next().is_some_and(|c| c.is_uppercase())
}

/// Collect `Path` / `a.b.Type` Field chains into type path segments.
fn type_path_segments_from_expr(
    ctx: &HandCtx<'_>,
    expr: ExprId,
) -> Option<smallvec::SmallVec<[SmolStr; 3]>> {
    match ctx.pool.expr(expr) {
        ExprKind::Path { path } if path.len() == 1 && is_type_like_name(&path[0]) => {
            Some(path.clone())
        }
        ExprKind::Field { base, field } if is_type_like_name(field) => {
            let mut segs = type_path_segments_from_expr_module(ctx, *base)?;
            segs.push(field.clone());
            Some(segs)
        }
        _ => None,
    }
}

/// Module path prefix: `lib` or `a.b` (all value/module segments).
fn type_path_segments_from_expr_module(
    ctx: &HandCtx<'_>,
    expr: ExprId,
) -> Option<smallvec::SmallVec<[SmolStr; 3]>> {
    match ctx.pool.expr(expr) {
        ExprKind::Path { path } => Some(path.clone()),
        ExprKind::Field { base, field } => {
            let mut segs = type_path_segments_from_expr_module(ctx, *base)?;
            segs.push(field.clone());
            Some(segs)
        }
        _ => None,
    }
}

/// After a type-shaped path, `{` starts a struct lit if empty or `ident:`.
fn looks_like_struct_lit_after_type_path(
    ctx: &HandCtx<'_>,
    cur: &Cursor<'_>,
    left: ExprId,
) -> bool {
    if type_path_segments_from_expr(ctx, left).is_none() {
        return false;
    }
    // Peek inside `{` without consuming.
    if cur.peek_kind() != Some(TokenKind::LBrace) {
        return false;
    }
    match cur.peek_at(1).map(|t| t.kind) {
        Some(TokenKind::RBrace) => true, // `Type {}`
        Some(TokenKind::IdentValue | TokenKind::IdentType) => {
            // `Type { field: ... }`
            cur.peek_at(2).is_some_and(|t| t.kind == TokenKind::Colon)
        }
        _ => false,
    }
}

/// Parse `{ field: expr, ... }` after a type-shaped Path/Field into StructLiteral.
fn try_struct_lit_from_type_path(
    ctx: &mut HandCtx<'_>,
    cur: &mut Cursor<'_>,
    left: ExprId,
) -> Option<ExprId> {
    let segs = type_path_segments_from_expr(ctx, left)?;
    let left_span = ctx.pool.expr_span(left);
    let name = TypeName {
        span: left_span,
        path: segs,
    };
    let empty_args = ctx.pool.alloc_type_expr_list(&[]);
    let ty = ctx.pool.alloc_type_expr(TypeExpr::Named {
        span: left_span,
        name,
        args: empty_args,
    });

    cur.expect(TokenKind::LBrace)?;
    let mut fields = Vec::new();
    if cur.peek_kind() != Some(TokenKind::RBrace) {
        loop {
            if cur.peek_kind() == Some(TokenKind::RangeExclusive) {
                let dot_tok = cur.expect(TokenKind::RangeExclusive)?;
                let value = try_hand_lower_expr(ctx, cur, 0)?;
                let fend = ctx.pool.expr_span(value).end;
                let init_id = ctx.pool.alloc_field_init(FieldInit {
                    span: ctx.span(dot_tok.start, fend),
                    name: SmolStr::new(".."),
                    value,
                });
                fields.push(init_id);
                if cur.eat(TokenKind::Comma) {
                    // optional trailing comma
                }
                break;
            }
            let name_tok = cur.peek()?;
            if !matches!(name_tok.kind, TokenKind::IdentValue | TokenKind::IdentType) {
                return None;
            }
            let fname = SmolStr::new(ctx.text(name_tok)?);
            let fstart = name_tok.start;
            cur.bump();
            let value = if cur.eat(TokenKind::Colon) {
                try_hand_lower_expr(ctx, cur, 0)?
            } else {
                let fspan = ctx.span(fstart, name_tok.start + name_tok.len);
                ctx.pool.alloc_expr(
                    ExprKind::Path {
                        path: smallvec::smallvec![fname.clone()],
                    },
                    fspan,
                )
            };
            let fend = ctx.pool.expr_span(value).end;
            let init_id = ctx.pool.alloc_field_init(FieldInit {
                span: ctx.span(fstart, fend),
                name: fname,
                value,
            });
            fields.push(init_id);
            if !cur.eat(TokenKind::Comma) {
                break;
            }
            if cur.peek_kind() == Some(TokenKind::RBrace) {
                break;
            }
        }
    }
    let close = cur.expect(TokenKind::RBrace)?;
    let range = ctx.pool.alloc_field_init_list(&fields);
    Some(ctx.pool.alloc_expr(
        ExprKind::StructLiteral { ty, fields: range },
        ctx.span(left_span.start, close.start + close.len),
    ))
}

fn looks_like_generic_args(cur: &Cursor<'_>) -> bool {
    // scan for matching `>` then `( ` or `{`
    let mut depth = 0i32;
    let mut i = 0usize;
    while let Some(t) = cur.peek_at(i) {
        match t.kind {
            TokenKind::Lt => depth += 1,
            TokenKind::Gt => {
                depth -= 1;
                if depth == 0 {
                    return cur
                        .peek_at(i + 1)
                        .is_some_and(|n| matches!(n.kind, TokenKind::LParen | TokenKind::LBrace));
                }
            }
            TokenKind::Eof => return false,
            _ => {}
        }
        i += 1;
        if i > 64 {
            return false;
        }
    }
    false
}

fn parse_primary(ctx: &mut HandCtx<'_>, cur: &mut Cursor<'_>) -> Option<ExprId> {
    let t = cur.peek()?;
    let start = t.start;
    match t.kind {
        TokenKind::IntDec | TokenKind::IntHex | TokenKind::IntBin | TokenKind::IntOct => {
            let value = SmolStr::new(ctx.text(t)?);
            cur.bump();
            Some(
                ctx.pool
                    .alloc_expr(ExprKind::Int { value }, ctx.token_span(t)),
            )
        }
        TokenKind::Float => {
            let value = SmolStr::new(ctx.text(t)?);
            cur.bump();
            Some(
                ctx.pool
                    .alloc_expr(ExprKind::Float { value }, ctx.token_span(t)),
            )
        }
        TokenKind::BoolTrue | TokenKind::BoolFalse => {
            cur.bump();
            Some(ctx.pool.alloc_expr(
                ExprKind::Bool {
                    value: matches!(t.kind, TokenKind::BoolTrue),
                },
                ctx.token_span(t),
            ))
        }
        TokenKind::Char => {
            let value = SmolStr::new(t.char_content(ctx.source));
            cur.bump();
            Some(
                ctx.pool
                    .alloc_expr(ExprKind::Char { value }, ctx.token_span(t)),
            )
        }
        TokenKind::Nil => {
            cur.bump();
            Some(ctx.pool.alloc_expr(ExprKind::Nil, ctx.token_span(t)))
        }
        TokenKind::KwSelf => {
            cur.bump();
            Some(ctx.pool.alloc_expr(
                ExprKind::Path {
                    path: smallvec![SmolStr::new_static("self")],
                },
                ctx.token_span(t),
            ))
        }
        TokenKind::IdentValue => {
            // `mod.Type.method` pattern: identical check to RD parse_prefix
            if cur.peek_at(1).is_some_and(|t| t.kind == TokenKind::Dot)
                && cur
                    .peek_at(2)
                    .is_some_and(|t| t.kind == TokenKind::IdentType)
            {
                return parse_type_led(ctx, cur, start);
            }
            let text = ctx.text(t)?;
            cur.bump();
            Some(ctx.pool.alloc_expr(
                ExprKind::Path {
                    path: smallvec![SmolStr::new(text)],
                },
                ctx.token_span(t),
            ))
        }
        // T2.2: `.Ok(val)` / `.None` — leading Dot + type/value ident (+ optional call args).
        TokenKind::Dot => {
            cur.bump();
            let name_tok = cur.peek()?;
            if !matches!(name_tok.kind, TokenKind::IdentValue | TokenKind::IdentType) {
                return None;
            }
            let name = SmolStr::new(ctx.text(name_tok)?);
            cur.bump();
            let mut end = name_tok.start + name_tok.len;
            let args = if cur.eat(TokenKind::LParen) {
                let mut arg_ids = Vec::new();
                if cur.peek_kind() != Some(TokenKind::RParen) {
                    loop {
                        arg_ids.push(try_hand_lower_expr(ctx, cur, 0)?);
                        if cur.eat(TokenKind::Comma) {
                            continue;
                        }
                        break;
                    }
                }
                let close = cur.expect(TokenKind::RParen)?;
                end = close.start + close.len;
                ctx.pool.alloc_expr_list(&arg_ids)
            } else {
                ctx.pool.alloc_expr_list(&[])
            };
            Some(
                ctx.pool
                    .alloc_expr(ExprKind::VariantSugar { name, args }, ctx.span(start, end)),
            )
        }
        TokenKind::IdentType => parse_type_led(ctx, cur, start),
        // type token as type-led (int is TypeInt etc.)
        k if primitive_type_token_name(k).is_some() => parse_type_led(ctx, cur, start),
        TokenKind::LParen => {
            cur.bump();
            if looks_like_lambda(cur) {
                return parse_lambda(ctx, cur, start);
            }
            let inner = try_hand_lower_expr(ctx, cur, 0)?;
            let close = cur.expect(TokenKind::RParen)?;
            Some(ctx.pool.alloc_expr(
                ExprKind::Group { expr: inner },
                ctx.span(start, close.start + close.len),
            ))
        }
        TokenKind::StringStart => parse_string(ctx, cur, start, TokenKind::StringEnd),
        TokenKind::MultilineStringStart => {
            parse_string(ctx, cur, start, TokenKind::MultilineStringEnd)
        }
        TokenKind::RawString => {
            let value = SmolStr::new(t.raw_string_content(ctx.source));
            cur.bump();
            let span = ctx.token_span(t);
            let part = StringPart::Text { span, text: value };
            let part_id = ctx.pool.alloc_string_part(part);
            let range = ctx.pool.alloc_string_part_list(&[part_id]);
            Some(
                ctx.pool
                    .alloc_expr(ExprKind::InterpolatedString { parts: range }, span),
            )
        }
        TokenKind::LBracket => {
            cur.bump();
            let mut items = Vec::new();
            if cur.peek_kind() != Some(TokenKind::RBracket) {
                loop {
                    items.push(try_hand_lower_expr(ctx, cur, 0)?);
                    if cur.eat(TokenKind::Comma) {
                        continue;
                    }
                    break;
                }
            }
            let close = cur.expect(TokenKind::RBracket)?;
            let range = ctx.pool.alloc_expr_list(&items);
            Some(ctx.pool.alloc_expr(
                ExprKind::Array { items: range },
                ctx.span(start, close.start + close.len),
            ))
        }
        TokenKind::KwIf => parse_if_expr(ctx, cur, start),
        TokenKind::KwMatch => parse_match_expr(ctx, cur, start),
        TokenKind::KwAsync => {
            cur.bump();
            let block = parse_block_tokens(ctx, cur)?;
            let end = block.span.end;
            let block_id = ctx.pool.alloc_block(block);
            Some(ctx.pool.alloc_expr(
                ExprKind::AsyncBlock { block: block_id },
                ctx.span(start, end),
            ))
        }
        TokenKind::KwUnsafe => {
            cur.bump();
            let block = parse_block_tokens(ctx, cur)?;
            let end = block.span.end;
            let block_id = ctx.pool.alloc_block(block);
            Some(ctx.pool.alloc_expr(
                ExprKind::UnsafeBlock { block: block_id },
                ctx.span(start, end),
            ))
        }
        _ => None,
    }
}

fn parse_type_led(ctx: &mut HandCtx<'_>, cur: &mut Cursor<'_>, start: u32) -> Option<ExprId> {
    let ty = parse_type(ctx, cur)?;
    if cur.eat(TokenKind::LBrace) {
        let mut fields = Vec::new();
        if cur.peek_kind() != Some(TokenKind::RBrace) {
            loop {
                let name_tok = cur.expect(TokenKind::IdentValue)?;
                let name = SmolStr::new(ctx.text(name_tok)?);
                let fstart = name_tok.start;
                cur.expect(TokenKind::Colon)?;
                let value = try_hand_lower_expr(ctx, cur, 0)?;
                let fend = ctx.pool.expr_span(value).end;
                let init_id = ctx.pool.alloc_field_init(FieldInit {
                    span: ctx.span(fstart, fend),
                    name,
                    value,
                });
                fields.push(init_id);
                if !cur.eat(TokenKind::Comma) {
                    break;
                }
                if cur.peek_kind() == Some(TokenKind::RBrace) {
                    break;
                }
            }
        }
        let close = cur.expect(TokenKind::RBrace)?;
        let range = ctx.pool.alloc_field_init_list(&fields);
        return Some(ctx.pool.alloc_expr(
            ExprKind::StructLiteral { ty, fields: range },
            ctx.span(start, close.start + close.len),
        ));
    }
    // Type.member
    let named_info = match ctx.pool.type_expr(ty) {
        TypeExpr::Named { name, args, .. } if args.is_empty() => Some(name.clone()),
        TypeExpr::Primitive { span, name } => Some(TypeName {
            span: *span,
            path: smallvec::smallvec![name.clone()],
        }),
        _ => None,
    };
    if let Some(type_name) = named_info
        && cur.eat(TokenKind::Dot)
    {
        let mem = cur.peek()?;
        if !matches!(mem.kind, TokenKind::IdentValue | TokenKind::IdentType) {
            return None;
        }
        let member = SmolStr::new(ctx.text(mem)?);
        cur.bump();
        return Some(ctx.pool.alloc_expr(
            ExprKind::TypePath { type_name, member },
            ctx.span(start, mem.start + mem.len),
        ));
    }
    None
}

fn parse_if_expr(ctx: &mut HandCtx<'_>, cur: &mut Cursor<'_>, start: u32) -> Option<ExprId> {
    cur.expect(TokenKind::KwIf)?;
    // Condition tokens until depth-0 `{` (avoid trailing-block absorption).
    let toks = cur.remaining();
    let mut depth = 0i32;
    let mut brace_at = None;
    for (i, t) in toks.iter().enumerate() {
        if depth == 0 && matches!(t.kind, TokenKind::LBrace) {
            brace_at = Some(i);
            break;
        }
        match t.kind {
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => depth += 1,
            TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                depth = depth.saturating_sub(1);
            }
            _ => {}
        }
    }
    let brace_at = brace_at?;
    if brace_at == 0 {
        return None;
    }
    let mut ccur = Cursor::new(&toks[..brace_at]);
    let cond_expr = try_hand_lower_expr(ctx, &mut ccur, 0)?;
    if !ccur.at_end() {
        return None;
    }
    for _ in 0..brace_at {
        cur.bump();
    }
    let condition = Condition::Expr {
        span: ctx.pool.expr_span(cond_expr),
        expr: cond_expr,
    };
    let then_block = parse_block_tokens(ctx, cur)?;
    let else_block = if cur.eat(TokenKind::KwElse) {
        if cur.peek_kind() == Some(TokenKind::KwIf) {
            let nested_start = cur.peek()?.start;
            let nested = parse_if_expr(ctx, cur, nested_start)?;
            let nested_id = ctx.pool.alloc_stmt(crate::Stmt::Expr {
                span: ctx.pool.expr_span(nested),
                expr: nested,
            });
            crate::Block {
                span: ctx.pool.expr_span(nested),
                statements: vec![nested_id],
            }
        } else {
            parse_block_tokens(ctx, cur)?
        }
    } else {
        return None;
    };
    let then_id = ctx.pool.alloc_block(then_block);
    let else_id = ctx.pool.alloc_block(else_block);
    let end = ctx.pool.block(else_id).span.end;
    Some(ctx.pool.alloc_expr(
        ExprKind::If {
            condition,
            then_block: then_id,
            else_block: else_id,
        },
        ctx.span(start, end),
    ))
}

fn parse_match_expr(ctx: &mut HandCtx<'_>, cur: &mut Cursor<'_>, start: u32) -> Option<ExprId> {
    cur.expect(TokenKind::KwMatch)?;
    // Value stops before `{` (no trailing-block call).
    let toks = cur.remaining();
    let mut depth = 0i32;
    let mut brace_at = None;
    for (i, t) in toks.iter().enumerate() {
        if depth == 0 && matches!(t.kind, TokenKind::LBrace) {
            brace_at = Some(i);
            break;
        }
        match t.kind {
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => depth += 1,
            TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                depth = depth.saturating_sub(1);
            }
            _ => {}
        }
    }
    let brace_at = brace_at?;
    if brace_at == 0 {
        return None;
    }
    let mut vcur = Cursor::new(&toks[..brace_at]);
    let value = try_hand_lower_expr(ctx, &mut vcur, 0)?;
    if !vcur.at_end() {
        return None;
    }
    for _ in 0..brace_at {
        cur.bump();
    }
    cur.expect(TokenKind::LBrace)?;
    let mut arms = Vec::new();
    while cur.peek_kind() != Some(TokenKind::RBrace) && !cur.at_end() {
        cur.skip_semis();
        if cur.peek_kind() == Some(TokenKind::RBrace) {
            break;
        }
        let arm = parse_match_arm(ctx, cur)?;
        arms.push(ctx.pool.alloc_match_arm(arm));
    }
    let close = cur.expect(TokenKind::RBrace)?;
    let range = ctx.pool.alloc_match_arm_list(&arms);
    Some(ctx.pool.alloc_expr(
        ExprKind::Match { value, arms: range },
        ctx.span(start, close.start + close.len),
    ))
}

fn looks_like_lambda(cur: &Cursor<'_>) -> bool {
    let mut depth = 1i32;
    let mut i = 0usize;
    while let Some(t) = cur.peek_at(i) {
        match t.kind {
            TokenKind::LParen => depth += 1,
            TokenKind::RParen => {
                depth -= 1;
                if depth == 0 {
                    return cur
                        .peek_at(i + 1)
                        .is_some_and(|n| matches!(n.kind, TokenKind::FatArrow));
                }
            }
            TokenKind::Eof => return false,
            _ => {}
        }
        i += 1;
        if i > 64 {
            return false;
        }
    }
    false
}

fn parse_lambda(ctx: &mut HandCtx<'_>, cur: &mut Cursor<'_>, start: u32) -> Option<ExprId> {
    let mut params = Vec::new();
    if cur.peek_kind() != Some(TokenKind::RParen) {
        loop {
            let name_tok = cur.expect(TokenKind::IdentValue)?;
            let name = SmolStr::new(ctx.text(name_tok)?);
            let p_start = name_tok.start;
            let ty = if cur.peek_kind().is_some_and(can_start_type_kind) {
                Some(parse_type(ctx, cur)?)
            } else {
                None
            };
            let p_end = ty
                .map(|id| ctx.pool.type_expr_span(id).end)
                .unwrap_or(name_tok.start + name_tok.len);
            params.push(LambdaParam {
                span: ctx.span(p_start, p_end),
                name,
                ty,
            });
            if cur.eat(TokenKind::Comma) {
                continue;
            }
            break;
        }
    }
    cur.expect(TokenKind::RParen)?;
    cur.expect(TokenKind::FatArrow)?;
    let body = if cur.peek_kind() == Some(TokenKind::LBrace) {
        let block = parse_block_tokens(ctx, cur)?;
        let end = block.span.end;
        let body = LambdaBody::Block {
            span: block.span,
            block,
        };
        let param_ids: Vec<_> = params
            .into_iter()
            .map(|p| ctx.pool.alloc_lambda_param(p))
            .collect();
        let params_range = ctx.pool.alloc_lambda_param_list(&param_ids);
        return Some(ctx.pool.alloc_expr(
            ExprKind::Lambda {
                params: params_range,
                body,
            },
            ctx.span(start, end),
        ));
    } else {
        let expr = try_hand_lower_expr(ctx, cur, 0)?;
        LambdaBody::Expr {
            span: ctx.pool.expr_span(expr),
            expr,
        }
    };
    let end = match &body {
        LambdaBody::Expr { span, .. } | LambdaBody::Block { span, .. } => span.end,
    };
    let param_ids: Vec<_> = params
        .into_iter()
        .map(|p| ctx.pool.alloc_lambda_param(p))
        .collect();
    let params_range = ctx.pool.alloc_lambda_param_list(&param_ids);
    Some(ctx.pool.alloc_expr(
        ExprKind::Lambda {
            params: params_range,
            body,
        },
        ctx.span(start, end),
    ))
}

fn parse_string(
    ctx: &mut HandCtx<'_>,
    cur: &mut Cursor<'_>,
    start: u32,
    end_kind: TokenKind,
) -> Option<ExprId> {
    cur.bump(); // start
    let mut parts = Vec::new();
    loop {
        let t = cur.peek()?;
        match t.kind {
            k if k == end_kind => {
                let end_tok = cur.bump()?;
                let range = ctx.pool.alloc_string_part_list(&parts);
                return Some(ctx.pool.alloc_expr(
                    ExprKind::InterpolatedString { parts: range },
                    ctx.span(start, end_tok.start + end_tok.len),
                ));
            }
            TokenKind::StringText | TokenKind::StringEscape => {
                let text = SmolStr::new(ctx.text(t)?);
                let span = ctx.token_span(t);
                cur.bump();
                parts.push(ctx.pool.alloc_string_part(StringPart::Text { span, text }));
            }
            TokenKind::InterpStart => {
                let interp_start = cur.bump()?;
                let expr = try_hand_lower_expr(ctx, cur, 0)?;
                let close = cur.expect(TokenKind::InterpEnd)?;
                let span = ctx.span(interp_start.start, close.start + close.len);
                parts.push(ctx.pool.alloc_string_part(StringPart::Expr { span, expr }));
            }
            _ => {
                let text = SmolStr::new(ctx.text(t).unwrap_or(""));
                let span = ctx.token_span(t);
                cur.bump();
                parts.push(ctx.pool.alloc_string_part(StringPart::Text { span, text }));
            }
        }
    }
}
