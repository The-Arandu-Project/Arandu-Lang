use super::{
    BinaryOp, Block, CatchHandler, FieldInit, LambdaBody, LambdaParam, ParseError, ParseErrorCode,
    Parser, Stmt, StringPart, TokenKind, TypeExpr, TypeName, UnaryOp, is_primitive_type_token,
    merge_text_parts, span_between,
};
use crate::ast::ast_pool::{ExprId, ExprKind};
use smol_str::SmolStr;

impl<'a> Parser<'a> {
    pub(super) fn parse_expr(&mut self, min_bp: u8) -> Result<ExprId, ParseError> {
        // Outermost expression only → one EXPR green node (event sink).
        let wrap = min_bp == 0;
        if wrap {
            self.start_node(crate::syntax::SyntaxKind::EXPR);
        }
        let start = self.mark();
        let result = match self.try_parse_expr(min_bp) {
            Ok(expr) => Ok(expr),
            Err(err) => {
                self.report_error(err);
                // No synchronize_expr yet, to avoid eating too much.
                // It just falls back to ExprKind::Error so parent parses can fail/recover naturally.
                let span = self.span_from_mark(start);
                Ok(self.pool.alloc_expr(ExprKind::Error, span))
            }
        };
        if wrap {
            self.finish_node();
        }
        result
    }

    pub(super) fn try_parse_expr(&mut self, min_bp: u8) -> Result<ExprId, ParseError> {
        let mut left = self.parse_prefix()?;
        loop {
            let next_kind = self.current().kind;
            match next_kind {
                TokenKind::Lt => {
                    if self.looks_like_generic_call_or_block_suffix() {
                        let generic_start = self.pool.expr_span(left);
                        let type_range = self.parse_generic_args()?;
                        let span = span_between(generic_start, self.previous().span(self.file_id));
                        left = self.pool.alloc_expr(
                            ExprKind::Generic {
                                callee: left,
                                args: type_range,
                            },
                            span,
                        );

                        if matches!(self.current().kind, TokenKind::LParen) {
                            left = self.finish_call(left)?;
                        } else if self.allow_block_calls {
                            left = self.finish_trailing_block_call(left)?;
                        } else {
                            return Err(ParseError::new(
                                ParseErrorCode::ExpectedToken,
                                "generic block calls are not valid in this context",
                                self.current(),
                                self.file_id,
                                self.source,
                            ));
                        }
                        continue;
                    }
                    if self.looks_like_bare_generic_args()
                        && self.looks_like_generic_args_boundary()
                    {
                        return Err(ParseError::new(
                            ParseErrorCode::ExpectedToken,
                            "generic arguments in expressions must be followed by call arguments or block",
                            self.current(),
                            self.file_id,
                            self.source,
                        ));
                    }
                }
                TokenKind::LParen => {
                    left = self.finish_call(left)?;
                    continue;
                }
                TokenKind::LBrace if self.allow_block_calls => {
                    left = self.finish_trailing_block_call(left)?;
                    continue;
                }
                TokenKind::LBracket => {
                    let span_start = self.pool.expr_span(left);
                    self.advance();
                    let index = self.parse_expr(0)?;
                    self.expect_kind(TokenKind::RBracket)?;
                    let span = span_between(span_start, self.previous().span(self.file_id));
                    left = self
                        .pool
                        .alloc_expr(ExprKind::Index { base: left, index }, span);
                    continue;
                }
                TokenKind::SafeIndexStart => {
                    self.advance();
                    let span_start = self.pool.expr_span(left);
                    let index = self.parse_expr(0)?;
                    self.expect_kind(TokenKind::RBracket)?;
                    let span = span_between(span_start, self.previous().span(self.file_id));
                    left = self
                        .pool
                        .alloc_expr(ExprKind::SafeIndex { base: left, index }, span);
                    continue;
                }
                TokenKind::Dot => {
                    self.advance();
                    let span_start = self.pool.expr_span(left);
                    let field = self.expect_ident_value()?;
                    let span = span_between(span_start, self.previous().span(self.file_id));
                    left = self
                        .pool
                        .alloc_expr(ExprKind::Field { base: left, field }, span);
                    continue;
                }
                TokenKind::SafeDot => {
                    self.advance();
                    let span_start = self.pool.expr_span(left);
                    let field = self.expect_ident_value()?;
                    let span = span_between(span_start, self.previous().span(self.file_id));
                    left = self
                        .pool
                        .alloc_expr(ExprKind::SafeField { base: left, field }, span);
                    continue;
                }
                TokenKind::Question => {
                    self.advance();
                    let span_start = self.pool.expr_span(left);
                    let span = span_between(span_start, self.previous().span(self.file_id));
                    left = self.pool.alloc_expr(ExprKind::Try { expr: left }, span);
                    continue;
                }
                TokenKind::KwCatch => {
                    let left_bp = 10;
                    let right_bp = 10;
                    if left_bp < min_bp {
                        break;
                    }
                    let span_start = self.pool.expr_span(left);
                    self.advance();
                    let handler = if self.eat_kind(TokenKind::Pipe) {
                        let handler_start = self.pos.saturating_sub(1);
                        let error = self.expect_ident_value()?;
                        self.expect_kind(TokenKind::Pipe)?;
                        let block = self.parse_block()?;
                        let catch_handler = CatchHandler::Block {
                            span: self.span_from_mark(handler_start),
                            error,
                            block,
                        };
                        self.pool.alloc_catch_handler(catch_handler)
                    } else {
                        let handler_start = self.mark();
                        let expr = self.parse_expr(right_bp)?;
                        let catch_handler = CatchHandler::Expr {
                            span: self.span_from_mark(handler_start),
                            expr,
                        };
                        self.pool.alloc_catch_handler(catch_handler)
                    };
                    let span = span_between(span_start, self.previous().span(self.file_id));
                    left = self.pool.alloc_expr(
                        ExprKind::Catch {
                            expr: left,
                            handler,
                        },
                        span,
                    );
                    continue;
                }
                TokenKind::KwAs => {
                    let left_bp = 140;
                    if left_bp < min_bp {
                        break;
                    }
                    let span_start = self.pool.expr_span(left);
                    self.advance();
                    let ty = self.parse_type()?;
                    let span = span_between(span_start, self.pool.type_expr_span(ty));
                    left = self
                        .pool
                        .alloc_expr(ExprKind::Cast { expr: left, ty }, span);
                    continue;
                }
                _ => {}
            }

            let Some((op, left_bp, right_bp)) = self.current_binary() else {
                break;
            };
            if left_bp < min_bp {
                break;
            }
            if matches!(op, BinaryOp::RangeExclusive | BinaryOp::RangeInclusive) {
                let left_kind = self.pool.expr(left);
                if matches!(
                    left_kind,
                    ExprKind::Binary {
                        op: BinaryOp::RangeExclusive | BinaryOp::RangeInclusive,
                        ..
                    }
                ) {
                    return Err(ParseError::new(
                        ParseErrorCode::ExpectedToken,
                        "chained ranges require parentheses",
                        self.current(),
                        self.file_id,
                        self.source,
                    ));
                }
            }
            let span_start = self.pool.expr_span(left);
            self.advance();
            let right = self.parse_expr(right_bp)?;
            let span = span_between(span_start, self.pool.expr_span(right));
            left = if op == BinaryOp::NullCoalesce {
                self.pool
                    .alloc_expr(ExprKind::NullCoalesce { left, right }, span)
            } else {
                self.pool
                    .alloc_expr(ExprKind::Binary { op, left, right }, span)
            };
        }
        Ok(left)
    }

    pub(super) fn parse_prefix(&mut self) -> Result<ExprId, ParseError> {
        let start = self.mark();
        match &self.current().kind {
            TokenKind::Minus => {
                self.advance();
                let expr = self.parse_expr(150)?;
                let span = self.span_from_mark(start);
                Ok(self.pool.alloc_expr(
                    ExprKind::Unary {
                        op: UnaryOp::Neg,
                        expr,
                    },
                    span,
                ))
            }
            TokenKind::Bang => {
                self.advance();
                let expr = self.parse_expr(150)?;
                let span = self.span_from_mark(start);
                Ok(self.pool.alloc_expr(
                    ExprKind::Unary {
                        op: UnaryOp::Not,
                        expr,
                    },
                    span,
                ))
            }
            TokenKind::Tilde => {
                self.advance();
                let expr = self.parse_expr(150)?;
                let span = self.span_from_mark(start);
                Ok(self.pool.alloc_expr(
                    ExprKind::Unary {
                        op: UnaryOp::BitNot,
                        expr,
                    },
                    span,
                ))
            }
            TokenKind::KwAwait => {
                self.advance();
                let expr = self.parse_expr(150)?;
                let span = self.span_from_mark(start);
                Ok(self.pool.alloc_expr(
                    ExprKind::Unary {
                        op: UnaryOp::Await,
                        expr,
                    },
                    span,
                ))
            }
            TokenKind::Amp => {
                self.advance();
                let is_mut = self.eat_name("KW_MUT");
                let expr = self.parse_expr(150)?;
                let span = self.span_from_mark(start);
                Ok(self.pool.alloc_expr(
                    ExprKind::Unary {
                        op: if is_mut {
                            UnaryOp::RefMut
                        } else {
                            UnaryOp::Ref
                        },
                        expr,
                    },
                    span,
                ))
            }
            TokenKind::Star => {
                self.advance();
                let expr = self.parse_expr(150)?;
                let span = self.span_from_mark(start);
                Ok(self.pool.alloc_expr(
                    ExprKind::Unary {
                        op: UnaryOp::Deref,
                        expr,
                    },
                    span,
                ))
            }
            TokenKind::KwAsync => {
                self.advance();
                let block = self.parse_block()?;
                let block_id = self.pool.alloc_block(block);
                let span = self.span_from_mark(start);
                Ok(self
                    .pool
                    .alloc_expr(ExprKind::AsyncBlock { block: block_id }, span))
            }
            TokenKind::KwUnsafe => {
                self.advance();
                let block = self.parse_block()?;
                let block_id = self.pool.alloc_block(block);
                let span = self.span_from_mark(start);
                Ok(self
                    .pool
                    .alloc_expr(ExprKind::UnsafeBlock { block: block_id }, span))
            }
            TokenKind::KwIf => self.parse_if_expr(),
            TokenKind::KwMatch => self.parse_match_expr(),
            TokenKind::KwSelf => {
                self.advance();
                let span = self.span_from_mark(start);
                Ok(self.pool.alloc_expr(
                    ExprKind::Path {
                        path: vec![SmolStr::new("self")].into(),
                    },
                    span,
                ))
            }
            TokenKind::IdentValue => {
                if self
                    .tokens
                    .get(self.pos + 1)
                    .is_some_and(|t| matches!(t.kind, TokenKind::Dot))
                    && self
                        .tokens
                        .get(self.pos + 2)
                        .is_some_and(|t| matches!(t.kind, TokenKind::IdentType))
                {
                    self.parse_type_led_expr()
                } else {
                    let name = self.expect_ident_value()?;
                    let span = self.span_from_mark(start);
                    Ok(self.pool.alloc_expr(
                        ExprKind::Path {
                            path: vec![name].into(),
                        },
                        span,
                    ))
                }
            }
            kind if matches!(kind, TokenKind::IdentType) || is_primitive_type_token(kind) => {
                self.parse_type_led_expr()
            }
            TokenKind::IntDec | TokenKind::IntHex | TokenKind::IntBin | TokenKind::IntOct => {
                let value = SmolStr::new(self.current_text());
                self.advance();
                let span = self.span_from_mark(start);
                Ok(self.pool.alloc_expr(ExprKind::Int { value }, span))
            }
            TokenKind::Float => {
                let value = SmolStr::new(self.current_text());
                self.advance();
                let span = self.span_from_mark(start);
                Ok(self.pool.alloc_expr(ExprKind::Float { value }, span))
            }
            TokenKind::BoolTrue => {
                self.advance();
                let span = self.span_from_mark(start);
                Ok(self.pool.alloc_expr(ExprKind::Bool { value: true }, span))
            }
            TokenKind::BoolFalse => {
                self.advance();
                let span = self.span_from_mark(start);
                Ok(self.pool.alloc_expr(ExprKind::Bool { value: false }, span))
            }
            TokenKind::Char => {
                let value = SmolStr::new(self.current().char_content(self.source));
                self.advance();
                let span = self.span_from_mark(start);
                Ok(self.pool.alloc_expr(ExprKind::Char { value }, span))
            }
            TokenKind::Nil => {
                self.advance();
                let span = self.span_from_mark(start);
                Ok(self.pool.alloc_expr(ExprKind::Nil, span))
            }
            TokenKind::StringStart => self.parse_string_like("STRING_START", "STRING_END"),
            TokenKind::MultilineStringStart => {
                self.parse_string_like("MULTILINE_STRING_START", "MULTILINE_STRING_END")
            }
            TokenKind::RawString => {
                let value = SmolStr::new(self.current().raw_string_content(self.source));
                self.advance();
                let span = self.span_from_mark(start);
                let text_part = StringPart::Text { span, text: value };
                let part_id = self.pool.alloc_string_part(text_part);
                let range = self.pool.alloc_string_part_list(&[part_id]);
                Ok(self
                    .pool
                    .alloc_expr(ExprKind::InterpolatedString { parts: range }, span))
            }
            TokenKind::LParen => {
                let mut is_lambda = false;
                {
                    let checkpoint = self.checkpoint();
                    if self.eat_name("LPAREN") {
                        let mut params_ok = true;
                        if !self.at_kind_name("RPAREN") {
                            loop {
                                if !matches!(self.current().kind, TokenKind::IdentValue) {
                                    params_ok = false;
                                    break;
                                }
                                self.advance();
                                if self.can_start_type() && self.parse_type().is_err() {
                                    params_ok = false;
                                    break;
                                }
                                if !self.eat_name("COMMA") {
                                    break;
                                }
                                if self.at_kind_name("RPAREN") {
                                    break;
                                }
                            }
                        }
                        if params_ok && self.eat_name("RPAREN") && self.at_kind_name("FAT_ARROW") {
                            is_lambda = true;
                        }
                    }
                    checkpoint.rollback(self);
                }

                if is_lambda {
                    self.parse_lambda()
                } else {
                    self.advance();
                    let expr = self.parse_expr(0)?;
                    self.expect_name("RPAREN")?;
                    let span = self.span_from_mark(start);
                    Ok(self.pool.alloc_expr(ExprKind::Group { expr }, span))
                }
            }
            TokenKind::LBracket => self.parse_array(),
            _ => Err(ParseError::new(
                ParseErrorCode::ExpectedExpression,
                "expected expression",
                self.current(),
                self.file_id,
                self.source,
            )),
        }
    }

    pub(super) fn finish_call(&mut self, callee: ExprId) -> Result<ExprId, ParseError> {
        let span_start = self.pool.expr_span(callee);
        self.expect_name("LPAREN")?;
        let args = self.parse_arguments()?;
        self.expect_name("RPAREN")?;
        // Trailing-block call `f(args) { ... }` only when allowed.
        // Critical: `match f() { arms }` uses parse_expr_without_block_calls so the
        // match's `{` must not be swallowed as a trailing block (else arms parse as
        // statements → "expected type-qualified..." on `Some(x)` / `Ok(x)`).
        let trailing_block = if self.allow_block_calls && self.at_kind_name("LBRACE") {
            let block = self.parse_block()?;
            Some(self.pool.alloc_block(block))
        } else {
            None
        };
        let end = trailing_block.as_ref().map_or_else(
            || self.previous().span(self.file_id),
            |block_id| self.pool.block(*block_id).span,
        );
        let range = self.pool.alloc_expr_list(&args);
        Ok(self.pool.alloc_expr(
            ExprKind::Call {
                callee,
                args: range,
                trailing_block,
            },
            span_between(span_start, end),
        ))
    }

    pub(super) fn finish_trailing_block_call(
        &mut self,
        callee: ExprId,
    ) -> Result<ExprId, ParseError> {
        let span_start = self.pool.expr_span(callee);
        let trailing_block = self.parse_block()?;
        let block_span = trailing_block.span;
        let block_id = self.pool.alloc_block(trailing_block);
        let range = self.pool.alloc_expr_list(&[]);
        Ok(self.pool.alloc_expr(
            ExprKind::Call {
                callee,
                args: range,
                trailing_block: Some(block_id),
            },
            span_between(span_start, block_span),
        ))
    }

    pub(super) fn parse_if_expr(&mut self) -> Result<ExprId, ParseError> {
        let start = self.mark();
        self.expect_name("KW_IF")?;
        let condition = self.parse_condition()?;
        let then_block = self.parse_block()?;
        self.expect_name("KW_ELSE")?;
        let else_block = if self.at_kind_name("KW_IF") {
            let nested = self.parse_if_expr()?;
            let nested_span = self.pool.expr_span(nested);
            Block {
                span: nested_span,
                statements: vec![self.pool.alloc_stmt(Stmt::Expr {
                    span: nested_span,
                    expr: nested,
                })],
            }
        } else {
            self.parse_block()?
        };
        let then_id = self.pool.alloc_block(then_block);
        let else_id = self.pool.alloc_block(else_block);
        let span = self.span_from_mark(start);
        Ok(self.pool.alloc_expr(
            ExprKind::If {
                condition,
                then_block: then_id,
                else_block: else_id,
            },
            span,
        ))
    }

    pub(super) fn parse_array(&mut self) -> Result<ExprId, ParseError> {
        let start = self.mark();
        self.expect_name("LBRACKET")?;
        let mut items = Vec::new();
        if !self.at_kind_name("RBRACKET") {
            loop {
                items.push(self.parse_expr(0)?);
                if !self.eat_name("COMMA") {
                    break;
                }
                if self.at_kind_name("RBRACKET") {
                    break;
                }
            }
        }
        self.expect_name("RBRACKET")?;
        let range = self.pool.alloc_expr_list(&items);
        let span = self.span_from_mark(start);
        Ok(self.pool.alloc_expr(ExprKind::Array { items: range }, span))
    }

    pub(super) fn parse_lambda(&mut self) -> Result<ExprId, ParseError> {
        let start = self.mark();
        self.expect_name("LPAREN")?;
        let mut params = Vec::new();
        if !self.at_kind_name("RPAREN") {
            loop {
                let param_start = self.mark();
                let name = self.expect_ident_value()?;
                let ty = if self.can_start_type() {
                    Some(self.parse_type()?)
                } else {
                    None
                };
                let param = LambdaParam {
                    span: self.span_from_mark(param_start),
                    name,
                    ty,
                };
                let param_id = self.pool.alloc_lambda_param(param);
                params.push(param_id);
                if !self.eat_name("COMMA") {
                    break;
                }
                if self.at_kind_name("RPAREN") {
                    break;
                }
            }
        }
        self.expect_name("RPAREN")?;
        self.expect_name("FAT_ARROW")?;
        let body = if self.at_kind_name("LBRACE") {
            let block = self.parse_block()?;
            LambdaBody::Block {
                span: block.span,
                block,
            }
        } else {
            let body_start = self.mark();
            let expr = self.parse_expr(0)?;
            LambdaBody::Expr {
                span: self.span_from_mark(body_start),
                expr,
            }
        };
        let range = self.pool.alloc_lambda_param_list(&params);
        let span = self.span_from_mark(start);
        Ok(self.pool.alloc_expr(
            ExprKind::Lambda {
                params: range,
                body,
            },
            span,
        ))
    }

    pub(super) fn parse_type_led_expr(&mut self) -> Result<ExprId, ParseError> {
        let start = self.mark();
        let ty = self.parse_type()?;
        if self.eat_name("LBRACE") {
            let mut fields = Vec::new();
            if !self.at_kind_name("RBRACE") {
                loop {
                    let field_start = self.mark();
                    let name = self.expect_ident_value()?;
                    self.expect_name("COLON")?;
                    let value = self.parse_expr(0)?;
                    let init = FieldInit {
                        span: self.span_from_mark(field_start),
                        name,
                        value,
                    };
                    let init_id = self.pool.alloc_field_init(init);
                    fields.push(init_id);
                    if !self.eat_name("COMMA") {
                        break;
                    }
                    if self.at_kind_name("RBRACE") {
                        break;
                    }
                }
            }
            self.expect_name("RBRACE")?;
            let range = self.pool.alloc_field_init_list(&fields);
            let type_id = ty;
            let span = self.span_from_mark(start);
            return Ok(self.pool.alloc_expr(
                ExprKind::StructLiteral {
                    ty: type_id,
                    fields: range,
                },
                span,
            ));
        }
        let named_info = match self.pool.type_expr(ty) {
            TypeExpr::Named { name, args, .. } if args.is_empty() => Some(name.clone()),
            TypeExpr::Primitive { span, name } => Some(TypeName {
                span: *span,
                path: smallvec::smallvec![name.clone()],
            }),
            _ => None,
        };
        if let Some(type_name) = named_info
            && self.eat_name("DOT")
        {
            let member = self.expect_name_like()?;
            let span = self.span_from_mark(start);
            return Ok(self
                .pool
                .alloc_expr(ExprKind::TypePath { type_name, member }, span));
        }
        Err(ParseError::new(
            ParseErrorCode::ExpectedExpression,
            "expected type-qualified expression or struct literal",
            self.current(),
            self.file_id,
            self.source,
        ))
    }

    pub(super) fn parse_match_expr(&mut self) -> Result<ExprId, ParseError> {
        let start = self.mark();
        self.expect_name("KW_MATCH")?;
        let value = self.parse_expr_without_block_calls(0)?;
        self.expect_name("LBRACE")?;
        let mut arms = Vec::new();
        while !self.at_kind_name("RBRACE") {
            self.skip_semicolons();
            if self.at_kind_name("RBRACE") {
                break;
            }
            let arm = self.parse_match_arm()?;
            let arm_id = self.pool.alloc_match_arm(arm);
            arms.push(arm_id);
        }
        self.expect_name("RBRACE")?;
        let range = self.pool.alloc_match_arm_list(&arms);
        let span = self.span_from_mark(start);
        Ok(self
            .pool
            .alloc_expr(ExprKind::Match { value, arms: range }, span))
    }

    pub(super) fn parse_string_like(
        &mut self,
        start_name: &str,
        end_name: &str,
    ) -> Result<ExprId, ParseError> {
        let start = self.mark();
        self.expect_name(start_name)?;
        let mut parts = Vec::new();
        while !self.at_kind_name(end_name) {
            match &self.current().kind {
                TokenKind::StringText | TokenKind::StringEscape => {
                    let span = self.current().span(self.file_id);
                    let text = SmolStr::new(self.current_text());
                    self.advance();
                    parts.push(StringPart::Text { span, text });
                }
                TokenKind::InterpStart => {
                    let part_start = self.mark();
                    self.advance();
                    let expr = self.parse_expr(0)?;
                    self.expect_name("INTERP_END")?;
                    parts.push(StringPart::Expr {
                        span: self.span_from_mark(part_start),
                        expr,
                    });
                }
                _ => {
                    return Err(ParseError::new(
                        ParseErrorCode::ExpectedExpression,
                        "expected string part",
                        self.current(),
                        self.file_id,
                        self.source,
                    ));
                }
            }
        }
        self.expect_name(end_name)?;
        let merged = merge_text_parts(parts);
        let mut part_ids = Vec::new();
        for p in merged {
            part_ids.push(self.pool.alloc_string_part(p));
        }
        let range = self.pool.alloc_string_part_list(&part_ids);
        let span = self.span_from_mark(start);
        Ok(self
            .pool
            .alloc_expr(ExprKind::InterpolatedString { parts: range }, span))
    }

    pub(super) fn parse_arguments(&mut self) -> Result<Vec<ExprId>, ParseError> {
        let mut args = Vec::new();
        if self.at_kind_name("RPAREN") {
            return Ok(args);
        }
        loop {
            args.push(self.parse_expr(0)?);
            if !self.eat_name("COMMA") {
                break;
            }
            if self.at_kind_name("RPAREN") {
                break;
            }
        }
        Ok(args)
    }
}

#[derive(Clone, Copy)]
struct BinaryOpInfo {
    op: BinaryOp,
    bp: u8,
}

const BINARY_OP_TABLE: [BinaryOpInfo; 21] = [
    BinaryOpInfo {
        op: BinaryOp::NullCoalesce,
        bp: 20,
    }, // 0
    BinaryOpInfo {
        op: BinaryOp::Or,
        bp: 30,
    }, // 1
    BinaryOpInfo {
        op: BinaryOp::And,
        bp: 40,
    }, // 2
    BinaryOpInfo {
        op: BinaryOp::Equal,
        bp: 50,
    }, // 3
    BinaryOpInfo {
        op: BinaryOp::NotEqual,
        bp: 50,
    }, // 4
    BinaryOpInfo {
        op: BinaryOp::Lt,
        bp: 60,
    }, // 5
    BinaryOpInfo {
        op: BinaryOp::Gt,
        bp: 60,
    }, // 6
    BinaryOpInfo {
        op: BinaryOp::LtEqual,
        bp: 60,
    }, // 7
    BinaryOpInfo {
        op: BinaryOp::GtEqual,
        bp: 60,
    }, // 8
    BinaryOpInfo {
        op: BinaryOp::RangeExclusive,
        bp: 70,
    }, // 9
    BinaryOpInfo {
        op: BinaryOp::RangeInclusive,
        bp: 70,
    }, // 10
    BinaryOpInfo {
        op: BinaryOp::BitOr,
        bp: 80,
    }, // 11
    BinaryOpInfo {
        op: BinaryOp::BitXor,
        bp: 90,
    }, // 12
    BinaryOpInfo {
        op: BinaryOp::BitAnd,
        bp: 100,
    }, // 13
    BinaryOpInfo {
        op: BinaryOp::ShiftLeft,
        bp: 110,
    }, // 14
    BinaryOpInfo {
        op: BinaryOp::ShiftRight,
        bp: 110,
    }, // 15
    BinaryOpInfo {
        op: BinaryOp::Add,
        bp: 120,
    }, // 16
    BinaryOpInfo {
        op: BinaryOp::Sub,
        bp: 120,
    }, // 17
    BinaryOpInfo {
        op: BinaryOp::Mul,
        bp: 130,
    }, // 18
    BinaryOpInfo {
        op: BinaryOp::Div,
        bp: 130,
    }, // 19
    BinaryOpInfo {
        op: BinaryOp::Mod,
        bp: 130,
    }, // 20
];

const fn token_kind_index(kind: &TokenKind) -> usize {
    match kind {
        TokenKind::NullCoalesce => 0,
        TokenKind::LogicalOr => 1,
        TokenKind::LogicalAnd => 2,
        TokenKind::EqualEqual => 3,
        TokenKind::BangEqual => 4,
        TokenKind::Lt => 5,
        TokenKind::Gt => 6,
        TokenKind::LtEqual => 7,
        TokenKind::GtEqual => 8,
        TokenKind::RangeExclusive => 9,
        TokenKind::RangeInclusive => 10,
        TokenKind::Pipe => 11,
        TokenKind::Caret => 12,
        TokenKind::Amp => 13,
        TokenKind::ShiftLeft => 14,
        TokenKind::ShiftRight => 15,
        TokenKind::Plus => 16,
        TokenKind::Minus => 17,
        TokenKind::Star => 18,
        TokenKind::Slash => 19,
        TokenKind::Percent => 20,
        _ => 255,
    }
}

impl<'a> Parser<'a> {
    pub(super) fn current_binary(&self) -> Option<(BinaryOp, u8, u8)> {
        let idx = token_kind_index(&self.current().kind);
        if idx < 21 {
            let info = BINARY_OP_TABLE[idx];
            Some((info.op, info.bp, info.bp + 1))
        } else {
            None
        }
    }
}
