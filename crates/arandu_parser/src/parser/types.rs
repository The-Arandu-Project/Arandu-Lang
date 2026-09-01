use super::{
    GenericParam, Ownership, Param, ParseError, ParseErrorCode, Parser, ResultType, TokenKind,
    TypeExpr, TypeName, WhereItem, is_type_token, primitive_type_name,
};
use crate::{IndexRange, TypeExprId};

fn type_expr_is_err_slot(ty_id: TypeExprId, pool: &crate::ast::ast_pool::AstPool) -> bool {
    match pool.type_expr(ty_id) {
        TypeExpr::Nullable { inner, .. } => type_expr_is_err_slot(*inner, pool),
        TypeExpr::Primitive { name, .. } => name == "Err",
        TypeExpr::Named { name, args, .. } => {
            args.is_empty() && name.path.len() == 1 && name.path[0] == "Err"
        }
        _ => false,
    }
}

fn result_type_must_use_result_generic(
    result: &ResultType,
    pool: &crate::ast::ast_pool::AstPool,
) -> bool {
    match result {
        ResultType::Single { ty, .. } => type_expr_is_err_slot(*ty, pool),
        ResultType::Multi { types, .. } => {
            let list = pool.type_expr_list(*types);
            list.len() == 2 && type_expr_is_err_slot(list[1], pool)
        }
    }
}

use smallvec::SmallVec;
use smol_str::SmolStr;

impl<'a> Parser<'a> {
    pub(super) fn parse_generic_params(
        &mut self,
    ) -> Result<SmallVec<[GenericParam; 2]>, ParseError> {
        if !self.eat_name("LT") {
            return Ok(SmallVec::new());
        }
        let params_vec = self.parse_comma_separated_list("GT", 1, |parser| {
            let start = parser.mark();
            let name = parser.expect_ident_type()?;
            let constraints = if parser.eat_name("COLON") {
                parser.parse_constraint_list()?.into()
            } else {
                SmallVec::new()
            };
            // T2.1: `T = DefaultType` after optional constraints.
            let default = if parser.eat_name("EQUAL") {
                Some(parser.parse_type()?)
            } else {
                None
            };
            Ok(GenericParam {
                span: parser.span_from_mark(start),
                name,
                constraints,
                default,
            })
        })?;
        self.expect_name("GT")?;
        Ok(params_vec.into())
    }

    pub(super) fn parse_generic_args(&mut self) -> Result<IndexRange, ParseError> {
        self.expect_name("LT")?;
        let args = self.parse_comma_separated_list("GT", 1, super::Parser::parse_type)?;
        self.expect_name("GT")?;
        let range = self.pool.alloc_type_expr_list(&args);
        Ok(range)
    }

    pub(super) fn parse_where_clause(
        &mut self,
        end_name: &str,
    ) -> Result<SmallVec<[WhereItem; 2]>, ParseError> {
        if !self.eat_name("KW_WHERE") {
            return Ok(SmallVec::new());
        }
        let mut items = SmallVec::new();
        loop {
            let start = self.mark();
            let name = self.expect_ident_type()?;
            self.expect_name("COLON")?;
            let constraints = self.parse_constraint_list()?.into();
            items.push(WhereItem {
                span: self.span_from_mark(start),
                name,
                constraints,
            });
            if !self.eat_name("COMMA") {
                break;
            }
            if self.at_kind_name(end_name) {
                break;
            }
        }
        Ok(items)
    }

    pub(super) fn parse_constraint_list(&mut self) -> Result<Vec<TypeName>, ParseError> {
        let mut constraints = vec![self.parse_type_name()?];
        while self.eat_name("PLUS") {
            constraints.push(self.parse_type_name()?);
        }
        Ok(constraints)
    }

    fn parse_ownership(&mut self) -> Option<Ownership> {
        if self.eat_name("KW_OWN") {
            Some(Ownership::Own)
        } else if self.eat_name("KW_MUT") {
            Some(Ownership::Mut)
        } else if self.eat_name("KW_SHARED") {
            Some(Ownership::Shared)
        } else {
            None
        }
    }

    pub(super) fn parse_params(
        &mut self,
        method_receiver: Option<&TypeName>,
    ) -> Result<Vec<Param>, ParseError> {
        let params = self.parse_comma_separated_list("RPAREN", 0, |parser| {
            let start = parser.mark();
            let attrs = parser.parse_attributes()?;
            let ownership = parser.parse_ownership();
            let name = if parser.eat_name("KW_SELF") {
                SmolStr::new("self")
            } else {
                parser.expect_ident_value()?
            };
            let is_receiver = name == "self";
            let ty = if is_receiver {
                if parser.eat_name("COLON") {
                    parser.parse_type()?
                } else {
                    let receiver = method_receiver.ok_or_else(|| {
                        ParseError::new(
                            ParseErrorCode::ExpectedType,
                            "receiver parameter 'self' requires an explicit type here",
                            parser.current(),
                            parser.file_id,
                            parser.source,
                        )
                    })?;
                    let empty_args = parser.pool.alloc_type_expr_list(&[]);
                    parser.pool.alloc_type_expr(TypeExpr::Named {
                        span: receiver.span,
                        name: receiver.clone(),
                        args: empty_args,
                    })
                }
            } else {
                parser.expect_name("COLON")?;
                parser.parse_type()?
            };
            let is_variadic = parser.eat_name("ELLIPSIS");
            Ok(Param {
                span: parser.span_from_mark(start),
                attrs: attrs.into(),
                ownership,
                name,
                ty,
                is_variadic,
                is_receiver,
            })
        })?;
        Ok(params)
    }

    pub(super) fn parse_result_type(&mut self) -> Result<ResultType, ParseError> {
        let start = self.mark();
        let result = if self.eat_name("LPAREN") {
            let types = self.parse_comma_separated_list("RPAREN", 2, super::Parser::parse_type)?;
            self.expect_name("RPAREN")?;
            let range = self.pool.alloc_type_expr_list(&types);
            ResultType::Multi {
                span: self.span_from_mark(start),
                types: range,
            }
        } else {
            let ty = self.parse_type()?;
            ResultType::Single {
                span: self.span_from_mark(start),
                ty,
            }
        };
        if result_type_must_use_result_generic(&result, &self.pool) {
            let token = *self.current();
            return Err(ParseError::new(
                ParseErrorCode::InvalidResultReturn,
                "function result must use `Result<T, E>` syntax",
                &token,
                self.file_id,
                self.source,
            ));
        }
        Ok(result)
    }

    pub(super) fn parse_type(&mut self) -> Result<TypeExprId, ParseError> {
        let start = self.mark();
        let mut ty = self.parse_type_primary()?;
        if self.eat_name("QUESTION") {
            let span = self.span_from_mark(start);
            ty = self
                .pool
                .alloc_type_expr(TypeExpr::Nullable { span, inner: ty });
        }
        Ok(ty)
    }

    pub(super) fn parse_type_primary(&mut self) -> Result<TypeExprId, ParseError> {
        let start = self.mark();
        // Canonical ownership spelling: `own T`, `ref T`, `mut ref T`.
        // `own` is semantically the default and therefore lowers directly to T.
        if self.eat_name("KW_OWN") {
            return self.parse_type();
        }
        if self.eat_name("KW_REF") {
            let inner = self.parse_type()?;
            let span = self.span_from_mark(start);
            return Ok(self.pool.alloc_type_expr(TypeExpr::Ref { span, inner }));
        }
        if self.eat_name("KW_MUT") {
            self.expect_name("KW_REF")?;
            let inner = self.parse_type()?;
            let span = self.span_from_mark(start);
            return Ok(self.pool.alloc_type_expr(TypeExpr::RefMut { span, inner }));
        }
        // `&mut T` / `&T` (F2.0 safe references)
        if self.eat_name("AMP") {
            let is_mut = self.eat_name("KW_MUT");
            let inner = self.parse_type()?;
            let span = self.span_from_mark(start);
            return Ok(self.pool.alloc_type_expr(if is_mut {
                TypeExpr::RefMut { span, inner }
            } else {
                TypeExpr::Ref { span, inner }
            }));
        }
        if self.eat_name("KW_PTR") {
            self.expect_name("LBRACKET")?;
            let inner = self.parse_type()?;
            self.expect_name("RBRACKET")?;
            let span = self.span_from_mark(start);
            return Ok(self.pool.alloc_type_expr(TypeExpr::Pointer { span, inner }));
        }
        if self.eat_name("LBRACKET") {
            if self.eat_name("RBRACKET") {
                let inner = self.parse_type_primary()?;
                let span = self.span_from_mark(start);
                return Ok(self.pool.alloc_type_expr(TypeExpr::Slice { span, inner }));
            }
            let size = match &self.current().kind {
                TokenKind::IntDec => {
                    let value = SmolStr::new(self.current_text());
                    self.advance();
                    value
                }
                _ => {
                    return Err(ParseError::new(
                        ParseErrorCode::ExpectedToken,
                        "expected array size",
                        self.current(),
                        self.file_id,
                        self.source,
                    ));
                }
            };
            self.expect_name("RBRACKET")?;
            let elem = self.parse_type_primary()?;
            let span = self.span_from_mark(start);
            return Ok(self
                .pool
                .alloc_type_expr(TypeExpr::Array { span, size, elem }));
        }
        if self.eat_name("KW_FUNC") {
            self.expect_name("LPAREN")?;
            let mut params = Vec::new();
            if !self.at_kind_name("RPAREN") {
                loop {
                    params.push(self.parse_type()?);
                    if !self.eat_name("COMMA") {
                        break;
                    }
                    if self.at_kind_name("RPAREN") {
                        break;
                    }
                }
            }
            self.expect_name("RPAREN")?;
            let result = if self.can_start_type() || self.at_kind_name("LPAREN") {
                Some(self.parse_result_type()?)
            } else {
                None
            };
            let span = self.span_from_mark(start);
            let params_range = self.pool.alloc_type_expr_list(&params);
            return Ok(self.pool.alloc_type_expr(TypeExpr::Func {
                span,
                params: params_range,
                result,
            }));
        }
        if self.eat_name("LPAREN") {
            let mut params = Vec::new();
            let mut saw_comma = false;
            if !self.at_kind_name("RPAREN") {
                loop {
                    params.push(self.parse_type()?);
                    if !self.eat_name("COMMA") {
                        break;
                    }
                    saw_comma = true;
                    if self.at_kind_name("RPAREN") {
                        break;
                    }
                }
            }
            self.expect_name("RPAREN")?;
            if self.eat_name("ARROW") {
                let result_start = self.mark();
                let result_ty = self.parse_type()?;
                let result = ResultType::Single {
                    span: self.span_from_mark(result_start),
                    ty: result_ty,
                };
                let span = self.span_from_mark(start);
                let params = self.pool.alloc_type_expr_list(&params);
                return Ok(self.pool.alloc_type_expr(TypeExpr::Func {
                    span,
                    params,
                    result: Some(result),
                }));
            }
            if saw_comma || params.len() != 1 {
                return Err(ParseError::new(
                    ParseErrorCode::ExpectedToken,
                    "expected '->' after function type parameters",
                    self.current(),
                    self.file_id,
                    self.source,
                ));
            }
            let ty = params[0];
            let span = self.span_from_mark(start);
            return Ok(self
                .pool
                .alloc_type_expr(TypeExpr::Group { span, inner: ty }));
        }
        if let Some(name) = primitive_type_name(&self.current().kind) {
            self.advance();
            let span = self.span_from_mark(start);
            return Ok(self.pool.alloc_type_expr(TypeExpr::Primitive {
                span,
                name: name.into(),
            }));
        }
        if matches!(
            self.current().kind,
            TokenKind::IdentValue | TokenKind::IdentType
        ) {
            let name = self.parse_type_name()?;
            let args = if self.at_kind_name("LT") {
                self.parse_generic_args()?
            } else {
                self.pool.alloc_type_expr_list(&[])
            };
            let span = self.span_from_mark(start);
            return Ok(self
                .pool
                .alloc_type_expr(TypeExpr::Named { span, name, args }));
        }
        Err(ParseError::new(
            ParseErrorCode::ExpectedType,
            "expected type",
            self.current(),
            self.file_id,
            self.source,
        ))
    }

    pub(super) fn parse_type_name(&mut self) -> Result<TypeName, ParseError> {
        let start = self.mark();
        let mut path = Vec::new();
        while matches!(self.current().kind, TokenKind::IdentValue)
            && self
                .tokens
                .get(self.pos + 1)
                .is_some_and(|token| matches!(token.kind, TokenKind::Dot))
        {
            path.push(self.expect_ident_value()?);
            self.expect_name("DOT")?;
        }
        let last = match &self.current().kind {
            TokenKind::IdentType => {
                let name = SmolStr::new(self.current_text());
                self.advance();
                name
            }
            TokenKind::IdentValue if self.current_text() == "void" => {
                let name = SmolStr::new(self.current_text());
                self.advance();
                name
            }
            _ => self.expect_ident_type()?,
        };
        path.push(last);
        Ok(TypeName {
            span: self.span_from_mark(start),
            path: path.into(),
        })
    }

    pub(super) fn can_start_type(&self) -> bool {
        matches!(
            self.current().kind,
            TokenKind::KwPtr | TokenKind::LBracket | TokenKind::KwFunc | TokenKind::LParen
        ) || is_type_token(&self.current().kind)
    }

    pub(super) fn looks_like_generic_call_or_block_suffix(&self) -> bool {
        self.find_matching_gt(self.pos)
            .and_then(|gt| self.tokens.get(gt + 1))
            .is_some_and(|token| matches!(token.kind, TokenKind::LParen | TokenKind::LBrace))
    }

    pub(super) fn looks_like_bare_generic_args(&self) -> bool {
        self.find_matching_gt(self.pos).is_some()
    }

    pub(super) fn looks_like_generic_args_boundary(&self) -> bool {
        self.find_matching_gt(self.pos)
            .and_then(|gt| self.tokens.get(gt + 1))
            .is_none_or(|token| {
                matches!(
                    token.kind,
                    TokenKind::Semicolon
                        | TokenKind::Comma
                        | TokenKind::RParen
                        | TokenKind::RBracket
                        | TokenKind::RBrace
                        | TokenKind::Eof
                )
            })
    }

    pub(super) fn find_matching_gt(&self, start: usize) -> Option<usize> {
        if !matches!(self.tokens.get(start)?.kind, TokenKind::Lt) {
            return None;
        }

        let mut depth = 0usize;
        for (index, token) in self.tokens.iter().enumerate().skip(start) {
            match token.kind {
                TokenKind::Lt => depth += 1,
                TokenKind::Gt => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return Some(index);
                    }
                }
                TokenKind::Eof => return None,
                _ => {}
            }
        }
        None
    }
}
