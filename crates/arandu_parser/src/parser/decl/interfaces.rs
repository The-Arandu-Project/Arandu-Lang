use super::super::{
    Attribute, FuncDecl, FuncName, InterfaceDecl, ParseError, ParseErrorCode, Parser, TopLevelDecl,
    TypeName, Visibility,
};
use smallvec::SmallVec;
use smol_str::SmolStr;

impl<'a> Parser<'a> {
    pub(in crate::parser) fn parse_interface_decl(
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

    pub(in crate::parser) fn parse_impl_decl(
        &mut self,
        _attrs: Vec<Attribute>,
        _visibility: Visibility,
    ) -> Result<Vec<TopLevelDecl>, ParseError> {
        self.expect_name("KW_IMPL")?;
        let impl_generic_params = self.parse_generic_params()?;
        let target_type_name = self.parse_type_name()?;
        let type_generic_args = if self.eat_name("LT") {
            let args = self.parse_generic_list(1, super::super::Parser::parse_type)?;
            self.expect_gt()?;
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
}
