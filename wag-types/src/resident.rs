//! Region-residency computation (D054): which declared datatypes live in the
//! per-actor region. A type is **resident** if any field position — direct or
//! transitive through other declared types and aliases — contains `Ref T` or a
//! transitively resident declared type. The result is a monotone fixed point over
//! the declaration graph (cycles terminate because values only flip false→true).

use std::collections::BTreeMap;

use wag_syntax::{Item, Type};

/// Compute residency for every declared datatype and alias in `program`.
pub fn compute_residency(program: &wag_syntax::Program) -> BTreeMap<String, bool> {
    compute_residency_items(&program.items)
}

/// Same as [`compute_residency`] but over a single module's items (multi-module
/// programs merge per-module maps).
pub fn compute_residency_items(items: &[Item]) -> BTreeMap<String, bool> {
    // Collect declarations. A sum type ORs the field types of all its constructors.
    let mut ctor_fields: BTreeMap<String, Vec<Type>> = BTreeMap::new();
    for item in items {
        match item {
            // Datatype decls (parametric or not) carry their constructors here.
            Item::ParametricType(p) => {
                for c in &p.ctors {
                    ctor_fields
                        .entry(p.name.clone())
                        .or_default()
                        .extend(c.fields.iter().map(|f| f.ty.clone()));
                }
            }
            // Bare constructor items (if any path emits them).
            Item::Constructor(c) => {
                if let Some(name) = c.result_type.head_name() {
                    ctor_fields
                        .entry(name)
                        .or_default()
                        .extend(c.fields.iter().map(|f| f.ty.clone()));
                }
            }
            _ => {}
        }
    }
    let aliases: Vec<(String, Type)> = items
        .iter()
        .filter_map(|i| match i {
            Item::TypeAlias(a) => Some((a.name.clone(), a.ty.clone())),
            _ => None,
        })
        .collect();

    // Seed every declared name as non-resident; iterate to the fixed point.
    let mut map: BTreeMap<String, bool> = BTreeMap::new();
    for name in ctor_fields.keys() {
        map.insert(name.clone(), false);
    }
    for (name, _) in &aliases {
        map.entry(name.clone()).or_insert(false);
    }

    let mut changed = true;
    while changed {
        changed = false;
        for (name, fields) in &ctor_fields {
            let r = fields.iter().any(|f| ty_contains_ref_or_resident(f, &map));
            if map.get(name) != Some(&r) {
                map.insert(name.clone(), r);
                changed = true;
            }
        }
        for (name, ty) in &aliases {
            let r = ty_contains_ref_or_resident(ty, &map);
            if map.get(name) != Some(&r) {
                map.insert(name.clone(), r);
                changed = true;
            }
        }
    }
    map
}

/// True if `ty` contains a `Ref` in any field position or references a declared
/// type that is (currently known to be) resident. Recursion only descends the
/// finite structure of `ty`; name references terminate via the map lookup, so no
/// separate cycle guard is needed.
fn ty_contains_ref_or_resident(ty: &Type, map: &BTreeMap<String, bool>) -> bool {
    match ty {
        Type::Name(n) => {
            if n == "Ref" {
                return true; // degenerate nullary form
            }
            // A self/mutual reference contributes the type's *current* known
            // residency — never an assumed `true` — so a Ref-free recursive user
            // type (§7 rule 14) stays non-resident.
            map.get(n.as_str()).copied().unwrap_or(false)
        }
        Type::App(head, args) => {
            head.head_name().as_deref() == Some("Ref")
                || args.iter().any(|a| ty_contains_ref_or_resident(a, map))
        }
        Type::Borrow(inner) => ty_contains_ref_or_resident(inner, map),
        // A function type is resident if its argument or result position contains Ref.
        Type::Func(args, ret) => {
            args.iter().any(|a| ty_contains_ref_or_resident(a, map))
                || ty_contains_ref_or_resident(ret, map)
        }
        // Product: any component resident → the whole product is resident.
        Type::Product(comps) => comps.iter().any(|c| ty_contains_ref_or_resident(c, map)),
    }
}
