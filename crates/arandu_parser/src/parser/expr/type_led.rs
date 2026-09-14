use super::super::{FieldInit, ParseError, ParseErrorCode, Parser, TypeExpr, TypeName};
use crate::ast::ast_pool::{ExprId, ExprKind};
use smol_str::SmolStr;

impl<'a> Parser<'a> {
    pub(super) fn parse_type_led_expr(&mut self) -> Result<ExprId, ParseError> {
        let start = self.mark();
        let ty = self.parse_type()?;
        if self.eat_name("LBRACE") {
            let mut fields = Vec::new();
            if !self.at_kind_name("RBRACE") {
                loop {
                    if self.eat_name("RANGE_EXCLUSIVE") {
                        // Struct update syntax: `{ ..base }` or `{ field: val, ..base }`
                        let base_expr = self.parse_expr(0)?;
                        let base_span = self.pool.expr_span(base_expr);
                        let init = FieldInit {
                            span: base_span,
                            name: SmolStr::new(".."),
                            value: base_expr,
                        };
                        let init_id = self.pool.alloc_field_init(init);
                        fields.push(init_id);
                        if self.eat_name("COMMA") {
                            // optional trailing comma after ..base
                        }
                        break;
                    }
                    let field_start = self.mark();
                    let name = self.expect_ident_value()?;
                    let value = if self.eat_name("COLON") {
                        self.parse_expr(0)?
                    } else {
                        // Field init shorthand: `{ x }` desugars to `{ x: x }`
                        let field_span = self.span_from_mark(field_start);
                        self.pool.alloc_expr(
                            ExprKind::Path {
                                path: smallvec::smallvec![name.clone()],
                            },
                            field_span,
                        )
                    };
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
}
