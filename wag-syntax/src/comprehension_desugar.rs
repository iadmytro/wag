//! Comprehension desugaring: rewrite `Expr::Comprehension` into
//! `map`/`filter`/`flatMap`/`fold` + lambda expressions.
//!
//! Runs after lambda lifting (so lambdas in the output are already lifted),
//! but we emit `Expr::Lambda` nodes which get lifted by a second pass.
//! Actually, the order is: desugar → lambda_lift. So we emit Lambdas here.

use crate::ast::*;
use crate::Span;

/// Desugar all comprehensions in the program.
/// Must be called BEFORE lambda lifting.
pub fn desugar_comprehensions(program: &mut Program) {
    for item in &mut program.items {
        match item {
            Item::Func(f) => desugar_in_expr(&mut f.body),
            Item::Actor(a) => desugar_in_expr(&mut a.body),
            Item::Method(m) => desugar_in_expr(&mut m.body),
            Item::Constructor(_) => {}
            Item::TypeAlias(_) | Item::ParametricType(_) => {}
            Item::Class(_) | Item::Instance(_) => {}
        }
    }
}

fn desugar_in_expr(expr: &mut Expr) {
    match expr {
        Expr::Comprehension { generators, accs, guards, result, span } => {
            let span = *span;
            let generators = std::mem::take(generators);
            let accs = std::mem::take(accs);
            let guards = std::mem::take(guards);
            let result = Box::as_ref(result).clone();
            *expr = desugar_comprehension(&generators, &accs, &guards, &result, span);
            // Desugar any nested comprehensions in the result.
            desugar_in_expr(expr);
        }
        // Recurse into all other expression forms.
        Expr::Apply { fun, args, .. } => {
            desugar_in_expr(fun);
            for a in args { desugar_in_expr(a); }
        }
        Expr::BinOp { lhs, rhs, .. } => {
            desugar_in_expr(lhs);
            desugar_in_expr(rhs);
        }
        Expr::Field { base, .. } => desugar_in_expr(base),
        Expr::RecordUpdate { receiver, clauses, .. } => {
            desugar_in_expr(receiver);
            for c in clauses { desugar_in_clause(c); }
        }
        Expr::SetField { receiver, value, .. } => {
            desugar_in_expr(receiver);
            desugar_in_expr(value);
        }
        Expr::RegionWrite { receiver, value, .. } => {
            desugar_in_expr(receiver);
            desugar_in_expr(value);
        }
        Expr::Borrow { inner, .. } => desugar_in_expr(inner),
        Expr::Bind { value, body, .. } => {
            desugar_in_expr(value);
            desugar_in_expr(body);
        }
        Expr::Seq { first, second, .. } => {
            desugar_in_expr(first);
            desugar_in_expr(second);
        }
        Expr::If { arms, .. } => {
            for arm in arms {
                if let Some(c) = &mut arm.condition { desugar_in_expr(c); }
                desugar_in_expr(&mut arm.body);
            }
        }
        Expr::Match { scrutinee, arms, .. } => {
            desugar_in_expr(scrutinee);
            for arm in arms { desugar_in_expr(&mut arm.body); }
        }
        Expr::Do { stmts, .. } => {
            for s in stmts {
                match s {
                    DoStmt::Bind { expr, .. } => desugar_in_expr(expr),
                    DoStmt::Return(e) => desugar_in_expr(e),
                }
            }
        }
        Expr::ListLit { elems, .. } => {
            for e in elems { desugar_in_expr(e); }
        }
        Expr::Lambda { body, .. } => desugar_in_expr(body),
        Expr::Product { elems, .. } => {
            for e in elems { desugar_in_expr(e); }
        }
        Expr::Send { target, msg, .. } => {
            desugar_in_expr(target);
            desugar_in_expr(msg);
        }
        Expr::Int { .. } | Expr::Str { .. } | Expr::Var { .. }
        | Expr::Upper { .. } | Expr::AtName { .. } | Expr::Self_ { .. }
        | Expr::Unit { .. } => {}
    }
}

fn desugar_in_clause(clause: &mut UpdateClause) {
    match clause {
        UpdateClause::Leaf { value, .. } => desugar_in_expr(value),
        UpdateClause::Group { clauses, .. } => {
            for c in clauses { desugar_in_clause(c); }
        }
    }
}

/// Core desugaring logic.
fn desugar_comprehension(
    generators: &[Generator],
    accs: &[AccInit],
    guards: &[Expr],
    result: &Expr,
    span: Span,
) -> Expr {
    // Check for wildcard guard (no filter).
    let has_wildcard = guards.len() == 1 && is_wildcard(&guards[0]);
    let effective_guards: Vec<&Expr> = if has_wildcard { vec![] } else { guards.iter().collect() };

    if accs.is_empty() {
        // List comprehension.
        desugar_list_comp(generators, &effective_guards, result, span)
    } else {
        // Fold comprehension.
        desugar_fold_comp(generators, accs, &effective_guards, result, span)
    }
}

fn is_wildcard(e: &Expr) -> bool {
    matches!(e, Expr::Var { name, .. } if name == "_")
}

/// List comprehension: no accs.
fn desugar_list_comp(
    generators: &[Generator],
    guards: &[&Expr],
    result: &Expr,
    span: Span,
) -> Expr {
    if generators.len() == 1 {
        let gen = &generators[0];
        // Single generator: map/filter on the source.
        if gen.vars.len() == 1 {
            let var = &gen.vars[0];
            let source = &gen.sources[0];

            // Build: map (var -> result) (filter (var -> g1 && g2 && ...) (copy source))
            // — the traversal consumes a copy; the original list survives (§6.7).
            let mut inner = non_consuming_source(source, span);
            if !guards.is_empty() {
                for g in guards.iter().rev() {
                    let lambda = Expr::Lambda {
                        params: vec![var.clone()],
                        body: Box::new((*g).clone()),
                        span,
                    };
                    inner = apply("filter", vec![lambda, inner], span);
                }
            }
            let map_lambda = Expr::Lambda {
                params: vec![var.clone()],
                body: Box::new(result.clone()),
                span,
            };
            apply("map", vec![map_lambda, inner], span)
        } else {
            // Zip: multiple vars, multiple sources.
            // zip s1 s2 ... → List (Pair (Pair a b) c) ...
            // For simplicity, handle 2-element zip.
            desugar_zip_comp(generators[0].clone(), guards, result, span)
        }
    } else {
        // Cartesian product: nested flatMap.
        desugar_cartesian(generators, guards, result, span)
    }
}

/// Zip comprehension: `[x, y, ... <- xs, ys, ... | g -> e]`
///
/// Desugars to a right-nested `zip` (yielding nested `Pair`s) + `map`/`filter`
/// whose lambdas destructure the pairs via nested `match` arms. `Pair` is a
/// single-constructor type, so each match arm is exhaustive-by-construction (§3.1).
fn desugar_zip_comp(
    gen: Generator,
    guards: &[&Expr],
    result: &Expr,
    span: Span,
) -> Expr {
    let vars = &gen.vars;
    // Precondition (enforced at parse time): vars.len() == sources.len() >= 2.
    debug_assert!(vars.len() == gen.sources.len() && vars.len() >= 2);

    // zip s1 (zip s2 (zip s3 ... sn)), filtered per guard, then mapped.
    let zipped = nested_zip(&gen.sources, span);
    let inner = apply_filters(&zipped, vars, guards, span);
    let map_lambda = zip_lambda(vars, result.clone(), span);
    apply("map", vec![map_lambda, inner], span)
}

/// Wrap `inner` in one `filter` layer per guard (applied innermost-first).
/// Each guard lambda binds `vars`: `(v -> g)` for a single var, or
/// `(p -> match p { … -> g })` for a zip generator.
fn apply_filters(inner: &Expr, vars: &[String], guards: &[&Expr], span: Span) -> Expr {
    let mut expr = inner.clone();
    for g in guards.iter().rev() {
        let lambda = if vars.len() == 1 {
            Expr::Lambda {
                params: vec![vars[0].clone()],
                body: Box::new((*g).clone()),
                span,
            }
        } else {
            zip_lambda(vars, (*g).clone(), span)
        };
        expr = apply("filter", vec![lambda, expr], span);
    }
    expr
}

/// `zip s1 (zip s2 (... (zip s(n-1) sn)))` — right-associative nested zip.
fn nested_zip(sources: &[Expr], span: Span) -> Expr {
    match sources.len() {
        0 => unreachable!("zip comprehension needs at least one source"),
        // Each source is traversed over a copy (§6.7); `zip` consumes its args.
        1 => non_consuming_source(&sources[0], span),
        _ => apply(
            "zip",
            vec![non_consuming_source(&sources[0], span), nested_zip(&sources[1..], span)],
            span,
        ),
    }
}

/// Build `(p -> <destructure p into `vars`, then `body`>)`.
fn zip_lambda(vars: &[String], body: Expr, span: Span) -> Expr {
    let p = "__zip".to_string();
    let scrutinee = Expr::Var { name: p.clone(), span };
    Expr::Lambda {
        params: vec![p],
        body: Box::new(nested_match(scrutinee, vars, body, span)),
        span,
    }
}

/// Destructure a nested-`Pair` value, binding `vars[0..]` in order and
/// evaluating `body`. Produces nested `match` arms:
///   - 2 vars:  `match s { Pair v1 v2 -> body }`
///   - n > 2:   `match s { Pair v1 r -> match r { Pair v2 ... -> body } }`
fn nested_match(scrutinee: Expr, vars: &[String], body: Expr, span: Span) -> Expr {
    let arm = |pattern: Pat, b: Expr| MatchArm { pattern, body: b };
    match vars.len() {
        0 => body,
        1 => Expr::Match {
            scrutinee: Box::new(scrutinee),
            arms: vec![arm(Pat::Var { name: vars[0].clone(), span }, body)],
            span,
        },
        2 => {
            let p1 = Pat::Var { name: vars[0].clone(), span };
            let p2 = Pat::Var { name: vars[1].clone(), span };
            let ctor = Pat::Ctor { name: "Pair".into(), qualifier: None, args: vec![p1, p2], span };
            Expr::Match {
                scrutinee: Box::new(scrutinee),
                arms: vec![arm(ctor, body)],
                span,
            }
        }
        _ => {
            let rest = "__zip_rest".to_string();
            let head = Pat::Var { name: vars[0].clone(), span };
            let rest_pat = Pat::Var { name: rest.clone(), span };
            let ctor = Pat::Ctor { name: "Pair".into(), qualifier: None, args: vec![head, rest_pat], span };
            let inner = nested_match(Expr::Var { name: rest, span }, &vars[1..], body, span);
            Expr::Match {
                scrutinee: Box::new(scrutinee),
                arms: vec![arm(ctor, inner)],
                span,
            }
        }
    }
}

/// Cartesian product: `[x <- xs, y <- ys | g -> e]`
fn desugar_cartesian(
    generators: &[Generator],
    guards: &[&Expr],
    result: &Expr,
    span: Span,
) -> Expr {
    // flatMap (x -> [y <- ys | g -> e]) xs
    // Recursively desugar the inner comprehension.
    let outer_gen = &generators[0];
    let inner_gens = &generators[1..];

    // Build inner comprehension (without the outer generator). If the outer
    // var is used in several lambdas inside `inner` (guard + result), the
    // multi-use is resolved by lambda lifting's post-lift pass, which wraps
    // each owned-argument use of a captured var in `copy` (§6.7).
    let inner = desugar_list_comp(inner_gens, guards, result, span);

    // Wrap in flatMap with a lambda binding the outer var.
    let var = &outer_gen.vars[0];
    let lambda = Expr::Lambda {
        params: vec![var.clone()],
        body: Box::new(inner),
        span,
    };
    apply("flatMap", vec![lambda, non_consuming_source(&outer_gen.sources[0], span)], span)
}

/// Fold comprehension: `fold (x, acc -> result) init source`, where the
/// generator may be a zip (`[x, y <- xs, ys | …]` → element is a nested `Pair`
/// destructured via `match`) and guards filter the source first.
///
/// Preconditions (enforced at parse time, which owns the supported-form
/// validation — this pass has no error channel):
///   - exactly one generator (cartesian fold rejected),
///   - exactly one accumulator (multi-acc fold rejected).
fn desugar_fold_comp(
    generators: &[Generator],
    accs: &[AccInit],
    guards: &[&Expr],
    result: &Expr,
    span: Span,
) -> Expr {
    debug_assert!(generators.len() == 1, "cartesian fold is rejected at parse time");
    debug_assert!(accs.len() == 1, "multi-acc fold is rejected at parse time");

    let gen = &generators[0];
    let vars = &gen.vars;
    let acc_name = &accs[0].name;
    let acc_init = &accs[0].init;

    // Element domain: the source itself (single var) or a nested zip (multi
    // var). Traversal consumes a copy; the original list survives (§6.7).
    let domain = if vars.len() == 1 {
        non_consuming_source(&gen.sources[0], span)
    } else {
        nested_zip(&gen.sources, span)
    };
    let inner = apply_filters(&domain, vars, guards, span);

    // Accumulator lambda: (x, acc -> result) or (p, acc -> match p { … -> result }).
    let fold_lambda = if vars.len() == 1 {
        Expr::Lambda {
            params: vec![vars[0].clone(), acc_name.clone()],
            body: Box::new(result.clone()),
            span,
        }
    } else {
        let p = "__zip".to_string();
        Expr::Lambda {
            params: vec![p.clone(), acc_name.clone()],
            body: Box::new(nested_match(
                Expr::Var { name: p, span },
                vars,
                result.clone(),
                span,
            )),
            span,
        }
    };

    apply("fold", vec![fold_lambda, acc_init.clone(), inner], span)
}

/// Helper: build `name arg1 arg2 ...` application.
fn apply(name: &str, args: Vec<Expr>, span: Span) -> Expr {
    let fun = Expr::Var { name: name.into(), span };
    Expr::Apply {
        fun: Box::new(fun),
        args,
        span,
    }
}

/// Comprehension traversal must not consume the source list (§6.7): wrap each
/// generator source in `copy` so the original binding survives the traversal.
fn non_consuming_source(source: &Expr, span: Span) -> Expr {
    apply("copy", vec![source.clone()], span)

}
