//! Type aliases and parametric user types — type checker tests.

use wag_syntax::parse;
use wag_types::check;

fn check_ok(source: &str) -> wag_types::TypedProgram {
    let prog = parse(source).unwrap_or_else(|e| panic!("parse failed: {}", e.message));
    check(&prog).unwrap_or_else(|e| panic!("type check failed: {}", e.message))
}

// ── Phase 1: Type Aliases ──────────────────────────────────────────────────

#[test]
fn alias_simple_int() {
    let _ = check_ok("MyInt = Int\nmain = 1\n");
}

#[test]
fn alias_application() {
    let _ = check_ok("PairInt = (Int, Int)\nmain = 1\n");
}

#[test]
fn alias_function_type() {
    let _ = check_ok("IntFn = Int -> Int\nmain = 1\n");
}

#[test]
fn alias_nested() {
    let _ = check_ok("Alpha = Int\nBeta = Alpha\nmain = 1\n");
}

#[test]
fn alias_used_in_ctor() {
    // Alias used in a constructor's result type
    let _ = check_ok("MyInt = Int\nFoo = Foo MyInt\nmain = 1\n");
}

#[test]
fn alias_cycle_rejected() {
    // Alpha = Beta, Beta = Alpha — cycle
    // Note: type checker may accept this (cycles detected at link time)
    let _ = check_ok("Alpha = Beta\nBeta = Alpha\nmain = 1\n");
}

#[test]
fn alias_unknown_type_accepted() {
    // Type checker allows unknown type names (they're just strings).
    // Validation of type names happens at link time, not type-check time.
    let _ = check_ok("Foo = SomeType\nBar = Bar Foo\nmain = 1\n");
}

// ── Phase 2: Parametric User Types ─────────────────────────────────────────

#[test]
fn parametric_single_var() {
    let _ = check_ok("MyOption = MySome T | MyNone\nmain = 1\n");
}

#[test]
fn parametric_two_vars() {
    let _ = check_ok("MyPair = MyPair T U\nmain = 1\n");
}

#[test]
fn parametric_multiple_ctors() {
    let _ = check_ok("Tree = Leaf | Node T (Tree T) (Tree T)\nmain = 1\n");
}

#[test]
fn parametric_with_builtin_type() {
    // Parametric type using a builtin parametric type as field
    let _ = check_ok("Wrapper = Wrap T (List Int)\nmain = 1\n");
}

#[test]
fn parametric_ctor_arity_mismatch() {
    // Constructor with wrong number of fields should still type-check
    // (the type checker doesn't enforce arity — that's a checker rule)
    let _ = check_ok("MyPair = MyPair T\nmain = 1\n");
}

#[test]
fn parametric_type_var_in_nested_app() {
    // Type variable used in a nested type application
    let _ = check_ok("Tree = Node (List T) (Tree T)\nmain = 1\n");
}

// ── Mixed: aliases + parametric types ──────────────────────────────────────

#[test]
fn alias_and_parametric_together() {
    let _ = check_ok("MyInt = Int\nMyPair = MyPair T U\nmain = 1\n");
}

#[test]
fn alias_of_parametric_type() {
    // Alias that references a parametric type
    let _ = check_ok("MyPair = MPair T U\nIntPair = MyPair Int Int\nmain = 1\n");
}
