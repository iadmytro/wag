//! Step 6 (A1b): deferred field re-resolution, Option C
//! (doc/handoff-a1-parametric-inference.md). Field accesses whose receiver was
//! still a hole at inference time are recorded and re-resolved against the
//! concrete receiver once the module's final substitution has settled.

use wag_types::{check_str, Ty, TypedProgram};

fn ok(src: &str) -> TypedProgram {
    check_str(src).unwrap_or_else(|e| panic!("type error: {e}\n{src}"))
}

/// The binding for a unary function `name`, decomposed into `(arg, ret)`.
fn unary(tp: &TypedProgram, name: &str) -> (Ty, Ty) {
    let ty = tp
        .bindings
        .get(&(name.to_string(), 1))
        .unwrap_or_else(|| panic!("`{name}` not in bindings"));
    match ty {
        Ty::Fun(args, ret) if args.len() == 1 => (args[0].clone(), ret.as_ref().clone()),
        other => panic!("`{name}` is not a unary function: {other:?}"),
    }
}

/// The receiver hole of `x.v` is still a Var when the field access is inferred,
/// but a later use in the same body (`takeV x`) binds it to `P`. Re-resolution
/// must succeed and leave `f` fully concrete.
#[test]
fn deferred_field_resolves_in_same_body() {
    let src = "Pt = Pt v Int\ntakeV = Pt -> Int: x -> x.v\nf = x -> x.v + takeV x";
    let tp = ok(src);
    let (arg, ret) = unary(&tp, "f");
    assert_eq!(arg, Ty::Con("Pt".into()), "receiver should be `Pt`: {arg:?}");
    assert_eq!(ret, Ty::int(), "result is Int: {ret:?}");
}

/// The handoff's flagship case: unannotated mutual recursion where each
/// function's parameter hole is only pinned by the other's call site. At
/// baseline this was a closedness violation (free metavars in mono schemes);
/// re-resolution now settles every receiver and result hole.
#[test]
fn mutual_recursion_deferred_fields_resolve() {
    let src = "TB = TB g Int\nTA = TA f TB\nfa = x -> fb (x.f) + 1\nfb = y -> fa (TA (TB (y.g)))\nrun = fa (TA (TB 7))";
    let tp = ok(src);
    // `TA`/`TB` are nullary datatypes (type params must be single uppercase
    // letters), so the receivers are plain constructor types.
    let (arg, ret) = unary(&tp, "fa");
    assert_eq!(arg, Ty::Con("TA".into()), "fa receiver: {arg:?}");
    assert_eq!(ret, Ty::int(), "fa result is Int: {ret:?}");
    let (arg, ret) = unary(&tp, "fb");
    assert_eq!(arg, Ty::Con("TB".into()), "fb receiver: {arg:?}");
    assert_eq!(ret, Ty::int(), "fb result is Int: {ret:?}");
    assert_eq!(tp.bindings[&("run".to_string(), 0)], Ty::int());
}

/// A singleton-SCC unannotated function's parameter hole is generalized at
/// call sites and never settles — re-resolution must flag it instead of
/// silently generalizing `getv` to an unsound `forall a b. a -> b`.
#[test]
fn unresolved_receiver_is_an_error() {
    let src = "Pt = Pt v Int\ngetv = x -> x.v\nrun2 = getv (Pt 3)";
    let err = check_str(src).unwrap_err().to_string();
    assert!(
        err.contains("cannot infer receiver type of field `.v`"),
        "unexpected error: {err}"
    );
}

