//! Program / item / actor / type-signature parsing (grammar fragment of `parser.rs`).
//!
//! Free `impl Parser` methods: `parse_program`, `parse_item`, `parse_actor_item`,
//! `parse_pragma`, `parse_type`, `parse_sig_after_colon`, and their helpers.

use super::{ParseError, Parser};
use crate::ast::*;
use crate::token::Token;
use crate::Span;

impl<'a> Parser<'a> {
    pub(crate) fn parse_program(&mut self) -> Result<Program, ParseError> {
        let (exports, imports) = self.parse_header()?;
        let mut items = Vec::new();
        let mut seen_funcs: Vec<(String, usize)> = Vec::new();
        let mut seen_actors: Vec<String> = Vec::new();
        let mut pending_pragma: Option<Pragma> = None;

        while !self.at_eof() {
            // Skip stray layout separators / braces between top-level items
            if matches!(
                self.peek_kind(),
                Some(
                    Token::VirtualSemi
                        | Token::Semi
                        | Token::VirtualRBrace
                        | Token::VirtualLBrace
                )
            ) {
                self.pos += 1;
                continue;
            }

            if matches!(self.peek_kind(), Some(Token::At)) {
                // Distinguishing pragma `@Name[...]` from `@Name` declaration / atom is
                // done by lookahead for `[` after the name.
                if self.lookahead_is_pragma() {
                    if pending_pragma.is_some() {
                        return Err(ParseError::new(
                            self.span_here(),
                            "pragma must be followed by an `@`-declaration",
                        ));
                    }
                    pending_pragma = Some(self.parse_pragma()?);
                    continue;
                }
                let mut actor = self.parse_actor_item()?;
                if let Some(pr) = pending_pragma.take() {
                    if pr.name != actor.name {
                        return Err(ParseError::new(
                            pr.span,
                            format!(
                                "pragma `@{}[...]` does not match following `@{}` declaration",
                                pr.name, actor.name
                            ),
                        ));
                    }
                    actor.pragma = Some(pr);
                }
                if seen_actors.iter().any(|n| n == &actor.name) {
                    return Err(ParseError::new(
                        actor.span,
                        format!(
                            "duplicate equation for `@{}` (only one equation per `@`-declaration)",
                            actor.name
                        ),
                    ));
                }
                seen_actors.push(actor.name.clone());
                items.push(Item::Actor(actor));
                continue;
            }

            if let Some(pragma) = &pending_pragma {
                return Err(ParseError::new(
                    pragma.span,
                    "pragma must be followed immediately by an `@`-declaration",
                ));
            }

            let item = self.parse_item()?;
            if let Item::Func(ref f) = item {
                let arity = f.params.len();
                if seen_funcs.iter().any(|(n, a)| n == &f.name && *a == arity) {
                    return Err(ParseError::new(
                        f.span,
                        format!(
                            "duplicate equation for function `{}` at arity {}",
                            f.name, arity
                        ),
                    ));
                }
                seen_funcs.push((f.name.clone(), arity));
            }
            items.push(item);
        }

        if let Some(pr) = pending_pragma {
            return Err(ParseError::new(
                pr.span,
                "pragma must be followed immediately by an `@`-declaration",
            ));
        }

        Ok(Program { exports, imports, items, source: Some(self.source.to_string()) })
    }

    fn parse_header(
        &mut self,
    ) -> Result<(Option<ExportBlock>, Option<ImportBlock>), ParseError> {
        let mut exports = None;
        let mut imports = None;
        if self.peek_kind() == Some(&Token::Export) {
            exports = Some(self.parse_export_block()?);
        }
        if self.peek_kind() == Some(&Token::Import) {
            imports = Some(self.parse_import_block()?);
        }
        if self.peek_kind() == Some(&Token::Export) {
            return Err(ParseError::new(
                self.span_here(),
                "export block must come before import block",
            ));
        }
        Ok((exports, imports))
    }

    fn parse_export_block(&mut self) -> Result<ExportBlock, ParseError> {
        let start = self.span_here().start;
        self.expect(&Token::Export)?;
        self.expect(&Token::VirtualLBrace)?;
        let mut items = Vec::new();
        loop {
            if self.peek_kind() == Some(&Token::VirtualRBrace) {
                break;
            }
            if !items.is_empty() {
                self.expect(&Token::VirtualSemi)?;
            }
            items.push(self.parse_export_item()?);
        }
        self.expect(&Token::VirtualRBrace)?;
        // `..` must be the sole item.
        if items.iter().any(|i| matches!(i, ExportItem::All { .. })) && items.len() > 1 {
            return Err(ParseError::new(
                self.span_here(),
                "`..` must be the sole item in an export block",
            ));
        }
        self.check_item_order(
            &items.iter().map(|i| i.kind()).collect::<Vec<_>>(),
            "export block",
        )?;
        Ok(ExportBlock {
            items,
            span: Span::new(start, self.span_here().end),
        })
    }

    fn parse_import_block(&mut self) -> Result<ImportBlock, ParseError> {
        let start = self.span_here().start;
        self.expect(&Token::Import)?;
        self.expect(&Token::VirtualLBrace)?;
        let mut groups = Vec::new();
        loop {
            if self.peek_kind() == Some(&Token::VirtualRBrace) {
                break;
            }
            if !groups.is_empty() {
                self.expect(&Token::VirtualSemi)?;
            }
            let module_tok = self.bump()?;
            let module_start = module_tok.span.start;
            let module = match &module_tok.value {
                Token::Ident(s) => s.clone(),
                _ => {
                    return Err(ParseError::new(
                        module_tok.span,
                        "expected module name in import block",
                    ))
                }
            };
            self.expect(&Token::VirtualLBrace)?;
            let mut items = Vec::new();
            loop {
                if self.peek_kind() == Some(&Token::VirtualRBrace) {
                    break;
                }
                if !items.is_empty() {
                    self.expect(&Token::VirtualSemi)?;
                }
                items.push(self.parse_import_item()?);
            }
            self.expect(&Token::VirtualRBrace)?;
            self.check_item_order(
                &items.iter().map(|i| i.kind()).collect::<Vec<_>>(),
                &format!("import group `{module}`"),
            )?;
            groups.push(ImportGroup {
                module,
                items,
                span: Span::new(module_start, self.span_here().end),
            });
        }
        self.expect(&Token::VirtualRBrace)?;
        Ok(ImportBlock {
            groups,
            span: Span::new(start, self.span_here().end),
        })
    }

    fn parse_export_item(&mut self) -> Result<ExportItem, ParseError> {
        if self.peek_kind() == Some(&Token::DotDot) {
            let tok = self.bump()?;
            return Ok(ExportItem::All { span: tok.span });
        }
        let (first, first_is_actor, first_span) = self.parse_item_name()?;
        let (name, alias, end_span) = if self.peek_kind() == Some(&Token::LeftArrow) {
            self.pos += 1;
            let (source, source_is_actor, source_span) = self.parse_item_name()?;
            if ItemKind::from_name(&first, first_is_actor)
                != ItemKind::from_name(&source, source_is_actor)
            {
                return Err(ParseError::new(
                    source_span,
                    "alias and source must be the same kind",
                ));
            }
            (source, Some(first), source_span)
        } else {
            (first, None, first_span)
        };
        let is_function = !first_is_actor
            && !name
                .chars()
                .next()
                .unwrap_or(' ')
                .is_ascii_uppercase();
        // Optional `from <module>` clause (re-export). Checked before the
        // signature: a re-exported name has no surfaced signature.
        let (from, from_end) = if self.peek_kind() == Some(&Token::From) {
            self.pos += 1;
            let mod_tok = self.bump()?;
            let mod_name = match &mod_tok.value {
                Token::Ident(s) => s.clone(),
                _ => {
                    return Err(ParseError::new(
                        mod_tok.span,
                        "expected module name after `from`",
                    ))
                }
            };
            (Some(mod_name), mod_tok.span.end)
        } else {
            (None, 0)
        };
        let signature = if from.is_none() && is_function {
            self.parse_export_signature()?
        } else {
            None
        };
        let span_end = if signature.is_some() {
            self.span_here().end
        } else if from.is_some() {
            from_end
        } else {
            end_span.end
        };
        Ok(ExportItem::Named {
            name,
            is_actor: first_is_actor,
            alias,
            signature,
            from,
            span: Span::new(first_span.start, span_end),
        })
    }

    fn parse_import_item(&mut self) -> Result<ImportItem, ParseError> {
        if self.peek_kind() == Some(&Token::DotDot) {
            let tok = self.bump()?;
            return Ok(ImportItem::All { span: tok.span });
        }
        let (first, first_is_actor, first_span) = self.parse_item_name()?;
        let (name, alias, end_span) = if self.peek_kind() == Some(&Token::LeftArrow) {
            self.pos += 1;
            let (source, source_is_actor, source_span) = self.parse_item_name()?;
            if ItemKind::from_name(&first, first_is_actor)
                != ItemKind::from_name(&source, source_is_actor)
            {
                return Err(ParseError::new(
                    source_span,
                    "alias and source must be the same kind",
                ));
            }
            (source, Some(first), source_span)
        } else {
            (first, None, first_span)
        };
        Ok(ImportItem::Named {
            name,
            is_actor: first_is_actor,
            alias,
            span: Span::new(first_span.start, end_span.end),
        })
    }

    fn parse_item_name(&mut self) -> Result<(String, bool, Span), ParseError> {
        if self.peek_kind() == Some(&Token::At) {
            let at_tok = self.bump()?;
            let at_start = at_tok.span.start;
            let tok = self.bump()?;
            let name = match &tok.value {
                Token::Ident(s) | Token::UpperIdent(s) => s.clone(),
                _ => {
                    return Err(ParseError::new(
                        tok.span,
                        "expected actor name after `@`",
                    ))
                }
            };
            return Ok((name, true, Span::new(at_start, tok.span.end)));
        }
        let tok = self.bump()?;
        match &tok.value {
            Token::Ident(s) | Token::UpperIdent(s) => Ok((s.clone(), false, tok.span)),
            _ => Err(ParseError::new(tok.span, "expected item name")),
        }
    }

    fn parse_export_signature(&mut self) -> Result<Option<Type>, ParseError> {
        if self.peek_kind() == Some(&Token::VirtualSemi)
            || self.peek_kind() == Some(&Token::VirtualRBrace)
        {
            return Ok(None);
        }
        let (fields, result, is_func_sig, _) = self.parse_sig_after_colon()?;
        if is_func_sig {
            Ok(Some(Type::Func(
                fields.into_iter().map(|f| f.ty).collect(),
                Box::new(result),
            )))
        } else {
            Ok(Some(result))
        }
    }

    fn check_item_order(
        &self,
        kinds: &[Option<ItemKind>],
        context: &str,
    ) -> Result<(), ParseError> {
        let mut max_kind: Option<ItemKind> = None;
        for k in kinds.iter().flatten() {
            if let Some(max) = max_kind {
                if *k < max {
                    return Err(ParseError::new(
                        self.span_here(),
                        format!("items in {context} must be ordered Data \u{2192} Actors \u{2192} Functions"),
                    ));
                }
            }
            max_kind = Some(match max_kind {
                Some(m) => m.max(*k),
                None => *k,
            });
        }
        Ok(())
    }

    pub(crate) fn lookahead_is_pragma(&self) -> bool {
        // `@` Name `[`
        if !matches!(self.peek_kind(), Some(Token::At)) {
            return false;
        }
        match self.tokens.get(self.pos + 1).map(|t| &t.value) {
            Some(Token::Ident(_) | Token::UpperIdent(_)) => {
                matches!(
                    self.tokens.get(self.pos + 2).map(|t| &t.value),
                    Some(Token::LBracket)
                )
            }
            _ => false,
        }
    }

    pub(crate) fn parse_pragma(&mut self) -> Result<Pragma, ParseError> {
        let start = self.span_here().start;
        self.expect(&Token::At)?;
        let name_tok = self.bump()?.clone();
        let name = match &name_tok.value {
            Token::Ident(s) | Token::UpperIdent(s) => s.clone(),
            _ => {
                return Err(ParseError::new(
                    name_tok.span,
                    "expected name after `@` in pragma",
                ));
            }
        };
        self.expect(&Token::LBracket)?;
        let mut args = Vec::new();
        if !matches!(self.peek_kind(), Some(Token::RBracket)) {
            loop {
                let key_tok = self.bump()?.clone();
                let key = match &key_tok.value {
                    Token::Ident(s) => s.clone(),
                    _ => {
                        return Err(ParseError::new(
                            key_tok.span,
                            "expected lowercase pragma argument name",
                        ));
                    }
                };
                self.expect_eq()?;
                let val_tok = self.bump()?.clone();
                let value = match &val_tok.value {
                    Token::IntLit(v) => *v,
                    _ => {
                        return Err(ParseError::new(
                            val_tok.span,
                            "expected integer pragma argument value",
                        ));
                    }
                };
                args.push(PragmaArg {
                    key,
                    value,
                    span: Span::new(key_tok.span.start, val_tok.span.end),
                });
                if self.eat(&Token::Comma) {
                    continue;
                }
                break;
            }
        }
        let end = self.expect(&Token::RBracket)?;
        Ok(Pragma {
            name,
            args,
            span: Span::new(start, end.end),
        })
    }

    /// Unified actor declaration (D034):
    /// `@Name: [props:] [typeSig:] params = body`
    ///
    /// Forms:
    ///   `@main: @std: Option Int, MainMsg -> Option Int: st msg = ...`
    ///   `@main: @std: st msg = ...`
    ///   `@Counter: Int, CounterMsg -> Option Int: state msg = ...`
    ///   `@Counter: state msg = ...`
    pub(crate) fn parse_actor_item(&mut self) -> Result<ActorDef, ParseError> {
        let start = self.span_here().start;
        self.expect(&Token::At)?;
        let name_tok = self.bump()?.clone();
        let name = match &name_tok.value {
            Token::Ident(s) | Token::UpperIdent(s) => s.clone(),
            _ => {
                return Err(ParseError::new(
                    name_tok.span,
                    "expected actor name after `@`",
                ));
            }
        };

        // Unified syntax: `@name = [@Prop1, @Prop2]: TypeSig: params -> body`
        self.expect_eq()?;

        // Parse optional properties: `@Name` references before the type sig
        // Format: `@name = @Prop1, @Prop2: TypeSig: params -> body`
        let mut properties = Vec::new();
        if matches!(self.peek_kind(), Some(Token::At)) {
            loop {
                self.expect(&Token::At)?;
                let tok = self.bump()?.clone();
                match &tok.value {
                    Token::Ident(s) | Token::UpperIdent(s) => properties.push(s.clone()),
                    _ => {
                        return Err(ParseError::new(
                            tok.span,
                            "expected actor name in properties",
                        ));
                    }
                }
                if self.eat(&Token::Comma) {
                    continue;
                }
                break;
            }
            // Consume the `:` separator after properties
            self.expect(&Token::Colon)?;
        }

        if self.lookahead_has_top_level_colon() {
            // typeSig present: `@name = TypeSig: params -> body`
            let (fields, result_ty, is_func_sig, _) = self.parse_sig_after_colon()?;
            let signature = if is_func_sig {
                Some(Type::Func(
                    fields.into_iter().map(|f| f.ty).collect(),
                    Box::new(result_ty),
                ))
            } else {
                Some(result_ty)
            };
            self.expect(&Token::Colon)?;
            let params = self.parse_param_names()?;
            self.expect(&Token::Arrow)?;
            let body = self.parse_block_or_expr()?;
            let span = Span::new(start, body.span().end);
            Ok(ActorDef {
                name,
                properties,
                signature,
                params,
                body,
                pragma: None,
                span,
            })
        } else {
            // no typeSig: `@name = params -> body`
            let params = self.parse_param_names()?;
            self.expect(&Token::Arrow)?;
            let body = self.parse_block_or_expr()?;
            let span = Span::new(start, body.span().end);
            Ok(ActorDef {
                name,
                properties,
                signature: None,
                params,
                body,
                pragma: None,
                span,
            })
        }
    }

    /// Parse the properties section: comma-separated `@Name` references.
    pub(crate) fn parse_actor_properties(&mut self) -> Result<Vec<String>, ParseError> {
        let mut props = Vec::new();
        loop {
            self.expect(&Token::At)?;
            let tok = self.bump()?.clone();
            match &tok.value {
                Token::Ident(s) | Token::UpperIdent(s) => props.push(s.clone()),
                _ => {
                    return Err(ParseError::new(
                        tok.span,
                        "expected actor name in properties section",
                    ));
                }
            }
            if self.eat(&Token::Comma) {
                continue;
            }
            break;
        }
        Ok(props)
    }

    /// Scan forward for the first depth-0 `:` or `=`.
    /// Returns `Some(true)` if `:` comes first (typeSig present),
    /// `Some(false)` if `=` comes first (no typeSig), `None` if neither.
    pub(crate) fn lookahead_section_delim(&self) -> Option<bool> {
        let mut depth: i32 = 0;
        let mut i = self.pos;
        while i < self.tokens.len() {
            match &self.tokens[i].value {
                Token::LParen | Token::LBracket => depth += 1,
                Token::RParen | Token::RBracket => depth -= 1,
                Token::Colon if depth == 0 => return Some(true),
                Token::Eq if depth == 0 => return Some(false),
                _ => {}
            }
            i += 1;
        }
        None
    }

    /// Consume leading line-boundary (virtual) tokens. Called only after
    /// Skips virtual semis between definitions within a datatype block.
    fn skip_virtual_semis(&mut self) {
        while matches!(
            self.peek_kind(),
            Some(Token::VirtualSemi | Token::VirtualRBrace)
        ) {
            self.pos += 1;
        }
    }

    pub(crate) fn parse_item(&mut self) -> Result<Item, ParseError> {
        let name_tok = self.bump()?.clone();
        let (name, name_is_upper, name_span) = match &name_tok.value {
            Token::Ident(s) => (s.clone(), false, name_tok.span),
            Token::UpperIdent(s) => (s.clone(), true, name_tok.span),
            other => {
                return Err(ParseError::new(
                    name_tok.span,
                    format!("expected top-level name, found {other:?}"),
                ));
            }
        };

        // Method declaration (D056/D057): `TypeName.name = ...` — UpperIdent
        // followed by `. ident`. Calls need no new syntax (`l.append 42` is an
        // ordinary application of a field expression); only the declaration form.
        if name_is_upper && self.peek_kind() == Some(&Token::Dot) {
            return self.parse_method_def(&name, &name_span);
        }

        // Class/Instance (D075): `Name C₁ … Cₙ = ...` — uppercase name followed
        // by bare UpperIdent(s) before the `=`. Datatype heads are empty (nothing
        // between name and `=`), so UpperIdents in the head uniquely identify
        // class/instance declarations. Body shape disambiguates: `ident :` →
        // class, `ident ... =` → instance.
        // Instances may also use a parenthesized type expression: `Name (TypeExpr) = ...`.
        if name_is_upper
            && (matches!(self.peek_kind(), Some(Token::UpperIdent(_)) | Some(Token::LParen)))
        {
            if self.peek_kind() == Some(&Token::LParen) {
                // Instance with parenthesized type: `Name (TypeExpr) = ...`
                let instance_ty = self.parse_type_full(0)?;
                self.expect_eq()?;
                return self.parse_instance_decl(&name, &instance_ty, &name_span);
            }

            let mut head_params = Vec::new();
            while matches!(self.peek_kind(), Some(Token::UpperIdent(_))) {
                let tok = self.bump()?;
                if let Token::UpperIdent(ref s) = tok.value {
                    head_params.push(s.clone());
                }
            }
            self.expect_eq()?;

            // Peek past VirtualLBrace to determine body shape.
            let save_pos = self.pos;
            if self.peek_kind() == Some(&Token::VirtualLBrace) {
                self.pos += 1;
            }
            let is_class_body = match self.tokens.get(self.pos).map(|t| &t.value) {
                // `ident :` → class method signature.
                Some(Token::Ident(_)) => {
                    matches!(self.tokens.get(self.pos + 1).map(|t| &t.value), Some(&Token::Colon))
                }
                // `(op_symbol) :` → class method with operator name.
                Some(Token::LParen) => {
                    // Skip past `)` to find the separator.
                    let mut i = self.pos + 1;
                    while i < self.tokens.len()
                        && !matches!(&self.tokens[i].value, Token::RParen)
                    {
                        i += 1;
                    }
                    matches!(self.tokens.get(i + 1).map(|t| &t.value), Some(Token::Colon))
                }
                _ => false,
            };
            self.pos = save_pos;

            if is_class_body {
                return self.parse_class_decl(&name, &head_params, &name_span);
            } else {
                let ctor_name = head_params
                    .first()
                    .cloned()
                    .ok_or_else(|| ParseError::new(name_span.clone(), "instance requires a type name"))?;
                let instance_ty = Type::Name(ctor_name);
                return self.parse_instance_decl(&name, &instance_ty, &name_span);
            }
        }

        // Unified syntax: `name = ...`
        self.expect_eq()?;

        // Disambiguate by lookahead on the RHS
        // Function type (has `->` at top level) → TypeAlias
        if name_is_upper && self.lookahead_has_top_level_arrow() {
            Self::reject_type_var_name(&name, &name_span)?;
            let ty = self.parse_type_full(0)?;
            let span = Span::new(name_span.start, self.span_here().start);
            return Ok(Item::TypeAlias(TypeAliasDecl { name, ty, span }));
        }
        // Datatype: `Name = Variant1 | Variant2 | ...` or `Name = Variant Fields`.
        if name_is_upper
            && (self.lookahead_has_top_level_pipe() || self.lookahead_first_is_upper())
        {
            return self.parse_datatype_def(&name, &name_span);
        }

        if self.lookahead_has_top_level_colon() {
            // `name = TypeSig:` — what follows the colon decides the binding kind.
            let (fields, result_ty, is_func_sig, constraints) = self.parse_sig_after_colon()?;
            let signature = if is_func_sig {
                Some(Type::Func(
                    fields.into_iter().map(|f| f.ty).collect(),
                    Box::new(result_ty),
                ))
            } else {
                Some(result_ty)
            };
            self.expect(&Token::Colon)?;

            // A function binding is `params -> body`: zero-or-more lowercase-ident
            // params (comma-separated) immediately followed by `->`. Anything else
            // after the colon — an UpperIdent, literal, parenthesized expr, or a
            // var-application — is a bare value whose type was annotated above:
            // a typed value binding `name = TypeSig: <value>`. Under §3.1
            // value/function unity this produces the same AST shape as a nullary
            // function (signature set, no params), so the checker treats the
            // binding's type as its body type.
            if self.lookahead_params_then_arrow() {
                let params = self.parse_param_names()?;
                self.expect(&Token::Arrow)?;
                let body = self.parse_block_or_expr()?;
                let span = Span::new(name_span.start, body.span().end);
                return Ok(Item::Func(FuncDef {
                    name,
                    properties: Vec::new(),
                    signature,
                    constraints,
                    params,
                    body,
                    span,
                }));
            }

            let body = self.parse_block_or_expr()?;
            let span = Span::new(name_span.start, body.span().end);
            return Ok(Item::Func(FuncDef {
                name,
                properties: Vec::new(),
                signature,
                constraints,
                params: Vec::new(),
                body,
                span,
            }));
        }

        if self.lookahead_is_func_no_sig() {
            // Function without type sig: `name = params -> body`
            let params = self.parse_param_names()?;
            self.expect(&Token::Arrow)?;
            let body = self.parse_block_or_expr()?;
            let span = Span::new(name_span.start, body.span().end);
            return Ok(Item::Func(FuncDef {
                name,
                properties: Vec::new(),
                signature: None,
                constraints: Vec::new(),
                params,
                body,
                span,
            }));
        }

        if name_is_upper {
            // Type alias: `Name = TypeExpr`
            Self::reject_type_var_name(&name, &name_span)?;
            let ty = self.parse_type_full(0)?;
            let span = Span::new(name_span.start, self.span_here().start);
            return Ok(Item::TypeAlias(TypeAliasDecl {
                name,
                ty,
                span,
            }));
        }

        // Value: `name = expr`
        let body = self.parse_block_or_expr()?;
        let span = Span::new(name_span.start, body.span().end);
        Ok(Item::Func(FuncDef {
            name,
            properties: Vec::new(),
            signature: None,
            constraints: Vec::new(),
            params: Vec::new(),
            body,
            span,
        }))
    }

    /// Parse a method declaration (D056/D057): `TypeName.name = [TypeSig:] params -> body`.
    ///
    /// The type name has already been consumed; this starts at the `.`. Same
    /// RHS shape as [`Item::Func`] bindings, but a bare value is rejected — a
    /// method must be `[sig:] params -> body`. The receiver is implicit and is
    /// not in `params`.
    fn parse_method_def(&mut self, type_name: &str, _type_span: &Span) -> Result<Item, ParseError> {
        self.expect(&Token::Dot)?;
        let mname_tok = self.bump()?;
        let (mname, mname_span) = match &mname_tok.value {
            Token::Ident(s) => (s.clone(), mname_tok.span),
            other => {
                return Err(ParseError::new(
                    mname_tok.span,
                    format!("expected method name after '.', found {other:?}"),
                ))
            }
        };
        self.expect_eq()?;

        let mut signature = None;
        if self.lookahead_has_top_level_colon() {
            let (fields, result_ty, is_func_sig, _) = self.parse_sig_after_colon()?;
            signature = Some(if is_func_sig {
                Type::Func(
                    fields.into_iter().map(|f| f.ty).collect(),
                    Box::new(result_ty),
                )
            } else {
                result_ty
            });
            self.expect(&Token::Colon)?;
        }

        if !self.lookahead_params_then_arrow() && signature.is_none() {
            return Err(ParseError::new(
                mname_span,
                "method requires a type signature or `params -> body` form",
            ));
        }

        let params = self.parse_param_names()?;
        self.expect(&Token::Arrow)?;
        // Method bodies admit write statements (`path <- expr`, D056/D057).
        self.method_body_depth += 1;
        let body_result = self.parse_block_or_expr();
        self.method_body_depth -= 1;
        let body = body_result?;
        // D059: method bodies are call-free — only constructor applications.
        // Exception: prelude-declared builtin types (Arr) may call builtins
        // from their methods to expose a safe API over raw primitives.
        if type_name != "Arr" {
            if let Some(call_span) = find_method_call(&body) {
                return Err(ParseError::new(
                    call_span,
                    format!(
                        "method `{type_name}.{mname}` is call-free (D059): only constructor \
                         applications are allowed in a method body"
                    ),
                ));
            }
        }
        let span = Span::new(mname_span.start, body.span().end);
        Ok(Item::Method(MethodDef {
            type_name: type_name.to_string(),
            name: mname,
            signature,
            params,
            body,
            span,
        }))
    }

    /// Parse a datatype definition: `Name = Variant1 | Variant2 | ...`
    /// Each variant is `UpperIdent [TypeArg ...]` (whitespace-separated fields).
    fn parse_datatype_def(&mut self, name: &str, name_span: &Span) -> Result<Item, ParseError> {
        Self::reject_type_var_name(name, name_span)?;
        let mut ctors = Vec::new();
        let mut type_vars: Vec<String> = Vec::new();

        loop {
            // Variant name (uppercase). A non-uppercase token ends the variant list.
            if !self.lookahead_first_is_upper() {
                break;
            }
            let ctor_tok = self.bump()?;
            let (ctor_name, ctor_span) = match &ctor_tok.value {
                Token::UpperIdent(s) => (s.clone(), ctor_tok.span),
                _ => unreachable!("lookahead_first_is_upper gate"),
            };

            // Optional field types (whitespace-separated, stop at `|` or item boundary).
            // Each field is optionally labeled: `label Type` or just `Type`.
            let mut fields = Vec::new();
            while (self.starts_type_atom_or_var() || matches!(self.peek_kind(), Some(Token::Amp)))
                && self.peek_kind() != Some(&Token::Pipe)
                && !self.looks_like_item_start()
            {
                // Check for labeled field: lowercase ident followed by a type atom
                let label = if let Some(Token::Ident(lbl)) = self.peek_kind().cloned() {
                    // Peek ahead: if next token starts a type atom, this is a label
                    let next_is_type = self
                        .tokens
                        .get(self.pos + 1)
                        .is_some_and(|t| {
                            matches!(
                                t.value,
                                Token::UpperIdent(_) | Token::LParen | Token::Amp
                            )
                        });
                    if next_is_type {
                        self.pos += 1; // consume label
                        Some(lbl)
                    } else {
                        None
                    }
                } else {
                    None
                };

                let ty = if matches!(self.peek_kind(), Some(Token::Amp)) {
                    // `&List Int` — borrow of a full type application
                    self.parse_param_t(0)?
                } else {
                    // Single type atom (e.g. `Int`, `Actor`, `T`) or parenthesized
                    // application (e.g. `(List String)`). Non-parenthesized type
                    // applications are not supported in constructor fields — use
                    // parens to disambiguate from adjacent unlabeled fields.
                    self.parse_type_atom(0)?
                };
                // Collect type vars (single uppercase letters), recursing into
                // nested positions like `Ref (LNode T)` — a parametric type may
                // mention its params only inside field types.
                Self::collect_type_var_names(&ty, &mut type_vars);
                fields.push(FieldDecl {
                    label,
                    ty,
                });
            }

            ctors.push(ConstructorDecl {
                name: ctor_name,
                fields,
                result_type: Type::Name(name.to_string()), // fixed below
                span: ctor_span,
            });

            if !self.eat(&Token::Pipe) {
                break;
            }
        }

        if ctors.is_empty() {
            return Err(ParseError::new(
                name_span.clone(),
                format!("`{name}`: expected a variant (uppercase identifier)"),
            ));
        }

        // Set result types to `Name T1 T2 ...`
        for ctor in &mut ctors {
            if type_vars.is_empty() {
                ctor.result_type = Type::Name(name.to_string());
            } else {
                ctor.result_type = Type::App(
                    Box::new(Type::Name(name.to_string())),
                    type_vars.iter().map(|v| Type::Name(v.clone())).collect(),
                );
            }
        }

        let span = Span::new(name_span.start, self.span_here().start);
        Ok(Item::ParametricType(ParametricTypeDecl {
            name: name.to_string(),
            type_vars,
            ctors,
            span,
        }))
    }

    /// Parse a class declaration body (D075, §6.10). Called after the head
    /// (`Name C₁ … Cₙ =`) has been consumed; positioned at VirtualLBrace.
    fn parse_class_decl(
        &mut self,
        name: &str,
        params: &[String],
        name_span: &Span,
    ) -> Result<Item, ParseError> {
        self.expect(&Token::VirtualLBrace)?;

        let mut methods = Vec::new();
        loop {
            if self.peek_kind() == Some(&Token::VirtualRBrace) {
                break;
            }

            // Method name: lowercase ident or `(op_symbol)`.
            let (mname, mname_span) = if self.peek_kind() == Some(&Token::LParen) {
                let lparen_start = self.bump()?.span.start;
                let op_tok = self.bump()?;
                if !Self::is_op_symbol(&op_tok.value) {
                    return Err(ParseError::new(
                        op_tok.span,
                        format!("expected operator symbol in class method name, found {:?}", op_tok.value),
                    ));
                }
                let mname = Self::op_symbol_str(&op_tok.value).to_string();
                let rparen_tok = self.bump()?;
                if !matches!(rparen_tok.value, Token::RParen) {
                    return Err(ParseError::new(
                        rparen_tok.span,
                        format!("expected ')' after operator symbol in class method name, found {:?}", rparen_tok.value),
                    ));
                }
                (mname, Span::new(lparen_start, rparen_tok.span.end))
            } else {
                let mname_tok = self.bump()?;
                match &mname_tok.value {
                    Token::Ident(s) => (s.clone(), mname_tok.span),
                    other => {
                        return Err(ParseError::new(
                            mname_tok.span,
                            format!("expected method name in class body, found {other:?}"),
                        ))
                    }
                }
            };

            // Colon separating name from signature.
            self.expect(&Token::Colon)?;

            // Type signature: `A, B -> C` or bare `T`.
            let (fields, result_ty, is_func_sig, _) = self.parse_sig_after_colon()?;
            let sig = if is_func_sig {
                Type::Func(
                    fields.into_iter().map(|f| f.ty).collect(),
                    Box::new(result_ty),
                )
            } else {
                result_ty
            };

            methods.push(ClassMethod {
                name: mname,
                sig,
                span: mname_span,
            });

            // Separator or end of block.
            if self.peek_kind() == Some(&Token::VirtualRBrace) {
                break;
            }
            self.expect(&Token::VirtualSemi)?;
        }

        self.expect(&Token::VirtualRBrace)?;

        let span = Span::new(name_span.start, self.span_here().start);
        Ok(Item::Class(ClassDecl {
            name: name.to_string(),
            params: params.to_vec(),
            methods,
            span,
        }))
    }

    /// Parse an instance declaration body (D075, §6.10). Called after the head
    /// (`Name TypeExpr =`) has been consumed; positioned at VirtualLBrace.
    fn parse_instance_decl(
        &mut self,
        name: &str,
        instance_ty: &Type,
        name_span: &Span,
    ) -> Result<Item, ParseError> {
        self.expect(&Token::VirtualLBrace)?;

        let mut funcs: Vec<FuncDef> = Vec::new();
        loop {
            if self.peek_kind() == Some(&Token::VirtualRBrace) {
                break;
            }

            // Function name: lowercase ident or `(op_symbol)`.
            let (fname, fname_span) = if self.peek_kind() == Some(&Token::LParen) {
                let lparen_start = self.bump()?.span.start;
                let op_tok = self.bump()?;
                if !Self::is_op_symbol(&op_tok.value) {
                    return Err(ParseError::new(
                        op_tok.span,
                        format!("expected operator symbol in instance method name, found {:?}", op_tok.value),
                    ));
                }
                let fname = Self::op_symbol_str(&op_tok.value).to_string();
                let rparen_tok = self.bump()?;
                if !matches!(rparen_tok.value, Token::RParen) {
                    return Err(ParseError::new(
                        rparen_tok.span,
                        format!("expected ')' after operator symbol in instance method name, found {:?}", rparen_tok.value),
                    ));
                }
                (fname, Span::new(lparen_start, rparen_tok.span.end))
            } else {
                let fname_tok = self.bump()?;
                match &fname_tok.value {
                    Token::Ident(s) => (s.clone(), fname_tok.span),
                    other => {
                        return Err(ParseError::new(
                            fname_tok.span,
                            format!("expected method name in instance body, found {other:?}"),
                        ))
                    }
                }
            };

            // Two forms: `name = TypeSig: params -> body` (with signature,
            // optionally constrained `C T => …`, §3.12) or `name params = body`.
            let (mut signature, mut constraints) = (None, Vec::new());
            let (params, body) = if self.peek_kind() == Some(&Token::Eq) {
                self.pos += 1; // consume `=`
                let (fields, result_ty, is_func_sig, c) = self.parse_sig_after_colon()?;
                signature = Some(if is_func_sig {
                    Type::Func(
                        fields.into_iter().map(|f| f.ty).collect(),
                        Box::new(result_ty),
                    )
                } else {
                    result_ty
                });
                constraints = c;
                self.expect(&Token::Colon)?;
                let mut ps = Vec::new();
                while matches!(self.peek_kind(), Some(Token::Ident(_))) {
                    let tok = self.bump()?;
                    if let Token::Ident(ref s) = tok.value {
                        ps.push(s.clone());
                    }
                }
                self.expect(&Token::Arrow)?;
                let b = self.parse_block_or_expr()?;
                (ps, b)
            } else {
                // Param names: consecutive lowercase idents until `=`.
                let mut ps = Vec::new();
                while matches!(self.peek_kind(), Some(Token::Ident(_))) {
                    let tok = self.bump()?;
                    if let Token::Ident(ref s) = tok.value {
                        ps.push(s.clone());
                    }
                }

                // `=` separating params from body.
                self.expect_eq()?;

                // Body expression (may be a multi-line block).
                let b = self.parse_block_or_expr()?;
                (ps, b)
            };
            let body_end = body.span().end;

            funcs.push(FuncDef {
                name: fname,
                properties: Vec::new(),
                signature,
                constraints,
                params,
                body,
                span: Span::new(fname_span.start, body_end),
            });

            // Separator or end of block.
            if self.peek_kind() == Some(&Token::VirtualRBrace) {
                break;
            }
            self.expect(&Token::VirtualSemi)?;
        }

        self.expect(&Token::VirtualRBrace)?;

        let span = Span::new(name_span.start, self.span_here().start);
        Ok(Item::Instance(InstanceDecl {
            class_name: name.to_string(),
            instance_ty: instance_ty.clone(),
            methods: funcs,
            helpers: Vec::new(),
            span,
        }))
    }

    /// Tokens valid as a parenthesized operator name: `(+)`, `(-)`, `(==)`, …
    pub(crate) fn is_op_symbol(tok: &Token) -> bool {
        matches!(
            tok,
            Token::Plus
                | Token::Minus
                | Token::Star
                | Token::Slash
                | Token::Percent
                | Token::Gt
                | Token::Lt
                | Token::Ge
                | Token::Le
                | Token::EqEq
                | Token::Ne
                | Token::Bang
                | Token::Caret
                | Token::Pipe
                | Token::Amp
                | Token::LtLt
                | Token::GtGt
        )
    }

    /// Surface spelling of an operator-symbol token (for class method names).
    fn op_symbol_str(tok: &Token) -> &'static str {
        match tok {
            Token::Plus => "+",
            Token::Minus => "-",
            Token::Star => "*",
            Token::Slash => "/",
            Token::Percent => "%",
            Token::Gt => ">",
            Token::Lt => "<",
            Token::Ge => ">=",
            Token::Le => "<=",
            Token::EqEq => "==",
            Token::Ne => "!=",
            Token::Bang => "!",
            Token::Caret => "^",
            Token::Pipe => "|",
            Token::Amp => "&",
            Token::LtLt => "<<",
            Token::GtGt => ">>",
            _ => unreachable!("is_op_symbol gate"),
        }
    }

    /// Check if the first upcoming token is an uppercase identifier (constructor name).
    fn lookahead_first_is_upper(&self) -> bool {
        self.tokens
            .get(self.pos)
            .is_some_and(|t| matches!(t.value, Token::UpperIdent(_)))
    }

    /// Check if there's a `|` at depth 0 in the upcoming tokens (before item boundary).
    fn lookahead_has_top_level_pipe(&self) -> bool {
        let mut i = self.pos;
        let mut depth: i32 = 0;
        while i < self.tokens.len() {
            match &self.tokens[i].value {
                Token::LParen | Token::LBracket => depth += 1,
                Token::RParen | Token::RBracket => depth -= 1,
                Token::Pipe if depth == 0 => return true,
                Token::At
                | Token::Eq
                | Token::VirtualSemi
                | Token::VirtualRBrace
                | Token::VirtualLBrace => return false,
                _ => {}
            }
            i += 1;
        }
        false
    }

    /// Check if there's a `->` at depth 0 in the upcoming tokens (before item boundary).
    /// Used to detect function types (TypeAlias) vs datatypes (ParametricType).
    fn lookahead_has_top_level_arrow(&self) -> bool {
        let mut i = self.pos;
        let mut depth: i32 = 0;
        while i < self.tokens.len() {
            match &self.tokens[i].value {
                Token::LParen | Token::LBracket => depth += 1,
                Token::RParen | Token::RBracket => depth -= 1,
                Token::Arrow if depth == 0 => return true,
                Token::At
                | Token::Eq
                | Token::VirtualSemi
                | Token::VirtualRBrace
                | Token::VirtualLBrace => return false,
                _ => {}
            }
            i += 1;
        }
        false
    }

    /// Check if there's a `:` at depth 0 in the upcoming tokens (before item boundary).
    fn lookahead_has_top_level_colon(&self) -> bool {
        let mut i = self.pos;
        let mut depth: i32 = 0;
        while i < self.tokens.len() {
            match &self.tokens[i].value {
                Token::LParen | Token::LBracket => depth += 1,
                Token::RParen | Token::RBracket => depth -= 1,
                Token::Colon if depth == 0 => return true,
                Token::At
                | Token::Eq
                | Token::VirtualSemi
                | Token::VirtualRBrace
                | Token::VirtualLBrace => return false,
                _ => {}
            }
            i += 1;
        }
        false
    }

    /// Check if upcoming tokens look like `param1, param2, ... -> ...`
    fn lookahead_is_func_no_sig(&self) -> bool {
        if !matches!(
            self.tokens.get(self.pos),
            Some(t) if matches!(t.value, Token::Ident(_))
        ) {
            return false;
        }
        let mut i = self.pos;
        while i < self.tokens.len() {
            match &self.tokens[i].value {
                Token::Ident(_) | Token::Comma => i += 1,
                Token::Arrow => return true,
                Token::At
                | Token::Eq
                | Token::VirtualSemi
                | Token::VirtualRBrace => return false,
                _ => return false,
            }
        }
        false
    }

    /// Check if upcoming tokens look like `param1, param2, ... -> ...`.
    /// Like `lookahead_is_func_no_sig`, but also accepts an empty param list
    /// (a leading `->`, the nullary-function form) so it can distinguish a
    /// function binding from a typed value binding after a type-sig colon.
    fn lookahead_params_then_arrow(&self) -> bool {
        let mut i = self.pos;
        let mut depth: i32 = 0;
        loop {
            match &self.tokens.get(i).map(|t| &t.value) {
                Some(Token::Ident(_)) | Some(Token::Comma) => i += 1,
                Some(Token::Arrow) if depth == 0 => return true,
                Some(Token::At
                | Token::Eq
                | Token::VirtualSemi
                | Token::VirtualRBrace
                | Token::VirtualLBrace) => return false,
                Some(Token::LParen | Token::LBracket) => { depth += 1; i += 1 }
                Some(Token::RParen | Token::RBracket) => { depth -= 1; i += 1 }
                _ => return false,
            }
        }
    }

    /// Parse a comma-separated list of lowercase property keywords.
    /// Returns the list of property names.
    pub(crate) fn parse_properties(&mut self) -> Result<Vec<String>, ParseError> {
        let mut props = Vec::new();
        loop {
            let tok = self.bump()?;
            match &tok.value {
                Token::Ident(s) => props.push(s.clone()),
                _ => {
                    return Err(ParseError::new(
                        tok.span,
                        "expected property keyword (lowercase identifier)",
                    ));
                }
            }
            if !self.eat(&Token::Comma) {
                break;
            }
        }
        Ok(props)
    }

    pub(crate) fn expect_eq(&mut self) -> Result<(), ParseError> {
        match self.peek_kind() {
            Some(Token::Eq) => {
                self.pos += 1;
                Ok(())
            }
            _ => Err(ParseError::new(self.span_here(), "expected `=` in equation")),
        }
    }

    /// Reject single uppercase letter names (A–Z) as type constructors.
    /// Single uppercase letters are reserved for type variables.
    fn reject_type_var_name(name: &str, span: &Span) -> Result<(), ParseError> {
        if name.len() == 1 && name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
            return Err(ParseError::new(
                *span,
                format!(
                    "`{name}` is a single uppercase letter — reserved for type variables. \
                     Use a multi-letter name for type constructors."
                ),
            ));
        }
        Ok(())
    }

    /// Collect single-uppercase-letter type-variable names from a surface
    /// type, recursing into `Ref`/application/product positions, in
    /// first-occurrence order (deduplicated).
    fn collect_type_var_names(ty: &Type, out: &mut Vec<String>) {
        match ty {
            Type::Name(n) => {
                if n.len() == 1
                    && n.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                    && !out.contains(n)
                {
                    out.push(n.clone());
                }
            }
            Type::Borrow(inner) => Self::collect_type_var_names(inner, out),
            Type::App(head, args) => {
                Self::collect_type_var_names(head, out);
                for a in args {
                    Self::collect_type_var_names(a, out);
                }
            }
            Type::Product(types) => {
                for t in types {
                    Self::collect_type_var_names(t, out);
                }
            }
            // Function types never appear as datatype field types.
            Type::Func(_, _) => {}
        }
    }

    pub(crate) fn parse_param_names(&mut self) -> Result<Vec<String>, ParseError> {
        let mut params = Vec::new();
        loop {
            match self.peek_kind() {
                Some(Token::Ident(s)) => {
                    params.push(s.clone());
                    self.pos += 1;
                }
                Some(Token::IdentAt(s)) => {
                    // Handle-name param: `x@` marks an actor-typed parameter (§2.2).
                    params.push(s.clone());
                    self.pos += 1;
                }
                _ => break,
            }
            if !self.eat(&Token::Comma) {
                break;
            }
        }
        Ok(params)
    }

    /// Check if a token kind is a valid type variable (lowercase ident or single uppercase letter).
    fn is_type_var_token(kind: Option<&Token>) -> bool {
        kind.is_some_and(Self::is_type_var_token_val)
    }

    fn is_type_var_token_val(t: &Token) -> bool {
        match t {
            Token::Ident(_) => true,
            Token::UpperIdent(s) => s.len() == 1 && s.chars().next().is_some_and(|c| c.is_ascii_uppercase()),
            _ => false,
        }
    }

    /// Parse type variable names: lowercase idents OR single uppercase letters (A-Z).
    pub(crate) fn parse_type_var_names(&mut self) -> Result<Vec<String>, ParseError> {
        let mut vars = Vec::new();
        loop {
            match self.peek_kind() {
                Some(Token::Ident(s)) => {
                    vars.push(s.clone());
                    self.pos += 1;
                }
                Some(Token::UpperIdent(s)) if s.len() == 1 && s.chars().next().is_some_and(|c| c.is_ascii_uppercase()) => {
                    vars.push(s.clone());
                    self.pos += 1;
                }
                _ => break,
            }
        }
        if vars.is_empty() {
            let span = self.span_here();
            return Err(ParseError::new(span, "expected type variable (single uppercase letter or lowercase ident)"));
        }
        Ok(vars)
    }

    /// Parse a constructor block for a parametric type declaration.
    ///
    /// ```wag
    /// MyPair a b
    ///   MyPair: a, b
    /// ```
    ///
    /// Each constructor line is `UpperIdent` or `UpperIdent : fields`.
    /// The result type of each constructor is the enclosing type name.
    ///
    /// Constructor lines are indented deeper than the header. We parse them
    /// by scanning the token stream: a constructor line starts with an
    /// `UpperIdent` on a new line. The block ends when we hit a token on a
    /// line at or shallower than the header's column (or a non-constructor
    /// token).
    pub(crate) fn parse_ctor_block(&mut self, type_name: &str, type_vars: &[String], header_line: usize, header_col: usize) -> Result<Vec<ConstructorDecl>, ParseError> {
        let mut ctors = Vec::new();

        loop {
            // Check if the next token starts a new constructor line.
            // A constructor line starts with an UpperIdent on a line > header_line
            // AND at a column > header_col (indented deeper).
            let next_tok = self.tokens.get(self.pos);
            let is_ctor_start = match next_tok {
                Some(t) if matches!(t.value, Token::UpperIdent(_)) => {
                    let (line, col) = self.line_index.line_col_at(t.span.start);
                    line > header_line && col > header_col
                }
                _ => false,
            };
            if !is_ctor_start {
                break;
            }

            // Constructor name (uppercase)
            let ctor_tok = self.bump()?;
            let (ctor_name, ctor_span) = match &ctor_tok.value {
                Token::UpperIdent(s) => (s.clone(), ctor_tok.span),
                other => {
                    return Err(ParseError::new(
                        ctor_tok.span,
                        format!("expected constructor name, found {other:?}"),
                    ))
                }
            };
            let mut fields = Vec::new();
            if self.eat(&Token::Colon) {
                let first = self.parse_labeled_param()?;
                fields.push(first);
                while self.eat(&Token::Comma) {
                    fields.push(self.parse_labeled_param()?);
                }
            }
            // Result type: type name applied to its type variables (e.g. `List T`).
            let result_type = if type_vars.is_empty() {
                Type::Name(type_name.to_string())
            } else {
                Type::App(
                    Box::new(Type::Name(type_name.to_string())),
                    type_vars.iter().map(|v| Type::Name(v.clone())).collect(),
                )
            };
            ctors.push(ConstructorDecl {
                name: ctor_name,
                fields,
                result_type,
                span: ctor_span,
            });

            if ctors.is_empty() {
                return Err(ParseError::new(
                    self.span_here(),
                    "parametric type must have at least one constructor",
                ));
            }
        }

        if ctors.is_empty() {
            return Err(ParseError::new(
                self.span_here(),
                "parametric type must have at least one constructor",
            ));
        }

        Ok(ctors)
    }

    /// After `:`, parse either bare `T` or `ParamList -> T`.
    /// Returns (fields, result_type, is_func_sig).
    /// Parses a signature after `:`. Returns (params, result, is_func_sig, constraints).
    /// Constraints (§3.12): a comma-separated class list followed by `=>`
    /// (`Eq T, Fold T => A, B -> C`); the list reuses the type grammar.
    pub(crate) fn parse_sig_after_colon(
        &mut self,
    ) -> Result<(Vec<FieldDecl>, Type, bool, Vec<Type>), ParseError> {
        let first = self.parse_labeled_param()?;
        let mut fields = vec![first];

        while self.eat(&Token::Comma) {
            fields.push(self.parse_labeled_param()?);
        }

        // `=>` after the type list: those types are class constraints, and the
        // real parameter list follows (§3.12). `=>` is legal only in Sig position.
        if matches!(self.peek_kind(), Some(Token::FatArrow)) {
            if fields.iter().any(|f| f.label.is_some()) {
                return Err(ParseError::new(
                    self.span_here(),
                    "labeled parameters are not allowed in a constraint list",
                ));
            }
            let constraints: Vec<Type> = fields.drain(..).map(|f| f.ty).collect();
            self.pos += 1; // consume `=>`
            let first = self.parse_labeled_param()?;
            let mut params = vec![first];
            while self.eat(&Token::Comma) {
                params.push(self.parse_labeled_param()?);
            }
            if !self.eat_arrow() {
                return Err(ParseError::new(
                    self.span_here(),
                    "expected `->` after parameter list in constrained signature",
                ));
            }
            let result = self.parse_type(0)?;
            return Ok((params, result, true, constraints));
        }

        if self.eat_arrow() {
            let result = self.parse_type(0)?;
            Ok((fields, result, true, Vec::new()))
        } else if fields.len() == 1 && fields[0].label.is_none() {
            // bare T
            let ty = fields.pop().unwrap().ty;
            Ok((Vec::new(), ty, false, Vec::new()))
        } else {
            Err(ParseError::new(
                self.span_here(),
                "expected `->` after parameter list in signature",
            ))
        }
    }

    pub(crate) fn eat_arrow(&mut self) -> bool {
        matches!(self.peek_kind(), Some(Token::Arrow)) && {
            self.pos += 1;
            true
        }
    }

    pub(crate) fn parse_labeled_param(&mut self) -> Result<FieldDecl, ParseError> {
        // Optional `label :` where label is lowercase ident and next is Colon
        // before a type. Types never start with lowercase, so:
        //   ident : type  => labeled
        //   type          => unlabeled
        // ParamT allows a leading `&` (borrow); nested `T` arguments do not (TYPE-13/14).
        if let Some(Token::Ident(label)) = self.peek_kind().cloned() {
            // Peek ahead for Colon
            if self.tokens.get(self.pos + 1).map(|t| &t.value) == Some(&Token::Colon) {
                self.pos += 2; // ident + colon
                let ty = self.parse_param_t(0)?;
                return Ok(FieldDecl {
                    label: Some(label),
                    ty,
                });
            }
        }
        let ty = self.parse_param_t(0)?;
        Ok(FieldDecl { label: None, ty })
    }

    /// `ParamT` = `&`? `T` — borrow only at the parameter surface (§3.1).
    /// `open_depth` counts enclosing parens still open around this type; it
    /// gates the `(A, B) -> R` product-arrow form (see parse_type_atom).
    pub(crate) fn parse_param_t(&mut self, open_depth: i32) -> Result<Type, ParseError> {
        if matches!(self.peek_kind(), Some(Token::Amp)) {
            self.pos += 1;
            // `&List Int` borrows the full application, not only `List`
            let inner = self.parse_type(open_depth)?;
            Ok(Type::Borrow(Box::new(inner)))
        } else {
            self.parse_type(open_depth)
        }
    }

    /// Parse a type `T` (not a bare top-level `FuncT` unless parenthesized).
    /// `&T` is **not** part of `T` — only of `ParamT` (TYPE-13/14).
    pub(crate) fn parse_type(&mut self, open_depth: i32) -> Result<Type, ParseError> {
        self.parse_type_app(open_depth)
    }

    /// Parse a full type expression, including function types (`T -> U`).
    /// Used in type alias context where `->` is not part of a signature.
    pub(crate) fn parse_type_full(&mut self, open_depth: i32) -> Result<Type, ParseError> {
        let head = self.parse_type_app(open_depth)?;
        if self.eat_arrow() {
            let result = self.parse_type_full(open_depth)?;
            Ok(Type::Func(vec![head], Box::new(result)))
        } else {
            Ok(head)
        }
    }

    pub(crate) fn parse_type_app(&mut self, open_depth: i32) -> Result<Type, ParseError> {
        let head = self.parse_type_atom(open_depth)?;
        let mut args = Vec::new();
        while self.starts_type_atom_or_var() && !self.looks_like_item_start() {
            // Don't swallow the next top-level `Name:` / equation as a type arg
            args.push(self.parse_type_atom(open_depth)?);
        }
        if args.is_empty() {
            Ok(head)
        } else {
            Ok(Type::App(Box::new(head), args))
        }
    }

    pub(crate) fn starts_type_atom(&self) -> bool {
        matches!(
            self.peek_kind(),
            Some(Token::UpperIdent(_) | Token::LParen)
        )
    }

    /// Like `starts_type_atom` but also matches lowercase idents (type variables
    /// in parametric type context, e.g. `Tree a`). Used in type-argument
    /// position only — NOT in signature detection where `Ident` means a param.
    pub(crate) fn starts_type_atom_or_var(&self) -> bool {
        matches!(
            self.peek_kind(),
            Some(Token::UpperIdent(_) | Token::Ident(_) | Token::LParen)
        )
    }

    pub(crate) fn parse_type_atom(&mut self, open_depth: i32) -> Result<Type, ParseError> {
        match self.peek_kind() {
            Some(Token::LParen) => {
                self.pos += 1;
                // Parenthesized: (T), (ParamList -> T), or after (A, B) a following
                // `-> U` outside the inner list for nested HO types:
                // `((State, Msg) -> Option State)`.
                // Params live one paren deeper than this group.
                let first = self.parse_param_t(open_depth + 1)?;
                let mut params = vec![first];
                while self.eat(&Token::Comma) {
                    params.push(self.parse_param_t(open_depth + 1)?);
                }
                if self.eat_arrow() {
                    // Still inside this paren (not yet closed): enclosing depth
                    // is open_depth + 1.
                    let result = self.parse_type(open_depth + 1)?;
                    self.expect(&Token::RParen)?;
                    return Ok(Type::Func(params, Box::new(result)));
                }
                self.expect(&Token::RParen)?;
                if params.len() == 1 {
                    let ty = params.pop().unwrap();
                    if matches!(ty, Type::Borrow(_)) {
                        return Err(ParseError::new(
                            self.span_here(),
                            "`&T` is only legal in parameter position (ParamT), not as a general type T",
                        ));
                    }
                    // `(T)` is pure grouping. Do **not** consume a following `->`
                    // here: that would steal the arrow from signatures like
                    // `List (List Int) -> MainMsg` (the paren is a type argument).
                    // Nested function types write the arrow inside parens before
                    // `RParen`, or use multi-param form `(A, B) -> R` below.
                    return Ok(ty);
                }
                // `(A, B) -> R` with arrow outside the pair-parens — required so
                // `((State, Msg) -> Option State)` parses (inner pair closes,
                // then arrow, then result, then outer paren).
                // Only valid when an enclosing paren is still open (open_depth >=
                // 1); otherwise this `->` belongs to the surrounding signature or
                // function type, e.g. `List (T, U) -> R`.
                if open_depth >= 1 && self.eat_arrow() {
                    let result = self.parse_type(open_depth)?;
                    return Ok(Type::Func(params, Box::new(result)));
                }
                // `(A, B)` without arrow is a product type
                Ok(Type::Product(params))
            }
            Some(Token::UpperIdent(name)) | Some(Token::Ident(name)) => {
                let name = name.clone();
                self.pos += 1;
                Ok(Type::Name(name))
            }
            _ => Err(ParseError::new(
                self.span_here(),
                format!("expected type, found {:?}", self.peek_kind()),
            )),
        }
    }

    /// Inside `(...)`: parse T or ParamList -> T (labels not expected in FuncT params usually).
    pub(crate) fn parse_sig_after_colon_or_type_in_parens(
        &mut self,
    ) -> Result<(Vec<FieldDecl>, Type, bool), ParseError> {
        // Param slots are ParamT (`&T` allowed); the result after `->` is plain T.
        let first = self.parse_param_t(0)?;
        let mut params = vec![FieldDecl {
            label: None,
            ty: first,
        }];
        while self.eat(&Token::Comma) {
            params.push(FieldDecl {
                label: None,
                ty: self.parse_param_t(0)?,
            });
        }
        if self.eat_arrow() {
            let result = self.parse_type(0)?;
            Ok((params, result, true))
        } else if params.len() == 1 {
            // Bare `(T)` — if the sole "param" was a borrow, reject (borrow not in T).
            let ty = params.pop().unwrap().ty;
            if matches!(ty, Type::Borrow(_)) {
                return Err(ParseError::new(
                    self.span_here(),
                    "`&T` is only legal in parameter position (ParamT), not as a general type T",
                ));
            }
            Ok((Vec::new(), ty, false))
        } else {
            Err(ParseError::new(
                self.span_here(),
                "expected `->` in parenthesized function type",
            ))
        }
    }
}

/// D059 walk: find the first call in a method body whose callee is not a
/// constructor (`Expr::Upper`). Bare field reads / nullary methods are legal —
/// only applications of non-constructor funs are rejected.
fn find_method_call(expr: &Expr) -> Option<Span> {
    use Expr::*;
    match expr {
        Apply { fun, args, span } => {
            if !matches!(fun.as_ref(), Upper { .. }) {
                return Some(*span);
            }
            args.iter().find_map(find_method_call)
        }
        Int { .. } | Str { .. } | Var { .. } | Upper { .. } | AtName { .. } | Self_ { .. } | Unit { .. } => None,
        BinOp { lhs, rhs, .. } => find_method_call(lhs).or_else(|| find_method_call(rhs)),
        Field { base, .. } => find_method_call(base),
        RecordUpdate { receiver, clauses, .. } => {
            let mut it = find_method_call(receiver);
            for c in clauses {
                if let Some(s) = match c {
                    UpdateClause::Leaf { value, .. } => find_method_call(value),
                    UpdateClause::Group { clauses, .. } => clauses.iter().find_map(|c| match c {
                        UpdateClause::Leaf { value, .. } => find_method_call(value),
                        UpdateClause::Group { .. } => None, // desugared before this walk
                    }),
                } {
                    it = Some(s);
                    break;
                }
            }
            it
        },
        SetField { receiver, value, .. } => find_method_call(receiver).or_else(|| find_method_call(value)),
        RegionWrite { receiver, value, .. } => find_method_call(receiver).or_else(|| find_method_call(value)),
        Borrow { inner, .. } => find_method_call(inner),
        Bind { value, body, .. } => find_method_call(value).or_else(|| find_method_call(body)),
        Seq { first, second, .. } => find_method_call(first).or_else(|| find_method_call(second)),
        If { arms, .. } => arms.iter().find_map(|a| {
            a.condition.as_ref().and_then(find_method_call).or_else(|| find_method_call(&a.body))
        }),
        Match { scrutinee, arms, .. } => {
            let mut it = find_method_call(scrutinee);
            for arm in arms {
                if let Some(s) = find_method_call(&arm.body) {
                    it = Some(s);
                    break;
                }
            }
            it
        }
        Do { stmts, .. } => stmts.iter().find_map(|s| match s {
            DoStmt::Bind { expr, .. } => find_method_call(expr),
            DoStmt::Return(e) => find_method_call(e),
        }),
        ListLit { elems, .. } | Product { elems, .. } => elems.iter().find_map(find_method_call),
        Lambda { body, .. } => find_method_call(body),
        Send { target, msg, .. } => find_method_call(target).or_else(|| find_method_call(msg)),
        Comprehension { .. } => None, // desugared before this walk
    }
}
