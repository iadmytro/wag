//! Rule 10 at call sites (Phase 1): passing an owned value as a function
//! argument / method receiver is a *consuming* use, so reusing the binding
//! afterwards — or using it twice in one expression — must be rejected with a
//! two-span diagnostic (illegal site + consuming origin). Borrowed (`&T`)
//! parameters and field reads stay non-consuming.

use wag_check::{check_with_prelude, CheckError, Rule};

fn assert_ok(src: &str) {
    check_with_prelude(src).unwrap_or_else(|e| panic!("expected ok, got {e}\n---\n{src}"));
}

/// Assert a rule-10 use-after-move rejection carrying the origin (consuming
/// site) for the two-span diagnostic.
fn assert_moved(label: &str, src: &str) {
    let err = match check_with_prelude(src) {
        Ok(_) => panic!("{label}: expected rule 10 error, got ok\n---\n{src}"),
        Err(e) => e,
    };
    let ce = err.downcast_ref::<CheckError>().unwrap_or_else(|| {
        panic!("{label}: not a CheckError: {err}\n---\n{src}")
    });
    assert_eq!(ce.rule, Rule::Linearity, "{label}: expected rule 10, got {:?}", ce.rule);
    assert!(ce.origin.is_some(), "{label}: expected origin span for two-span diagnostic");
    println!("{label}: rejected as intended: {ce}");
}

/// Double-passing an owned value to a consuming (owned-param) function.
#[test]
fn double_pass_owned_arg_rejected() {
    assert_moved(
        "double_pass",
        "g = List Int -> Int: xs -> xs.len\nf = List Int -> Int: xs -> g xs + g xs\nmain = f listNew\n",
    );
}

/// Pass, then reuse in tail position (product construction).
#[test]
fn pass_then_tail_use_rejected() {
    assert_moved(
        "pass_then_tail",
        "g = List Int -> Int: xs -> xs.len\nf = List Int -> (Int, List Int): xs -> (g xs, xs)\nmain = 1\n",
    );
}

/// Embed in a product first, then pass — the second use is illegal.
#[test]
fn embed_then_pass_rejected() {
    assert_moved(
        "embed_then_pass",
        "g = List Int -> Int: xs -> xs.len\nf = List Int -> (List Int, Int): xs -> (xs, g xs)\nmain = 1\n",
    );
}

/// A `&T` parameter may be used repeatedly (field reads are borrows).
#[test]
fn borrow_param_double_use_accepted() {
    assert_ok("hb = &List Int -> Int: xs -> xs.len + xs.len\nmain = hb listNew\n");
}

/// Move-and-return methods consume the receiver handle; a second call on the
/// same binding is use-after-move.
#[test]
fn method_receiver_double_use_rejected() {
    assert_moved(
        "method_recv",
        "m = Int: ->\n  h0 = listNew\n  a = h0.append 1\n  b = h0.append 2\n  a.len + b.len\nmain = m\n",
    );
}

/// Field access is a borrow: reading the same field twice is fine.
#[test]
fn field_access_double_read_accepted() {
    assert_ok("fr = Int: ->\n  h0 = listNew\n  h0.len + h0.len\nmain = fr\n");
}

/// A bare opaque type parameter is undecidable → conservatively tracked as
/// `Tracked(Owned)` (D-A soundness fix), so double-passing is rejected with a
/// rule-10 two-span diagnostic. Whether undecidable params should be tracked at
/// all remains an open item (#1).
#[test]
fn opaque_param_double_pass_rejected() {
    assert_moved(
        "opaque_param",
        "gU = T -> Int: x -> 1\nfU = T -> Int: x -> gU x + gU x\nmain = 1\n",
    );
}

/// `List T` is decidedly non-PF under every instantiation of `T`, so it is
/// tracked linear even in generic code — double-passing is rejected.
#[test]
fn generic_listh_double_pass_rejected() {
    assert_moved(
        "generic_listh",
        "gL = List T -> Int: xs -> 1\nfL = List T -> Int: xs -> gL xs + gL xs\nmain = 1\n",
    );
}

// ─── D070: sequential condition/guard consumption across branches ──────────
// If-conditions and match `when`-guards run sequentially — reaching arm i's
// body means conditions/guards 0..i all executed. A consuming condition or
// guard therefore removes the binding for every later arm (bodies stay
// exclusive). Pre-D070 each arm was checked from a fresh pre-branch state, so
// cross-arm double use type-checked but crashed at runtime under D-B moves.

/// Consuming if-condition: the binding is gone in the fall-through arm.
#[test]
fn d070_if_condition_consume_rejects_later_arm_use() {
    assert_moved(
        "d070_if_cond",
        "f = List Int -> Bool: l -> True\nconsume = List Int -> Int: l -> 0\ng = List Int -> Int: xs -> if\n  f xs -> 0\n  _    -> consume xs\nmain = g listNew\n",
    );
}

// NOTE: match `when`-guard regression tests are deferred — guards are currently
// parsed but stripped by `fields::desugar_expr` before linearity and never
// evaluated by the VM (dead code end-to-end). They return with the guard
// implementation increment (see doc/wag-next-plan.md, D070 follow-up).

/// Non-consuming condition (read-only method): the binding survives for later arms.
#[test]
fn d070_if_condition_non_consuming_allows_later_arm_use() {
    assert_ok(
        "consume = List Int -> Int: l -> 0\ng = List Int -> Int: xs -> if\n  xs.isEmpty -> 0\n  _          -> consume xs\nmain = g listNew\n",
    );
}

/// The copy idiom keeps the original for a later arm while the condition
/// consumes its argument (fixture 8_4a shape).
#[test]
fn d070_copy_in_condition_keeps_original_for_later_arm() {
    assert_ok(
        "specIsEmpty = List Int -> Bool: lst -> match lst.peek\n  None   -> True\n  Some _ -> False\nsum = List Int -> Int: l -> 1\ng = List Int -> Int: xs -> if\n  specIsEmpty (copy xs) -> 0\n  _                     -> sum xs\nmain = g listNew\n",
    );
}
