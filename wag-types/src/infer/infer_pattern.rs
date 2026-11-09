//! Pattern binding (type-level). Exhaustiveness checking lives in `wag-check`.

use wag_syntax::{Expr, LitPat, Pat};

use crate::error::{Stage, TypeError};
use crate::types::{Scheme, Ty};
use crate::unify::{pretty_ty, unify};

use super::context::Context;
use super::util::{is_comparable, is_likely_pointer_free, surface_to_ty_resolved};

/// Render a short, human-readable label for an expression so type errors can
/// name the value being matched/used instead of leaking a bare metavar.
fn expr_label(e: &Expr) -> String {
    match e {
        Expr::Var { name, .. } => name.clone(),
        // Field chains (`p.a.b`) read left-to-right; anything else is shown
        // generically so the error points at a real binding, not `?N`.
        Expr::Field { base, field, .. } => {
            format!("{}.{}", expr_label(base), field)
        }
        _ => "this expression".into(),
    }
}

impl Context {
    pub(super) fn bind_pattern(
        &mut self,
        pat: &Pat,
        scrut: &Ty,
        scrut_expr: Option<&Expr>,
    ) -> Result<Vec<(String, Option<Scheme>)>, TypeError> {
        let mut saved = Vec::new();
        self.bind_pat_rec(pat, scrut, &mut saved, scrut_expr)?;
        Ok(saved)
    }

    pub(super) fn bind_pat_rec(
        &mut self,
        pat: &Pat,
        scrut: &Ty,
        saved: &mut Vec<(String, Option<Scheme>)>,
        scrut_expr: Option<&Expr>,
    ) -> Result<(), TypeError> {
        let scrut = self.subst.apply(scrut);
        // Matching &T uses same patterns; binders get T (owned view of structure) —
        // for borrow well-formedness the checker cares; for types, nested fields keep shape.
        let scrut_base = scrut.strip_borrow().clone();
        match pat {
            Pat::Wildcard { .. } => Ok(()),
            Pat::Var { name, span } => {
                // Bind to full scrut type (including borrow if scrut was borrowed).
                saved.push((name.clone(), self.env.remove(&(name.clone(), 0))));
                self.env
                    .insert((name.clone(), 0), Scheme::mono(scrut.clone()));
                // Side table: record the resolved binding type so downstream
                // ownership checks (wag-check) consume it instead of re-deriving
                // from the global constructor table, whose field types stay as
                // uninstantiated variables for generic builtins (§3.2).
                if let Some(item) = &self.cur_item {
                    self.expr_types.insert((item.clone(), *span), scrut.clone());
                }
                Ok(())
            }
            Pat::Ctor {
                name,
                qualifier,
                args,
                span,
            } => {
                // Outbox rule (D043, spec §5.5.7): an arm for a reply ctor
                // delivered by one of the actor's properties is legal even
                // though the ctor does not belong to the scrutinee Msg type.
                // Qualified (`@std.Line`) must name an in-property outbox ctor;
                // a bare name is an outbox ctor when in some property's outbox.
                // The ctors themselves are prelude-declared (D035 retired).
                let outbox_arm = match qualifier.as_deref() {
                    Some(q) => {
                        self.cur_props.iter().any(|p| p == q) && crate::outbox::in_outbox(q, name)
                    }
                    None => self
                        .cur_props
                        .iter()
                        .any(|p| crate::outbox::in_outbox(p, name)),
                };

                let ctor = self.ctors.get(name).cloned().ok_or_else(|| {
                    let display = match qualifier {
                        Some(ref a) => format!("@{a}.{name}"),
                        None => name.clone(),
                    };
                    TypeError::new(
                        Stage::Inference,
                        *span,
                        format!("unknown constructor `{display}` in pattern"),
                    )
                })?;
                // Outbox arms are delivered into the mailbox outside the Msg
                // type — they are not unified with the scrutinee (spec §5.5.7).
                let (field_tys, _result_ty) = self.bind_ctor_scheme(
                    &ctor.scheme,
                    (!outbox_arm).then_some(&scrut_base),
                    *span,
                )?;
                if field_tys.len() != args.len() {
                    return Err(TypeError::new(
                        Stage::Inference,
                        *span,
                        format!(
                            "constructor `{name}` expects {} pattern arg(s), found {}",
                            field_tys.len(),
                            args.len()
                        ),
                    ));
                }
                // If scrut was borrowed, nested patterns also see borrowed field types.
                // Spec: match on &T re-borrows the fields of the matched constructor.
                let scrut_borrowed = matches!(scrut, Ty::Borrow(_));
                for (p, ft) in args.iter().zip(field_tys.iter()) {
                    let ft = self.subst.apply(ft);
                    let nested = if scrut_borrowed {
                        // Pointer-free fields stay owned; non-PF become borrowed.
                        match &ft {
                            Ty::Con(_) | Ty::App(_, _) if is_likely_pointer_free(&ft) => ft,
                            other => {
                                // User types: borrow the whole field if non-trivial.
                                match other {
                                    Ty::Con(_) => other.clone(), // nullary user / Int-like
                                    _ => Ty::Borrow(Box::new(other.clone())),
                                }
                            }
                        }
                    } else {
                        ft
                    };
                    self.bind_pat_rec(p, &nested, saved, scrut_expr)?;
                }
                Ok(())
            }
            // Literal pattern (§9): pins a value in an `==`-supporting position.
            // Its concrete type must match the scrutinee (value match) or the
            // constructor field it occupies; both reduce to unification. Only
            // comparable primitives are allowed here.
            Pat::Literal { lit, span } => {
                let literal_ty = match lit {
                    LitPat::Int(_) => Ty::Con("Int".into()),
                    // Resolve `String` through any user-defined alias so the
                    // pattern agrees with declaration-site expansion (§3.2).
                    LitPat::Str(_) => surface_to_ty_resolved(
                        &wag_syntax::Type::Name("String".into()),
                        &self.aliases,
                    )?,
                };
                if !is_comparable(&scrut_base) {
                    return Err(TypeError::new(
                        Stage::Inference,
                        *span,
                        "literal pattern is not valid here",
                    )
                    .with_context(format!(
                        "literal patterns are only valid where the type is Int, Bool, String, or Char; this position has `{}`",
                        pretty_ty(&scrut_base, &self.subst)
                    )));
                }
                unify(&mut self.subst, &scrut_base, &literal_ty, *span)
            }
            // Native product destructuring `(p1, p2 [, p3])` (§3.6). The
            // scrutinee type is an internal `Pair`/`Triple` application; bind each
            // element pattern against the matching field.
            Pat::Product { elems, span } => {
                let fields = match &scrut_base {
                    Ty::App(n, args)
                        if (n == "Pair" || n == "Triple") && args.len() == elems.len() =>
                    {
                        args.clone()
                    }
                    Ty::App(n, _) => {
                        let who = scrut_expr
                            .map(expr_label)
                            .unwrap_or_else(|| "the scrutinee".into());
                        return Err(TypeError::new(
                            Stage::Inference,
                            *span,
                            format!(
                                "product pattern with {} field(s) cannot match `{who}` (type `{n}`)",
                                elems.len()
                            ),
                        )
                        .with_context(format!(
                            "`{who}` is not a tuple; `(a, b)` patterns destructure only product/tuple values"
                        )));
                    }
                    other => {
                        let who = scrut_expr
                            .map(expr_label)
                            .unwrap_or_else(|| "the scrutinee".into());
                        return Err(TypeError::new(
                            Stage::Inference,
                            *span,
                            format!(
                                "product pattern cannot match `{who}` — its type `{}` is not a product",
                                pretty_ty(other, &self.subst)
                            ),
                        )
                        .with_context(format!(
                            "the value being matched is `{who}`; use an arm for each of its constructors instead of a tuple pattern"
                        )));
                    }
                };
                for (p, ft) in elems.iter().zip(fields.iter()) {
                    self.bind_pat_rec(p, ft, saved, scrut_expr)?;
                }
                Ok(())
            }
        }
    }

    pub(super) fn restore(&mut self, saved: Vec<(String, Option<Scheme>)>) {
        for (name, old) in saved.into_iter().rev() {
            match old {
                Some(s) => {
                    self.env.insert((name, 0), s);
                }
                None => {
                    self.env.remove(&(name, 0));
                }
            }
        }
    }
}
