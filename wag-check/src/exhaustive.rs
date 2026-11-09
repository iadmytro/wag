//! Match exhaustiveness check (§3.6).
//!
//! Walks the AST to find all `Expr::Match` nodes and verifies that the
//! patterns collectively cover every constructor of the matched type.
//! The type is derived from the constructors in the patterns (via
//! `CtorInfo::type_name`), so this works for both concrete and polymorphic
//! matches — no type resolution needed.

use std::collections::{BTreeMap, BTreeSet};

use wag_syntax::{Expr, Item, MatchArm, Pat};
use wag_types::TypedProgram;

use crate::error::{CheckError, Rule};

/// Check all match expressions in the program for exhaustiveness.
/// Per-item exhaustiveness check (D067).
pub(crate) fn check_item(item: &Item, typed: &TypedProgram) -> Result<(), CheckError> {
    match item {
        Item::Func(f) => check_expr(&f.body, typed),
        Item::Actor(a) => check_expr(&a.body, typed),
        _ => Ok(()),
    }
}

fn check_expr(expr: &Expr, typed: &TypedProgram) -> Result<(), CheckError> {
    match expr {
        Expr::Match { arms, span, .. } => check_match(arms, *span, typed),
        Expr::Apply { fun, args, .. } => {
            check_expr(fun, typed)?;
            for a in args {
                check_expr(a, typed)?;
            }
            Ok(())
        }
        Expr::BinOp { lhs, rhs, .. } => {
            check_expr(lhs, typed)?;
            check_expr(rhs, typed)
        }
        Expr::Field { base, .. } | Expr::Borrow { inner: base, .. } => check_expr(base, typed),
        Expr::RecordUpdate { receiver, clauses, .. } => {
            check_expr(receiver, typed)?;
            for c in clauses {
                match c {
                    wag_syntax::UpdateClause::Leaf { value, .. } => check_expr(value, typed)?,
                    wag_syntax::UpdateClause::Group { clauses, .. } => {
                        for cc in clauses {
                            if let wag_syntax::UpdateClause::Leaf { value, .. } = cc {
                                check_expr(value, typed)?;
                            }
                        }
                    }
                }
            }
            Ok(())
        }
        Expr::SetField { receiver, value, .. } => {
            check_expr(receiver, typed)?;
            check_expr(value, typed)
        }
        Expr::RegionWrite { receiver, value, .. } => {
            check_expr(receiver, typed)?;
            check_expr(value, typed)
        }
        Expr::Bind { value, body, .. } => {
            check_expr(value, typed)?;
            check_expr(body, typed)
        }
        Expr::Seq { first, second, .. } => {
            check_expr(first, typed)?;
            check_expr(second, typed)
        }
        Expr::If { arms, .. } => {
            for arm in arms {
                if let Some(c) = &arm.condition {
                    check_expr(c, typed)?;
                }
                check_expr(&arm.body, typed)?;
            }
            Ok(())
        }
        // Leaf nodes — nothing to recurse into.
        _ => Ok(()),
    }
}

fn check_match(
    arms: &[MatchArm],
    span: wag_syntax::Span,
    typed: &TypedProgram,
) -> Result<(), CheckError> {
    // Group arms by top-level constructor; detect catch-all.
    let mut groups: BTreeMap<String, Vec<&Vec<Pat>>> = BTreeMap::new();
    let mut catch_all = false;
    for (i, arm) in arms.iter().enumerate() {
        if catch_all {
            return Err(CheckError::new(
                Rule::Exhaustiveness,
                span,
                format!("unreachable match arm at index {i} after catch-all"),
            ));
        }
        match &arm.pattern {
            Pat::Wildcard { .. } | Pat::Var { .. } => catch_all = true,
            Pat::Ctor { name, args, .. } => {
                groups.entry(name.clone()).or_default().push(args);
            }
            // Native product destructuring `(p1, p2 [, p3])` (§3.6). Inference
            // labels every product `Ty::App("Pair", …)` regardless of arity, so all
            // supported arities group uniformly under `Pair`. A full tuple pattern
            // covers every field position (checked below), and products carry no
            // data constructors — the exhaustive pass early-returns on their empty
            // ctor set — so no catch-all is needed here.
            Pat::Product { elems, .. } => {
                groups.entry("Pair".into()).or_default().push(elems);
            }
            // Literal pattern (§9): pins one value but covers no data
            // constructor, so it contributes neither a group nor a catch-all.
            // Value match over Int/String/Char has no constructors (all_ctors is
            // empty below) and so carries no exhaustiveness obligation.
            Pat::Literal { .. } => {}
        }
    }
    if catch_all {
        return Ok(());
    }

    // Look up the scrutinee type recorded during inference (pipeline: types → check).
    let Some(tn) = typed.match_types.get(&span) else {
        // No recorded scrutinee type: either a type-variable scrutinee or a
        // compiler-generated (desugared field-access) match — exhaustiveness is not
        // governed by a user scrutinee type, so skip.
        return Ok(());
    };

    // Get all constructors of this type.
    let mut all_ctors: BTreeSet<String> = typed
        .type_ctors
        .get(tn)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .collect();

    // Outbox arms (D043): if this is an actor's Msg type, its outbox
    // constructors are also required arms.
    for (actor_name, msg_ty) in &typed.actors {
        // Actor ty is App("Actor", [inner])
        if let wag_types::Ty::App(_, args) = msg_ty {
            if args.len() == 1 {
                if let wag_types::Ty::Con(inner_name) = &args[0] {
                    if inner_name == tn {
                        if let Some(ctors) = typed.actor_outbox.get(actor_name) {
                            for c in ctors {
                                all_ctors.insert(c.clone());
                            }
                        }
                    }
                }
            }
        }
    }

    if all_ctors.is_empty() {
        return Ok(());
    }

    // All top-level constructors must be present.
    for c in &all_ctors {
        if !groups.contains_key(c) {
            return Err(CheckError::new(
                Rule::Exhaustiveness,
                span,
                format!("non-exhaustive match on `{tn}`: missing `{c}`"),
            ));
        }
    }

    // For each constructor, verify nested patterns collectively cover
    // all possibilities for each field position.
    for (ctor_name, args_list) in &groups {
        let ctor = match typed.constructors.get(ctor_name) {
            Some(c) => c,
            None => continue,
        };
        let num_fields = ctor.fields.len();
        if num_fields == 0 {
            continue;
        }
        for i in 0..num_fields {
            let col: Vec<&Pat> = args_list.iter().filter_map(|args| args.get(i)).collect();
            if col.is_empty() {
                continue;
            }
            // A Var/Wildcard/product in any arm covers all possibilities for this
            // field position (products are checked element-wise by inference).
            if col.iter().any(|p| {
                matches!(p, Pat::Wildcard { .. } | Pat::Var { .. } | Pat::Product { .. })
            }) {
                continue;
            }
            // Extract the field's type-constructor name (skip type vars).
            // A1b: sanctioned read — monomorphic scrutinee types have no param
            // vars; the empty-args fallback is exact.
            let field_type_name = match ctor.field_ty(i, &[]) {
                wag_types::Ty::Con(n) => Some(n.clone()),
                wag_types::Ty::App(n, _) => Some(n.clone()),
                _ => None,
            };
            let Some(ftn) = field_type_name else { continue };
            let field_ctors: BTreeSet<String> = typed
                .type_ctors
                .get(&ftn)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .collect();
            if field_ctors.is_empty() {
                continue;
            }
            let mut covered: BTreeSet<String> = BTreeSet::new();
            for p in &col {
                if let Pat::Ctor { name, .. } = p {
                    covered.insert(name.clone());
                }
            }
            for c in &field_ctors {
                if !covered.contains(c) {
                    return Err(CheckError::new(
                        Rule::Exhaustiveness,
                        span,
                        format!(
                            "non-exhaustive match on `{tn}`: `{ctor_name}` arms don't cover all `{ftn}` cases (missing `{c}`)"
                        ),
                    ));
                }
            }
        }
    }

    Ok(())
}
