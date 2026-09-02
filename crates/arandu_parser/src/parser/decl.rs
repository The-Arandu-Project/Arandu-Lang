use super::{
    Attribute, ConstDecl, EnumDecl, EnumPayload, EnumVariant, ExternDecl, FieldDecl, FuncDecl,
    FuncName, FuncSignature, ImportDecl, ImportItem, InterfaceDecl, ModuleDecl, ParseError,
    ParseErrorCode, Parser, StructDecl, TokenKind, TopLevelDecl, TypeAliasDecl, TypeName,
    Visibility, is_contextual_module_segment,
};
use smallvec::SmallVec;
use smol_str::SmolStr;

impl<'a> Parser<'a> {
    pub(super) fn expect_optional_semicolon_after_module_path(&mut self) -> Result<(), ParseError> {
        if self.at_kind_name("SEMICOLON") {
            self.expect_semicolon()?;
        } else {
            let last_segment_is_contextual = is_contextual_module_segment(&self.previous().kind);
            let next_starts_top_level = self.at_kind_name("EOF")
                || self.at_kind_name("KW_IMPORT")
                || self.at_soft_keyword("from")
                || matches!(
                    self.current().kind,
                    TokenKind::At
                        | TokenKind::KwPublic
                        | TokenKind::KwConst
                        | TokenKind::KwType
                        | TokenKind::KwAsync
                        | TokenKind::KwFunc
                        | TokenKind::KwStruct
                        | TokenKind::KwEnum
                        | TokenKind::KwInterface
                        | TokenKind::KwExtern
                );
            if !(last_segment_is_contextual && next_starts_top_level) {
                self.expect_semicolon()?;
            }
        }
        Ok(())
    }

    pub(crate) fn parse_module(&mut self) -> Result<ModuleDecl, ParseError> {
        self.collect_doc_comments();
        let docs = self.take_pending_docs();
        let start = self.mark();
        self.expect_name("KW_MODULE")?;
        let path = self.parse_module_path()?;
        self.expect_optional_semicolon_after_module_path()?;
        let module = ModuleDecl {
            span: self.span_from_mark(start),
            path,
        };
        self.attach_docs(docs, module.span);
        Ok(module)
    }

    pub(crate) fn parse_import(&mut self) -> Result<ImportDecl, ParseError> {
        self.collect_doc_comments();
        let docs = self.take_pending_docs();
        let start = self.mark();

        if self.at_soft_keyword("from") {
            self.advance();
            if self.at_kind_name("STRING_START") {
                let source = self.parse_string_literal()?;
                self.expect_name("KW_IMPORT")?;
                self.expect_name("LBRACE")?;
                let items = self.parse_comma_separated_list("RBRACE", 1, |parser| {
                    let item_start = parser.mark();
                    let name = parser.expect_import_name()?;
                    let alias = if parser.eat_name("KW_AS") {
                        Some(parser.expect_import_name()?)
                    } else {
                        None
                    };
                    Ok(ImportItem {
                        span: parser.span_from_mark(item_start),
                        name,
                        alias,
                    })
                })?;
                self.skip_semicolons();
                self.expect_name("RBRACE")?;
                self.expect_optional_semicolon_after_module_path()?;
                let import = ImportDecl::ExternalNamed {
                    span: self.span_from_mark(start),
                    source,
                    items,
                };
                self.attach_docs(docs, import.span());
                return Ok(import);
            } else {
                let path = self.parse_module_path()?;
                self.expect_name("KW_IMPORT")?;
                self.expect_name("LBRACE")?;
                let items = self.parse_comma_separated_list("RBRACE", 1, |parser| {
                    let item_start = parser.mark();
                    let name = parser.expect_import_name()?;
                    let alias = if parser.eat_name("KW_AS") {
                        Some(parser.expect_import_name()?)
                    } else {
                        None
                    };
                    Ok(ImportItem {
                        span: parser.span_from_mark(item_start),
                        name,
                        alias,
                    })
                })?;
                self.skip_semicolons();
                self.expect_name("RBRACE")?;
                self.expect_optional_semicolon_after_module_path()?;
                let import = ImportDecl::Named {
                    span: self.span_from_mark(start),
                    path,
                    items,
                };
                self.attach_docs(docs, import.span());
                return Ok(import);
            }
        }

        // `import <path> as <alias>` or `import "<source>" as <alias>`
        self.expect_name("KW_IMPORT")?;

        if self.at_kind_name("STRING_START") {
            let source = self.parse_string_literal()?;
            self.expect_name("KW_AS")?;
            let alias = self.expect_import_name()?;
            self.expect_optional_semicolon_after_module_path()?;
            let import = ImportDecl::ExternalAlias {
                span: self.span_from_mark(start),
                source,
                alias,
            };
            self.attach_docs(docs, import.span());
            return Ok(import);
        }

        let path = self.parse_module_path()?;
        let alias = if self.eat_name("KW_AS") {
            self.expect_import_name()?
        } else {
            // `parse_module_path` always pushes at least one segment.
            path.last().cloned().ok_or_else(|| {
                ParseError::new(
                    ParseErrorCode::ExpectedToken,
                    "expected module path segment",
                    self.current(),
                    self.file_id,
                    self.source,
                )
            })?
        };
        self.expect_optional_semicolon_after_module_path()?;
        let import = ImportDecl::ModuleAlias {
            span: self.span_from_mark(start),
            path,
            alias,
        };
        self.attach_docs(docs, import.span());
        Ok(import)
    }

    pub(super) fn parse_string_literal(&mut self) -> Result<SmolStr, ParseError> {
        self.expect_name("STRING_START")?;
        let mut text = String::new();
        while !self.at_kind_name("STRING_END") {
            match &self.current().kind {
                TokenKind::StringText | TokenKind::StringEscape => {
                    text.push_str(self.current_text());
                    self.advance();
                }
                _ => {
                    return Err(ParseError::new(
                        ParseErrorCode::ExpectedToken,
                        "expected string content",
                        self.current(),
                        self.file_id,
                        self.source,
                    ));
                }
            }
        }
        self.expect_name("STRING_END")?;
        Ok(text.into())
    }

    pub(crate) fn parse_top_level_decls(&mut self) -> Result<Vec<TopLevelDecl>, ParseError> {
        let start = self.mark();
        match self.try_parse_top_level_decls() {
            Ok(decls) => Ok(decls),
            Err(err) => {
                self.diagnostics.push(err);
                self.synchronize_top_level();
                Ok(vec![TopLevelDecl::Error(self.span_from_mark(start))])
            }
        }
    }

    pub(super) fn try_parse_top_level_decls(&mut self) -> Result<Vec<TopLevelDecl>, ParseError> {
        self.collect_doc_comments();
        let docs = self.take_pending_docs();
        let item_kind = self.peek_top_level_item_kind();
        self.start_node(item_kind);
        let result = (|| {
            let attrs = self.parse_attributes()?;
            let visibility = self.parse_visibility();
            match self.current().kind {
                TokenKind::KwConst => Ok(vec![TopLevelDecl::Const(
                    self.parse_const(attrs, visibility)?,
                )]),
                TokenKind::KwType => Ok(vec![TopLevelDecl::TypeAlias(
                    self.parse_type_alias(attrs, visibility)?,
                )]),
                TokenKind::KwAsync | TokenKind::KwFunc => Ok(vec![TopLevelDecl::Func(
                    self.parse_func(attrs, visibility)?,
                )]),
                TokenKind::KwStruct => Ok(vec![TopLevelDecl::Struct(
                    self.parse_struct_decl(attrs, visibility)?,
                )]),
                TokenKind::KwEnum => Ok(vec![TopLevelDecl::Enum(
                    self.parse_enum_decl(attrs, visibility)?,
                )]),
                TokenKind::KwInterface => Ok(vec![TopLevelDecl::Interface(
                    self.parse_interface_decl(attrs, visibility)?,
                )]),
                TokenKind::KwExtern => {
                    Ok(vec![TopLevelDecl::Extern(self.parse_extern_decl(attrs)?)])
                }
                TokenKind::KwImpl => self.parse_impl_decl(attrs, visibility),
                _ => Err(ParseError::new(
                    ParseErrorCode::ExpectedTopLevelDecl,
                    "expected top-level declaration",
                    self.current(),
                    self.file_id,
                    self.source,
                )),
            }
        })();
        self.finish_node();
        let decls = result?;
        if let Some(first) = decls.first() {
            self.attach_docs(docs, first.span());
        }
        Ok(decls)
    }

    /// Look ahead past `@attr` / `public` / `async` to classify the green item kind.
    fn peek_top_level_item_kind(&self) -> crate::syntax::SyntaxKind {
        use crate::syntax::SyntaxKind;
        let mut i = self.pos;
        while i < self.tokens.len() {
            match self.tokens[i].kind {
                TokenKind::At => {
                    i += 1;
                    // skip attr name and optional ( … )
                    if i < self.tokens.len()
                        && matches!(
                            self.tokens[i].kind,
                            TokenKind::IdentValue | TokenKind::IdentType
                        )
                    {
                        i += 1;
                    }
                    if i < self.tokens.len() && matches!(self.tokens[i].kind, TokenKind::LParen) {
                        let mut depth = 0i32;
                        while i < self.tokens.len() {
                            match self.tokens[i].kind {
                                TokenKind::LParen => depth += 1,
                                TokenKind::RParen => {
                                    depth -= 1;
                                    i += 1;
                                    if depth == 0 {
                                        break;
                                    }
                                    continue;
                                }
                                TokenKind::Eof => break,
                                _ => {}
                            }
                            i += 1;
                        }
                    }
                }
                TokenKind::KwPublic | TokenKind::KwAsync | TokenKind::Semicolon => i += 1,
                TokenKind::KwConst => return SyntaxKind::CONST_ITEM,
                TokenKind::KwType => return SyntaxKind::TYPE_ALIAS_ITEM,
                TokenKind::KwFunc => return SyntaxKind::FUNC_ITEM,
                TokenKind::KwStruct => return SyntaxKind::STRUCT_ITEM,
                TokenKind::KwEnum => return SyntaxKind::ENUM_ITEM,
                TokenKind::KwInterface => return SyntaxKind::INTERFACE_ITEM,
                TokenKind::KwExtern => return SyntaxKind::EXTERN_ITEM,
                TokenKind::KwImpl => return SyntaxKind::IMPL_ITEM,
                _ => return SyntaxKind::ITEM,
            }
        }
        SyntaxKind::ITEM
    }

    pub(super) fn parse_const(
        &mut self,
        attrs: Vec<Attribute>,
        visibility: Visibility,
    ) -> Result<ConstDecl, ParseError> {
        let start = self.mark();
        self.expect_name("KW_CONST")?;
        let name = self.expect_name_like()?;
        let ty = if self.can_start_type() {
            Some(self.parse_type()?)
        } else {
            None
        };
        self.expect_name("EQUAL")?;
        let value = self.parse_expr(0)?;
        self.expect_semicolon()?;
        Ok(ConstDecl {
            span: self.span_from_mark(start),
            attrs: attrs.into(),
            visibility,
            name,
            ty,
            value,
        })
    }

    pub(super) fn parse_type_alias(
        &mut self,
        attrs: Vec<Attribute>,
        visibility: Visibility,
    ) -> Result<TypeAliasDecl, ParseError> {
        let start = self.mark();
        self.expect_name("KW_TYPE")?;
        let name = self.expect_ident_type()?;
        let generic_params = self.parse_generic_params()?;
        self.expect_name("EQUAL")?;
        let ty = self.parse_type()?;
        self.expect_semicolon()?;
        Ok(TypeAliasDecl {
            span: self.span_from_mark(start),
            attrs: attrs.into(),
            visibility,
            name,
            generic_params,
            ty,
        })
    }

    pub(super) fn parse_func(
        &mut self,
        attrs: Vec<Attribute>,
        visibility: Visibility,
    ) -> Result<FuncDecl, ParseError> {
        let start = self.mark();
        let is_async = self.eat_name("KW_ASYNC");
        self.expect_name("KW_FUNC")?;
        let name = self.parse_func_name()?;
        let generic_params = self.parse_generic_params()?;
        self.expect_name("LPAREN")?;
        let method_receiver = match &name {
            FuncName::Method { receiver, .. } => Some(receiver),
            FuncName::Free { .. } => None,
        };
        let params = self.parse_params(method_receiver)?;
        self.expect_name("RPAREN")?;
        let result = if self.eat_name("COLON") {
            Some(self.parse_result_type()?)
        } else {
            None
        };
        let where_clause = self.parse_where_clause("LBRACE")?;
        let body = self.parse_block()?;
        Ok(FuncDecl {
            span: self.span_from_mark(start),
            attrs: attrs.into(),
            visibility,
            is_async,
            name,
            generic_params,
            params,
            result,
            where_clause,
            body,
        })
    }

    pub(super) fn parse_struct_decl(
        &mut self,
        attrs: Vec<Attribute>,
        visibility: Visibility,
    ) -> Result<StructDecl, ParseError> {
        let start = self.mark();
        self.expect_name("KW_STRUCT")?;
        let name = self.expect_ident_type()?;
        let generic_params = self.parse_generic_params()?;
        let where_clause = self.parse_where_clause("LBRACE")?;
        self.start_node(crate::syntax::SyntaxKind::BLOCK);
        self.expect_name("LBRACE")?;
        let mut fields = Vec::new();
        while !self.at_kind_name("RBRACE") {
            self.skip_semicolons();
            if self.at_kind_name("RBRACE") {
                break;
            }
            if self.at_kind_name("EOF") {
                self.diagnostics.push(ParseError::new(
                    ParseErrorCode::ExpectedToken,
                    "expected '}'",
                    self.current(),
                    self.file_id,
                    self.source,
                ));
                break;
            }
            self.start_node(crate::syntax::SyntaxKind::STMT);
            fields.push(self.parse_field_decl(true)?);
            self.finish_node();
        }
        self.expect_name("RBRACE")?;
        self.finish_node(); // BLOCK
        self.skip_semicolons();
        Ok(StructDecl {
            span: self.span_from_mark(start),
            attrs: attrs.into(),
            visibility,
            name,
            generic_params,
            where_clause,
            fields,
        })
    }

    pub(super) fn parse_field_decl(
        &mut self,
        require_semicolon: bool,
    ) -> Result<FieldDecl, ParseError> {
        self.collect_doc_comments();
        let docs = self.take_pending_docs();
        let start = self.mark();
        let attrs = self.parse_attributes()?;
        let visibility = self.parse_visibility();
        let name = self.expect_ident_value()?;
        self.expect_name("COLON")?;
        let ty = self.parse_type()?;
        if require_semicolon || self.at_kind_name("SEMICOLON") {
            self.expect_semicolon()?;
        }
        let field = FieldDecl {
            span: self.span_from_mark(start),
            attrs: attrs.into(),
            visibility,
            name,
            ty,
        };
        self.attach_docs(docs, field.span);
        Ok(field)
    }

    pub(super) fn parse_enum_decl(
        &mut self,
        attrs: Vec<Attribute>,
        visibility: Visibility,
    ) -> Result<EnumDecl, ParseError> {
        let start = self.mark();
        self.expect_name("KW_ENUM")?;
        let name = self.expect_ident_type()?;
        let generic_params = self.parse_generic_params()?;
        let where_clause = self.parse_where_clause("LBRACE")?;
        self.start_node(crate::syntax::SyntaxKind::BLOCK);
        self.expect_name("LBRACE")?;
        let mut variants = Vec::new();
        while !self.at_kind_name("RBRACE") {
            self.skip_semicolons();
            if self.at_kind_name("RBRACE") {
                break;
            }
            if self.at_kind_name("EOF") {
                self.diagnostics.push(ParseError::new(
                    ParseErrorCode::ExpectedToken,
                    "expected '}'",
                    self.current(),
                    self.file_id,
                    self.source,
                ));
                break;
            }
            self.start_node(crate::syntax::SyntaxKind::STMT);
            variants.push(self.parse_enum_variant()?);
            self.finish_node();
            if self.eat_name("COMMA") {
                continue;
            }
            self.skip_semicolons();
        }
        self.expect_name("RBRACE")?;
        self.finish_node(); // BLOCK
        self.skip_semicolons();
        Ok(EnumDecl {
            span: self.span_from_mark(start),
            attrs: attrs.into(),
            visibility,
            name,
            generic_params,
            where_clause,
            variants,
        })
    }

    pub(super) fn parse_enum_variant(&mut self) -> Result<EnumVariant, ParseError> {
        self.collect_doc_comments();
        let docs = self.take_pending_docs();
        let start = self.mark();
        let attrs = self.parse_attributes()?;
        let name = self.expect_ident_type()?;
        let payload = if self.eat_name("LPAREN") {
            let payload_start = self.pos.saturating_sub(1);
            let types = self.parse_comma_separated_list("RPAREN", 0, super::Parser::parse_type)?;
            self.expect_name("RPAREN")?;
            let range = self.pool.alloc_type_expr_list(&types);
            Some(EnumPayload::Tuple {
                span: self.span_from_mark(payload_start),
                types: range,
            })
        } else if self.eat_name("LBRACE") {
            let payload_start = self.pos.saturating_sub(1);
            let fields = self
                .parse_comma_separated_list("RBRACE", 0, |parser| parser.parse_field_decl(false))?;
            self.expect_name("RBRACE")?;
            Some(EnumPayload::Struct {
                span: self.span_from_mark(payload_start),
                fields,
            })
        } else {
            None
        };
        let variant = EnumVariant {
            span: self.span_from_mark(start),
            attrs: attrs.into(),
            name,
            payload,
        };
        self.attach_docs(docs, variant.span);
        Ok(variant)
    }

    pub(super) fn parse_interface_decl(
        &mut self,
        attrs: Vec<Attribute>,
        visibility: Visibility,
    ) -> Result<InterfaceDecl, ParseError> {
        let start = self.mark();
        self.expect_name("KW_INTERFACE")?;
        let name = self.expect_ident_type()?;
        let generic_params = self.parse_generic_params()?;
        let where_clause = self.parse_where_clause("LBRACE")?;

        // Build a synthetic receiver TypeName so that `self` inside interface
        // method signatures doesn't require an explicit type annotation —
        // matching Rust's trait behaviour where `self` implicitly means `Self`.
        let self_receiver = TypeName {
            span: arandu_lexer::Span::new(0, 0, 0),
            path: {
                let mut path = SmallVec::new();
                path.push(SmolStr::new_static("Self"));
                path
            },
        };

        let members = self.parse_braced_member_list(|parser| {
            let attrs = parser.parse_attributes()?;
            parser.parse_func_signature_with_receiver(attrs, Some(&self_receiver))
        })?;
        Ok(InterfaceDecl {
            span: self.span_from_mark(start),
            attrs: attrs.into(),
            visibility,
            name,
            generic_params,
            where_clause,
            members,
        })
    }

    pub(super) fn parse_extern_decl(
        &mut self,
        attrs: Vec<Attribute>,
    ) -> Result<ExternDecl, ParseError> {
        let start = self.mark();
        self.expect_name("KW_EXTERN")?;
        let abi = self.parse_abi_literal()?;
        let members = self.parse_braced_member_list(|parser| {
            let attrs = parser.parse_attributes()?;
            parser.parse_func_signature(attrs)
        })?;
        Ok(ExternDecl {
            span: self.span_from_mark(start),
            attrs: attrs.into(),
            abi,
            members,
        })
    }

    pub(super) fn parse_impl_decl(
        &mut self,
        _attrs: Vec<Attribute>,
        _visibility: Visibility,
    ) -> Result<Vec<TopLevelDecl>, ParseError> {
        self.expect_name("KW_IMPL")?;
        let impl_generic_params = self.parse_generic_params()?;
        let target_type_name = self.parse_type_name()?;
        let type_generic_args = if self.eat_name("LT") {
            let args = self.parse_comma_separated_list("GT", 1, super::Parser::parse_type)?;
            self.expect_name("GT")?;
            args
        } else {
            Vec::new()
        };
        // The target arguments select the implemented instantiation. The
        // nominal receiver remains `TypeName`; its declaration owns the
        // generic parameter symbols imported into every member scope.
        let _type_generic_args = type_generic_args;
        let where_clause = self.parse_where_clause("LBRACE")?;

        self.start_node(crate::syntax::SyntaxKind::BLOCK);
        self.expect_name("LBRACE")?;

        let mut methods = Vec::new();
        while !self.at_kind_name("RBRACE") {
            self.skip_semicolons();
            if self.at_kind_name("RBRACE") {
                break;
            }
            if self.at_kind_name("EOF") {
                self.diagnostics.push(ParseError::new(
                    ParseErrorCode::ExpectedToken,
                    "expected '}'",
                    self.current(),
                    self.file_id,
                    self.source,
                ));
                break;
            }

            self.start_node(crate::syntax::SyntaxKind::FUNC_ITEM);
            let method_start = self.mark();
            let m_attrs = self.parse_attributes()?;
            let m_visibility = self.parse_visibility();
            let is_async = self.eat_name("KW_ASYNC");
            self.expect_name("KW_FUNC")?;
            let func_name_start = self.mark();
            let method_name = self.expect_ident_value()?;
            let member_func_name = FuncName::Method {
                span: self.span_from_mark(func_name_start),
                receiver: target_type_name.clone(),
                name: method_name,
            };

            let method_generics = self.parse_generic_params()?;
            let mut all_generics = impl_generic_params.clone();
            all_generics.extend(method_generics);

            self.expect_name("LPAREN")?;
            let params = self.parse_params(Some(&target_type_name))?;
            self.expect_name("RPAREN")?;

            let result = if self.eat_name("COLON") {
                Some(self.parse_result_type()?)
            } else {
                None
            };

            let m_where = self.parse_where_clause("LBRACE")?;
            let mut all_where = where_clause.clone();
            all_where.extend(m_where);

            let body = self.parse_block()?;
            self.finish_node(); // FUNC_ITEM

            let func_decl = FuncDecl {
                span: self.span_from_mark(method_start),
                attrs: m_attrs.into(),
                visibility: m_visibility,
                is_async,
                name: member_func_name,
                generic_params: all_generics,
                params,
                result,
                where_clause: all_where,
                body,
            };
            methods.push(func_decl);
            self.skip_semicolons();
        }

        self.expect_name("RBRACE")?;
        self.finish_node(); // BLOCK

        if methods.is_empty() {
            return Err(ParseError::new(
                ParseErrorCode::ExpectedTopLevelDecl,
                "an impl block must declare at least one function",
                self.previous(),
                self.file_id,
                self.source,
            ));
        }
        Ok(methods.into_iter().map(TopLevelDecl::Func).collect())
    }

    pub(super) fn parse_abi_literal(&mut self) -> Result<SmolStr, ParseError> {
        self.expect_name("STRING_START")?;
        let abi = match &self.current().kind {
            TokenKind::StringText => {
                let text = SmolStr::new(self.current_text());
                self.advance();
                text
            }
            _ => {
                return Err(ParseError::new(
                    ParseErrorCode::ExpectedToken,
                    "expected static ABI string",
                    self.current(),
                    self.file_id,
                    self.source,
                ));
            }
        };
        self.expect_name("STRING_END")?;
        Ok(abi)
    }

    pub(super) fn parse_func_signature(
        &mut self,
        attrs: Vec<Attribute>,
    ) -> Result<FuncSignature, ParseError> {
        self.parse_func_signature_with_receiver(attrs, None)
    }

    pub(super) fn parse_func_signature_with_receiver(
        &mut self,
        attrs: Vec<Attribute>,
        receiver: Option<&TypeName>,
    ) -> Result<FuncSignature, ParseError> {
        self.collect_doc_comments();
        let docs = self.take_pending_docs();
        let start = self.mark();
        self.expect_name("KW_FUNC")?;
        let name = self.expect_ident_value()?;
        let generic_params = self.parse_generic_params()?;
        self.expect_name("LPAREN")?;
        let params = self.parse_params(receiver)?;
        self.expect_name("RPAREN")?;
        let result = if self.eat_name("COLON") {
            Some(self.parse_result_type()?)
        } else {
            None
        };
        let where_clause = self.parse_where_clause("SEMICOLON")?;
        let signature = FuncSignature {
            span: self.span_from_mark(start),
            attrs: attrs.into(),
            name,
            generic_params,
            params,
            result,
            where_clause,
        };
        self.attach_docs(docs, signature.span);
        Ok(signature)
    }

    pub(super) fn parse_attributes(&mut self) -> Result<Vec<Attribute>, ParseError> {
        let mut attrs = Vec::new();
        while self.eat_name("AT") {
            let start = self.pos.saturating_sub(1);
            let name_span = self.current().span(self.file_id);
            let name = self.expect_name_like()?;
            let args = if self.eat_name("LPAREN") {
                let args = self.parse_arguments()?;
                self.expect_name("RPAREN")?;
                args
            } else {
                Vec::new()
            };
            attrs.push(Attribute {
                span: self.span_from_mark(start),
                name_span,
                name,
                args,
            });
            self.skip_semicolons();
        }
        Ok(attrs)
    }

    pub(super) fn parse_comma_separated_list<T, F>(
        &mut self,
        end_name: &str,
        min_items: usize,
        mut parse_item: F,
    ) -> Result<Vec<T>, ParseError>
    where
        F: FnMut(&mut Self) -> Result<T, ParseError>,
    {
        if self.at_kind_name(end_name) {
            if min_items == 0 {
                return Ok(Vec::new());
            }
            return Err(ParseError::new(
                ParseErrorCode::ExpectedToken,
                format!("expected item before {end_name}"),
                self.current(),
                self.file_id,
                self.source,
            ));
        }

        let mut items = Vec::new();
        loop {
            items.push(parse_item(self)?);
            if !self.eat_name("COMMA") {
                break;
            }
            if self.at_kind_name(end_name) {
                break;
            }
        }

        if items.len() < min_items {
            return Err(ParseError::new(
                ParseErrorCode::ExpectedToken,
                format!("expected at least {min_items} item(s) before {end_name}"),
                self.current(),
                self.file_id,
                self.source,
            ));
        }

        Ok(items)
    }

    pub(super) fn parse_braced_member_list<T, F>(
        &mut self,
        mut parse_item: F,
    ) -> Result<Vec<T>, ParseError>
    where
        F: FnMut(&mut Self) -> Result<T, ParseError>,
    {
        self.start_node(crate::syntax::SyntaxKind::BLOCK);
        self.expect_name("LBRACE")?;
        let mut items = Vec::new();
        while !self.at_kind_name("RBRACE") {
            self.skip_semicolons();
            if self.at_kind_name("RBRACE") {
                break;
            }
            if self.at_kind_name("EOF") {
                self.diagnostics.push(ParseError::new(
                    ParseErrorCode::ExpectedToken,
                    "expected '}'",
                    self.current(),
                    self.file_id,
                    self.source,
                ));
                break;
            }
            self.start_node(crate::syntax::SyntaxKind::STMT);
            items.push(parse_item(self)?);
            self.expect_semicolon()?;
            self.finish_node();
        }
        self.expect_name("RBRACE")?;
        self.finish_node(); // BLOCK
        self.skip_semicolons();
        Ok(items)
    }

    pub(super) fn parse_visibility(&mut self) -> Visibility {
        if self.eat_name("KW_PUBLIC") {
            Visibility::Public
        } else {
            Visibility::Private
        }
    }

    pub(super) fn parse_module_path(&mut self) -> Result<SmallVec<[SmolStr; 3]>, ParseError> {
        let mut path = SmallVec::new();
        path.push(self.expect_module_segment()?);
        while self.eat_name("DOT") {
            path.push(self.expect_module_segment()?);
        }
        Ok(path)
    }

    pub(super) fn parse_func_name(&mut self) -> Result<FuncName, ParseError> {
        let start = self.pos;
        if matches!(
            self.current().kind,
            TokenKind::IdentType | TokenKind::IdentValue
        ) {
            let checkpoint = self.checkpoint();
            if let Ok(receiver) = self.parse_type_name()
                && self.eat_name("DOT")
            {
                let name = self.expect_ident_value()?;
                return Ok(FuncName::Method {
                    span: self.span_from_mark(start),
                    receiver,
                    name,
                });
            }
            checkpoint.rollback(self);
        }

        let name = self.expect_ident_value()?;
        Ok(FuncName::Free {
            span: self.span_from_mark(start),
            name,
        })
    }

    pub(super) fn expect_import_name(&mut self) -> Result<SmolStr, ParseError> {
        match &self.current().kind {
            TokenKind::IdentValue | TokenKind::IdentType => {
                let name = SmolStr::new(self.current_text());
                self.advance();
                Ok(name)
            }
            _ => Err(ParseError::expected(
                ParseErrorCode::ExpectedToken,
                "expected import identifier",
                self.current(),
                self.file_id,
                self.source,
                &["import identifier"],
            )),
        }
    }
}
