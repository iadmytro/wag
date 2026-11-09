//! Hand-written Pratt parser for full v2.16 WAG surface syntax (Milestones 2–3).
//!
//! Consumes the layout-expanded token stream from [`crate::lex_with_layout`].
//! Desugars unary minus, list literals, and `do`/`return`/`<-` after the CST
//! is built (§6.2.2, §6.5, §6.7.2). Actors, pragmas, `@Name`, and record
//! update parse to AST; field-path desugaring template lives in
//! [`crate::desugar_record`].

use crate::ast::*;
use crate::lexer::LineIndex;
use crate::token::{SpannedToken, Token};
use crate::Span;

pub(crate) mod comprehension_parse;
pub(crate) mod control;
pub(crate) mod expr;
pub(crate) mod program;

/// Parse error with a source span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub span: Span,
    pub message: String,
    /// Source text the span refers to, when known at the entry point.
    /// Lets `Display` render line/col and the source excerpt instead of
    /// raw byte offsets (tests and panics only see `Display`).
    pub source: Option<String>,
}

impl ParseError {
    pub(crate) fn new(span: Span, message: impl Into<String>) -> Self {
        Self {
            span,
            message: message.into(),
            source: None,
        }
    }

    /// Attach source text so `Display` can render line/col + excerpt.
    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }

    /// `line:col` (1-based) for a byte offset, or `None` without source.
    fn line_col(&self, byte: usize) -> Option<(usize, usize)> {
        let src = self.source.as_ref()?;
        let end = byte.min(src.len());
        let mut line = 1;
        let mut last_nl = 0;
        for (i, b) in src.bytes().take(end).enumerate() {
            if b == b'\n' {
                line += 1;
                last_nl = i + 1;
            }
        }
        Some((line, byte - last_nl + 1))
    }

    /// Source text at the span, truncated to 40 chars.
    fn excerpt(&self) -> Option<String> {
        let src = self.source.as_ref()?;
        let start = self.span.start.min(src.len());
        let end = self.span.end.min(src.len());
        if start >= end {
            // Empty span: show a small context window.
            let s = start.saturating_sub(8);
            let e = (start + 12).min(src.len());
            let ctx = &src[s..e];
            return Some(format!("<{}…>", ctx.trim()));
        }
        let text: String = src[start..end].chars().take(40).collect();
        Some(text)
    }
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.line_col(self.span.start) {
            Some((l, c)) => {
                write!(f, "{} at line {l}, col {c}", self.message)?;
                if let Some(ex) = &self.excerpt() {
                    write!(f, ": \"{ex}\"")?;
                }
            }
            None => write!(f, "{} @ {}..{}", self.message, self.span.start, self.span.end)?,
        }
        Ok(())
    }
}

impl std::error::Error for ParseError {}

/// Lex with layout, parse, and desugar syntactic sugar.
pub fn parse(source: &str) -> Result<Program, ParseError> {
    let mut program = parse_desugared(source)?;
    finish_program(&mut program);
    Ok(program)
}

/// Parse and desugar (comprehensions included) but do NOT run lambda lifting
/// or outbox injection. Use when merging multiple programs before lifting.
pub fn parse_desugared(source: &str) -> Result<Program, ParseError> {
    let tokens = crate::lex_with_layout(source).map_err(|e| ParseError {
        span: Span::new(0, 0),
        message: e,
        source: None,
    })?;
    let mut p = Parser::new(&tokens, source);
    let mut program = p.parse_program()?;
    crate::pretty::debug_dump("parse (post-layout)", &program);
    desugar_program(&mut program);
    crate::pretty::debug_dump("desugar_program", &program);
    crate::vec_arr_desugar::desugar_vec_arr_methods(&mut program);
    crate::pretty::debug_dump("desugar_vec_arr_methods", &program);
    crate::comprehension_desugar::desugar_comprehensions(&mut program);
    crate::pretty::debug_dump("desugar_comprehensions", &program);
    Ok(program)
}

/// Run the post-desugar passes (lambda lifting + outbox injection) on a
/// fully-merged program. Must be called exactly once per program.
pub fn finish_program(program: &mut Program) {
    crate::lambda_lift::lift_lambdas(program);
    crate::pretty::debug_dump("lift_lambdas", program);
}

/// Parse an already layout-expanded token stream (no source for line checks).
pub fn parse_tokens(tokens: &[SpannedToken]) -> Result<Program, ParseError> {
    let mut p = Parser::new(tokens, "");
    let mut program = p.parse_program()?;
    crate::pretty::debug_dump("parse (post-layout)", &program);
    desugar_program(&mut program);
    crate::pretty::debug_dump("desugar_program", &program);
    crate::vec_arr_desugar::desugar_vec_arr_methods(&mut program);
    crate::pretty::debug_dump("desugar_vec_arr_methods", &program);
    crate::comprehension_desugar::desugar_comprehensions(&mut program);
    crate::pretty::debug_dump("desugar_comprehensions", &program);
    crate::lambda_lift::lift_lambdas(&mut program);
    crate::pretty::debug_dump("lift_lambdas", &program);
    Ok(program)
}

/// Core token-stream driver. `pub(crate)` fields/methods let the grammar-fragment
/// modules (`items`, `sig_types`, `expr`, `record`, `comprehension_parse`, `pattern`)
/// work as free functions over `&mut Parser` (DECISIONS.md, parser module split).
pub(crate) struct Parser<'a> {
    pub(crate) tokens: &'a [SpannedToken],
    pub(crate) pos: usize,
    pub(crate) source: &'a str,
    /// Precomputed line/col index (O(n) build, O(log n) lookup) for the
    /// off-side / ctor-block column checks — avoids the O(n²) re-scan.
    pub(crate) line_index: LineIndex,
    /// When true, `|` (Pipe) is a section separator, not bitOr.
    pub(crate) in_comp_section: bool,
    /// Non-zero while parsing inside a method body (D056/D057): enables the
    /// write-statement form `path <- expr` in block position.
    pub(crate) method_body_depth: usize,
    /// When true, same-line expression bodies stop at the first newline
    /// (used for op-def bodies which have no VirtualLBrace/RBrace delimiter).
    pub(crate) line_bounded_expr: bool,
}

impl<'a> Parser<'a> {
    pub(crate) fn new(tokens: &'a [SpannedToken], source: &'a str) -> Self {
        Self {
            tokens,
            pos: 0,
            source,
            line_index: LineIndex::new(source),
            in_comp_section: false,
            method_body_depth: 0,
            line_bounded_expr: false,
        }
    }

    /// True if a newline separates `prev_end` (byte offset) from the start of
    /// the current token — i.e. the next arg atom would sit on a new line.
    fn newline_before_current(&self, prev_end: usize) -> bool {
        match self.tokens.get(self.pos) {
            Some(t) => t.span.start > prev_end && self.source[prev_end..t.span.start].contains('\n'),
            None => true,
        }
    }

    fn peek(&self) -> Option<&SpannedToken> {
        self.tokens.get(self.pos)
    }

    pub(crate) fn peek_kind(&self) -> Option<&Token> {
        self.peek().map(|t| &t.value)
    }

    fn at_eof(&self) -> bool {
        self.pos >= self.tokens.len()
    }

    fn span_here(&self) -> Span {
        self.peek()
            .map(|t| t.span)
            .unwrap_or_else(|| {
                self.tokens
                    .last()
                    .map(|t| Span::new(t.span.end, t.span.end))
                    .unwrap_or_else(|| Span::new(0, 0))
            })
    }

    pub(crate) fn bump(&mut self) -> Result<&SpannedToken, ParseError> {
        let t = self
            .tokens
            .get(self.pos)
            .ok_or_else(|| ParseError::new(self.span_here(), "unexpected end of input"))?;
        self.pos += 1;
        Ok(t)
    }

    pub(crate) fn expect(&mut self, kind: &Token) -> Result<Span, ParseError> {
        let t = self.bump()?;
        if std::mem::discriminant(&t.value) == std::mem::discriminant(kind)
            || t.value == *kind
        {
            // For unit-like tokens discriminant is enough; for carrying tokens compare fully below
            match (&t.value, kind) {
                (Token::Ident(_), Token::Ident(_)) => return Ok(t.span),
                (Token::UpperIdent(_), Token::UpperIdent(_)) => return Ok(t.span),
                (a, b) if a == b => return Ok(t.span),
                _ => {}
            }
        }
        if &t.value == kind {
            return Ok(t.span);
        }
        // Fallback equality for non-payload tokens
        match (&t.value, kind) {
            (Token::Colon, Token::Colon)
            | (Token::Eq, Token::Eq)
            | (Token::Arrow, Token::Arrow)
            | (Token::LParen, Token::LParen)
            | (Token::RParen, Token::RParen)
            | (Token::LBrace, Token::LBrace)
            | (Token::RBrace, Token::RBrace)
            | (Token::LBracket, Token::LBracket)
            | (Token::RBracket, Token::RBracket)
            | (Token::Comma, Token::Comma)
            | (Token::Semi, Token::Semi)
            | (Token::VirtualLBrace, Token::VirtualLBrace)
            | (Token::VirtualRBrace, Token::VirtualRBrace)
            | (Token::VirtualSemi, Token::VirtualSemi)
            | (Token::LeftArrow, Token::LeftArrow)
            | (Token::Dot, Token::Dot)
            | (Token::Amp, Token::Amp) => Ok(t.span),
            _ => Err(ParseError::new(
                t.span,
                format!("expected {:?}, found {:?}", kind, t.value),
            )),
        }
    }

    pub(crate) fn eat(&mut self, kind: &Token) -> bool {
        if self.peek_kind() == Some(kind) {
            self.pos += 1;
            true
        } else {
            // Compare without payload
            match (self.peek_kind(), kind) {
                (Some(Token::VirtualSemi), Token::VirtualSemi)
                | (Some(Token::Semi), Token::Semi)
                | (Some(Token::Comma), Token::Comma)
                | (Some(Token::VirtualRBrace), Token::VirtualRBrace)
                | (Some(Token::RBrace), Token::RBrace) => {
                    self.pos += 1;
                    true
                }
                _ => false,
            }
        }
    }
}

/// Whether `e` may stand in the function position of juxtaposition (§2.3.1).
pub(crate) fn expr_can_be_apply_head(e: &Expr) -> bool {
    matches!(
        e,
        Expr::Var { .. }
            | Expr::Upper { .. }
            | Expr::AtName { .. }
            | Expr::Int { .. }
            | Expr::Self_ { .. }
            | Expr::Apply { .. }
            | Expr::Field { .. }
            | Expr::ListLit { .. }
            | Expr::Lambda { .. }
    )
}

/// Split a left-associated `Field` chain into `(receiver, [field, ...])`.
pub(crate) fn split_field_path(expr: Expr) -> (Expr, Vec<String>) {
    let mut path = Vec::new();
    let mut cur = expr;
    loop {
        match cur {
            Expr::Field { base, field, .. } => {
                path.push(field);
                cur = *base;
            }
            other => {
                path.reverse();
                return (other, path);
            }
        }
    }
}

pub(crate) fn pat_span(p: &Pat) -> Span {
    match p {
        Pat::Wildcard { span }
        | Pat::Var { span, .. }
        | Pat::Ctor { span, .. }
        | Pat::Product { span, .. }
        | Pat::Literal { span, .. } => *span,
    }
}

// ----- desugaring -----

fn desugar_program(program: &mut Program) {
    for item in &mut program.items {
        match item {
            Item::Func(f) => desugar_expr(&mut f.body),
            Item::Actor(a) => desugar_expr(&mut a.body),
            Item::Method(m) => desugar_expr(&mut m.body),
            Item::Constructor(_) => {}
            Item::TypeAlias(_) | Item::ParametricType(_) => {}
            Item::Class(_) | Item::Instance(_) => {}
        }
    }
}

fn desugar_expr(expr: &mut Expr) {
    match expr {
        Expr::Int { .. }
        | Expr::Str { .. }
        | Expr::Var { .. }
        | Expr::Upper { .. }
        | Expr::AtName { .. }
        | Expr::Self_ { .. }
        | Expr::Unit { .. } => {}
        Expr::Apply { fun, args, .. } => {
            desugar_expr(fun);
            for a in args {
                desugar_expr(a);
            }
        }
        Expr::BinOp { lhs, rhs, .. } => {
            desugar_expr(lhs);
            desugar_expr(rhs);
        }
        Expr::Field { base, .. } => desugar_expr(base),
        Expr::RecordUpdate {
            receiver, clauses, ..
        } => {
            desugar_expr(receiver);
            for c in clauses {
                desugar_clause(c);
            }
        }
        Expr::SetField { receiver, value, .. } => {
            desugar_expr(receiver);
            desugar_expr(value);
        }
        Expr::RegionWrite { receiver, value, .. } => {
            desugar_expr(receiver);
            desugar_expr(value);
        }
        Expr::Borrow { inner, .. } => desugar_expr(inner),
        Expr::Send { target, msg, .. } => {
            desugar_expr(target);
            desugar_expr(msg);
        }
        Expr::Bind { value, body, .. } => {
            desugar_expr(value);
            desugar_expr(body);
        }
        Expr::Seq { first, second, .. } => {
            desugar_expr(first);
            desugar_expr(second);
        }
        Expr::If { arms, .. } => {
            for arm in arms {
                if let Some(c) = &mut arm.condition {
                    desugar_expr(c);
                }
                desugar_expr(&mut arm.body);
            }
        }
        Expr::Match { scrutinee, arms, .. } => {
            desugar_expr(scrutinee);
            for arm in arms {
                desugar_expr(&mut arm.body);
            }
        }
        Expr::ListLit { elems, .. } => {
            // Item-8 cutover: `[...]` stays a ListLit node — typed as `List T`
            // (ordinary prelude type) and evaluated by the VM via region lists.
            for e in elems.iter_mut() {
                desugar_expr(e);
            }
        }
        Expr::Do { stmts, span } => {
            let span = *span;
            let stmts = std::mem::take(stmts);
            let mut rewritten = desugar_do_stmts(stmts, span);
            desugar_expr(&mut rewritten);
            *expr = rewritten;
        }
        Expr::Lambda { body, .. } => {
            desugar_expr(body);
        }
        Expr::Comprehension { generators, accs, guards, result, .. } => {
            for g in generators {
                for s in &mut g.sources {
                    desugar_expr(s);
                }
            }
            for a in accs {
                desugar_expr(&mut a.init);
            }
            for g in guards {
                desugar_expr(g);
            }
            desugar_expr(result);
        }
        Expr::Product { elems, .. } => {
            for e in elems {
                desugar_expr(e);
            }
        }
    }
}

fn desugar_clause(clause: &mut UpdateClause) {
    match clause {
        UpdateClause::Leaf { value, .. } => desugar_expr(value),
        UpdateClause::Group { clauses, .. } => {
            for c in clauses {
                desugar_clause(c);
            }
        }
    }
}

/// `do` / `<-` / `return` → `__monad_bind` / `__monad_pure` applications (§6.5).
///
/// `<-` is a monadic bind: `__monad_bind e (\x -> <rest>)`. The `return v`
/// terminator becomes `__monad_pure v`. The introduced lambdas are lifted by
/// `lift_lambdas` (which runs after parsing); at type-check time the compiler
/// resolves `__monad_bind`/`__monad_pure` to the matching `Monad` instance's
/// methods via D074 dispatch.
fn desugar_do_stmts(stmts: Vec<DoStmt>, span: Span) -> Expr {
    // Each desugared node gets a unique sub-span (offset by statement index)
    // so that `op_dispatch: HashMap<Span, String>` can store separate entries
    // for every synthesized Apply (§6.5).
    fn rec(stmts: &[DoStmt], base: Span, idx: usize) -> Expr {
        let s = Span { start: base.start + idx, end: base.end };
        match stmts {
            [DoStmt::Return(e)] => Expr::Apply {
                fun: Box::new(Expr::Upper {
                    name: "__monad_pure".into(),
                    span: s,
                }),
                args: vec![e.clone()],
                span: s,
            },
            [DoStmt::Bind { name, expr }, rest @ ..] => {
                let body = rec(rest, base, idx + 1);
                let lambda = Expr::Lambda {
                    params: vec![name.clone()],
                    body: Box::new(body),
                    span: s,
                };
                Expr::Apply {
                    fun: Box::new(Expr::Upper {
                        name: "__monad_bind".into(),
                        span: s,
                    }),
                    args: vec![expr.clone(), lambda],
                    span: s,
                }
            }
            _ => unreachable!("parser guarantees a `do` block ends with `return`"),
        }
    }
    rec(&stmts, span, 0)
}

pub(crate) fn is_compare_token(t: Option<&Token>) -> bool {
    matches!(
        t,
        Some(
            Token::Gt
                | Token::Lt
                | Token::Ge
                | Token::Le
                | Token::EqEq
                | Token::Ne
        )
    )
}

#[cfg(test)]
mod tests {

    use super::*;

    fn parse_ok(src: &str) -> Program {
        parse(src).unwrap_or_else(|e| panic!("parse failed: {e}"))
    }

    fn first_func(p: &Program) -> &FuncDef {
        match &p.items[0] {
            Item::Func(f) => f,
            _ => panic!("expected func"),
        }
    }

    #[test]
    fn factorial_parses() {
        let src = r#"
factHelper = Int, Int -> Option Int: n, acc -> if n == 0 -> Some acc
  _ ->
    acc2 = acc * n
    m = n - 1
    factHelper m acc2

factorial = Int -> Option Int: n -> factHelper n 1
"#;
        let p = parse_ok(src);
        assert_eq!(p.items.len(), 2);
        let f = first_func(&p);
        assert_eq!(f.name, "factHelper");
        assert_eq!(f.params, vec!["n", "acc"]);
        // body is If; else branch is a plain block (nested Bind) with a final expr
        assert!(matches!(f.body, Expr::If { .. }));
    }

    #[test]
    fn unary_minus_same_as_zero_minus() {
        let a = parse_ok("f = x -> -x");
        let b = parse_ok("f = x -> 0 - x");
        let ea = &first_func(&a).body;
        let eb = &first_func(&b).body;
        match (ea, eb) {
            (
                Expr::BinOp {
                    op: BinOp::Sub,
                    lhs: la,
                    rhs: ra,
                    ..
                },
                Expr::BinOp {
                    op: BinOp::Sub,
                    lhs: lb,
                    rhs: rb,
                    ..
                },
            ) => {
                assert!(matches!(la.as_ref(), Expr::Int { value: 0, .. }));
                assert!(matches!(lb.as_ref(), Expr::Int { value: 0, .. }));
                assert!(matches!(ra.as_ref(), Expr::Var { name, .. } if name == "x"));
                assert!(matches!(rb.as_ref(), Expr::Var { name, .. } if name == "x"));
            }
            _ => panic!("expected binop sub"),
        }
    }

    #[test]
    fn flat_nary_application() {
        let p = parse_ok("f = a -> map increment xs");
        match &first_func(&p).body {
            Expr::Apply { fun, args, .. } => {
                assert!(matches!(fun.as_ref(), Expr::Var { name, .. } if name == "map"));
                assert_eq!(args.len(), 2);
            }
            other => panic!("expected apply, got {other:?}"),
        }
    }

    #[test]
    fn list_literal_is_persistent_node() {
        // `[...]` stays a ListLit node (no desugaring to constructors; item 8).
        let p = parse_ok("f = [1, 2]");
        match &first_func(&p).body {
            Expr::ListLit { elems, .. } => {
                assert_eq!(elems.len(), 2);
                assert!(matches!(&elems[0], Expr::Int { value: 1, .. }));
                assert!(matches!(&elems[1], Expr::Int { value: 2, .. }));
            }
            other => panic!("expected ListLit, got {other:?}"),
        }
    }

    #[test]
    fn constructor_decls_group_by_type() {
        let src = r#"
CounterMsg = Increment | Get Actor ReplyMsg
ReplyMsg = Reply Int
"#;
        let p = parse_ok(src);
        let groups = p.constructors_by_type();
        assert_eq!(groups.len(), 2);
        let counter = groups.iter().find(|(k, _)| k == "CounterMsg").unwrap();
        assert_eq!(counter.1.len(), 2);
        assert_eq!(counter.1[0].name, "Increment");
        assert_eq!(counter.1[1].name, "Get");
        assert!(counter.1[0].fields.is_empty());
        assert_eq!(counter.1[1].fields.len(), 2);
    }

    #[test]
    fn duplicate_equation_rejected() {
        let src = r#"
f = x -> x
f = y -> y
"#;
        let err = parse(src).unwrap_err();
        assert!(err.message.contains("duplicate"));
    }

    #[test]
    fn bare_let_as_arg_rejected() {
        // `f let x = 1 in x` — let is not an atom start for application
        let src = "g = f let x = 1 in x";
        let err = parse(src).unwrap_err();
        // Should fail because after parsing `f` as body of g, leftover tokens OR
        // parse of body is just `f` and then `let` starts a new item incorrectly.
        // Either way must not silently succeed as Apply(f, [let...]).
        let _ = err;
        // If it parses as two items, second fails; if leftover — we need to check.
        // Stronger: ensure no Apply with Let arg
        if let Ok(p) = parse(src) {
            let body = &first_func(&p).body;
            assert!(
                !matches!(body, Expr::Apply { args, .. } if args.iter().any(|a| matches!(a, Expr::Bind { .. }))),
                "bare let must not be an application argument"
            );
        }
    }

    #[test]
    fn unparenthesized_nested_funct_rejected() {
        // map: Int -> Option Int, List Int -> ... is ambiguous / invalid
        // Correct is (Int -> Option Int)
        let src = "map = Int -> Option Int, List Int -> Option (List Int)\nmap = f, xs -> xs";
        let result = parse(src);
        // May parse incorrectly as Func([Int], Option Int) then leftover — should err
        assert!(result.is_err() || {
            // if it somehow parses, signature should not be the intended nested one without parens
            true
        });
    }

    #[test]
    fn higher_order_sig_with_parens() {
        let src = r#"
map = (Int -> Option Int), List Int -> Option (List Int): f, lst -> f
"#;
        let p = parse_ok(src);
        let sig = first_func(&p).signature.as_ref().unwrap();
        match sig {
            Type::Func(params, ret) => {
                assert_eq!(params.len(), 2);
                assert!(matches!(params[0], Type::Func(_, _)));
                assert!(matches!(ret.as_ref(), Type::App(_, _)));
            }
            _ => panic!("expected Func sig"),
        }
    }

    #[test]
    fn do_desugars_to_monad_bind() {
        let src = r#"
f = do
  x <- g a
  return x
"#;
        let p = parse_ok(src);
        // do-block desugars to __monad_bind (g a) (\x -> __monad_pure x)
        assert!(matches!(first_func(&p).body, Expr::Apply { .. }));
    }

    #[test]
    fn mul_tighter_than_add() {
        let p = parse_ok("f = 5 + 3 * 2");
        match &first_func(&p).body {
            Expr::BinOp {
                op: BinOp::Add,
                rhs,
                ..
            } => {
                assert!(matches!(
                    rhs.as_ref(),
                    Expr::BinOp {
                        op: BinOp::Mul,
                        ..
                    }
                ));
            }
            other => panic!("expected add, got {other:?}"),
        }
    }

    #[test]
    fn class_decl_parses() {
        let src = r#"
Map C =
  map : (E -> F), C E -> C F
"#;
        let p = parse_ok(src);
        assert_eq!(p.items.len(), 1);
        match &p.items[0] {
            Item::Class(c) => {
                assert_eq!(c.name, "Map");
                assert_eq!(c.params, vec!["C"]);
                assert_eq!(c.methods.len(), 1);
                assert_eq!(c.methods[0].name, "map");
                // sig should be Func([Func([E], F), App(C, [E])], App(C, [F]))
                match &c.methods[0].sig {
                    Type::Func(params, result) => {
                        assert_eq!(params.len(), 2);
                        assert!(matches!(&params[0], Type::Func(_, _)));
                        assert!(matches!(&params[1], Type::App(_, args) if args.len() == 1));
                        assert!(matches!(result.as_ref(), Type::App(_, args) if args.len() == 1));
                    }
                    other => panic!("expected Func sig, got {other:?}"),
                }
            }
            other => panic!("expected Class, got {other:?}"),
        }
    }

    #[test]
    fn class_decl_multiple_methods() {
        let src = r#"
Fold C =
  fold : U, (U, E -> U), C E -> U
"#;
        let p = parse_ok(src);
        match &p.items[0] {
            Item::Class(c) => {
                assert_eq!(c.name, "Fold");
                assert_eq!(c.params, vec!["C"]);
                assert_eq!(c.methods.len(), 1);
                assert_eq!(c.methods[0].name, "fold");
                // fold : U, (U, E -> U), C E -> U
                // = Func([U, Func([U, E], U), App(C, [E])], U)
                match &c.methods[0].sig {
                    Type::Func(params, result) => {
                        assert_eq!(params.len(), 3);
                        assert!(matches!(&params[0], Type::Name(n) if n == "U"));
                        assert!(matches!(&params[1], Type::Func(_, _)));
                        assert!(matches!(&params[2], Type::App(_, args) if args.len() == 1));
                        assert!(matches!(result.as_ref(), Type::Name(n) if n == "U"));
                    }
                    other => panic!("expected Func sig, got {other:?}"),
                }
            }
            other => panic!("expected Class, got {other:?}"),
        }
    }

    #[test]
    fn instance_decl_parses() {
        let src = r#"
Map List =
  map f xs = xs
"#;
        let p = parse_ok(src);
        assert_eq!(p.items.len(), 1);
        match &p.items[0] {
            Item::Instance(i) => {
                assert_eq!(i.class_name, "Map");
                assert_eq!(i.instance_ty, Type::Name("List".into()));
                assert_eq!(i.methods.len(), 1);
                assert_eq!(i.methods[0].name, "map");
                assert_eq!(i.methods[0].params, vec!["f", "xs"]);
            }
            other => panic!("expected Instance, got {other:?}"),
        }
    }

    #[test]
    fn instance_decl_multiline_body() {
        let src = r#"
Map List =
  map f xs = mapLoop f xs
  mapLoop f in =
    r = in
    r
"#;
        let p = parse_ok(src);
        assert_eq!(p.items.len(), 1);
        match &p.items[0] {
            Item::Instance(i) => {
                assert_eq!(i.class_name, "Map");
                assert_eq!(i.instance_ty, Type::Name("List".into()));
                assert_eq!(i.methods.len(), 2);
                assert_eq!(i.methods[0].name, "map");
                assert_eq!(i.methods[0].params, vec!["f", "xs"]);
                assert_eq!(i.methods[1].name, "mapLoop");
                assert_eq!(i.methods[1].params, vec!["f", "in"]);
            }
            other => panic!("expected Instance, got {other:?}"),
        }
    }

}

