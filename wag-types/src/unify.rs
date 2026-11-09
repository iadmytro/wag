//! Unification and substitution.

use std::collections::HashMap;

use crate::error::{OriginInfo, Stage, TypeError};
use crate::types::Ty;
use wag_syntax::Span;

#[derive(Debug, Default, Clone)]
pub struct Subst {
    map: Vec<(u32, Ty)>,
    /// Provenance recorded at `fresh()` time: metavar index → what introduced it.
    /// Lives on the `Subst` because that value already flows to every unification
    /// and pretty-print site. It is *not* cleared by per-module resets (only
    /// bindings are), so origins survive multi-module inference — metavar indices
    /// are globally unique, so each index maps to exactly one origin.
    origin: HashMap<u32, OriginInfo>,
    /// Type-constructor variable bindings (D075): live ctor-var index → resolved
    /// type constructor name. Populated during unification when a `CtorApp` is
    /// matched against a concrete `App(name, …)`.
    ctor_map: HashMap<u32, String>,
}

impl Subst {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, v: u32) -> Option<&Ty> {
        self.map.iter().find(|(k, _)| *k == v).map(|(_, t)| t)
    }

    /// All current bindings (insertion order), for the `WAG_DEBUG_SUBST` dump.
    pub(crate) fn entries(&self) -> impl Iterator<Item = (u32, &Ty)> + '_ {
        self.map.iter().map(|(k, t)| (*k, t))
    }

    /// Record what introduced a metavariable (called from `fresh`).
    pub(super) fn record_origin(&mut self, v: u32, origin: OriginInfo) {
        self.origin.insert(v, origin);
    }

    /// Clear all bindings but keep recorded provenance (per-module reset).
    pub(super) fn clear_bindings(&mut self) {
        self.map.clear();
    }

    /// Provenance for a metavariable, if one was recorded.
    pub(super) fn origin_of(&self, v: u32) -> Option<&OriginInfo> {
        self.origin.get(&v)
    }

    pub fn bind(&mut self, v: u32, ty: Ty) {
        // A metavar may be bound exactly once: callers must apply the current
        // subst to both sides first (the production `unify` does), so a Var
        // reaching here is free. Re-binding would silently drop the earlier
        // binding — an invalid state, not a recoverable conflict (A1b).
        debug_assert!(
            self.get(v).is_none(),
            "Subst::bind: metavar ?{v} already bound"
        );
        // Apply existing subst to ty first.
        let ty = self.apply(&ty);
        self.map.push((v, ty));
    }

    pub fn apply(&self, ty: &Ty) -> Ty {
        match ty {
            Ty::Var(v) => {
                if let Some(t) = self.get(*v) {
                    // Follow chain.
                    self.apply(t)
                } else {
                    Ty::Var(*v)
                }
            }
            // Bound vars are opaque to substitution (A1b: never bound).
            Ty::Bound(b) => Ty::Bound(*b),
            Ty::Con(n) => Ty::Con(n.clone()),
            Ty::App(n, args) => Ty::App(n.clone(), args.iter().map(|a| self.apply(a)).collect()),
            Ty::Borrow(t) => Ty::Borrow(Box::new(self.apply(t))),
            Ty::Fun(ps, r) => Ty::Fun(
                ps.iter().map(|p| self.apply(p)).collect(),
                Box::new(self.apply(r)),
            ),
            Ty::CtorApp(v, args) => {
                let applied_args: Vec<Ty> = args.iter().map(|a| self.apply(a)).collect();
                if let Some(name) = self.ctor_map.get(v) {
                    Ty::App(name.clone(), applied_args)
                } else {
                    Ty::CtorApp(*v, applied_args)
                }
            }
        }
    }

    /// Bind a type-constructor variable to a concrete constructor name (D075).
    pub fn bind_ctor(&mut self, v: u32, name: String) {
        self.ctor_map.insert(v, name);
    }

    /// Look up the resolved name for a ctor var, if bound.
    pub fn ctor_of(&self, v: u32) -> Option<&str> {
        self.ctor_map.get(&v).map(|s| s.as_str())
    }
}

pub fn occurs(v: u32, ty: &Ty, subst: &Subst) -> bool {
    match subst.apply(ty) {
        Ty::Var(u) => u == v,
        Ty::Bound(_) => false,
        Ty::Con(_) => false,
        Ty::App(_, args) => args.iter().any(|a| occurs(v, a, subst)),
        Ty::Borrow(t) => occurs(v, &t, subst),
        Ty::Fun(ps, r) => ps.iter().any(|p| occurs(v, p, subst)) || occurs(v, &r, subst),
        Ty::CtorApp(u, args) => u == v || args.iter().any(|a| occurs(v, a, subst)),
    }
}

/// Pretty-print a type for user-facing error messages: resolves through the
/// substitution and replaces remaining free vars with generic letters (a, b, c, …).
pub fn pretty_ty(ty: &Ty, subst: &Subst) -> String {
    let resolved = subst.apply(ty);
    // Collect free vars and assign them sequential letters.
    let mut free_vars: Vec<u32> = Vec::new();
    collect_free_vars(&resolved, &mut free_vars);
    free_vars.sort();
    free_vars.dedup();
    let mut rename: HashMap<u32, String> = HashMap::new();
    for (i, v) in free_vars.iter().enumerate() {
        rename.insert(*v, var_letter(i));
    }
    pretty_ty_inner(&resolved, &rename)
}

/// Pretty-print two types with a *shared* free-var lettering, so distinct
/// metavariables never print as the same letter. Used when comparing a body's
/// inferred type against its declared signature: independent per-type lettering
/// can render two different vars identically (``Option List a`` vs
/// ``Option List a``) and hide the mismatch.
pub fn pretty_ty_pair(a: &Ty, b: &Ty, subst: &Subst) -> (String, String) {
    let ra = subst.apply(a);
    let rb = subst.apply(b);
    let mut free_vars: Vec<u32> = Vec::new();
    collect_free_vars(&ra, &mut free_vars);
    collect_free_vars(&rb, &mut free_vars);
    free_vars.sort();
    free_vars.dedup();
    let mut rename: HashMap<u32, String> = HashMap::new();
    for (i, v) in free_vars.iter().enumerate() {
        rename.insert(*v, var_letter(i));
    }
    (pretty_ty_inner(&ra, &rename), pretty_ty_inner(&rb, &rename))
}

/// Collect the free metavariable indices of an already-substituted type.
pub fn free_var_indices(ty: &Ty) -> Vec<u32> {
    let mut out = Vec::new();
    collect_free_vars(ty, &mut out);
    out.sort();
    out.dedup();
    out
}

/// Collect the bound (quantified / datatype-param) variable indices of a type.
pub fn bound_var_indices(ty: &Ty) -> Vec<u32> {
    let mut out = Vec::new();
    collect_bound_vars(ty, &mut out);
    out.sort();
    out.dedup();
    out
}

/// Rewrite every `Ty::Var` in an already-finalized type to `Ty::Bound` using a
/// dense old→new index map (A1b 3c). Existing `Bound`s pass through — exported
/// tables never mix ctor-scheme params with global vars, so the namespaces do
/// not collide. A var absent from the map is left as-is; callers collect the
/// full set first, so this only happens on a collection bug.
pub fn var_to_bound(ty: &Ty, map: &std::collections::BTreeMap<u32, u32>) -> Ty {
    match ty {
        Ty::Var(v) => Ty::Bound(map.get(v).copied().unwrap_or(*v)),
        Ty::Bound(b) => Ty::Bound(*b),
        Ty::Con(n) => Ty::Con(n.clone()),
        Ty::App(n, args) => {
            Ty::App(n.clone(), args.iter().map(|a| var_to_bound(a, map)).collect())
        }
        Ty::Borrow(t) => Ty::Borrow(Box::new(var_to_bound(t, map))),
        Ty::Fun(ps, r) => Ty::Fun(
            ps.iter().map(|p| var_to_bound(p, map)).collect(),
            Box::new(var_to_bound(r, map)),
        ),
        Ty::CtorApp(v, args) => Ty::CtorApp(
            *v,
            args.iter().map(|a| var_to_bound(a, map)).collect(),
        ),
    }
}

fn collect_bound_vars(ty: &Ty, out: &mut Vec<u32>) {
    match ty {
        Ty::Bound(b) => out.push(*b),
        Ty::Var(_) | Ty::Con(_) => {},
        Ty::App(_, args) => {
            for a in args {
                collect_bound_vars(a, out);
            }
        }
        Ty::Borrow(t) => collect_bound_vars(t, out),
        Ty::Fun(ps, r) => {
            for p in ps {
                collect_bound_vars(p, out);
            }
            collect_bound_vars(r, out);
        }
        Ty::CtorApp(_, args) => {
            for a in args {
                collect_bound_vars(a, out);
            }
        }
    }
}

/// Pretty-print a type for user-facing messages, replacing each free metavariable
/// with the description of what introduced it (falling back to a letter when no
/// origin is recorded). Used where revealing the cause matters more than brevity.
pub fn pretty_ty_origin(ty: &Ty, subst: &Subst) -> String {
    let resolved = subst.apply(ty);
    let free = free_var_indices(&resolved);
    let mut rename: HashMap<u32, String> = HashMap::new();
    for (i, v) in free.iter().enumerate() {
        let _ = match subst.origin_of(*v) {
            Some(o) => rename.insert(*v, format!("`{}`", o.desc)),
            None => rename.insert(*v, var_letter(i)),
        };
    }
    pretty_ty_inner(&resolved, &rename)
}

/// Build a provenance hint explaining the free metavariables shared across
/// several types, assigning letters once over the union so references stay
/// consistent. e.g. "`a` = parameter `x`; `b` = return type of `f`" (or
/// "unknown type `a`" when no origin was recorded). Returns None when there is
/// nothing to explain.
pub fn combined_provenance_hint(tys: &[&Ty], subst: &Subst) -> Option<String> {
    let mut all: Vec<u32> = Vec::new();
    for t in tys {
        collect_free_vars(&subst.apply(t), &mut all);
    }
    all.sort();
    all.dedup();
    if all.is_empty() {
        return None;
    }
    let mut rename: HashMap<u32, String> = HashMap::new();
    for (i, v) in all.iter().enumerate() {
        rename.insert(*v, var_letter(i));
    }
    let mut parts = Vec::new();
    for &v in &all {
        match subst.origin_of(v) {
            Some(o) => parts.push(format!("`{}` = {}", rename[&v], o.desc)),
            None => parts.push(format!("unknown type `{}`", rename[&v])),
        }
    }
    Some(parts.join("; "))
}

/// Provenance hint for a single type (delegates to the combined helper).
pub fn provenance_hint(ty: &Ty, subst: &Subst) -> Option<String> {
    combined_provenance_hint(&[ty], subst)
}

/// Span of the introducing expression/binding for the first free metavariable in
/// `ty` that has a recorded origin. Used to anchor a secondary diagnostic label.
pub fn origin_span(ty: &Ty, subst: &Subst) -> Option<Span> {
    let resolved = subst.apply(ty);
    for v in free_var_indices(&resolved) {
        if let Some(o) = subst.origin_of(v) {
            return Some(o.span);
        }
    }
    None
}

fn collect_free_vars(ty: &Ty, out: &mut Vec<u32>) {
    match ty {
        Ty::Var(v) => out.push(*v),
        // Bound vars are quantified, not free metavariables (A1b).
        Ty::Bound(_) => {},
        Ty::Con(_) => {}
        Ty::App(_, args) => {
            for a in args {
                collect_free_vars(a, out);
            }
        }
        Ty::Borrow(t) => collect_free_vars(t, out),
        Ty::Fun(ps, r) => {
            for p in ps {
                collect_free_vars(p, out);
            }
            collect_free_vars(r, out);
        }
        Ty::CtorApp(v, args) => {
            out.push(*v);
            for a in args {
                collect_free_vars(a, out);
            }
        }
    }
}

fn var_letter(idx: usize) -> String {
    // a, b, c, …, z, a1, b1, …
    if idx < 26 {
        ((b'a' + idx as u8) as char).to_string()
    } else {
        let n = idx - 26;
        format!("{}{}", (b'a' + (n % 26) as u8) as char, n / 26 + 1)
    }
}

fn pretty_ty_inner(ty: &Ty, rename: &std::collections::HashMap<u32, String>) -> String {
    match ty {
        Ty::Var(v) => rename.get(v).cloned().unwrap_or_else(|| format!("?{v}")),
        // Bound vars print as type-parameter letters (t0, t1, …).
        Ty::Bound(i) => format!("t{i}"),
        Ty::Con(n) => n.clone(),
        Ty::App(n, args) => {
            let mut s = n.clone();
            for a in args {
                s.push(' ');
                s.push_str(&pretty_ty_inner(a, rename));
            }
            s
        }
        Ty::Borrow(t) => format!("&{}", pretty_ty_inner(t, rename)),
        Ty::Fun(ps, r) => {
            if ps.is_empty() {
                format!("() -> {}", pretty_ty_inner(r, rename))
            } else {
                let params: Vec<String> = ps
                    .iter()
                    .map(|p| {
                        let s = pretty_ty_inner(p, rename);
                        // Parenthesize nested functions.
                        if matches!(p, Ty::Fun(_, _)) {
                            format!("({s})")
                        } else {
                            s
                        }
                    })
                    .collect();
                format!("{} -> {}", params.join(", "), pretty_ty_inner(r, rename))
            }
        }
        Ty::CtorApp(v, args) => {
            let mut s = rename.get(v).cloned().unwrap_or_else(|| format!("?cv{v}"));
            for a in args {
                s.push(' ');
                s.push_str(&pretty_ty_inner(a, rename));
            }
            s
        }
    }
}

pub fn unify(subst: &mut Subst, a: &Ty, b: &Ty, span: Span) -> Result<(), TypeError> {
    let a = subst.apply(a);
    let b = subst.apply(b);
    match (a, b) {
        (Ty::Var(v), t) | (t, Ty::Var(v)) => {
            if let Ty::Var(u) = &t {
                if *u == v {
                    return Ok(());
                }
            }
            if occurs(v, &t, subst) {
                return Err(TypeError::new(
                    Stage::Inference,
                    span,
                    format!("occurs check failed: {}", pretty_ty_origin(&t, subst)),
                )
                .with_context(provenance_hint(&t, subst).unwrap_or_default())
                .with_origin_span(origin_span(&t, subst)));
            }
            subst.bind(v, t);
            Ok(())
        }
        // Two bound vars unify only when they are the same parameter.
        (Ty::Bound(x), Ty::Bound(y)) if x == y => Ok(()),
        (Ty::Con(x), Ty::Con(y)) if x == y => Ok(()),
        (Ty::App(n1, a1), Ty::App(n2, a2)) if n1 == n2 && a1.len() == a2.len() => {
            for (x, y) in a1.iter().zip(a2.iter()) {
                unify(subst, x, y, span)?;
            }
            Ok(())
        }
        (Ty::Borrow(x), Ty::Borrow(y)) => unify(subst, &x, &y, span),
        (Ty::Fun(p1, r1), Ty::Fun(p2, r2)) if p1.len() == p2.len() => {
            for (x, y) in p1.iter().zip(p2.iter()) {
                unify(subst, x, y, span)?;
            }
            unify(subst, &r1, &r2, span)
        }
        // CtorApp unification (D075): a ctor var applied to args matches
        // either another CtorApp with the same var, or a concrete App/Con.
        (Ty::CtorApp(v, a1), Ty::CtorApp(w, a2)) if v == w && a1.len() == a2.len() => {
            for (x, y) in a1.iter().zip(a2.iter()) {
                unify(subst, x, y, span)?;
            }
            Ok(())
        }
        (Ty::CtorApp(v, args), Ty::App(name, args2)) if args.len() == args2.len() => {
            subst.bind_ctor(v, name.clone());
            for (x, y) in args.iter().zip(args2.iter()) {
                unify(subst, x, y, span)?;
            }
            Ok(())
        }
        (Ty::App(name, args2), Ty::CtorApp(v, args)) if args.len() == args2.len() => {
            subst.bind_ctor(v, name.clone());
            for (x, y) in args.iter().zip(args2.iter()) {
                unify(subst, x, y, span)?;
            }
            Ok(())
        }
        (Ty::CtorApp(v, args), Ty::Con(name)) if args.is_empty() => {
            subst.bind_ctor(v, name.clone());
            Ok(())
        }
        (a, b) => Err(TypeError::new(
            Stage::Inference,
            span,
            format!(
                "type mismatch: expected `{}`, found `{}`",
                pretty_ty(&a, subst),
                pretty_ty(&b, subst)
            ),
        )
        .with_context(combined_provenance_hint(&[&a, &b], subst).unwrap_or_default())
        .with_origin_span(origin_span(&a, subst).or_else(|| origin_span(&b, subst)))),
    }
}

/// Join two `if`/`match` arm types (§3.6). Arms must unify directly —
/// there is no implicit `T` → `Option T` lifting, because the runtime
/// returns exactly what the arm produced and an unwrapped `T` where an
/// `Option T` is expected (e.g. an actor's `Option State` protocol) would
/// be a type-correct program that misbehaves at the boundary. `Option T`
/// arms are written explicitly: `Some v` / `None` (the polymorphic `None`
/// unifies with any `Option T` without lifting).
/// Returns the joined type (after subst).
pub fn unify_arm_join(subst: &mut Subst, a: &Ty, b: &Ty, span: Span) -> Result<Ty, TypeError> {
    let a = subst.apply(a);
    let b = subst.apply(b);
    let mut trial = subst.clone();
    match unify(&mut trial, &a, &b, span) {
        Ok(()) => {
            *subst = trial;
            Ok(subst.apply(&a))
        }
        Err(_) => Err(
            TypeError::new(
                Stage::Inference,
                span,
                format!(
                    "type mismatch in arms: `{}` vs `{}` (no Option lift — write `Some`/`None` explicitly, §3.6)",
                    pretty_ty(&a, subst),
                    pretty_ty(&b, subst)
                ),
            )
            .with_context(combined_provenance_hint(&[&a, &b], subst).unwrap_or_default())
            .with_origin_span(origin_span(&a, subst).or_else(|| origin_span(&b, subst))),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wag_syntax::Span;

    fn sp() -> Span {
        Span::new(0, 0)
    }

    #[test]
    fn var_var_identity() {
        let mut s = Subst::new();
        assert!(unify(&mut s, &Ty::Var(0), &Ty::Var(0), sp()).is_ok());
    }

    #[test]
    fn var_binds_con() {
        let mut s = Subst::new();
        unify(&mut s, &Ty::Var(0), &Ty::int(), sp()).unwrap();
        assert_eq!(s.apply(&Ty::Var(0)), Ty::int());
    }

    #[test]
    fn var_binds_nested_and_applies() {
        let mut s = Subst::new();
        unify(&mut s, &Ty::Var(0), &Ty::list(Ty::Var(1)), sp()).unwrap();
        unify(&mut s, &Ty::Var(1), &Ty::int(), sp()).unwrap();
        // ?0 = List ?1, then ?1 = Int → ?0 applies to List Int
        assert_eq!(s.apply(&Ty::Var(0)), Ty::list(Ty::int()));
    }

    #[test]
    fn con_mismatch_is_inference_error() {
        let mut s = Subst::new();
        let e = unify(&mut s, &Ty::int(), &Ty::bool(), sp()).unwrap_err();
        assert_eq!(e.stage, Stage::Inference);
    }

    #[test]
    fn app_arity_mismatch() {
        let mut s = Subst::new();
        // Pair (2 args) vs Option (1 arg)
        let e = unify(
            &mut s,
            &Ty::pair(Ty::int(), Ty::bool()),
            &Ty::option(Ty::int()),
            sp(),
        )
        .unwrap_err();
        assert_eq!(e.stage, Stage::Inference);
    }

    #[test]
    fn fun_arity_mismatch() {
        let mut s = Subst::new();
        let a = Ty::Fun(vec![Ty::int()], Box::new(Ty::unit()));
        let b = Ty::Fun(vec![Ty::int(), Ty::int()], Box::new(Ty::unit()));
        assert!(unify(&mut s, &a, &b, sp()).is_err());
    }

    #[test]
    fn fun_params_and_result_unify() {
        let mut s = Subst::new();
        let a = Ty::Fun(vec![Ty::int(), Ty::Var(0)], Box::new(Ty::Var(1)));
        let b = Ty::Fun(vec![Ty::Var(2), Ty::bool()], Box::new(Ty::list(Ty::bool())));
        unify(&mut s, &a, &b, sp()).unwrap();
        assert_eq!(s.apply(&Ty::Var(0)), Ty::bool());
        assert_eq!(s.apply(&Ty::Var(1)), Ty::list(Ty::bool()));
        assert_eq!(s.apply(&Ty::Var(2)), Ty::int());
    }

    #[test]
    fn occurs_check_rejects_recursive_binding() {
        let mut s = Subst::new();
        let t = Ty::Fun(vec![Ty::Var(0)], Box::new(Ty::int()));
        let e = unify(&mut s, &Ty::Var(0), &t, sp()).unwrap_err();
        assert!(e.message.contains("occurs"), "got: {}", e.message);
    }

    #[test]
    fn borrow_unifies_through_inner() {
        let mut s = Subst::new();
        let a = Ty::Borrow(Box::new(Ty::Var(0)));
        let b = Ty::Borrow(Box::new(Ty::list(Ty::int())));
        unify(&mut s, &a, &b, sp()).unwrap();
        assert_eq!(s.apply(&Ty::Var(0)), Ty::list(Ty::int()));
    }

    #[test]
    fn borrow_vs_non_borrow_mismatch() {
        let mut s = Subst::new();
        let a = Ty::Borrow(Box::new(Ty::list(Ty::int())));
        let b = Ty::list(Ty::int());
        assert!(unify(&mut s, &a, &b, sp()).is_err());
    }

    #[test]
    fn subst_chain_follows() {
        let mut s = Subst::new();
        s.bind(0, Ty::Var(1));
        s.bind(1, Ty::int());
        assert_eq!(s.apply(&Ty::Var(0)), Ty::int());
    }

    // --- arm joining (§3.6: direct unify only, no Option lift) ---

    #[test]
    fn arm_join_direct() {
        let mut s = Subst::new();
        let t = unify_arm_join(&mut s, &Ty::list(Ty::int()), &Ty::list(Ty::Var(0)), sp()).unwrap();
        assert_eq!(t, Ty::list(Ty::int()));
    }

    #[test]
    fn arm_join_rejects_plain_vs_option_both_directions() {
        let mut s = Subst::new();
        let e = unify_arm_join(&mut s, &Ty::int(), &Ty::option(Ty::int()), sp()).unwrap_err();
        assert_eq!(e.stage, Stage::Inference);

        let mut s = Subst::new();
        let e = unify_arm_join(&mut s, &Ty::option(Ty::int()), &Ty::int(), sp()).unwrap_err();
        assert_eq!(e.stage, Stage::Inference);
    }

    #[test]
    fn arm_join_rejects_plain_vs_option_var() {
        // Con vs Option ?0: direct unify fails (Con vs App); no lifting,
        // so this is a mismatch even though the var could have been bound.
        let mut s = Subst::new();
        let e = unify_arm_join(&mut s, &Ty::int(), &Ty::option(Ty::Var(0)), sp()).unwrap_err();
        assert_eq!(e.stage, Stage::Inference);
    }

    #[test]
    fn arm_join_rejects_incompatible() {
        let mut s = Subst::new();
        let e = unify_arm_join(&mut s, &Ty::int(), &Ty::bool(), sp()).unwrap_err();
        assert_eq!(e.stage, Stage::Inference);
        assert!(e.message.contains("no Option lift"), "got: {}", e.message);
    }

    // --- pretty_ty ---

    #[test]
    fn pretty_resolved_con() {
        let s = Subst::new();
        assert_eq!(pretty_ty(&Ty::int(), &s), "Int");
    }

    #[test]
    fn pretty_unresolved_var_becomes_letter() {
        let s = Subst::new();
        // ?0 is unassigned → should become 'a'
        assert_eq!(pretty_ty(&Ty::Var(0), &s), "a");
    }

    #[test]
    fn pretty_multiple_vars_sequential() {
        let s = Subst::new();
        // Fun(?0, ?1) -> ?0 : should be "a, b -> a"
        let t = Ty::Fun(vec![Ty::Var(0), Ty::Var(1)], Box::new(Ty::Var(0)));
        assert_eq!(pretty_ty(&t, &s), "a, b -> a");
    }

    #[test]
    fn pretty_resolved_through_subst() {
        let mut s = Subst::new();
        s.bind(0, Ty::int());
        // ?0 is assigned to Int → should show "Int", not "a"
        assert_eq!(pretty_ty(&Ty::Var(0), &s), "Int");
    }

    #[test]
    fn pretty_list_with_free_var() {
        let s = Subst::new();
        // List ?0 → "List a"
        assert_eq!(pretty_ty(&Ty::list(Ty::Var(0)), &s), "List a");
    }

    #[test]
    fn pretty_option_with_free_var() {
        let s = Subst::new();
        // Option ?0 → "Option a"
        assert_eq!(pretty_ty(&Ty::option(Ty::Var(5)), &s), "Option a");
    }

    #[test]
    fn pretty_nested_fun() {
        let s = Subst::new();
        // (Int -> a) -> b : nested fun in params should be parenthesized
        let inner = Ty::Fun(vec![Ty::int()], Box::new(Ty::Var(0)));
        let outer = Ty::Fun(vec![inner], Box::new(Ty::Var(1)));
        assert_eq!(pretty_ty(&outer, &s), "(Int -> a) -> b");
    }

    // --- var_to_bound (A1b 3c) ---

    #[test]
    fn var_to_bound_dense_renames() {
        let map: std::collections::BTreeMap<u32, u32> = [(7u32, 0u32), (12, 1)].into();
        let t = Ty::Fun(vec![Ty::Var(7), Ty::list(Ty::Var(12))], Box::new(Ty::Var(7)));
        assert_eq!(
            var_to_bound(&t, &map),
            Ty::Fun(
                vec![Ty::Bound(0), Ty::list(Ty::Bound(1))],
                Box::new(Ty::Bound(0)),
            )
        );
    }

    #[test]
    fn var_to_bound_passes_existing_bound_through() {
        let map: std::collections::BTreeMap<u32, u32> = [(5u32, 0u32)].into();
        let t = Ty::App("T".into(), vec![Ty::Bound(1), Ty::Var(5)]);
        assert_eq!(
            var_to_bound(&t, &map),
            Ty::App("T".into(), vec![Ty::Bound(1), Ty::Bound(0)])
        );
    }

    #[test]
    fn pretty_error_message_uses_letters() {
        // Simulate the error message that users see.
        let mut s = Subst::new();
        let a = Ty::Fun(vec![Ty::Var(41), Ty::Var(42)], Box::new(Ty::Var(42)));
        let b = Ty::int();
        let e = unify(&mut s, &a, &b, sp()).unwrap_err();
        // Should NOT contain "?41" or "?42"
        assert!(!e.message.contains("?41"), "leaked var: {}", e.message);
        assert!(!e.message.contains("?42"), "leaked var: {}", e.message);
        // Should contain letters
        assert!(e.message.contains('a'), "expected letter a: {}", e.message);
    }
}

#[cfg(test)]
mod provenance_tests {
    use super::*;
    use crate::error::OriginInfo;
    use wag_syntax::Span;

    #[test]
    fn hint_reveals_origin_desc() {
        let mut s = Subst::new();
        let v = Ty::Var(7);
        s.record_origin(7, OriginInfo::new("parameter `x`", Span::empty(0)));
        // Free var 7 present -> hint should name it and its origin.
        let h = provenance_hint(&v, &s).unwrap();
        // Known origin -> "`a` = parameter `x`" (no longer prefixed with "unknown type").
        assert!(h.contains("`x`"), "hint was: {h}");
        assert!(h.contains("parameter"), "hint was: {h}");
    }

    #[test]
    fn origin_span_returns_recorded() {
        let mut s = Subst::new();
        let sp = Span::new(3, 5);
        s.record_origin(2, OriginInfo::new("return type of `f`", sp));
        assert_eq!(origin_span(&Ty::Var(2), &s), Some(sp));
        assert_eq!(origin_span(&Ty::int(), &s), None);
    }

    #[test]
    fn mismatch_error_carries_provenance() {
        let mut s = Subst::new();
        s.record_origin(0, OriginInfo::new("parameter `x`", Span::empty(1)));
        // Int vs &<free var 0>: the mismatch arm keeps the var free and attaches
        // provenance (a Var-vs-concrete unify would instead bind and succeed).
        let e = unify(
            &mut s,
            &Ty::int(),
            &Ty::Borrow(Box::new(Ty::Var(0))),
            Span::empty(2),
        )
        .unwrap_err();
        assert!(
            e.context.as_ref().unwrap().contains("`x`"),
            "ctx was: {:?}",
            e.context
        );
        assert_eq!(e.origin_span, Some(Span::empty(1)));
    }
}
