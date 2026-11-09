//! Test-infra builtin: `assert : Bool, String -> Unit`.
//!
//! A passing assertion returns `Ok(Unit)`; a failing one surfaces as
//! `VmError::Panic("assertion failed: <msg>")` so `wagc test` can mark the
//! enclosing test FAIL.

use wag_interp::{prepare, Engine, Value, VmError};

// Nullary functions use the `Name = RetType:  -> body` form (two spaces
// between `:` and `->`, no parameter list).
const SRC: &str = r#"
okAssert = Unit:  -> assert True "fine"
badAssert = Unit:  -> assert False "boom happened"
"#;

fn engine() -> wag_interp::LoadedProgram {
    let (_p, _t, loaded) = prepare(SRC).unwrap_or_else(|e| panic!("prepare failed: {e}"));
    loaded
}

#[test]
fn passing_assert_returns_unit() {
    let prog = engine();
    let mut e = Engine::new(&prog);
    match e.call_name("okAssert", vec![]) {
        Ok(Value::Unit) => {}
        other => panic!("expected Ok(Unit), got {other:?}"),
    }
}

#[test]
fn failing_assert_panics_with_message() {
    let prog = engine();
    let mut e = Engine::new(&prog);
    match e.call_name("badAssert", vec![]) {
        Err(VmError::Panic(msg)) => {
            assert!(msg.contains("boom happened"), "unexpected message: {msg}");
        }
        other => panic!("expected Panic, got {other:?}"),
    }
}
