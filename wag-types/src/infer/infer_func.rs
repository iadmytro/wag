//! Function and actor inference.

use wag_syntax::{ActorDef, FuncDef, MethodDef, Span, Type};

use crate::error::{OriginInfo, Stage, TypeError};
use crate::types::{Scheme, Ty};
use crate::unify::{pretty_ty, unify};

use super::context::Context;
use super::util::{
    collect_free_vars, collect_type_var_names, resolve_aliases, surface_to_ty_resolved,
    surface_to_ty_with_vars,
};

/// A parsed signature: parameter types, return annotation, quantified
/// type-variable indices, and the name→Var map for the signature's type vars.
pub(super) struct SigParts {
    pub params: Vec<Ty>,
    pub ret: Option<Ty>,
    /// Indices of the fresh `Ty::Var`s allocated for the signature's type vars.
    pub var_idxs: Vec<usize>,
    /// Type-var name → its fresh Var index (empty when monomorphic).
    pub var_map: std::collections::HashMap<String, u32>,
    /// Class constraints from a `C₁ T₁, … => sig` annotation (§3.12).
    pub constraints: Vec<crate::types::Constraint>,
}

impl Context {
    /// Parse a function's signature into `SigParts`.
    ///
    /// Parameter types come from the annotation if present, else fresh metavars;
    /// explicit type variables (single uppercase A–Z) in the signature get fresh
    /// `Ty::Var` indices to be generalized later. Pure w.r.t. inference state for
    /// concrete signatures (no free type variables): it allocates no metavariables,
    /// so it is safe to call both here and during the module-level seeding pass
    /// before bodies are inferred.
    pub(super) fn signature_parts(&mut self, f: &FuncDef) -> Result<SigParts, TypeError> {
        match &f.signature {
            Some(sig) => {
                let mut parts = self.resolve_signature(sig, f.params.len(), &f.name, f.span)?;
                parts.constraints = self.constraint_parts(f, &parts.var_map)?;
                Ok(parts)
            }
            None => Ok(SigParts {
                // Unannotated parameter holes may stay free and get generalized
                // for singleton-SCC bindings (§3.5 Option A) — record them so the
                // validator does not flag the generalization (A1b).
                params: f
                    .params
                    .iter()
                    .map(|p| {
                        let ty = self.fresh_with(OriginInfo::new(format!("parameter `{p}`"), f.span));
                        if let Ty::Var(v) = &ty {
                            self.expected_free.insert(*v);
                        }
                        ty
                    })
                    .collect(),
                ret: None,
                var_idxs: Vec::new(),
                var_map: std::collections::HashMap::new(),
                constraints: Vec::new(),
            }),
        }
    }

    /// Convert a `C₁ T₁, … =>` constraint list (§3.12) into internal
    /// `Constraint`s, resolving each class's type argument against the
    /// signature's type-var map. A constraint whose argument is not a signature
    /// type variable is rejected (v1: constraints quantify over signature vars).
    fn constraint_parts(
        &self,
        f: &FuncDef,
        var_map: &std::collections::HashMap<String, u32>,
    ) -> Result<Vec<crate::types::Constraint>, TypeError> {
        let mut out = Vec::new();
        for c in &f.constraints {
            // `C T` (app) or bare `C` (nullary class).
            let (class_name, arg) = match c {
                Type::Name(n) => (n.clone(), None),
                Type::App(head, args) => {
                    let head_name = match head.as_ref() {
                        Type::Name(n) => n.clone(),
                        _ => {
                            return Err(TypeError::new(
                                Stage::Inference,
                                f.span,
                                "constraint head must be a class name",
                            ))
                        }
                    };
                    (head_name, args.first())
                }
                _ => {
                    return Err(TypeError::new(
                        Stage::Inference,
                        f.span,
                        "constraint must be a class name or class application",
                    ))
                }
            };
            match arg {
                None => out.push(crate::types::Constraint {
                    class_name,
                    ctor_var: 0,
                }),
                Some(Type::Name(vname)) => {
                    let idx = *var_map
                        .get(vname)
                        .ok_or_else(|| {
                            TypeError::new(
                                Stage::Inference,
                                f.span,
                                format!(
                                    "constraint `{class_name} {vname}` names a variable not bound in the signature"
                                ),
                            )
                        })?;
                    out.push(crate::types::Constraint {
                        class_name,
                        ctor_var: idx,
                    });
                }
                Some(_) => {
                    return Err(TypeError::new(
                        Stage::Inference,
                        f.span,
                        "constraint type argument must be a type variable",
                    ))
                }
            }
        }
        Ok(out)
    }

    /// Core signature resolution shared by functions and methods (D061).
    fn resolve_signature(
        &mut self,
        sig: &Type,
        n_params: usize,
        label: &str,
        span: Span,
    ) -> Result<SigParts, TypeError> {
        // Detect type variables in the signature.
        let mut var_names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        collect_type_var_names(sig, &mut var_names);

        let (sig_ty, indices, var_map) = if var_names.is_empty() {
            (
                surface_to_ty_resolved(sig, &self.aliases)?,
                Vec::new(),
                std::collections::HashMap::new(),
            )
        } else {
            // Assign fresh Ty::Var indices to each type variable name.
            let mut var_map = std::collections::HashMap::new();
            let mut idxs = Vec::new();
            for name in &var_names {
                let v = self.fresh_with(OriginInfo::new(format!("type variable `{name}`"), span));
                if let Ty::Var(idx) = v {
                    // Signature vars are generalized at function exit, never
                    // unified away — the validator must not flag them (A1b).
                    self.expected_free.insert(idx);
                    var_map.insert(name.clone(), idx);
                    idxs.push(idx as usize);
                }
            }
            let resolved = resolve_aliases(sig, &self.aliases, 0)?;
            (surface_to_ty_with_vars(&resolved, &var_map)?, idxs, var_map)
        };

        match self.subst.apply(&sig_ty) {
            Ty::Fun(ps, r) => {
                if ps.len() != n_params {
                    return Err(TypeError::new(
                        Stage::Inference,
                        span,
                        format!(
                            "`{label}` has {} params but signature has {}",
                            n_params,
                            ps.len()
                        ),
                    ));
                }
                Ok(SigParts {
                    params: ps,
                    ret: Some(*r),
                    var_idxs: indices,
                    var_map,
                    constraints: Vec::new(),
                })
            }
            other if n_params == 0 => Ok(SigParts {
                params: vec![],
                ret: Some(other),
                var_idxs: indices,
                var_map,
                constraints: Vec::new(),
            }),
            other => Err(TypeError::new(
                Stage::Inference,
                span,
                format!(
                    "signature of `{label}` is not a function type: {}",
                    pretty_ty(&other, &self.subst)
                ),
            )),
        }
    }

    /// Infer a method body (D056/D057). A method is a single-step operation on
    /// the receiver handle: not a top-level binding, no generalization (§3.5),
    /// and no method-to-method calls in v1. Bare atoms resolve against the
    /// receiver's fields; `self` denotes the handle itself (move-and-return).
    pub(super) fn infer_method(&mut self, m: &MethodDef) -> Result<(), TypeError> {
        let label = format!("{}.{}", m.type_name, m.name);
        // Signature → (param_tys, return annotation). Parametric receivers are
        // supported (D061): the receiver's declared params bind to the same fresh
        // vars as the matching type-var names in the signature.
        let sig_parts = match &m.signature {
            Some(sig) => self.resolve_signature(sig, m.params.len(), &label, m.span)?,
            None => SigParts {
                // Unannotated method parameter holes may stay free and get
                // generalized into the method scheme — record them (A1b).
                params: m
                    .params
                    .iter()
                    .map(|p| {
                        let ty = self.fresh_with(OriginInfo::new(format!("parameter `{p}`"), m.span));
                        if let Ty::Var(v) = &ty {
                            self.expected_free.insert(*v);
                        }
                        ty
                    })
                    .collect(),
                ret: None,
                var_idxs: Vec::new(),
                var_map: std::collections::HashMap::new(),
                constraints: Vec::new(),
            },
        };
        // v1: every type var in a method signature must be a declared param of
        // the receiver — free polymorphism in method sigs is not supported
        // (checker/VM paths unverified; it would silently degenerate to mono).
        if m.signature.is_some() {
            let declared = self
                .type_params
                .get(&m.type_name)
                .cloned()
                .unwrap_or_default();
            for name in sig_parts.var_map.keys() {
                if !declared.contains(name) {
                    return Err(TypeError::new(
                        Stage::Inference,
                        m.span,
                        format!(
                            "type variable `{name}` in method `{label}` is not a parameter of `{}` — polymorphic method signatures are not supported in v1",
                            m.type_name
                        ),
                    ));
                }
            }
        }
        let param_tys = sig_parts.params;
        let ret_ann = sig_parts.ret;

        // Receiver type args (D061): one Var per declared param of the receiver
        // datatype — shared with the signature when the name occurs there, fresh
        // otherwise (implicitly quantified at call sites).
        let recv_args = self.receiver_var_idxs(&m.type_name, &label, m.span, &sig_parts.var_map);

        // Extend env with params (same save/restore discipline as infer_func).
        let mut saved = Vec::new();
        for (p, ty) in m.params.iter().zip(param_tys.iter()) {
            saved.push((p.clone(), self.env.remove(&(p.clone(), 0))));
            self.env.insert((p.clone(), 0), Scheme::mono(ty.clone()));
        }

        // Receiver mode: bare atoms resolve against the receiver's fields and
        // `self` denotes the handle (see infer_expr arms). The receiver type is
        // `App(name, recv_args)` for parametric types, `Con(name)` otherwise.
        let old_receiver = self.method_receiver.replace((m.type_name.clone(), recv_args));
        self.cur_item = Some(label.clone());
        let body_result = self.infer_expr(&m.body);
        self.cur_item = None;
        self.method_receiver = old_receiver;

        let body_ty = body_result?;
        if let Some(ret) = &ret_ann {
            // Dedicated diagnostic (D062-era): a bare unification error here
            // leaks metavar letters and points at the whole body without saying
            // which side is the signature. Surface both rendered types plus the
            // original mismatch as context, anchored at the definition.
            if let Err(e) = unify(&mut self.subst, &body_ty, ret, m.body.span()) {
                // Raw debug dump: pretty-printing can render distinct
                // metavariables identically; show actual indices + origins.
                if std::env::var_os("WAG_DEBUG").is_some() {
                    eprintln!("\n===== WAG_DEBUG: method `{label}` body/sig mismatch =====");
                    let rb = self.subst.apply(&body_ty);
                    let rr = self.subst.apply(ret);
                    eprintln!("body_ty (raw): {rb:?}");
                    eprintln!("ret_ann (raw): {rr:?}");
                    for v in crate::unify::free_var_indices(&rb) {
                        let origin = self
                            .subst
                            .origin_of(v)
                            .map(|o| format!(" [origin: {}]", o.desc))
                            .unwrap_or_default();
                        match self.subst.get(v) {
                            Some(t) => eprintln!("  ?{v}{origin} = {t:?}"),
                            None => eprintln!("  ?{v}{origin} (free)"),
                        }
                    }
                    for v in crate::unify::free_var_indices(&rr) {
                        if !crate::unify::free_var_indices(&rb).contains(&v) {
                            let origin = self
                                .subst
                                .origin_of(v)
                                .map(|o| format!(" [origin: {}]", o.desc))
                                .unwrap_or_default();
                            match self.subst.get(v) {
                                Some(t) => eprintln!("  ?{v}{origin} = {t:?}"),
                                None => eprintln!("  ?{v}{origin} (free)"),
                            }
                        }
                    }
                }
                // Shared lettering: distinct metavariables must print as
                // distinct letters or the mismatch is invisible.
                let (body_s, ret_s) = crate::unify::pretty_ty_pair(&body_ty, ret, &self.subst);
                return Err(
                    TypeError::new(
                        Stage::Inference,
                        m.body.span(),
                        format!(
                            "method `{label}`: body infers as `{body_s}`, but the signature declares `{ret_s}`",
                        ),
                    )
                    .with_context(e.context.unwrap_or_default())
                    .with_origin_span(Some(m.span)),
                );
            }
        }

        // Link boundary rule (D056/D057): no link value may flow out of a method
        // body — the result type must not contain `Ref`.
        let final_ret = self.subst.apply(&body_ty);
        if crate::boundary::ty_contains_ref(&final_ret) {
            return Err(TypeError::new(
                Stage::Boundary,
                m.body.span(),
                format!("method `{label}`: a link value may not flow out of a method body (result type contains `Ref`)"),
            ));
        }

        // Restore params.
        for (p, old) in saved {
            if let Some(s) = old {
                self.env.insert((p, 0), s);
            } else {
                self.env.remove(&(p, 0));
            }
        }

        let final_params = param_tys.iter().map(|t| self.subst.apply(t)).collect();
        self.method_tys.insert(
            (m.type_name.clone(), m.name.clone()),
            Ty::Fun(final_params, Box::new(final_ret)),
        );
        Ok(())
    }

    pub(super) fn infer_func(&mut self, f: &FuncDef) -> Result<(), TypeError> {
        // D051/D052: only unify against an existing binding with the SAME (name, arity).
        // A different-arity entry is a new overload — no unification needed.
        let key = (f.name.clone(), f.params.len());
        let expected_scheme: Option<Scheme> = self.env.get(&key).cloned();
        // §3.1 value/function unity: a 0-arg function is its result.
        let expected: Option<Ty> = expected_scheme.map(|s| {
            let ty = self.instantiate(&s);
            if f.params.is_empty() {
                if let Ty::Fun(ref ps, ref ret) = &ty {
                    if ps.is_empty() {
                        return (**ret).clone();
                    }
                }
            }
            ty
        });
        // Parameter types: from annotation if present, else fresh.
        // Explicit type variables (single uppercase A–Z) in the signature are
        // assigned fresh Ty::Var indices and generalized in the resulting Scheme.
        let sig_parts = self.signature_parts(f)?;
        let param_tys = sig_parts.params;
        let ret_ann = sig_parts.ret;
        let sig_vars = sig_parts.var_idxs;
        let sig_constraints = sig_parts.constraints;

        // Extend env with params.
        let mut saved = Vec::new();
        for (p, ty) in f.params.iter().zip(param_tys.iter()) {
            saved.push((p.clone(), self.env.remove(&(p.clone(), 0))));
            self.env.insert((p.clone(), 0), Scheme::mono(ty.clone()));
        }

        // D081 v2: `main_boot` (the desugared `@main` `Start` arm, §5.4.1) is a
        // plain function, but its body may reference `self` to send the `End`
        // message (or transition to a run arm). Give it the `@main` actor's self
        // handle type — the creation function's `Actor Msg` return, the same
        // metavar `@main`'s behavior unifies with — so `self@ (End code)` type-checks.
        let prev_actor_self = if f.name == "main_boot" {
            let self_ty = self
                .env
                .get(&("@main".to_string(), 1))
                .cloned()
                .and_then(|s| Some(self.instantiate(&s)))
                .and_then(|ty| match ty {
                    Ty::Fun(_, ret) => Some(*ret),
                    _ => None,
                })
                .filter(|ty| matches!(ty, Ty::App(n, _) if n == "Actor"));
            std::mem::replace(&mut self.actor_self, self_ty)
        } else {
            None
        };

        // Predeclare this function as its own annotation — signature type vars are
        // already instantiated to fresh metavars in `param_tys`/`ret_ann` above)
        // so recursive calls inside the body resolve against the concrete return
        // type instead of a bare Var. Without this, shape-requiring constructs on a
        // recursive call's result (product/ctor matches §3.2/§6.4, field access §3.3)
        // see an unresolved `Var` until post-body unification, which is too late.
        if f.signature.is_some() {
            let ann_fun = Ty::Fun(param_tys.clone(), Box::new(ret_ann.clone().unwrap()));
            self.env
                .insert((f.name.clone(), f.params.len()), Scheme::mono(ann_fun));
        }

        // Save/clear active constraints: constrained calls in the body propagate
        // their class requirements to this function's scheme (D075, §3.12).
        // Seed with the signature's own constraints so the body may rely on them
        // (e.g. `Eq T => …` lets the body write `x == y` for `x : T`).
        let saved_constraints = std::mem::take(&mut self.active_constraints);
        self.active_constraints.extend(sig_constraints);

        // Record expression types under this item's name (side table).
        self.cur_item = Some(f.name.clone());
        let body_ty = self
            .infer_expr(&f.body)
            .map_err(|e| e.with_context(format!("in `{}`", f.name)))?;
        self.cur_item = None;
        if let Some(ret) = ret_ann {
            unify(&mut self.subst, &body_ty, &ret, f.body.span())
                .map_err(|e| e.with_context(format!("in `{}` return", f.name)))?;
        }

        // §3.1 value/function unity: a 0-param binding denotes its value directly
        // (type = body type), while an n>=1-param binding is a function value
        // (type = T1, …, Tn -> R). Arity is the only distinction.
        let fun_ty = if f.params.is_empty() {
            self.subst.apply(&body_ty)
        } else {
            Ty::Fun(param_tys, Box::new(self.subst.apply(&body_ty)))
        };
        if let Some(expected) = expected {
            unify(&mut self.subst, &expected, &fun_ty, f.span)?;
        }

        // Restore.
        for (p, old) in saved {
            match old {
                Some(s) => {
                    self.env.insert((p, 0), s);
                }
                None => {
                    self.env.remove(&(p, 0));
                }
            }
        }
        // Update env slot with fully substituted type.
        let final_ty = self.subst.apply(&fun_ty);
        let f_arity = f.params.len();
        // Collect constraints from constrained calls in the body (D075, §3.12).
        // Deduplicate: same (class_name, ctor_var) pair appears once.
        let mut body_constraints: Vec<crate::types::Constraint> = self.active_constraints.clone();
        body_constraints.sort_by(|a, b| {
            a.class_name.cmp(&b.class_name).then(a.ctor_var.cmp(&b.ctor_var))
        });
        body_constraints.dedup();

        // Generalize: explicit type variables from the signature take priority.
        // Otherwise, if this binding is in a singleton SCC and has free type
        // variables, bind them in the scheme so each call site can instantiate
        // independently (§3.5, Option A).
        if !sig_vars.is_empty() {
            let free_vars = collect_free_vars(&final_ty, &self.subst);
            self.env.insert(
                (f.name.clone(), f_arity),
                Scheme {
                    vars: free_vars,
                    body: final_ty,
                    constraints: body_constraints,
                },
            );
        } else if self.singleton_sccs.contains(&f.name) {
            let free_vars = collect_free_vars(&final_ty, &self.subst);
            if !free_vars.is_empty() || !body_constraints.is_empty() {
                self.env.insert(
                    (f.name.clone(), f_arity),
                    Scheme {
                        vars: free_vars,
                        body: final_ty,
                        constraints: body_constraints,
                    },
                );
            } else {
                self.env
                    .insert((f.name.clone(), f_arity), Scheme::mono(final_ty));
            }
        } else if !body_constraints.is_empty() {
            // No free vars but has constraints: still need a scheme to carry them.
            self.env.insert(
                (f.name.clone(), f_arity),
                Scheme {
                    vars: vec![],
                    body: final_ty,
                    constraints: body_constraints,
                },
            );
        } else {
            self.env
                .insert((f.name.clone(), f_arity), Scheme::mono(final_ty));
        }
        // Restore pre-body constraints (they belong to the enclosing scope).
        self.active_constraints = saved_constraints;
        self.actor_self = prev_actor_self;
        Ok(())
    }

    pub(super) fn infer_actor(&mut self, a: &ActorDef) -> Result<(), TypeError> {
        // Behavior metavar lives under `("@name", arity)` — actors occupy the
        // `@`-prefixed namespace (§2), so a same-named function must not be picked up.
        // Exact-arity lookup: `lookup` would hit the handle key `("@name", 0)` first.
        let expected = self.lookup_arity(&format!("@{}", a.name), a.params.len(), a.span)?;

        // Behavior must be (State, Msg) -> State (§5.4). The actor always
        // returns a State — it never self-terminates via `None`. The `State`
        // is a live metavar here, also used as the function's return type so
        // the body's return and the State param are the same metavar.
        let state = self.fresh_with(OriginInfo::new(format!("State of `{}`", a.name), a.span));
        let msg = self.fresh_with(OriginInfo::new(format!("Msg of `{}`", a.name), a.span));
        let beh_shape = Ty::Fun(
            vec![state.clone(), msg.clone()],
            Box::new(state.clone()),
        );

        let (param_tys, ret_ann) = if let Some(sig) = &a.signature {
            let sig_ty = surface_to_ty_resolved(sig, &self.aliases)?;
            unify(&mut self.subst, &sig_ty, &beh_shape, a.span)?;
            match self.subst.apply(&sig_ty) {
                Ty::Fun(ps, r) => {
                    if ps.len() != a.params.len() {
                        return Err(TypeError::new(
                            Stage::Inference,
                            a.span,
                            format!(
                                "actor `{}` has {} params but signature has {}",
                                a.name,
                                a.params.len(),
                                ps.len()
                            ),
                        ));
                    }
                    (ps, Some(*r))
                }
                other => {
                    return Err(TypeError::new(
                        Stage::Inference,
                        a.span,
                        format!(
                            "actor signature must be a function type, got {}",
                            pretty_ty(&other, &self.subst)
                        ),
                    ));
                }
            }
        } else {
            if a.params.len() != 2 {
                return Err(TypeError::new(
                    Stage::Inference,
                    a.span,
                    format!(
                        "actor `{}` must take (State, Msg) — got {} params",
                        a.name,
                        a.params.len()
                    ),
                ));
            }
            (
                vec![state.clone(), msg.clone()],
                Some(state.clone()),
            )
        };

        let msg_ty = self
            .subst
            .apply(&param_tys.get(1).cloned().unwrap_or_else(|| msg.clone()));
        // Update the creation function (arity 1) with resolved State/Msg types.
        let state_ty = self.subst.apply(&param_tys[0].clone());
        let ckey = (format!("@{}", a.name), 1);
        self.env.insert(
            ckey,
            Scheme::mono(Ty::Fun(vec![state_ty], Box::new(Ty::actor(msg_ty.clone())))),
        );
        let self_ty = Ty::actor(msg_ty.clone());
        let prev_self = self.actor_self.replace(self_ty.clone());

        // Unify predeclared @Name : Actor Msg with the real Msg type.
        // System actors (predeclared) keep a handle reference at arity 0;
        // user actors only expose the creation function (arity 1, above).
        let at_key = format!("@{}", a.name);
        if let Some(scheme) = self.env.get(&(at_key.clone(), 0)).cloned() {
            let existing = self.instantiate(&scheme);
            unify(&mut self.subst, &existing, &self_ty, a.span)?;
            self.env.insert((at_key, 0), Scheme::mono(self_ty));
        }

        let mut saved = Vec::new();
        for (p, ty) in a.params.iter().zip(param_tys.iter()) {
            saved.push((p.clone(), self.env.remove(&(p.clone(), 0))));
            self.env.insert((p.clone(), 0), Scheme::mono(ty.clone()));
        }

        // Outbox rules (D043): the actor's `@`-properties determine which
        // outbox constructors its Msg match may cover.
        let prev_props = std::mem::replace(
            &mut self.cur_props,
            a.properties
                .iter()
                .map(|p| p.trim_start_matches('@').to_string())
                .collect(),
        );
        let msg_base = self.subst.apply(&msg_ty);
        let prev_msg = std::mem::replace(
            &mut self.cur_msg,
            match msg_base {
                Ty::Con(n) => Some(n),
                _ => None,
            },
        );
        // Record expression types under this item's canonical name (side table).
        let prev_item = self.cur_item.replace(format!("@{}", a.name));
        let body_ty = self
            .infer_expr(&a.body)
            .map_err(|e| e.with_context(format!("in `@{}` behavior", a.name)))?;
        self.cur_item = prev_item;
        if let Some(ret) = ret_ann {
            unify(&mut self.subst, &body_ty, &ret, a.body.span())
                .map_err(|e| e.with_context(format!("in `@{}` behavior return", a.name)))?;
        }
        self.cur_props = prev_props;
        self.cur_msg = prev_msg;

        let fun_ty = Ty::Fun(param_tys, Box::new(self.subst.apply(&body_ty)));
        unify(&mut self.subst, &expected, &fun_ty, a.span)?;
        unify(&mut self.subst, &fun_ty, &beh_shape, a.span)?;

        for (p, old) in saved {
            match old {
                Some(s) => {
                    self.env.insert((p, 0), s);
                }
                None => {
                    self.env.remove(&(p, 0));
                }
            }
        }
        let final_ty = self.subst.apply(&fun_ty);
        self.env.insert(
            (format!("@{}", a.name), a.params.len()),
            Scheme::mono(final_ty.clone()),
        );
        self.actor_self = prev_self;
        Ok(())
    }
}
