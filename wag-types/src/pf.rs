//! Pointer-free classification (§3.2), derived once per program (A1b invariant:
//! downstream consumers read the derived table, never declaration-space field types).
//!
//! A type is pointer-free when its values can be copied without ownership tracking.
//! This is a *separate* analysis from region residency (D054): `List Int` / `String`
//! own heap data (non-PF) yet need no region cell (non-resident), while resident
//! types are always non-PF.

use std::collections::{BTreeMap, BTreeSet};

use crate::types::{CtorInfo, Ty};

/// Per declared type name: pointer-free? Monotone fixed point over constructor
/// field types; self-referential types converge to non-pointer-free (§7 rule 14).
/// Built-in heads get entries too but the walker below handles them by rule.
pub fn compute_pointer_free(ctors: &BTreeMap<String, CtorInfo>) -> BTreeMap<String, bool> {
    let mut by_type: BTreeMap<String, Vec<&CtorInfo>> = BTreeMap::new();
    for c in ctors.values() {
        by_type.entry(c.type_name.clone()).or_default().push(c);
    }
    let names: BTreeSet<String> = by_type.keys().cloned().collect();
    let mut pf: BTreeMap<String, bool> = names.iter().map(|n| (n.clone(), false)).collect();
    loop {
        let mut changed = false;
        for name in &names {
            if pf[name] {
                continue;
            }
            if by_type[name].iter().all(|c| c.fields.iter().all(|f| ty_is_pointer_free(&f.ty, &pf)))
            {
                pf.insert(name.clone(), true);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    pf
}

/// Pointer-free for an arbitrary (applied) type. `pf_types` is the per-user-type
/// table from [`compute_pointer_free`]; user heads absent from it are conservatively
/// non-pointer-free. Built-in heads follow fixed rules: `Char`/`Int`/`Bool`/`Unit`
/// inline; actor handles and functions copyable; `List` owns heap data;
/// `Option`/`Pair`/`Triple` recurse into their args.
pub fn ty_is_pointer_free(ty: &Ty, pf_types: &BTreeMap<String, bool>) -> bool {
    match ty {
        Ty::CtorApp(..) => false, // conservative: unresolved ctor var
        Ty::Var(_) | Ty::Bound(_) => false, // conservative: unresolved metavar / bound param
        Ty::Con(n) => {
            matches!(n.as_str(), "Int" | "Bool" | "Unit" | "Char") || *pf_types.get(n).unwrap_or(&false)
        }
        Ty::App(n, args) => match n.as_str() {
            "Actor" => true,
            "Option" => args.first().map(|a| ty_is_pointer_free(a, pf_types)).unwrap_or(false),
            "Pair" | "Triple" => args.iter().all(|a| ty_is_pointer_free(a, pf_types)),
            _ => *pf_types.get(n).unwrap_or(&false), // user type (mono or parametric)
        },
        Ty::Borrow(t) => ty_is_pointer_free(t, pf_types),
        Ty::Fun(_, _) => true,
    }
}
