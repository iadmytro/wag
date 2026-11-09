//! Milestone 3 parser tests: actors, pragmas, record update, desugar template.

use std::fs;
use std::path::{Path, PathBuf};

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

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("fixtures")
}

#[test]
fn m3_fixtures_parse_ok() {
    let dir = fixtures_dir();
    for name in [
        "actors/counter_msg.wag",
        "actors/counter_pragma.wag",
        "actors/counter_supervised.wag",
        "io/main_std.wag",
        "actors/monitor.wag",
        "types/field_access.wag",
    ] {
        let src = fs::read_to_string(dir.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        parse_ok(&src);
    }
}

// `m3_neg_fixtures` removed (D051): neg_clause_shape_mixed.wag and
// neg_bare_with_as_arg.wag exercised the deleted `with`/clause-shape syntax.

#[test]
fn m3_pragma_attaches_to_actor() {
    let src = fs::read_to_string(fixtures_dir().join("actors/counter_pragma.wag")).unwrap();
    let p = parse_ok(&src);
    let actors = p.actors();
    assert_eq!(actors.len(), 1);
    let a = actors[0];
    assert_eq!(a.name, "counter");
    let pr = a.pragma.as_ref().expect("pragma");
    assert_eq!(pr.name, "counter");
    assert_eq!(pr.args.len(), 1);
    assert_eq!(pr.args[0].key, "arena_bytes");
    assert_eq!(pr.args[0].value, 1_048_576);
}

#[test]
fn m3_pragma_without_actor_err() {
    parse_err("@Counter[arena_bytes = 1]\nFoo: Foo\n");
}

#[test]
fn m3_duplicate_actor_equation_err() {
    // In the new unified syntax, a second `@Name` line is a new (duplicate) declaration
    parse_err(
        r#"
M: Unit
@A: Int, M -> Option Int: s m = Some s
@A s m = None
"#,
    );
}

#[test]
fn m3_snapshot_8_14_actors() {
    let src = fs::read_to_string(fixtures_dir().join("actors/counter_supervised.wag")).unwrap();
    let p = parse_ok(&src);
    let names: Vec<_> = p.actors().iter().map(|a| a.name.as_str()).collect();
    assert_eq!(names, vec!["counter", "supervisor"]);
    assert!(p.constructors().iter().any(|c| c.name == "SupervisorState"));
    // SupervisorState has one field: a type application `ChildActor CounterMsg`.
    let st = p
        .constructors()
        .into_iter()
        .find(|c| c.name == "SupervisorState")
        .unwrap();
    assert_eq!(st.fields.len(), 1);
    assert!(matches!(&st.fields[0].ty, Type::App(head, args) if **head == Type::Name("Actor".into()) && *args == vec![Type::Name("CounterMsg".into())]));
}

#[test]
fn m3_snapshot_8_18_actor_fields() {
    let src = fs::read_to_string(fixtures_dir().join("actors/monitor.wag")).unwrap();
    let p = parse_ok(&src);
    assert_eq!(p.actors().len(), 2);
    let reg = p
        .constructors()
        .into_iter()
        .find(|c| c.name == "Register")
        .unwrap();
    // Register has one field: a type application `(Actor LoggerMsg)`.
    assert_eq!(reg.fields.len(), 1);
    assert!(matches!(&reg.fields[0].ty, Type::App(head, args) if **head == Type::Name("Actor".into()) && *args == vec![Type::Name("LoggerMsg".into())]));
}

#[test]
fn m3_snapshot_8_21_field_and_with() {
    let src = fs::read_to_string(fixtures_dir().join("types/field_access.wag")).unwrap();
    let p = parse_ok(&src);
    // Person has 7 labeled fields in new datatype syntax.
    let person = p
        .constructors()
        .into_iter()
        .find(|c| c.name == "Person")
        .unwrap();
    assert_eq!(person.fields.len(), 7);
    let labels: Vec<_> = person.fields.iter().map(|f| f.label.clone().unwrap()).collect();
    assert_eq!(labels, vec!["name", "age", "address", "account", "score", "active", "tags"]);

    let promote = p
        .items
        .iter()
        .find_map(|i| match i {
            Item::Func(f) if f.name == "promote" => Some(f),
            _ => None,
        })
        .expect("promote");
    // D051: multi-field update is now sequential applications in a block,
    // which desugars to a nested `let` chain ending in the final application.
    assert!(matches!(&promote.body, Expr::Bind { .. }), "body={:?}", promote.body);

    let rename = p
        .items
        .iter()
        .find_map(|i| match i {
            Item::Func(f) if f.name == "rename" => Some(f),
            _ => None,
        })
        .expect("rename");
    // D051: single-field update is an application `p.name newName`.
    match &rename.body {
        Expr::Apply { fun, args, .. } => {
            assert!(matches!(
                fun.as_ref(),
                Expr::Field { base, field, .. }
                    if field == "name"
                        && matches!(base.as_ref(), Expr::Var { name, .. } if name == "p")
            ));
            assert_eq!(args.len(), 1);
        }
        other => panic!("expected Apply(Field), got {other:?}"),
    }
}

#[test]
fn m3_desugar_template_two_level() {
    let leaves = vec![ResolvedLeaf {
        steps: vec![
            ResolvedFieldStep {
                constructor: "Person".into(),
                arity: 2,
                field_index: 1,
            },
            ResolvedFieldStep {
                constructor: "Address".into(),
                arity: 2,
                field_index: 1,
            },
        ],
        value: Expr::Var {
            name: "newCity".into(),
            span: Span::new(0, 0),
        },
    }];
    let out = desugar_record_update_template(
        Expr::Var {
            name: "p".into(),
            span: Span::new(0, 0),
        },
        &leaves,
        Span::new(0, 0),
    )
    .unwrap();
    assert!(matches!(out, Expr::Bind { .. }));
}

#[test]
fn m3_label_reuse_across_types_ok() {
    // same label `x` on two different constructors — legal
    parse_ok(
        r#"
Alpha = x Int
Beta = x Int
f = a -> a.x
"#,
    );
}

#[test]
fn m3_self_in_actor_body() {
    let src = fs::read_to_string(fixtures_dir().join("io/main_std.wag")).unwrap();
    let p = parse_ok(&src);
    let main = p.actors().into_iter().find(|a| a.name == "main").unwrap();
    fn contains_self(e: &Expr) -> bool {
        match e {
            Expr::Self_ { .. } => true,
            Expr::Apply { fun, args, .. } => {
                contains_self(fun) || args.iter().any(contains_self)
            }
            Expr::BinOp { lhs, rhs, .. } => contains_self(lhs) || contains_self(rhs),
            Expr::Field { base, .. } => contains_self(base),
            Expr::RecordUpdate {
                receiver, clauses, ..
            } => {
                contains_self(receiver)
                    || clauses.iter().any(|c| match c {
                        UpdateClause::Leaf { value, .. } => contains_self(value),
                        UpdateClause::Group { clauses, .. } => clauses.iter().any(|_| false),
                    })
            }
            Expr::Borrow { inner, .. } => contains_self(inner),
            Expr::Bind { value, body, .. } => contains_self(value) || contains_self(body),
            Expr::Seq { first, second, .. } => contains_self(first) || contains_self(second),
            Expr::If { arms, .. } => arms.iter().any(|a| {
                a.condition.as_ref().map(contains_self).unwrap_or(false) || contains_self(&a.body)
            }),
            Expr::Match { scrutinee, arms, .. } => {
                contains_self(scrutinee) || arms.iter().any(|a| contains_self(&a.body))
            }
            Expr::Do { stmts, .. } => stmts.iter().any(|s| match s {
                DoStmt::Bind { expr, .. } | DoStmt::Return(expr) => contains_self(expr),
            }),
            Expr::ListLit { elems, .. } => elems.iter().any(contains_self),
            Expr::Send { target, msg, .. } => contains_self(target) || contains_self(msg),
            _ => false,
        }
    }
    // After do-desugar, self appears inside nested match/apply.
    assert!(contains_self(&main.body), "expected self in @main body");
}

// ─── D034: Unified actor declaration syntax ───────────────────────────────────

#[test]
fn d034_actor_with_typesig() {
    let src = r#"
MainMsg = Start List (List Int) | Line String | EndOfInput

@main = Option Int, MainMsg -> Option (Option Int): st, msg ->
  s@ = @std
  match msg
    Start argv ->
      s@ (RequestLine self)
      return (Some 0)
    Line chars ->
      s@ (Write chars)
      s@ (RequestLine self)
      return st
    EndOfInput -> None
"#;
let prog = parse_ok(src);
let actors = prog.actors();
let actor = actors.first().unwrap();
assert_eq!(actor.name, "main");
assert!(actor.signature.is_some(), "typeSig should be present");
}

#[test]
fn d034_actor_no_typesig() {
    let src = r#"
MainMsg = Start List (List Int) | Line String | EndOfInput

@main = st, msg ->
  s@ = @std
  match msg
    Start argv ->
      s@ (RequestLine self)
      return (Some 0)
    Line chars -> st
    EndOfInput -> st
"#;
    let prog = parse_ok(src);
    let actors = prog.actors();
    let actor = actors.first().unwrap();
    assert_eq!(actor.name, "main");
    assert!(actor.signature.is_none(), "typeSig should be absent");
}

#[test]
fn d034_qualified_pattern_in_match() {
    let src = r#"
MainMsg = Start List (List Int) | Line String | EndOfInput

@main = st, msg -> match msg
  Start argv -> st
  Line chars -> st
  EndOfInput -> st
"#;
    let prog = parse_ok(src);
    let actors = prog.actors();
    let actor = actors.first().unwrap();
    // Verify the body contains a Match with 3 arms
    if let Expr::Match { arms, .. } = &actor.body {
        assert_eq!(arms.len(), 3);
    } else {
        panic!("expected Match body, got {:?}", actor.body);
    }
}

#[test]
fn handle_suffix_in_ctor_pattern() {
    // §2.2: the `@` handle suffix is valid in pattern position — the ctor
    // pattern's arg loop must accept `IdentAt` (regression: starts_pat_atom
    // omitted it, so `Get replyTo@` failed with "expected `->` in arm").
    let src = r#"
CounterMsg = Increment | Get (Actor ReplyMsg)
ReplyMsg = CountIs Int

@counter = Int, CounterMsg -> Int: state, msg -> match msg
  Increment -> state + 1
  Get replyTo@ ->
    replyTo@ (CountIs (copy state))
    state
"#;
    let prog = parse_ok(src);
    let actors = prog.actors();
    let actor = actors.first().unwrap();
    if let Expr::Match { arms, .. } = &actor.body {
        assert_eq!(arms.len(), 2);
    } else {
        panic!("expected Match body, got {:?}", actor.body);
    }
}

#[test]
fn d034_old_two_line_form_rejected() {
    // The old 2-line form is no longer supported (D034: name appears once)
    parse_err(
        r#"
MainMsg = Start List (List Int)

@main: Option Int, MainMsg -> Option (Option Int)
@main st msg = match msg
  Start argv -> st
"#,
    );
}

#[test]
fn d034_no_colon_form_rejected() {
    // @Name params = body (no colon) is no longer supported
    parse_err(
        r#"
MainMsg = Start List (List Int)

@main st msg = match msg
  Start argv -> st
"#,
    );
}

// ─── Type signature vs property disambiguation ───────────────────────────────

#[test]
fn type_sig_parses_ok() {
    let src = "f = Int -> Int: x -> x + 1\n";
    let p = parse_ok(src);
    let fns: Vec<_> = p.items.iter().filter_map(|i| match i { Item::Func(f) => Some(f), _ => None }).collect();
    assert_eq!(fns.len(), 1);
    assert_eq!(fns[0].name, "f");
    assert!(fns[0].signature.is_some(), "expected a type signature");
}

#[test]
fn type_sig_with_list_parses_ok() {
    let src = "isEmpty = List Int -> Bool: xs -> True\n";
    let p = parse_ok(src);
    let fns: Vec<_> = p.items.iter().filter_map(|i| match i { Item::Func(f) => Some(f), _ => None }).collect();
    assert_eq!(fns.len(), 1);
    assert!(fns[0].signature.is_some(), "expected a type signature");
}

#[test]
fn type_sig_with_type_var_parses_ok() {
    // Single uppercase letter = type variable (new convention).
    let src = "isEmpty = List T -> Bool: xs -> True\n";
    let p = parse_ok(src);
    let fns: Vec<_> = p.items.iter().filter_map(|i| match i { Item::Func(f) => Some(f), _ => None }).collect();
    assert_eq!(fns.len(), 1);
    assert!(fns[0].signature.is_some(), "expected a type signature");
}
