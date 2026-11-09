//! Whole-program tail-cycle law (§7 rule 1).

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use wag_syntax::{Expr, Item, Program, Span};
use wag_types::{Ty, TypedProgram};

use crate::error::{CheckError, Rule};

/// An edge in the call graph: `from` calls `to` at `span`, optionally in tail
/// position. Higher-order edges (a flow-analysis artifact for an application
/// of a function parameter) carry `is_ho = true`; their span points where the
/// parameter is applied, which may be library/prelude code.
#[derive(Debug, Clone)]
pub struct CallEdge {
    pub from: String,
    pub to: String,
    pub is_tail: bool,
    pub span: Span,
    pub is_ho: bool,
}

/// Result of the rule-1 analysis.
#[derive(Debug, Clone, Default)]
pub struct TailCycleInfo {
    /// Every named function/actor that participates in a call-graph cycle.
    pub cyclic_fns: BTreeSet<String>,
    /// All collected call edges (including higher-order conservative edges).
    pub edges: Vec<CallEdge>,
}

/// Rule-1 analysis with module attribution (D067): `name_module` maps each
/// top-level function/actor name to its originating module; a cycle error is
/// stamped with the anchor edge's source-function module.
pub fn check_tail_cycles_with(
    program: &Program,
    typed: &TypedProgram,
    name_module: &HashMap<String, String>,
) -> Result<TailCycleInfo, CheckError> {
    let tops = top_level_names(program);
    let ho_params = higher_order_params(program, typed);
    let flow = compute_ho_flow(program, &tops, &ho_params);
    let edges = collect_edges(program, &tops, &ho_params, &flow);
    let cyclic_fns = scc_cyclic_nodes(&edges);

    // Offending edges: non-tail calls whose endpoints share an SCC. The
    // diagnostic anchors at the first such edge's call site (its span).
    // Direct edges are preferred over higher-order ones: a direct span is a
    // concrete call site, while an HO span points where a parameter is
    // applied — often prelude/library code the user cannot change.
    let mut bad: Vec<(String, Span, bool)> = Vec::new(); // (label, span, is_ho)
    for e in &edges {
        if !e.is_tail
            && cyclic_fns.contains(&e.from)
            && cyclic_fns.contains(&e.to)
            && same_scc(&e.from, &e.to, &edges)
        {
            bad.push((format!("{} -> {}", e.from, e.to), e.span, e.is_ho));
        }
    }
    // Anchor at the first direct offending edge; fall back to any (HO) edge.
    if let Some((first, span, _)) = bad
        .iter()
        .find(|(_, _, ho)| !ho)
        .or_else(|| bad.first())
    {
        // `first` is `"from -> to"`; attribute the error to `from`'s module.
        let from = first.split(" -> ").next().unwrap_or("");
        let mut err = CheckError::new(
            Rule::TailCycle,
            *span,
            format!("non-tail call `{}` lies on a call-graph cycle", first),
        );
        if let Some(m) = name_module.get(from) {
            err.module = Some(m.clone());
        }
        return Err(err);
    }

    Ok(TailCycleInfo { cyclic_fns, edges })
}

fn top_level_names(program: &Program) -> HashSet<String> {
    let mut s = HashSet::new();
    for item in &program.items {
        match item {
            Item::Func(f) => {
                s.insert(f.name.clone());
            }
            Item::Actor(a) => {
                s.insert(a.name.clone());
            }
            _ => {}
        }
    }
    s
}

/// Map function name → list of (param_index, param_name) for Fun-typed parameters.
fn higher_order_params(
    program: &Program,
    typed: &TypedProgram,
) -> HashMap<String, Vec<(usize, String)>> {
    let mut m = HashMap::new();
    for item in &program.items {
        let (name, params, key) = match item {
            Item::Func(f) => (&f.name, &f.params, (f.name.clone(), f.params.len())),
            // Actor behavior is keyed `("@name", arity)` (separate namespace, §2).
            Item::Actor(a) => (&a.name, &a.params, (format!("@{}", a.name), a.params.len())),
            _ => continue,
        };
        let Some(Ty::Fun(ps, _)) = typed.bindings.get(&key) else {
            continue;
        };
        let mut hos = Vec::new();
        for (i, p) in params.iter().enumerate() {
            if i < ps.len() && matches!(ps[i], Ty::Fun(_, _)) {
                hos.push((i, p.clone()));
            }
        }
        if !hos.is_empty() {
            m.insert(name.clone(), hos);
        }
    }
    m
}

/// flow[fn][param_idx] = set of top-level function names that may flow there.
fn compute_ho_flow(
    program: &Program,
    tops: &HashSet<String>,
    ho_params: &HashMap<String, Vec<(usize, String)>>,
) -> HashMap<String, HashMap<usize, BTreeSet<String>>> {
    let mut flow: HashMap<String, HashMap<usize, BTreeSet<String>>> = HashMap::new();

    // Ensure entries exist.
    for (f, hos) in ho_params {
        let e = flow.entry(f.clone()).or_default();
        for (i, _) in hos {
            e.entry(*i).or_default();
        }
    }

    // Collect param-name → index for each function.
    let param_index = param_indices(program);

    let mut changed = true;
    while changed {
        changed = false;
        for item in &program.items {
            let (fname, body) = match item {
                Item::Func(f) => (f.name.as_str(), &f.body),
                Item::Actor(a) => (a.name.as_str(), &a.body),
                _ => continue,
            };
            let mut sites = Vec::new();
            find_applies(body, false, &mut sites);
            for (fun_expr, args, _) in sites {
                let Some(g) = resolve_callee_name(fun_expr, tops) else {
                    // Higher-order head — not a flow *into* a named function's params via this head.
                    // Still, args may flow if we knew the callees; handled via edges, not flow seeds.
                    continue;
                };
                // Only track flow into HO params of g.
                let Some(hos) = ho_params.get(g) else {
                    continue;
                };
                for (pi, _) in hos {
                    if *pi >= args.len() {
                        continue;
                    }
                    let arg = &args[*pi];
                    // Direct top-level function value (not a parameter of the current function).
                    if let Some(n) = atom_top_name(arg, tops) {
                        // Shadowing guard: if `n` is a parameter of the current function,
                        // it refers to the parameter, not the top-level binding.
                        let is_param_of_current = param_index
                            .get(fname)
                            .and_then(|m| m.get(&n))
                            .is_some();
                        if !is_param_of_current
                            && flow
                                .entry(g.to_string())
                                .or_default()
                                .entry(*pi)
                                .or_default()
                                .insert(n)
                        {
                            changed = true;
                        }
                    }
                    // Parameter of current function that is HO — union its flow.
                    if let Expr::Var { name, .. } = arg {
                        if let Some(&from_i) = param_index.get(fname).and_then(|m| m.get(name)) {
                            if ho_params
                                .get(fname)
                                .map(|h| h.iter().any(|(i, _)| *i == from_i))
                                .unwrap_or(false)
                            {
                                let src = flow
                                    .get(fname)
                                    .and_then(|m| m.get(&from_i))
                                    .cloned()
                                    .unwrap_or_default();
                                let dst = flow
                                    .entry(g.to_string())
                                    .or_default()
                                    .entry(*pi)
                                    .or_default();
                                for n in src {
                                    if dst.insert(n) {
                                        changed = true;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    flow
}

fn param_indices(program: &Program) -> HashMap<String, HashMap<String, usize>> {
    let mut m = HashMap::new();
    for item in &program.items {
        let (name, params) = match item {
            Item::Func(f) => (&f.name, &f.params),
            Item::Actor(a) => (&a.name, &a.params),
            _ => continue,
        };
        let mut inner = HashMap::new();
        for (i, p) in params.iter().enumerate() {
            inner.insert(p.clone(), i);
        }
        m.insert(name.clone(), inner);
    }
    m
}

fn collect_edges(
    program: &Program,
    tops: &HashSet<String>,
    ho_params: &HashMap<String, Vec<(usize, String)>>,
    flow: &HashMap<String, HashMap<usize, BTreeSet<String>>>,
) -> Vec<CallEdge> {
    let param_index = param_indices(program);
    let mut edges = Vec::new();
    for item in &program.items {
        let (fname, body) = match item {
            Item::Func(f) => (f.name.as_str(), &f.body),
            Item::Actor(a) => (a.name.as_str(), &a.body),
            _ => continue,
        };
        let mut sites = Vec::new();
        find_applies(body, true, &mut sites);
        for (fun_expr, _args, is_tail) in &sites {
            let span = fun_expr.span();
            // Higher-order: head is a HO parameter of current function.
            // Must be checked BEFORE top-level resolution to handle shadowing
            // (a parameter name that coincides with a top-level binding).
            let ho_pi = if let Expr::Var { name, .. } = fun_expr {
                param_index
                    .get(fname)
                    .and_then(|m| m.get(name))
                    .copied()
                    .filter(|&pi| {
                        ho_params
                            .get(fname)
                            .map(|h| h.iter().any(|(i, _)| *i == pi))
                            .unwrap_or(false)
                    })
            } else {
                None
            };
            if let Some(pi) = ho_pi {
                let callees = flow
                    .get(fname)
                    .and_then(|m| m.get(&pi))
                    .cloned()
                    .unwrap_or_default();
                for to in callees {
                    edges.push(CallEdge {
                        from: fname.to_string(),
                        to,
                        is_tail: *is_tail,
                        span,
                        is_ho: true,
                    });
                }
                continue;
            }
            // Named top-level callee.
            if let Some(to) = resolve_callee_name(fun_expr, tops) {
                edges.push(CallEdge {
                    from: fname.to_string(),
                    to: to.to_string(),
                    is_tail: *is_tail,
                    span,
                    is_ho: false,
                });
                continue;
            }
        }
    }
    edges
}

/// Collect `(fun_expr, args, is_tail_call_site)` for every `Apply`.
fn find_applies<'a>(expr: &'a Expr, in_tail: bool, out: &mut Vec<(&'a Expr, &'a [Expr], bool)>) {
    match expr {
        Expr::Apply { fun, args, .. } => {
            out.push((fun.as_ref(), args.as_slice(), in_tail));
            find_applies(fun, false, out);
            for a in args {
                find_applies(a, false, out);
            }
        }
        Expr::BinOp { lhs, rhs, .. } => {
            find_applies(lhs, false, out);
            find_applies(rhs, false, out);
        }
        Expr::Send { target, msg, .. } => {
            find_applies(target, false, out);
            find_applies(msg, false, out);
        }
        Expr::Field { base, .. } | Expr::Borrow { inner: base, .. } => {
            find_applies(base, false, out);
        }
        Expr::RecordUpdate {
            receiver, clauses, ..
        } => {
            find_applies(receiver, false, out);
            for c in clauses {
                walk_clause_applies(c, out);
            }
        }
        Expr::SetField { receiver, value, .. } => {
            find_applies(receiver, false, out);
            find_applies(value, false, out);
        }
        Expr::RegionWrite { receiver, value, .. } => {
            find_applies(receiver, false, out);
            find_applies(value, false, out);
        }
        Expr::Bind { value, body, .. } => {
            find_applies(value, false, out);
            find_applies(body, in_tail, out);
        }
        Expr::Seq { first, second, .. } => {
            find_applies(first, false, out);
            find_applies(second, in_tail, out);
        }
        Expr::If { arms, .. } => {
            for arm in arms {
                if let Some(c) = &arm.condition {
                    find_applies(c, false, out);
                }
                find_applies(&arm.body, in_tail, out);
            }
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            find_applies(scrutinee, false, out);
            for arm in arms {
                find_applies(&arm.body, in_tail, out);
            }
        }
        Expr::ListLit { elems, .. } => {
            for e in elems {
                find_applies(e, false, out);
            }
        }
        Expr::Lambda { body, .. } => {
            find_applies(body, false, out);
        }
        Expr::Comprehension { generators, accs, guards, result, .. } => {
            for g in generators { for s in &g.sources { find_applies(s, false, out); } }
            for a in accs { find_applies(&a.init, false, out); }
            for g in guards { find_applies(g, false, out); }
            find_applies(result, false, out);
        }
        Expr::Product { elems, .. } => {
            for e in elems { find_applies(e, false, out); }
        }
        Expr::Int { .. }
        | Expr::Str { .. }
        | Expr::Var { .. }
        | Expr::Upper { .. }
        | Expr::AtName { .. }
        | Expr::Self_ { .. }
        | Expr::Unit { .. }
        | Expr::Do { .. } => {}
    }
}

fn walk_clause_applies<'a>(c: &'a wag_syntax::UpdateClause, out: &mut Vec<(&'a Expr, &'a [Expr], bool)>) {
    match c {
        wag_syntax::UpdateClause::Leaf { value, .. } => find_applies(value, false, out),
        wag_syntax::UpdateClause::Group { clauses, .. } => {
            for c in clauses {
                walk_clause_applies(c, out);
            }
        }
    }
}

fn resolve_callee_name<'a>(fun: &'a Expr, tops: &HashSet<String>) -> Option<&'a str> {
    match fun {
        Expr::Var { name, .. } | Expr::Upper { name, .. } if tops.contains(name) => {
            Some(name.as_str())
        }
        Expr::AtName { name, .. } if tops.contains(name) => Some(name.as_str()),
        _ => None,
    }
}

fn atom_top_name(expr: &Expr, tops: &HashSet<String>) -> Option<String> {
    match expr {
        Expr::Var { name, .. } | Expr::Upper { name, .. } if tops.contains(name) => {
            Some(name.clone())
        }
        _ => None,
    }
}

fn scc_cyclic_nodes(edges: &[CallEdge]) -> BTreeSet<String> {
    let mut nodes = BTreeSet::new();
    for e in edges {
        nodes.insert(e.from.clone());
        nodes.insert(e.to.clone());
    }
    let mut adj: HashMap<String, Vec<String>> = HashMap::new();
    let mut radj: HashMap<String, Vec<String>> = HashMap::new();
    for n in &nodes {
        adj.entry(n.clone()).or_default();
        radj.entry(n.clone()).or_default();
    }
    for e in edges {
        adj.get_mut(&e.from).unwrap().push(e.to.clone());
        radj.get_mut(&e.to).unwrap().push(e.from.clone());
    }

    // Kosaraju
    let mut seen = HashSet::new();
    let mut order = Vec::new();
    for n in &nodes {
        if !seen.contains(n) {
            dfs1(n, &adj, &mut seen, &mut order);
        }
    }
    let mut seen2 = HashSet::new();
    let mut cyclic = BTreeSet::new();
    for n in order.iter().rev() {
        if seen2.contains(n) {
            continue;
        }
        let mut comp = Vec::new();
        dfs2(n, &radj, &mut seen2, &mut comp);
        let multi = comp.len() > 1;
        let self_loop = comp.len() == 1
            && edges
                .iter()
                .any(|e| e.from == comp[0] && e.to == comp[0]);
        if multi || self_loop {
            for c in comp {
                cyclic.insert(c);
            }
        }
    }
    cyclic
}

fn dfs1(n: &str, adj: &HashMap<String, Vec<String>>, seen: &mut HashSet<String>, order: &mut Vec<String>) {
    seen.insert(n.to_string());
    if let Some(ns) = adj.get(n) {
        for m in ns {
            if !seen.contains(m) {
                dfs1(m, adj, seen, order);
            }
        }
    }
    order.push(n.to_string());
}

fn dfs2(n: &str, radj: &HashMap<String, Vec<String>>, seen: &mut HashSet<String>, comp: &mut Vec<String>) {
    seen.insert(n.to_string());
    comp.push(n.to_string());
    if let Some(ns) = radj.get(n) {
        for m in ns {
            if !seen.contains(m) {
                dfs2(m, radj, seen, comp);
            }
        }
    }
}

/// True if `a` can reach `b` and `b` can reach `a` via the edge set.
fn same_scc(a: &str, b: &str, edges: &[CallEdge]) -> bool {
    if a == b {
        return edges.iter().any(|e| e.from == a && e.to == a) || a == b;
        // For a != b we still need mutual reachability; for a == b, same node in cyclic set already.
    }
    reaches(a, b, edges) && reaches(b, a, edges)
}

fn reaches(from: &str, to: &str, edges: &[CallEdge]) -> bool {
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for e in edges {
        adj.entry(e.from.as_str()).or_default().push(e.to.as_str());
    }
    let mut q = VecDeque::new();
    let mut seen = HashSet::new();
    q.push_back(from);
    seen.insert(from);
    while let Some(n) = q.pop_front() {
        if n == to {
            return true;
        }
        if let Some(ns) = adj.get(n) {
            for m in ns {
                if seen.insert(*m) {
                    q.push_back(*m);
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(src: &str) -> (Program, TypedProgram) {
        let p = wag_syntax::parse(src).unwrap_or_else(|e| panic!("parse: {e}\n{src}"));
        let t = wag_types::check(&p).unwrap_or_else(|e| panic!("type: {e}\n{src}"));
        (p, t)
    }

    /// Self-recursion in tail position is rule-1 legal (fixture 8_1 shape).
    #[test]
    fn self_tail_recursion_is_ok() {
        let (p, t) = typed(
            r#"
f = Int -> Int: n -> if n == 0 -> 0
  _ -> f (n - 1)
"#,
        );
        let info = check_tail_cycles_with(&p, &t, &HashMap::new()).unwrap();
        assert!(info.cyclic_fns.contains("f"));
    }

    /// Mutual recursion where every edge is tail-position (fixture 8_19 shape).
    #[test]
    fn mutual_tail_recursion_is_ok() {
        let (p, t) = typed(
            r#"
even = Int -> Bool: n -> if n == 0 -> True
  _ -> odd n

odd = Int -> Bool: n -> if n == 0 -> False
  _ -> even n
"#,
        );
        let info = check_tail_cycles_with(&p, &t, &HashMap::new()).unwrap();
        assert!(info.cyclic_fns.contains("even"));
        assert!(info.cyclic_fns.contains("odd"));
    }

    /// Non-tail call lying on a cycle must be rejected (§7 rule 1).
    #[test]
    fn non_tail_edge_on_cycle_is_rejected() {
        let (p, t) = typed(
            r#"
g = Int -> Int: n -> if n == 0 -> 0
  _ -> id (h n)

h = Int -> Int: n -> g n

id = Int -> Int: x -> x
"#,
        );
        let e = check_tail_cycles_with(&p, &t, &HashMap::new()).unwrap_err();
        assert_eq!(e.rule, Rule::TailCycle);
        assert!(e.message.contains("non-tail"), "got: {}", e.message);
    }

    /// Non-tail self-call (bind from own result) is a rule-1 violation.
    #[test]
    fn non_tail_self_call_is_rejected() {
        let (p, t) = typed(
            r#"
bad = Int -> Int: n -> id (bad n)

id = Int -> Int: x -> x
"#,
        );
        let e = check_tail_cycles_with(&p, &t, &HashMap::new()).unwrap_err();
        assert_eq!(e.rule, Rule::TailCycle);
    }

    /// Acyclic non-tail calls are fine — rule 1 only constrains cycles.
    #[test]
    fn acyclic_non_tail_calls_are_ok() {
        let (p, t) = typed(
            r#"
inc = Int -> Int: n -> n + 1

useInc = Int -> Int: n -> id (inc n)

id = Int -> Int: x -> x
"#,
        );
        let info = check_tail_cycles_with(&p, &t, &HashMap::new()).unwrap();
        assert!(info.cyclic_fns.is_empty());
        assert!(info.edges.iter().any(|e| {
            e.from == "useInc" && e.to == "inc" && !e.is_tail
        }));
    }

    /// No calls at all — no cycles, no edges.
    #[test]
    fn no_calls_no_cycles() {
        let (p, t) = typed(
            r#"
identity = Int -> Int: n -> n
"#,
        );
        let info = check_tail_cycles_with(&p, &t, &HashMap::new()).unwrap();
        assert!(info.cyclic_fns.is_empty());
        assert!(info.edges.is_empty());
    }

    /// HO parameter shadowing a top-level binding must not create a false cycle.
    ///
    /// `wrap` has HO param `f`; `f` is also a top-level binding that calls `wrap`.
    /// The call `f x` inside `wrap` refers to the *parameter*, not the binding.
    #[test]
    fn ho_param_shadowing_does_not_create_false_cycle() {
        let (p, t) = typed(
            r#"
wrap = (Int -> Int), Int -> Int: f, x -> f x

f = Int -> Int: x -> wrap id x

id = Int -> Int: x -> x
"#,
        );
        let info = check_tail_cycles_with(&p, &t, &HashMap::new()).unwrap();
        // `wrap` is not cyclic (it calls its HO param, which may be `id` or `f`,
        // but the edge wrap->f should NOT exist because `f` is a param of `wrap`).
        assert!(
            !info.edges.iter().any(|e| e.from == "wrap" && e.to == "f"),
            "wrap->f edge should not exist (param shadowing): {:?}",
            info.edges
        );
    }
}
