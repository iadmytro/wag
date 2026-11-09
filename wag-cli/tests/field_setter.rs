//! Field mutation by application (§3.3.1 / D051).
//!
//! `a.f v` is the 1-arg setter overload: consumes `a`, writes field `f`,
//! returns the updated structure. `a.f` (0 args) is the getter: borrows,
//! returns the field value.

use wag_cli::compile_and_load;
use wag_interp::{Engine, Value};

fn compile(src: &str) -> wag_interp::LoadedProgram {
    let compiled = compile_and_load(src).unwrap_or_else(|e| panic!("compile failed for {src:?}: {e}"));
    compiled.loaded().clone()
}

fn call(e: &mut Engine<'_>, name: &str, args: Vec<Value>) -> Value {
    e.call_name(name, args).expect("call failed")
}

#[test]
fn setter_returns_updated_struct() {
    // Person has two Int fields. `p.x v` sets field x to v and returns the struct.
    let src = "Person = Person x Int y Int\nmakeP = Int, Int -> Person: a, b -> Person a b\nsetX = Person, Int -> Person: p, v ->\n  p1 = p.x v\n  p1\ngetX = Person -> Int: p ->\n  p.x\n";
    let prog = compile(src);
    let mut e = Engine::new(&prog);
    // makeP 10 20 → Person { x=10, y=20 }
    let p = call(&mut e, "makeP", vec![Value::Int(10), Value::Int(20)]);
    // setX p 99 → Person { x=99, y=20 }
    let p1 = call(&mut e, "setX", vec![p, Value::Int(99)]);
    // getX p1 → 99
    assert_eq!(call(&mut e, "getX", vec![p1]), Value::Int(99));
}

#[test]
fn getter_borrows_and_returns_field() {
    let src = "Person = Person x Int y Int\nmakeP = Int, Int -> Person: a, b -> Person a b\ngetX = Person -> Int: p ->\n  p.x\ngetY = Person -> Int: p ->\n  p.y\n";
    let prog = compile(src);
    let mut e = Engine::new(&prog);
    let p1 = call(&mut e, "makeP", vec![Value::Int(10), Value::Int(20)]);
    let p2 = call(&mut e, "makeP", vec![Value::Int(10), Value::Int(20)]);
    assert_eq!(call(&mut e, "getX", vec![p1]), Value::Int(10));
    assert_eq!(call(&mut e, "getY", vec![p2]), Value::Int(20));
}

#[test]
fn sequential_setters_chain() {
    let src = "Rec = Rec a Int b Int c Int\nmakeR = Int, Int, Int -> Rec: x, y, z -> Rec x y z\nupdateAll = Rec, Int, Int, Int -> Rec: p, a, b, c ->\n  p1 = p.a a\n  p2 = p1.b b\n  p3 = p2.c c\n  p3\ngetC = Rec -> Int: p ->\n  p.c\n";
    let prog = compile(src);
    let mut e = Engine::new(&prog);
    let p = call(&mut e, "makeR", vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
    let p3 = call(&mut e, "updateAll", vec![
        p,
        Value::Int(10),
        Value::Int(20),
        Value::Int(30),
    ]);
    assert_eq!(call(&mut e, "getC", vec![p3]), Value::Int(30));
}

#[test]
fn setter_preserves_other_fields() {
    let src = "Rec = Rec a Int b Int c Int\nmakeR = Int, Int, Int -> Rec: x, y, z -> Rec x y z\nsetB = Rec, Int -> Rec: p, v ->\n  p1 = p.b v\n  p1\ngetA = Rec -> Int: p ->\n  p.a\ngetC = Rec -> Int: p ->\n  p.c\n";
    let prog = compile(src);
    let mut e = Engine::new(&prog);
    let p = call(&mut e, "makeR", vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
    let p_dup = call(&mut e, "makeR", vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
    let p1 = call(&mut e, "setB", vec![p, Value::Int(99)]);
    let p1_dup = call(&mut e, "setB", vec![p_dup, Value::Int(99)]);
    // a and c should be unchanged
    assert_eq!(call(&mut e, "getA", vec![p1]), Value::Int(1));
    assert_eq!(call(&mut e, "getC", vec![p1_dup]), Value::Int(3));
}
