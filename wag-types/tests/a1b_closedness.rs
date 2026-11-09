//! A1b closedness invariant — permanent regression tests.
//!
//! The inference pipeline must never emit a type containing an unbound
//! placeholder metavar: every live metavar is either bound by the final
//! substitution or belongs to one of the expected-free classes (signature
//! vars, instantiation vars, unannotated parameter holes, call-result holes,
//! Option-C deferral holes) and gets generalized into a scheme. The validator
//! (`wag_types::infer::validate`) enforces this; these tests keep it honest on
//! real programs.
//!
//! A1b 3c: at assembly, every surviving free var in the exported tables is
//! renumbered to a dense `Ty::Bound` index — so **no `Ty::Var` may appear in
//! any exported type**, and generalized (expected-free) vars show up as
//! `Bound`.

use wag_types::{check_str, Ty};

fn prelude() -> String {
    std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../prelude/prelude.wag"
    ))
    .expect("prelude.wag readable")
}

/// Count `Ty::Var` occurrences in a type. A1b 3c: must be zero in exported
/// tables — any hit is an internal error.
fn count_vars(ty: &Ty) -> usize {
    match ty {
        Ty::Var(_) => 1,
        // Bound vars are quantified, not unbound metavars (A1b).
        Ty::Bound(_) => 0,
        Ty::Con(_) => 0,
        Ty::App(_, args) => args.iter().map(count_vars).sum(),
        Ty::Borrow(t) => count_vars(t),
        Ty::Fun(ps, r) => ps.iter().map(count_vars).sum::<usize>() + count_vars(r),
        // Ctor vars are resolved during unification; not regular metavars.
        Ty::CtorApp(..) => 0,
    }
}

/// Count `Ty::Bound` (quantified / expected-free) occurrences in a type.
fn count_bound(ty: &Ty) -> usize {
    match ty {
        Ty::Var(_) => 0,
        Ty::Bound(_) => 1,
        Ty::Con(_) => 0,
        Ty::App(_, args) => args.iter().map(count_bound).sum(),
        Ty::Borrow(t) => count_bound(t),
        Ty::Fun(ps, r) => ps.iter().map(count_bound).sum::<usize>() + count_bound(r),
        Ty::CtorApp(..) => 0,
    }
}

/// The prelude is the largest real program in the repo and exercises every
/// expected-free class (polymorphic list functions, unannotated helpers,
/// methods, actors). Run it with `WAG_DEBUG=1` so the closedness validator is
/// active: any leak surfaces as a `closedness violation(s)` type error.
///
/// This test binary is dedicated to these checks, so setting the process env
/// var here does not interfere with other suites (each integration test file
/// is its own process).
#[test]
fn prelude_validates_clean_under_wag_debug() {
    std::env::set_var("WAG_DEBUG", "1");
    match check_str(&prelude()) {
        Ok(_) => {}
        Err(e) => panic!("prelude failed closedness validation: {e}"),
    }
}

/// Unannotated self-recursive helper in a singleton SCC: its parameter and
/// call-result holes stay free and are generalized (§3.5 Option A). The
/// exported binding must therefore contain quantified (dense `Bound`) vars —
/// the historical A1 bug was that such leaks went unnoticed downstream.
#[test]
fn unannotated_recursive_helper_generalizes() {
    let tp = check_str(
        "Tree = Leaf | Node T (Tree T)\n\
         mapHelper = f, xs, acc -> match xs\n\
         \x20 Leaf -> acc\n\
         \x20 Node h t -> mapHelper f t (Node (f h) acc)\n",
    )
    .expect("unannotated self-recursion typechecks");
    let ty = &tp.bindings[&("mapHelper".to_string(), 3)];
    assert_eq!(count_vars(ty), 0, "no live metavars in exported tables: {ty}");
    assert!(
        count_bound(ty) >= 2,
        "expected generalized (bound-var) binding for mapHelper, got {ty}"
    );
}

/// Annotated recursive helper: the signature var is generalized and survives
/// in the exported binding as a dense `Bound` index — legitimate, not a leak.
#[test]
fn annotated_recursive_helper_keeps_signature_var() {
    let tp = check_str(
        "Tree = Leaf | Node T (Tree T)\n\
         mapHelper = Tree T, Tree T -> Tree T: xs, acc -> match xs\n\
         \x20 Leaf -> acc\n\
         \x20 Node h t -> mapHelper t (Node h acc)\n",
    )
    .expect("annotated self-recursion typechecks");
    let ty = &tp.bindings[&("mapHelper".to_string(), 2)];
    assert_eq!(count_vars(ty), 0, "no live metavars in exported tables: {ty}");
    assert!(
        count_bound(ty) == 3,
        "expected one signature var in three positions, got {ty}"
    );
}
