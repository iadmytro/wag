//! Utility functions: type helpers, var renaming, free var collection.

use std::collections::HashMap;

use wag_syntax::{Span, Type};

use crate::error::{Stage, TypeError};
use crate::types::Ty;
use crate::unify::Subst;

/// Types that support `==`/`!=` (both sides must be the same one of these).
pub(super) fn is_comparable(ty: &Ty) -> bool {
    match ty {
        Ty::Con(n) => matches!(n.as_str(), "Int" | "Bool" | "Unit" | "String" | "Char"),
        _ => false,
    }
}

pub(super) fn is_likely_pointer_free(ty: &Ty) -> bool {
    match ty {
        // `Char` is a codepoint — pointer-free (inline, like Int). `Vector a` is
        // arena-resident and non-pointer-free (handled by the `Ty::App` arm below,
        // which excludes it from the PF constructor list).
        Ty::Con(n) => matches!(n.as_str(), "Int" | "Bool" | "Unit" | "Char"),
        Ty::App(n, args) => {
            matches!(
                n.as_str(),
                "Actor" | "Pair" | "Triple"
            ) && args.iter().all(is_likely_pointer_free)
        }
        Ty::Borrow(_) => true,
        Ty::Fun(_, _) => true,
        Ty::CtorApp(..) => false, // conservative: unresolved ctor var
        Ty::Var(_) | Ty::Bound(_) => false,
    }
}

pub(super) fn rename_vars(ty: &Ty, map: &HashMap<u32, Ty>) -> Ty {
    match ty {
        Ty::Var(v) => map.get(v).cloned().unwrap_or(Ty::Var(*v)),
        // Ctor-scheme params (A1b 3b): Bound(k) is replaced by its
        // instantiation; a missing entry means the scheme was not fully
        // instantiated — leave it so validators can flag it.
        Ty::Bound(b) => map.get(b).cloned().unwrap_or(Ty::Bound(*b)),
        Ty::Con(n) => Ty::Con(n.clone()),
        Ty::App(n, args) => Ty::App(
            n.clone(),
            args.iter().map(|a| rename_vars(a, map)).collect(),
        ),
        Ty::Borrow(t) => Ty::Borrow(Box::new(rename_vars(t, map))),
        Ty::Fun(ps, r) => Ty::Fun(
            ps.iter().map(|p| rename_vars(p, map)).collect(),
            Box::new(rename_vars(r, map)),
        ),
        Ty::CtorApp(v, args) => {
            // The ctor-var index is replaced by its instantiation (a fresh Var).
            let new_v = match map.get(v) {
                Some(Ty::Var(iv)) => *iv,
                _ => *v,
            };
            Ty::CtorApp(
                new_v,
                args.iter().map(|a| rename_vars(a, map)).collect(),
            )
        }
    }
}

/// Convert a surface `wag_syntax::Type` into a monotype (no free vars).
/// Delegates to [`surface_to_ty_var`] with an empty var map so both paths
/// share the same flattening and builtin-arity validation.
pub fn surface_to_ty(ty: &Type) -> Result<Ty, TypeError> {
    surface_to_ty_var(ty, &HashMap::new(), false)
}

/// Resolve type aliases in a surface type expression, producing a type with
/// no alias references. Detects cycles via depth limit.
pub(super) fn resolve_aliases(
    ty: &Type,
    aliases: &HashMap<String, Type>,
    depth: usize,
) -> Result<Type, TypeError> {
    if depth > 50 {
        return Err(TypeError::new(
            Stage::Inference,
            Span::new(0, 0),
            "type alias cycle detected",
        ));
    }
    match ty {
        Type::Name(n) => {
            if let Some(aliased) = aliases.get(n) {
                resolve_aliases(aliased, aliases, depth + 1)
            } else {
                Ok(Type::Name(n.clone()))
            }
        }
        Type::Borrow(inner) => Ok(Type::Borrow(Box::new(resolve_aliases(
            inner,
            aliases,
            depth + 1,
        )?))),
        Type::Func(ps, r) => {
            let ps: Result<Vec<_>, _> = ps
                .iter()
                .map(|p| resolve_aliases(p, aliases, depth + 1))
                .collect();
            let r = resolve_aliases(r, aliases, depth + 1)?;
            Ok(Type::Func(ps?, Box::new(r)))
        }
        Type::App(head, args) => {
            let head = resolve_aliases(head, aliases, depth + 1)?;
            let args: Result<Vec<_>, _> = args
                .iter()
                .map(|a| resolve_aliases(a, aliases, depth + 1))
                .collect();
            Ok(Type::App(Box::new(head), args?))
        }
        Type::Product(types) => {
            let types: Result<Vec<_>, _> = types
                .iter()
                .map(|t| resolve_aliases(t, aliases, depth + 1))
                .collect();
            Ok(Type::Product(types?))
        }
    }
}

/// Resolve aliases then convert to internal `Ty`.
pub fn surface_to_ty_resolved(ty: &Type, aliases: &HashMap<String, Type>) -> Result<Ty, TypeError> {
    let resolved = resolve_aliases(ty, aliases, 0)?;
    surface_to_ty(&resolved)
}

/// Expand aliased type names inside an internal `Ty`: any `Ty::Con(n)` whose
/// name is a registered alias is replaced by the alias's expansion. Aliases
/// are transparent abbreviations (§3.2); builtin schemes name `String`
/// directly, so they must be brought to agreement with declaration-site
/// expansion before inference.
pub(super) fn expand_ty_aliases(
    ty: &Ty,
    aliases: &HashMap<String, Type>,
    depth: usize,
) -> Result<Ty, TypeError> {
    if depth > 50 {
        return Err(TypeError::new(
            Stage::Inference,
            Span::new(0, 0),
            "type alias cycle detected",
        ));
    }
    match ty {
        Ty::Con(n) => {
            if let Some(st) = aliases.get(n) {
                let resolved = resolve_aliases(st, aliases, depth + 1)?;
                surface_to_ty(&resolved)
            } else {
                Ok(Ty::Con(n.clone()))
            }
        }
        Ty::App(h, args) => {
            let mut out = Vec::with_capacity(args.len());
            for a in args {
                out.push(expand_ty_aliases(a, aliases, depth + 1)?);
            }
            Ok(Ty::App(h.clone(), out))
        }
        Ty::Borrow(inner) => Ok(Ty::Borrow(Box::new(expand_ty_aliases(
            inner, aliases, depth + 1,
        )?))),
        Ty::Fun(ps, r) => {
            let mut out = Vec::with_capacity(ps.len());
            for p in ps {
                out.push(expand_ty_aliases(p, aliases, depth + 1)?);
            }
            Ok(Ty::Fun(out, Box::new(expand_ty_aliases(r, aliases, depth + 1)?)))
        }
        other => Ok(other.clone()),
    }
}

/// Convert a surface type to internal `Ty`, resolving type variable names
/// (lowercase idents) to live `Ty::Var` indices. Used for signatures and
/// method schemes.
pub(super) fn surface_to_ty_with_vars(
    ty: &Type,
    type_vars: &HashMap<String, u32>,
) -> Result<Ty, TypeError> {
    surface_to_ty_var(ty, type_vars, false)
}

/// Convert a surface type to internal `Ty`, resolving type variable names to
/// declaration-space `Ty::Bound` indices. Used for parametric ctor schemes and
/// field types (A1b 3b: bound vars are structurally distinct from live
/// metavars).
pub(super) fn surface_to_ty_with_bound(
    ty: &Type,
    type_vars: &HashMap<String, u32>,
) -> Result<Ty, TypeError> {
    surface_to_ty_var(ty, type_vars, true)
}

fn surface_to_ty_var(
    ty: &Type,
    type_vars: &HashMap<String, u32>,
    bound: bool,
) -> Result<Ty, TypeError> {
    match ty {
        Type::Name(n) => {
            if let Some(&idx) = type_vars.get(n) {
                Ok(if bound { Ty::Bound(idx) } else { Ty::Var(idx) })
            } else {
                Ok(Ty::Con(n.clone()))
            }
        }
        Type::Borrow(inner) => Ok(Ty::Borrow(Box::new(surface_to_ty_var(
            inner, type_vars, bound,
        )?))),
        Type::Func(ps, r) => {
            let ps: Result<Vec<_>, _> = ps
                .iter()
                .map(|p| surface_to_ty_var(p, type_vars, bound))
                .collect();
            Ok(Ty::Fun(
                ps?,
                Box::new(surface_to_ty_var(r, type_vars, bound)?),
            ))
        }
        Type::App(head, args) => {
            let mut flat_args: Vec<Ty> = Vec::new();
            let head_name = flatten_app_var(head, args, &mut flat_args, type_vars, bound)?;
            check_builtin_arity(&head_name, &flat_args)?;
            Ok(Ty::App(head_name, flat_args))
        }
        Type::Product(types) => {
            let args: Vec<Ty> = types
                .iter()
                .map(|t| surface_to_ty_var(t, type_vars, bound))
                .collect::<Result<_, _>>()?;
            Ok(Ty::App("Pair".to_string(), args))
        }
    }
}

fn flatten_app_var(
    head: &Type,
    args: &[Type],
    out: &mut Vec<Ty>,
    type_vars: &HashMap<String, u32>,
    bound: bool,
) -> Result<String, TypeError> {
    match head {
        Type::Name(n) => {
            for a in args {
                out.push(surface_to_ty_var(a, type_vars, bound)?);
            }
            Ok(n.clone())
        }
        Type::App(h2, a2) => {
            let name = flatten_app_var(h2, a2, out, type_vars, bound)?;
            for a in args {
                out.push(surface_to_ty_var(a, type_vars, bound)?);
            }
            Ok(name)
        }
        Type::Borrow(_) | Type::Func(_, _) | Type::Product(_) => Err(TypeError::new(
            Stage::Inference,
            Span::new(0, 0),
            "invalid type application head",
        )),
    }
}

/// Convert a surface type to internal `Ty`, resolving class param names to
/// `Ty::CtorApp` nodes and type variable names to `Ty::Var` indices.
/// Used for class method signatures (D075, §3.12).
pub(super) fn surface_to_ty_with_ctor_vars(
    ty: &Type,
    type_vars: &HashMap<String, u32>,
    ctor_vars: &HashMap<String, u32>,
) -> Result<Ty, TypeError> {
    match ty {
        Type::Name(n) => {
            if let Some(&idx) = ctor_vars.get(n) {
                Ok(Ty::CtorApp(idx, vec![]))
            } else if let Some(&idx) = type_vars.get(n) {
                Ok(Ty::Var(idx))
            } else {
                Ok(Ty::Con(n.clone()))
            }
        }
        Type::Borrow(inner) => Ok(Ty::Borrow(Box::new(surface_to_ty_with_ctor_vars(
            inner, type_vars, ctor_vars,
        )?))),
        Type::Func(ps, r) => {
            let ps: Result<Vec<_>, _> = ps
                .iter()
                .map(|p| surface_to_ty_with_ctor_vars(p, type_vars, ctor_vars))
                .collect();
            Ok(Ty::Fun(
                ps?,
                Box::new(surface_to_ty_with_ctor_vars(r, type_vars, ctor_vars)?),
            ))
        }
        Type::App(head, args) => {
            let (head_name, all_args) = flatten_app_head(head, args);
            if let Some(&idx) = ctor_vars.get(&head_name) {
                let mut flat_args: Vec<Ty> = Vec::new();
                for a in &all_args {
                    flat_args.push(surface_to_ty_with_ctor_vars(a, type_vars, ctor_vars)?);
                }
                Ok(Ty::CtorApp(idx, flat_args))
            } else {
                let mut flat_args: Vec<Ty> = Vec::new();
                for a in &all_args {
                    flat_args.push(surface_to_ty_with_ctor_vars(a, type_vars, ctor_vars)?);
                }
                check_builtin_arity(&head_name, &flat_args)?;
                Ok(Ty::App(head_name, flat_args))
            }
        }
        Type::Product(types) => {
            let args: Vec<Ty> = types
                .iter()
                .map(|t| surface_to_ty_with_ctor_vars(t, type_vars, ctor_vars))
                .collect::<Result<_, _>>()?;
            Ok(Ty::App("Pair".to_string(), args))
        }
    }
}

/// Flatten nested `Type::App` to `(head_name, all_args)` without converting.
fn flatten_app_head(head: &Type, args: &[Type]) -> (String, Vec<Type>) {
    match head {
        Type::Name(n) => (n.clone(), args.to_vec()),
        Type::App(h2, a2) => {
            let (name, mut all) = flatten_app_head(h2, a2);
            all.extend(args.iter().cloned());
            (name, all)
        }
        _ => (String::new(), args.to_vec()),
    }
}

/// §3.1: the builtin parametric types are fixed-arity instances of `TypeApp`.
/// A flat application with the wrong argument count — e.g. `Option List T`
/// (parsed as head `Option` + args `[List, T]`) — is rejected here instead
/// of silently building a malformed type; the nested form needs parens:
/// `Option (List T)`.
fn check_builtin_arity(head: &str, args: &[Ty]) -> Result<(), TypeError> {
    let expected = match head {
        "Actor" => 1,
        _ => return Ok(()),
    };
    if args.len() != expected {
        Err(TypeError::new(
            Stage::Inference,
            Span::new(0, 0),
            format!(
                "`{head}` expects {expected} type argument, got {} (nested applications need parens, e.g. `Option (List T)`)",
                args.len()
            ),
        ))
    } else {
        Ok(())
    }
}

/// Collect type variable names from a surface type.
/// Convention: a single uppercase letter (A–Z) is a type variable.
/// Multi-letter uppercase names are type constructors.
pub(super) fn collect_type_var_names(ty: &Type, out: &mut std::collections::BTreeSet<String>) {
    match ty {
        Type::Name(n) => {
            if n.len() == 1 && n.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                out.insert(n.clone());
            }
        }
        Type::Borrow(inner) => collect_type_var_names(inner, out),
        Type::Func(ps, r) => {
            for p in ps {
                collect_type_var_names(p, out);
            }
            collect_type_var_names(r, out);
        }
        Type::App(head, args) => {
            collect_type_var_names(head, out);
            for a in args {
                collect_type_var_names(a, out);
            }
        }
        Type::Product(types) => {
            for t in types {
                collect_type_var_names(t, out);
            }
        }
    }
}

// ─── Generalization helpers (§3.5, Option A) ───────────────────────────────

/// Collect all type variable ids in `ty` that are NOT resolved in `subst`.
pub(super) fn collect_free_vars(ty: &Ty, subst: &Subst) -> Vec<u32> {
    let mut vars: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
    collect_free_vars_into(ty, subst, &mut vars);
    vars.into_iter().collect()
}

pub(super) fn collect_free_vars_into(
    ty: &Ty,
    subst: &Subst,
    out: &mut std::collections::BTreeSet<u32>,
) {
    match ty {
        Ty::Var(v) => {
            if subst.get(*v).is_none() {
                out.insert(*v);
            }
        }
        // Bound vars are quantified, not free metavariables (A1b).
        Ty::Bound(_) => {},
        Ty::Con(_) => {}
        Ty::App(_, args) => {
            for a in args {
                collect_free_vars_into(a, subst, out);
            }
        }
        Ty::Borrow(t) => collect_free_vars_into(t, subst, out),
        Ty::Fun(ps, r) => {
            for p in ps {
                collect_free_vars_into(p, subst, out);
            }
            collect_free_vars_into(r, subst, out);
        }
        Ty::CtorApp(v, args) => {
            if subst.get(*v).is_none() {
                out.insert(*v);
            }
            for a in args {
                collect_free_vars_into(a, subst, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(s: &str) -> Type {
        Type::Name(s.to_string())
    }
    /// `Head A B ...` as a flat surface App.
    fn app(head: &str, args: &[&str]) -> Type {
        Type::App(
            Box::new(name(head)),
            args.iter().map(|a| name(a)).collect(),
        )
    }

    #[test]
    fn builtin_arity_ok_single_arg() {
        for ty in [app("Option", &["Int"]), app("List", &["T"]), app("Actor", &["U"])] {
            let t = surface_to_ty(&ty).expect("single-arg builtin should resolve");
            assert!(matches!(t, Ty::App(_, ref a) if a.len() == 1), "{t:?}");
        }
    }

    #[test]
    fn builtin_arity_rejects_extra_flat_args() {
        // `Actor Int String` parses as head Actor + flat args [Int, String].
        let t = surface_to_ty(&app("Actor", &["Int", "String"]));
        let err = t.expect_err("flat 2-arg Actor must be rejected");
        assert!(
            err.to_string().contains("`Actor` expects 1 type argument, got 2"),
            "unexpected message: {err}"
        );
    }

    #[test]
    fn builtin_arity_rejects_zero_args() {
        let t = surface_to_ty(&name("Actor"));
        // A bare `Option` is a Name, not an App — no arity check applies here;
        // it resolves to Con and fails later at name resolution. Just ensure we
        // don't panic or misreport in this converter.
        assert!(matches!(t, Ok(Ty::Con(_))));
    }

    #[test]
    fn nested_application_with_parens_is_accepted() {
        // `Option (List T)` — the correct form for a parametric argument.
        let ty = Type::App(
            Box::new(name("Option")),
            vec![Type::App(Box::new(name("List")), vec![name("T")])],
        );
        let t = surface_to_ty(&ty).expect("parenthesized nested app should resolve");
        assert!(
            matches!(&t, Ty::App(h, a) if h == "Option" && a.len() == 1
                && matches!(&a[0], Ty::App(h2, _) if h2 == "List")),
            "{t:?}"
        );
    }

    #[test]
    fn user_type_head_is_not_arity_checked_here() {
        // User parametric types are variable-arity (§3.1); this converter has no
        // declaration table, so `MyPair Int` must pass through untouched.
        let t = surface_to_ty(&app("MyPair", &["Int"])).expect("user head passes through");
        assert!(matches!(t, Ty::App(ref h, _) if h == "MyPair"));
    }
}
