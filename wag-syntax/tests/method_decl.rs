//! Method declaration syntax (D056/D057): `TypeName.name = [sig:] params -> body`.
//!
//! Calls need no new syntax — `l.append 42` is an ordinary application of a
//! field expression. Only the declaration form is new here.

use wag_syntax::*;

fn parse_ok(src: &str) -> Program {
    parse(src).unwrap_or_else(|e| panic!("parse failed: {e}\nsource:\n{src}"))
}

fn parse_err(src: &str) {
    assert!(
        parse(src).is_err(),
        "expected parse error, got Ok\nsource:\n{src}"
    );
}

#[test]
fn method_with_sig_and_params() {
    let p = parse_ok(
        r#"
List = LH head Int len Int
List.append = Int -> List: v -> self
"#,
    );
    let methods = p.methods();
    assert_eq!(methods.len(), 1);
    let m = methods[0];
    assert_eq!(m.type_name, "List");
    assert_eq!(m.name, "append");
    assert_eq!(m.dotted_name(), "List.append");
    assert_eq!(m.params, vec!["v".to_string()]);
    match m.signature.as_ref().expect("signature") {
        Type::Func(args, ret) => {
            assert_eq!(args.len(), 1);
            assert_eq!(ret.head_name().as_deref(), Some("List"));
        }
        other => panic!("expected function signature, got {other:?}"),
    }
}

#[test]
fn method_nullary_no_sig() {
    let p = parse_ok(
        r#"
LNode = LEnd | LNodeC val Int next (Ref LNode)
List = LH head (Ref LNode) len Int
List.next = -> None
"#,
    );
    let methods = p.methods();
    assert_eq!(methods.len(), 1);
    assert_eq!(methods[0].name, "next");
    assert!(methods[0].params.is_empty());
    assert!(methods[0].signature.is_none());
}

#[test]
fn method_block_body_with_write_statements() {
    // Write statements (D056/D057): `path <- expr` in block position, bare-atom
    // and dotted LHS. Parsed as RecordUpdate over Self_; the checker rewrites
    // them to in-place region writes. `=` stays an immutable let.
    let p = parse_ok(
        r#"
LNode = LEnd | LNodeC val Int next (Ref LNode)
List = LH head (Ref LNode) tail (Ref LNode) len Int
List.append = Int -> List: v ->
  node = LNodeC (Some v) LEnd
  spliced = match tail
    LEnd ->
      head <- node
      tail <- node
      self
    _    ->
      tail.next <- node
      tail <- node
      self
  len <- len + 1
  spliced
"#,
    );
    assert_eq!(p.methods().len(), 1);
}

#[test]
fn write_statement_ast_shape() {
    // `head <- node` is a statement: Seq(RecordUpdate(Self_, [Leaf(head, node)]), self).
    let p = parse_ok(
        r#"
List = LH head Int len Int
List.bump = Unit -> List: u ->
  len <- len + 1
  self
"#,
    );
    match &p.methods()[0].body {
        Expr::Seq { first, second, .. } => {
            match first.as_ref() {
                Expr::RecordUpdate { receiver, clauses, .. } => {
                    assert!(matches!(receiver.as_ref(), Expr::Self_ { .. }));
                    assert_eq!(clauses.len(), 1);
                    match &clauses[0] {
                        UpdateClause::Leaf { path, value: rhs, .. } => {
                            assert_eq!(path.len(), 1);
                            assert_eq!(path[0], "len");
                            // RHS is `len + 1`
                            assert!(matches!(rhs, Expr::BinOp { .. }));
                        }
                        other => panic!("expected leaf clause, got {other:?}"),
                    }
                },
                other => panic!("expected record update, got {other:?}"),
            }
            // Final expression is `self`.
            assert!(matches!(second.as_ref(), Expr::Self_ { .. }));
        }
        other => panic!("expected seq write, got {other:?}"),
    }
}

#[test]
fn block_may_end_with_write_statement() {
    // A write statement may be the last item of a block: it is then the
    // block's final expression (no trailing value needed).
    let p = parse_ok(
        r#"
List = LH head Int len Int
List.bump = Unit -> List: u ->
  len <- len + 1
"#,
    );
    // Body is the bare RecordUpdate — no Let wrapper, no final expr.
    match &p.methods()[0].body {
        Expr::RecordUpdate { receiver, clauses, .. } => {
            assert!(matches!(receiver.as_ref(), Expr::Self_ { .. }));
            assert_eq!(clauses.len(), 1);
        }
        other => panic!("expected bare record update, got {other:?}"),
    }
}

#[test]
fn left_arrow_rejected_outside_method_bodies() {
    // `<-` as a statement is only valid in method bodies (and do blocks).
    parse_err(
        r#"
f = Int -> Int: x ->
  y <- x + 1
  y
"#,
    );
}

#[test]
fn method_call_is_field_application() {
    // `l.append 42` must parse as Apply(Field(l, "append"), [42]) — no new call form.
    let p = parse_ok(
        r#"
List = LH head Int len Int
main = List -> List: l -> l.append 42
"#,
    );
    match &p.items[1] {
        Item::Func(f) => match &f.body {
            Expr::Apply { fun, args, .. } => {
                match fun.as_ref() {
                    Expr::Field { base, field, .. } => {
                        assert_eq!(field, "append");
                        match base.as_ref() {
                            Expr::Var { name, .. } => assert_eq!(name, "l"),
                            other => panic!("expected var base, got {other:?}"),
                        }
                    }
                    other => panic!("expected field fun, got {other:?}"),
                }
                assert_eq!(args.len(), 1);
            }
            other => panic!("expected application, got {other:?}"),
        },
        other => panic!("expected func item, got {other:?}"),
    }
}

#[test]
fn method_negative_cases() {
    // No `=` after the dotted name.
    parse_err("List.append Int -> List: v -> self");
    // Uppercase "method" name — not a valid ident.
    parse_err("List.Append = Int -> List: v -> self");
    // Dotted name with nothing after the dot.
    parse_err("List. = 5");
    // Bare value body (no sig, no params) is rejected for methods.
    parse_err("List.append = 5");
}

#[test]
fn method_body_rejects_calls_d059() {
    // D059: method bodies are call-free — only constructor applications.
    // User function call in a method body.
    parse_err(
        r#"
f = Int -> Int: x -> x + 1
List = LH head Int len Int
List.append = Int -> List: v -> f v
"#,
    );
    // Method application (Apply over a Field fun) in a method body.
    parse_err(
        r#"
List = LH head Int len Int
List.append = Int -> List: v -> v.bump 1
"#,
    );
    // Call nested under a let binding is still rejected.
    parse_err(
        r#"
f = Int -> Int: x -> x + 1
List = LH head Int len Int
List.append = Int -> List: v ->
  y = f v
  self
"#,
    );
}

#[test]
fn method_body_allows_constructor_apps_and_field_reads_d059() {
    // Constructor applications and bare field reads (incl. nullary-method form)
    // are legal in method bodies.
    let p = parse_ok(
        r#"
LNode = LEnd | LNodeC val Int next (Ref LNode)
List = LH head (Ref LNode) len Int
List.append = Int -> List: v ->
  node = LNodeC v LEnd
  match head
    LEnd -> self
    _    -> self
"#,
    );
    assert_eq!(p.methods().len(), 1);
}

#[test]
fn multiple_methods_same_type() {
    let p = parse_ok(
        r#"
List = LH head Int len Int
List.append = Int -> List: v -> self
List.removeFirst = (Option Int, List): -> (None, self)
List.first = Option Int: -> None
"#,
    );
    let names: Vec<String> = p.methods().iter().map(|m| m.dotted_name()).collect();
    assert_eq!(names, vec!["List.append", "List.removeFirst", "List.first"]);
}
