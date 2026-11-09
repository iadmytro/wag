//! Parser/lexer tests — non-actor surface.
//!
//! Scope: non-actor surface available in Milestone 2 (lex, layout, Pratt parse,
//! do/list/unary-minus desugar). Actor/`@` decls, pragmas, and `with`/record-update
//! are deferred to the actor/record tests (see `catalog_actors.rs`).
//!
//! Result codes: PARSE-OK, PARSE-ERR, PARSE-OK-DEFERRED (semantic rejections belong
//! to wag-types/wag-check; the parser must accept them). Ambiguity rulings are locked
//! in DECISIONS.md (D007+).

use std::fs;
use std::path::{Path, PathBuf};

use wag_syntax::{
    lex, lex_with_layout, parse, BinOp, Expr, Item, Pat, Program, Token, Type, UpdateClause,
};

fn expr_body(src: &str) -> Expr {
    let p = parse(src).unwrap_or_else(|e| panic!("parse failed for {src:?}: {e}"));
    match &p.items[0] {
        Item::Func(f) => f.body.clone(),
        other => panic!("expected func, got {other:?}"),
    }
}

fn parse_ok(src: &str) -> Program {
    parse(src).unwrap_or_else(|e| panic!("expected PARSE-OK for {src:?}, got {e}"))
}

fn parse_err(src: &str) {
    if let Ok(p) = parse(src) {
        panic!("expected PARSE-ERR for {src:?}, got OK: {p:?}");
    }
}

fn ty_wrap(ty: &str) -> String {
    format!("___t = {ty}: x -> 0")
}

fn expr_wrap(e: &str) -> String {
    format!("___t = {e}")
}

fn pat_wrap(pat: &str) -> String {
    format!("___t = x -> match x\n  {pat} -> 0\n  _ -> 0")
}

fn first_func_body(p: &Program) -> &Expr {
    match &p.items[0] {
        Item::Func(f) => &f.body,
        _ => panic!("expected func"),
    }
}

// ----- COMPREHENSIONS -----
// Supported forms parse; unsupported forms (zip, multi-acc, multi-gen fold) are
// rejected at parse time with a clear span-anchored error (not a silent value).

#[test]
fn comp_supported_forms_parse() {
    // map
    parse_ok(&expr_wrap("[x <- xs | _ -> x + 1]"));
    // filter
    parse_ok(&expr_wrap("[x <- xs | x > 0 -> x]"));
    // filter + map
    parse_ok(&expr_wrap("[x <- xs | x > 0 -> x * 2]"));
    // multiple guards (AND)
    parse_ok(&expr_wrap("[x <- xs | x > 0, x < 100 -> x * 2]"));
    // cartesian product
    parse_ok(&expr_wrap("[x <- xs, y <- ys | x + y > 10 -> x + y]"));
    // fold (single acc)
    parse_ok(&expr_wrap("[x <- xs | acc = 0 | _ -> x + acc]"));
    // fold with guard
    parse_ok(&expr_wrap("[x <- xs | acc = 0 | x > 0 -> x + acc]"));
}

#[test]
fn comp_zip_accepted() {
    // Zip comprehensions are now supported: 2- and 3-element, with/without guards,
    // and as a fold generator. Each must parse cleanly.
    parse_ok(&expr_wrap("[x, y <- xs, ys | _ -> x + y]"));
    parse_ok(&expr_wrap("[x, y <- xs, ys | x < 3 -> x * y]"));
    parse_ok(&expr_wrap("[a, b, c <- xs, ys, zs | _ -> a + b + c]"));
    parse_ok(&expr_wrap("[x, y <- xs, ys | s = 0 | _ -> x * y + s]"));
    // Mismatched var/source counts are rejected (one variable per source list).
    parse_err(&expr_wrap("[x, y <- xs | _ -> x + y]"));
    parse_err(&expr_wrap("[x <- xs, ys | _ -> x]"));
}

#[test]
fn comp_multi_acc_rejected() {
    let err = parse(&expr_wrap("[x <- xs | a = 0, b = 0 | _ -> a + b]")).unwrap_err();
    assert!(
        err.to_string().contains("accumulator"),
        "expected a multi-acc error, got: {err}"
    );
}

#[test]
fn comp_fold_multi_gen_rejected() {
    let err = parse(&expr_wrap("[x <- xs, y <- ys | acc = 0 | _ -> acc + x]")).unwrap_err();
    assert!(
        err.to_string().contains("multiple generators"),
        "expected a multi-gen fold error, got: {err}"
    );
}

// ----- LEX -----

#[test]
fn lex_01_value_idents() {
    for id in ["x", "_foo", "mapHelper", "x1"] {
        let src = expr_wrap(id);
        parse_ok(&src);
    }
    // bare `_` is wildcard token, not a value ident in expr position
    parse_err(&expr_wrap("_"));
}

#[test]
fn lex_02_wildcard_vs_underscore_ident() {
    let ts = lex("_ _foo").unwrap();
    assert!(matches!(ts[0].value, Token::Wildcard));
    assert!(matches!(&ts[1].value, Token::Ident(s) if s == "_foo"));
    parse_ok(&pat_wrap("_"));
    parse_ok(&pat_wrap("_foo"));
}

#[test]
fn lex_03_uppercase_idents() {
    for id in ["Int", "Option", "Some", "Cons", "MyMsg"] {
        let ts = lex(id).unwrap();
        assert!(matches!(&ts[0].value, Token::UpperIdent(s) if s == id));
    }
}

#[test]
fn lex_04_builtin_ctors_not_keywords() {
    // Declaring ordinary uppercase names that look like builtins must lex/parse.
    let src = r#"
Trueish = Trueish
Falseish = Falseish
Somelike = Somelike
Nonelike = Nonelike
Emptyish = Emptyish
Listish = Consish Int Listish | Emptyish
Startish = Startish
Downish = Downish
"#;
    parse_ok(src);
}

#[test]
fn lex_05_at_names_tokenize() {
    for src in ["@Counter", "@main", "@std", "@x"] {
        let ts = lex(src).unwrap();
        assert!(matches!(ts[0].value, Token::At), "{src}");
        assert!(matches!(
            &ts[1].value,
            Token::UpperIdent(_) | Token::Ident(_)
        ));
    }
}

#[test]
fn lex_06_at_space_ambiguous_accepted_as_two_tokens() {
    // Ruling: ERR. Lexer treats `@` and name separately
    // with or without space (D007).
    let a = lex("@Counter").unwrap();
    let b = lex("@ Counter").unwrap();
    assert_eq!(a.len(), 2);
    assert_eq!(b.len(), 2);
    assert!(matches!(a[0].value, Token::At));
    assert!(matches!(b[0].value, Token::At));
}

#[test]
fn lex_07_keywords_not_idents() {
    let kws = [
        "if", "match", "let", "in", "do", "return", "spawn", "send", "self", "copy",
        "retire", "monitor",
    ];
    for kw in kws {
        // keyword must not be usable as a binding name
        let src = format!("___t = {kw} = 5\n0");
        parse_err(&src);
    }
}

#[test]
fn lex_08_line_comment() {
    parse_ok("-- comment to end of line\n___t = 5");
}

#[test]
fn lex_09_block_comment() {
    parse_ok("{- block -}\n___t = 5");
}

#[test]
fn lex_10_nested_block_comments() {
    parse_ok("{- outer {- inner -} still outer -}\n___t = 5");
    parse_ok("{- a {- b {- c -} b -} a -}\n___t = 1");
}

#[test]
fn lex_11_unterminated_block_comment() {
    assert!(lex("{- unterminated").is_err());
    parse_err("{- unterminated\n___t = 1");
}

#[test]
fn lex_12_outer_unclosed_nested_comment() {
    assert!(lex("{- outer {- inner -}").is_err());
}

#[test]
fn lex_13_decimal_lits() {
    for n in ["0", "42", "1234567890"] {
        parse_ok(&expr_wrap(n));
    }
}

#[test]
fn lex_14_hex_lits() {
    for n in ["0xFF", "0x1a2b", "0XAB", "0xff", "0x0"] {
        parse_ok(&expr_wrap(n));
    }
}

#[test]
fn lex_15_hex_needs_digits() {
    assert!(lex("0x").is_err());
    parse_err(&expr_wrap("0x"));
}

#[test]
fn lex_16_hex_bad_digit() {
    assert!(lex("0xGG").is_err());
    parse_err(&expr_wrap("0xGG"));
}

#[test]
fn lex_17_leading_zeros_ambiguous_accept() {
    // AMBIGUOUS — accept as decimal 7 (D007)
    let p = parse_ok(&expr_wrap("007"));
    match first_func_body(&p) {
        Expr::Int { value: 7, .. } => {}
        other => panic!("expected 7, got {other:?}"),
    }
}

#[test]
fn lex_18_no_float_literals() {
    // Tokenizes as Int Dot Int; field parse fails → PARSE-ERR
    parse_err(&expr_wrap("3.14"));
    parse_err(&expr_wrap("1.5e10"));
}

#[test]
fn lex_19_dot_digit_not_field() {
    parse_err("f = p -> p.5");
}

#[test]
fn lex_20_operator_tokens() {
    // 25 distinct operator/punct tokens (spelling variants of multi-char ops counted once each).
    let src = "-> <- >= <= != == + - * / > < : = ( ) [ ] { } ; , @ & .";
    let ts = lex(src).unwrap();
    assert_eq!(ts.len(), 25, "{ts:?}");
}

#[test]
fn lex_21_through_25_maximal_munch() {
    let ts = lex("-> <- >= <= != ==").unwrap();
    assert!(matches!(ts[0].value, Token::Arrow));
    assert!(matches!(ts[1].value, Token::LeftArrow));
    assert!(matches!(ts[2].value, Token::Ge));
    assert!(matches!(ts[3].value, Token::Le));
    assert!(matches!(ts[4].value, Token::Ne));
    assert!(matches!(ts[5].value, Token::EqEq));
    assert!(matches!(lex("!").unwrap()[0].value, Token::Bang));
}

#[test]
fn lex_26_eqeq_vs_eq_eq() {
    parse_ok("f = a, b -> a==b");
    parse_err("f = a, b -> a = =b");
}

#[test]
fn lex_27_x_minus_1() {
    let b = expr_body("f = x -> x-1");
    match b {
        Expr::BinOp {
            op: BinOp::Sub,
            lhs,
            rhs,
            ..
        } => {
            assert!(matches!(lhs.as_ref(), Expr::Var { name, .. } if name == "x"));
            assert!(matches!(rhs.as_ref(), Expr::Int { value: 1, .. }));
        }
        other => panic!("{other:?}"),
    }
}

// ----- LAY -----

#[test]
fn lay_01_do_indent() {
    let src = r#"
f = x -> do
  v1 <- add x x
  v2 <- multiply v1 2
  return v2
"#;
    let p = parse_ok(src);
    assert!(matches!(first_func_body(&p), Expr::Apply { .. }), "do desugars to monad bind");
}

#[test]
fn lay_02_do_braces_same_shape() {
    let a = r#"
f = x -> do
  v1 <- add x x
  v2 <- multiply v1 2
  return v2
"#;
    let b = r#"
f = x -> do { v1 <- add x x; v2 <- multiply v1 2; return v2 }
"#;
    let pa = parse_ok(a);
    let pb = parse_ok(b);
    let ba = first_func_body(&pa);
    let bb = first_func_body(&pb);
    // Compare structure ignoring spans: Debug with spans differs — strip via shape helpers
    assert_eq!(shape_expr(ba), shape_expr(bb));
}

fn shape_expr(e: &Expr) -> String {
    match e {
        Expr::Int { value, .. } => format!("Int({value})"),
        Expr::Str { value, .. } => format!("Str({value})"),
        Expr::Var { name, .. } => format!("Var({name})"),
        Expr::Upper { name, .. } => format!("Upper({name})"),
        Expr::Self_ { .. } => "Self".into(),
        Expr::Unit { .. } => "Unit".into(),
        Expr::Apply { fun, args, .. } => format!(
            "App({},[{}])",
            shape_expr(fun),
            args.iter().map(shape_expr).collect::<Vec<_>>().join(",")
        ),
        Expr::BinOp { op, lhs, rhs, .. } => {
            format!("Bin({op:?},{},{})", shape_expr(lhs), shape_expr(rhs))
        }
        Expr::Field { base, field, .. } => format!("Field({},{})", shape_expr(base), field),
        Expr::SetField { receiver, field_idx, value, .. } => format!("Set({},{},{})", shape_expr(receiver), field_idx, shape_expr(value)),
        Expr::RegionWrite { receiver, path, value, .. } => format!("RWrite({},{:?},{})", shape_expr(receiver), path, shape_expr(value)),
        Expr::Borrow { inner, .. } => format!("Borrow({})", shape_expr(inner)),
        Expr::Bind { name, value, body, .. } => {
            format!("Bind({name},{},{})", shape_expr(value), shape_expr(body))
        }
        Expr::Seq { first, second, .. } => {
            format!("Seq({},{})", shape_expr(first), shape_expr(second))
        }
        Expr::If { arms, .. } => format!(
            "If([{}])",
            arms.iter()
                .map(|a| format!(
                    "{}=>{}",
                    a.condition
                        .as_ref()
                        .map(shape_expr)
                        .unwrap_or_else(|| "_".into()),
                    shape_expr(&a.body)
                ))
                .collect::<Vec<_>>()
                .join(";")
        ),
        Expr::Match { scrutinee, arms, .. } => format!(
            "Match({},[{}])",
            shape_expr(scrutinee),
            arms.iter()
                .map(|a| format!("{}=>{}", shape_pat(&a.pattern), shape_expr(&a.body)))
                .collect::<Vec<_>>()
                .join(";")
        ),
        Expr::Do { stmts, .. } => format!("Do({stmts:?})"),
        Expr::ListLit { elems, .. } => format!(
            "List([{}])",
            elems.iter().map(shape_expr).collect::<Vec<_>>().join(",")
        ),
        Expr::AtName { name, .. } => format!("At({name})"),
        Expr::RecordUpdate {
            receiver, clauses, ..
        } => format!(
            "Upd({},[{}])",
            shape_expr(receiver),
            clauses
                .iter()
                .map(shape_clause)
                .collect::<Vec<_>>()
                .join(";")
        ),
        Expr::Lambda { params, body, .. } => format!(
            "Lambda([{}],{})",
            params.join(","),
            shape_expr(body)
        ),
        Expr::Comprehension { generators, accs, guards, result, .. } => format!(
            "Comp(gen={},acc={},g={},r={})",
            generators.len(),
            accs.len(),
            guards.len(),
            shape_expr(result)
        ),
        Expr::Product { elems, .. } => format!(
            "Prod([{}])",
            elems.iter().map(shape_expr).collect::<Vec<_>>().join(",")
        ),
        Expr::Send { target, msg, .. } => format!("Send({},{})", shape_expr(target), shape_expr(msg)),
    }
}

fn shape_clause(c: &UpdateClause) -> String {
    match c {
        UpdateClause::Leaf { path, value, .. } => {
            format!("L({}.={})", path.join("."), shape_expr(value))
        }
        UpdateClause::Group { path, clauses, .. } => format!(
            "G({}.[{}])",
            path.join("."),
            clauses
                .iter()
                .map(shape_clause)
                .collect::<Vec<_>>()
                .join(";")
        ),
    }
}

fn shape_pat(p: &Pat) -> String {
    match p {
        Pat::Wildcard { .. } => "_".into(),
        Pat::Literal { lit, .. } => match lit {
            wag_syntax::LitPat::Int(v) => format!("L({v})"),
            wag_syntax::LitPat::Str(v) => format!("L(\"{v}\")"),
        },
        Pat::Var { name, .. } => format!("v({name})"),
        Pat::Ctor { name, args, .. } => format!(
            "C({name},[{}])",
            args.iter().map(shape_pat).collect::<Vec<_>>().join(",")
        ),
        Pat::Product { elems, .. } => format!(
            "P({})",
            elems.iter().map(shape_pat).collect::<Vec<_>>().join(",")
        ),
    }
}

#[test]
fn lay_03_dedent_closes() {
    let src = r#"
f = x -> do
  v1 <- add x x
  return v1
g = y -> y
"#;
    let p = parse_ok(src);
    assert_eq!(p.items.len(), 2);
}

#[test]
fn lay_04_continuation_line() {
    // deeper indent continues previous item (expression continuation)
    let src = r#"
f = x -> do
  v1 <- add
    x x
  return v1
"#;
    // May or may not parse depending on layout; try
    let _ = parse(src);
}

#[test]
fn lay_05_multi_level_dedent() {
    let src = r#"
f = x -> do
  a <- do
    b <- g x
    return b
  return a
"#;
    parse_ok(src);
}

#[test]
fn lay_06_blank_line_in_block() {
    let src = r#"
f = x -> do
  v1 <- add x x

  return v1
"#;
    parse_ok(src);
}

#[test]
fn lay_07_comment_line_in_block() {
    let src = r#"
f = x -> do
  v1 <- add x x
  -- skip
  return v1
"#;
    parse_ok(src);
}

#[test]
fn lay_08_block_comment_in_block() {
    let src = r#"
f = x -> do
  v1 <- add x x
  {- c -}
  return v1
"#;
    parse_ok(src);
}

#[test]
fn lay_09_if_layout() {
    let src = r#"
f = x -> if
  x > 0 -> x
  _ -> 0
"#;
    parse_ok(src);
    let src2 = r#"
f = x -> if x > 0 -> x
  _ -> 0
"#;
    parse_ok(src2);
}

// LAY-10 removed (D051): `with`/`<-` field-update syntax no longer parses.

#[test]
fn lay_11_match_layout() {
    let src = r#"
f = x -> match x
  Cons h t -> h
  Empty -> 0
"#;
    parse_ok(src);
}

#[test]
fn lay_12_tabs_ambiguous() {
    // AMBIGUOUS — current layout may accept or reject tabs; record actual
    let src = "f = x -> do\n\tv1 <- add x x\n\treturn v1\n";
    let _ = parse(src); // do not assert until D007 locks tabs
}


// ----- PREC -----

#[test]
fn prec_01_field_chain_left() {
    let b = expr_body("f = p -> p.address.city");
    match b {
        Expr::Field { base, field, .. } => {
            assert_eq!(field, "city");
            match base.as_ref() {
                Expr::Field { base, field, .. } => {
                    assert_eq!(field, "address");
                    assert!(matches!(base.as_ref(), Expr::Var { name, .. } if name == "p"));
                }
                other => panic!("{other:?}"),
            }
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn prec_02_field_tighter_than_app() {
    let b = expr_body("f = p -> g p.age");
    match b {
        Expr::Apply { fun, args, .. } => {
            assert!(matches!(fun.as_ref(), Expr::Var { name, .. } if name == "g"));
            assert_eq!(args.len(), 1);
            assert!(matches!(&args[0], Expr::Field { field, .. } if field == "age"));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn prec_03_flat_nary_app() {
    let b = expr_body("f = map x1 x2 x3");
    match b {
        Expr::Apply { fun, args, .. } => {
            assert!(matches!(fun.as_ref(), Expr::Var { name, .. } if name == "map"));
            assert_eq!(args.len(), 3);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn prec_04_ctor_app_flat() {
    for (src, n) in [("f = Some x", 1), ("f = Cons h t", 2), ("f = Pair a b", 2), ("f = Triple a b c", 3)] {
        let b = expr_body(src);
        match b {
            Expr::Apply { args, .. } => assert_eq!(args.len(), n, "{src}"),
            other => panic!("{src}: {other:?}"),
        }
    }
}

#[test]
fn prec_05_mul_tighter_than_add() {
    let b = expr_body("f = 5 + 3 * 2");
    match b {
        Expr::BinOp {
            op: BinOp::Add,
            rhs,
            ..
        } => assert!(matches!(rhs.as_ref(), Expr::BinOp { op: BinOp::Mul, .. })),
        other => panic!("{other:?}"),
    }
}

#[test]
fn prec_06_mul_then_add() {
    let b = expr_body("f = 5 * 3 + 2");
    match b {
        Expr::BinOp {
            op: BinOp::Add,
            lhs,
            ..
        } => assert!(matches!(lhs.as_ref(), Expr::BinOp { op: BinOp::Mul, .. })),
        other => panic!("{other:?}"),
    }
}

#[test]
fn prec_07_add_left_assoc() {
    let b = expr_body("f = 1 + 2 + 3");
    assert!(matches!(
        b,
        Expr::BinOp {
            op: BinOp::Add,
            lhs,
            ..
        } if matches!(lhs.as_ref(), Expr::BinOp { op: BinOp::Add, .. })
    ));
}

#[test]
fn prec_08_sub_left_assoc() {
    let b = expr_body("f = 1 - 2 - 3");
    assert!(matches!(
        b,
        Expr::BinOp {
            op: BinOp::Sub,
            lhs,
            ..
        } if matches!(lhs.as_ref(), Expr::BinOp { op: BinOp::Sub, .. })
    ));
}

#[test]
fn prec_09_mul_div_left_assoc() {
    let b = expr_body("f = 1 * 2 / 3");
    assert!(matches!(
        b,
        Expr::BinOp {
            op: BinOp::Div,
            lhs,
            ..
        } if matches!(lhs.as_ref(), Expr::BinOp { op: BinOp::Mul, .. })
    ));
}

#[test]
fn prec_10_compare() {
    let b = expr_body("f = a, b -> a > b");
    assert!(matches!(b, Expr::BinOp { op: BinOp::Gt, .. }));
}

#[test]
fn prec_11_nonassoc_compare_chain() {
    parse_err("f = 1 > 2 > 3");
}

#[test]
fn prec_12_paren_compare_ok() {
    parse_ok("f = a, b -> (a > b) == True");
}




#[test]
fn prec_16_spawn_at_atom() {
    let p = parse_ok("f = spawn @Counter 0");
    let body = first_func_body(&p);
    assert!(matches!(
        body,
        Expr::Apply { args, .. }
            if args.len() == 2
                && matches!(&args[0], Expr::AtName { name, .. } if name == "Counter")
    ));
}

#[test]
fn prec_17_self_atom() {
    let b = expr_body("f = self");
    assert!(matches!(b, Expr::Self_ { .. }));
}

#[test]
fn prec_18_umin_then_sub() {
    let b = expr_body("f = x, y -> -x - y");
    // (0 - x) - y
    assert_eq!(
        shape_expr(&b),
        "Bin(Sub,Bin(Sub,Int(0),Var(x)),Var(y))"
    );
}

#[test]
fn prec_19_umin_vs_mul() {
    // Precedence: `-x * 2` ⇒ `0 - (x * 2)`
    let b = expr_body("f = x -> -x * 2");
    assert_eq!(
        shape_expr(&b),
        "Bin(Sub,Int(0),Bin(Mul,Var(x),Int(2)))"
    );
}

#[test]
fn prec_20_f_minus_x_is_sub() {
    let b = expr_body("f = x -> g -x");
    assert!(matches!(b, Expr::BinOp { op: BinOp::Sub, .. }));
}

#[test]
fn prec_21_paren_umin_as_arg() {
    let b = expr_body("f = x -> g (-x)");
    match b {
        Expr::Apply { args, .. } => {
            assert!(matches!(
                &args[0],
                Expr::BinOp {
                    op: BinOp::Sub,
                    ..
                }
            ));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn prec_23_keywords_as_apps() {
    parse_ok("f = spawn Counter 0");
    parse_ok("f = a, m -> send a m");
    parse_ok("f = x -> copy x");
    parse_ok("f = w -> retire w");
    parse_ok("f = t -> monitor t");
}

// ----- ATOM -----

#[test]
fn atom_01_basic_atoms() {
    parse_ok(&expr_wrap("x"));
    parse_ok(&expr_wrap("42"));
    parse_ok(&expr_wrap("0xFF"));
    parse_ok(&expr_wrap("(1 + 2)"));
    let b = expr_body(&expr_wrap("@Name"));
    assert!(matches!(b, Expr::AtName { name, .. } if name == "Name"));
}

#[test]
fn atom_02_ctor_in_app_flat_ambiguous() {
    // AMBIGUOUS — flat application: f Some x → App(f,[Some,x]) (D007)
    let b = expr_body("f = g Some x");
    match b {
        Expr::Apply { fun, args, .. } => {
            assert!(matches!(fun.as_ref(), Expr::Var { name, .. } if name == "g"));
            assert_eq!(args.len(), 2);
            assert!(matches!(&args[0], Expr::Upper { name, .. } if name == "Some"));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn atom_03_paren_ctor_app() {
    let b = expr_body("f = g (Some x)");
    match b {
        Expr::Apply { args, .. } => {
            assert_eq!(args.len(), 1);
            assert!(matches!(&args[0], Expr::Apply { .. }));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn atom_04_match_some_x_scrutinee() {
    // Same flat-atom rule: `Some` is atom, `x` may start arm or continue — layout dependent.
    // Bare multi-token scrutinee without parens is rejected when `x` is eaten as arm pattern.
    let src = "___t = y -> match Some x\n  _ -> 0\n";
    // Current: may ERR or OK; prefer ERR for multi-arg bare ctor as scrutinee when ambiguous
    let _ = parse(src);
}

// ATOM-07/08 removed (D051): `with` is no longer a keyword.

#[test]
fn atom_09_field_as_arg() {
    let b = expr_body("f = g x.y");
    match b {
        Expr::Apply { args, .. } => {
            assert!(matches!(&args[0], Expr::Field { field, .. } if field == "y"));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn atom_11_paren_match_scrutinee() {
    parse_ok(
        r#"
___t = y -> match (compute x)
  _ -> 0
"#,
    );
}

#[test]
fn atom_12_bare_app_scrutinee() {
    parse_err(
        r#"
___t = y -> match compute x
  _ -> 0
"#,
    );
}

// ATOM-13 removed (D051): `with` is no longer a keyword.


#[test]
fn atom_15_field_scrutinee() {
    parse_ok(
        r#"
___t = p -> match p.address.city
  _ -> 0
"#,
    );
}

// ----- CTRL -----

#[test]
fn ctrl_01_if_two_arms() {
    parse_ok(
        r#"
f = x -> if
  x > 0 -> 1
  _ -> 0
"#,
    );
}

#[test]
fn ctrl_02_if_no_catchall_ambiguous_ok() {
    // AMBIGUOUS — accept; exhaustiveness is later
    parse_ok(
        r#"
f = x -> if
  x > 0 -> 1
  x < 0 -> 2
"#,
    );
}

#[test]
fn ctrl_03_match_basic() {
    parse_ok(
        r#"
f = x -> match x
  Some y -> y
  None -> 0
"#,
    );
}

#[test]
fn ctrl_04_match_wildcard_arm() {
    parse_ok(
        r#"
f = x -> match x
  Cons h t -> h
  _ -> 0
"#,
    );
}

#[test]
fn ctrl_05_arm_after_catchall_deferred() {
    // PARSE-OK-DEFERRED (redundant arms / exhaustiveness later)
    parse_ok(
        r#"
f = x -> match x
  _ -> 0
  Some y -> y
"#,
    );
}

#[test]
fn ctrl_06_nested_do_if_match() {
    parse_ok(
        r#"
f = x -> do
  y <- if
    x > 0 -> Some x
    _ -> None
  return match y
    Some z -> z
    None -> 0
"#,
    );
}

#[test]
fn ctrl_07_empty_match_err() {
    parse_err("f = x -> match x {\n}");
    parse_err("f = x -> match x\n");
}

#[test]
fn ctrl_08_field_scrutinee() {
    parse_ok(
        r#"
f = p -> match p.age
  _ -> 0
"#,
    );
}

// ----- TYPE -----

#[test]
fn type_01_base() {
    for t in ["Int", "Bool", "Unit"] {
        parse_ok(&ty_wrap(t));
    }
}

#[test]
fn type_02_apps() {
    parse_ok(&ty_wrap("Option Int"));
    parse_ok(&ty_wrap("List Int"));
    parse_ok(&ty_wrap("Pair Int Bool"));
    parse_ok(&ty_wrap("Triple Int Bool Unit"));
}

#[test]
fn type_03_actor_types() {
    parse_ok(&ty_wrap("Actor CounterMsg"));
    parse_ok(&ty_wrap("ChildActor CounterMsg"));
}

#[test]
fn type_04_unknown_type_deferred() {
    parse_ok(&ty_wrap("MyUserType"));
}

#[test]
fn type_05_paren_group() {
    parse_ok(&ty_wrap("(Int)"));
}

#[test]
fn type_06_ho_param() {
    parse_ok(
        r#"
map = (Int -> Option Int), List Int -> Option (List Int): f, xs -> None
"#,
    );
}

#[test]
fn type_07_second_arrow_err() {
    parse_err(
        r#"
f: Int -> Option Int, Bool -> Option Int
f = 0
"#,
    );
}

#[test]
fn type_08_spawn_double_paren() {
    // HO function type as a parameter (single-arg form; multi-param HO uses
    // `(State -> ...)` until nested `(State, Msg) ->` inside extra parens is generalized).
    parse_ok(
        r#"
spawnLike = (State -> Option State), State -> ChildActor Msg: h, s -> None
"#,
    );
    // Multi-param function type nested as ParamT — `((State, Msg) -> Option State)`
    parse_ok(
        r#"
spawnLike2 = ((State, Msg) -> Option State) -> ChildActor Msg: h -> None
"#,
    );
}

#[test]
fn type_09_nullary_sig_via_ctor() {
    parse_ok("Boot = Boot");
}

#[test]
fn type_10_self_type_as_name() {
    // `self` is keyword in expr; as type name use Actor Msg
    parse_ok(&ty_wrap("Actor Msg"));
}

#[test]
fn type_11_borrow_param() {
    parse_ok(
        r#"
isEmpty = &List Int -> Bool: xs -> True
"#,
    );
}

#[test]
fn type_12_borrow_scalar_param_deferred() {
    // well-formedness of borrowing non-pointer types is later; syntax OK
    parse_ok(
        r#"
f = &Int -> Int: x -> 0
"#,
    );
}

#[test]
fn type_13_borrow_nested_in_option_err() {
    parse_err(&ty_wrap("Option (&List Int)"));
    parse_err(&ty_wrap("Option (&Int)"));
}

#[test]
fn type_14_borrow_nested_in_type_app_err() {
    parse_err(&ty_wrap("List (&Int)"));
    parse_err(&ty_wrap("Pair (&List Int) Int"));
    // field whose type is App with nested borrow
    parse_err("Bad = Bad (&Int)");
}

#[test]
fn type_15_lowercase_type_err() {
    // Lowercase type names are a type-check error, not a parse error.
    // The parser accepts them; wag-types rejects them.
    parse_ok(&ty_wrap("a -> a"));
}

#[test]
fn type_16_unparen_multi_param_nested() {
    // bare multi-param FuncT nested without parens — rejected as second arrow / bad nest
    parse_err(
        r#"
f: Int, Bool -> Option Int -> Int
f = 0
"#,
    );
}

#[test]
fn type_17_deep_parens() {
    parse_ok(&ty_wrap("((((Int))))"));
}

// ----- CTOR -----

#[test]
fn ctor_01_nullary() {
    let p = parse_ok("Boot = Boot");
    match &p.items[0] {
        Item::ParametricType(pt) => assert_eq!(pt.ctors[0].name, "Boot"),
        _ => panic!("expected ParametricType"),
    }
}

#[test]
fn ctor_02_unlabeled_fields() {
    parse_ok("MyList = Cons Int MyList");
}

#[test]
fn ctor_03_labeled_fields() {
    let p = parse_ok("Person = Person String Int");
    match &p.items[0] {
        Item::ParametricType(pt) => {
            assert_eq!(pt.ctors[0].fields.len(), 2);
        }
        _ => panic!(),
    }
}

#[test]
fn ctor_04_mixed_group() {
    let src = r#"
CounterMsg = Increment | Get Actor ReplyMsg
ReplyMsg = Reply Int
"#;
    let p = parse_ok(src);
    let g = p.constructors_by_type();
    assert!(g.iter().any(|(k, v)| k == "CounterMsg" && v.len() == 2));
}

#[test]
fn ctor_05_option_like() {
    parse_ok("MyOption = Some Int | None");
}

#[test]
fn ctor_06_borrow_field_param() {
    // top-level ParamT borrow on a field is OK
    parse_ok("Peek = Peek &List Int");
}

#[test]
fn ctor_07_result_type_name() {
    let p = parse_ok("MyPair = MyPair Int Int");
    match &p.items[0] {
        Item::ParametricType(pt) => assert!(matches!(&pt.ctors[0].result_type, Type::Name(n) if n == "MyPair")),
        _ => panic!(),
    }
}

#[test]
fn ctor_08_multiline_person() {
    parse_ok("Person = Person String Int");
}

#[test]
fn ctor_09_uppercase_only_names() {
    parse_err("person: Int -> person");
}

#[test]
fn ctor_10_11_duplicate_deferred() {
    // duplicate ctor names — parser may accept (deferred) or reject; deferred to wag-types by design
    let src = r#"
A: T
A: T
"#;
    // Parser currently may parse both as constructors
    let _ = parse(src);
}

#[test]
fn ctor_12_labeled_nullary_malformed() {
    // `name:` without type
    parse_err("Person: name: -> Person");
}

// ----- FLD (access only; update deferred) -----

#[test]
fn fld_01_single_field() {
    let b = expr_body("f = p -> p.age");
    assert!(matches!(b, Expr::Field { field, .. } if field == "age"));
}

#[test]
fn fld_02_chain() {
    parse_ok("f = p -> p.address.city");
}

// FLD-03 (D051): the `<-` field-update syntax was removed; record mutation is
// now application-based (`set_age p 1`). Both old forms must fail to parse.
#[test]
fn fld_03_field_update_removed() {
    parse_err("f = p -> p.age <- 1");
    parse_err(
        r#"
f = p -> with p
  .age <- 1
"#,
    );
}

// ----- DO -----

#[test]
fn do_01_desugar_bind_chain() {
    let src = r#"
f = do
  a <- g 1
  b <- h a
  return b
"#;
    let p = parse_ok(src);
    let b = first_func_body(&p);
    // outermost is __monad_bind application
    match b {
        Expr::Apply { fun, .. } => {
            assert!(matches!(fun.as_ref(), Expr::Upper { name, .. } if name == "__monad_bind"));
        }
        _ => panic!("expected Apply(__monad_bind, ...)"),
    }
}

#[test]
fn do_02_final_bare_expr_rejected() {
    // a final bare expression is NOT a `do` statement — `do` chains `Option`
    // results via `<-` / `return` only; sequence statements in a block instead.
    let src = r#"
f = do
  a <- g 1
  a
"#;
    parse_err(src);
}

#[test]
fn do_03_return_some() {
    let src = r#"
f = do
  return 1
"#;
    let p = parse_ok(src);
    let b = first_func_body(&p);
    // single return → Some 1 (apply or upper?)
    assert!(
        matches!(b, Expr::Apply { .. }) || matches!(b, Expr::Upper { .. }) || matches!(b, Expr::Match { .. }),
        "{b:?}"
    );
}

#[test]
fn do_04_only_exprs_rejected() {
    // bare expressions (e.g. message sends) are not `do` statements.
    parse_err(
        r#"
f = do
  g 1
  h 2
"#,
    );
}

#[test]
fn do_05_mix_bind_expr_rejected() {
    // a bare expression mixed with `<-` binds is not valid `do`.
    parse_err(
        r#"
f = do
  g 1
  x <- h 2
  return x
"#,
    );
}

#[test]
fn do_06_empty_do() {
    // empty do body — ERR or empty match chain
    let r = parse("f = do {\n}");
    // accept either ERR or OK with empty desugar; prefer documenting
    let _ = r;
}

#[test]
fn do_07_nested_do() {
    parse_ok(
        r#"
f = do
  x <- do
    y <- g 1
    return y
  return x
"#,
    );
}

// ----- PAT -----

#[test]
fn pat_01_var() {
    parse_ok(&pat_wrap("x"));
}

#[test]
fn pat_02_wildcard() {
    parse_ok(&pat_wrap("_"));
}

#[test]
fn pat_03_bool_ctors() {
    parse_ok(&pat_wrap("True"));
    parse_ok(&pat_wrap("False"));
}

#[test]
fn pat_04_some() {
    parse_ok(&pat_wrap("Some x"));
}

#[test]
fn pat_05_none() {
    parse_ok(&pat_wrap("None"));
}

#[test]
fn pat_06_Empty() {
    parse_ok(&pat_wrap("Empty"));
}

#[test]
fn pat_07_cons() {
    parse_ok(&pat_wrap("Cons h t"));
}

#[test]
fn pat_08_pair_triple() {
    parse_ok(&pat_wrap("Pair a b"));
    parse_ok(&pat_wrap("Triple a b c"));
}

#[test]
fn pat_09_user_ctor() {
    parse_ok(&pat_wrap("MyCtor p1 p2 p3"));
}

#[test]
fn pat_10_nested_ctor_ok() {
    // AMBIGUOUS — accepts nested ctor patterns (D007)
    parse_ok(&pat_wrap("Cons (Some x) t"));
}

#[test]
fn pat_11_var_catchall() {
    parse_ok(
        r#"
f = x -> match x
  Some y -> y
  z -> z
"#,
    );
}

#[test]
fn pat_12_lowercase_not_special() {
    parse_ok(&pat_wrap("some"));
    parse_ok(&pat_wrap("none"));
    parse_ok(&pat_wrap("Empty"));
    parse_ok(&pat_wrap("cons"));
}

// ----- LIST -----

#[test]
fn list_01_three() {
    // `[...]` is a persistent AST node (ListLit), not desugared to Cons/Empty.
    let b = expr_body("f = [1, 2, 3]");
    assert_eq!(shape_expr(&b), "List([Int(1),Int(2),Int(3)])");
}

#[test]
fn list_02_singleton() {
    let b = expr_body("f = [1]");
    assert_eq!(shape_expr(&b), "List([Int(1)])");
}

#[test]
fn list_03_empty() {
    let b = expr_body("f = []");
    assert!(matches!(b, Expr::ListLit { elems, .. } if elems.is_empty()));
}

#[test]
fn list_04_trailing_comma_err() {
    parse_err("f = [1, 2, 3,]");
}

#[test]
fn list_05_app_elements() {
    parse_ok("f = [Some 1, None, Some 3]");
}

#[test]
fn list_06_nested() {
    parse_ok("f = [[1, 2], [3, 4]]");
}

// ----- FN -----

#[test]
fn fn_01_sig_and_eq() {
    parse_ok(
        r#"
f = Int -> Int: x -> x
"#,
    );
}

#[test]
fn fn_02_eq_only() {
    parse_ok("f = x -> x");
}

#[test]
fn fn_03_multi_params() {
    let p = parse_ok("f = a, b, c -> a");
    match &p.items[0] {
        Item::Func(f) => assert_eq!(f.params, vec!["a", "b", "c"]),
        _ => panic!(),
    }
}

#[test]
fn fn_04_zero_params() {
    parse_ok("f = 1");
}

#[test]
fn fn_05_duplicate_equation_err() {
    parse_err("f = 1\nf = 2");
}

#[test]
fn fn_06_sig_without_equation_err() {
    parse_err("f: Int -> Int");
}

#[test]
fn fn_07_multi_clause_patterns_err() {
    // single equation only; pattern params not in equations
    parse_err("f 0 = 1\nf n = n");
}

#[test]
fn fn_08_single_equation_rule_enforced_in_parser() {
    // Deferred to wag-types by design; parser already rejects duplicates (stricter OK for tests)
    parse_err(
        r#"
g x = x
g = y -> y
"#,
    );
}

// ----- UMIN -----

#[test]
fn umin_01_desugar() {
    let a = shape_expr(&expr_body("f = x -> -x"));
    let b = shape_expr(&expr_body("f = x -> 0 - x"));
    assert_eq!(a, b);
}

#[test]
fn umin_02_spaced() {
    parse_ok("f = x -> - x");
}

#[test]
fn umin_03_double() {
    let b = expr_body("f = x -> - -x");
    // 0 - (0 - x)
    assert!(matches!(b, Expr::BinOp { op: BinOp::Sub, .. }));
}

#[test]
fn umin_04_in_list() {
    parse_ok("f = [-1, 2, -3]");
}

#[test]
fn umin_05_paren() {
    parse_ok("f = x -> (-x)");
}

#[test]
fn umin_06_not_in_literal_token() {
    let ts = lex("-7").unwrap();
    assert!(matches!(ts[0].value, Token::Minus));
    assert!(matches!(ts[1].value, Token::IntLit(7)));
}

// ----- CMT -----

#[test]
fn cmt_01_line() {
    parse_ok("f = 1 -- tail\n");
}

#[test]
fn cmt_02_block() {
    parse_ok("f = {- x -} 1");
}

#[test]
fn cmt_03_nested() {
    parse_ok("f = {- {- x -} -} 1");
}

#[test]
fn cmt_04_unterminated_err() {
    parse_err("f = {- oops");
}

#[test]
fn cmt_05_depth_err() {
    parse_err("f = {- a {- b -}");
}

// ----- ERR -----

#[test]
fn err_01_unclosed_paren() {
    parse_err("f = (1 + 2");
}

#[test]
fn err_02_unclosed_bracket() {
    parse_err("f = [1, 2");
}

#[test]
fn err_03_bare_arrow() {
    parse_err("f = ->");
}

#[test]
fn err_04_stray_eq() {
    parse_err("f = = 1");
}

#[test]
fn err_05_empty_input() {
    // Empty source is a valid empty program (no items).
    let p = parse_ok("");
    assert!(p.items.is_empty());
}

#[test]
fn err_06_only_comment() {
    let p = parse_ok("-- nothing\n");
    assert!(p.items.is_empty());
}

#[test]
fn err_07_junk_token() {
    parse_err("f = §x");
}

#[test]
fn err_08_missing_rhs() {
    parse_err("f x =");
}

#[test]
fn err_09_match_without_arrow() {
    parse_err(
        r#"
f = x -> match x
  Some y y
"#,
    );
}

#[test]
fn err_10_with_as_label_deferred_keyword() {
    // `with` is keyword — cannot be field label
    parse_err("Person: with: Int -> Person");
}

// ----- INT (fixtures + deferred corpus) -----

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("fixtures")
}

#[test]
fn int_m2_fixtures_parse_ok() {
    let names = [
        "basics/factorial.wag",
        "basics/fibonacci.wag",
        "lists/sum_list.wag",
        "lists/length.wag",
        "lists/is_empty.wag",
        "lists/reverse.wag",
        "lists/map.wag",
        "lists/filter.wag",
        "lists/max_list.wag",
        "basics/gcd.wag",
        "basics/pow.wag",
        "lists/contains.wag",
        "lists/count.wag",
        "lists/insertion_sort.wag",
        "linearity/copy_vs_borrow.wag",
        "lists/is_sorted.wag",
        "basics/mutual_recursion.wag",
        "basics/higher_order_cycles.wag",
    ];
    let dir = fixtures_dir();
    for name in names {
        let src = fs::read_to_string(dir.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        parse_ok(&src);
    }
}

#[test]
fn int_23_24_deferred_still_parse() {
    // INT-23/24: must parse; rejected later by wag-check
    let dir = fixtures_dir();
    parse_ok(&fs::read_to_string(dir.join("basics/mutual_recursion.wag")).unwrap());
    parse_ok(&fs::read_to_string(dir.join("basics/higher_order_cycles.wag")).unwrap());
}

#[test]
fn int_actor_fixtures_parse_m3() {
    let dir = fixtures_dir();
    for name in [
        "actors/counter_supervised.wag",
        "io/main_std.wag",
        "actors/monitor.wag",
        "types/field_access.wag",
    ] {
        let src = fs::read_to_string(dir.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        parse_ok(&src);
    }
}

#[test]
fn layout_token_stream_smoke() {
    let ts = lex_with_layout(
        r#"
f = x -> do
  return x
"#,
    )
    .unwrap();
    assert!(
        ts.iter()
            .any(|t| matches!(t.value, Token::VirtualLBrace | Token::LBrace | Token::Do))
    );
}

// ---- D034: unified declaration syntax — old 2-line form rejected ----

#[test]
fn d034_old_two_line_form_rejected_with_params() {
    // Old 2-line form `pow: Int, Int -> Int` + `pow base exp = ...` is now rejected.
    // The new unified form `pow = Int, Int -> Int: base, exp -> ...` is valid.
    let src = "pow: Int, Int -> Int\npow base exp = base * exp\n";
    let err = parse(src).unwrap_err();
    assert!(
        err.to_string().contains("expected `=`"),
        "expected parse error, got: {}",
        err
    );
}

#[test]
fn d034_old_two_line_form_rejected_nullary() {
    let src = "lenHello: Int\nlenHello = 5\n";
    let err = parse(src).unwrap_err();
    assert!(
        err.to_string().contains("expected `=`"),
        "expected parse error, got: {}",
        err
    );
}

#[test]
fn d034_unified_form_parses_ok() {
    // sig + params
    let p = parse_ok("pow = Int, Int -> Int: base, exp -> base * exp\n");
    let f = match &p.items[0] {
        Item::Func(f) => f,
        _ => panic!("expected func"),
    };
    assert_eq!(f.params, vec!["base", "exp"]);
    assert!(f.signature.is_some());

    // value binding (no sig, no params)
    let p = parse_ok("x = 5\n");
    let f = match &p.items[0] {
        Item::Func(f) => f,
        _ => panic!("expected func"),
    };
    assert!(f.params.is_empty());
    assert!(f.signature.is_none());

    // no sig, with param
    let p = parse_ok("f = x -> x\n");
    let f = match &p.items[0] {
        Item::Func(f) => f,
        _ => panic!("expected func"),
    };
    assert!(f.signature.is_none());
    assert_eq!(f.params, vec!["x"]);
}

#[test]
fn typed_value_binding_parses_ok() {
    // `name = TypeSig: <value>` — a bare value whose type was annotated above.
    // Produces the same AST shape as a nullary function (signature set, no params):
    // §3.1 value/function unity makes the binding's type its body type.
    let p = parse_ok("xs = List Int: Cons 1 (Cons 2 Empty)\n");
    let f = match &p.items[0] {
        Item::Func(f) => f,
        _ => panic!("expected func"),
    };
    assert!(f.params.is_empty());
    assert!(f.signature.is_some());

    // Nullary constructor value.
    let p = parse_ok("empty = List Int: Empty\n");
    let f = match &p.items[0] {
        Item::Func(f) => f,
        _ => panic!("expected func"),
    };
    assert!(f.params.is_empty());
    assert!(f.signature.is_some());

    // A typed value whose body is a variable application (lowercase ident after
    // the colon, but no `->`): must be treated as a value, not params.
    let p = parse_ok("y = List Int: head xs\n");
    let f = match &p.items[0] {
        Item::Func(f) => f,
        _ => panic!("expected func"),
    };
    assert!(f.params.is_empty());
    assert!(f.signature.is_some());

    // A real function with a sig still parses with its params (arrow present).
    let p = parse_ok("f = List Int -> Int: xs -> 0\n");
    let f = match &p.items[0] {
        Item::Func(f) => f,
        _ => panic!("expected func"),
    };
    assert_eq!(f.params, vec!["xs"]);
    assert!(f.signature.is_some());
}

#[test]
fn d034_properties_parse_ok() {
    // Properties are not yet part of the new syntax; verify basic parse works.
    let p = parse_ok("add = Int, Int -> Int: a, b -> a + b\n");
    let f = match &p.items[0] {
        Item::Func(f) => f,
        _ => panic!("expected func"),
    };
    assert!(f.signature.is_some());
    assert_eq!(f.params, vec!["a", "b"]);
}

#[test]
fn d034_properties_without_sig() {
    let p = parse_ok("f = x -> x\n");
    let f = match &p.items[0] {
        Item::Func(f) => f,
        _ => panic!("expected func"),
    };
    assert!(f.signature.is_none());
    assert_eq!(f.params, vec!["x"]);
}
