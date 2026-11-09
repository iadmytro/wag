//! Regression: nested constructor patterns must not panic on a non-matching arm.
//!
//! `peek_match` used to check only the *outer* ctor id for user ctors and never
//! recursed into the pattern's field sub-patterns. So an arm like
//! `W PushAtom` "peeks" true for ANY `Wrap`, then `destructure` blindly
//! destructures a head that is actually a different ctor (e.g. `Climb`) against
//! `PushAtom`, hitting an arity mismatch and panicking instead of falling through
//! to the next arm. The fix makes `peek_match` a complete structural check: every
//! field must peek-match its sub-pattern before an arm is committed to.

use wag_interp::{prepare, Engine, Value};

fn compile(src: &str) -> wag_interp::LoadedProgram {
    let (_p, _t, loaded) = prepare(src).unwrap_or_else(|e| panic!("prepare failed for {src:?}: {e}"));
    loaded
}

/// `Frame` has a nullary ctor (`PushAtom`) and a 1-field ctor (`Climb Int`).
/// `Wrap` boxes one Frame so arms can carry nested sub-patterns.
const FRAME_SRC: &str = "Frame = PushAtom | Climb Int\n\
                         Wrap = W Frame\n\
                         mkClimb = Int -> Wrap: n -> W (Climb n)\n\
                         mkPush = Int -> Wrap: d -> W PushAtom\n";

#[test]
fn nested_ctor_nonmatch_falls_through() {
    // Head is `Climb`; the arm `W PushAtom` must NOT match and should fall
    // through to the wildcard. Before the fix this panicked with an arity mismatch.
    let src = format!(
        "{FRAME_SRC}\nf = Wrap -> Int: x -> match x\n\
         W PushAtom -> 1\n\
         _ -> 2\n"
    );
    let prog = compile(&src);
    let mut e = Engine::new(&prog);
    let arg = e.call_name("mkClimb", vec![Value::Int(5)]).expect("mkClimb");
    assert_eq!(e.call_name("f", vec![arg]).expect("must fall through"), Value::Int(2));
}

#[test]
fn nested_ctor_match_binds() {
    // Head IS `PushAtom`; the arm matches and binds. Proves a genuine match still
    // works after making peek structural (not just non-matches).
    let src = format!(
        "{FRAME_SRC}\nf = Wrap -> Int: x -> match x\n\
         W PushAtom -> 1\n\
         _ -> 2\n"
    );
    let prog = compile(&src);
    let mut e = Engine::new(&prog);
    let arg = e.call_name("mkPush", vec![Value::Int(0)]).expect("mkPush");
    assert_eq!(e.call_name("f", vec![arg]).expect("must match"), Value::Int(1));
}

#[test]
fn nested_ctor_falls_through_to_later_matching_arm() {
    // The strongest case: the first arm's OUTER ctor (`W`) matches but its inner
    // sub-pattern (`PushAtom`) does not, so control must reach a LATER arm that does
    // match — not just a trailing wildcard. Before the fix this panicked on arm 1.
    let src = format!(
        "{FRAME_SRC}\ng = Wrap -> Int: x -> match x\n\
         W PushAtom -> 10\n\
         W (Climb n) -> n + 100\n\
         _ -> -1\n"
    );
    let prog = compile(&src);
    let mut e = Engine::new(&prog);
    let arg = e.call_name("mkClimb", vec![Value::Int(5)]).expect("mkClimb");
    assert_eq!(e.call_name("g", vec![arg]).expect("must reach arm 2"), Value::Int(105));
}

#[test]
fn nested_ctor_ref_backed_nonmatch_and_match() {
    // `Node` is recursive, so its values are nested owned data (`Value::Data`),
    // exercising the Data path in `peek_ctor`. Covers both a non-matching head (Leaf)
    // and a matching one (Branch).
    let src = "Node = Leaf | Branch Int Node\n\
               leaf = Node: -> Leaf\n\
               branch7 = Node: -> Branch 7 Leaf\n\
               h = Node -> Int: x -> match x\n\
                 Branch n _ -> n\n\
                 _ -> -1\n";
    let prog = compile(src);

    // Head is Leaf → `Branch ..` does not match → wildcard.
    let mut e = Engine::new(&prog);
    let leaf = e.call_name("leaf", vec![]).expect("leaf");
    assert_eq!(e.call_name("h", vec![leaf]).expect("leaf non-match"), Value::Int(-1));

    // Head is Branch 7 → matches, binds n=7.
    let mut e = Engine::new(&prog);
    let branch = e.call_name("branch7", vec![]).expect("branch7");
    assert_eq!(e.call_name("h", vec![branch]).expect("branch match"), Value::Int(7));
}

#[test]
fn nested_ctor_deeply_nested_nonmatch() {
    // Two levels of nesting: `W (Climb 9)` where the head is actually a
    // different ctor. The inner non-match must propagate up and skip the whole arm.
    let src = format!(
        "{FRAME_SRC}\nk = Wrap -> Int: x -> match x\n\
         W (Climb 9) -> 1000\n\
         W (Climb n) -> n\n\
         _ -> -1\n"
    );
    let prog = compile(&src);
    let mut e = Engine::new(&prog);
    // Head is Climb(5): arm 1 (`Climb 9`) literal sub-pattern fails, arm 2 binds n=5.
    let arg = e.call_name("mkClimb", vec![Value::Int(5)]).expect("mkClimb");
    assert_eq!(e.call_name("k", vec![arg]).expect("deep non-match"), Value::Int(5));
}
