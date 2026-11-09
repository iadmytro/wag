//! Expression inference: apply, field access, update clauses.

use wag_syntax::{BinOp, Expr, Span, UpdateClause};

use crate::error::{OriginInfo, Stage, TypeError};
use crate::types::{Scheme, Ty};
use crate::unify::{pretty_ty, unify, Subst, unify_arm_join};

use super::context::Context;
use super::util::{collect_free_vars, is_comparable, is_likely_pointer_free, surface_to_ty_resolved};

/// Canonical operator name for the overload table (D074) — matches the
/// `(operator)` symbol spellings accepted by the parser.
pub(super) fn binop_name(op: &BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Rem => "%",
        BinOp::Gt => ">",
        BinOp::Lt => "<",
        BinOp::Ge => ">=",
        BinOp::Le => "<=",
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::BitNot => "!",
        BinOp::BitXor => "^",
        BinOp::BitOr => "|",
        BinOp::BitAnd => "&",
        BinOp::ShiftLeft => "<<",
        BinOp::ShiftRight => ">>",
    }
}

impl Context {
    /// Static-dispatch lookup for an operator use-site (D074). Tries each
    /// entry for `op_name` in declaration order. Two phases per candidate:
    /// a scratch-subst match test (allocates no real vars, so failed
    /// candidates leak nothing into `expected_free` — A1b), then a committed
    /// instantiate + unify against the operand types. Returns the selected
    /// result type; for fn entries also records the rewrite decision in
    /// `op_dispatch`. `None` when no entry matches — callers fall back to
    /// built-in operator semantics.
    pub(super) fn dispatch_binop(
        &mut self,
        op_name: &str,
        arity: usize,
        lt: &Ty,
        rt: Option<&Ty>,
        span: Span,
    ) -> Option<Ty> {
        // Dispatch requires concrete operand types: if an operand is still a
        // metavariable hole, committing to an instance would force the user's
        // type (e.g. binding a fresh element var to `String`). Fall back to
        // native semantics — v1 has no deferred operator resolution.
        //
        // Exception (§3.12): if a live class constraint covers the operand's
        // variable (e.g. `Eq T => …` with `x : T`), the constraint is the
        // type-level guarantee — accept the op and let the VM's structural
        // equality resolve it at runtime, where `T` is concrete.
        let lt_concrete = collect_free_vars(lt, &self.subst).is_empty();
        let rt_concrete = match rt {
            Some(r) => collect_free_vars(r, &self.subst).is_empty(),
            None => true,
        };
        if !lt_concrete || !rt_concrete {
            if let Some(class_name) = self.op_class_index.get(op_name) {
                // Resolve through the substitution: match-arm bindings get fresh
                // vars that unify with the signature's type vars, so the raw
                // lt/rt may be Var(n) where n != ctor_var, but subst.apply(lt)
                // yields Var(ctor_var).
                let lt_res = self.subst.apply(lt);
                let rt_res = rt.map(|r| self.subst.apply(r));
                let covered = self.active_constraints.iter().any(|c| {
                    c.class_name == *class_name
                        && {
                            // Resolve the constraint's var through the substitution:
                            // ctor instantiation may have unified the signature's
                            // type var (ctor_var) with a fresh var, so both sides
                            // must be in canonical (post-subst) form to compare.
                            let ctor_res = self.subst.apply(&Ty::Var(c.ctor_var));
                            matches!((&lt_res, &ctor_res), (Ty::Var(a), Ty::Var(b)) if a == b)
                                || rt_res.as_ref().is_some_and(|r| {
                                    matches!((r, &ctor_res), (Ty::Var(a), Ty::Var(b)) if a == b)
                                })
                        }
                });
                if covered {
                    // Unify both sides to a shared var (type equality), mirroring
                    // the native `==` path; the constraint guarantees comparability.
                    // On unification failure fall through to native semantics.
                    if let Some(rty) = rt {
                        if unify(&mut self.subst, lt, rty, span).is_err() {
                            return None;
                        }
                    }
                    return Some(Ty::bool());
                }
            }
            return None;
        }

        // Look up the class for this operator symbol.
        let class_name = match self.op_class_index.get(op_name) {
            Some(c) => c,
            None => return None,
        };

        // Find a matching instance whose type unifies with the operand type.
        let instances: Vec<_> = match self.class_instances.get(class_name) {
            Some(v) => v.to_vec(),
            None => return None,
        };
        for (inst_ty, impl_name) in &instances {
            // Scratch-unify the instance type against the left operand.
            let mut scratch = Subst::new();
            if unify(&mut scratch, inst_ty, lt, Span::empty(0)).is_err() {
                continue;
            }

            // Look up the impl function's scheme (keyed by param count): the
            // cross-module op-impl table first — synthetic `__op_*` names are not
            // source exports, so a dependent module's import env lacks them —
            // then this module's own env (single-program path).
            let key = (impl_name.clone(), arity);
            let scheme = match self.op_impl_env.get(&key).or_else(|| self.env.get(&key)) {
                Some(s) => s.clone(),
                None => return None,
            };
            let ps = match &scheme.body {
                Ty::Fun(ps, _) => ps.clone(),
                _ => continue,
            };
            if ps.len() != arity {
                continue;
            }

            // Commit: instantiate and unify with live operands.
            let start = self.next_var;
            let inst = self.instantiate(&scheme);
            let (ips, ir) = match &inst {
                Ty::Fun(ps, r) => (ps.clone(), (*r).clone()),
                _ => continue,
            };
            if ips.len() != arity {
                for v in start..self.next_var {
                    self.expected_free.remove(&v);
                }
                continue;
            }
            let saved_subst = self.subst.clone();
            let ok = unify(&mut self.subst, &ips[0], lt, span).is_ok()
                && rt.is_some_and(|rty| unify(&mut self.subst, &ips[1], rty, span).is_ok());
            if !ok {
                self.subst = saved_subst;
                for v in start..self.next_var {
                    self.expected_free.remove(&v);
                }
                continue;
            }

            self.op_dispatch.insert(span, impl_name.clone());
            return Some(self.subst.apply(&ir));
        }
        None
    }

    /// Monad do-notation dispatch (§6.5): resolve `__monad_bind` / `__monad_pure`
    /// to the matching `Monad` instance's method via D074 op_dispatch rewrite.
    /// Lambda arguments are handled inline (not lifted) so the param type is
    /// known from the monadic value's inner type at inference time.
    fn dispatch_monad(
        &mut self,
        name: &str,
        args: &[Expr],
        span: Span,
    ) -> Result<Ty, TypeError> {
        if name == "__monad_bind" {
            self.dispatch_monad_bind(args, span)
        } else {
            self.dispatch_monad_pure(args, span)
        }
    }

    /// `__monad_bind e k`: infer the monadic value, extract its inner type,
    /// then handle the continuation (inline lambda or function reference).
    fn dispatch_monad_bind(
        &mut self,
        args: &[Expr],
        span: Span,
    ) -> Result<Ty, TypeError> {
        let (monad_expr, cont) = match args {
            [m, k] => (m, k),
            _ => {
                return Err(TypeError::new(
                    Stage::Inference,
                    span,
                    "internal: __monad_bind expects 2 arguments",
                ));
            }
        };

        // Infer the monadic value and extract its type constructor + inner type.
        let m_ty = self.infer_expr(monad_expr)?;
        let m_ty = self.subst.apply(&m_ty);
        let (ctor_name, inner_ty) = match &m_ty {
            Ty::App(n, args) if !args.is_empty() => (n.clone(), args[0].clone()),
            Ty::Con(n) => (n.clone(), self.fresh_with(OriginInfo::new("monad inner type", span))),
            _ => {
                return Err(TypeError::new(
                    Stage::Inference,
                    span,
                    format!(
                        "cannot dispatch `__monad_bind`: argument type `{}` is not a concrete monadic type",
                        pretty_ty(&m_ty, &self.subst)
                    ),
                ));
            }
        };

        // Look up the bind impl scheme.
        let impl_name = format!("__bind_Monad_{}", ctor_name);
        let key = (impl_name.clone(), 2usize);
        let scheme = match self.op_impl_env.get(&key).or_else(|| self.env.get(&key)) {
            Some(s) => s.clone(),
            None => {
                return Err(TypeError::new(
                    Stage::Inference,
                    span,
                    format!("no `Monad` instance for `{}`", ctor_name),
                ));
            }
        };

        // If the continuation is an inline lambda, bind its param with the
        // inner type and infer the body directly.
        if let Expr::Lambda { params, body, .. } = cont {
            if params.len() != 1 {
                return Err(TypeError::new(
                    Stage::Inference,
                    span,
                    "internal: do-notation lambda expects exactly 1 param",
                ));
            }
            let x = &params[0];
            let old = self.env.get(&(x.clone(), 0)).cloned();
            self.env.insert((x.clone(), 0), Scheme::mono(inner_ty.clone()));
            let body_ty = self.infer_expr(body)?;
            match old {
                Some(s) => { self.env.insert((x.clone(), 0), s); }
                None => { self.env.remove(&(x.clone(), 0)); }
            }
            // Unify the body type with the expected monadic result.
            let inst = self.instantiate(&scheme);
            if let Ty::Fun(_, r) = &inst {
                let expected = self.subst.apply(r);
                unify(&mut self.subst, &expected, &body_ty, span)?;
            }
            self.op_dispatch.insert(span, impl_name);
            return Ok(self.subst.apply(&body_ty));
        }

        // Continuation is a function reference: infer it and unify.
        let k_ty = self.infer_expr(cont)?;
        let inst = self.instantiate(&scheme);
        let (ps, r) = match &inst {
            Ty::Fun(ps, r) => (ps.clone(), (*r).clone()),
            _ => {
                return Err(TypeError::new(
                    Stage::Inference,
                    span,
                    "internal: Monad bind impl is not a function type",
                ));
            }
        };
        let arg_tys = vec![m_ty, k_ty];
        for (p, a) in ps.iter().zip(arg_tys.iter()) {
            let p = self.subst.apply(p);
            let a = self.subst.apply(a);
            unify(&mut self.subst, &p, &a, span)?;
        }
        self.op_dispatch.insert(span, impl_name);
        Ok(self.subst.apply(&r))
    }

    /// `__monad_pure v`: infer the value, find the Monad instance, return M A.
    fn dispatch_monad_pure(
        &mut self,
        args: &[Expr],
        span: Span,
    ) -> Result<Ty, TypeError> {
        let val = match args {
            [v] => v,
            _ => {
                return Err(TypeError::new(
                    Stage::Inference,
                    span,
                    "internal: __monad_pure expects 1 argument",
                ));
            }
        };

        // Scan op_impl_env for `__pure_Monad_*` entries to find the instance.
        let prefix = "__pure_Monad_";
        let matches: Vec<&String> = self
            .op_impl_env
            .keys()
            .filter(|(n, a)| n.starts_with(prefix) && *a == 1)
            .map(|(n, _)| n)
            .collect();
        let ctor_name = match matches.len() {
            0 => {
                return Err(TypeError::new(
                    Stage::Inference,
                    span,
                    "no `Monad` instances declared",
                ));
            }
            1 => matches[0][prefix.len()..].to_string(),
            _ => {
                return Err(TypeError::new(
                    Stage::Inference,
                    span,
                    "ambiguous `__monad_pure`: multiple `Monad` instances; add a type annotation to disambiguate",
                ));
            }
        };

        let impl_name = format!("__pure_Monad_{}", ctor_name);
        let key = (impl_name.clone(), 1usize);
        let scheme = match self.op_impl_env.get(&key).or_else(|| self.env.get(&key)) {
            Some(s) => s.clone(),
            None => {
                return Err(TypeError::new(
                    Stage::Inference,
                    span,
                    format!("no `Monad` instance for `{}`", ctor_name),
                ));
            }
        };

        let v_ty = self.infer_expr(val)?;
        let inst = self.instantiate(&scheme);
        let (ps, r) = match &inst {
            Ty::Fun(ps, r) => (ps.clone(), (*r).clone()),
            _ => {
                return Err(TypeError::new(
                    Stage::Inference,
                    span,
                    "internal: Monad pure impl is not a function type",
                ));
            }
        };
        for (p, a) in ps.iter().zip(std::iter::once(&v_ty)) {
            let p = self.subst.apply(p);
            let a = self.subst.apply(a);
            unify(&mut self.subst, &p, &a, span)?;
        }
        self.op_dispatch.insert(span, impl_name);
        Ok(self.subst.apply(&r))
    }

    /// Infer an expression's type and record it in the side table
    /// (`expr_types`) keyed by `(current item, node span)`, so downstream
    /// ownership checks (`wag-check` linearity) consume the inferred
    /// type instead of re-deriving it heuristically.
    pub(super) fn infer_expr(&mut self, expr: &Expr) -> Result<Ty, TypeError> {
        let ty = self.infer_expr_node(expr)?;
        if let Some(item) = &self.cur_item {
            // Field access yields a borrowed view of the field (§3.3/§4.3.3):
            // record `&T` for non-pointer-free results so ownership checks treat
            // the result as borrow (multiple reads, no escape). Unification above
            // still sees the bare `T`, so binding a field result stays legal.
            // Method dispatch (D056) is a call with an owned result — never a
            // borrow view.
            let applied = self.subst.apply(&ty);
            let rec = match expr {
                Expr::Field { base, field, .. } => {
                    if self.is_method_dispatch(base, field) {
                        // Method dispatch (D056): owned call result.
                        applied
                    } else if matches!(base.as_ref(), Expr::Self_ { .. })
                        && self.method_receiver.is_some()
                    {
                        // Self-peek (D060): recorded exactly like the old
                        // bare-atom fallback — links stay non-values, no borrow view.
                        applied
                    } else if !is_likely_pointer_free(&applied) {
                        Ty::Borrow(Box::new(applied))
                    } else {
                        applied
                    }
                }
                _ => applied,
            };
            self.expr_types.insert((item.clone(), expr.span()), rec);
        }
        Ok(ty)
    }

    pub(super) fn infer_expr_node(&mut self, expr: &Expr) -> Result<Ty, TypeError> {
        match expr {
            Expr::Int { .. } => Ok(Ty::int()),
            Expr::Unit { .. } => Ok(Ty::unit()),
            // Expand `String` through any user-defined alias so the literal
            // agrees with declaration-site expansion (§3.2 transparency).
            Expr::Str { .. } => surface_to_ty_resolved(
                &wag_syntax::Type::Name("String".into()),
                &self.aliases,
            ),
            Expr::Var { name, span } => {
                if name == "_" {
                    // Defensive: a `_`-named binding is an unconstrained discard.
                    // (Statement sequencing now uses `Seq`; this guards a stray `_`.)
                    let ty = self.fresh_with(OriginInfo::new("discard binding `_`", *span));
                    if let Ty::Var(v) = &ty {
                        self.expected_free.insert(*v);
                    }
                    return Ok(ty);
                }
                // D060: bare atoms in method bodies are desugared to
                // `Self_.field` before inference — no receiver fallback here.
                self.lookup(name, *span)
            }
            Expr::Upper { name, span } => self.lookup(name, *span),
            Expr::AtName { name, span } => {
                // D078: system actors → handle (arity 0); user actors → creation fn (arity 1).
                self.lookup_actor(name, *span)
            }
            Expr::Self_ { span } => {
                if let Some(t) = &self.actor_self {
                    return Ok(t.clone());
                }
                // Method receiver (D057/D061): `self` denotes the handle itself —
                // `App(name, args)` for parametric receivers, bare `Con` otherwise.
                if let Some((recv, args)) = &self.method_receiver {
                    return Ok(if args.is_empty() {
                        Ty::Con(recv.clone())
                    } else {
                        Ty::App(
                            recv.clone(),
                            args.iter().map(|v| Ty::Var(*v)).collect(),
                        )
                    });
                }
                Err(TypeError::new(
                    Stage::Inference,
                    *span,
                    "`self` is only valid inside an @-declaration body or a method body",
                ))
            }
            Expr::Apply { fun, args, span } => self.infer_apply(fun, args, *span),
            Expr::Send { target, msg, span } => {
                let target_ty = self.infer_expr(target)?;
                let msg_ty = self.infer_expr(msg)?;
                let t_var = self.fresh_with(OriginInfo::new("actor message type in send", *span));
                unify(&mut self.subst, &target_ty, &Ty::actor(t_var.clone()), *span).map_err(|_| {
                    TypeError::new(
                        Stage::Inference,
                        *span,
                        format!(
                            "send target must be Actor T, found `{}`",
                            pretty_ty(&self.subst.apply(&target_ty), &self.subst)
                        ),
                    )
                })?;
                let resolved_msg = self.subst.apply(&t_var);
                unify(&mut self.subst, &msg_ty, &resolved_msg, msg.span())?;
                Ok(Ty::unit())
            }
            Expr::BinOp {
                op,
                lhs,
                rhs,
                span,
            } => {
                let lt = self.infer_expr(lhs)?;
                let rt = self.infer_expr(rhs)?;
                // Static dispatch (D074): consult the overload table first.
                // `!` is unary — its AST `rhs` is a dummy, excluded from matching.
                if let Some(t) = self.dispatch_binop(
                    binop_name(op),
                    if matches!(op, BinOp::BitNot) { 1 } else { 2 },
                    &lt,
                    if matches!(op, BinOp::BitNot) { None } else { Some(&rt) },
                    *span,
                ) {
                    return Ok(t);
                }
                match op {
                    // `+` is Int-only (D073): string concatenation is the prelude's
                    // `stringConcat`; a proper overloading mechanism is an open item (§9).
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => {
                        unify(&mut self.subst, &lt, &Ty::int(), lhs.span())?;
                        unify(&mut self.subst, &rt, &Ty::int(), rhs.span())?;
                        Ok(Ty::int())
                    }
                    BinOp::BitAnd | BinOp::BitOr => {
                        // Overloaded: Int (bitwise) or Bool (logical).
                        let fresh = self
                            .fresh_with(OriginInfo::new(format!("result of `{op:?}`"), lhs.span()));
                        unify(&mut self.subst, &lt, &fresh, lhs.span())?;
                        unify(&mut self.subst, &rt, &fresh, rhs.span())?;
                        let t = self.subst.apply(&fresh);
                        match &t {
                            Ty::Con(n) if n == "Int" || n == "Bool" => Ok(t),
                            _ => Err(TypeError::new(
                                Stage::Inference,
                                lhs.span(),
                                format!(
                                    "`{op:?}` requires both operands to be `Int` or `Bool`, got {t}"
                                ),
                            )),
                        }
                    }
                    BinOp::BitXor => {
                        unify(&mut self.subst, &lt, &Ty::int(), lhs.span())?;
                        unify(&mut self.subst, &rt, &Ty::int(), rhs.span())?;
                        Ok(Ty::int())
                    }
                    BinOp::BitNot => {
                        // Bool-only (D074): `!` is logical NOT; use `bitNot` for Int.
                        let fresh = self
                            .fresh_with(OriginInfo::new(format!("result of `{op:?}"), lhs.span()));
                        unify(&mut self.subst, &lt, &fresh, lhs.span())?;
                        let t = self.subst.apply(&fresh);
                        match &t {
                            Ty::Con(n) if n == "Bool" => Ok(t),
                            _ => Err(TypeError::new(
                                Stage::Inference,
                                lhs.span(),
                                format!(
                                    "`!` requires a `Bool` operand (use `bitNot` for Int), got {t}"
                                ),
                            )),
                        }
                    }
                    BinOp::ShiftLeft | BinOp::ShiftRight => {
                        unify(&mut self.subst, &lt, &Ty::int(), lhs.span())?;
                        unify(&mut self.subst, &rt, &Ty::int(), rhs.span())?;
                        Ok(Ty::int())
                    }
                    BinOp::Gt | BinOp::Lt | BinOp::Ge | BinOp::Le => {
                        unify(&mut self.subst, &lt, &Ty::int(), lhs.span())?;
                        unify(&mut self.subst, &rt, &Ty::int(), rhs.span())?;
                        Ok(Ty::bool())
                    }
                    // ==/!= : comparable types (Int, Bool, Unit, String, Char), both sides equal.
                    BinOp::Eq | BinOp::Ne => {
                        let fresh = self
                            .fresh_with(OriginInfo::new(format!("result of `{op:?}"), lhs.span()));
                        unify(&mut self.subst, &lt, &fresh, lhs.span())?;
                        unify(&mut self.subst, &rt, &fresh, rhs.span())?;
                        let t = self.subst.apply(&fresh);
                        if !is_comparable(&t) {
                            return Err(TypeError::new(
                                Stage::Inference,
                                lhs.span(),
                                format!(
                                    "`{op:?}` requires comparable operands (Int, Bool, Unit, String, Char, Vec T, Option T), got {}",
                                    pretty_ty(&t, &self.subst)
                                ),
                            ));
                        }
                        Ok(Ty::bool())
                    }
                }
            }
            Expr::Field { base, field, span } => {
                let bt = self.infer_expr(base)?;
                // Method dispatch (D056): declaration-driven — if `field` is a
                // declared method on the receiver type this is a method call.
                match self.method_ref_type(&bt, field, *span)? {
                    // Scheme shape is `Fun(recv :: ps, r)` (D061): unify the
                    // receiver position against the actual base type.
                    Some(Ty::Fun(ps, r)) => {
                        let (recv_ty, rest) = match ps.split_first() {
                            Some((r0, rest)) => (r0, rest),
                            // Unreachable: every seeded scheme carries the receiver.
                            None => return Ok(Ty::Fun(vec![], r.clone())),
                        };
                        unify(&mut self.subst, &bt, recv_ty, *span)?;
                        if rest.is_empty() {
                            // Nullary methods are applied by bare read (D057):
                            // `d.next` is the call itself.
                            return Ok(self.subst.apply(&r));
                        }
                        return Ok(Ty::Fun(rest.to_vec(), r.clone()));
                    }
                    Some(ty) => return Ok(ty),
                    None => {}
                }
                self.field_type(&bt, field, *span)
            }
            Expr::RecordUpdate {
                receiver,
                clauses,
                span,
            } => {
                let rt = self.infer_expr(receiver)?;
                let owned = rt.strip_borrow().clone();
                // Receiver of update must be owned (not borrow) ultimately.
                let recv = self.subst.apply(&rt);
                if matches!(recv, Ty::Borrow(_)) {
                    return Err(TypeError::new(
                        Stage::Inference,
                        *span,
                        "record update requires an owned receiver",
                    ));
                }
                // Write statement in a method body (D056/D057): `path <- expr`
                // folds to RecordUpdate(Self_, …). It is an effect, not a
                // value — Unit-typed; the clauses are still validated.
                let is_write_stmt = matches!(receiver.as_ref(), Expr::Self_ { .. })
                    && self.method_receiver.is_some();
                self.check_update_clauses(&owned, clauses)?;
                if is_write_stmt {
                    Ok(Ty::unit())
                } else {
                    Ok(owned)
                }
            }
            Expr::SetField {
                receiver,
                field_idx,
                value,
                span,
            } => {
                let rt = self.infer_expr(receiver)?;
                let owned = rt.strip_borrow().clone();
                let recv = self.subst.apply(&rt);
                if matches!(recv, Ty::Borrow(_)) {
                    return Err(TypeError::new(
                        Stage::Inference,
                        *span,
                        "set-field requires an owned receiver",
                    ));
                }
                let vt = self.infer_expr(value)?;
                // Tightened (A1b): unify the value against the bound field type
                // instead of discarding it. SetField is checker-emitted for
                // single-ctor receivers only (§3.3.1); a Var receiver defers
                // like `field_type` (Option C) — nothing to bind yet, and
                // multi-ctor shapes are unreachable (the checker rejects them).
                let base = deref_ref(&recv);
                if let Ty::Con(n) | Ty::App(n, _) = &base {
                    let ctor_names = self.type_ctors.get(n).cloned().unwrap_or_default();
                    if let [cn] = &ctor_names[..] {
                        if let Some(info) = self.ctors.get(cn).cloned() {
                            let (field_tys, _) =
                                self.bind_ctor_scheme(&info.scheme, Some(&base), *span)?;
                            let ft = field_tys.get(*field_idx).ok_or_else(|| {
                                TypeError::new(
                                    Stage::Inference,
                                    *span,
                                    format!(
                                        "internal: ctor `{cn}` has no inferred type for field index {field_idx}"
                                    ),
                                )
                            })?;
                            unify(&mut self.subst, &vt, ft, *span)?;
                        }
                    }
                }
                Ok(owned)
            }
            Expr::RegionWrite {
                receiver, value, ..
            } => {
                // Checker-emitted; a write statement has no value.
                let rt = self.infer_expr(receiver)?;
                if matches!(self.subst.apply(&rt), Ty::Borrow(_)) {
                    return Err(TypeError::new(
                        Stage::Inference,
                        receiver.span(),
                        "region-write requires an owned receiver",
                    ));
                }
                self.infer_expr(value)?;
                Ok(Ty::unit())
            }
            Expr::Borrow { inner, span: _ } => {
                let t = self.infer_expr(inner)?;
                let t = self.subst.apply(&t);
                // &(&T) is not introduced; borrowing owned T yields &T.
                // If already &T, keep &T (idempotent at call sites).
                match t {
                    Ty::Borrow(_) => Ok(t),
                    other => Ok(Ty::Borrow(Box::new(other))),
                }
            }
            Expr::Bind {
                name,
                value,
                body,
                span,
            } => {
                let vt = self.infer_expr(value)?;
                // let cannot bind a borrow (§3.2).
                let vt = self.subst.apply(&vt);
                if matches!(vt, Ty::Borrow(_)) {
                    return Err(TypeError::new(
                        Stage::Inference,
                        *span,
                        format!(
                            "`{name}` cannot bind a borrowed type `{}`",
                            pretty_ty(&vt, &self.subst)
                        ),
                    )
                    .with_context(
                        "a `let` binding must own its value; the assigned expression has type \
                             `&T`. Assign an owned value, or use the borrow directly instead of \
                             binding it."
                            .to_string(),
                    ));
                }
                let old = self.env.remove(&(name.clone(), 0));
                self.env.insert((name.clone(), 0), Scheme::mono(vt));
                let bt = self.infer_expr(body)?;
                match old {
                    Some(s) => {
                        self.env.insert((name.clone(), 0), s);
                    }
                    None => {
                        self.env.remove(&(name.clone(), 0));
                    }
                }
                Ok(bt)
            }
            Expr::Seq { first, second, .. } => {
                // Evaluate `first` for its effect, discard its value; `second`
                // is the sequence's value. No binding is introduced.
                let _ = self.infer_expr(first)?;
                self.infer_expr(second)
            }
            Expr::If { arms, span } => {
                if arms.is_empty() {
                    return Err(TypeError::new(
                        Stage::Inference,
                        *span,
                        "if requires at least one arm",
                    ));
                }
                // Trailing `_` required (§3.4) — presence check.
                let has_catch = arms.iter().any(|a| a.condition.is_none());
                if !has_catch {
                    return Err(TypeError::new(
                        Stage::Inference,
                        *span,
                        "if requires a trailing `_` catch-all arm",
                    ));
                }
                let mut acc: Option<Ty> = None;
                for arm in arms {
                    if let Some(c) = &arm.condition {
                        let ct = self.infer_expr(c)?;
                        unify(&mut self.subst, &ct, &Ty::bool(), c.span())?;
                    }
                    let bt = self.infer_expr(&arm.body)?;
                    acc = Some(match acc {
                        None => bt,
                        Some(prev) => unify_arm_join(&mut self.subst, &prev, &bt, *span)?,
                    });
                }
                Ok(acc.unwrap())
            }
            Expr::Match {
                scrutinee,
                arms,
                span,
            } => {
                let st = self.infer_expr(scrutinee)?;
                let mut st = self.subst.apply(&st);
                // Auto-deref through Ref T for pattern matching.
                if let Ty::App(name, ref args) = &st {
                    if name == "Ref" && args.len() == 1 {
                        st = args[0].clone();
                    }
                }
                let mut acc: Option<Ty> = None;
                for arm in arms {
                    let saved = self.bind_pattern(&arm.pattern, &st, Some(scrutinee.as_ref()))?;
                    let bt = self.infer_expr(&arm.body)?;
                    self.restore(saved);
                    acc = Some(match acc {
                        None => bt,
                        Some(prev) => unify_arm_join(&mut self.subst, &prev, &bt, *span)?,
                    });
                }
                if arms.is_empty() {
                    return Err(TypeError::new(
                        Stage::Inference,
                        *span,
                        "match requires at least one arm",
                    ));
                }
                // Record scrutinee type AFTER arms (type vars resolved by unification).
                // Only concrete Con/App scrutinees are recorded; keyed by the full span
                // so prelude/user matches with overlapping start offsets never collide.
                let resolved = self.subst.apply(&st);
                if let Ty::Con(name) = &resolved {
                    self.match_types.insert(*span, name.clone());
                } else if let Ty::App(name, _) = &resolved {
                    self.match_types.insert(*span, name.clone());
                }
                Ok(acc.unwrap())
            }
            Expr::Do { span, .. } => Err(TypeError::new(
                Stage::Inference,
                *span,
                "internal: Do should have been desugared before typing",
            )),
            Expr::ListLit { span, elems } => {
                // `[e1..en] : List T` (item-8 cutover): `List` is an ordinary
                // prelude-declared type; all elements must unify to one T.
                let mut elem_ty = None;
                for e in elems {
                    let t = self.infer_expr(e)?;
                    match &mut elem_ty {
                        None => elem_ty = Some(t),
                        Some(acc) => unify(&mut self.subst, acc, &t, e.span())?,
                    }
                }
                let t = elem_ty.unwrap_or_else(|| {
                    self.fresh_with(OriginInfo::new("element type of empty list literal", *span))
                });
                Ok(Ty::App(
                    "List".into(),
                    vec![self.subst.apply(&t)],
                ))
            },
            Expr::Lambda { span, .. } => Err(TypeError::new(
                Stage::Inference,
                *span,
                "internal: Lambda should have been lifted before typing",
            )),
            Expr::Comprehension { span, .. } => Err(TypeError::new(
                Stage::Inference,
                *span,
                "internal: Comprehension should have been desugared before typing",
            )),
            Expr::Product { elems, .. } => {
                let mut tys = Vec::with_capacity(elems.len());
                for e in elems {
                    tys.push(self.infer_expr(e)?);
                }
                Ok(Ty::App("Pair".into(), tys))
            }
        }
    }

    pub(super) fn infer_apply(
        &mut self,
        fun: &Expr,
        args: &[Expr],
        span: Span,
    ) -> Result<Ty, TypeError> {
        if let Expr::Field { .. } = fun {
            // Method dispatch by application (D056): `h.append 20` — the last
            // field-chain segment names a declared method on the receiver type.
            if let Some(ty) = self.infer_method_call(fun, args, span)? {
                return Ok(ty);
            }
            // Field mutation by application (§3.3.1): `a.f v` where f is a field
            // (not a function) — the 1-arg setter overload. Returns the struct type.
            if args.len() == 1 {
                return self.infer_field_setter(fun, &args[0], span);
            }
        }

        // Reply-to fields on IO request ctors accept Actor handles.
        // `RequestLine` is single-arg; `RequestConnection`/`RequestData`
        // carry an Int (port / token) plus the reply target (spec §5.5.7).
        if let Expr::Upper { name, .. } = fun {
            let (reply_pos, result) = match (name.as_str(), args.len()) {
                ("RequestLine", 1) => (0, Ty::Con("StdMsg".into())),
                ("RequestConnection", 2) => (1, Ty::Con("NetMsg".into())),
                ("RequestData", 2) => (1, Ty::Con("NetMsg".into())),
                _ => (usize::MAX, Ty::unit()),
            };
            if reply_pos != usize::MAX {
                let arg_tys: Result<Vec<Ty>, _> = args
                    .iter()
                    .map(|a| {
                        let t = self.infer_expr(a)?;
                        Ok(self.subst.apply(&t))
                    })
                    .collect();
                let arg_tys = arg_tys?;
                // Non-reply fields are Int (port / token).
                for (i, t) in arg_tys.iter().enumerate() {
                    if i != reply_pos {
                        unify(&mut self.subst, t, &Ty::int(), span)?;
                    }
                }
                let t0 = arg_tys[reply_pos].clone();
                let msg = self.fresh_with(OriginInfo::new(
                    format!("reply target type in `{name}`"),
                    span,
                ));
                let as_actor = Ty::actor(msg);
                unify(&mut self.subst, &t0, &as_actor, span).map_err(|_| {
                    TypeError::new(
                        Stage::Inference,
                        span,
                        format!("`{name}` expects Actor reply-to, found `{t0}`"),
                    )
                })?;
                return Ok(result);
            }
        }

        // Monad do-notation dispatch (§6.5): `__monad_bind` / `__monad_pure`
        // are resolved to the matching `Monad` instance's methods via D074
        // op_dispatch rewrite at load time. Handled before arg inference because
        // lambda arguments are kept inline (not lifted) and need special handling.
        if let Expr::Upper { name, .. } = fun {
            if name == "__monad_bind" || name == "__monad_pure" {
                return self.dispatch_monad(name, args, span);
            }
        }

        // Infer argument types first (needed for both overload dispatch and
        // normal param/arg unification below).
        let arg_tys: Result<Vec<_>, _> = args.iter().map(|a| self.infer_expr(a)).collect();
        let arg_tys = arg_tys?;

        // D051: for bare Var callees, try arity-specific lookup so overloaded
        // names resolve to the correct overload. Local bindings (vars/params,
        // keyed `(name, 0)`) shadow top-level overloads — check those first.
        // The `lookup_arity` path bypasses `infer_expr(fun)` (no side-table
        // recording there), so capture the raw scheme type and record it under
        // the callee span after unification below (§3.2, work item A).
        let callee_raw: Option<Ty> = if let Expr::Var { name, .. } = fun {
            if self.env.contains_key(&(name.clone(), 0)) {
                None
            } else {
                self.lookup_arity(name, args.len(), span).ok()
            }
        } else {
            None
        };
        let ft = if let Some(raw) = &callee_raw {
            raw.clone()
        } else {
            self.infer_expr(fun)?
        };

        // Automatic borrow at call sites: if param is &T and arg is T, wrap.
        let ft = self.subst.apply(&ft);
        let result = match ft {
            Ty::Fun(params, ret) => {
                let fun_name = match fun {
                    Expr::Var { name, .. } => name.clone(),
                    Expr::Upper { name, .. } => name.clone(),
                    _ => "?".into(),
                };
                if arg_tys.len() > params.len() {
                    let expected: Vec<String> =
                        params.iter().map(|p| pretty_ty(p, &self.subst)).collect();
                    let actual: Vec<String> =
                        arg_tys.iter().map(|a| pretty_ty(a, &self.subst)).collect();
                    return Err(TypeError::new(
                        Stage::Inference,
                        span,
                        format!(
                            "`{fun_name}`: too many arguments\n  expected: ({})\n  found:    ({})",
                            expected.join(", "),
                            actual.join(", ")
                        ),
                    ));
                }
                // Unify the provided args with the leading params. Under-
                // application (arg_tys.len() < params.len()) is allowed: it
                // yields a partially-applied function value (currying),
                // matching the VM's `Value::Closure { fn_id, pre_args }`.
                let sig: Vec<String> = params.iter().map(|p| pretty_ty(p, &self.subst)).collect();
                let n = arg_tys.len();
                for (i, (p, a)) in params.iter().take(n).zip(arg_tys.iter()).enumerate() {
                    let p = self.subst.apply(p);
                    let a = self.subst.apply(a);
                    let res = match (&p, &a) {
                        (Ty::Borrow(inner), arg) if !matches!(arg, Ty::Borrow(_)) => {
                            // Auto-borrow owned to &T.
                            unify(&mut self.subst, inner, arg, span)
                        }
                        (Ty::Borrow(inner), Ty::Borrow(arg_inner)) => {
                            unify(&mut self.subst, inner, arg_inner, span)
                        }
                        (Ty::App(name, args), arg)
                            if {
                                name == "Ref"
                                    && args.len() == 1
                                    && !matches!(arg, Ty::App(n2, _) if n2 == "Ref")
                            } =>
                        {
                            // Auto-ref: storing an owned T into a Ref T field
                            // records the address (no explicit `ref` op needed).
                            unify(&mut self.subst, &args[0], arg, span)
                        }
                        _ => unify(&mut self.subst, &p, &a, span),
                    };
                    if let Err(e) = res {
                        return Err(TypeError::new(
                            Stage::Inference,
                            e.span,
                            format!(
                                "`{fun_name}`: argument {i} type mismatch\n  {}({}) -> {}\n  arg {i}: expected `{}`, found `{}`",
                                fun_name,
                                sig.join(", "),
                                pretty_ty(&self.subst.apply(&ret), &self.subst),
                                pretty_ty(&p, &self.subst),
                                pretty_ty(&a, &self.subst)
                            ),
                        ));
                    }
                }
                // Fully applied → result type. Partially applied → the
                // remaining (curried) function type.
                if n == params.len() {
                    self.subst.apply(&ret)
                } else {
                    let remaining: Vec<Ty> = params.iter().skip(n).cloned().collect();
                    Ty::Fun(remaining, ret)
                }
            }
            other => {
                // Could still be a metavariable — unify with function type.
                // The result hole may stay free (e.g. unannotated recursive
                // self-calls) and get generalized — record it (A1b).
                let ret = self.fresh_with(OriginInfo::new("result of the call", span));
                if let Ty::Var(v) = &ret {
                    self.expected_free.insert(*v);
                }
                let expected = Ty::Fun(arg_tys.clone(), Box::new(ret.clone()));
                unify(&mut self.subst, &other, &expected, span)?;
                self.subst.apply(&ret)
            }
        };
        // Record the resolved callee type for downstream ownership checks.
        if let (Some(raw), Some(item)) = (&callee_raw, &self.cur_item) {
            self.expr_types
                .insert((item.clone(), fun.span()), self.subst.apply(raw));
        }
        Ok(result)
    }

    /// Method dispatch by application (D056): `h.append 20`. When the last
    /// segment of a field chain names a declared method on the receiver type,
    /// infer it as a call and return its result type. Returns None when no such
    /// method exists so the caller can fall back to setter resolution.
    fn infer_method_call(
        &mut self,
        fun: &Expr,
        args: &[Expr],
        span: Span,
    ) -> Result<Option<Ty>, TypeError> {
        // Decompose the field chain: Field(Field(Var(h), a), b) → (Var(h), ["a", "b"]).
        let mut path = Vec::new();
        let mut cur = fun;
        while let Expr::Field { base, field, .. } = cur {
            path.push(field.clone());
            cur = base.as_ref();
        }
        if path.is_empty() {
            return Ok(None);
        }
        let name = path.last().unwrap().clone();
        let root_ty = self.infer_expr(cur)?;
        match self.method_ref_type(&root_ty, &name, span)? {
            Some(Ty::Fun(ps, r)) => {
                // Scheme shape is `Fun(recv :: ps, r)` (D061): unify the receiver
                // position first, then match args against the remaining params.
                let (recv_ty, rest) = ps.split_first().ok_or_else(|| {
                    TypeError::new(
                        Stage::Inference,
                        span,
                        format!("method `{name}` has a malformed scheme (no receiver parameter)"),
                    )
                })?;
                unify(&mut self.subst, &root_ty, recv_ty, span)?;
                if rest.len() != args.len() {
                    return Err(TypeError::new(
                        Stage::Inference,
                        span,
                        format!(
                            "method `{name}` expects {} argument(s), got {}",
                            rest.len(),
                            args.len()
                        ),
                    ));
                }
                for (a, p) in args.iter().zip(rest) {
                    let at = self.infer_expr(a)?;
                    unify(&mut self.subst, &at, p, a.span())?;
                }
                Ok(Some(self.subst.apply(&r)))
            }
            Some(_) => Err(TypeError::new(
                Stage::Inference,
                span,
                format!("method `{name}` does not have a function type"),
            )),
            None => Ok(None),
        }
    }

    /// Field mutation by application (§3.3.1): `a.f v` or nested `a.b.c v`.
    /// Decomposes the field chain, infers root type, walks path to target field,
    /// unifies arg with field type, returns root struct type (setter result).
    pub(super) fn infer_field_setter(
        &mut self,
        fun: &Expr,
        arg: &Expr,
        span: Span,
    ) -> Result<Ty, TypeError> {
        // Decompose the field chain: Field(Field(Var(p), a), b) → (Var(p), ["a", "b"])
        let mut path = Vec::new();
        let mut cur = fun;
        while let Expr::Field { base, field, .. } = cur {
            path.push(field.clone());
            cur = base.as_ref();
        }
        path.reverse();

        if path.is_empty() {
            return Err(TypeError::new(
                Stage::Inference,
                span,
                "field mutation requires at least one field in the path",
            ));
        }

        // Infer the root expression type → struct type S.
        let root_ty = self.infer_expr(cur)?;
        let root_owned = root_ty.strip_borrow().clone();

        // Walk the field path to find the target field's type.
        let mut ty = root_ty.clone();
        for (i, seg) in path.iter().enumerate() {
            let is_last = i == path.len() - 1;
            ty = self.field_type(&ty, seg, span)?;
            if !is_last {
                // Intermediate segments must be struct/Ref types we can walk into.
                // field_type already auto-derefs Ref; nothing extra needed here.
            }
        }

        // Unify the argument with the target field type.
        let arg_ty = self.infer_expr(arg)?;
        unify(&mut self.subst, &arg_ty, &ty, arg.span())?;

        // Setter returns the root struct type (the updated structure).
        Ok(self.subst.apply(&root_owned))
    }

    pub(super) fn field_type(
        &mut self,
        recv: &Ty,
        field: &str,
        span: Span,
    ) -> Result<Ty, TypeError> {
        let recv = self.subst.apply(recv);
        // Auto-deref through Ref T for field access.
        let base = deref_ref(&recv.strip_borrow().clone());
        // Option C (A1b step 6): receiver still a hole — defer resolution to
        // end of module, when the final substitution has settled it. A fresh
        // result hole is handed out now; re-resolution unifies it with the
        // field's actual type (or errors if the receiver never resolves).
        if matches!(base, Ty::Var(_)) {
            let ty = self.fresh_with(OriginInfo::new(
                format!("result of `.{field}` on unresolved receiver"),
                span,
            ));
            self.deferred_fields.push(super::context::DeferredField {
                span,
                recv: base.clone(),
                field: field.to_string(),
                result: ty.clone(),
            });
            return Ok(ty);
        }
        // Special: `.last` on `List T` → `List T` (§3.3.1).
        if field == "last" {
            if let Ty::App(name, args) = &base {
                if name == "List" {
                    return Ok(Ty::App(name.clone(), args.clone()));
                }
            }
            return Err(TypeError::new(
                Stage::Inference,
                span,
                format!(
                    "`.last` is only valid on `List T`, not `{}`",
                    pretty_ty(&base, &self.subst)
                ),
            ));
        }
        let type_name = match &base {
            Ty::Con(n) => n.clone(),
            Ty::App(n, _) => n.clone(),
            other => {
                return Err(TypeError::new(
                    Stage::Inference,
                    span,
                    format!(
                        "cannot project field `{field}` from `{}`",
                        pretty_ty(other, &self.subst)
                    ),
                ));
            }
        };
        self.resolve_field_on(&type_name, &base, field, span)
    }

    /// Resolve `field` on a concrete receiver type (`Con`/`App`) to its
    /// instantiated field type: look up the datatype's constructors, find the
    /// matching field label, and bind the ctor scheme against the actual
    /// receiver so parametric fields instantiate (§3.3). Shared by eager
    /// resolution in `field_type` and end-of-module re-resolution of deferred
    /// accesses (Option C, A1b step 6).
    pub(super) fn resolve_field_on(
        &mut self,
        type_name: &str,
        base: &Ty,
        field: &str,
        span: Span,
    ) -> Result<Ty, TypeError> {
        let ctor_names = self.type_ctors.get(type_name).cloned().unwrap_or_default();
        if ctor_names.is_empty() {
            return Err(TypeError::new(
                Stage::Inference,
                span,
                format!("unknown type `{type_name}` for field access"),
            ));
        }
        // Prefer single-constructor; if multi, still try label lookup (checker rejects multi).
        for cn in &ctor_names {
            let info = match self.ctors.get(cn).cloned() {
                Some(i) => i,
                None => continue,
            };
            for (i, f) in info.fields.iter().enumerate() {
                if f.label.as_deref() == Some(field) {
                    // Bind the scheme's fresh vars against the actual receiver so
                    // field types are instantiated: `.head` on `List Int` must be
                    // `Ref (LNode Int)`, not `Ref (LNode ?free)` — otherwise pattern
                    // vars stay free and arm joins silently absorb them (§3.6).
                    let (field_tys, _) =
                        self.bind_ctor_scheme(&info.scheme, Some(base), span)?;
                    // Ctor schemes are Fun-typed with one param per field; a
                    // missing entry is malformed registration — error rather than
                    // leak the raw declaration-space type.
                    return field_tys.get(i).cloned().ok_or_else(|| {
                        TypeError::new(
                            Stage::Inference,
                            span,
                            format!(
                                "internal: ctor `{cn}` has no inferred type for field `{field}`"
                            ),
                        )
                    });
                }
            }
        }
        Err(TypeError::new(
            Stage::Inference,
            span,
            format!("type `{type_name}` has no field `{field}`"),
        ))
    }

    /// True when `base.field` dispatches a declared method (D056) rather than
    /// selecting a struct field. Uses the base's recorded type from the side
    /// table (the base is always inferred before its Field wrapper).
    fn is_method_dispatch(&self, base: &Expr, field: &str) -> bool {
        let Some(item) = &self.cur_item else {
            return false;
        };
        let Some(bt) = self.expr_types.get(&(item.clone(), base.span())) else {
            return false;
        };
        let head = match bt.strip_borrow() {
            Ty::Con(n) | Ty::App(n, _) => n.clone(),
            _ => return false,
        };
        self.methods_table
            .get(&head)
            .is_some_and(|m| m.contains_key(field))
    }

    /// If `field` names a method declared on the receiver type, return its
    /// instantiated call type; else None (ordinary field read). D056/D057:
    /// dispatch is declaration-driven and method/field names are disjoint per
    /// type. Inside a method body any such reference is rejected — no
    /// method-to-method calls in v1.
    pub(super) fn method_ref_type(
        &mut self,
        recv: &Ty,
        field: &str,
        span: Span,
    ) -> Result<Option<Ty>, TypeError> {
        let stripped = self.subst.apply(recv).strip_borrow().clone();
        let name = match &stripped {
            Ty::Con(n) => n.clone(),
            Ty::App(n, _) => n.clone(),
            _ => return Ok(None),
        };
        match self.methods_table.get(&name).and_then(|m| m.get(field)) {
            None => Ok(None),
            Some(_) if self.method_receiver.is_some() => Err(TypeError::new(
                Stage::Inference,
                span,
                format!("method-to-method calls are not allowed in v1 (`{field}` on `{name}`)"),
            )),
            Some(scheme) => {
                let s = scheme.clone();
                Ok(Some(self.instantiate(&s)))
            }
        }
    }

    pub(super) fn check_update_clauses(
        &mut self,
        recv: &Ty,
        clauses: &[UpdateClause],
    ) -> Result<(), TypeError> {
        for c in clauses {
            match c {
                UpdateClause::Leaf { path, value, span } => {
                    let mut ty = recv.clone();
                    for seg in path {
                        ty = self.field_type(&ty, seg, *span)?;
                    }
                    let vt = self.infer_expr(value)?;
                    // Auto-ref: storing an owned T into a Ref T field records
                    // the address (implicit allocation, D054/D056) — same rule
                    // as constructor arguments.
                    let ty = self.subst.apply(&ty);
                    match (&ty, &self.subst.apply(&vt)) {
                        (Ty::App(n, args), arg)
                            if n == "Ref"
                                && args.len() == 1
                                && !matches!(arg, Ty::App(n2, _) if n2 == "Ref") =>
                        {
                            unify(&mut self.subst, &args[0], &vt, value.span())?;
                        }
                        _ => unify(&mut self.subst, &vt, &ty, value.span())?,
                    };
                }
                UpdateClause::Group {
                    path,
                    clauses,
                    span,
                } => {
                    let mut ty = recv.clone();
                    for seg in path {
                        ty = self.field_type(&ty, seg, *span)?;
                    }
                    self.check_update_clauses(&ty, clauses)?;
                }
            }
        }
        Ok(())
    }
}

/// Auto-deref through `Ref T` (field access and writes see the referent, D054).
pub(super) fn deref_ref(ty: &Ty) -> Ty {
    if let Ty::App(name, args) = ty {
        if name == "Ref" && args.len() == 1 {
            return args[0].clone();
        }
    }
    ty.clone()
}
