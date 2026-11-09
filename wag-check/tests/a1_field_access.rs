//! A1 soundness (full pipeline): field/method access on the prelude's Ref-based
//! list handles, and match-arm typing against an explicit `Option` signature.
//! Migrated from the temporary `wag-check/tests/a1_repro.rs`, which only
//! printed results — these now assert.

use wag_check::check_with_prelude;

fn assert_ok(src: &str) {
    check_with_prelude(src).unwrap_or_else(|e| panic!("expected ok, got {e}\n---\n{src}"));
}

fn assert_err(label: &str, src: &str) {
    match check_with_prelude(src) {
        Ok(_) => panic!("{label}: expected an error, got ok\n---\n{src}"),
        Err(e) => println!("{label}: rejected as intended: {e}"),
    }
}

/// Mixed arms (`v` vs `None`) against an explicit `Option Int` signature must
/// be REJECTED — no implicit T → Option T lifting (§3.6).
#[test]
fn opt_sig_mixed_arms_rejected() {
    assert_err(
        "opt_sig_mixed",
        "f = Option Int: ->\n  end = LEnd\n  node = LNodeC 42 end\n  h = LH node node node 1\n  match h.head\n    LNodeC v _ -> v\n    LEnd       -> None\n",
    );
}

/// Explicit `Some v` is accepted, and `v` really is `Int`.
#[test]
fn opt_sig_explicit_some_accepted() {
    assert_ok(
        // rule 10: embedding `node` consumes it, so the extra references are
        // pre-copied (copy is a non-consuming read of an auto-borrow).
        "f = Option Int: ->\n  end = LEnd\n  node = LNodeC 42 end\n  n1 = copy node\n  n2 = copy node\n  h = LH node n1 n2 1\n  match h.head\n    LNodeC v _ -> Some v\n    LEnd       -> None\n",
    );
}

/// Field read through the handle (auto-deref, D054).
#[test]
fn field_read_len() {
    assert_ok("main = listNew.len");
}

/// Method call with argument on a Ref-based handle.
#[test]
fn method_call_append() {
    assert_ok("main = listNew.append 1");
}

/// Nullary method returning a product, destructured in match arms.
#[test]
fn method_call_pop_first() {
    assert_ok(
        "main = match listNew.popFirst\n  (None, l) -> l.len\n  (Some v, l) -> l.len\n",
    );
}
