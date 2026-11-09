//! A1 soundness: parametric type flow-through for field access and methods on
//! generic datatypes (doc/handoff-a1-parametric-inference.md). Migrated from the
//! temporary `wag-check/tests/a1_repro.rs` with structural assertions — each
//! case pins down *which* quantified var flows where, not just "no error".

use wag_types::{check, Ty, TypedProgram};

fn infer(src: &str) -> TypedProgram {
    let mut p = wag_syntax::parse(src).unwrap_or_else(|e| panic!("parse failed: {e}\n{src}"));
    // D060: early desugar of method-body bare names (pipeline stage).
    wag_syntax::desugar_method_bare_names(&mut p);
    check(&p).unwrap_or_else(|e| panic!("type error: {}\n{src}", e.message))
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

/// `Some(k)` when `ty` is exactly the quantified var `t{k}`. A1b 3c: exported
/// tables carry no live metavars — signature vars are dense `Bound` indices.
fn bound_of(ty: &Ty) -> Option<u32> {
    match ty {
        Ty::Bound(k) => Some(*k),
        _ => None,
    }
}

/// A1b closedness: no `Ty::Var` anywhere in `ty`.
fn assert_closed(label: &str, ty: &Ty) {
    fn walk(label: &str, ty: &Ty, path: &mut String) {
        match ty {
            Ty::Var(v) => panic!("{label}: unbound metavar ?{v} at {path:?}"),
            Ty::Con(_) => {}
            Ty::App(_, args) => {
                for (i, a) in args.iter().enumerate() {
                    let l = path.len();
                    path.push_str(&format!("[{i}]"));
                    walk(label, a, path);
                    path.truncate(l);
                }
            }
            Ty::Borrow(inner) => {
                path.push('&');
                walk(label, inner, path);
                path.pop();
            }
            Ty::Fun(args, ret) => {
                for (i, a) in args.iter().enumerate() {
                    let l = path.len();
                    path.push_str(&format!("[{i}]"));
                    walk(label, a, path);
                    path.truncate(l);
                }
                path.push_str("->");
                walk(label, ret, path);
            }
            // Bound vars are quantified — closed by definition (A1b).
            Ty::Bound(_) => {}
            // Ctor vars should be resolved to App nodes after full inference.
            Ty::CtorApp(v, _) => panic!("{label}: unresolved ctor var ?cv{v} at {path:?}"),
        }
    }
    walk(label, ty, &mut String::new());
}

/// Field access on a parametric type: the field's type var must be the *same*
/// quantified var as the receiver's — `f : forall T. TA T -> T`. This is the
/// core A1 property (leaking a fresh var here was the historical bug).
#[test]
fn ctor_param_flows_through() {
    let tp = infer("TA = C c T\nf = TA T -> T: x -> x.c");
    let (arg, ret) = unary(&tp, "f");
    match &arg {
        Ty::App(n, args) => {
            assert_eq!(n, "TA", "receiver should be `TA …`: {arg:?}");
            let k = bound_of(&args[0]).unwrap_or_else(|| panic!("TA's arg is not a quantified var: {arg:?}"));
            assert_eq!(bound_of(&ret), Some(k), "result must reuse t{k}: {ret:?}");
        }
        other => panic!("receiver should be `TA …`, got {other:?}"),
    }
}

/// Method call on a parametric type: inference converges on the declared shape
/// — `f : forall T. TA2 T -> Int`, with the receiver's quantified var
/// instantiated into the method's self-param (no free vars introduced by the
/// dispatch).
#[test]
fn method_param_flows_through() {
    let tp = infer("TA2 = C c T n Int\nTA2.m = Int: -> n\nf = TA2 T -> Int: x -> x.m");
    let (arg, ret) = unary(&tp, "f");
    match &arg {
        Ty::App(n, args) => {
            assert_eq!(n, "TA2", "receiver should be `TA2 …`: {arg:?}");
            bound_of(&args[0]).unwrap_or_else(|| panic!("TA2's arg is not a quantified var: {arg:?}"));
        }
        other => panic!("receiver should be `TA2 …`, got {other:?}"),
    }
    assert!(matches!(&ret, Ty::Con(n) if n == "Int"), "result is Int: {ret:?}");
}

/// Multi-step field path through non-parametric types: fully closed concrete
/// type — `f : Outer -> Int`.
#[test]
fn nested_field_is_closed() {
    let tp = infer("Inner = I v Int\nOuter = O i Inner\nf = Outer -> Int: x -> x.i.v");
    let (arg, ret) = unary(&tp, "f");
    assert!(matches!(&arg, Ty::Con(n) if n == "Outer"), "receiver is `Outer`: {arg:?}");
    assert!(matches!(&ret, Ty::Con(n) if n == "Int"), "result is `Int`: {ret:?}");
    assert_closed("f", &Ty::Fun(vec![arg], Box::new(ret)));
}

/// Matching through a `Ref`-typed field resolves the referent's exact declared
/// type via the ctor scheme (D054) — arms pattern-match on `Flag`, so a leak
/// (field left as an unbound metavar) would fail arm unification. The binding
/// is fully closed: `f : Pt -> Bool`.
#[test]
fn ref_field_match_is_closed() {
    let tp = infer(
        "Flag = FOn | FOff\nPt = Pt r (Ref Flag)\nf = Pt -> Bool: x -> match x.r\n  FOn  -> True\n  FOff -> False",
    );
    let (arg, ret) = unary(&tp, "f");
    assert!(matches!(&arg, Ty::Con(n) if n == "Pt"), "receiver is `Pt`: {arg:?}");
    assert!(matches!(&ret, Ty::Con(n) if n == "Bool"), "result is `Bool`: {ret:?}");
    assert_closed("f", &Ty::Fun(vec![arg], Box::new(ret)));
}
