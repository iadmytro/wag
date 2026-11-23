//! Vec/Arr method-call desugaring: rewrite `Apply(Field(Var(v), name), args)`
//! into `Apply(Var(builtinName), [v, ...args])` for known Vec/Arr methods.
//!
//! Runs after `desugar_program` (do/list sugar) and before comprehension
//! desugaring. Only rewrites **unambiguous** method names — those that map to
//! exactly one builtin across Vec and Arr. Ambiguous names (`.get`, `.len`,
//! `.toList`) are left as field-access expressions; users should use the
//! standalone function form for those (`vecGet v i`, `arrLen a`).
//!
//! Mapping table:
//!   Vec-only:  `.push` → vecPush,  `.empty` → vecEmpty
//!   Arr-only:  `.new`  → arrNew,   `.set`   → arrSet,  `.fromList` → arrFromList

use crate::ast::*;

/// Desugar all Vec/Arr method calls in the program.
pub fn desugar_vec_arr_methods(program: &mut Program) {
    for item in &mut program.items {
        match item {
            Item::Func(f) => rewrite_expr(&mut f.body),
            Item::Actor(a) => rewrite_expr(&mut a.body),
            Item::Method(m) => rewrite_expr(&mut m.body),
            Item::Constructor(_) => {}
            Item::TypeAlias(_) | Item::ParametricType(_) | Item::ConstrainedDatatype(_) => {}
            Item::Class(_) | Item::Instance(_) => {}
        }
    }
}

/// Unambiguous Vec/Arr method name → builtin function name.
fn method_to_builtin(name: &str) -> Option<&'static str> {
    match name {
        // Vec-only
        "push" => Some("vecPush"),
        "empty" => Some("vecEmpty"),
        // Arr-only
        "new" => Some("arrNew"),
        "set" => Some("arrSet"),
        "fromList" => Some("arrFromList"),
        _ => None,
    }
}

fn rewrite_expr(expr: &mut Expr) {
    match expr {
        // `v.push x` → `vecPush v x`
        Expr::Apply { fun, args, span } => {
            if let Expr::Field { base, field, .. } = &**fun {
                if let Expr::Var { name, .. } = &**base {
                    if let Some(builtin) = method_to_builtin(&field) {
                        let mut new_args = Vec::with_capacity(args.len() + 1);
                        new_args.push(Expr::Var { name: name.clone(), span: *span });
                        new_args.extend(std::mem::take(args));
                        *expr = Expr::Apply {
                            fun: Box::new(Expr::Var { name: builtin.into(), span: *span }),
                            args: new_args,
                            span: *span,
                        };
                        return;
                    }
                }
            }
            // Recurse into the function position and all arguments.
            rewrite_expr(fun);
            for a in args {
                rewrite_expr(a);
            }
        }
        Expr::BinOp { lhs, rhs, .. } => {
            rewrite_expr(lhs);
            rewrite_expr(rhs);
        }
        Expr::Field { base, .. } => rewrite_expr(base),
        Expr::RecordUpdate { receiver, clauses, .. } => {
            rewrite_expr(receiver);
            for c in clauses {
                rewrite_clause(c);
            }
        }
        Expr::SetField { receiver, value, .. } => {
            rewrite_expr(receiver);
            rewrite_expr(value);
        }
        Expr::RegionWrite { receiver, value, .. } => {
            rewrite_expr(receiver);
            rewrite_expr(value);
        }
        Expr::Borrow { inner, .. } => rewrite_expr(inner),
        Expr::Bind { value, body, .. } => {
            rewrite_expr(value);
            rewrite_expr(body);
        }
        Expr::Seq { first, second, .. } => {
            rewrite_expr(first);
            rewrite_expr(second);
        }
        Expr::If { arms, .. } => {
            for arm in arms {
                if let Some(c) = &mut arm.condition {
                    rewrite_expr(c);
                }
                rewrite_expr(&mut arm.body);
            }
        }
        Expr::Match { scrutinee, arms, .. } => {
            rewrite_expr(scrutinee);
            for arm in arms {
                rewrite_expr(&mut arm.body);
            }
        }
        Expr::Do { stmts, .. } => {
            for s in stmts {
                match s {
                    DoStmt::Bind { expr, .. } => rewrite_expr(expr),
                    DoStmt::Return(e) => rewrite_expr(e),
                }
            }
        }
        Expr::ListLit { elems, .. } => {
            for e in elems {
                rewrite_expr(e);
            }
        }
        Expr::Lambda { body, .. } => rewrite_expr(body),
        Expr::Product { elems, .. } => {
            for e in elems {
                rewrite_expr(e);
            }
        }
        Expr::Comprehension { generators, accs, guards, result, .. } => {
            for g in generators {
                for s in &mut g.sources {
                    rewrite_expr(s);
                }
            }
            for a in accs {
                rewrite_expr(&mut a.init);
            }
            for g in guards {
                rewrite_expr(g);
            }
            rewrite_expr(result);
        }
        Expr::Send { target, msg, .. } => {
            rewrite_expr(target);
            rewrite_expr(msg);
        }
        Expr::Int { .. } | Expr::Str { .. } | Expr::Var { .. }
        | Expr::Upper { .. } | Expr::AtName { .. } | Expr::Self_ { .. }
        | Expr::Unit { .. } => {}
    }
}

fn rewrite_clause(clause: &mut UpdateClause) {
    match clause {
        UpdateClause::Leaf { value, .. } => rewrite_expr(value),
        UpdateClause::Group { clauses, .. } => {
            for c in clauses {
                rewrite_clause(c);
            }
        }
    }
}
