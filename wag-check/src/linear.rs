//! Linearity (rule 10), borrow well-formedness (rule 11), and copy/& PF checks.

use std::collections::HashMap;

use wag_syntax::{Expr, Item, Pat, Span, UpdateClause};
use wag_types::{Ty, TypedProgram};

use crate::error::{CheckError, Rule};
use crate::fields::FieldTable;

#[derive(Clone, Debug, PartialEq)]
enum Value {
    /// Untracked (copyable) — PF or Fun. No obligation.
    Untracked(Ty),
    /// Tracked with obligation state.
    Tracked(Tracked),
}

/// Where and why a tracked binding was last consumed (rule 10 diagnostics).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MoveInfo {
    /// Span of the consuming use.
    pub at: Span,
    /// Human-readable reason (e.g. "passed to owned parameter of `g`").
    pub why: String,
}

#[derive(Clone, Debug, PartialEq)]
enum Tracked {
    /// Owned — contains heap refs, must be moved exactly once (rule 10).
    Owned,
    /// Moved — already consumed; carries the consuming site for diagnostics.
    Moved(MoveInfo),
    /// Borrowed — `&T` view, never consumable/returnable/sendable (rule 11).
    Borrowed,
}

/// Rule 10 use-after-move diagnostic with a linked origin label pointing at
/// the consuming use.
fn moved_err(name: &str, span: Span, info: &MoveInfo) -> CheckError {
    CheckError {
        rule: Rule::Linearity,
        span,
        message: format!("use after move of `{name}`"),
        origin: Some((info.at, info.why.clone())),
        module: None,
        source: None,
    }
}

/// Classify a type into its linearity `Value` (§4.3.2). Under full linearity every
/// owned *data* value is linear — consumed by its next owned use — regardless of
/// pointer-freedom, which affects only representation (inline vs. heap-cell-backed,
/// §3.8), never linearity. The only non-linear values are **reference** values:
/// function values and actor handles (`Actor T`), copyable/sharable
/// (§5.5); links (`Ref`) are non-values (D054/D056); a borrowed view is a read-only,
/// non-owning reference (§4.3.3).
fn value_for(ty: &Ty, _typed: &TypedProgram) -> Result<Value, CheckError> {
    match ty {
        // A borrowed view is a read-only, non-owning reference (§4.3.3).
        Ty::Borrow(_) => Ok(Value::Tracked(Tracked::Borrowed)),
        // Links are non-values — never linearly tracked (D054/D056).
        Ty::App(n, _) if n == "Ref" => Ok(Value::Untracked(ty.clone())),
        // Reference values: function values and actor handles are copyable/sharable (§5.5),
        // not linear data — exempt from linearity regardless of their (abstract) argument types.
        Ty::Fun(_, _) => Ok(Value::Untracked(ty.clone())),
        Ty::App(n, _) if n == "Actor" => Ok(Value::Untracked(ty.clone())),
        // Every other owned value is linear data (§4.3.2) — including abstract/undecidable
        // types: a bare type var may instantiate to a heap-backed type such as `List Int`,
        // so it must be tracked, not assumed copyable.
        _ => Ok(Value::Tracked(Tracked::Owned)),
    }
}

/// True when `ty` mentions type constructor `name` anywhere.
fn ty_mentions(ty: &Ty, name: &str) -> bool {
    match ty {
        Ty::Con(n) => n == name,
        Ty::App(n, args) => n == name || args.iter().any(|a| ty_mentions(a, name)),
        Ty::Borrow(t) => ty_mentions(t, name),
        Ty::Fun(ps, r) => ps.iter().any(|p| ty_mentions(p, name)) || ty_mentions(r, name),
        // A bound param or unresolved ctor var is opaque: cannot mention a specific constructor.
        Ty::CtorApp(..) | Ty::Var(_) | Ty::Bound(_) => false,
    }
}

fn count_clause(c: &UpdateClause) -> usize {
    match c {
        UpdateClause::Leaf { value, .. } => count_self_uses(value),
        UpdateClause::Group { clauses, .. } => clauses.iter().map(count_clause).sum(),
    }
}

/// A self-peek (D060): `Self_.field` — a non-consuming read of a receiver
/// field. Bare atoms desugar to this form; it is never a handle re-emission.
fn is_self_peek(e: &Expr) -> bool {
    matches!(e, Expr::Field { base, .. } if matches!(base.as_ref(), Expr::Self_ { .. }))
}

/// Count `self` occurrences in an expression (D057 method linearity).
/// Self-peeks (`Self_.field`) do not count — only standalone handle uses.
fn count_self_uses(e: &Expr) -> usize {
    match e {
        Expr::Self_ { .. } => 1,
        Expr::Int { .. }
        | Expr::Str { .. }
        | Expr::Upper { .. }
        | Expr::AtName { .. }
        | Expr::Var { .. }
        | Expr::Unit { .. }
        | Expr::Do { .. } => 0,
        Expr::Apply { fun, args, .. } => {
            count_self_uses(fun) + args.iter().map(count_self_uses).sum::<usize>()
        }
        Expr::BinOp { lhs, rhs, .. } => count_self_uses(lhs) + count_self_uses(rhs),
        // Self-peek: a field read is not a handle use (D060).
        Expr::Field { base, .. } if matches!(base.as_ref(), Expr::Self_ { .. }) => 0,
        Expr::Field { base, .. } => count_self_uses(base),
        Expr::RecordUpdate { receiver, clauses, .. } => {
            count_self_uses(receiver) + clauses.iter().map(count_clause).sum::<usize>()
        }
        Expr::SetField { receiver, value, .. }
    | Expr::RegionWrite { receiver, value, .. } => {
            count_self_uses(receiver) + count_self_uses(value)
        }
        Expr::Send { target, msg, .. } => count_self_uses(target) + count_self_uses(msg),
        Expr::Borrow { inner, .. } => count_self_uses(inner),
        Expr::Bind { value, body, .. } => count_self_uses(value) + count_self_uses(body),
        Expr::Seq { first, second, .. } => count_self_uses(first) + count_self_uses(second),
        Expr::If { arms, .. } => arms
            .iter()
            .map(|a| {
                a.condition.as_ref().map(count_self_uses).unwrap_or(0)
                    + count_self_uses(&a.body)
            })
            .sum::<usize>(),
        Expr::Match { scrutinee, arms, .. } => {
            count_self_uses(scrutinee)
                + arms
                    .iter()
                    .map(|a| count_self_uses(&a.body))
                    .sum::<usize>()
        }
        Expr::ListLit { elems, .. } | Expr::Product { elems, .. } => {
            elems.iter().map(count_self_uses).sum::<usize>()
        }
        Expr::Lambda { body, .. } => count_self_uses(body),
        Expr::Comprehension { generators, accs, guards, result, .. } => generators
            .iter()
            .flat_map(|g| g.sources.iter())
            .map(count_self_uses)
            .sum::<usize>()
                + accs.iter().map(|a| count_self_uses(&a.init)).sum::<usize>()
                + guards.iter().map(count_self_uses).sum::<usize>()
                + count_self_uses(result),
    }
}

/// Per-item linearity + borrow check (D067). `check_modules` iterates
/// module-tagged items and stamps errors with the originating module.
pub(crate) fn check_item(item: &Item, typed: &TypedProgram) -> Result<(), CheckError> {
    match item {
            Item::Func(f) => {
                let mut state = base_state(typed);
                if let Some(Ty::Fun(ps, ret)) = typed.bindings.get(&(f.name.clone(), f.params.len())) {
                    for (i, p) in f.params.iter().enumerate() {
                        if i < ps.len() {
                            bind_name(&mut state, p, &ps[i], typed)?;
                        }
                    }
                    check_expr(&f.body, &mut state, &f.name, typed, Some(ret), true)?;
                } else {
                    check_expr(
                        &f.body,
                        &mut state,
                        &f.name,
                        typed,
                        None,
                        true,
                    )?;
                }
            }
            Item::Actor(a) => {
                let mut state = base_state(typed);
                // Actor behavior is keyed `("@name", arity)` (separate namespace, §2).
                if let Some(Ty::Fun(ps, ret)) =
                    typed.bindings.get(&(format!("@{}", a.name), a.params.len()))
                {
                    for (i, p) in a.params.iter().enumerate() {
                        if i < ps.len() {
                            bind_name(&mut state, p, &ps[i], typed)?;
                        }
                    }
                    let actor_key = format!("@{}", a.name);
                    check_expr(&a.body, &mut state, &actor_key, typed, Some(ret), true)?;
                }
            }
            Item::Method(m) => {
                let mut state = base_state(typed);
                if let Some(Ty::Fun(ps, ret)) =
                    typed.method_tys.get(&(m.type_name.clone(), m.name.clone()))
                {
                    for (i, p) in m.params.iter().enumerate() {
                        if i < ps.len() {
                            bind_name(&mut state, p, &ps[i], typed)?;
                        }
                    }
                    // `self` linearity (D057): a move-and-return method re-emits
                    // the handle exactly once per execution path — tracked in
                    // state as `__self__` so match arms clone it correctly and
                    // per-arm uses are independent. A read-only method never
                    // consumes it.
                    let consuming = ty_mentions(ret, &m.type_name);
                    let uses = count_self_uses(&m.body);
                    if consuming {
                        state.insert("__self__".to_string(), Value::Tracked(Tracked::Owned));
                        if uses == 0 {
                            return Err(CheckError::new(
                                Rule::Linearity,
                                m.span,
                                format!(
                                    "method `{}.{}' is move-and-return (result includes `{}`) and must re-emit `self` in its result",
                                    m.type_name, m.name, m.type_name
                                ),
                            ));
                        }
                    } else if uses > 0 {
                        return Err(CheckError::new(
                            Rule::Linearity,
                            m.span,
                            format!(
                                "method `{}.{}' does not return the receiver handle; `self` cannot be used (found {} use(s))",
                                m.type_name, m.name, uses
                            ),
                        ));
                    }
                    check_expr(
                        &m.body,
                        &mut state,
                        &format!("{}.{}", m.type_name, m.name),
                        typed,
                        Some(ret),
                        true,
                    )?;
                }
            }
            _ => {}
        }
    Ok(())
}

/// Look up an expression's inferred type from the side table (`expr_types`,
/// keyed by `(item, node span)`) — computed once during inference (§3.2).
/// Every real source node is recorded by `wag-types`; checker-desugared
/// synthetic nodes reuse their source root's span and hit the same entry.
fn expr_ty_typed(expr: &Expr, item: &str, typed: &TypedProgram) -> Option<Ty> {
    typed.expr_types.get(&(item.to_string(), expr.span())).cloned()
}

/// `expr_ty_typed` as a hard requirement. Inference guarantees an entry for
/// every real node (and desugared subtrees share the source root's span), so
/// a miss is an internal invariant violation, not a recoverable condition.
fn require_ty(expr: &Expr, item: &str, typed: &TypedProgram) -> Result<Ty, CheckError> {
    expr_ty_typed(expr, item, typed).ok_or_else(|| {
        CheckError::new(
            Rule::Internal,
            expr.span(),
            format!(
                "internal error: no inferred type recorded for span {:?} in `{item}`",
                expr.span()
            ),
        )
    })
}

/// True when the expression is a borrow (`&T`) — either syntactically (Borrow
/// node, Var bound to Borrowed in state) or from the side table. Field reads
/// are borrows per the side table (§3.3/§4.3.3); method dispatch on a field
/// chain is an owned call result (D056), which inference records as such.
fn is_borrow_typed(
    expr: &Expr,
    state: &HashMap<String, Value>,
    item: &str,
    typed: &TypedProgram,
) -> bool {
    match expr {
        Expr::Borrow { .. } => return true,
        Expr::Var { name, .. } if is_borrowed_in_state(state, name) => return true,
        _ => {}
    }
    matches!(expr_ty_typed(expr, item, typed), Some(Ty::Borrow(_)))
}

/// Build the initial state map (work item B): top-level bindings are function/
/// actor handles — untracked (copyable). Local params are added by the caller.
fn base_state(typed: &TypedProgram) -> HashMap<String, Value> {
    let mut m = HashMap::new();
    for ((name, _arity), ty) in &typed.bindings {
        if !m.contains_key(name) {
            // Top-level items are always untracked (Fun or Actor handle).
            m.insert(name.clone(), Value::Untracked(ty.clone()));
        }
    }
    m
}

/// Consume a tracked binding: Owned → Moved(why at span); Borrowed → err 11.
/// A second consume of an already-Moved binding is a rule 10 error (the
/// consuming use itself is the illegal reuse). Untracked (PF/Fun) and absence
/// are no-ops.
fn consume_state(
    state: &mut HashMap<String, Value>,
    name: &str,
    span: Span,
    why: &str,
) -> Result<(), CheckError> {
    match state.get(name) {
        Some(Value::Tracked(Tracked::Owned)) => {
            state.insert(
                name.to_string(),
                Value::Tracked(Tracked::Moved(MoveInfo { at: span, why: why.to_string() })),
            );
            Ok(())
        }
        Some(Value::Tracked(Tracked::Borrowed)) => Err(CheckError::new(
            Rule::Borrow,
            span,
            format!("rule 11: consuming use of borrowed `{name}`"),
        )),
        // Redundant consume — the reuse itself is illegal (rule 10).
        Some(Value::Tracked(Tracked::Moved(info))) => Err(moved_err(name, span, info)),
        _ => Ok(()),
    }
}

/// True when the name is bound to a Borrowed value in the state map.
fn is_borrowed_in_state(state: &HashMap<String, Value>, name: &str) -> bool {
    matches!(state.get(name), Some(Value::Tracked(Tracked::Borrowed)))
}

/// Bind a name with its classified value. Returns the previous binding if any.
fn bind_name(
    state: &mut HashMap<String, Value>,
    name: &str,
    ty: &Ty,
    typed: &TypedProgram,
) -> Result<Option<Value>, CheckError> {
    let val = value_for(ty, typed)?;
    Ok(state.insert(name.to_string(), val))
}

// Pointer-free classification lives in `wag_types::pf` (A1b invariant) and is
// consulted only for *representation* decisions — e.g. the per-field borrow/copy
// split on a borrowed match (§4.3.3). It no longer gates linearity: under full
// linearity every owned data value is tracked regardless of pointer-freedom.

/// `in_tail` — expression is in return/tail position of the enclosing function.
fn check_expr(
    expr: &Expr,
    state: &mut HashMap<String, Value>,
    item: &str,
    typed: &TypedProgram,
    ret_ty: Option<&Ty>,
    in_tail: bool,
) -> Result<(), CheckError> {
    match expr {
        Expr::Var { name, span } => {
            // Rule 10: ANY use (consuming or not) of an already-moved binding
            // is illegal — the value was consumed by its previous owning use.
            if let Some(Value::Tracked(Tracked::Moved(info))) = state.get(name) {
                return Err(moved_err(name, *span, info));
            }
            // Bare var in tail/return: a borrow cannot escape (rule 11c).
            if in_tail && is_borrowed_in_state(state, name) {
                return Err(CheckError::new(
                    Rule::Borrow,
                    *span,
                    format!("rule 11: cannot return borrowed binding `{name}`"),
                ));
            }
            Ok(())
        }
        Expr::Self_ { span } => {
            // Receiver handle (D057): linear in move-and-return methods
            // (tracked as `__self__`); untracked elsewhere (actor `self`,
            // read-only methods).
            if let Some(Value::Tracked(Tracked::Moved(info))) = state.get("__self__") {
                return Err(moved_err("self", *span, info));
            }
            consume_state(state, "__self__", *span, "re-emitted `self`")
        }
        Expr::Int { .. }
        | Expr::Str { .. }
        | Expr::Upper { .. }
        | Expr::AtName { .. } => Ok(()),
        Expr::Send { target, msg, .. } => {
            check_expr(target, state, item, typed, ret_ty, false)?;
            check_expr(msg, state, item, typed, ret_ty, false)?;
            Ok(())
        }
        Expr::Borrow { inner, span } => {
            // Returning / producing a borrow in tail position escapes (rule 11c).
            if in_tail {
                return Err(CheckError::new(
                    Rule::Borrow,
                    *span,
                    "rule 11: cannot return a borrow",
                ));
            }
            // Borrowing a var is non-consuming.
            check_expr(inner, state, item, typed, ret_ty, false)
        }
        Expr::Apply { fun, args, span } => {
            // Special forms: copy, send, spawn, ...
            if let Expr::Var { name, .. } = fun.as_ref() {
                if name == "copy" {
                    if args.len() != 1 {
                        return Err(CheckError::new(
                            Rule::CopyBorrowPf,
                            *span,
                            "copy expects one argument",
                        ));
                    }
                    // copy: &T -> T — non-consuming read.
                    check_expr(&args[0], state, item, typed, ret_ty, false)?;
                    return Ok(());
                }
                if name == "send" {
                    // send target payload — consuming owned use; must not be borrow.
                    if args.len() >= 2 {
                        check_expr(&args[0], state, item, typed, ret_ty, false)?;
                        if is_borrow_typed(&args[1], state, item, typed) {
                            return Err(CheckError::new(
                                Rule::Borrow,
                                args[1].span(),
                                "rule 11: cannot send a borrowed value",
                            ));
                        }
                        check_expr(&args[1], state, item, typed, ret_ty, false)?;
                        consume_root_var(
                            &args[1],
                            state,
                            args[1].span(),
                            "sent as message payload",
                        )?;
                    }
                    return Ok(());
                }
            }

            // Determine parameter ownership from callee type (side table).
            let fun_ty = expr_ty_typed(fun, item, typed);
            check_expr(fun, state, item, typed, ret_ty, false)?;
            let mut param_types: Vec<&Ty> = match &fun_ty {
                Some(Ty::Fun(ps, _)) => ps.iter().collect(),
                _ => vec![],
            };
            // Method call (D056/D069): `h.m args` — the receiver is fun's base
            // (not in `args`); declared params follow it. Move-and-return
            // methods consume the receiver handle; read-only methods do not.
            let mut arg_why = match fun.as_ref() {
                Expr::Var { name, .. } => format!("passed to `{name}`"),
                Expr::Upper { name, .. } => format!("embedded in `{name} ...` value"),
                _ => "consumed by call".to_string(),
            };
            // Peel one Field level — the called method is the outermost field,
            // resolved against its base's head type (mirrors `infer_local`).
            if let Expr::Field { base, field, .. } = fun.as_ref() {
                if let Some(base_ty) = expr_ty_typed(base, item, typed) {
                    if let Some(head) = FieldTable::type_head(base_ty.strip_borrow()) {
                        if let Some(Ty::Fun(ps, ret)) =
                            typed.method_tys.get(&(head.clone(), field.clone()))
                        {
                            // `method_tys` holds the declared params only (no
                            // receiver), so use them as-is. A plain Var base is
                            // consumed for move-and-return methods; chained bases
                            // (`(h.bump).append`) are no-ops here — their inner
                            // link consumes via the Field arm below.
                            param_types = ps.iter().collect();
                            arg_why = format!("passed to method `{head}.{field}`");
                            if ty_mentions(ret, &head) {
                                consume_root_var(
                                    base,
                                    state,
                                    base.span(),
                                    &format!("consumed by move-and-return method `{head}.{field}`"),
                                )?;
                            }
                        }
                    }
                }
            }
            for (i, arg) in args.iter().enumerate() {
                let param_is_borrow = param_types
                    .get(i)
                    .map(|p| matches!(p, Ty::Borrow(_)))
                    .unwrap_or(false);
                if param_is_borrow {
                    // May pass owned (auto-borrow) or borrow; non-consuming.
                    check_expr(arg, state, item, typed, ret_ty, false)?;
                } else {
                    if is_borrow_typed(arg, state, item, typed) {
                        return Err(CheckError::new(
                            Rule::Borrow,
                            arg.span(),
                            "rule 11: cannot pass borrow to owned parameter",
                        ));
                    }
                    check_expr(arg, state, item, typed, ret_ty, false)?;
                    // Consume root-Var args that are tracked owned (rule 10).
                    consume_root_var(arg, state, arg.span(), &arg_why)?;
                }
            }
            let _ = span;
            Ok(())
        }
        Expr::BinOp { lhs, rhs, .. } => {
            check_expr(lhs, state, item, typed, ret_ty, false)?;
            check_expr(rhs, state, item, typed, ret_ty, false)
        }
        Expr::Field { base, field, .. } => {
            if is_self_peek(expr) {
                // Self-peek (D060): a non-consuming read of a receiver field —
                // the region outlives the handle re-emission, so peeks never
                // consume `__self__` and are legal even after it.
                return Ok(());
            }
            // Field access borrows; non-consuming on base.
            check_expr(base, state, item, typed, ret_ty, false)?;
            // Nullary move-and-return method call (D069): `h.bump` stays a bare
            // Field in the checked AST (`desugar_field_chain` leaves methods
            // intact for the VM), so the Apply arm never sees it — consume the
            // receiver here. Check-then-consume: an already-moved base errors
            // above first.
            if let Some(base_ty) = expr_ty_typed(base, item, typed) {
                if let Some(head) = FieldTable::type_head(base_ty.strip_borrow()) {
                    if let Some(Ty::Fun(ps, ret)) =
                        typed.method_tys.get(&(head.clone(), field.clone()))
                    {
                        if ps.is_empty() && ty_mentions(ret, &head) {
                            consume_root_var(
                                base,
                                state,
                                expr.span(),
                                &format!("consumed by move-and-return method `{head}.{field}`"),
                            )?;
                        }
                    }
                }
            }
            Ok(())
        }
        Expr::SetField { receiver, value, span, .. } => {
            if is_borrow_typed(receiver, state, item, typed) {
                return Err(CheckError::new(
                    Rule::Borrow,
                    *span,
                    "set-field requires owned receiver",
                ));
            }
            // Owned-model setter (§3.3.1 / D081): the receiver is consumed and
            // rebuilt — a non-self receiver does not survive the write. A
            // method-body write statement desugars here with `self` as
            // receiver — skipping keeps `__self__` available for the handle
            // re-emission (D057).
            // Value is checked first: its reads may borrow from the receiver
            // (RHS borrows, §3.3.1), matching VM eval order (value, then consume).
            check_expr(value, state, item, typed, ret_ty, false)?;
            if !matches!(receiver.as_ref(), Expr::Self_ { .. }) {
                check_expr(receiver, state, item, typed, ret_ty, false)?;
                consume_root_var(receiver, state, receiver.span(), "consumed by field setter")?;
            }
            Ok(())
        }
        Expr::RegionWrite { receiver, value, span, .. } => {
            // In-place write through the receiver's region (D058): no allocation,
            // non-consuming over `self` (the handle stays available for re-emission,
            // D057); a non-self receiver handle is consumed. The value is consumed.
            if is_borrow_typed(receiver, state, item, typed) {
                return Err(CheckError::new(
                    Rule::Borrow,
                    *span,
                    "region-write requires owned receiver",
                ));
            }
            if !matches!(receiver.as_ref(), Expr::Self_ { .. }) {
                check_expr(receiver, state, item, typed, ret_ty, false)?;
            }
            check_expr(value, state, item, typed, ret_ty, false)?;
            Ok(())
        }
        Expr::RecordUpdate {
            receiver,
            clauses,
            span,
        } => {
            if is_borrow_typed(receiver, state, item, typed) {
                return Err(CheckError::new(
                    Rule::Borrow,
                    *span,
                    "record update requires owned receiver",
                ));
            }
            check_clauses_linear(clauses, state, item, typed, ret_ty)?;
            consume_root_var(receiver, state, receiver.span(), "record update")?;
            check_expr(receiver, state, item, typed, ret_ty, false)
        }
        Expr::Bind {
            name,
            value,
            body,
            span,
        } => {
            check_expr(value, state, item, typed, ret_ty, false)?;
            // D4: binding cannot bind a borrow.
            if is_borrow_typed(value, state, item, typed) {
                return Err(CheckError::new(
                    Rule::Borrow,
                    *span,
                    "binding cannot bind a borrow",
                ));
            }
            // Consume linear components of value into the binding.
            if let Expr::Var { name: v, .. } = value.as_ref() {
                if matches!(state.get(v), Some(Value::Tracked(Tracked::Owned))) {
                    state.insert(
                        v.clone(),
                        Value::Tracked(Tracked::Moved(MoveInfo {
                            at: *span,
                            why: format!("bound to `{name}`"),
                        })),
                    );
                }
            }
            let vt = require_ty(value, item, typed)?;
            let old = bind_name(state, name, &vt, typed)?;
            check_expr(body, state, item, typed, ret_ty, in_tail)?;
            // Restore previous binding (shadowing).
            match old {
                Some(val) => { state.insert(name.clone(), val); }
                None => { state.remove(name); }
            }
            Ok(())
        }
        Expr::Seq { first, second, .. } => {
            // `first` runs for its effect; its value is discarded (a linear
            // result would be flagged as a leak at the function boundary).
            // `second` runs in the same state and is the sequence's value.
            check_expr(first, state, item, typed, ret_ty, false)?;
            check_expr(second, state, item, typed, ret_ty, in_tail)
        }
        Expr::If { arms, span } => {
            // Bodies are exclusive (only one runs), but conditions run
            // sequentially: reaching arm i's body means conditions 0..i all
            // executed. So condition consumption accumulates in `running`
            // across arms while body consumption does not (D070). Moves join
            // into `state` afterwards so a value consumed on *any* path is
            // Moved after the if.
            let mut running = state.clone();
            for arm in arms {
                if let Some(c) = &arm.condition {
                    check_expr(c, &mut running, item, typed, ret_ty, false)?;
                }
                let mut state_body = running.clone();
                check_expr(&arm.body, &mut state_body, item, typed, ret_ty, in_tail)?;
                // Union-of-moves join: Owned before, not Owned after → Moved.
                join_arm_moves(state, &state_body, *span);
            }
            Ok(())
        }
        Expr::Match {
            scrutinee,
            arms,
            span,
        } => {
            let scrut_borrowed = is_borrow_typed(scrutinee, state, item, typed);
            // Matching on the receiver handle (D057) or a self-peek field
            // (D060: `match Self_.cur`) is non-consuming — it inspects link
            // fields without moving the region. Also covers the template
            // matches generated by multi-step write desugaring.
            if !matches!(scrutinee.as_ref(), Expr::Self_ { .. }) && !is_self_peek(scrutinee) {
                check_expr(scrutinee, state, item, typed, ret_ty, false)?;
            }
            if !scrut_borrowed {
                // Consuming match (rule 10). A scrutinee var already moved by
                // an outer use is caught by the Var arm in check_expr above.
                consume_root_var(scrutinee, state, *span, "matched here")?;
            }
            let st = require_ty(scrutinee, item, typed)?;
            // Bodies are exclusive (only one runs), but `when`-guards run
            // sequentially: reaching arm i's body means guards 0..i all
            // executed. Guard consumption accumulates in `running`; pattern
            // bindings and body consumption do not (D070). Each arm starts
            // from the post-scrutinee state plus prior guard moves.
            let pre = state.clone();
            for arm in arms {
                let mut state_arm = pre.clone();
                bind_pattern(
                    &arm.pattern,
                    item,
                    &st,
                    scrut_borrowed,
                    &mut state_arm,
                    typed,
                )?;
                check_expr(
                    &arm.body,
                    &mut state_arm,
                    item,
                    typed,
                    ret_ty,
                    in_tail,
                )?;
                // Union-of-moves join: Owned before, not Owned after → Moved.
                join_arm_moves(state, &state_arm, *span);
            }
            Ok(())
        }
        Expr::ListLit { elems, span } => {
            for e in elems {
                if is_borrow_typed(e, state, item, typed) {
                    return Err(CheckError::new(
                        Rule::Borrow,
                        *span,
                        "rule 11: cannot embed borrow in constructed value",
                    ));
                }
                // Check first (catches reuse of an already-moved value), then
                // mark the embedding as a consuming use.
                check_expr(e, state, item, typed, ret_ty, false)?;
                consume_root_var(e, state, e.span(), "embedded in list literal")?;
            }
            Ok(())
        }
        Expr::Lambda { body, .. } => {
            check_expr(body, state, item, typed, ret_ty, false)?;
            Ok(())
        }
        Expr::Comprehension { generators, accs, guards, result, .. } => {
            for g in generators { for s in &g.sources { check_expr(s, state, item, typed, ret_ty, false)?; } }
            for a in accs { check_expr(&a.init, state, item, typed, ret_ty, false)?; }
            for g in guards { check_expr(g, state, item, typed, ret_ty, false)?; }
            check_expr(result, state, item, typed, ret_ty, false)?;
            Ok(())
        }
        Expr::Product { elems, span } => {
            // Same ownership story as list literals: embedding an owned value
            // into a constructed product consumes it (rule 10); borrows cannot
            // be embedded (rule 11).
            for e in elems {
                if is_borrow_typed(e, state, item, typed) {
                    return Err(CheckError::new(
                        Rule::Borrow,
                        *span,
                        "rule 11: cannot embed borrow in constructed value",
                    ));
                }
                // Check first (catches reuse of an already-moved value), then
                // mark the embedding as a consuming use.
                check_expr(e, state, item, typed, ret_ty, false)?;
                consume_root_var(e, state, e.span(), "embedded in product")?;
            }
            Ok(())
        }
        Expr::Unit { .. } | Expr::Do { .. } => Ok(()),
    }
}

/// Consume a root-Var expression if it's tracked owned. Non-Var expressions
/// are no-ops (sub-consumes handled by recursion into check_expr).
fn consume_root_var(
    expr: &Expr,
    state: &mut HashMap<String, Value>,
    span: Span,
    why: &str,
) -> Result<(), CheckError> {
    match expr {
        Expr::Var { name, .. } => consume_state(state, name, span, why),
        _ => Ok(()),
    }
}

/// Union-of-moves join after a branch: names Owned before the branch but not
/// Owned after it become Moved (they may have been consumed in that branch).
fn join_arm_moves(
    state: &mut HashMap<String, Value>,
    arm_state: &HashMap<String, Value>,
    node_span: Span,
) {
    let moved: Vec<(String, MoveInfo)> = state
        .iter()
        .filter(|(_, v)| matches!(v, Value::Tracked(Tracked::Owned)))
        .filter(|(k, _)| {
            !matches!(arm_state.get(k.as_str()), Some(Value::Tracked(Tracked::Owned)))
        })
        .map(|(k, _)| {
            let info = match arm_state.get(k.as_str()) {
                Some(Value::Tracked(Tracked::Moved(i))) => i.clone(),
                _ => MoveInfo {
                    at: node_span,
                    why: "moved in one branch".to_string(),
                },
            };
            (k.clone(), info)
        })
        .collect();
    for (k, info) in moved {
        state.insert(k, Value::Tracked(Tracked::Moved(info)));
    }
}

fn check_clauses_linear(
    clauses: &[wag_syntax::UpdateClause],
    state: &mut HashMap<String, Value>,
    item: &str,
    typed: &TypedProgram,
    ret_ty: Option<&Ty>,
) -> Result<(), CheckError> {
    for c in clauses {
        match c {
            wag_syntax::UpdateClause::Leaf { value, .. } => {
                if is_borrow_typed(value, state, item, typed) {
                    return Err(CheckError::new(
                        Rule::Borrow,
                        value.span(),
                        "rule 11: cannot embed borrow in record update",
                    ));
                }
                consume_root_var(value, state, value.span(), "embedded in record update")?;
                check_expr(value, state, item, typed, ret_ty, false)?;
            }
            wag_syntax::UpdateClause::Group { clauses, .. } => {
                check_clauses_linear(clauses, state, item, typed, ret_ty)?;
            }
        }
    }
    Ok(())
}

fn bind_pattern(
    pat: &Pat,
    item: &str,
    scrut: &Ty,
    scrut_borrowed: bool,
    state: &mut HashMap<String, Value>,
    typed: &TypedProgram,
) -> Result<(), CheckError> {
    match pat {
        Pat::Wildcard { .. } => Ok(()),
        // Literal pattern (§9): binds no variable.
        Pat::Literal { .. } => Ok(()),
        Pat::Var { name, span } => {
            // Side table first: inference recorded the resolved binding type here.
            let ty = match typed.expr_types.get(&(item.to_string(), *span)) {
                Some(t) => t.clone(),
                None if scrut_borrowed && !matches!(scrut, Ty::Borrow(_)) => {
                    Ty::Borrow(Box::new(scrut.clone()))
                }
                None => scrut.clone(),
            };
            bind_name(state, name, &ty, typed).map(|_| ())
        }
        Pat::Ctor { name, args, span, .. } => {
            if let Some(info) = typed.constructors.get(name) {
                // A1b: apply the scrutinee's actual args to the declared field
                // types — bound names get concrete types, never raw param vars.
                let scrut_args: Vec<Ty> = match scrut.strip_borrow() {
                    Ty::App(_, a) => a.clone(),
                    _ => Vec::new(),
                };
                for (i, ap) in args.iter().enumerate() {
                    let ft = info.fields.get(i).map(|_| {
                        let fty = info.field_ty(i, &scrut_args);
                        if scrut_borrowed
                            && !wag_types::ty_is_pointer_free(&fty, &typed.pointer_free_types)
                        {
                            match &fty {
                                Ty::Borrow(_) => fty.clone(),
                                o => Ty::Borrow(Box::new(o.clone())),
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
                    let nested_borrowed = scrut_borrowed
                        && !wag_types::ty_is_pointer_free(&ft, &typed.pointer_free_types);
                    bind_pattern(ap, item, &ft, nested_borrowed, state, typed)?;
                }
            }
            Ok(())
        }
        // Native product destructuring `(p1, p2 [, p3])` (§3.6).
        Pat::Product { elems, .. } => {
            let fields = match scrut.strip_borrow() {
                Ty::App(n, args) if (n == "Pair" || n == "Triple") && args.len() == elems.len() => {
                    args.clone()
                }
                _ => Vec::new(),
            };
            for (i, ap) in elems.iter().enumerate() {
                let ft = fields.get(i).cloned().unwrap_or(Ty::unit());
                let ft = if scrut_borrowed {
                    if wag_types::ty_is_pointer_free(&ft, &typed.pointer_free_types) {
                        ft
                    } else {
                        match &ft {
                            Ty::Borrow(_) => ft.clone(),
                            o => Ty::Borrow(Box::new(o.clone())),
                        }
                    }
                } else {
                    ft
                };
                let nested_borrowed = scrut_borrowed
                    && !wag_types::ty_is_pointer_free(&ft, &typed.pointer_free_types);
                bind_pattern(ap, item, &ft, nested_borrowed, state, typed)?;
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use wag_types::{
        compute_pointer_free, ty_is_pointer_free, CtorInfo, FieldInfo, Scheme, TypeCtors,
    };

    /// TypedProgram with two user types:
    /// - `Color` (Red/Blue, nullary) — pointer-free
    /// - `Bag` (Hold : List Int) — linear
    fn typed() -> TypedProgram {
        let mk = |name: &str, ty: &str, fields: Vec<Ty>| CtorInfo {
            name: name.into(),
            type_name: ty.into(),
            fields: fields.into_iter().map(|t| FieldInfo::new(None, t)).collect(),
            n_params: 0,
            scheme: Scheme::mono(Ty::Con(ty.into())),
        };
        let constructors: BTreeMap<String, CtorInfo> = vec![
            mk("Red", "Color", vec![]),
            mk("Blue", "Color", vec![]),
            mk("Hold", "Bag", vec![Ty::list(Ty::int())]),
        ]
        .into_iter()
        .map(|c| (c.name.clone(), c))
        .collect();
        let mut type_ctors: TypeCtors = BTreeMap::new();
        type_ctors.insert("Color".into(), vec!["Red".into(), "Blue".into()]);
        type_ctors.insert("Bag".into(), vec!["Hold".into()]);
        let pointer_free_types = compute_pointer_free(&constructors);
        TypedProgram {
            bindings: BTreeMap::new(),
            constructors,
            type_ctors,
            actors: BTreeMap::new(),
            actor_outbox: BTreeMap::new(),
            match_types: std::collections::HashMap::new(),
            expr_types: std::collections::HashMap::new(),
            residency: BTreeMap::new(),
            method_tys: BTreeMap::new(),
            type_params: BTreeMap::new(),
            pointer_free_types,
            ctor_meta: BTreeMap::new(),
            op_dispatch: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn value_for_data_types_are_linear() {
        let t = typed();
        // Every owned data value is linear, regardless of pointer-freedom (§4.3.2) —
        // inline primitives included.
        assert_eq!(value_for(&Ty::int(), &t).unwrap(), Value::Tracked(Tracked::Owned));
        assert_eq!(value_for(&Ty::bool(), &t).unwrap(), Value::Tracked(Tracked::Owned));
        assert_eq!(value_for(&Ty::unit(), &t).unwrap(), Value::Tracked(Tracked::Owned));
        // Heap-backed data types, as before.
        assert_eq!(value_for(&Ty::list(Ty::int()), &t).unwrap(), Value::Tracked(Tracked::Owned));
        assert_eq!(value_for(&Ty::Con("Bag".into()), &t).unwrap(), Value::Tracked(Tracked::Owned));
    }

    #[test]
    fn value_for_reference_values_are_untracked() {
        let t = typed();
        // A borrowed view is a read-only, non-owning reference (§4.3.3).
        assert_eq!(
            value_for(&Ty::Borrow(Box::new(Ty::list(Ty::int()))), &t).unwrap(),
            Value::Tracked(Tracked::Borrowed)
        );
        // Reference values — function values and actor handles — are copyable/sharable (§5.5).
        let f = Ty::Fun(vec![Ty::int()], Box::new(Ty::unit()));
        assert!(matches!(value_for(&f, &t).unwrap(), Value::Untracked(_)));
        assert!(matches!(value_for(&Ty::actor(Ty::int()), &t).unwrap(), Value::Untracked(_)));
    }

    #[test]
    fn pointer_free_recurses_through_wrappers() {
        let t = typed();
        let pf = &t.pointer_free_types;
        assert!(ty_is_pointer_free(&Ty::int(), pf));
        assert!(ty_is_pointer_free(&Ty::bool(), pf));
        assert!(ty_is_pointer_free(&Ty::unit(), pf));
        assert!(ty_is_pointer_free(&Ty::option(Ty::int()), pf));
        assert!(ty_is_pointer_free(&Ty::pair(Ty::int(), Ty::bool()), pf));
        assert!(ty_is_pointer_free(&Ty::triple(Ty::int(), Ty::int(), Ty::int()), pf));
        // NOTE: a borrow is a third ownership category (non-consuming, §4.3.3),
        // neither PF nor linear. classify() short-circuits a top-level `&T` to
        // Own::Borrow before the PF check is ever consulted, and §3.1 forbids
        // nesting `&T` inside any container — so the PF walker never sees a
        // well-formed Borrow. We therefore do NOT assert on its defensive Borrow arm.
        assert!(ty_is_pointer_free(&Ty::actor(Ty::int()), pf));
        // Functions are treated as PF-like handles.
        assert!(ty_is_pointer_free(
            &Ty::Fun(vec![Ty::int()], Box::new(Ty::unit())),
            pf
        ));
    }

    #[test]
    fn pointer_free_fails_when_any_component_is_heap() {
        let t = typed();
        let pf = &t.pointer_free_types;
        assert!(!ty_is_pointer_free(&Ty::list(Ty::int()), pf));
        assert!(!ty_is_pointer_free(&Ty::option(Ty::list(Ty::int())), pf));
        assert!(!ty_is_pointer_free(&Ty::pair(Ty::int(), Ty::list(Ty::bool())), pf));
        // §3.2: user types are pointer-free iff every constructor field is
        // pointer-free — `Color` (nullary constructors) qualifies; `Bag`
        // (holds a `List Int`) does not.
        assert!(ty_is_pointer_free(&Ty::Con("Color".into()), pf));
        assert!(!ty_is_pointer_free(&Ty::Con("Bag".into()), pf));
        // unknown type is conservatively non-PF
        assert!(!ty_is_pointer_free(&Ty::Con("Mystery".into()), pf));
        // unification variables are conservatively non-PF
        assert!(!ty_is_pointer_free(&Ty::Var(0), pf));
    }

    #[test]
    fn pointer_free_recursive_user_type_is_not_pf() {
        // `Tree = Leaf | Node Tree Int` is self-referential → non-pointer-free (§3.2).
        let mk = |name: &str, ty: &str, fields: Vec<Ty>| CtorInfo {
            name: name.into(),
            type_name: ty.into(),
            fields: fields.into_iter().map(|t| FieldInfo::new(None, t)).collect(),
            n_params: 0,
            scheme: Scheme::mono(Ty::Con(ty.into())),
        };
        let constructors: std::collections::BTreeMap<String, CtorInfo> = vec![
            mk("Leaf", "Tree", vec![]),
            mk("Node", "Tree", vec![Ty::Con("Tree".into()), Ty::int()]),
        ]
        .into_iter()
        .map(|c| (c.name.clone(), c))
        .collect();
        // Must terminate (no infinite recursion) and classify non-PF.
        let pf = compute_pointer_free(&constructors);
        assert!(!ty_is_pointer_free(&Ty::Con("Tree".into()), &pf));
    }

    #[test]
    fn value_for_abstract_types_are_linear_too() {
        let t = typed();
        // Under full linearity there is no "undecidable → copyable" escape hatch: a bare
        // type var may instantiate to a heap-backed type such as `List Int`, so every data
        // shape — concrete or abstract — is tracked linear (§4.3.2).
        assert_eq!(
            value_for(&Ty::option(Ty::list(Ty::int())), &t).unwrap(),
            Value::Tracked(Tracked::Owned)
        );
        assert_eq!(
            value_for(&Ty::pair(Ty::Var(0), Ty::list(Ty::bool())), &t).unwrap(),
            Value::Tracked(Tracked::Owned)
        );
        assert_eq!(value_for(&Ty::Var(0), &t).unwrap(), Value::Tracked(Tracked::Owned));
        assert_eq!(value_for(&Ty::Bound(0), &t).unwrap(), Value::Tracked(Tracked::Owned));
        assert_eq!(
            value_for(&Ty::option(Ty::Var(0)), &t).unwrap(),
            Value::Tracked(Tracked::Owned)
        );
    }
}
