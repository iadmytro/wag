//! Closedness validator for the A1b invariant: **no resolution step may emit a
//! type containing unbound placeholder vars.**
//!
//! Always on: both assembly sites in [`super::program`] run it before any
//! type leaves inference, so a violation is a hard `TypeError`, not a warning.
//!
//! Two var classes legitimately remain *free* at inference end and are
//! generalized into schemes: **signature type vars** (explicit `T`, `U`) and
//! **per-call-site instantiation vars** (`instantiate()`). Both are recorded in
//! [`Context::expected_free`] at creation. Every other metavar must be bound by
//! the final substitution — a leftover is a leak. Rules:
//! - Scheme bodies (top-level bindings, methods) and concrete tables: closed
//!   under the final substitution up to expected-free vars. The declared
//!   `vars` list is *not* the criterion — aliasing bindings can postdate
//!   scheme freeze, so a body may legitimately contain an expected-free var
//!   that was never in `vars`; renumbering + per-binding implicit
//!   quantification then makes every exported type closed.
//! - Ctor schemes live in declaration space: their params are `Bound(0..n)`
//!   (A1b 3b), structurally distinct from metavars. They must contain **no**
//!   `Ty::Var` at all, and every `Bound` index must be quantified.
//! - Generalized schemes (bindings, methods): every quantified var must be an
//!   expected-free var.
//! - Concrete-type tables (`method_tys`, the `expr_types` side table, actor
//!   message types): closed under the **final** substitution up to expected-free
//!   vars — applying the final subst may leave only signature/instantiation
//!   vars. Side tables record point-in-time types, so a var that the final
//!   subst binds is fine; one it does not bind and was never expected free is a
//!   leak.

use std::collections::{BTreeMap, BTreeSet};

use wag_syntax::Span;

use crate::types::{CtorInfo, Scheme, Ty};
use crate::unify::{bound_var_indices, free_var_indices, pretty_ty_origin, Subst};

use super::context::Context;

/// One closedness violation with the span that anchors its diagnostic. The
/// span is the offending metavar's origin (where it was created) when known,
/// so the renderer points at the code that introduced the hole.
#[derive(Debug)]
pub(super) struct Violation {
    pub msg: String,
    pub span: Span,
}

impl Violation {
    fn internal(msg: String) -> Self {
        Violation { msg, span: Span::new(0, 0) }
    }
}

/// Human-readable name for a metavar in user-facing messages. Every var gets an
/// origin at creation, so the raw `?N` fallback is only reachable for internal
/// (compiler-bug) cases.
fn origin_desc(subst: &Subst, v: u32) -> String {
    subst
        .origin_of(v)
        .map(|o| o.desc.clone())
        .unwrap_or_else(|| format!("an inferred type (metavar ?{v})"))
}

/// What the final substitution did to a hole: unbound, aliased to another
/// metavar (cross-binding entanglement), or bound to a concrete type. This is
/// the piece that tells the user *why* the hole survived.
fn binding_note(subst: &Subst, v: u32) -> String {
    match subst.get(v) {
        None => "never constrained by any call site".to_string(),
        Some(Ty::Var(u)) => format!(
            "aliased to {}",
            origin_desc(subst, *u)
        ),
        Some(t) => format!("bound to `{}`", pretty_ty_origin(t, subst)),
    }
}

/// Ctor-scheme rule (declaration space, A1b 3b): no live metavars anywhere,
/// and every `Bound` index must appear in the scheme's quantifier list.
/// Ctor field types come from `bind_ctor_scheme` (local Bounds only), so a
/// violation here is a compiler bug, not user error — raw indices are kept for
/// developer debugging.
pub(super) fn check_ctor_scheme(label: &str, info: &CtorInfo) -> Vec<Violation> {
    let mut errs = Vec::new();
    for v in free_var_indices(&info.scheme.body) {
        errs.push(Violation::internal(format!("{label}: internal error (compiler bug): metavar ?{v} leaked into declaration-space scheme — please report")));
    }
    let qv: BTreeSet<u32> = info.scheme.vars.iter().copied().collect();
    for b in bound_var_indices(&info.scheme.body) {
        if !qv.contains(&b) {
            errs.push(Violation::internal(format!("{label}: internal error (compiler bug): param t{b} is not quantified — please report")));
        }
    }
    errs
}

/// Generalization rule: every quantified var must be an expected-free var
/// (signature or instantiation). A body-created metavar that survived to the
/// scheme was silently over-generalized — exactly the shape of the historical
/// A1 leaks.
pub(super) fn check_generalized(
    label: &str,
    scheme: &Scheme,
    expected_free: &BTreeSet<u32>,
    subst: &Subst,
) -> Vec<Violation> {
    // Only vars still live in the exported body matter: re-resolution may bind a
    // generalized var after freezing (harmless — it no longer appears).
    let applied = subst.apply(&scheme.body);
    let live = free_var_indices(&applied);
    scheme
        .vars
        .iter()
        .copied()
        .filter(|v| live.contains(v))
        .filter(|v| !expected_free.contains(v))
        .map(|v| Violation {
            msg: format!(
                "{label}: {} was inferred in the body and cannot be quantified ({}) — inferred type: `{}` — add a signature to the binding",
                origin_desc(subst, v),
                binding_note(subst, v),
                pretty_ty_origin(&applied, subst)
            ),
            span: subst.origin_of(v).map(|o| o.span).unwrap_or_else(|| Span::new(0, 0)),
        })
        .collect()
}

/// Closedness rule: after the final substitution, only expected-free vars may
/// remain.
pub(super) fn check_closed(
    label: &str,
    ty: &Ty,
    subst: &Subst,
    expected_free: &BTreeSet<u32>,
) -> Vec<Violation> {
    let applied = subst.apply(ty);
    free_var_indices(&applied)
        .into_iter()
        .filter(|v| !expected_free.contains(v))
        .map(|v| Violation {
            msg: format!(
                "{label}: cannot infer {} ({}) — recorded type: `{}`",
                origin_desc(subst, v),
                binding_note(subst, v),
                pretty_ty_origin(&applied, subst)
            ),
            span: subst.origin_of(v).map(|o| o.span).unwrap_or_else(|| Span::new(0, 0)),
        })
        .collect()
}

/// Debug dump gated on `WAG_DEBUG_SUBST`: prints exactly what the validator
/// sees (raw schemes with var indices, final subst with origins,
/// expected_free). Off by default; writes to stderr. Exists so closedness
/// violations can be diagnosed from data instead of guessing.
fn debug_dump(
    ctx: &Context,
    raw_bindings: &BTreeMap<(String, usize), Scheme>,
    actor_tys: &BTreeMap<String, Ty>,
) {
    if std::env::var_os("WAG_DEBUG_SUBST").is_none() {
        return;
    }
    /// `?N` plus its origin desc, for compact inline var listing.
    fn tag(subst: &Subst, v: u32) -> String {
        match subst.origin_of(v) {
            Some(o) => format!("?{v}({})", o.desc),
            None => format!("?{v}"),
        }
    }
    fn free_tags(subst: &Subst, ty: &Ty) -> String {
        let vs = free_var_indices(ty);
        let inner = vs
            .iter()
            .map(|v| tag(subst, *v))
            .collect::<Vec<_>>()
            .join(", ");
        format!("[{inner}] ({} vars)", vs.len())
    }
    eprintln!("== WAG_DEBUG_SUBST ==");
    let ef: Vec<u32> = ctx.expected_free.iter().copied().collect();
    eprintln!("expected_free: {ef:?}");
    let n: u32 = ctx.subst.entries().map(|(v, _)| v).max().unwrap_or(0) + 1;
    eprintln!("subst (highest index ~{n}):");
    for (v, t) in ctx.subst.entries() {
        let origin = ctx
            .subst
            .origin_of(v)
            .map(|o| format!(" [origin: {} @ {:?}]", o.desc, o.span))
            .unwrap_or_default();
        eprintln!("  ?{v} := {t:?}{origin}");
    }
    eprintln!("bindings:");
    for ((name, arity), scheme) in raw_bindings {
        eprintln!(
            "  `{name}` (arity {arity}): vars={:?} free_raw={} body={:?}",
            scheme.vars,
            free_tags(&ctx.subst, &scheme.body),
            scheme.body
        );
    }
    if !ctx.methods_table.is_empty() {
        eprintln!("method schemes:");
        for (tname, methods) in &ctx.methods_table {
            for (mname, scheme) in methods {
                eprintln!(
                    "  `{tname}.{mname}`: vars={:?} free_raw={} body={:?}",
                    scheme.vars,
                    free_tags(&ctx.subst, &scheme.body),
                    scheme.body
                );
            }
        }
    }
    if !ctx.method_tys.is_empty() {
        eprintln!("method_tys:");
        for ((tname, mname), ty) in &ctx.method_tys {
            eprintln!("  `{tname}.{mname}`: free={} ty={:?}", free_tags(&ctx.subst, ty), ty);
        }
    }
    if !ctx.expr_types.is_empty() {
        eprintln!("expr_types:");
        for ((item, span), ty) in &ctx.expr_types {
            eprintln!("  `{item}` @ {span:?}: free={} ty={:?}", free_tags(&ctx.subst, ty), ty);
        }
    }
    if !actor_tys.is_empty() {
        eprintln!("actor_tys:");
        for (actor, ty) in actor_tys {
            eprintln!("  `@{actor}`: free={} ty={:?}", free_tags(&ctx.subst, ty), ty);
        }
    }
    eprintln!("== end WAG_DEBUG_SUBST ==");
}

/// Run every check against the inference context at assembly time. Returns one
/// violation per problem (empty = clean).
pub(super) fn validate(
    ctx: &Context,
    raw_bindings: &BTreeMap<(String, usize), Scheme>,
    actor_tys: &BTreeMap<String, Ty>,
) -> Vec<Violation> {
    debug_dump(ctx, raw_bindings, actor_tys);
    let mut errs = Vec::new();
    for ((name, arity), scheme) in raw_bindings {
        let label = format!("binding `{name}` (arity {arity})");
        // Scheme bodies use the same closedness rule as concrete tables:
        // after the final subst only expected-free vars may remain. The
        // declared `vars` list can be stale (aliasing postdates freeze);
        // renumbering + per-binding implicit quantification closes the rest.
        errs.extend(check_closed(&label, &scheme.body, &ctx.subst, &ctx.expected_free));
        errs.extend(check_generalized(&label, scheme, &ctx.expected_free, &ctx.subst));
    }
    for (ctor_name, info) in &ctx.ctors {
        // Declaration space: no live metavars, all Bound indices quantified
        // (A1b 3b).
        errs.extend(check_ctor_scheme(&format!("ctor `{ctor_name}`"), info));
    }
    for (tname, methods) in &ctx.methods_table {
        for (mname, scheme) in methods {
            let label = format!("method `{tname}.{mname}`");
            errs.extend(check_closed(&label, &scheme.body, &ctx.subst, &ctx.expected_free));
            errs.extend(check_generalized(
                &label,
                scheme,
                &ctx.expected_free,
                &ctx.subst,
            ));
        }
    }
    for ((tname, mname), ty) in &ctx.method_tys {
        errs.extend(check_closed(
            &format!("method type `{tname}.{mname}`"),
            ty,
            &ctx.subst,
            &ctx.expected_free,
        ));
    }
    for ((item, span), ty) in &ctx.expr_types {
        errs.extend(check_closed(
            &format!("expr type of `{item}` @ {span:?}"),
            ty,
            &ctx.subst,
            &ctx.expected_free,
        ));
    }
    for (actor, ty) in actor_tys {
        errs.extend(check_closed(
            &format!("actor `@{actor}` message type"),
            ty,
            &ctx.subst,
            &ctx.expected_free,
        ));
    }
    errs
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Red→green proof of the invariant: a scheme body with a live var that is
    /// neither expected-free nor bound by the final subst must be reported.
    #[test]
    fn catches_unexpected_free_var_in_scheme_body() {
        // `a -> ?7` — ?3 is a signature var (expected free), ?7 was created in
        // the body and never resolved: a leak.
        let ty = Ty::Fun(vec![Ty::Var(3)], Box::new(Ty::Var(7)));
        let errs = check_closed("binding `f` (arity 1)", &ty, &Subst::new(), &BTreeSet::from([3u32]));
        assert_eq!(errs.len(), 1);
        assert!(errs[0].msg.contains("?7"), "{errs:?}");
    }

    /// An expected-free var that was never in the (stale) declared `vars` list
    /// is legitimate: aliasing can postdate freeze, and renumbering + implicit
    /// quantification closes it at export.
    #[test]
    fn scheme_body_with_aliased_expected_free_var_passes() {
        // wrapInt shape from the inference regression: body List(?16)->List(?18),
        // final subst aliases both to ?23 (idInt's param, expected free).
        let ty = Ty::Fun(
            vec![Ty::App("List".into(), vec![Ty::Var(16)])],
            Box::new(Ty::App("List".into(), vec![Ty::Var(18)])),
        );
        let mut s = Subst::new();
        s.bind(16, Ty::Var(23));
        s.bind(18, Ty::Var(23));
        assert!(check_closed("binding `wrapInt` (arity 1)", &ty, &s, &BTreeSet::from([16u32, 18, 23])).is_empty());
    }

    /// Red→green for the generalization rule: a body-created metavar that
    /// survived into the scheme's quantifier list is an over-generalized leak.
    #[test]
    fn catches_generalized_body_metavar() {
        // ?9 was created during body inference (not in expected_free) yet ended
        // up quantified — silent over-generalization, the A1 bug class.
        let scheme = Scheme {
            vars: vec![3, 9],
            body: Ty::Fun(vec![Ty::Var(3)], Box::new(Ty::Var(9))),
        constraints: vec![],        };
        let mut ef = BTreeSet::new();
        ef.insert(3); // signature var
        let errs = check_generalized("binding `f` (arity 1)", &scheme, &ef, &Subst::new());
        assert_eq!(errs.len(), 1);
        assert!(errs[0].msg.contains("?9"), "{errs:?}");
    }

    #[test]
    fn signature_and_instantiation_vars_pass_generalization() {
        let scheme = Scheme {
            vars: vec![3, 4],
            body: Ty::Fun(vec![Ty::Var(3)], Box::new(Ty::App("List".into(), vec![Ty::Var(4)]))),
        constraints: vec![],        };
        let ef = BTreeSet::from([3u32, 4]);
        assert!(check_generalized("binding `f` (arity 1)", &scheme, &ef, &Subst::new()).is_empty());
    }

    #[test]
    fn catches_unbound_var_in_concrete_table() {
        // Side-table entry recorded point-in-time; the final subst never bound ?9
        // and ?9 was never expected free.
        let ty = Ty::App(
            "Option".into(),
            vec![Ty::Var(9), Ty::Con("Int".into())],
        );
        let errs = check_closed("expr type of `m` @ (1,2)", &ty, &Subst::new(), &BTreeSet::new());
        assert_eq!(errs.len(), 1);
        assert!(errs[0].msg.contains("?9"), "{errs:?}");
    }

    #[test]
    fn var_bound_by_final_subst_is_closed() {
        // ?5 was resolved later — the recorded copy is stale but closed under
        // the final subst, which is exactly what downstream consumers see.
        let ty = Ty::Fun(vec![Ty::Var(5)], Box::new(Ty::Con("Unit".into())));
        let mut s = Subst::new();
        s.bind(5, Ty::int());
        assert!(check_closed("method type `T.m`", &ty, &s, &BTreeSet::new()).is_empty());
    }

    /// Red→green for declaration space (A1b 3b): a live metavar inside a ctor
    /// scheme is a leak; an unquantified Bound index is malformed.
    #[test]
    fn catches_metavar_in_ctor_scheme() {
        let info = CtorInfo {
            name: "C".into(),
            type_name: "T".into(),
            fields: vec![],
            n_params: 1,
            scheme: Scheme {
                vars: vec![0],
                body: Ty::Fun(vec![Ty::Bound(0)], Box::new(Ty::Var(9))),
            constraints: vec![],            },
        };
        let errs = check_ctor_scheme("ctor `C`", &info);
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errs[0].msg.contains("?9"), "{errs:?}");
    }

    #[test]
    fn catches_unquantified_bound_in_ctor_scheme() {
        let info = CtorInfo {
            name: "C".into(),
            type_name: "T".into(),
            fields: vec![],
            n_params: 1,
            scheme: Scheme {
                vars: vec![0],
                body: Ty::Fun(vec![Ty::Bound(0)], Box::new(Ty::App("U".into(), vec![Ty::Bound(3)]))),
            constraints: vec![],            },
        };
        let errs = check_ctor_scheme("ctor `C`", &info);
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errs[0].msg.contains("t3"), "{errs:?}");
    }

    #[test]
    fn clean_bound_ctor_scheme_passes() {
        let info = CtorInfo {
            name: "Some".into(),
            type_name: "Option".into(),
            fields: vec![],
            n_params: 1,
            scheme: Scheme {
                vars: vec![0],
                body: Ty::Fun(vec![Ty::Bound(0)], Box::new(Ty::option(Ty::Bound(0)))),
            constraints: vec![],            },
        };
        assert!(check_ctor_scheme("ctor `Some`", &info).is_empty());
    }

    #[test]
    fn expected_free_var_survives_final_subst() {
        // ?3 is a signature var: it stays free after the final subst and that is
        // legitimate — generalization quantifies it.
        let ty = Ty::Fun(vec![Ty::Var(3)], Box::new(Ty::Var(3)));
        assert!(check_closed("expr type of `f` @ (1,2)", &ty, &Subst::new(), &BTreeSet::from([3u32])).is_empty());
    }
}
