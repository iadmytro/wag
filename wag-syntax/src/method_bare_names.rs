//! D060: single early desugar of bare atoms in method bodies.
//!
//! Bare atoms in a method body resolve against the receiver (D057): fields
//! (`tail`, `len`) — local bindings shadow them. This pass rewrites every
//! unbound bare atom to explicit form (`tail` → `Self_.tail`). After this
//! pass no other path exists for a name (the per-pass fallbacks in inference /
//! checker / VM were removed); a typo'd field surfaces at inference as an
//! unknown-field error on the receiver type.
//!
//! Stateless by design: method bodies are call-free (D059), so any unbound
//! bare atom can only be intended as a receiver field — no datatype or
//! cross-module information is needed, and the pass runs per module in a
//! strictly sequential pipeline (right after parse, before `wag-types`).

use std::collections::HashSet;

use crate::ast::*;

/// Rewrite every unbound bare atom in each method body of `program` to
/// explicit `Self_.field` form. Bound names (params, lets, match patterns)
/// are left alone and shadow receiver fields.
pub fn desugar_method_bare_names(program: &mut Program) {
    for item in program.items.iter_mut() {
        if let Item::Method(m) = item {
            // Arr methods may call builtins (D059 exemption) — bare atoms are
            // not guaranteed to be receiver fields, so skip the rewrite.
            if m.type_name == "Arr" {
                continue;
            }
            // Method params are bound names — they shadow receiver fields.
            let mut locals: HashSet<String> = m.params.iter().cloned().collect();
            rewrite_expr(&mut m.body, &mut locals);
        }
    }
}

fn rewrite_expr(expr: &mut Expr, locals: &mut HashSet<String>) {
    match expr {
        Expr::Var { name, span } => {
            if !locals.contains(name.as_str()) && name != "_" {
                *expr = Expr::Field {
                    base: Box::new(Expr::Self_ { span: *span }),
                    field: name.clone(),
                    span: *span,
                };
            }
        }
        Expr::Apply { fun, args, .. } => {
            rewrite_expr(fun, locals);
            for a in args.iter_mut() {
                rewrite_expr(a, locals);
            }
        }
        Expr::BinOp { lhs, rhs, .. } => {
            rewrite_expr(lhs, locals);
            rewrite_expr(rhs, locals);
        }
        Expr::Field { base, .. } | Expr::Borrow { inner: base, .. } => {
            // Field labels are not atoms — only the base expression can hold bare names.
            rewrite_expr(base, locals);
        }
        Expr::RecordUpdate { receiver, clauses, .. } => {
            rewrite_expr(receiver, locals);
            for c in clauses.iter_mut() {
                match c {
                    UpdateClause::Leaf { value, .. } => rewrite_expr(value, locals),
                    UpdateClause::Group { clauses: inner, .. } => {
                        for c2 in inner.iter_mut() {
                            if let UpdateClause::Leaf { value, .. } = c2 {
                                rewrite_expr(value, locals);
                            }
                        }
                    }
                }
            }
        }
        Expr::SetField { receiver, value, .. }
        | Expr::RegionWrite { receiver, value, .. } => {
            // Checker-emitted nodes cannot exist at this stage; recurse defensively.
            rewrite_expr(receiver, locals);
            rewrite_expr(value, locals);
        }
        Expr::Bind { name, value, body, .. } => {
            rewrite_expr(value, locals);
            let fresh = locals.insert(name.clone());
            rewrite_expr(body, locals);
            if fresh {
                locals.remove(name);
            }
        }
        Expr::Seq { first, second, .. } => {
            rewrite_expr(first, locals);
            rewrite_expr(second, locals);
        }
        Expr::If { arms, .. } => {
            for arm in arms.iter_mut() {
                if let Some(cond) = &mut arm.condition {
                    rewrite_expr(cond, locals);
                }
                rewrite_expr(&mut arm.body, locals);
            }
        }
        Expr::Match { scrutinee, arms, .. } => {
            rewrite_expr(scrutinee, locals);
            for arm in arms.iter_mut() {
                let mut saved = Vec::new();
                bind_pat(&arm.pattern, locals, &mut saved);
                rewrite_expr(&mut arm.body, locals);
                restore(locals, saved);
            }
        }
        Expr::Do { stmts, .. } => {
            // `do` desugars to nested Let before this pass runs; handled for completeness.
            let mut saved = Vec::new();
            for s in stmts.iter_mut() {
                match s {
                    DoStmt::Bind { name, expr } => {
                        rewrite_expr(expr, locals);
                        saved.push((name.clone(), locals.insert(name.clone())));
                    }
                    DoStmt::Return(e) => rewrite_expr(e, locals),
                }
            }
            restore(locals, saved);
        }
        Expr::ListLit { elems, .. } | Expr::Product { elems, .. } => {
            for e in elems.iter_mut() {
                rewrite_expr(e, locals);
            }
        }
        Expr::Lambda { params, body, .. } => {
            let mut saved = Vec::new();
            for p in params {
                saved.push((p.clone(), locals.insert(p.clone())));
            }
            rewrite_expr(body, locals);
            restore(locals, saved);
        }
        Expr::Comprehension { generators, accs, guards, result, .. } => {
            for g in generators.iter_mut() {
                for s in g.sources.iter_mut() {
                    rewrite_expr(s, locals);
                }
            }
            let mut saved = Vec::new();
            for a in accs.iter_mut() {
                rewrite_expr(&mut a.init, locals);
                saved.push((a.name.clone(), locals.insert(a.name.clone())));
            }
            // Generator vars are bound for guards/result.
            for g in &*generators {
                for v in &g.vars {
                    saved.push((v.clone(), locals.insert(v.clone())));
                }
            }
            for guard in guards.iter_mut() {
                rewrite_expr(guard, locals);
            }
            rewrite_expr(result, locals);
            restore(locals, saved);
        }
        // Int, Str, Upper, AtName, Self_ — no bare atoms.
        _ => {}
    }
}

fn bind_pat(pat: &Pat, locals: &mut HashSet<String>, saved: &mut Vec<(String, bool)>) {
    match pat {
        Pat::Var { name, .. } => saved.push((name.clone(), locals.insert(name.clone()))),
        Pat::Ctor { args, .. } => {
            for a in args {
                bind_pat(a, locals, saved);
            }
        }
        Pat::Product { elems, .. } => {
            for e in elems {
                bind_pat(e, locals, saved);
            }
        }
        Pat::Wildcard { .. } | Pat::Literal { .. } => {}
    }
}

fn restore(locals: &mut HashSet<String>, saved: Vec<(String, bool)>) {
    for (name, fresh) in saved.into_iter().rev() {
        if fresh {
            locals.remove(&name);
        }
    }
}
