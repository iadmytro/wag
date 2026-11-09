//! Type alias and parametric type parser tests.

use wag_syntax::{Item, Program, Type};

fn parse_ok(src: &str) -> Program {
    wag_syntax::parse(src).unwrap_or_else(|e| panic!("parse failed: {e}\nsource:\n{src}"))
}

// ── Type aliases ──────────────────────────────────────────────────────────────

#[test]
fn type_alias_function_type() {
    // Function types are TypeAliases (not datatypes)
    let p = parse_ok("IntFn = Int -> Int\nmain = 1\n");
    match &p.items[0] {
        Item::TypeAlias(a) => {
            assert_eq!(a.name, "IntFn");
            assert!(matches!(a.ty, Type::Func(_, _)));
        }
        other => panic!("expected TypeAlias, got {other:?}"),
    }
}

#[test]
fn datatype_single_variant() {
    // `Name = UpperIdent ...` is a datatype (single variant)
    let p = parse_ok("Alias = Int\nmain = 1\n");
    assert_eq!(p.items.len(), 2);
    match &p.items[0] {
        Item::ParametricType(pt) => {
            assert_eq!(pt.name, "Alias");
            assert_eq!(pt.ctors.len(), 1);
            assert_eq!(pt.ctors[0].name, "Int");
        }
        other => panic!("expected ParametricType, got {other:?}"),
    }
}

#[test]
fn datatype_single_variant_with_fields() {
    let p = parse_ok("PairInt = Pair Int Int\nmain = 1\n");
    match &p.items[0] {
        Item::ParametricType(pt) => {
            assert_eq!(pt.name, "PairInt");
            assert_eq!(pt.ctors.len(), 1);
            assert_eq!(pt.ctors[0].name, "Pair");
            assert_eq!(pt.ctors[0].fields.len(), 2);
        }
        other => panic!("expected ParametricType, got {other:?}"),
    }
}

// ── Parametric types ──────────────────────────────────────────────────────────

#[test]
fn parametric_type_single_var() {
    let src = "MyOption = MySome T | MyNone\nmain = 1\n";
    let p = parse_ok(src);
    assert_eq!(p.items.len(), 2);
    match &p.items[0] {
        Item::ParametricType(pt) => {
            assert_eq!(pt.name, "MyOption");
            assert_eq!(pt.type_vars, vec!["T"]);
            assert_eq!(pt.ctors.len(), 2);
            assert_eq!(pt.ctors[0].name, "MySome");
            assert_eq!(pt.ctors[0].fields.len(), 1);
            assert_eq!(pt.ctors[1].name, "MyNone");
            assert!(pt.ctors[1].fields.is_empty());
        }
        other => panic!("expected ParametricType, got {other:?}"),
    }
}

#[test]
fn parametric_type_two_vars() {
    let src = "MyPair = First T U | Second U T\nmain = 1\n";
    let p = parse_ok(src);
    match &p.items[0] {
        Item::ParametricType(pt) => {
            assert_eq!(pt.name, "MyPair");
            assert_eq!(pt.type_vars, vec!["T", "U"]);
            assert_eq!(pt.ctors.len(), 2);
            assert_eq!(pt.ctors[0].name, "First");
            assert_eq!(pt.ctors[0].fields.len(), 2);
            assert_eq!(pt.ctors[1].name, "Second");
            assert_eq!(pt.ctors[1].fields.len(), 2);
        }
        other => panic!("expected ParametricType, got {other:?}"),
    }
}

#[test]
fn parametric_type_multiple_ctors() {
    let src = "Tree = Leaf | Node T (Tree T) (Tree T)\nmain = 1\n";
    let p = parse_ok(src);
    match &p.items[0] {
        Item::ParametricType(pt) => {
            assert_eq!(pt.name, "Tree");
            assert_eq!(pt.type_vars, vec!["T"]);
            assert_eq!(pt.ctors.len(), 2);
            assert_eq!(pt.ctors[0].name, "Leaf");
            assert!(pt.ctors[0].fields.is_empty());
            assert_eq!(pt.ctors[1].name, "Node");
            assert_eq!(pt.ctors[1].fields.len(), 3);
        }
        other => panic!("expected ParametricType, got {other:?}"),
    }
}

// ── Mixed declarations ────────────────────────────────────────────────────────

#[test]
fn mixed_declarations_order() {
    let src = "IntFn = Int -> Int\nMyPair = First T U | Second U T\nmain = 1\n";
    let p = parse_ok(src);
    assert_eq!(p.items.len(), 3);
    assert!(matches!(&p.items[0], Item::TypeAlias(_)));
    assert!(matches!(&p.items[1], Item::ParametricType(_)));
    assert!(matches!(&p.items[2], Item::Func(_)));
}

// ── Pretty print round-trip ───────────────────────────────────────────────────

#[test]
fn pretty_print_type_alias() {
    let p = parse_ok("IntFn = Int -> Int\nmain = 1\n");
    let pretty = wag_syntax::pretty_program(&p);
    assert!(pretty.contains("IntFn"), "pretty: {pretty}");
}

#[test]
fn pretty_print_parametric_type() {
    let p = parse_ok("MyPair = First T U | Second U T\nmain = 1\n");
    let pretty = wag_syntax::pretty_program(&p);
    assert!(pretty.contains("MyPair"), "pretty: {pretty}");
    assert!(pretty.contains("T"), "pretty: {pretty}");
}

// ── Operator overloads (V1 syntax) ────────────────────────────────────────────

#[test]
fn op_def_bare_ident_rejected() {
    // Only parenthesized operators can be overloaded; a bare identifier at the
    // variant column is not an op def (named polymorphism deferred to type classes).
    let src = "MyList = MyEmpty | MyCons T (MyList T)\n  map f h = listMap f h\nmain = 1\n";
    assert!(wag_syntax::parse(src).is_err());
}

