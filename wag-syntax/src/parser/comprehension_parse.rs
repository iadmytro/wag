//! Comprehension parsing (§6.7.5) — the `[generators | acc_inits? | guards -> result]` form.
//!
//! Free functions over `&mut Parser` (see DECISIONS.md, parser module split):
//! the core driver stays in `parser.rs`; each grammar fragment is its own module.
//! Desugaring of parsed comprehensions lives in `comprehension_desugar.rs`.

use crate::ast::*;
use super::{Parser, ParseError};
use crate::Span;
use crate::Token;

/// Check if the tokens after `[` look like a comprehension generator: `ident (, ident)* <-`.
/// `p.pos` is already past `[` (consumed by bump).
pub(crate) fn looks_like_comprehension(p: &Parser) -> bool {
    let mut pos = p.pos;
    // First token must be Ident.
    let t = p.tokens.get(pos);
    let Some(t) = t else { return false };
    let Token::Ident(_) = &t.value else { return false };
    pos += 1;
    // Optional `, ident` sequences.
    loop {
        if pos >= p.tokens.len() { return false; }
        match &p.tokens[pos].value {
            Token::Comma => {
                pos += 1;
                let t2 = p.tokens.get(pos);
                let Some(t2) = t2 else { return false };
                let Token::Ident(_) = &t2.value else { return false };
                pos += 1;
            }
            Token::LeftArrow => return true,
            _ => return false,
        }
    }
}

/// Parse a full comprehension: `[generators | acc_inits? | guards -> result]`.
/// Assumes `[` has already been consumed.
pub(crate) fn parse_comprehension(p: &mut Parser, start: usize) -> Result<Expr, ParseError> {
    // --- Parse generators ---
    let mut generators = Vec::new();
    loop {
        // Parse: `ident (, ident)* <- expr (, expr)*`
        let mut vars = Vec::new();
        let first = p.bump()?;
        let Token::Ident(v0) = &first.value.clone() else {
            return Err(ParseError::new(first.span, "expected variable name in generator"));
        };
        vars.push(v0.clone());
        let mut pos = p.pos;
        // Collect comma-separated vars before `<-`.
        while pos < p.tokens.len() {
            match &p.tokens[pos].value {
                Token::Comma => {
                    pos += 1;
                    let t = p.tokens.get(pos).ok_or_else(|| {
                        ParseError::new(Span::empty(start), "expected variable after comma")
                    })?;
                    let Token::Ident(v) = &t.value else {
                        return Err(ParseError::new(t.span, "expected variable name"));
                    };
                    vars.push(v.clone());
                    pos += 1;
                }
                Token::LeftArrow => break,
                _ => {
                    return Err(ParseError::new(
                        p.tokens.get(pos).map(|t| t.span).unwrap_or_else(|| Span::empty(start)),
                        "expected `<-` in generator",
                    ));
                }
            }
        }
        // Consume up to and including `<-`.
        p.pos = pos + 1;
        // Parse source expressions (one per var, comma-separated).
        // Use bp=8 so that `|` (bp 6-7) is NOT consumed as bitOr.
        // Disambiguation: after a source, if `,` is followed by
        // `Ident <-` it is a NEW generator (cartesian), not another source
        // (zip). Otherwise the comma continues the source list (zip).
        let mut sources = Vec::new();
        loop {
            let src = p.parse_expr(8)?;
            sources.push(src);
            if p.eat(&Token::Comma) {
                // Look ahead: is the next token `Ident` followed by `<-`?
                if matches!(p.peek_kind(), Some(Token::Ident(_)))
                    && p.pos + 1 < p.tokens.len()
                    && matches!(p.tokens[p.pos + 1].value, Token::LeftArrow)
                {
                    p.pos -= 1; // un-eat the comma — new generator follows
                    break;
                }
                continue; // another source (zip)
            }
            break;
        }
        let gen_span = Span::new(start, sources.last().unwrap().span().end);
        generators.push(Generator { vars, sources, span: gen_span });
        // Check for more generators (comma before another ident).
        if p.eat(&Token::Comma) {
            // Next should be an Ident (another generator).
            if matches!(p.peek_kind(), Some(Token::Ident(_))) {
                continue;
            }
            // Otherwise the comma was part of the sources list — this is a
            // disambiguation issue. For now, treat as end of generators.
            p.pos -= 1; // un-eat the comma
            break;
        }
        break;
    }

    // --- Parse sections after `|` ---
    let mut accs: Vec<AccInit> = Vec::new();
    let mut guards: Vec<Expr> = Vec::new();

    if p.eat(&Token::Pipe) {
        // Parse section 2: could be acc inits or guards.
        let section2 = parse_pipe_section(p)?;
        // Determine if it's acc inits or guards.
        if p.eat(&Token::Pipe) {
            // Section 2 was acc inits (since there's a section 3).
            for item in section2 {
                accs.push(section_item_to_acc(item)?);
            }
            // Parse section 3 (guards).
            let section3 = parse_pipe_section(p)?;
            for item in section3 {
                guards.push(section_item_to_guard(item)?);
            }
        } else {
            // No section 3 — section 2 is guards.
            for item in section2 {
                guards.push(section_item_to_guard(item)?);
            }
        }
    }

    // --- Parse `->` and result ---
    let arrow_span = p.expect(&Token::Arrow)?;
    let result = p.parse_expr(0)?;
    let rbracket_span = p.expect(&Token::RBracket)?;

    let _ = arrow_span;

    // --- Validate supported forms (reject unsupported with clear errors) ---
    // This validation is the gate for the desugar pass, which rewrites the
    // AST silently and has no error channel: any form that reaches
    // `desugar_comprehensions` must be one it can handle.
    let span = Span::new(start, rbracket_span.end);
    for gen in &generators {
        if gen.vars.len() != gen.sources.len() {
            return Err(ParseError::new(
                gen.span,
                "comprehension generator must name one variable per source list",
            ));
        }
    }
    if accs.len() > 1 {
        return Err(ParseError::new(
            span,
            "multiple-accumulator comprehension `[x <- xs | a = 0, b = 0 | ...]` is not yet supported",
        ));
    }
    if !accs.is_empty() && generators.len() > 1 {
        return Err(ParseError::new(
            span,
            "fold over multiple generators `[x <- xs, y <- ys | a = 0 | ...]` is not yet supported",
        ));
    }
    // §6.7.5: an accumulator must be used — a declared-but-unreferenced
    // accumulator is a compile error. A real use of the outer accumulator
    // must appear as an `Expr::Var` node of that name in the result, so
    // this scan can never reject a program that uses it; shadowed
    // rebinding of the same name can only *miss* the error (safe side).
    if let Some(acc) = accs.first() {
        if !expr_refs(&result, &acc.name) {
            return Err(ParseError::new(
                acc.span,
                format!(
                    "comprehension accumulator `{}` is declared but never used in the result",
                    acc.name
                ),
            ));
        }
    }

    Ok(Expr::Comprehension {
        generators,
        accs,
        guards,
        result: Box::new(result),
        span,
    })
}

/// True if `name` appears as a variable reference anywhere in `expr`.
///
/// Deliberately naive (no shadowing analysis): used only for the
/// §6.7.5 unused-accumulator check, where a false negative (missing the
/// error because an inner binder re-uses the name) is the safe direction and
/// a false positive is impossible — any genuine use of the outer name must
/// surface as an `Expr::Var` node of that name. Pattern bindings (`Pat::Var`)
/// are not `Expr` nodes and correctly do not count as uses.
fn expr_refs(expr: &Expr, name: &str) -> bool {
match expr {
    Expr::Var { name: n, .. } => n == name,
    Expr::Apply { fun, args, .. } => {
        expr_refs(fun, name) || args.iter().any(|a| expr_refs(a, name))
    }
    Expr::BinOp { lhs, rhs, .. } => expr_refs(lhs, name) || expr_refs(rhs, name),
    Expr::Field { base, .. } => expr_refs(base, name),
    Expr::Borrow { inner, .. } => expr_refs(inner, name),
    Expr::Bind { value, body, .. } => expr_refs(value, name) || expr_refs(body, name),
    Expr::Seq { first, second, .. } => expr_refs(first, name) || expr_refs(second, name),
    Expr::If { arms, .. } => arms.iter().any(|a| {
        a.condition.as_ref().is_some_and(|c| expr_refs(c, name)) || expr_refs(&a.body, name)
    }),
    Expr::Match { scrutinee, arms, .. } => {
        expr_refs(scrutinee, name)
            || arms.iter().any(|a| expr_refs(&a.body, name))
    }
    Expr::Do { stmts, .. } => stmts.iter().any(|s| match s {
        DoStmt::Bind { expr, .. } => expr_refs(expr, name),
        DoStmt::Return(e) => expr_refs(e, name),
    }),
    Expr::ListLit { elems, .. } => elems.iter().any(|e| expr_refs(e, name)),
    Expr::Lambda { body, .. } => expr_refs(body, name),
    Expr::RecordUpdate { receiver, clauses, .. } => {
        expr_refs(receiver, name)
            || clauses.iter().any(|c| match c {
                UpdateClause::Leaf { value, .. } => expr_refs(value, name),
                UpdateClause::Group { clauses, .. } => clauses
                    .iter()
                    .any(|inner| matches!(inner, UpdateClause::Leaf { value, .. } if expr_refs(value, name))),
            })
    }
    Expr::SetField { receiver, value, .. } => {
        expr_refs(receiver, name) || expr_refs(value, name)
    }
    Expr::RegionWrite { receiver, value, .. } => {
        expr_refs(receiver, name) || expr_refs(value, name)
    }
    Expr::Comprehension { generators, accs, guards, result, .. } => {
        generators.iter().any(|g| g.sources.iter().any(|s| expr_refs(s, name)))
            || accs.iter().any(|a| expr_refs(&a.init, name))
            || guards.iter().any(|g| expr_refs(g, name))
            || expr_refs(result, name)
    }
    Expr::Product { elems, .. } => elems.iter().any(|e| expr_refs(e, name)),
    Expr::Send { target, msg, .. } => expr_refs(target, name) || expr_refs(msg, name),
    Expr::Int { .. }
    | Expr::Str { .. }
    | Expr::Upper { .. }
    | Expr::AtName { .. }
    | Expr::Self_ { .. }
    | Expr::Unit { .. } => false,
}
}

/// Parse a pipe-delimited section (items separated by commas).
/// Returns a Vec of raw items (expr or acc-init pattern).
pub(crate) fn parse_pipe_section(p: &mut Parser) -> Result<Vec<Expr>, ParseError> {
    p.in_comp_section = true;
    let result = parse_pipe_section_inner(p);
    p.in_comp_section = false;
    result
}

pub(crate) fn parse_pipe_section_inner(p: &mut Parser) -> Result<Vec<Expr>, ParseError> {
    let mut items = Vec::new();
    // Wildcard `_` is a guard.
    if matches!(p.peek_kind(), Some(Token::Wildcard)) {
        p.pos += 1;
        items.push(Expr::Var { name: "_".into(), span: Span::empty(0) });
        return Ok(items);
    }
    loop {
        // Try acc init: `ident = expr`
        let save = p.pos;
        if let Some(first) = p.tokens.get(p.pos) {
            if let Token::Ident(_) = &first.value {
                // Check if next is `=` (acc init) or something else (guard).
                if p.tokens.get(p.pos + 1).map(|t| &t.value) == Some(&Token::Eq) {
                    // Acc init: `name = expr`
                    p.pos += 2; // consume ident + =
                    let init = p.parse_expr(0)?;
                    // Encode as a special marker: we'll decode later.
                    let name = if let Token::Ident(n) = &first.value { n.clone() } else { "?".into() };
                    items.push(Expr::Bind {
                        name,
                        value: Box::new(init),
                        body: Box::new(Expr::Int { value: 0, span: Span::empty(0) }),
                        span: Span::empty(0),
                    });
                    if p.eat(&Token::Comma) { continue; }
                    break;
                }
            }
        }
        p.pos = save;
        // Otherwise: parse as guard expression.
        let guard = p.parse_expr(0)?;
        items.push(guard);
        if p.eat(&Token::Comma) { continue; }
        break;
    }
    Ok(items)
}

pub(crate) fn section_item_to_acc(item: Expr) -> Result<AccInit, ParseError> {
    match item {
        Expr::Bind { name, value, .. } => Ok(AccInit {
            name,
            init: *value,
            span: Span::empty(0),
        }),
        _ => Err(ParseError::new(
            item.span(),
            "expected accumulator init `name = expr`",
        )),
    }
}

pub(crate) fn section_item_to_guard(item: Expr) -> Result<Expr, ParseError> {
    // If it's a Let marker, it was an acc init misplaced in guards.
    if matches!(item, Expr::Bind { .. }) {
        return Err(ParseError::new(
            item.span(),
            "accumulator init found in guard section",
        ));
    }
    Ok(item)
}
