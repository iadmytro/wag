//! `Ref T` visibility boundary (D056) and method/field name disjointness (D057).
//!
//! Links are non-values confined to declaration field positions and method bodies:
//! `Ref U` may never appear in top-level function, actor, or method signatures. Under
//! D057 this is purely syntactic — methods cannot recurse or call each other on cycles,
//! so no call-graph analysis is needed (spec §6.9).

use std::collections::{BTreeMap, BTreeSet};
use wag_syntax::{Item, Type};

use crate::error::{Stage, TypeError};
use crate::types::{Scheme, Ty};

/// Reject `Ref U` anywhere in an explicit top-level function / actor / method signature.
pub fn check_signatures(items: &[Item]) -> Result<(), TypeError> {
    for item in items {
        let (sig, span, what, who) = match item {
            Item::Func(f) => (&f.signature, f.span, "top-level function", f.name.clone()),
            Item::Actor(a) => (
                &a.signature,
                a.span,
                "actor message signature",
                format!("@{}", a.name),
            ),
            Item::Method(m) => (
                &m.signature,
                m.span,
                "method signature",
                format!("{}.{}", m.type_name, m.name),
            ),
            _ => continue,
        };
        if let Some(ty) = sig {
            if contains_ref(ty) {
                return Err(TypeError::new(
                    Stage::Boundary,
                    span,
                    format!(
                        "`Ref T` may not appear in the signature of {what} `{who}` (`{sig}`) — \
                         links are confined to declaration field positions and method bodies (D056)",
                        sig = wag_syntax::pretty_type(ty)
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// Reject inferred schemes containing `Ref U` (post-inference): the boundary holds for
/// implicit signatures too. Builtin/predecl schemes never contain `Ref`, so any hit is
/// user code; entries with no matching item are skipped defensively.
pub fn check_schemes(
    bindings: &BTreeMap<(String, usize), Scheme>,
    items: &[Item],
) -> Result<(), TypeError> {
    for ((name, _arity), scheme) in bindings {
        if !ty_contains_ref(&scheme.body) {
            continue;
        }
        let bare = name.strip_prefix('@').unwrap_or(name.as_str());
        let span = items.iter().find_map(|i| match i {
            Item::Func(f) if f.name == bare => Some(f.span),
            Item::Actor(a) if a.name == bare => Some(a.span),
            _ => None,
        });
        let Some(span) = span else { continue };
        return Err(TypeError::new(
            Stage::Boundary,
            span,
            format!(
                "`{name}` has an inferred signature containing `Ref T` (`{ty}`) — links \
                 may not cross call boundaries (D056)",
                ty = crate::pretty_ty(&scheme.body, &crate::unify::Subst::new())
            ),
        ));
    }
    Ok(())
}

/// A method name must not collide with a field label of its receiver type (D056/D057):
/// `l.x` disambiguates by declaration, so the two namespaces are disjoint per type.
pub fn check_method_field_disjointness(items: &[Item]) -> Result<(), TypeError> {
    // receiver type → field labels declared in this module.
    let mut fields: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for item in items {
        match item {
            Item::Constructor(c) => {
                if let Some(tn) = c.result_type.head_name() {
                    let set = fields.entry(tn).or_default();
                    for f in &c.fields {
                        if let Some(label) = &f.label {
                            set.insert(label.clone());
                        }
                    }
                }
            }
            Item::ParametricType(p) => {
                let set = fields.entry(p.name.clone()).or_default();
                for c in &p.ctors {
                    for f in &c.fields {
                        if let Some(label) = &f.label {
                            set.insert(label.clone());
                        }
                    }
                }
            }
            _ => {}
        }
    }

    for item in items {
        if let Item::Method(m) = item {
            if fields
                .get(&m.type_name)
                .is_some_and(|s| s.contains(&m.name))
            {
                return Err(TypeError::new(
                    Stage::Boundary,
                    m.span,
                    format!(
                        "method `{}` collides with a field of type `{}` — method and \
                         field names are disjoint per type (D056)",
                        m.name, m.type_name
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// True if this surface type position contains `Ref` anywhere.
fn contains_ref(ty: &Type) -> bool {
    match ty {
        Type::Name(n) => n == "Ref",
        Type::App(h, args) => contains_ref(h) || args.iter().any(contains_ref),
        Type::Borrow(t) => contains_ref(t),
        Type::Func(ps, r) => ps.iter().any(contains_ref) || contains_ref(r),
        Type::Product(es) => es.iter().any(contains_ref),
    }
}

/// True if this internal type position contains `Ref` anywhere.
pub(crate) fn ty_contains_ref(ty: &Ty) -> bool {
    match ty {
        Ty::App(n, args) => n == "Ref" || args.iter().any(ty_contains_ref),
        Ty::Borrow(t) => ty_contains_ref(t),
        Ty::Fun(ps, r) => ps.iter().any(ty_contains_ref) || ty_contains_ref(r),
        Ty::CtorApp(_, args) => args.iter().any(ty_contains_ref),
        Ty::Var(_) | Ty::Bound(_) | Ty::Con(_) => false,
    }
}
