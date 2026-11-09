//! Layout preprocessing: insert virtual `{` `;` `}` per §2.1.
//!
//! Fixed-reference-column rule for `do` / `if` / `match` / `with`.
//! When the first layout item begins on the same line as the opener, the
//! reference column is taken from the first token of the next line that still
//! belongs to this layout context (deferred column) — matching the style used
//! in the §8 examples.

use crate::lexer::LineIndex;
use crate::token::{spanned, SpannedToken, Token};
use crate::Span;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutError {
    pub span: Span,
    pub message: String,
}

#[derive(Debug)]
enum Ctx {
    /// Explicit `{` … `}` — layout does not insert tokens inside.
    Explicit { depth: usize },
    /// Implicit layout block.
    Implicit {
        /// `None` until the reference column is known.
        col: Option<usize>,
        /// Tokens already emitted inside this context (not counting virtual open).
        emitted: usize,
        /// Line of the layout keyword / open point — same-line first items defer `col`.
        open_line: usize,
        /// Opened by `with` (not `match`/`do`/`if`). Closed before `)` so
        /// parenthesized `with` expressions terminate cleanly.
        from_with: bool,
        /// Opened by `import`: group-headers (module names) open item sub-blocks.
        from_import: bool,
        /// Whether the current layout item has seen `=` (leaf vs group header).
        item_seen_eq: bool,
    },
}

/// Apply layout to a raw lexed token stream.
pub fn layout(source: &str, tokens: &[SpannedToken]) -> Result<Vec<SpannedToken>, LayoutError> {
    let mut out: Vec<SpannedToken> = Vec::new();
    let mut stack: Vec<Ctx> = Vec::new();
    let mut i = 0usize;
    // Built once (O(n)); per-token line/col lookups are O(log n) instead of the
    // O(n) re-scan the free `line_col_at` did (which made this pass O(n²)).
    let line_index = LineIndex::new(source);

    while i < tokens.len() {
        let tok = &tokens[i];

        // Close / separate implicit contexts based on column of this token
        // (only when this token starts a "layout-significant" position — we
        // always re-check; deeper columns continue the current item).
        let (line, col) = line_index.line_col_at(tok.span.start);
        let _ = line;

        loop {
            let action = match stack.last() {
                Some(Ctx::Implicit {
                    col: Some(ref_col),
                    emitted,
                    ..
                }) if col == *ref_col && *emitted > 0 => Some(Action::Semi),
                Some(Ctx::Implicit {
                    col: Some(ref_col),
                    ..
                }) if col < *ref_col => Some(Action::Close),
                Some(Ctx::Implicit {
                    col: None,
                    emitted,
                    open_line,
                    ..
                }) if *emitted > 0 && line > *open_line => {
                    // Deferred after a same-line first item: this new-line token
                    // sets the reference column and starts a new layout item.
                    Some(Action::SetColAndSemi { col })
                }
                Some(Ctx::Implicit {
                    col: None,
                    emitted: 0,
                    open_line,
                    ..
                }) => {
                    if line > *open_line {
                        // First token already on a fresh line — classic offside.
                        Some(Action::SetCol { col })
                    } else {
                        // Same line as opener: emit as first item, keep col deferred.
                        None
                    }
                }
                _ => None,
            };

            match action {
                Some(Action::Semi) => {
                    out.push(virtual_at(Token::VirtualSemi, tok.span.start));
                    if let Some(Ctx::Implicit { item_seen_eq, .. }) = stack.last_mut() {
                        *item_seen_eq = false;
                    }
                    break;
                }
                Some(Action::Close) => {
                    out.push(virtual_at(Token::VirtualRBrace, tok.span.start));
                    stack.pop();
                    continue;
                }
                Some(Action::SetCol { col: c }) => {
                    if let Some(Ctx::Implicit { col, .. }) = stack.last_mut() {
                        *col = Some(c);
                    }
                    break;
                }
                Some(Action::SetColAndSemi { col: c }) => {
                    if let Some(Ctx::Implicit { col, item_seen_eq, .. }) = stack.last_mut() {
                        *col = Some(c);
                        *item_seen_eq = false;
                    }
                    out.push(virtual_at(Token::VirtualSemi, tok.span.start));
                    break;
                }
                None => break,
            }
        }

        // Nested `with` clause block: group header is a RelPath-only item
        // (no `=` yet); a deeper-indented `.` starts its ClauseBlock.
        if matches!(tok.value, Token::Dot) {
            if let Some(Ctx::Implicit {
                col: Some(ref_col),
                from_with: true,
                item_seen_eq: false,
                emitted,
                ..
            }) = stack.last()
            {
                if col > *ref_col && *emitted > 0 {
                    out.push(virtual_at(Token::VirtualLBrace, tok.span.start));
                    stack.push(Ctx::Implicit {
                        col: Some(col),
                        emitted: 0,
                        open_line: line,
                        from_with: true,
                        from_import: false,
                        item_seen_eq: false,
                    });
                }
            }
        }

        // Nested import group: a group header is the first token on a new line
        // at a deeper column. Same-line tokens are continuations (alias source,
        // actor name after `@`) and never open sub-blocks.
        let is_new_line = out
            .last()
            .map(|last| line_index.line_col_at(last.span.start).0 != line)
            .unwrap_or(true);
        if is_new_line {
            if let Some(Ctx::Implicit {
                col: Some(ref_col),
                from_import: true,
                item_seen_eq: false,
                emitted,
                ..
            }) = stack.last()
            {
                if col > *ref_col && *emitted > 0 {
                    out.push(virtual_at(Token::VirtualLBrace, tok.span.start));
                    stack.push(Ctx::Implicit {
                        col: Some(col),
                        emitted: 0,
                        open_line: line,
                        from_with: false,
                        from_import: true,
                        item_seen_eq: false,
                    });
                }
            }
        }

        // Close `with` layout before `)` / `]` so `(with p …)` places
        // VirtualRBrace before the closing paren. Do not close `match`/`do`/`if`
        // layouts — those routinely contain parenthesized subexpressions.
        if matches!(tok.value, Token::RParen | Token::RBracket) {
            while matches!(
                stack.last(),
                Some(Ctx::Implicit {
                    from_with: true,
                    ..
                })
            ) {
                out.push(virtual_at(Token::VirtualRBrace, tok.span.start));
                stack.pop();
            }
        }

        // Explicit brace depth tracking
        if tok.value == Token::LBrace {
            if let Some(Ctx::Explicit { depth }) = stack.last_mut() {
                *depth += 1;
            } else {
                // Could be opening explicit layout after keyword — handled below
            }
        }
        if tok.value == Token::RBrace {
            if let Some(Ctx::Explicit { depth }) = stack.last_mut() {
                if *depth > 1 {
                    *depth -= 1;
                } else {
                    stack.pop();
                }
                out.push(tok.clone());
                i += 1;
                continue;
            }
        }

        // Emit the token
        if matches!(tok.value, Token::LeftArrow) {
            if let Some(Ctx::Implicit { item_seen_eq, .. }) = stack.last_mut() {
                *item_seen_eq = true;
            }
        }
        out.push(tok.clone());
        bump_emitted(&mut stack);
        i += 1;

        // After emitting a layout opener, open a new context.
        if tok.value.opens_layout_after_self() {
            open_layout(&line_index, tokens, &mut i, &mut out, &mut stack, false, false)?;
        } else if tok.value == Token::Eq && out.len() >= 2
            && (matches!(out[out.len() - 2].value, Token::Ident(_))
                || matches!(out[out.len() - 2].value, Token::Colon))
            && i < tokens.len()
        {
            // `Ident Eq` inside an existing block — block binding. Only open
            // a nested layout context when the value is on a NEW line
            // (deeper indent = nested block). Same-line values are plain
            // expressions (no context).
            let eq_line = line_index.line_col_at(tok.span.start).0;
            let next_line = line_index.line_col_at(tokens[i].span.start).0;
            if next_line > eq_line {
                open_layout(&line_index, tokens, &mut i, &mut out, &mut stack, false, false)?;
            }
        } else if tok.value == Token::Eq && out.len() >= 3
            && matches!(out[out.len() - 2].value, Token::UpperIdent(_))
            && matches!(out[out.len() - 3].value, Token::UpperIdent(_))
            && i < tokens.len()
        {
            // Class/instance head (D075): `Name C₁ … =` — two consecutive
            // UpperIdents before `=`. Open a layout context for the indented
            // body (method sigs or function defs). Datatypes have only one
            // UpperIdent before `=`, so they are unaffected.
            let eq_line = line_index.line_col_at(tok.span.start).0;
            let next_line = line_index.line_col_at(tokens[i].span.start).0;
            if next_line > eq_line {
                open_layout(&line_index, tokens, &mut i, &mut out, &mut stack, false, false)?;
            }
        } else if tok.value == Token::Eq && out.len() >= 2
            && matches!(out[out.len() - 2].value, Token::RParen)
            && stack.is_empty()
            && i < tokens.len()
        {
            // Instance head with parenthesized type (D075): `Name (TypeExpr) =`.
            let eq_line = line_index.line_col_at(tok.span.start).0;
            let next_line = line_index.line_col_at(tokens[i].span.start).0;
            if next_line > eq_line {
                open_layout(&line_index, tokens, &mut i, &mut out, &mut stack, false, false)?;
            }
        } else if tok.value == Token::Arrow && i < tokens.len() {
            // Match arm body: `->` followed by a new line = block body.
            let arrow_line = line_index.line_col_at(tok.span.start).0;
            let next_line = line_index.line_col_at(tokens[i].span.start).0;
            if next_line > arrow_line {
                open_layout(&line_index, tokens, &mut i, &mut out, &mut stack, false, false)?;
            }
        } else if tok.value == Token::Match {
            // Scrutinee may be a `.`-chain (`match p.age`).
            skip_atom(source, tokens, &mut i, &mut out, &mut stack, true)?;
            open_layout(&line_index, tokens, &mut i, &mut out, &mut stack, false, false)?;

        } else if tok.value.is_import_opener() {
            // Import block: group-headers (module names) open item sub-blocks.
            open_layout(&line_index, tokens, &mut i, &mut out, &mut stack, false, true)?;
        }
    }

    // EOF: close remaining implicit contexts
    while let Some(ctx) = stack.pop() {
        match ctx {
            Ctx::Implicit { .. } => {
                let at = source.len();
                out.push(virtual_at(Token::VirtualRBrace, at));
            }
            Ctx::Explicit { .. } => {
                return Err(LayoutError {
                    span: Span::empty(source.len()),
                    message: "unclosed explicit '{' in layout".into(),
                });
            }
        }
    }

    Ok(out)
}

enum Action {
    Semi,
    Close,
    SetCol { col: usize },
    SetColAndSemi { col: usize },
}

fn virtual_at(tok: Token, at: usize) -> SpannedToken {
    spanned(tok, Span::empty(at))
}

fn last_real_span(out: &[SpannedToken]) -> Option<Span> {
    out.iter()
        .rev()
        .find(|t| !t.value.is_virtual())
        .map(|t| t.span)
}

fn bump_emitted(stack: &mut [Ctx]) {
    if let Some(Ctx::Implicit { emitted, .. }) = stack.last_mut() {
        *emitted = emitted.saturating_add(1);
    }
}

fn open_layout(
    line_index: &LineIndex,
    tokens: &[SpannedToken],
    i: &mut usize,
    out: &mut Vec<SpannedToken>,
    stack: &mut Vec<Ctx>,
    from_with: bool,
    from_import: bool,
) -> Result<(), LayoutError> {
    // Skip is done by caller for match; here next token is first of block.
    if *i >= tokens.len() {
        // empty layout at EOF
        let at = line_index.source_len();
        out.push(virtual_at(Token::VirtualLBrace, at));
        out.push(virtual_at(Token::VirtualRBrace, at));
        return Ok(());
    }

    let next = &tokens[*i];
    if next.value == Token::LBrace {
        // Explicit braces — consume LBrace into explicit context
        out.push(next.clone());
        *i += 1;
        stack.push(Ctx::Explicit { depth: 1 });
        return Ok(());
    }

    let at = next.span.start;
    let (open_line, _) = line_index.line_col_at(at);
    // Prefer the line of the layout keyword just emitted when available.
    let open_line = last_real_span(out)
        .map(|s| line_index.line_col_at(s.start).0)
        .unwrap_or(open_line);
    out.push(virtual_at(Token::VirtualLBrace, at));
    stack.push(Ctx::Implicit {
        col: None,
        emitted: 0,
        open_line,
        from_with,
        from_import,
        item_seen_eq: false,
    });
    Ok(())
}

/// Emit tokens that form a single atom (match scrutinee / with receiver).
///
/// When `allow_field_chain` is true (match), trailing `.field` segments are
/// included in the atom. For `with`, they must be left for the clause block.
// `out` needs `Vec::push`, so a slice would not do despite the lint.
#[allow(clippy::ptr_arg)]
fn skip_atom(
    source: &str,
    tokens: &[SpannedToken],
    i: &mut usize,
    out: &mut Vec<SpannedToken>,
    stack: &mut Vec<Ctx>,
    allow_field_chain: bool,
) -> Result<(), LayoutError> {
    if *i >= tokens.len() {
        return Err(LayoutError {
            span: Span::empty(source.len()),
            message: "expected atom after match/with".into(),
        });
    }

    // Optional &
    if tokens[*i].value == Token::Amp {
        out.push(tokens[*i].clone());
        bump_emitted(stack);
        *i += 1;
    }

    if *i >= tokens.len() {
        return Err(LayoutError {
            span: Span::empty(source.len()),
            message: "expected atom after '&'".into(),
        });
    }

    match &tokens[*i].value {
        Token::Ident(_)
        | Token::UpperIdent(_)
        | Token::IntLit(_)
        | Token::Wildcard
        | Token::SelfKw => {
            out.push(tokens[*i].clone());
            bump_emitted(stack);
            *i += 1;
        }
        Token::At => {
            out.push(tokens[*i].clone());
            bump_emitted(stack);
            *i += 1;
            if *i < tokens.len()
                && matches!(
                    tokens[*i].value,
                    Token::Ident(_) | Token::UpperIdent(_)
                )
            {
                out.push(tokens[*i].clone());
                bump_emitted(stack);
                *i += 1;
            }
        }
        Token::LParen => {
            out.push(tokens[*i].clone());
            bump_emitted(stack);
            *i += 1;
            let mut depth = 1i32;
            while *i < tokens.len() && depth > 0 {
                match tokens[*i].value {
                    Token::LParen => depth += 1,
                    Token::RParen => depth -= 1,
                    _ => {}
                }
                out.push(tokens[*i].clone());
                bump_emitted(stack);
                *i += 1;
            }
            if depth != 0 {
                return Err(LayoutError {
                    span: Span::empty(source.len()),
                    message: "unclosed '(' in match/with atom".into(),
                });
            }
        }
        other => {
            return Err(LayoutError {
                span: tokens[*i].span,
                message: format!("expected atom after match/with, found {other:?}"),
            });
        }
    }

    // Postfix field access chain: . Ident (only for match scrutinees)
    if allow_field_chain {
        while *i < tokens.len() && tokens[*i].value == Token::Dot {
            out.push(tokens[*i].clone());
            bump_emitted(stack);
            *i += 1;
            if *i < tokens.len()
                && matches!(
                    tokens[*i].value,
                    Token::Ident(_) | Token::UpperIdent(_)
                )
            {
                out.push(tokens[*i].clone());
                bump_emitted(stack);
                *i += 1;
            } else {
                return Err(LayoutError {
                    span: Span::empty(source.len()),
                    message: "expected field name after '.'".into(),
                });
            }
        }
    }

    Ok(())
}

/// Lex then apply layout.
pub fn lex_with_layout(source: &str) -> Result<Vec<SpannedToken>, String> {
    let raw = crate::lexer::lex(source).map_err(|e| format!("{} @ {:?}", e.message, e.span))?;
    layout(source, &raw).map_err(|e| format!("{} @ {:?}", e.message, e.span))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::Token;

    fn kinds(src: &str) -> Vec<Token> {
        lex_with_layout(src)
            .unwrap_or_else(|e| panic!("lex_with_layout failed: {e}\nsource:\n{src}"))
            .into_iter()
            .map(|t| t.value)
            .collect()
    }

    #[test]
    fn do_layout_indent() {
        let src = "do\n  x <- f a\n  y <- g x\n  return y\n";
        let k = kinds(src);
        assert_eq!(
            k,
            vec![
                Token::Do,
                Token::VirtualLBrace,
                Token::Ident("x".into()),
                Token::LeftArrow,
                Token::Ident("f".into()),
                Token::Ident("a".into()),
                Token::VirtualSemi,
                Token::Ident("y".into()),
                Token::LeftArrow,
                Token::Ident("g".into()),
                Token::Ident("x".into()),
                Token::VirtualSemi,
                Token::Return,
                Token::Ident("y".into()),
                Token::VirtualRBrace,
            ]
        );
    }

    #[test]
    fn do_explicit_braces() {
        let src = "do { v1 <- add x y; return v1 }";
        let k = kinds(src);
        assert_eq!(
            k,
            vec![
                Token::Do,
                Token::LBrace,
                Token::Ident("v1".into()),
                Token::LeftArrow,
                Token::Ident("add".into()),
                Token::Ident("x".into()),
                Token::Ident("y".into()),
                Token::Semi,
                Token::Return,
                Token::Ident("v1".into()),
                Token::RBrace,
            ]
        );
    }

    #[test]
    fn if_layout_newline_arms() {
        let src = "if\n  a == b -> Some a\n  _ -> None\n";
        let k = kinds(src);
        assert_eq!(
            &k[..3],
            &[Token::If, Token::VirtualLBrace, Token::Ident("a".into())]
        );
        assert!(k.contains(&Token::VirtualSemi));
        assert_eq!(k.last(), Some(&Token::VirtualRBrace));
    }

    #[test]
    fn if_same_line_first_arm_then_dedent_arm() {
        // §8.1 style
        let src = "factHelper n acc = if n == 0 -> Some acc\n  _ -> do\n    return acc\n";
        let k = kinds(src);
        // Expect: ... If { n == 0 -> Some acc ; _ -> do { return acc } }
        let if_pos = k.iter().position(|t| *t == Token::If).unwrap();
        assert_eq!(k[if_pos + 1], Token::VirtualLBrace);
        assert!(k[if_pos..].contains(&Token::VirtualSemi));
        assert!(k.iter().filter(|t| **t == Token::VirtualRBrace).count() >= 2);
    }

    #[test]
    fn match_layout_after_scrutinee() {
        let src = "match lst\n  Empty -> None\n  Cons h t -> Some h\n";
        let k = kinds(src);
        assert_eq!(
            k,
            vec![
                Token::Match,
                Token::Ident("lst".into()),
                Token::VirtualLBrace,
                Token::UpperIdent("Empty".into()),
                Token::Arrow,
                Token::UpperIdent("None".into()),
                Token::VirtualSemi,
                Token::UpperIdent("Cons".into()),
                Token::Ident("h".into()),
                Token::Ident("t".into()),
                Token::Arrow,
                Token::UpperIdent("Some".into()),
                Token::Ident("h".into()),
                Token::VirtualRBrace,
            ]
        );
    }

    #[test]
    fn match_borrow_scrutinee() {
        let src = "match &xs\n  Empty -> 0\n";
        let k = kinds(src);
        assert_eq!(
            &k[..4],
            &[
                Token::Match,
                Token::Amp,
                Token::Ident("xs".into()),
                Token::VirtualLBrace,
            ]
        );
    }

    #[test]
    fn spec_2_1_worked_example_token_stream() {
        let src = r#"-- This is a line comment; it runs to the end of the line.
{- This is a block comment. Block comments nest:
   {- like this -}
   and can span multiple lines. -}
triple: Int -> Option Int   -- an optional type annotation, checked against inference (§3.2)
triple x = x * 3              -- the function body; no `do` needed since this has no <- bindings
"#;
        let k = kinds(src);
        assert_eq!(
            k,
            vec![
                Token::Ident("triple".into()),
                Token::Colon,
                Token::UpperIdent("Int".into()),
                Token::Arrow,
                Token::UpperIdent("Option".into()),
                Token::UpperIdent("Int".into()),
                Token::Ident("triple".into()),
                Token::Ident("x".into()),
                Token::Eq,
                Token::Ident("x".into()),
                Token::Star,
                Token::IntLit(3),
            ]
        );
    }

    // match → if → match (the real lexer shape: lexH is a match whose arm
    // body is an if-chain, whose arm body contains a further match).
    const MATCH_IF_MATCH: &str = "f = Int -> Int: x -> match x\n\
                                 \x20 1 -> if\n\
                                 \x20\x20 x == 1 ->\n\
                                 \x20\x20\x20 match x\n\
                                 \x20\x20\x20\x20 1 -> 10\n\
                                 \x20\x20\x20\x20 _ -> 20\n\
                                 \x20\x20 _ -> 0\n\
                                 \x20 _ -> 0\n";

    // if → match → if (mirror shape: if whose arm body is a match, whose arm
    // body is a further if-chain). `_ -> 30` sits at the match-arm column (a
    // wildcard arm of the inner match); `_ -> 0` dedents to the if-arm column
    // (a new if arm). Indentation alone places each line unambiguously.
    const IF_MATCH_IF: &str = "f = Int -> Int: x -> if\n\
                              \x20 x == 1 ->\n\
                              \x20\x20 match x\n\
                              \x20\x20\x20 1 -> if\n\
                              \x20\x20\x20\x20 x == 2 -> 10\n\
                              \x20\x20\x20\x20 _ -> 20\n\
                              \x20\x20\x20 _ -> 30\n\
                              \x20 _ -> 0\n";

    #[test]
    fn match_if_match_layout_dump() {
        let k = kinds(MATCH_IF_MATCH);
        eprintln!("=== match→if→match layout token stream ===");
        for t in &k {
            eprintln!("  {:?}", t);
        }
    }

    #[test]
    fn match_if_match_parses() {
        crate::parse_desugared(MATCH_IF_MATCH)
            .expect("match→if→match should parse");
    }

    #[test]
    fn if_match_if_layout_dump() {
        let k = kinds(IF_MATCH_IF);
        eprintln!("=== if→match→if layout token stream ===");
        for t in &k {
            eprintln!("  {:?}", t);
        }
    }

    #[test]
    fn if_match_if_parses() {
        crate::parse_desugared(IF_MATCH_IF)
            .expect("if→match→if should parse");
    }
}
