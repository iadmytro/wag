//! N-ary product (tuple) execution tests — §3.6 / syntax unification.
//!
//! Products are fixed-arity value types (Rust-style): arity 2/3 use the inline
//! `Pair`/`Triple`, arity 4–7 use a heap-backed `Product`. Arity > 7 is an
//! *implementation limit* (not a language-design choice) and is rejected.
//!
//! These exercise the full pipeline (parse → type → check → load → run):
//! constructing N-ary product values, destructuring them in `match`, threading
//! them through function arguments, and the arity cap.

use wag_interp::{prepare, Engine, Value, CTOR_PAIR2, CTOR_PRODUCT};

/// Prepare a program from inline source; panic on any pipeline failure.
fn compile(src: &str) -> wag_interp::LoadedProgram {
    let (_p, _t, loaded) = prepare(src).unwrap_or_else(|e| panic!("prepare failed for {src:?}: {e}"));
    loaded
}

/// Call a top-level function and unwrap, panicking on VmError.
fn call(prog: &wag_interp::LoadedProgram, name: &str, args: Vec<Value>) -> Value {
    let mut e = Engine::new(prog);
    e.call_name(name, args).expect("call failed")
}

#[test]
fn seven_tuple_argument_destructure() {
    // Takes a single 7-tuple parameter; destructures it in a match arm and
    // sums the elements. Proves product types in signatures, N-ary pattern
    // binding, and heap-backed `Value::Product` matching at runtime.
    let src = "sum7 = (Int, Int, Int, Int, Int, Int, Int) -> Int: p -> match p\n\
               (a, b, c, d, e, f, g) -> a + b + c + d + e + f + g\n";
    let prog = compile(src);
    let mut e = Engine::new(&prog);
    let fields: Vec<Value> = [1, 2, 3, 4, 5, 6, 7].iter().map(|&n| Value::Int(n)).collect();
    assert_eq!(e.call_name("sum7", vec![Value::Data(CTOR_PRODUCT, fields)]).unwrap(), Value::Int(28));
}

#[test]
fn seven_tuple_literal_in_body() {
    // Construct a 7-tuple literal in the body (exercises `Expr::Product`
    // construction → heap-backed value), then destructure it. Takes one
    // extra param (wag signatures require >=1 parameter) added to the sum.
    let src = "sumLit = Int -> Int: extra -> match (10, 20, 30, 40, 50, 60, 70)\n\
               (a, b, c, d, e, f, g) -> a + b + c + d + e + f + g + extra\n";
    let prog = compile(src);
    assert_eq!(call(&prog, "sumLit", vec![Value::Int(0)]), Value::Int(280));
}

#[test]
fn seven_tuple_nested_destructure() {
    // Nested products: a 7-tuple whose third element is itself a pair. Proves
    // recursion through `Value::Product` elements during matching.
    let src = "extract = (Int, Int, (Int, Int), Int, Int, Int, Int) -> Int: p -> match p\n\
               (a, b, (c, d), e, f, g, h) -> a + b + c * d + e + f + g + h\n";
    let prog = compile(src);
    let mut e = Engine::new(&prog);
    let inner = Value::Data(CTOR_PAIR2, vec![Value::Int(3), Value::Int(4)]); // c=3, d=4 → c*d=12
    let outer_fields = vec![
        Value::Int(1), Value::Int(2), inner, Value::Int(5), Value::Int(6), Value::Int(7), Value::Int(8),
    ];
    let outer = Value::Data(CTOR_PRODUCT, outer_fields);
    // 1 + 2 + 12 + 5 + 6 + 7 + 8 = 41
    assert_eq!(e.call_name("extract", vec![outer]).unwrap(), Value::Int(41));
}

#[test]
fn seven_tuple_threaded_through_function() {
    // A function that returns a 7-tuple; another consumes it. Proves products
    // survive as values across calls (deep-copy / message paths). `make7` takes
    // one seed param placed as the first tuple element.
    let src = "make7 = Int -> (Int, Int, Int, Int, Int, Int, Int): seed -> (seed, 2, 3, 4, 5, 6, 7)\n\
               sum7 = (Int, Int, Int, Int, Int, Int, Int) -> Int: p -> match p\n\
               (a, b, c, d, e, f, g) -> a + b + c + d + e + f + g\n";
    let prog = compile(src);
    let mut e = Engine::new(&prog);
    // sum7(make7(1)) = 1+2+3+4+5+6+7 = 28
    let made = e.call_name("make7", vec![Value::Int(1)]).unwrap();
    assert_eq!(e.call_name("sum7", vec![made]).unwrap(), Value::Int(28));
}

#[test]
fn eight_tuple_pattern_rejected() {
    // Arity > 7 is an implementation limit: the parser rejects a 8-tuple
    // pattern with a clear error (not a silent truncation or panic).
    let src = "bad = (Int, Int, Int, Int, Int, Int, Int, Int) -> Int: p -> match p\n\
               (a, b, c, d, e, f, g, h) -> 0\n";
    let err = prepare(src).expect_err("8-tuple pattern must be rejected as an implementation limit");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("7") || msg.contains("limit"),
        "expected an arity-limit error, got: {msg}"
    );
}

#[test]
fn eight_tuple_value_rejected() {
    // The runtime also refuses to build an 8-tuple value (cap enforced in the
    // expression-eval path), rather than truncating or mis-shaping it.
    let src = "mk = n -> (Int, Int, Int, Int, Int, Int, Int, Int): n -> (1, 2, 3, 4, 5, 6, 7, 8)\n";
    let prog = compile(src);
    let mut e = Engine::new(&prog);
    // Pass mk's argument so the body (the 8-tuple construction) is evaluated.
    let err = e.call_name("mk", vec![Value::Int(0)]).expect_err("8-tuple value must be rejected");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("7") || msg.contains("limit"),
        "expected an arity-limit panic, got: {msg}"
    );
}
