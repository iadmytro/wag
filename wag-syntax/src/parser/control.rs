//! Control-flow and pattern parsing: `if`, `match`, patterns, `do` blocks
//! (grammar fragment of `parser.rs`).
//!
//! `impl Parser` methods: `parse_if`, `parse_match`, `parse_pattern`,
//! `parse_do`, `parse_do_stmt`, `open_block`, `close_block_if`, `looks_like_item_start`.

use super::{ParseError, Parser};
use crate::ast::*;
use crate::token::Token;
use crate::Span;

use super::pat_span;

impl<'a> Parser<'a> {
    pub(crate) fn expect_token_kind(
        &mut self,
        pred: impl FnOnce(&Token) -> bool,
        label: &str,
    ) -> Result<Span, ParseError> {
        let t = self.bump()?;
        if pred(&t.value) {
            Ok(t.span)
        } else {
            Err(ParseError::new(
                t.span,
                format!("expected `{label}`, found {:?}", t.value),
            ))
        }
    }

    pub(crate) fn parse_if(&mut self) -> Result<Expr, ParseError> {
        let start = self.span_here().start;
        self.expect_token_kind(|t| matches!(t, Token::If), "if")?;
        self.open_block()?;
        let mut arms = Vec::new();
        loop {
            if self.close_block_if() {
                break;
            }
            // Skip separators
            if matches!(
                self.peek_kind(),
                Some(Token::VirtualSemi | Token::Semi)
            ) {
                self.pos += 1;
                continue;
            }
            arms.push(self.parse_if_arm()?);
            // optional separator
            let _ = self.eat(&Token::VirtualSemi) || self.eat(&Token::Semi);
        }
        if arms.is_empty() {
            return Err(ParseError::new(
                Span::new(start, start),
                "`if` needs at least one arm",
            ));
        }
        let end = arms.last().unwrap().body.span().end;
        Ok(Expr::If {
            arms,
            span: Span::new(start, end),
        })
    }

    pub(crate) fn parse_if_arm(&mut self) -> Result<IfArm, ParseError> {
        // `_ -> body` or `cond -> body`
        if matches!(self.peek_kind(), Some(Token::Wildcard)) {
            self.pos += 1;
            self.expect_arrow()?;
            let body = self.parse_block_or_expr()?;
            return Ok(IfArm {
                condition: None,
                body,
            });
        }
        let cond = self.parse_expr(0)?;
        self.expect_arrow()?;
        let body = self.parse_block_or_expr()?;
        Ok(IfArm {
            condition: Some(cond),
            body,
        })
    }

    pub(crate) fn expect_arrow(&mut self) -> Result<(), ParseError> {
        match self.peek_kind() {
            Some(Token::Arrow) => {
                self.pos += 1;
                Ok(())
            }
            _ => Err(ParseError::new(self.span_here(), "expected `->` in arm")),
        }
    }

    pub(crate) fn parse_match(&mut self) -> Result<Expr, ParseError> {
        let start = self.span_here().start;
        self.expect_token_kind(|t| matches!(t, Token::Match), "match")?;
        // Scrutinee is a single atom (+ field chain), not a bare application
        let scrutinee = self.parse_expr(6)?;
        self.open_block()?;
        let mut arms = Vec::new();
        loop {
            if self.close_block_if() {
                break;
            }
            if matches!(
                self.peek_kind(),
                Some(Token::VirtualSemi | Token::Semi)
            ) {
                self.pos += 1;
                continue;
            }
            arms.push(self.parse_match_arm()?);
            let _ = self.eat(&Token::VirtualSemi) || self.eat(&Token::Semi);
        }
        if arms.is_empty() {
            return Err(ParseError::new(
                Span::new(start, scrutinee.span().end),
                "match requires at least one arm",
            ));
        }
        let end = arms
            .last()
            .map(|a| a.body.span().end)
            .unwrap_or(scrutinee.span().end);
        Ok(Expr::Match {
            scrutinee: Box::new(scrutinee),
            arms,
            span: Span::new(start, end),
        })
    }

    pub(crate) fn parse_match_arm(&mut self) -> Result<MatchArm, ParseError> {
        let pattern = self.parse_pattern()?;
        if self.peek_kind() == Some(&Token::When) {
            return Err(ParseError::new(
                self.span_here(),
                "match guards (`when`) are not supported",
            ));
        }
        self.expect_arrow()?;
        let body = self.parse_block_or_expr()?;
        Ok(MatchArm { pattern, body })
    }

    pub(crate) fn parse_pattern(&mut self) -> Result<Pat, ParseError> {
        match self.peek_kind() {
            Some(Token::Wildcard) => {
                let span = self.span_here();
                self.pos += 1;
                Ok(Pat::Wildcard { span })
            }
            Some(Token::Ident(name)) => {
                let name = name.clone();
                let span = self.span_here();
                self.pos += 1;
                Ok(Pat::Var { name, span })
            }
            Some(Token::UpperIdent(name)) => {
                let name = name.clone();
                let start = self.span_here().start;
                self.pos += 1;
                let mut args = Vec::new();
                while self.starts_pat_atom() {
                    args.push(self.parse_pat_atom()?);
                }
                let end = args
                    .last()
                    .map(|p| pat_span(p).end)
                    .unwrap_or(start);
                Ok(Pat::Ctor {
                    name,
                    qualifier: None,
                    args,
                    span: Span::new(start, end),
                })
            }
            Some(Token::At) => {
                // Qualified pattern: @Actor.Ctor args...
                self.pos += 1;
                let actor_tok = self.bump()?.clone();
                let actor = match &actor_tok.value {
                    Token::Ident(s) | Token::UpperIdent(s) => s.clone(),
                    _ => {
                        return Err(ParseError::new(
                            actor_tok.span,
                            "expected actor name after `@` in pattern",
                        ));
                    }
                };
                self.expect(&Token::Dot)?;
                let ctor_tok = self.bump()?.clone();
                let name = match &ctor_tok.value {
                    Token::UpperIdent(s) => s.clone(),
                    _ => {
                        return Err(ParseError::new(
                            ctor_tok.span,
                            "expected constructor name after `@Actor.` in pattern",
                        ));
                    }
                };
                let start = Span::new(actor_tok.span.start, ctor_tok.span.end).start;
                let mut args = Vec::new();
                while self.starts_pat_atom() {
                    args.push(self.parse_pat_atom()?);
                }
                let end = args
                    .last()
                    .map(|p| pat_span(p).end)
                    .unwrap_or(ctor_tok.span.end);
                Ok(Pat::Ctor {
                    name,
                    qualifier: Some(actor),
                    args,
                    span: Span::new(start, end),
                })
            }
            Some(Token::IntLit(v)) => {
                let span = self.span_here();
                let v = *v; // copy out before mutating self.pos
                self.pos += 1;
                Ok(Pat::Literal {
                    lit: LitPat::Int(v),
                    span,
                })
            }
            Some(Token::StringLit(s)) => {
                let span = self.span_here();
                let s = s.clone(); // owned before mutating self.pos
                self.pos += 1;
                Ok(Pat::Literal {
                    lit: LitPat::Str(s),
                    span,
                })
            }
            Some(Token::LParen) => self.parse_paren_pat(),
            _ => Err(ParseError::new(
                self.span_here(),
                format!("expected pattern, found {:?}", self.peek_kind()),
            )),
        }
    }

    pub(crate) fn starts_pat_atom(&self) -> bool {
        matches!(
            self.peek_kind(),
            Some(
                Token::Wildcard
                    | Token::Ident(_)
                    | Token::IdentAt(_)
                    | Token::UpperIdent(_)
                    | Token::IntLit(_)
                    | Token::StringLit(_)
                    | Token::LParen
            )
        )
    }

    /// Parse a parenthesized pattern: `(pat)` pure grouping or `(p1, p2 [, …])`
    /// native product destructuring (§3.6 / syntax unification). A trailing
    /// comma yields a `Pat::Product`; without one the parens were grouping and
    /// the inner pattern is returned. Element arity is 2–7 (implementation
    /// limit, §VM); more commas are rejected as an implementation limit.
    fn parse_paren_pat(&mut self) -> Result<Pat, ParseError> {
        let save = self.pos;
        self.pos += 1; // consume `(`
        let first = self.parse_pattern()?;
        let mut elems = vec![first];
        while self.eat(&Token::Comma) {
            elems.push(self.parse_pattern()?);
        }
        self.expect(&Token::RParen)?;
        if elems.len() > 7 {
            return Err(ParseError::new(
                Span::new(save, self.span_here().start),
                "tuple supports at most 7 elements (implementation limit)",
            ));
        }
        if elems.len() == 1 {
            Ok(elems.into_iter().next().unwrap())
        } else {
            Ok(Pat::Product {
                elems,
                span: Span::new(save, self.span_here().start),
            })
        }
    }

    pub(crate) fn parse_pat_atom(&mut self) -> Result<Pat, ParseError> {
        match self.peek_kind() {
            Some(Token::Wildcard) => {
                let span = self.span_here();
                self.pos += 1;
                Ok(Pat::Wildcard { span })
            }
            Some(Token::Ident(name)) => {
                let name = name.clone();
                let span = self.span_here();
                self.pos += 1;
                Ok(Pat::Var { name, span })
            }
            Some(Token::IdentAt(name)) => {
                // Handle-name pattern: `replyTo@` binds an actor-typed variable (§2.2).
                let name = name.clone();
                let span = self.span_here();
                self.pos += 1;
                Ok(Pat::Var { name, span })
            }
            Some(Token::UpperIdent(name)) => {
                // Nested ctor must be parenthesized for args; bare Upper is nullary
                let name = name.clone();
                let span = self.span_here();
                self.pos += 1;
                Ok(Pat::Ctor {
                    name,
                    qualifier: None,
                    args: vec![],
                    span,
                })
            }
            Some(Token::IntLit(v)) => {
                let span = self.span_here();
                let v = *v; // copy out before mutating self.pos
                self.pos += 1;
                Ok(Pat::Literal { lit: LitPat::Int(v), span })
            }
            Some(Token::StringLit(s)) => {
                let span = self.span_here();
                let s = s.clone(); // owned before mutating self.pos
                self.pos += 1;
                Ok(Pat::Literal { lit: LitPat::Str(s), span })
            }
            Some(Token::LParen) => self.parse_paren_pat(),
            _ => Err(ParseError::new(
                self.span_here(),
                "expected pattern atom",
            )),
        }
    }

    pub(crate) fn parse_do(&mut self) -> Result<Expr, ParseError> {
        let start = self.span_here().start;
        self.expect_token_kind(|t| matches!(t, Token::Do), "do")?;
        self.open_block()?;
        let mut stmts = Vec::new();
        loop {
            if self.close_block_if() {
                break;
            }
            if matches!(
                self.peek_kind(),
                Some(Token::VirtualSemi | Token::Semi)
            ) {
                self.pos += 1;
                continue;
            }
            stmts.push(self.parse_do_stmt()?);
            let _ = self.eat(&Token::VirtualSemi) || self.eat(&Token::Semi);
        }
        if stmts.is_empty() {
            return Err(ParseError::new(
                Span::new(start, start),
                "empty `do` block",
            ));
        }
        if !matches!(stmts.last(), Some(DoStmt::Return(_))) {
            return Err(ParseError::new(
                Span::new(start, start),
                "`do` block must end with `return` (§6.5)",
            ));
        }
        let end = match stmts.last() {
            Some(DoStmt::Bind { expr, .. }) => expr.span().end,
            Some(DoStmt::Return(e)) => e.span().end,
            None => start,
        };
        Ok(Expr::Do {
            stmts,
            span: Span::new(start, end),
        })
    }

    pub(crate) fn parse_do_stmt(&mut self) -> Result<DoStmt, ParseError> {
        // `x <- e` (§6.5 monadic bind over `Option`) or `return e` (§6.5).
        // Bare expressions (e.g. message sends) are NOT `do` statements —
        // sequence them in a block instead. (§2.2: handles carry @)
        if let Some(name) = match self.peek_kind() {
            Some(Token::Ident(n)) => Some((n.clone(), false)),
            Some(Token::IdentAt(n)) => Some((n.clone(), true)),
            _ => None,
        } {
            let (name, is_handle) = name;
            let next = self.tokens.get(self.pos + 1).map(|t| &t.value);
            if matches!(next, Some(Token::LeftArrow)) {
                // §2.2: actor-creation RHS requires @ on the bound name.
                if !is_handle {
                    let rhs_is_actor = matches!(self.tokens.get(self.pos + 2).map(|t| &t.value), Some(Token::At))
                        && matches!(self.tokens.get(self.pos + 3).map(|t| &t.value),
                            Some(Token::Ident(_)) | Some(Token::UpperIdent(_)));
                    if rhs_is_actor {
                        return Err(ParseError::new(
                            self.span_here(),
                            format!("handle binding requires `@` suffix: write `{name}@ <- ...` (§2.2)"),
                        ));
                    }
                }
                self.pos += 2;
                let expr = self.parse_expr(0)?;
                return Ok(DoStmt::Bind { name, expr });
            }
        }
        if matches!(self.peek_kind(), Some(Token::Return)) {
            self.pos += 1;
            // §6.5: `return e` ⇒ `Some e` (the block's final value).
            return Ok(DoStmt::Return(self.parse_expr(0)?));
        }
        // Bare expressions are not `do` statements (§6.5): `do` chains `Option`
        // results via `<-`/`return`. Sequence statements (sends, etc.) in a block.
        Err(ParseError::new(
            self.span_here(),
            "bare expression in `do` block — `do` chains `Option` results (`<-` / `return`); sequence statements such as message sends in a block instead",
        ))
    }

    pub(crate) fn open_block(&mut self) -> Result<(), ParseError> {
        match self.peek_kind() {
            Some(Token::VirtualLBrace | Token::LBrace) => {
                self.pos += 1;
                Ok(())
            }
            _ => Err(ParseError::new(
                self.span_here(),
                "expected block `{` (layout or explicit)",
            )),
        }
    }

    pub(crate) fn close_block_if(&mut self) -> bool {
        match self.peek_kind() {
            Some(Token::VirtualRBrace | Token::RBrace) => {
                self.pos += 1;
                true
            }
            None => true,
            _ => false,
        }
    }

    /// True when the upcoming tokens start another top-level item, so an
    /// expression (e.g. `x + 1`) must not treat the next name as an argument.
    ///
    /// Gate: the preceding token must be a "terminator" (not an atom), meaning
    /// the current token is at a statement-start position. This prevents false
    /// positives when application arguments (e.g. `build idInt xs`) are
    /// followed by a new top-level item on the next line.
    pub(crate) fn looks_like_item_start(&self) -> bool {
        // Line-boundary gate: if the previous token is on the same line, this
        // token is a continuation (argument), not a new item start.
        if self.pos > 0 && !self.source.is_empty() {
            let prev_end = self.tokens[self.pos - 1].span.end;
            let cur_start = self.tokens[self.pos].span.start;
            if prev_end < cur_start
                && cur_start <= self.source.len()
                && !self.source[prev_end..cur_start].contains('\n')
            {
                return false;
            }
        }
        match self.peek_kind() {
            // `name = ...` — new unified item start
            Some(Token::Ident(_)) => {
                matches!(self.tokens.get(self.pos + 1).map(|t| &t.value), Some(Token::Eq))
            }
            // `Name = ...` (datatype / type alias) or bare `Name` (nullary ctor / type ref).
            // Line-boundary check above confirms this is on a new line.
            Some(Token::UpperIdent(_)) => true,
            // `(op) … = …` — operator-overload def start (V1). A parenthesized
            // operator can never be an expression atom, so at statement-start
            // position it always begins a new definition and must terminate the
            // preceding application (e.g. `map f h = listMap f h` / `(+) …`).
            Some(Token::LParen) => self
                .tokens
                .get(self.pos + 2)
                .is_some_and(|t| t.value == Token::RParen)
                && self
                    .tokens
                    .get(self.pos + 1)
                    .is_some_and(|t| Self::is_op_symbol(&t.value)),
            // `@name = ...` — actor item start
            Some(Token::At) => {
                match self.tokens.get(self.pos + 1).map(|t| &t.value) {
                    Some(Token::Ident(_)) | Some(Token::UpperIdent(_)) => {
                        matches!(
                            self.tokens.get(self.pos + 2).map(|t| &t.value),
                            Some(Token::Eq | Token::LBracket)
                        )
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }
}
