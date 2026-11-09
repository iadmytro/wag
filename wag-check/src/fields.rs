//! Field-path table, rule 15 resolution, and desugaring of `.` / `with`.

use std::collections::HashMap;

use wag_syntax::{
    desugar_record_update_template, Expr, Item, Pat, Program, ResolvedFieldStep, ResolvedLeaf,
    Span, UpdateClause,
};
use wag_types::{CtorInfo, Ty, TypedProgram, TypeCtors};

use crate::error::{CheckError, Rule};

/// `(TypeName, field_label) → (ctor_name, field_index, arity, field_ty)`.
#[derive(Debug, Clone)]
pub struct FieldTable {
    /// (type_name, label) → (ctor, index, arity)
    pub by_label: HashMap<(String, String), (String, usize, usize)>,
    pub type_ctors: TypeCtors,
    pub constructors: HashMap<String, CtorInfo>,
    /// type name → method names (D056/D057). Method dispatch is left intact
    /// for the VM; field resolution must not treat it as a struct field.
    pub methods: std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
    /// (type_name, method) → result type (for local typing of method calls).
    pub method_results: std::collections::BTreeMap<(String, String), Ty>,
    /// Region-resident type names (D054): writes on `Self_` receivers of these
    /// types desugar to in-place `RegionWrite` instead of SetField/template.
    pub resident: std::collections::BTreeSet<String>,
}

impl FieldTable {
    /// True when `field` names a method on type `head` (not a struct field).
    pub fn is_method(&self, head: &str, field: &str) -> bool {
        self.methods.get(head).is_some_and(|ms| ms.contains(field))
    }
}

impl FieldTable {
    pub fn build(typed: &TypedProgram) -> Self {
        let mut by_label = HashMap::new();

        for (cname, info) in &typed.constructors {
            let labels: Vec<Option<String>> =
                info.fields.iter().map(|f| f.label.clone()).collect();
            let arity = labels.len();
            for (i, lab) in labels.iter().enumerate() {
                if let Some(l) = lab {
                    by_label.insert(
                        (info.type_name.clone(), l.clone()),
                        (cname.clone(), i, arity),
                    );
                }
            }
        }

        let mut methods: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> =
            std::collections::BTreeMap::new();
        let mut method_results: std::collections::BTreeMap<(String, String), Ty> =
            std::collections::BTreeMap::new();
        for ((tname, mname), ty) in &typed.method_tys {
            methods.entry(tname.clone()).or_default().insert(mname.clone());
            let result = match ty {
                Ty::Fun(_, r) => (**r).clone(),
                other => other.clone(),
            };
            method_results.insert((tname.clone(), mname.clone()), result);
        }

        let resident: std::collections::BTreeSet<String> = typed
            .residency
            .iter()
            .filter(|(_, &r)| r)
            .map(|(t, _)| t.clone())
            .collect();

        FieldTable {
            by_label,
            type_ctors: typed.type_ctors.clone(),
            constructors: typed.constructors.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            methods,
            method_results,
            resident,
        }
    }

    /// Result type of method `field` on type `head` (D056).
    pub fn method_result(&self, head: &str, field: &str, span: Span) -> Result<Ty, CheckError> {
        self.method_results
            .get(&(head.to_string(), field.to_string()))
            .cloned()
            .ok_or_else(|| {
                CheckError::new(
                    Rule::FieldPath,
                    span,
                    format!("method `{field}` on `{head}` has no known result type"),
                )
            })
    }

    /// A1b: applied type args of a path element — `App(name, args)` → its args;
    /// monomorphic `Con` / builtins → empty. Passed to [`CtorInfo::field_ty`] so
    /// resolved field types are concrete (no raw param vars).
    pub fn applied_args(ty: &Ty) -> &[Ty] {
        match ty.strip_borrow() {
            Ty::App(_, a) => a,
            _ => &[],
        }
    }

    /// Resolve one field step on `type_name`. Errors on multi-ctor (rule 15) or missing label.
    pub fn resolve_step(
        &self,
        type_name: &str,
        field: &str,
        span: Span,
        args: &[Ty],
    ) -> Result<(ResolvedFieldStep, Ty), CheckError> {
        let ctors = self
            .type_ctors
            .get(type_name)
            .cloned()
            .unwrap_or_default();
        if ctors.is_empty() {
            return Err(CheckError::new(
                Rule::FieldPath,
                span,
                format!("unknown type `{type_name}` in field path"),
            ));
        }
        if ctors.len() != 1 {
            return Err(CheckError::new(
                Rule::FieldPath,
                span,
                format!(
                    "rule 15: field access through multi-constructor type `{type_name}`"
                ),
            ));
        }
        let (ctor, idx, arity) = self
            .by_label
            .get(&(type_name.to_string(), field.to_string()))
            .cloned()
            .ok_or_else(|| {
                CheckError::new(
                    Rule::FieldPath,
                    span,
                    format!("type `{type_name}` has no field `{field}`"),
                )
            })?;
        let info = self.constructors.get(&ctor).ok_or_else(|| {
            CheckError::new(
                Rule::FieldPath,
                span,
                format!("missing constructor `{ctor}`"),
            )
        })?;
        // A1b: apply the receiver's actual args — no raw param vars in results.
        let field_ty = info.field_ty(idx, args);
        Ok((
            ResolvedFieldStep {
                constructor: ctor,
                arity,
                field_index: idx,
            },
            field_ty,
        ))
    }

    /// Resolve one field step for **write** (RecordUpdate → SetField).
    /// Skips the single-constructor restriction (rule 15) — owned = writable.
    pub fn resolve_step_write(
        &self,
        type_name: &str,
        field: &str,
        span: Span,
        args: &[Ty],
    ) -> Result<(ResolvedFieldStep, Ty), CheckError> {
        let ctors = self
            .type_ctors
            .get(type_name)
            .cloned()
            .unwrap_or_default();
        if ctors.is_empty() {
            return Err(CheckError::new(
                Rule::FieldPath,
                span,
                format!("unknown type `{type_name}` in field path"),
            ));
        }
        // No single-ctor check: writes are allowed on any owned value.
        let (ctor, idx, arity) = self
            .by_label
            .get(&(type_name.to_string(), field.to_string()))
            .cloned()
            .ok_or_else(|| {
                CheckError::new(
                    Rule::FieldPath,
                    span,
                    format!("type `{type_name}` has no field `{field}`"),
                )
            })?;
        let info = self.constructors.get(&ctor).ok_or_else(|| {
            CheckError::new(
                Rule::FieldPath,
                span,
                format!("missing constructor `{ctor}`"),
            )
        })?;
        // A1b: apply the receiver's actual args — no raw param vars in results.
        let field_ty = info.field_ty(idx, args);
        Ok((
            ResolvedFieldStep {
                constructor: ctor,
                arity,
                field_index: idx,
            },
            field_ty,
        ))
    }

    pub fn type_head(ty: &Ty) -> Option<String> {
        let mut t = ty.strip_borrow();
        // Auto-deref through Ref T for field path resolution.
        if let Ty::App(name, args) = t {
            if name == "Ref" && args.len() == 1 {
                t = &args[0];
            }
        }
        match t {
            Ty::Con(n) => Some(n.clone()),
            Ty::App(n, _) => Some(n.clone()),
            _ => None,
        }
    }
}

/// Desugar every `Field` / `RecordUpdate` in the program; leave other nodes intact.
pub fn desugar_program(
    program: &Program,
    typed: &TypedProgram,
    table: &FieldTable,
) -> Result<Program, CheckError> {
    let mut items = Vec::with_capacity(program.items.len());
    for item in &program.items {
        match item {
            Item::Func(f) => {
                let mut f = f.clone();
                // Bind param types for local inference of receivers.
                let mut env = local_env_from_func(&f.name, typed);
                for (i, p) in f.params.iter().enumerate() {
                    if let Some(Ty::Fun(ps, _)) = typed.bindings.get(&(f.name.clone(), f.params.len())) {
                        if i < ps.len() {
                            env.insert(p.clone(), ps[i].clone());
                        }
                    }
                }
                f.body = desugar_expr(&f.body, &mut env, table)?;
                items.push(Item::Func(f));
            }
            Item::Actor(a) => {
                let mut a = a.clone();
                let mut env = local_env_from_func(&a.name, typed);
                // Actor behavior is keyed `("@name", arity)` (separate namespace, §2).
                if let Some(Ty::Fun(ps, _)) =
                    typed.bindings.get(&(format!("@{}", a.name), a.params.len()))
                {
                    for (i, p) in a.params.iter().enumerate() {
                        if i < ps.len() {
                            env.insert(p.clone(), ps[i].clone());
                        }
                    }
                }
                a.body = desugar_expr(&a.body, &mut env, table)?;
                items.push(Item::Actor(a));
            }
            Item::Method(m) => {
                let mut m = m.clone();
                // Bind param types + receiver type from the method scheme
                // (D056/D057). `__self__` lets local inference resolve write
                // paths over the handle.
                let mut env = HashMap::new();
                // Receiver type (D061): `App(name, [Var(i) per declared param])`
                // for parametric types — arg index i matches the ctor table's
                // per-declaration var convention; bare `Con` otherwise.
                let recv_ty = match typed.type_params.get(&m.type_name) {
                    Some(ps) if !ps.is_empty() => Ty::App(
                        m.type_name.clone(),
                        (0..ps.len() as u32).map(Ty::Var).collect(),
                    ),
                    _ => Ty::Con(m.type_name.clone()),
                };
                env.insert("__self__".to_string(), recv_ty);
                if let Some(Ty::Fun(ps, _)) =
                    typed.method_tys.get(&(m.type_name.clone(), m.name.clone()))
                {
                    for (i, p) in m.params.iter().enumerate() {
                        if i < ps.len() {
                            env.insert(p.clone(), ps[i].clone());
                        }
                    }
                }
                m.body = desugar_expr(&m.body, &mut env, table)?;
                items.push(Item::Method(m));
            }
            other => items.push(other.clone()),
        }
    }
    Ok(Program { exports: None, imports: None, items, source: None })
}

fn local_env_from_func(name: &str, typed: &TypedProgram) -> HashMap<String, Ty> {
    let mut env = HashMap::new();
    // Builtin function types (first overload wins per name).
    for ((k, _arity), v) in wag_types::builtin_function_types() {
        env.entry(k.clone()).or_insert_with(|| v.clone());
    }
    // Top-level names as function values (first overload wins per name).
    for ((k, _arity), v) in &typed.bindings {
        env.entry(k.clone()).or_insert_with(|| v.clone());
    }
    let _ = name;
    env
}

fn desugar_expr(
    expr: &Expr,
    env: &mut HashMap<String, Ty>,
    table: &FieldTable,
) -> Result<Expr, CheckError> {
    match expr {
        Expr::Field { .. } => desugar_field_chain(expr, env, table),
        Expr::RecordUpdate {
            receiver,
            clauses,
            span,
        } => {
            let recv = desugar_expr(receiver, env, table)?;
            let recv_ty = infer_local(&recv, env, table)?;
            let owned = recv_ty.strip_borrow().clone();
            let leaves_raw = flatten_clauses(clauses, &[])?;
            let mut resolved = Vec::new();
            for (path, value, leaf_span) in leaves_raw {
                let value = desugar_expr(&value, env, table)?;
                let mut steps = Vec::new();
                let mut ty = owned.clone();
                for seg in &path {
                    let head = FieldTable::type_head(&ty).ok_or_else(|| {
                        CheckError::new(
                            Rule::FieldPath,
                            leaf_span,
                            format!("cannot resolve field `{seg}` on `{ty}`"),
                        )
                    })?;
                    let (step, next_ty) =
                        table.resolve_step_write(
                            &head,
                            seg,
                            leaf_span,
                            FieldTable::applied_args(&ty),
                        )?;
                    steps.push(step);
                    ty = next_ty;
                }
                resolved.push(ResolvedLeaf { steps, value });
            }

            // Region-resident receiver (D054/D058): a write statement on `Self_`
            // desugars to an in-place region write instead of SetField/template.
            if matches!(recv, Expr::Self_ { .. })
                && table.resident.contains(&FieldTable::type_head(&owned).unwrap_or_default())
            {
                if resolved.len() != 1 {
                    return Err(CheckError::new(
                        Rule::FieldPath,
                        *span,
                        "multiple writes in one statement on a region handle are not yet supported",
                    ));
                }
                let leaf = resolved.pop().expect("checked len == 1 above");
                if leaf.steps.is_empty() {
                    return Err(CheckError::new(
                        Rule::FieldPath,
                        *span,
                        "region write path must be a non-empty field chain",
                    ));
                }
                let path: Vec<usize> = leaf.steps.iter().map(|s| s.field_index).collect();
                return Ok(Expr::RegionWrite {
                    receiver: Box::new(recv),
                    path,
                    value: Box::new(leaf.value),
                    span: *span,
                });
            }

            // Fast path: single leaf, single step → in-place SetField (VM arena write).
            if resolved.len() == 1 && resolved[0].steps.len() == 1 {
                let step = &resolved[0].steps[0];
                let value = resolved[0].value.clone();
                let field_idx = step.field_index;
                return Ok(Expr::SetField {
                    receiver: Box::new(recv),
                    field_idx,
                    value: Box::new(value),
                    span: *span,
                });
            }
            desugar_record_update_template(recv, &resolved, *span).map_err(|e| {
                CheckError::new(Rule::FieldPath, *span, e)
            })
        }
        Expr::Apply { fun, args, span } => {
            // Field mutation by application (§3.3.1): `a.f v` → SetField.
            // Method calls (`l.append 42`) keep their Apply form for the VM.
            if let Expr::Field { base, field, .. } = fun.as_ref() {
                if args.len() == 1 && !is_method_dispatch(base, field, env, table) {
                    return self_desugar_field_setter(fun, &args[0], env, table, *span);
                }
            }
            Ok(Expr::Apply {
                fun: Box::new(desugar_expr(fun, env, table)?),
                args: args
                    .iter()
                    .map(|a| desugar_expr(a, env, table))
                    .collect::<Result<_, _>>()?,
                span: *span,
            })
        },
        Expr::BinOp { op, lhs, rhs, span } => Ok(Expr::BinOp {
            op: *op,
            lhs: Box::new(desugar_expr(lhs, env, table)?),
            rhs: Box::new(desugar_expr(rhs, env, table)?),
            span: *span,
        }),
        Expr::Borrow { inner, span } => Ok(Expr::Borrow {
            inner: Box::new(desugar_expr(inner, env, table)?),
            span: *span,
        }),
        Expr::Bind {
            name,
            value,
            body,
            span,
        } => {
            let value = desugar_expr(value, env, table)?;
            // Best-effort: field desugaring doesn't need exact types; the full
            // inference pass validates later. A failed lookup falls back to Unit.
            let vt = infer_local(&value, env, table).unwrap_or(Ty::unit());
            let old = env.insert(name.clone(), vt);
            let body = desugar_expr(body, env, table)?;
            match old {
                Some(t) => {
                    env.insert(name.clone(), t);
                }
                None => {
                    env.remove(name);
                }
            }
            Ok(Expr::Bind {
                name: name.clone(),
                value: Box::new(value),
                body: Box::new(body),
                span: *span,
            })
        }
        Expr::Seq { first, second, span } => {
            let first = desugar_expr(first, env, table)?;
            let second = desugar_expr(second, env, table)?;
            Ok(Expr::Seq {
                first: Box::new(first),
                second: Box::new(second),
                span: *span,
            })
        }
        Expr::If { arms, span } => {
            let mut out = Vec::new();
            for arm in arms {
                let condition = match &arm.condition {
                    Some(c) => Some(desugar_expr(c, env, table)?),
                    None => None,
                };
                let body = desugar_expr(&arm.body, env, table)?;
                out.push(wag_syntax::IfArm { condition, body });
            }
            Ok(Expr::If {
                arms: out,
                span: *span,
            })
        }
        Expr::Match {
            scrutinee,
            arms,
            span,
        } => {
            let scrutinee = desugar_expr(scrutinee, env, table)?;
            // Best-effort: see Let above.
            let st = infer_local(&scrutinee, env, table).unwrap_or(Ty::unit());
            let mut out_arms = Vec::new();
            for arm in arms {
                let saved = bind_pat_env(&arm.pattern, &st, env, table)?;
                let body = desugar_expr(&arm.body, env, table)?;
                restore_env(env, saved);
                out_arms.push(wag_syntax::MatchArm {
                    pattern: arm.pattern.clone(),
                    body,
                });
            }
            Ok(Expr::Match {
                scrutinee: Box::new(scrutinee),
                arms: out_arms,
                span: *span,
            })
        }
        Expr::ListLit { elems, span } => Ok(Expr::ListLit {
            elems: elems
                .iter()
                .map(|e| desugar_expr(e, env, table))
                .collect::<Result<_, _>>()?,
            span: *span,
        }),
        Expr::Product { elems, span } => Ok(Expr::Product {
            elems: elems
                .iter()
                .map(|e| desugar_expr(e, env, table))
                .collect::<Result<_, _>>()?,
            span: *span,
        }),
        Expr::Lambda { params, body, span } => Ok(Expr::Lambda {
            // Param types are not available at this stage; field chains on
            // params will report a resolution error (improvement over the
            // previous runtime "desugaring" panic).
            body: Box::new(desugar_expr(body, env, table)?),
            params: params.clone(),
            span: *span,
        }),
        Expr::Send { target, msg, span } => Ok(Expr::Send {
            target: Box::new(desugar_expr(target, env, table)?),
            msg: Box::new(desugar_expr(msg, env, table)?),
            span: *span,
        }),
        Expr::Comprehension {
            generators,
            accs,
            guards,
            result,
            span,
        } => {
            let mut out_generators = Vec::new();
            for gen in generators {
                let sources = gen
                    .sources
                    .iter()
                    .map(|s| desugar_expr(s, env, table))
                    .collect::<Result<_, _>>()?;
                out_generators.push(wag_syntax::Generator {
                    vars: gen.vars.clone(),
                    sources,
                    span: gen.span,
                });
            }
            let mut out_accs = Vec::new();
            for acc in accs {
                let init = desugar_expr(&acc.init, env, table)?;
                out_accs.push(wag_syntax::AccInit {
                    name: acc.name.clone(),
                    init,
                    span: acc.span,
                });
            }
            let out_guards = guards
                .iter()
                .map(|g| desugar_expr(g, env, table))
                .collect::<Result<_, _>>()?;
            let out_result = desugar_expr(result, env, table)?;
            Ok(Expr::Comprehension {
                generators: out_generators,
                accs: out_accs,
                guards: out_guards,
                result: Box::new(out_result),
                span: *span,
            })
        }
        other => Ok(other.clone()),
    }
}

/// Field mutation by application (§3.3.1): resolve `Apply(Field(...), [arg])`
/// to SetField / template rebuild.
fn self_desugar_field_setter(
    fun: &Expr,
    arg: &Expr,
    env: &mut HashMap<String, Ty>,
    table: &FieldTable,
    span: Span,
) -> Result<Expr, CheckError> {
    // Decompose the field chain: Field(Field(Var(p), a), b) → (Var(p), ["a", "b"])
    let mut fields = Vec::new();
    let mut cur = fun;
    while let Expr::Field { base, field, .. } = cur {
        fields.push(field.clone());
        cur = base.as_ref();
    }
    fields.reverse();

    // Desugar and infer the root expression.
    let recv = desugar_expr(cur, env, table)?;
    let recv_ty = infer_local(&recv, env, table)?;
    let owned = recv_ty.strip_borrow().clone();

    // Resolve each path step for write.
    let mut steps = Vec::new();
    let mut ty = owned.clone();
    for seg in &fields {
        let head = FieldTable::type_head(&ty).ok_or_else(|| {
            CheckError::new(
                Rule::FieldPath,
                span,
                format!("cannot resolve field `{seg}` on `{ty}`"),
            )
        })?;
        let (step, next_ty) =
            table.resolve_step_write(&head, seg, span, FieldTable::applied_args(&ty))?;
        steps.push(step);
        ty = next_ty;
    }

    // Desugar the value expression.
    let value = desugar_expr(arg, env, table)?;

    // Fast path: single step → in-place SetField.
    if steps.len() == 1 {
        let step = &steps[0];
        let field_idx = step.field_index;
        return Ok(Expr::SetField {
            receiver: Box::new(recv),
            field_idx,
            value: Box::new(value),
            span,
        });
    }

    // Multi-step: template rebuild (walk through intermediate Ref cells).
    let resolved = vec![ResolvedLeaf { steps, value }];
    desugar_record_update_template(recv, &resolved, span).map_err(|e| {
        CheckError::new(Rule::FieldPath, span, e)
    })
}

fn flatten_clauses(
    clauses: &[UpdateClause],
    prefix: &[String],
) -> Result<Vec<(Vec<String>, Expr, Span)>, CheckError> {
    let mut out = Vec::new();
    for c in clauses {
        match c {
            UpdateClause::Leaf { path, value, span } => {
                let mut full = prefix.to_vec();
                full.extend(path.iter().cloned());
                out.push((full, value.clone(), *span));
            }
            UpdateClause::Group {
                path,
                clauses,
                span: _,
            } => {
                let mut full = prefix.to_vec();
                full.extend(path.iter().cloned());
                out.extend(flatten_clauses(clauses, &full)?);
            }
        }
    }
    Ok(out)
}

/// Flatten `Field` chain and desugar to nested borrowed matches.
/// True when `base.field` dispatches a method (D056/D057) rather than
/// selecting a struct field.
fn is_method_dispatch(
    base: &Expr,
    field: &str,
    env: &mut HashMap<String, Ty>,
    table: &FieldTable,
) -> bool {
    let Ok(b) = desugar_expr(base, env, table) else {
        return false;
    };
    let Ok(ty) = infer_local(&b, env, table) else {
        return false;
    };
    match FieldTable::type_head(ty.strip_borrow()) {
        Some(head) => table.is_method(&head, field),
        None => false,
    }
}

fn desugar_field_chain(
    expr: &Expr,
    env: &mut HashMap<String, Ty>,
    table: &FieldTable,
) -> Result<Expr, CheckError> {
    let mut fields = Vec::new();
    let mut cur = expr;
    let span = expr.span();
    while let Expr::Field { base, field, .. } = cur {
        fields.push(field.clone());
        cur = base;
    }
    fields.reverse();
    let base = desugar_expr(cur, env, table)?;
    let base_ty = infer_local(&base, env, table)?;

    // Method dispatch (D056/D057): leave intact for the VM; field resolution
    // must not treat a method name as a struct field.
    if let Some(head) = FieldTable::type_head(base_ty.strip_borrow()) {
        if table.is_method(&head, &fields[0]) {
            return Ok(expr.clone());
        }
    }

    // Self-peek (D060): a single-step field read on the receiver handle is left
    // intact for the VM's non-consuming `self_field` lookup. Desugaring it to a
    // match over `Borrow(Self_)` would make linearity consume `__self__`,
    // breaking every method that reads a field and re-emits `self`.
    if fields.len() == 1 && matches!(cur, Expr::Self_ { .. }) {
        return Ok(expr.clone());
    }

    let mut steps = Vec::new();
    let mut ty = base_ty.strip_borrow().clone();
    for f in &fields {
        let head = FieldTable::type_head(&ty).ok_or_else(|| {
            CheckError::new(
                Rule::FieldPath,
                span,
                format!("cannot resolve field `{f}` on `{ty}`"),
            )
        })?;
        let (step, next) = table.resolve_step(&head, f, span, FieldTable::applied_args(&ty))?;
        steps.push((step, next.clone()));
        ty = next;
    }

    // Scrutinee: ensure borrowed view of the root.
    let scrut = match &base_ty {
        Ty::Borrow(_) => base,
        _ => Expr::Borrow {
            inner: Box::new(base),
            span,
        },
    };

    // Build nested match from the outside.
    fn build_match(
        scrut: Expr,
        rest: &[(ResolvedFieldStep, Ty)],
        span: Span,
    ) -> Expr {
        if rest.is_empty() {
            return scrut;
        }
        let (step, _fty) = &rest[0];
        let mut binders = Vec::with_capacity(step.arity);
        let mut field_var = String::new();
        for i in 0..step.arity {
            if i == step.field_index {
                field_var = format!("__f{}", i);
                binders.push(Pat::Var {
                    name: field_var.clone(),
                    span,
                });
            } else {
                binders.push(Pat::Wildcard { span });
            }
        }
        let inner_scrut = Expr::Var {
            name: field_var.clone(),
            span,
        };
        let body = if rest.len() == 1 {
            inner_scrut
        } else {
            build_match(inner_scrut, &rest[1..], span)
        };
        Expr::Match {
            scrutinee: Box::new(scrut),
            arms: vec![wag_syntax::MatchArm {
                pattern: Pat::Ctor {
                    name: step.constructor.clone(),
                    qualifier: None,
                    args: binders,
                    span,
                },
                body,
            }],
            span,
        }
    }

    Ok(build_match(scrut, &steps, span))
}

/// Lightweight type reconstruction for desugaring receivers (best-effort).
fn infer_local(expr: &Expr, env: &HashMap<String, Ty>, table: &FieldTable) -> Result<Ty, CheckError> {
    match expr {
        Expr::Var { name, span } => env.get(name).cloned().ok_or_else(|| {
            CheckError::new(
                Rule::FieldPath,
                *span,
                format!("unbound `{name}` during field desugar"),
            )
        }),
        Expr::Int { .. } => Ok(Ty::int()),
        Expr::Upper { name, .. } => {
            if let Some(c) = table.constructors.get(name) {
                // nullary or as value — use result type head
                Ok(Ty::Con(c.type_name.clone()))
            } else {
                Ok(Ty::unit())
            }
        }
        Expr::Borrow { inner, .. } => {
            let t = infer_local(inner, env, table)?;
            Ok(match t {
                Ty::Borrow(_) => t,
                other => Ty::Borrow(Box::new(other)),
            })
        }
        Expr::Field { .. } => {
            // Should have been desugared already when nested; resolve chain.
            let mut fields = Vec::new();
            let mut cur = expr;
            while let Expr::Field { base, field, .. } = cur {
                fields.push(field.clone());
                cur = base;
            }
            fields.reverse();
            // Method dispatch (D056): a bare read of a nullary method is the
            // call itself — type it as the method's result.
            let base_ty = infer_local(cur, env, table)?;
            if let Some(head) = FieldTable::type_head(base_ty.strip_borrow()) {
                if table.is_method(&head, &fields[0]) {
                    return Ok(table.method_result(&head, &fields[0], expr.span())?);
                }
            }
            let mut ty = base_ty.strip_borrow().clone();
            for f in fields {
                let head = FieldTable::type_head(&ty).unwrap_or_default();
                let (_, next) =
                    table.resolve_step(&head, &f, expr.span(), FieldTable::applied_args(&ty))?;
                ty = next;
            }
            Ok(ty)
        }
        Expr::Apply { fun, args, .. } => {
            // Builtin `copy : &a -> a` — result type is the argument's type
            // (not in `table.bindings`, so handle before the generic lookup).
            if let Expr::Var { name, .. } = fun.as_ref() {
                if name == "copy" {
                    if let Some(a) = args.first() {
                        return infer_local(a, env, table);
                    }
                }
            }
            // Method dispatch by application (D056): `h.append 20` → the
            // method's result type.
            if let Expr::Field { base, field, .. } = fun.as_ref() {
                if let Ok(bt) = infer_local(base, env, table) {
                    if let Some(head) = FieldTable::type_head(bt.strip_borrow()) {
                        if table.is_method(&head, field) {
                            return Ok(table.method_result(&head, field, fun.span())?);
                        }
                    }
                }
            }
            let ft = infer_local(fun, env, table)?;
            match ft {
                Ty::Fun(_, r) => Ok(*r),
                // Constructor application: result type is the type constructor.
                Ty::Con(name) => Ok(Ty::Con(name)),
                other => Err(CheckError::new(
                    Rule::FieldPath,
                    expr.span(),
                    format!("cannot apply value of type `{other}` — expected a function or constructor"),
                )),
            }
        }
        Expr::BinOp { op, .. } => match op {
            wag_syntax::BinOp::Add
            | wag_syntax::BinOp::Sub
            | wag_syntax::BinOp::Mul
            | wag_syntax::BinOp::Div => Ok(Ty::option(Ty::int())),
            _ => Ok(Ty::bool()),
        },
        Expr::Bind { name, value, body, .. } => {
            let mut e = env.clone();
            if let Ok(vt) = infer_local(value, env, table) {
                e.insert(name.clone(), vt);
            }
            infer_local(body, &e, table)
        }
        Expr::Seq { first, second, .. } => {
            // `first`'s value is discarded; both run in the same env (no binding).
            let _ = infer_local(first, env, table);
            infer_local(second, env, table)
        }
        Expr::Match { arms, .. } => {
            if let Some(a) = arms.first() {
                infer_local(&a.body, env, table)
            } else {
                Ok(Ty::unit())
            }
        }
        Expr::If { arms, .. } => {
            if let Some(a) = arms.first() {
                infer_local(&a.body, env, table)
            } else {
                Ok(Ty::unit())
            }
        }
        Expr::RecordUpdate { receiver, .. } => {
            Ok(infer_local(receiver, env, table)?.strip_borrow().clone())
        }
        Expr::SetField { receiver, .. } => {
            Ok(infer_local(receiver, env, table)?.strip_borrow().clone())
        }
        // Receiver handle (D056/D057): typed via the `__self__` env entry in
        // method bodies; untyped elsewhere (actor `self`).
        Expr::Self_ { .. } => Ok(env.get("__self__").cloned().unwrap_or_else(Ty::unit)),
        // List literal / comprehension: typed as `List ?a` so that method
        // dispatch (e.g. `[1].isEmpty`) resolves via the type head "List".
        Expr::ListLit { .. } | Expr::Comprehension { .. } => Ok(Ty::list(Ty::Var(0))),
        _ => Ok(Ty::unit()),
    }
}

fn bind_pat_env(
    pat: &Pat,
    scrut: &Ty,
    env: &mut HashMap<String, Ty>,
    table: &FieldTable,
) -> Result<Vec<(String, Option<Ty>)>, CheckError> {
    let mut saved = Vec::new();
    bind_pat_env_rec(pat, scrut, env, table, &mut saved)?;
    Ok(saved)
}

fn bind_pat_env_rec(
    pat: &Pat,
    scrut: &Ty,
    env: &mut HashMap<String, Ty>,
    table: &FieldTable,
    saved: &mut Vec<(String, Option<Ty>)>,
) -> Result<(), CheckError> {
    let base = scrut.strip_borrow();
    match pat {
        Pat::Wildcard { .. } => {}
        // Literal pattern (§9): binds no variable.
        Pat::Literal { .. } => {}
        Pat::Var { name, .. } => {
            saved.push((name.clone(), env.insert(name.clone(), scrut.clone())));
        }
        Pat::Ctor { name, args, span, .. } => {
            if let Some(info) = table.constructors.get(name) {
                // A1b: apply the scrutinee's actual args to the declared field
                // types — bound names get concrete types, never raw param vars.
                let scrut_args: &[Ty] = match base {
                    Ty::App(_, a) => a,
                    _ => &[],
                };
                for (i, ap) in args.iter().enumerate() {
                    let ft = info.fields.get(i).map(|_| {
                        let fty = info.field_ty(i, scrut_args);
                        // If scrut was borrowed, nested binders are borrowed views.
                        if matches!(scrut, Ty::Borrow(_)) {
                            match &fty {
                                Ty::Borrow(_) => fty.clone(),
                                other => Ty::Borrow(Box::new(other.clone())),
                            }
                        } else {
                            fty
                        }
                    }).ok_or_else(|| CheckError::new(
                        Rule::Internal,
                        *span,
                        format!(
                            "pattern `{name}` has {} args but constructor declares {}",
                            args.len(), info.fields.len()
                        ),
                    ))?;
                    bind_pat_env_rec(ap, &ft, env, table, saved)?;
                }
            } else {
                for ap in args {
                    bind_pat_env_rec(ap, &Ty::unit(), env, table, saved)?;
                }
            }
            let _ = base;
        }
        // Native product destructuring `(p1, p2 [, p3])` (§3.6): bind each
        // element against the corresponding `Pair`/`Triple` field.
        Pat::Product { elems, .. } => {
            // Lenient fallback is intentional: inference (§3.6) validates that the
            // scrutinee is a `Pair`/`Triple` of exactly `elems.len()` fields before
            // this check runs, so the `_` arm can't occur in practice. Falling back
            // to unit (rather than erroring) keeps field binding sound if the type
            // model ever diverges from the product-label assumption.
            let fields: Vec<Ty> = match base.clone() {
                Ty::App(n, args) if (n == "Pair" || n == "Triple") && args.len() == elems.len() => {
                    args
                }
                _ => elems.iter().map(|_| Ty::unit()).collect(),
            };
            let borrowed = matches!(scrut, Ty::Borrow(_));
            for (i, ap) in elems.iter().enumerate() {
                let ft = fields.get(i).cloned().unwrap_or(Ty::unit());
                let ft = if borrowed {
                    match &ft {
                        Ty::Borrow(_) => ft,
                        other => Ty::Borrow(Box::new(other.clone())),
                    }
                } else {
                    ft
                };
                bind_pat_env_rec(ap, &ft, env, table, saved)?;
            }
        }
    }
    Ok(())
}

fn restore_env(env: &mut HashMap<String, Ty>, saved: Vec<(String, Option<Ty>)>) {
    for (name, old) in saved.into_iter().rev() {
        match old {
            Some(t) => {
                env.insert(name, t);
            }
            None => {
                env.remove(&name);
            }
        }
    }
}
