//! Expression parsing — Pratt parser, application, records, lambdas, lists
//! (grammar fragment of `parser.rs`).
//!
//! `impl Parser` methods: `parse_expr`, `parse_expr_ex`, `parse_prefix`,
//! `parse_primary`, `parse_with`, `parse_clause_block`, `parse_list_lit`,
//! and their helpers.

use super::{ParseError, Parser};
use crate::ast::*;
use crate::token::Token;
use crate::Span;

use super::{expr_can_be_apply_head, is_compare_token};

/// A single item in a layout block: a `name = value` binding (retains a value,
/// desugars to `Expr::Bind`) or a bare expression statement (a send, a write,
/// … — desugars to `Expr::Seq`, its value discarded). §6.5 block bindings.
enum BlockItem {
    Bind(String, Box<Expr>, Span),
    Stmt(Box<Expr>, Span),
}

impl<'a> Parser<'a> {
    /// Parse a block (`{ items … final }`) or a plain expression.
    ///
    /// A block is a layout-delimited sequence of items — `name = value`
    /// bindings and bare expression statements (sends, writes) — followed by a
    /// final expression. Desugars to nested `Expr::Bind` / `Expr::Seq` (§6.5).
    pub(crate) fn parse_block_or_expr(&mut self) -> Result<Expr, ParseError> {
        if self.peek_kind() == Some(&Token::VirtualLBrace) {
            self.pos += 1; // consume VirtualLBrace
            let mut items: Vec<BlockItem> = Vec::new();
            loop {
                // Check for binding: Ident or IdentAt followed by Eq (§2.2: handles carry @)
                if let (Some(t0), Some(t1)) = (self.tokens.get(self.pos), self.tokens.get(self.pos + 1)) {
                    if t1.value == Token::Eq {
                        if let Some((name, is_handle)) = match &t0.value {
                            Token::Ident(n) => Some((n.clone(), false)),
                            Token::IdentAt(n) => Some((n.clone(), true)),
                            _ => None,
                        } {
                            // §2.2: actor-creation RHS requires @ on the bound name.
                            if !is_handle {
                                let rhs_is_actor = matches!(self.tokens.get(self.pos + 2).map(|t| &t.value), Some(Token::At))
                                    && matches!(self.tokens.get(self.pos + 3).map(|t| &t.value),
                                        Some(Token::Ident(_)) | Some(Token::UpperIdent(_)));
                                if rhs_is_actor {
                                    return Err(ParseError::new(
                                        t0.span,
                                        format!("handle binding requires `@` suffix: write `{name}@ = ...` (§2.2)"),
                                    ));
                                }
                            }
                            let name_span = t0.span;
                            self.pos += 2; // consume Ident + Eq
                            let value = Box::new(self.parse_block_or_expr()?);
                            let span = Span::new(name_span.start, value.span().end);
                            items.push(BlockItem::Bind(name, value, span));
                            // Expect VirtualSemi (separator to next item)
                            self.expect(&Token::VirtualSemi)?;
                            continue;
                        }
                    }
                }

                // Method-body write statement (D056/D057): `path <- expr` where the LHS is a
                // bare atom or dotted chain. Emits RecordUpdate over Self_; the checker
                // rewrites it to an in-place region write. Sequenced via `Expr::Seq`
                // (a statement) — `=` stays an immutable bind everywhere.
                if self.method_body_depth > 0 && self.lookahead_write_path() {
                        let lhs_start = self.tokens[self.pos].span.start;
                        let first = match &self.bump()?.value {
                            Token::Ident(s) => s.clone(),
                            other => unreachable!("lookahead_write_path validated: {other:?}"),
                        };
                        let mut path = vec![first];
                        while self.eat(&Token::Dot) {
                            let t = self.bump()?;
                            match &t.value {
                                Token::Ident(s) => path.push(s.clone()),
                                other => {
                                    return Err(ParseError::new(
                                        t.span,
                                        format!("expected field name after '.', found {other:?}"),
                                    ))
                                }
                            }
                        }
                        self.expect(&Token::LeftArrow)?;
                        let value = Box::new(self.parse_block_or_expr()?);
                        let write_span = Span::new(lhs_start, value.span().end);
                        let write = Expr::RecordUpdate {
                            receiver: Box::new(Expr::Self_ { span: write_span }),
                            clauses: vec![UpdateClause::Leaf {
                                path,
                                value: *value,
                                span: write_span,
                            }],
                            span: write_span,
                        };
                        // A block may end with a write statement — it is then the final
                        // expression (Unit). Otherwise it is a sequenced statement.
                        if self.peek_kind() == Some(&Token::VirtualRBrace) {
                            return self.finish_block(items, write);
                        }
                        items.push(BlockItem::Stmt(Box::new(write), write_span));
                        // Expect VirtualSemi (separator to next item)
                        self.expect(&Token::VirtualSemi)?;
                        continue;
                }
                // Bare expression: a statement if followed by `;` (VirtualSemi),
                // otherwise the block's final expression. The layout-inserted
                // VirtualSemi/VirtualRBrace bound the expression, so `parse_expr`
                // has already stopped at the item boundary.
                let expr = self.parse_expr(0)?;
                if self.peek_kind() == Some(&Token::VirtualSemi) {
                    let span = expr.span();
                    items.push(BlockItem::Stmt(Box::new(expr), span));
                    self.expect(&Token::VirtualSemi)?;
                    continue;
                }
                // Final expression
                return self.finish_block(items, expr);
            }
        }
        self.parse_expr(0)
    }

    /// Close a layout block and fold its items into nested `Bind`/`Seq` around
    /// the final expression. The first item becomes the outermost node, so
    /// source order is preserved.
    fn finish_block(
        &mut self,
        items: Vec<BlockItem>,
        final_expr: Expr,
    ) -> Result<Expr, ParseError> {
        let rbrace = self.bump()?;
        if rbrace.value != Token::VirtualRBrace {
            return Err(ParseError::new(
                rbrace.span,
                format!("expected block end, found {:?}", rbrace.value),
            ));
        }
        // Desugar: fold items (in reverse) into nested Bind/Seq.
        let mut result = Box::new(final_expr);
        for item in items.into_iter().rev() {
            result = match item {
                BlockItem::Bind(name, value, span) => Box::new(Expr::Bind {
                    name,
                    value,
                    body: result,
                    span,
                }),
                BlockItem::Stmt(stmt, span) => Box::new(Expr::Seq {
                    first: stmt,
                    second: result,
                    span,
                }),
            };
        }
        Ok(*result)
    }

    pub(crate) fn parse_expr(&mut self, min_bp: u8) -> Result<Expr, ParseError> {
        self.parse_expr_ex(min_bp, false)
    }

    pub(crate) fn parse_expr_ex(
        &mut self,
        min_bp: u8,
        stop_before_rel_clause: bool,
    ) -> Result<Expr, ParseError> {
        let mut left = self.parse_prefix()?;

        loop {
            // Postfix field access (level 6).
            if min_bp <= 6 && matches!(self.peek_kind(), Some(Token::Dot)) {
                if (stop_before_rel_clause || min_bp > 0) && self.looks_like_rel_clause_start() {
                    break;
                }
                self.pos += 1;
                let field_tok = self.bump()?;
                let field = match &field_tok.value {
                    Token::Ident(s) => s.clone(),
                    other => {
                        return Err(ParseError::new(
                            field_tok.span,
                            format!("expected field name after `.`, found {other:?}"),
                        ));
                    }
                };
                let span = Span::new(left.span().start, field_tok.span.end);
                left = Expr::Field {
                    base: Box::new(left),
                    field,
                    span,
                };
                continue;
            }

            // Flat application (level 5): one or more argument atoms.
            // Heads must be atomic (§2.3.1): bare if/match/do/let are not apply heads.
            // Also stop before a following top-level declaration (`name:` / `name … =`).
            let line_bounded =
                self.line_bounded_expr && self.newline_before_current(left.span().end);
            if min_bp <= 5
                && self.starts_arg_atom()
                && expr_can_be_apply_head(&left)
                && !self.looks_like_item_start()
                && !line_bounded
            {
                let mut args = Vec::new();
                while self.starts_arg_atom()
                    && !self.looks_like_item_start()
                    && !(self.line_bounded_expr && self.newline_before_current(left.span().end))
                {
                    // Argument is atom + field chain only (bp 6), not infix/app
                    if std::env::var("WAG_DEBUG").is_ok() {
                        eprintln!("[parse] arg-atom at pos={} tok={:?}", self.pos, self.peek_kind());
                    }
                    args.push(self.parse_expr_ex(6, stop_before_rel_clause)?);
                }
                if std::env::var("WAG_DEBUG").is_ok() {
                    eprintln!("[parse] Apply: nargs={} pos_after={}", args.len(), self.pos);
                }
                let span = Span::new(left.span().start, args.last().unwrap().span().end);
                left = Expr::Apply {
                    fun: Box::new(left),
                    args,
                    span,
                };
                continue;
            }

            // Infix operators
            // Inside a comprehension section, `|` is a section separator, not bitOr.
            if self.in_comp_section && matches!(self.peek_kind(), Some(Token::Pipe)) {
                break;
            }
            let (l_bp, r_bp, op) = match self.peek_infix() {
                Some(v) => v,
                None => break,
            };
            if l_bp < min_bp {
                break;
            }
            // Non-associative comparisons use l_bp == r_bp; a second compare at the
            // same min_bp is rejected (`a > b > c`). Left-assoc arithmetic uses
            // r_bp = l_bp + 1 so it never hits l_bp == min_bp for the same op.
            if l_bp == min_bp && is_compare_token(self.peek_kind()) {
                return Err(ParseError::new(
                    self.span_here(),
                    "comparison operators are non-associative; use parentheses",
                ));
            }
            self.pos += 1; // consume op
            let right = self.parse_expr_ex(r_bp, stop_before_rel_clause)?;
            let span = Span::new(left.span().start, right.span().end);
            left = Expr::BinOp {
                op,
                lhs: Box::new(left),
                rhs: Box::new(right),
                span,
            };
        }

        Ok(left)
    }

    pub(crate) fn peek_infix(&self) -> Option<(u8, u8, BinOp)> {
        // left-assoc: r_bp = l_bp + 1; non-assoc compares: r_bp = l_bp
        match self.peek_kind()? {
            Token::Star => Some((4, 5, BinOp::Mul)),
            Token::Slash => Some((4, 5, BinOp::Div)),
            Token::Percent => Some((4, 5, BinOp::Rem)),
            Token::Plus => Some((3, 4, BinOp::Add)),
            Token::Minus => Some((3, 4, BinOp::Sub)),
            Token::Gt => Some((2, 2, BinOp::Gt)),
            Token::Lt => Some((2, 2, BinOp::Lt)),
            Token::Ge => Some((2, 2, BinOp::Ge)),
            Token::Le => Some((2, 2, BinOp::Le)),
            Token::EqEq => Some((2, 2, BinOp::Eq)),
            Token::Ne => Some((2, 2, BinOp::Ne)),
            // Bitwise ops: a band above arithmetic (§2.3.1). Left-associative,
            // ordered shifts > & > ^ > | (C/Rust convention). All bind tighter than +/-. 
            Token::LtLt => Some((9, 10, BinOp::ShiftLeft)),
            Token::GtGt => Some((9, 10, BinOp::ShiftRight)),
            Token::Amp => Some((8, 9, BinOp::BitAnd)),
            Token::Caret => Some((7, 8, BinOp::BitXor)),
            Token::Pipe => Some((6, 7, BinOp::BitOr)),
            _ => None,
        }
    }

    /// Tokens that may start an application argument atom (§2.3.1).
    pub(crate) fn starts_arg_atom(&self) -> bool {
        matches!(
            self.peek_kind(),
            Some(
                Token::Ident(_)
                    | Token::IdentAt(_)
                    | Token::UpperIdent(_)
                    | Token::IntLit(_)
                    | Token::StringLit(_)
                    | Token::LParen
                    | Token::LBracket
                    | Token::At
                    | Token::SelfKw
                    | Token::Copy
                    | Token::Bang
            )
        )
    }

    pub(crate) fn parse_prefix(&mut self) -> Result<Expr, ParseError> {
        match self.peek_kind() {
            Some(Token::Minus) => {
                // Unary minus → 0 - x (§6.2.2). Operand min_bp=4: includes `*`/`/`
                // (`-x * 2` ⇒ `0-(x*2)`) but not `+`/`-` (`-x - y` ⇒ `(0-x)-y`).
                let start = self.span_here().start;
                self.pos += 1;
                let right = self.parse_expr(4)?;
                let span = Span::new(start, right.span().end);
                Ok(Expr::BinOp {
                    op: BinOp::Sub,
                    lhs: Box::new(Expr::Int {
                        value: 0,
                        span: Span::new(start, start),
                    }),
                    rhs: Box::new(right),
                    span,
                })
            }
            Some(Token::Bang) => {
                // Prefix NOT: `!x` → BitNot (§2.3.1). Operand min_bp=10: binds tighter
                // than all binary ops (shifts are 9,10), so `!x & y` = `(!x) & y`.
                let start = self.span_here().start;
                self.pos += 1;
                let right = self.parse_expr(10)?;
                let span = Span::new(start, right.span().end);
                Ok(Expr::BinOp {
                    op: BinOp::BitNot,
                    lhs: Box::new(right),
                    rhs: Box::new(Expr::Int {
                        value: 0,
                        span: Span::new(start, start),
                    }),
                    span,
                })
            }
            Some(Token::If) => self.parse_if(),
            Some(Token::Match) => self.parse_match(),
            Some(Token::Do) => self.parse_do(),
            Some(Token::Return) => {
                // D081 v2: `return e` ⇒ `e` (State returned directly, no Option wrapper).
                self.pos += 1;
                self.parse_expr(0)
            }
            _ => self.parse_primary(),
        }
    }

    pub(crate) fn parse_primary(&mut self) -> Result<Expr, ParseError> {
        let tok = self.bump()?.clone();
        match tok.value {
            Token::IntLit(v) => Ok(Expr::Int {
                value: v,
                span: tok.span,
            }),
            Token::StringLit(s) => Ok(Expr::Str {
                value: s,
                span: tok.span,
            }),
            Token::Ident(name) => Ok(Expr::Var {
                name,
                span: tok.span,
            }),
            Token::UpperIdent(name) => Ok(Expr::Upper {
                name,
                span: tok.span,
            }),
            Token::At => {
                let name_tok = self.bump()?.clone();
                let name = match &name_tok.value {
                    Token::Ident(s) | Token::UpperIdent(s) => s.clone(),
                    _ => {
                        return Err(ParseError::new(
                            name_tok.span,
                            "expected name after `@` in expression",
                        ));
                    }
                };
                Ok(Expr::AtName {
                    name,
                    span: Span::new(tok.span.start, name_tok.span.end),
                })
            }
            Token::SelfKw => {
                // `self@ msg` — send to self (§5.5.4). `self` is a keyword, so the
                // lexer emits `SelfKw` + `At` (not `IdentAt`); handle the send here.
                if self.peek_kind() == Some(&Token::At) {
                    self.pos += 1; // consume `@`
                    let msg = self.parse_expr(0)?;
                    let span = Span::new(tok.span.start, msg.span().end);
                    Ok(Expr::Send {
                        target: Box::new(Expr::Self_ { span: tok.span }),
                        msg: Box::new(msg),
                        span,
                    })
                } else {
                    Ok(Expr::Self_ { span: tok.span })
                }
            }
            Token::Copy => Ok(Expr::Var {
                name: "copy".into(),
                span: tok.span,
            }),
            Token::IdentAt(name) => {
                // Send target: `ident@ msg` (§5.5.3). Parse the message as a
                // full expression; layout separates subsequent statements.
                let name = name.clone();
                let msg = self.parse_expr(0)?;
                let span = Span::new(tok.span.start, msg.span().end);
                Ok(Expr::Send {
                    target: Box::new(Expr::Var {
                        name,
                        span: tok.span,
                    }),
                    msg: Box::new(msg),
                    span,
                })
            }
            Token::LParen => {
                // `()` — Unit literal (§3).
                if self.peek_kind() == Some(&Token::RParen) {
                    self.pos += 1; // consume RParen
                    return Ok(Expr::Unit { span: tok.span });
                }
                // Try lambda: `(ident (, ident)* -> body)`
                let save = self.pos;
                if let Some(params) = self.try_parse_lambda_params() {
                    // Consume the Arrow (already consumed in try_parse_lambda_params).
                    let body = self.parse_expr(0)?;
                    let rparen_span = self.expect(&Token::RParen)?;
                    return Ok(Expr::Lambda {
                        params,
                        body: Box::new(body),
                        span: Span::new(tok.span.start, rparen_span.end),
                    });
                }
                // Not a lambda — restore and parse as regular parenthesized expr.
                self.pos = save;
                let first = self.parse_expr(0)?;
                if self.eat(&Token::Comma) {
                    // Product value: (a, b, ...)
                    let mut elems = vec![first];
                    loop {
                        elems.push(self.parse_expr(0)?);
                        if !self.eat(&Token::Comma) { break; }
                    }
                    let rparen_span = self.expect(&Token::RParen)?;
                    return Ok(Expr::Product {
                        elems,
                        span: Span::new(tok.span.start, rparen_span.end),
                    });
                }
                self.expect(&Token::RParen)?;
                Ok(first)
            }
            Token::LBracket => {
                // Try comprehension: `[ident (, ident)* <- ...]`
                // `[` already consumed by bump().
                if super::comprehension_parse::looks_like_comprehension(self) {
                    return super::comprehension_parse::parse_comprehension(self, tok.span.start);
                }
                self.parse_list_lit(tok.span.start)
            }
            other => Err(ParseError::new(
                tok.span,
                format!("expected expression, found {other:?}"),
            )),
        }
    }



    /// True when the tokens at `pos` form a method-body write LHS —
    /// `Ident (. Ident)* LeftArrow`. Does not consume.
        fn lookahead_write_path(&self) -> bool {
        let mut j = self.pos;
        if !matches!(
            self.tokens.get(j).map(|t| &t.value),
            Some(Token::Ident(_))
        ) {
            return false;
        }
        loop {
            match self.tokens.get(j + 1).map(|t| &t.value) {
                Some(Token::Dot) => {
                    j += 2;
                    if !matches!(
                        self.tokens.get(j).map(|t| &t.value),
                        Some(Token::Ident(_))
                    ) {
                        return false;
                    }
                }
                Some(Token::LeftArrow) => return true,
                _ => return false,
            }
        }
    }

    /// True when tokens look like a record-update clause start: `.a.b = …`
    /// (used to stop field-chains on an RHS from consuming the next clause).
    pub(crate) fn looks_like_rel_clause_start(&self) -> bool {
        let mut j = self.pos;
        let mut saw_field = false;
        while matches!(self.tokens.get(j).map(|t| &t.value), Some(Token::Dot)) {
            j += 1;
            match self.tokens.get(j).map(|t| &t.value) {
                Some(Token::Ident(_)) => {
                    saw_field = true;
                    j += 1;
                }
                _ => return false,
            }
        }
        saw_field && matches!(self.tokens.get(j).map(|t| &t.value), Some(Token::Eq))
    }

    /// Try to parse lambda params: `Ident (, Ident)* Arrow`.
    /// On success, consumes the tokens and returns the param names.
    /// On failure, does NOT consume (caller restores position).
    pub(crate) fn try_parse_lambda_params(&mut self) -> Option<Vec<String>> {
        // First token must be an Ident.
        let first = self.tokens.get(self.pos)?;
        let Token::Ident(name0) = &first.value else {
            return None;
        };
        let mut params = vec![name0.clone()];
        let mut pos = self.pos + 1;

        // Consume optional `, Ident` sequences.
        loop {
            if pos >= self.tokens.len() {
                return None;
            }
            match &self.tokens[pos].value {
                Token::Comma => {
                    pos += 1;
                    let t = self.tokens.get(pos)?;
                    let Token::Ident(n) = &t.value else {
                        return None;
                    };
                    params.push(n.clone());
                    pos += 1;
                }
                Token::Arrow => {
                    // Success! Consume up to and including the Arrow.
                    self.pos = pos + 1;
                    return Some(params);
                }
                _ => return None,
            }
        }
    }

    pub(crate) fn parse_list_lit(&mut self, start: usize) -> Result<Expr, ParseError> {
        let mut elems = Vec::new();
        if !matches!(self.peek_kind(), Some(Token::RBracket)) {
            loop {
                elems.push(self.parse_expr(0)?);
                if self.eat(&Token::Comma) {
                    continue;
                }
                break;
            }
        }
        let end = self.expect(&Token::RBracket)?;
        Ok(Expr::ListLit {
            elems,
            span: Span::new(start, end.end),
        })
    }

}
