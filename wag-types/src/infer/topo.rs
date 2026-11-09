//! Call graph construction and topological ordering (D044).

use std::collections::HashMap;

use wag_syntax::{Expr, UpdateClause};

pub(super) fn build_call_graph(
    funcs: &[wag_syntax::FuncDef],
) -> HashMap<String, std::collections::BTreeSet<String>> {
    let top_names: std::collections::HashSet<&str> =
        funcs.iter().map(|f| f.name.as_str()).collect();
    let mut graph: HashMap<String, std::collections::BTreeSet<String>> = HashMap::new();
    for f in funcs {
        let mut callees = std::collections::BTreeSet::new();
        collect_calls(&f.body, &top_names, &mut callees);
        graph.insert(f.name.clone(), callees);
    }
    graph
}

pub(super) fn collect_calls(
    expr: &Expr,
    top_names: &std::collections::HashSet<&str>,
    out: &mut std::collections::BTreeSet<String>,
) {
    match expr {
        Expr::Apply { fun, args, .. } => {
            if let Expr::Var { name, .. } = fun.as_ref() {
                if top_names.contains(name.as_str()) {
                    out.insert(name.clone());
                }
            }
            collect_calls(fun, top_names, out);
            for a in args {
                collect_calls(a, top_names, out);
            }
        }
        Expr::Int { .. }
        | Expr::Str { .. }
        | Expr::Var { .. }
        | Expr::Upper { .. }
        | Expr::AtName { .. }
        | Expr::Self_ { .. }
        | Expr::Unit { .. } => {}
        Expr::BinOp { lhs, rhs, .. } => {
            collect_calls(lhs, top_names, out);
            collect_calls(rhs, top_names, out);
        }
        Expr::Field { base, .. } => collect_calls(base, top_names, out),
        Expr::RecordUpdate {
            receiver, clauses, ..
        } => {
            collect_calls(receiver, top_names, out);
            for c in clauses {
                match c {
                    UpdateClause::Leaf { value, .. } => collect_calls(value, top_names, out),
                    UpdateClause::Group { clauses: inner, .. } => {
                        for ic in inner {
                            if let UpdateClause::Leaf { value, .. } = ic {
                                collect_calls(value, top_names, out);
                            }
                        }
                    }
                }
            }
        }
        Expr::SetField {
            receiver, value, ..
        } => {
            collect_calls(receiver, top_names, out);
            collect_calls(value, top_names, out);
        }
        Expr::RegionWrite {
            receiver, value, ..
        } => {
            collect_calls(receiver, top_names, out);
            collect_calls(value, top_names, out);
        }
        Expr::Borrow { inner, .. } => collect_calls(inner, top_names, out),
        Expr::Bind { value, body, .. } => {
            collect_calls(value, top_names, out);
            collect_calls(body, top_names, out);
        }
        Expr::Seq { first, second, .. } => {
            collect_calls(first, top_names, out);
            collect_calls(second, top_names, out);
        }
        Expr::If { arms, .. } => {
            for arm in arms {
                if let Some(cond) = &arm.condition {
                    collect_calls(cond, top_names, out);
                }
                collect_calls(&arm.body, top_names, out);
            }
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            collect_calls(scrutinee, top_names, out);
            for arm in arms {
                collect_calls(&arm.body, top_names, out);
            }
        }
        Expr::Do { .. } => {}
        Expr::ListLit { elems, .. } => {
            for e in elems {
                collect_calls(e, top_names, out);
            }
        }
        Expr::Lambda { body, .. } => {
            collect_calls(body, top_names, out);
        }
        Expr::Comprehension {
            generators,
            accs,
            guards,
            result,
            ..
        } => {
            for g in generators {
                for s in &g.sources {
                    collect_calls(s, top_names, out);
                }
            }
            for a in accs {
                collect_calls(&a.init, top_names, out);
            }
            for g in guards {
                collect_calls(g, top_names, out);
            }
            collect_calls(result, top_names, out);
        }
        Expr::Product { elems, .. } => {
            for e in elems {
                collect_calls(e, top_names, out);
            }
        }
        Expr::Send { target, msg, .. } => {
            collect_calls(target, top_names, out);
            collect_calls(msg, top_names, out);
        }
    }
}

/// Check whether `name` is in a mutual-recursion cycle with at least one other
/// top-level function. Self-recursion alone does NOT count (singleton SCC).
/// Compute inference order: callees before callers (post-order DFS on call graph).
/// Self-recursive functions are handled by the visited set (no infinite loop).
/// Ties are broken by source order (lower index visited first).
pub(super) fn topo_infer_order(
    funcs: &[wag_syntax::FuncDef],
    call_graph: &HashMap<String, std::collections::BTreeSet<String>>,
) -> Vec<usize> {
    let name_to_idx: HashMap<&str, usize> = funcs
        .iter()
        .enumerate()
        .map(|(i, f)| (f.name.as_str(), i))
        .collect();
    let n = funcs.len();
    let mut visited = vec![false; n];
    let mut order: Vec<usize> = Vec::with_capacity(n);
    for start in 0..n {
        if !visited[start] {
            dfs_topo(
                start,
                funcs,
                &name_to_idx,
                call_graph,
                &mut visited,
                &mut order,
            );
        }
    }
    order
}

pub(super) fn dfs_topo(
    idx: usize,
    funcs: &[wag_syntax::FuncDef],
    name_to_idx: &HashMap<&str, usize>,
    call_graph: &HashMap<String, std::collections::BTreeSet<String>>,
    visited: &mut Vec<bool>,
    order: &mut Vec<usize>,
) {
    if visited[idx] {
        return;
    }
    visited[idx] = true;
    let name = &funcs[idx].name;
    if let Some(callees) = call_graph.get(name) {
        for callee in callees {
            if let Some(&j) = name_to_idx.get(callee.as_str()) {
                dfs_topo(j, funcs, name_to_idx, call_graph, visited, order);
            }
        }
    }
    order.push(idx);
}

pub(super) fn is_in_cycle(
    name: &str,
    call_graph: &HashMap<String, std::collections::BTreeSet<String>>,
) -> bool {
    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut stack: Vec<String> = Vec::new();
    // Seed: all direct callees of `name` except itself.
    if let Some(callees) = call_graph.get(name) {
        for c in callees {
            if c != name {
                stack.push(c.clone());
            }
        }
    }
    while let Some(current) = stack.pop() {
        if current == name {
            return true; // cycle found
        }
        if !visited.insert(current.clone()) {
            continue;
        }
        if let Some(callees) = call_graph.get(&current) {
            for c in callees {
                if !visited.contains(c) {
                    stack.push(c.clone());
                }
            }
        }
    }
    false
}
