//! Lambda lifting: rewrite `Expr::Lambda` nodes into synthetic top-level
//! functions + (partial) application, before type checking.
//!
//! `(x -> x + 1)` with no free vars  → `Var("__lambda_0")`  (fn ref)
//! `(y -> y + x)` with free `x`     → `Apply(Var("__lambda_0"), [x])`
//!   where `__lambda_0 y x = y + x`

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::Span;

/// Lift all lambdas in the program, appending synthetic functions to `items`.
pub fn lift_lambdas(program: &mut Program) {
    // Top-level names (functions, actors, constructors) are always in scope;
    // they are NOT capturable free variables. Filtering them out prevents
    // lambda lifting from wrongly capturing them (which would produce synthetic
    // functions with spurious params and broken partial applications).
    let mut top_names: HashSet<String> = program
        .items
        .iter()
        .map(|item| match item {
            Item::Func(f) => f.name.clone(),
            Item::Actor(a) => a.name.clone(),
            Item::Constructor(c) => c.name.clone(),
            Item::Method(m) => m.dotted_name(),
            Item::TypeAlias(a) => a.name.clone(),
            Item::ParametricType(p) => p.name.clone(),
            Item::Class(c) => c.name.clone(),
            Item::Instance(i) => i.class_name.clone(),
        })
        .collect();

    let mut counter = 0usize;
    let mut synthetics: Vec<FuncDef> = Vec::new();

    for item in &mut program.items {
        match item {
            Item::Func(f) => lift_in_expr(&mut f.body, &mut counter, &mut synthetics, &mut top_names),
            Item::Actor(a) => lift_in_expr(&mut a.body, &mut counter, &mut synthetics, &mut top_names),
            Item::Method(m) => lift_in_expr(&mut m.body, &mut counter, &mut synthetics, &mut top_names),
            Item::Constructor(_) => {}
            Item::TypeAlias(_) | Item::ParametricType(_) => {}
            Item::Class(_) | Item::Instance(_) => {}
        }
    }

    // Post-lift fixup for synthetic bodies only: under full linearity (§4.3.2)
    // every owned value is tracked, so a captured var passed as an owned
    // argument to ≥ 2 calls would violate rule 10 (e.g. a comprehension outer
    // var used in both the guard and result lambdas). Wrap each such use in
    // `copy` — non-consuming (§6.7) — so every call receives an independent
    // clone. User function bodies are left untouched: genuine double uses there
    // keep their natural rule-10 diagnostics.
    for s in &mut synthetics {
        fix_multi_arg_uses(&s.params, &mut s.body);
    }

    program.items.extend(synthetics.into_iter().map(Item::Func));
}

fn lift_in_expr(expr: &mut Expr, counter: &mut usize, out: &mut Vec<FuncDef>, top_names: &mut HashSet<String>) {
    match expr {
        Expr::Lambda { params, body, span } => {
            let mut body = std::mem::replace(body, Box::new(Expr::Int { value: 0, span: Span::empty(0) }));
            // Lift nested lambdas FIRST so their captured vars become explicit
            // references in the body. Then collect free vars from the lifted
            // body — this sees vars that were hidden inside inner lambdas.
            lift_in_expr(&mut body, counter, out, top_names);
            let free = collect_free(&body, params, top_names);

            let name = format!("__lambda_{}", counter);
            *counter += 1;

            // Param order: free vars FIRST (filled by the lambda-site partial
            // application), then the lambda's own params (filled when the
            // closure is later called). Matches the VM's `pre_args + new_args`
            // calling convention (wag-interp/src/eval.rs).
            let all_params: Vec<String> = {
                let mut v = free.to_vec();
                v.extend(params.iter().cloned());
                v
            };

            if std::env::var("WAG_DEBUG").is_ok() {
                eprintln!("[lambda_lift] {} = ({}) -> [free={:?}]", name, all_params.join(", "), free);
            }

            // Synthetic function: name all_params... = body
            let fn_def = FuncDef {
                name: name.clone(),
                properties: Vec::new(),
                signature: None,
                constraints: Vec::new(),
                params: all_params,
                body: *body,
                span: *span,
            };
            out.push(fn_def);
            // Register the synthetic name so enclosing lambdas don't capture it.
            top_names.insert(name.clone());

            // Replace the lambda with a reference or partial application.
            if free.is_empty() {
                *expr = Expr::Var { name, span: *span };
            } else {
                *expr = Expr::Apply {
                    fun: Box::new(Expr::Var { name, span: *span }),
                    args: free
                        .into_iter()
                        .map(|n| Expr::Var { name: n, span: *span })
                        .collect(),
                    span: *span,
                };
            }
        }
        // Recurse into all other expression forms.
        Expr::Apply { fun, args, .. } => {
            lift_in_expr(fun, counter, out, top_names);
            // Don't lift lambdas that are direct arguments to __monad_bind /
            // __monad_pure — they are handled inline by dispatch_monad during
            // type checking (the param type is known from the monadic value).
            let is_monad = matches!(&**fun, Expr::Upper { name, .. } if name == "__monad_bind" || name == "__monad_pure");
            for a in args {
                if is_monad && matches!(a, Expr::Lambda { .. }) {
                    // Keep this lambda inline; still lift nested lambdas in its body.
                    if let Expr::Lambda { body, .. } = a {
                        lift_in_expr(body, counter, out, top_names);
                    }
                } else {
                    lift_in_expr(a, counter, out, top_names);
                }
            }
        }
        Expr::BinOp { lhs, rhs, .. } => {
            lift_in_expr(lhs, counter, out, top_names);
            lift_in_expr(rhs, counter, out, top_names);
        }
        Expr::Field { base, .. } => lift_in_expr(base, counter, out, top_names),
        Expr::RecordUpdate { receiver, clauses, .. } => {
            lift_in_expr(receiver, counter, out, top_names);
            for c in clauses {
                lift_in_clause(c, counter, out, top_names);
            }
        }
        Expr::SetField { receiver, value, .. } => {
            lift_in_expr(receiver, counter, out, top_names);
            lift_in_expr(value, counter, out, top_names);
        }
        Expr::RegionWrite { receiver, value, .. } => {
            lift_in_expr(receiver, counter, out, top_names);
            lift_in_expr(value, counter, out, top_names);
        }
        Expr::Borrow { inner, .. } => lift_in_expr(inner, counter, out, top_names),
        Expr::Bind { value, body, .. } => {
            lift_in_expr(value, counter, out, top_names);
            lift_in_expr(body, counter, out, top_names);
        }
        Expr::Seq { first, second, .. } => {
            lift_in_expr(first, counter, out, top_names);
            lift_in_expr(second, counter, out, top_names);
        }
        Expr::If { arms, .. } => {
            for arm in arms {
                if let Some(c) = &mut arm.condition {
                    lift_in_expr(c, counter, out, top_names);
                }
                lift_in_expr(&mut arm.body, counter, out, top_names);
            }
        }
        Expr::Match { scrutinee, arms, .. } => {
            lift_in_expr(scrutinee, counter, out, top_names);
            for arm in arms {
                lift_in_expr(&mut arm.body, counter, out, top_names);
            }
        }
        Expr::Do { stmts, .. } => {
            for s in stmts {
                match s {
                    DoStmt::Bind { expr, .. } => lift_in_expr(expr, counter, out, top_names),
                    DoStmt::Return(e) => lift_in_expr(e, counter, out, top_names),
                }
            }
        }
        Expr::ListLit { elems, .. } => {
            for e in elems {
                lift_in_expr(e, counter, out, top_names);
            }
        }
        Expr::Comprehension { generators, accs, guards, result, .. } => {
            for g in generators {
                for s in &mut g.sources {
                    lift_in_expr(s, counter, out, top_names);
                }
            }
            for a in accs {
                lift_in_expr(&mut a.init, counter, out, top_names);
            }
            for g in guards {
                lift_in_expr(g, counter, out, top_names);
            }
            lift_in_expr(result, counter, out, top_names);
        }
        Expr::Product { elems, .. } => {
            for e in elems {
                lift_in_expr(e, counter, out, top_names);
            }
        }
        Expr::Send { target, msg, .. } => {
            lift_in_expr(target, counter, out, top_names);
            lift_in_expr(msg, counter, out, top_names);
        }
        // Atoms — nothing to do.
        Expr::Int { .. }
        | Expr::Str { .. }
        | Expr::Var { .. }
        | Expr::Upper { .. }
        | Expr::AtName { .. }
        | Expr::Self_ { .. }
        | Expr::Unit { .. } => {}
    }
}

fn lift_in_clause(clause: &mut UpdateClause, counter: &mut usize, out: &mut Vec<FuncDef>, top_names: &mut HashSet<String>) {
    match clause {
        UpdateClause::Leaf { value, .. } => lift_in_expr(value, counter, out, top_names),
        UpdateClause::Group { clauses, .. } => {
            for c in clauses {
                lift_in_clause(c, counter, out, top_names);
            }
        }
    }
}

/// Collect free variable names in `expr` that are not bound by `bound` and are
/// not top-level (always in scope, never capturable).
fn collect_free(expr: &Expr, bound: &[String], top_names: &HashSet<String>) -> Vec<String> {
    let mut all: Vec<String> = Vec::new();
    collect_var_refs(expr, &mut all);
    // Filter out bound vars (lambda params + any locally-bound vars) and top-level
    // names. Deduplicate by first occurrence (non-adjacent repeats must go too —
    // e.g. the same captured name used in two sub-lambdas would otherwise yield a
    // synthetic function with duplicate parameter names).
    let mut seen: HashSet<String> = HashSet::new();
    all.into_iter()
        // `copy` is a compiler builtin with the polymorphic scheme `&T -> T`
        // (freshly instantiated per use site). Capturing it as an owned argument
        // would force one monomorphic type for every use in the body, so it must
        // never be captured — synthetic bodies reference the builtin directly.
        .filter(|n| !bound.contains(n) && n != "_" && n != "copy" && !top_names.contains(n))
        .filter(|n| seen.insert(n.clone()))
        .collect()
}

/// Fix multi-use of captured vars in a synthetic body (see `lift_lambdas`).
/// Only the function's own parameters are considered: top-level names are not
/// linearity-tracked and never consumed as arguments.
fn fix_multi_arg_uses(params: &[String], body: &mut Expr) {
    let mut arg_counts: HashMap<String, usize> = HashMap::new();
    let mut fun_names: HashSet<String> = HashSet::new();
    count_arg_positions(body, &mut arg_counts, &mut fun_names);
    // A name used in callee position is a function value — untracked by
    // linearity (§5.5), never consumed as an argument, so it must not be wrapped.
    let multi: HashSet<String> = arg_counts
        .into_iter()
        .filter(|(name, count)| *count >= 2 && params.contains(name) && !fun_names.contains(name))
        .map(|(name, _)| name)
        .collect();
    if !multi.is_empty() {
        wrap_multi_arg_uses(body, &multi);
    }
}

/// Count direct-`Var` uses in argument position per name. Uses passed to the
/// `copy` builtin are non-consuming and excluded. Also record names used in
/// callee (fun) position.
fn count_arg_positions(
    expr: &Expr,
    counts: &mut HashMap<String, usize>,
    fun_names: &mut HashSet<String>,
) {
    match expr {
        Expr::Apply { fun, args, .. } => {
            if let Expr::Var { name, .. } = fun.as_ref() {
                fun_names.insert(name.clone());
            }
            let is_copy = matches!(fun.as_ref(), Expr::Var { name, .. } if name == "copy");
            for a in args {
                if !is_copy {
                    if let Expr::Var { name, .. } = a {
                        *counts.entry(name.clone()).or_insert(0) += 1;
                    }
                }
                count_arg_positions(a, counts, fun_names);
            }
            count_arg_positions(fun, counts, fun_names);
        }
        Expr::BinOp { lhs, rhs, .. } => {
            count_arg_positions(lhs, counts, fun_names);
            count_arg_positions(rhs, counts, fun_names);
        }
        Expr::Field { base, .. } => count_arg_positions(base, counts, fun_names),
        Expr::RecordUpdate { receiver, clauses, .. } => {
            count_arg_positions(receiver, counts, fun_names);
            for c in clauses {
                match c {
                    UpdateClause::Leaf { value, .. } => count_arg_positions(value, counts, fun_names),
                    UpdateClause::Group { clauses, .. } => {
                        for cc in clauses {
                            if let UpdateClause::Leaf { value, .. } = cc {
                                count_arg_positions(value, counts, fun_names);
                            }
                        }
                    }
                }
            }
        }
        Expr::SetField { receiver, value, .. } => {
            count_arg_positions(receiver, counts, fun_names);
            count_arg_positions(value, counts, fun_names);
        }
        Expr::RegionWrite { receiver, value, .. } => {
            count_arg_positions(receiver, counts, fun_names);
            count_arg_positions(value, counts, fun_names);
        }
        Expr::Borrow { inner, .. } => count_arg_positions(inner, counts, fun_names),
        Expr::Bind { value, body, .. } => {
            count_arg_positions(value, counts, fun_names);
            count_arg_positions(body, counts, fun_names);
        }
        Expr::Seq { first, second, .. } => {
            count_arg_positions(first, counts, fun_names);
            count_arg_positions(second, counts, fun_names);
        }
        Expr::If { arms, .. } => {
            for arm in arms {
                if let Some(c) = &arm.condition {
                    count_arg_positions(c, counts, fun_names);
                }
                count_arg_positions(&arm.body, counts, fun_names);
            }
        }
        Expr::Match { scrutinee, arms, .. } => {
            count_arg_positions(scrutinee, counts, fun_names);
            for arm in arms {
                count_arg_positions(&arm.body, counts, fun_names);
            }
        }
        Expr::Do { stmts, .. } => {
            for s in stmts {
                match s {
                    DoStmt::Bind { expr, .. } => count_arg_positions(expr, counts, fun_names),
                    DoStmt::Return(e) => count_arg_positions(e, counts, fun_names),
                }
            }
        }
        Expr::ListLit { elems, .. } => {
            for e in elems {
                count_arg_positions(e, counts, fun_names);
            }
        }
        Expr::Product { elems, .. } => {
            for e in elems {
                count_arg_positions(e, counts, fun_names);
            }
        }
        _ => {}
    }
}

/// Rewrite direct-`Var` uses of `multi` names in argument position to
/// `copy <var>` (non-consuming; each call gets an independent clone).
fn wrap_multi_arg_uses(expr: &mut Expr, multi: &HashSet<String>) {
    match expr {
        Expr::Apply { fun, args, .. } => {
            let is_copy = matches!(fun.as_ref(), Expr::Var { name, .. } if name == "copy");
            for a in args.iter_mut() {
                if !is_copy {
                    if let Expr::Var { name, span } = a {
                        if multi.contains(name) {
                            *a = Expr::Apply {
                                fun: Box::new(Expr::Var { name: "copy".into(), span: *span }),
                                args: vec![Expr::Var { name: name.clone(), span: *span }],
                                span: *span,
                            };
                        }
                    }
                }
                wrap_multi_arg_uses(a, multi);
            }
            wrap_multi_arg_uses(fun, multi);
        }
        Expr::BinOp { lhs, rhs, .. } => {
            wrap_multi_arg_uses(lhs, multi);
            wrap_multi_arg_uses(rhs, multi);
        }
        Expr::Field { base, .. } => wrap_multi_arg_uses(base, multi),
        Expr::RecordUpdate { receiver, clauses, .. } => {
            wrap_multi_arg_uses(receiver, multi);
            for c in clauses {
                match c {
                    UpdateClause::Leaf { value, .. } => wrap_multi_arg_uses(value, multi),
                    UpdateClause::Group { clauses, .. } => {
                        for cc in clauses {
                            if let UpdateClause::Leaf { value, .. } = cc {
                                wrap_multi_arg_uses(value, multi);
                            }
                        }
                    }
                }
            }
        }
        Expr::SetField { receiver, value, .. } => {
            wrap_multi_arg_uses(receiver, multi);
            wrap_multi_arg_uses(value, multi);
        }
        Expr::RegionWrite { receiver, value, .. } => {
            wrap_multi_arg_uses(receiver, multi);
            wrap_multi_arg_uses(value, multi);
        }
        Expr::Borrow { inner, .. } => wrap_multi_arg_uses(inner, multi),
        Expr::Bind { value, body, .. } => {
            wrap_multi_arg_uses(value, multi);
            wrap_multi_arg_uses(body, multi);
        }
        Expr::Seq { first, second, .. } => {
            wrap_multi_arg_uses(first, multi);
            wrap_multi_arg_uses(second, multi);
        }
        Expr::If { arms, .. } => {
            for arm in arms {
                if let Some(c) = &mut arm.condition {
                    wrap_multi_arg_uses(c, multi);
                }
                wrap_multi_arg_uses(&mut arm.body, multi);
            }
        }
        Expr::Match { scrutinee, arms, .. } => {
            wrap_multi_arg_uses(scrutinee, multi);
            for arm in arms {
                wrap_multi_arg_uses(&mut arm.body, multi);
            }
        }
        Expr::Do { stmts, .. } => {
            for s in stmts {
                match s {
                    DoStmt::Bind { expr, .. } => wrap_multi_arg_uses(expr, multi),
                    DoStmt::Return(e) => wrap_multi_arg_uses(e, multi),
                }
            }
        }
        Expr::ListLit { elems, .. } => {
            for e in elems {
                wrap_multi_arg_uses(e, multi);
            }
        }
        Expr::Product { elems, .. } => {
            for e in elems {
                wrap_multi_arg_uses(e, multi);
            }
        }
        _ => {}
    }
}

fn collect_var_refs(expr: &Expr, out: &mut Vec<String>) {
    match expr {
        Expr::Var { name, .. } => {
            if name != "_" {
                out.push(name.clone());
            }
        }
        Expr::Apply { fun, args, .. } => {
            collect_var_refs(fun, out);
            for a in args {
                collect_var_refs(a, out);
            }
        }
        Expr::BinOp { lhs, rhs, .. } => {
            collect_var_refs(lhs, out);
            collect_var_refs(rhs, out);
        }
        Expr::Field { base, .. } => collect_var_refs(base, out),
        Expr::RecordUpdate { receiver, clauses, .. } => {
            collect_var_refs(receiver, out);
            for c in clauses {
                collect_clause_refs(c, out);
            }
        }
        Expr::SetField { receiver, value, .. } => {
            collect_var_refs(receiver, out);
            collect_var_refs(value, out);
        }
        Expr::RegionWrite { receiver, value, .. } => {
            collect_var_refs(receiver, out);
            collect_var_refs(value, out);
        }
        Expr::Borrow { inner, .. } => collect_var_refs(inner, out),
        Expr::Bind { value, body, name, .. } => {
            collect_var_refs(value, out);
            // The bound name shadows in body — collect body refs then remove the bound name.
            let mut body_refs: Vec<String> = Vec::new();
            collect_var_refs(body, &mut body_refs);
            out.extend(body_refs.into_iter().filter(|n| n != name));
        }
        Expr::Seq { first, second, .. } => {
            collect_var_refs(first, out);
            collect_var_refs(second, out);
        }
        Expr::If { arms, .. } => {
            for arm in arms {
                if let Some(c) = &arm.condition {
                    collect_var_refs(c, out);
                }
                collect_var_refs(&arm.body, out);
            }
        }
        Expr::Match { scrutinee, arms, .. } => {
            collect_var_refs(scrutinee, out);
            for arm in arms {
                // Pattern-bound names shadow in body.
                let bound_names = pattern_names(&arm.pattern);
                let mut body_refs: Vec<String> = Vec::new();
                collect_var_refs(&arm.body, &mut body_refs);
                out.extend(body_refs.into_iter().filter(|n| !bound_names.iter().any(|bn| bn == n)));
            }
        }
        Expr::Do { stmts, .. } => {
            for s in stmts {
                match s {
                    DoStmt::Bind { expr, name } => {
                        collect_var_refs(expr, out);
                        // Subsequent stmts can see `name`, but we're collecting
                        // all refs flat, so we just skip the bound name.
                        let _ = name;
                    }
                    DoStmt::Return(e) => collect_var_refs(e, out),
                }
            }
        }
        Expr::ListLit { elems, .. } => {
            for e in elems {
                collect_var_refs(e, out);
            }
        }
        Expr::Send { target, msg, .. } => {
            collect_var_refs(target, out);
            collect_var_refs(msg, out);
        }
        Expr::Int { .. }
        | Expr::Str { .. }
        | Expr::Upper { .. }
        | Expr::AtName { .. }
        | Expr::Self_ { .. }
        | Expr::Unit { .. }
        | Expr::Lambda { .. } => {}
        Expr::Comprehension { generators, accs, guards, result, .. } => {
            for g in generators {
                for s in &g.sources {
                    collect_var_refs(s, out);
                }
            }
            for a in accs {
                collect_var_refs(&a.init, out);
            }
            for g in guards {
                collect_var_refs(g, out);
            }
            collect_var_refs(result, out);
        }
        Expr::Product { elems, .. } => {
            for e in elems {
                collect_var_refs(e, out);
            }
        }
    }
}

fn collect_clause_refs(clause: &UpdateClause, out: &mut Vec<String>) {
    match clause {
        UpdateClause::Leaf { value, .. } => collect_var_refs(value, out),
        UpdateClause::Group { clauses, .. } => {
            for c in clauses {
                collect_clause_refs(c, out);
            }
        }
    }
}

/// Collect all variable names bound by a pattern.
fn pattern_names(pat: &Pat) -> Vec<String> {
    let mut names = Vec::new();
    collect_pat_names(pat, &mut names);
    names
}

fn collect_pat_names(pat: &Pat, out: &mut Vec<String>) {
    match pat {
        Pat::Var { name, .. } => {
            if name != "_" {
                out.push(name.clone());
            }
        }
        Pat::Ctor { args, .. } => {
            for a in args {
                collect_pat_names(a, out);
            }
        }
        Pat::Product { elems, .. } => {
            for e in elems {
                collect_pat_names(e, out);
            }
        }
        Pat::Wildcard { .. } => {}
        // Literal patterns bind no variables (§9).
        Pat::Literal { .. } => {}
    }
}
