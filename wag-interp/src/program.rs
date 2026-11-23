//! Load-time function and constructor tables (§4.6.5).

use std::collections::HashMap;

use wag_syntax::{ActorDef, BinOp, Expr, FuncDef, IfArm, Item, MatchArm, Program, Span};
use wag_types::{Ty, TypedProgram};

use crate::value::{ConstructorId, FnId, Value, CTOR_ARRAY};

/// One entry in the flat function table.
#[derive(Clone, Debug)]
pub struct FnEntry {
    pub name: String,
    pub params: Vec<String>,
    /// Parameter types (owned, borrows stripped for arity only).
    pub param_tys: Vec<Ty>,
    pub body: wag_syntax::Expr,
    pub result_ty: Ty,
    /// Per-parameter linearity flags (D-B): true when the parameter type is owned
    /// data — a simple `Var` argument may be moved out of the caller env instead of
    /// deep-cloned at non-tail call sites. Pure mirror of the checker's `value_for`
    /// classification (§4.3, §7 rule 10); computed once at load.
    pub param_linear: Vec<bool>,
    /// True for move-and-return methods (D-B): `result_ty` mentions the receiver's
    /// head type name, so the checker consumes the receiver var at the call site and
    /// the VM may move it out of the env. Read-only methods (`peek`, `isEmpty`) are
    /// false — their receivers must stay in the caller env.
    pub recv_moved: bool,
}

/// Load-time metadata for one `@`-declaration (§5.5.2).
#[derive(Clone, Debug)]
pub struct ActorEntry {
    pub name: String,
    /// Behavior function id in [`LoadedProgram::functions`].
    pub behavior: FnId,
    /// `Msg` type name (head constructor type), if known.
    pub msg_type: Option<String>,
    /// Constructor id of nullary `Start` on this actor's Msg type, if any.
    pub start_ctor: Option<ConstructorId>,
    /// `Down` constructor (legacy monitor support; removed in D078 Step 5).
    #[deprecated(note = "monitor/Down removed by D078")]
    pub down_ctor: Option<ConstructorId>,
    /// `main_boot: List String -> State` — the desugared `Start` arm of `@main`.
    /// Present only for `@main`; the runtime calls it with argv to obtain the
    /// initial state (§5.4). `None` for every other actor.
    pub boot: Option<FnId>,
    /// `main_end: (State, Int) -> Unit` — the desugared `End` arm of `@main`.
    /// Present only for `@main`; the runtime calls it on an `End` message and
    /// exits with the given code (§5.4). `None` for every other actor.
    pub end: Option<FnId>,
    /// `End: Int` constructor on this actor's `Msg` type, if any — the id the
    /// runtime compares a dequeued value's constructor against to detect an
    /// `End` message before calling `end` (§5.4). Set for every actor whose Msg
    /// type has an `End`; the runtime only acts on it for `@main`, because only
    /// `@main` also has `end` (the `main_end` FnId) set.
    pub end_ctor: Option<ConstructorId>,
}

/// Identity of a constructor. Each `CtorEntry` has exactly one; this replaces
/// the old set of mutually-exclusive boolean flags so illegal combinations are
/// unrepresentable (§4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CtorKind {
    True,
    False,
    Unit,
    Pair,
    Triple,
    /// Any other user-defined data constructor.
    User,
}

/// Constructor runtime metadata.
#[derive(Clone, Debug)]
pub struct CtorEntry {
    pub name: String,
    pub id: ConstructorId,
    pub type_name: String,
    pub arity: usize,
    /// True when values of this constructor are pointer-free `Value::Data`.
    pub pointer_free: bool,
    /// Which constructor this is. Exactly one identity — the old 9-bool encoding
    /// allowed illegal combinations; a single enum makes them unrepresentable (§4).
    pub kind: CtorKind,
    /// Per-field flag: true if the field type is `Ref T` (arg must be a heap cell).
    pub field_is_ref: Vec<bool>,
    /// True when values of this constructor are region-resident (D054/D058):
    /// construction goes through the region path, not the Data-cell path.
    pub resident: bool,
    /// Field names in declaration order — used for bare receiver-field reads
    /// in method bodies (D056) and write-path resolution.
    pub field_names: Vec<String>,
    /// When set, this ctor has a `Bool` decider body: the VM evaluates it
    /// before construction. True → `Some(Data1(…))`, False → `None`.
    pub constrained_body: Option<FnId>,
}

/// Fully loaded, executable module.
#[derive(Clone, Debug)]
pub struct LoadedProgram {
    pub functions: Vec<FnEntry>,
    /// Primary (first-in-source-order) function per name — D052.
    pub fn_by_name: HashMap<String, FnId>,
    /// All top-level functions keyed by (name, arity) for overload disambiguation — D051/D052.
    pub fn_overloads: HashMap<(String, usize), FnId>,
    pub ctors: Vec<CtorEntry>,
    pub ctor_by_name: HashMap<String, ConstructorId>,
    /// Method dispatch table (D056/D057): keyed by (ctor id of the receiver
    /// handle's entry cell, method name). Methods are not top-level names —
    /// they never appear in `fn_by_name`/`fn_overloads`.
    pub method_fns: HashMap<(ConstructorId, String), FnId>,
    pub actors: HashMap<String, ActorEntry>,
    /// When true (after wag-check), matched-out cells may be reused in place.
    pub cell_reuse: bool,
}

impl LoadedProgram {
    pub fn fn_id(&self, name: &str) -> Option<FnId> {
        self.fn_by_name.get(name).copied()
    }

    /// Resolve a function by name and expected arity (for Apply sites with overloaded names).
    /// Falls back to the primary if no exact-arity match exists.
    pub fn fn_id_arity(&self, name: &str, arity: usize) -> Option<FnId> {
        self.fn_overloads
            .get(&(name.to_string(), arity))
            .copied()
            .or_else(|| self.fn_by_name.get(name).copied())
    }

    pub fn get_fn(&self, id: FnId) -> &FnEntry {
        &self.functions[id.0 as usize]
    }

    pub fn actor(&self, name: &str) -> Option<&ActorEntry> {
        self.actors.get(name)
    }

    /// Canonical id→entry lookup for constructors (mirrors [`Self::get_fn`]).
    pub fn get_ctor(&self, id: ConstructorId) -> &CtorEntry {
        &self.ctors[id.0 as usize]
    }

    /// `Some` constructor ID (prelude-declared, D088).
    pub fn opt_some_id(&self) -> ConstructorId {
        *self.ctor_by_name.get("Some").expect("Some ctor not in program")
    }

    /// `None` constructor ID (prelude-declared, D088).
    pub fn opt_none_id(&self) -> ConstructorId {
        *self.ctor_by_name.get("None").expect("None ctor not in program")
    }

    /// `Char` constructor ID (prelude constrained datatype).
    pub fn char_ctor_id(&self) -> Option<ConstructorId> {
        self.ctor_by_name.get("Char").copied()
    }

    /// `String` constructor ID (prelude constrained datatype).
    pub fn str_ctor_id(&self) -> Option<ConstructorId> {
        self.ctor_by_name.get("String").copied()
    }

    /// Build a `Char` value: `Data1(Char_ctor, Int(codepoint))`.
    pub fn make_char(&self, cp: u32) -> Value {
        let id = self.char_ctor_id().expect("Char ctor not in program");
        Value::Data1(id, Box::new(Value::Int(cp as i64)))
    }

    /// Build a `String` value: `Data1(String_ctor, Vec Int bytes)`.
    pub fn make_string(&self, s: &str) -> Value {
        let str_id = self.str_ctor_id().expect("String ctor not in program");
        let vbuf_id = self.ctor_by_name.get("VBuf").expect("VBuf ctor not in program");
        let some_id = self.opt_some_id();
        let slots: Vec<Value> = s.bytes()
            .map(|b| Value::Data1(some_id, Box::new(Value::Int(b as i64))))
            .collect();
        let arr = Value::Data(CTOR_ARRAY, slots);
        let n = s.len() as i64;
        let vec_val = Value::Data(*vbuf_id, vec![arr, Value::Int(n), Value::Int(n)]);
        Value::Data1(str_id, Box::new(vec_val))
    }

    /// Extract a `String` from a `Value` (inverse of [`Self::make_string`]).
    pub fn value_to_string(&self, v: &Value) -> Option<String> {
        let str_id = self.str_ctor_id()?;
        let vbuf_id = *self.ctor_by_name.get("VBuf")?;
        let some_id = self.opt_some_id();
        let Value::Data1(id, inner) = v else { return None };
        if *id != str_id { return None; }
        let Value::Data(cid, fields) = inner.as_ref() else { return None };
        if *cid != vbuf_id { return None; }
        let Value::Data(CTOR_ARRAY, slots) = &fields[0] else { return None };
        let mut bytes = Vec::with_capacity(slots.len());
        for slot in slots {
            let Value::Data1(sid, b) = slot else { return None };
            if *sid != some_id { return None; }
            let Value::Int(n) = b.as_ref() else { return None };
            bytes.push(*n as u8);
        }
        String::from_utf8(bytes).ok()
    }

    /// Dev-only invariant: the name→id maps must agree with the Vec tables they index.
    /// `LoadedProgram` is built once in [`load`] and then shared immutably, so this can
    /// only fail if construction itself is buggy — catch it at build time rather than as a
    /// wrong-ctor lookup deep in evaluation. (The id always equals the Vec index by how
    /// `load` assigns ids, so both the id and the name are checked.)
    #[cfg(debug_assertions)]
    fn assert_consistent(&self) {
        for (name, &cid) in &self.ctor_by_name {
            let e = &self.ctors[cid.0 as usize];
            debug_assert_eq!(e.id, cid, "ctor id mismatch for {name}");
            debug_assert_eq!(&e.name, name, "ctor name mismatch at {:?}", cid);
        }
        for (name, &fid) in &self.fn_by_name {
            let e = &self.functions[fid.0 as usize];
            debug_assert_eq!(&e.name, name, "fn name mismatch at {:?}", fid);
        }
    }
}

/// Linear (owned-data) classification of a type — mirror of
/// `wag_check::linear::value_for` (§4.3): linear = owned data; non-linear =
/// borrows, `Ref` links, function values, actor handles.
fn ty_is_linear(ty: &Ty) -> bool {
    match ty {
        Ty::Borrow(_) => false,
        Ty::App(n, _) if n == "Ref" => false,
        Ty::Fun(_, _) => false,
        Ty::App(n, _) if n == "Actor" => false,
        // Every other owned value is linear data — including bare type vars/bounds,
        // which may instantiate to heap-backed types.
        _ => true,
    }
}

/// True when `ty` mentions type constructor `name` anywhere (D-B: move-and-return
/// detection). Mirror of `wag_check::linear::ty_mentions`.
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

fn rewrite_clause(c: &wag_syntax::UpdateClause) -> wag_syntax::UpdateClause {
    use wag_syntax::UpdateClause::*;
    match c {
        Leaf { path, value, span } => Leaf {
            path: path.clone(),
            value: rewrite_self(value),
            span: *span,
        },
        Group { path, clauses, span } => Group {
            path: path.clone(),
            clauses: clauses.iter().map(rewrite_clause).collect(),
            span: *span,
        },
    }
}

/// Rewrite `Self_` to `Var("__self__")` throughout a method body (D056): the
/// receiver handle is an ordinary env binding at runtime.
fn rewrite_self(expr: &wag_syntax::Expr) -> wag_syntax::Expr {
    use wag_syntax::Expr::*;
    match expr {
        Self_ { span } => Var { name: "__self__".into(), span: *span },
        Int { .. } | Str { .. } | Var { .. } | Upper { .. } | AtName { .. } | Unit { .. } => expr.clone(),
        Send { target, msg, span } => Send {
            target: Box::new(rewrite_self(target)),
            msg: Box::new(rewrite_self(msg)),
            span: *span,
        },
        Apply { fun, args, span } => Apply {
            fun: Box::new(rewrite_self(fun)),
            args: args.iter().map(rewrite_self).collect(),
            span: *span,
        },
        BinOp { op, lhs, rhs, span } => BinOp {
            op: *op,
            lhs: Box::new(rewrite_self(lhs)),
            rhs: Box::new(rewrite_self(rhs)),
            span: *span,
        },
        Field { base, field, span } => Field {
            base: Box::new(rewrite_self(base)),
            field: field.clone(),
            span: *span,
        },
        RecordUpdate { receiver, clauses, span } => RecordUpdate {
            receiver: Box::new(rewrite_self(receiver)),
            clauses: clauses.iter().map(rewrite_clause).collect(),
            span: *span,
        },
        SetField { receiver, field_idx, value, span } => SetField {
            receiver: Box::new(rewrite_self(receiver)),
            field_idx: *field_idx,
            value: Box::new(rewrite_self(value)),
            span: *span,
        },
        RegionWrite { receiver, path, value, span } => RegionWrite {
            receiver: Box::new(rewrite_self(receiver)),
            path: path.clone(),
            value: Box::new(rewrite_self(value)),
            span: *span,
        },
        Borrow { inner, span } => Borrow {
            inner: Box::new(rewrite_self(inner)),
            span: *span,
        },
        Bind { name, value, body, span } => Bind {
            name: name.clone(),
            value: Box::new(rewrite_self(value)),
            body: Box::new(rewrite_self(body)),
            span: *span,
        },
        Seq { first, second, span } => Seq {
            first: Box::new(rewrite_self(first)),
            second: Box::new(rewrite_self(second)),
            span: *span,
        },
        If { arms, span } => If {
            arms: arms
                .iter()
                .map(|a| wag_syntax::IfArm {
                    condition: a.condition.as_ref().map(rewrite_self),
                    body: rewrite_self(&a.body),
                })
                .collect(),
            span: *span,
        },
        Match { scrutinee, arms, span } => Match {
            scrutinee: Box::new(rewrite_self(scrutinee)),
            arms: arms
                .iter()
                .map(|a| wag_syntax::MatchArm {
                    pattern: a.pattern.clone(),
                    body: rewrite_self(&a.body),
                })
                .collect(),
            span: *span,
        },
        Do { stmts, span } => Do {
            stmts: stmts
                .iter()
                .map(|s| match s {
                    wag_syntax::DoStmt::Bind { name, expr } => {
                        wag_syntax::DoStmt::Bind { name: name.clone(), expr: rewrite_self(expr) }
                    }
                    wag_syntax::DoStmt::Return(e) => wag_syntax::DoStmt::Return(rewrite_self(e)),
                })
                .collect(),
            span: *span,
        },
        ListLit { elems, span } => ListLit {
            elems: elems.iter().map(rewrite_self).collect(),
            span: *span,
        },
        Lambda { params, body, span } => Lambda {
            params: params.clone(),
            body: Box::new(rewrite_self(body)),
            span: *span,
        },
        Comprehension { .. } => expr.clone(), // desugared before load; no Self_ expected
        Product { elems, span } => Product {
            elems: elems.iter().map(rewrite_self).collect(),
            span: *span,
        },
    }
}

/// Whether a name is an operator symbol eligible for typeclass dispatch (D075).
/// Mirrors the parser's operator set (§6 surface syntax) and the index built by
/// `wag-types` from class method names.
fn is_op_name(name: &str) -> bool {
    matches!(
        name,
        "+" | "-" | "*" | "/" | "%" | ">" | "<" | ">=" | "<=" | "==" | "!=" | "!" | "^" | "|" | "&"
            | "<<" | ">>"
    )
}

/// Rewrite dispatched BinOp use-sites into applications of the overload impl.
///
/// Walks the expression tree; any `BinOp` whose span appears in `op_dispatch`
/// is replaced by `Apply(Var(impl_name), [lhs, rhs])`. Unary `!` (BitNot) uses
/// only `lhs` (the AST `rhs` is a parse-time dummy). Non-dispatched BinOps are
/// left untouched (they remain native VM operations).
fn rewrite_op_dispatch(expr: &Expr, op_dispatch: &HashMap<Span, String>) -> Expr {
    match expr {
        Expr::BinOp { op, lhs, rhs, span } => {
            if let Some(impl_name) = op_dispatch.get(span) {
                // Recurse into operands: a dispatched BinOp may contain another
                // dispatched BinOp (e.g. `a + b + c` → two nested String `+`).
                let lhs_rw = rewrite_op_dispatch(lhs, op_dispatch);
                let rhs_rw = rewrite_op_dispatch(rhs, op_dispatch);
                let args = if matches!(op, BinOp::BitNot) {
                    vec![lhs_rw]
                } else {
                    vec![lhs_rw, rhs_rw]
                };
                Expr::Apply {
                    fun: Box::new(Expr::Var {
                        name: impl_name.clone(),
                        span: *span,
                    }),
                    args,
                    span: *span,
                }
            } else {
                Expr::BinOp {
                    op: *op,
                    lhs: Box::new(rewrite_op_dispatch(lhs, op_dispatch)),
                    rhs: Box::new(rewrite_op_dispatch(rhs, op_dispatch)),
                    span: *span,
                }
            }
        }
        Expr::Apply { fun, args, span } => {
            if let Some(impl_name) = op_dispatch.get(span) {
                // Named-function dispatch (D074): rename the callee to the resolved impl.
                Expr::Apply {
                    fun: Box::new(Expr::Var {
                        name: impl_name.clone(),
                        span: *span,
                    }),
                    args: args.iter().map(|a| rewrite_op_dispatch(a, op_dispatch)).collect(),
                    span: *span,
                }
            } else {
                Expr::Apply {
                    fun: Box::new(rewrite_op_dispatch(fun, op_dispatch)),
                    args: args.iter().map(|a| rewrite_op_dispatch(a, op_dispatch)).collect(),
                    span: *span,
                }
            }
        }
        Expr::Field { base, field, span } => Expr::Field {
            base: Box::new(rewrite_op_dispatch(base, op_dispatch)),
            field: field.clone(),
            span: *span,
        },
        Expr::RecordUpdate { receiver, clauses, span } => Expr::RecordUpdate {
            receiver: Box::new(rewrite_op_dispatch(receiver, op_dispatch)),
            clauses: clauses.iter().map(|c| rewrite_clause_op(c, op_dispatch)).collect(),
            span: *span,
        },
        Expr::SetField { receiver, field_idx, value, span } => Expr::SetField {
            receiver: Box::new(rewrite_op_dispatch(receiver, op_dispatch)),
            field_idx: *field_idx,
            value: Box::new(rewrite_op_dispatch(value, op_dispatch)),
            span: *span,
        },
        Expr::RegionWrite { receiver, path, value, span } => Expr::RegionWrite {
            receiver: Box::new(rewrite_op_dispatch(receiver, op_dispatch)),
            path: path.clone(),
            value: Box::new(rewrite_op_dispatch(value, op_dispatch)),
            span: *span,
        },
        Expr::Borrow { inner, span } => Expr::Borrow {
            inner: Box::new(rewrite_op_dispatch(inner, op_dispatch)),
            span: *span,
        },
        Expr::Bind { name, value, body, span } => Expr::Bind {
            name: name.clone(),
            value: Box::new(rewrite_op_dispatch(value, op_dispatch)),
            body: Box::new(rewrite_op_dispatch(body, op_dispatch)),
            span: *span,
        },
        Expr::Seq { first, second, span } => Expr::Seq {
            first: Box::new(rewrite_op_dispatch(first, op_dispatch)),
            second: Box::new(rewrite_op_dispatch(second, op_dispatch)),
            span: *span,
        },
        Expr::If { arms, span } => Expr::If {
            arms: arms
                .iter()
                .map(|a| IfArm {
                    condition: a.condition.as_ref().map(|g| rewrite_op_dispatch(g, op_dispatch)),
                    body: rewrite_op_dispatch(&a.body, op_dispatch),
                })
                .collect(),
            span: *span,
        },
        Expr::Match { scrutinee, arms, span } => Expr::Match {
            scrutinee: Box::new(rewrite_op_dispatch(scrutinee, op_dispatch)),
            arms: arms
                .iter()
                .map(|a| MatchArm {
                    pattern: a.pattern.clone(),
                    body: rewrite_op_dispatch(&a.body, op_dispatch),
                })
                .collect(),
            span: *span,
        },
        Expr::Do { stmts, span } => Expr::Do {
            stmts: stmts.iter().map(|s| rewrite_do_stmt(s, op_dispatch)).collect(),
            span: *span,
        },
        Expr::ListLit { elems, span } => Expr::ListLit {
            elems: elems.iter().map(|e| rewrite_op_dispatch(e, op_dispatch)).collect(),
            span: *span,
        },
        Expr::Lambda { params, body, span } => Expr::Lambda {
            params: params.clone(),
            body: Box::new(rewrite_op_dispatch(body, op_dispatch)),
            span: *span,
        },
        Expr::Comprehension { generators, accs, guards, result, span } => {
            Expr::Comprehension {
                generators: generators.iter().map(|g| wag_syntax::Generator {
                    vars: g.vars.clone(),
                    sources: g.sources.iter().map(|s| rewrite_op_dispatch(s, op_dispatch)).collect(),
                    span: g.span,
                }).collect(),
                accs: accs.iter().map(|a| wag_syntax::AccInit {
                    name: a.name.clone(),
                    init: rewrite_op_dispatch(&a.init, op_dispatch),
                    span: a.span,
                }).collect(),
                guards: guards.iter().map(|g| rewrite_op_dispatch(g, op_dispatch)).collect(),
                result: Box::new(rewrite_op_dispatch(result, op_dispatch)),
                span: *span,
            }
        }
        Expr::Product { elems, span } => Expr::Product {
            elems: elems.iter().map(|e| rewrite_op_dispatch(e, op_dispatch)).collect(),
            span: *span,
        },
        Expr::Send { target, msg, span } => Expr::Send {
            target: Box::new(rewrite_op_dispatch(target, op_dispatch)),
            msg: Box::new(rewrite_op_dispatch(msg, op_dispatch)),
            span: *span,
        },
        // Leaf nodes: no children to rewrite.
        Expr::Int { .. } | Expr::Str { .. } | Expr::Var { .. } | Expr::Upper { .. }
        | Expr::AtName { .. } | Expr::Self_ { .. } | Expr::Unit { .. } => expr.clone(),
    }
}

/// Rewrite a DoStmt's expressions (used by `rewrite_op_dispatch`).
fn rewrite_do_stmt(stmt: &wag_syntax::DoStmt, op_dispatch: &HashMap<Span, String>) -> wag_syntax::DoStmt {
    match stmt {
        wag_syntax::DoStmt::Bind { name, expr } => wag_syntax::DoStmt::Bind {
            name: name.clone(),
            expr: rewrite_op_dispatch(expr, op_dispatch),
        },
        wag_syntax::DoStmt::Return(e) => wag_syntax::DoStmt::Return(rewrite_op_dispatch(e, op_dispatch)),
    }
}

/// Rewrite an UpdateClause's value expression (used by `rewrite_op_dispatch`).
fn rewrite_clause_op(c: &wag_syntax::UpdateClause, op_dispatch: &HashMap<Span, String>) -> wag_syntax::UpdateClause {
    use wag_syntax::UpdateClause::*;
    match c {
        Leaf { path, value, span } => Leaf {
            path: path.clone(),
            value: rewrite_op_dispatch(value, op_dispatch),
            span: *span,
        },
        Group { path, clauses, span } => Group {
            path: path.clone(),
            clauses: clauses.iter().map(|cc| rewrite_clause_op(cc, op_dispatch)).collect(),
            span: *span,
        },
    }
}

/// Lift any remaining inline `Expr::Lambda` nodes into synthetic top-level
/// functions so the VM can execute them (do-notation continuations are kept
/// inline during type checking and only lifted here at load time).
/// Uses closure conversion: captured free vars become extra params.
fn lift_remaining_lambdas(
    expr: &mut Expr,
    counter: &mut usize,
    new_fns: &mut Vec<FnEntry>,
    globals: &std::collections::HashSet<String>,
) -> Expr {
    match expr {
        Expr::Lambda { params, body, span } => {
            // Compute free vars in the body (before lifting nested lambdas).
            let bound: std::collections::HashSet<String> = params.iter().cloned().collect();
            let mut captured: Vec<String> = free_vars_expr(body, &bound, globals);
            // Remove the lambda's own params from captured (they're not free).
            captured.retain(|v| !params.contains(v));
            captured.sort();

            let name = format!("__lambda_{}", *counter);
            *counter += 1;

            // All params: captured vars first, then original lambda params.
            let all_params: Vec<String> = captured.iter().cloned().chain(params.iter().cloned()).collect();
            let n_params = all_params.len();

            // Recursively lift any nested lambdas inside the body.
            let lifted_body = lift_remaining_lambdas(body, counter, new_fns, globals);
            new_fns.push(FnEntry {
                name: name.clone(),
                params: all_params,
                param_tys: vec![Ty::Var(0); n_params],
                body: lifted_body,
                result_ty: Ty::Var(0),
                param_linear: vec![false; n_params],
                recv_moved: false,
            });

            // At the call site: apply the synthetic fn to the captured vars.
            if captured.is_empty() {
                Expr::Var { name, span: *span }
            } else {
                let args: Vec<Expr> = captured
                    .iter()
                    .map(|v| Expr::Var { name: v.clone(), span: *span })
                    .collect();
                Expr::Apply {
                    fun: Box::new(Expr::Var { name, span: *span }),
                    args,
                    span: *span,
                }
            }
        }
        Expr::Apply { fun, args, .. } => {
            let f = lift_remaining_lambdas(fun, counter, new_fns, globals);
            let a: Vec<Expr> = args.iter_mut().map(|a| lift_remaining_lambdas(a, counter, new_fns, globals)).collect();
            Expr::Apply { fun: Box::new(f), args: a, span: expr.span() }
        }
        Expr::BinOp { op, lhs, rhs, span } => {
            let l = lift_remaining_lambdas(lhs, counter, new_fns, globals);
            let r = lift_remaining_lambdas(rhs, counter, new_fns, globals);
            Expr::BinOp { op: *op, lhs: Box::new(l), rhs: Box::new(r), span: *span }
        }
        Expr::Field { base, field, span } => {
            let b = lift_remaining_lambdas(base, counter, new_fns, globals);
            Expr::Field { base: Box::new(b), field: field.clone(), span: *span }
        }
        Expr::RecordUpdate { receiver, clauses, span } => {
            let r = lift_remaining_lambdas(receiver, counter, new_fns, globals);
            let c: Vec<_> = clauses.iter_mut().map(|cl| lift_clause_lambda(cl, counter, new_fns, globals)).collect();
            Expr::RecordUpdate { receiver: Box::new(r), clauses: c, span: *span }
        }
        Expr::SetField { receiver, field_idx, value, span } => {
            let r = lift_remaining_lambdas(receiver, counter, new_fns, globals);
            let v = lift_remaining_lambdas(value, counter, new_fns, globals);
            Expr::SetField { receiver: Box::new(r), field_idx: *field_idx, value: Box::new(v), span: *span }
        }
        Expr::RegionWrite { receiver, path, value, span } => {
            let r = lift_remaining_lambdas(receiver, counter, new_fns, globals);
            let v = lift_remaining_lambdas(value, counter, new_fns, globals);
            Expr::RegionWrite { receiver: Box::new(r), path: path.clone(), value: Box::new(v), span: *span }
        }
        Expr::Borrow { inner, span } => {
            let i = lift_remaining_lambdas(inner, counter, new_fns, globals);
            Expr::Borrow { inner: Box::new(i), span: *span }
        }
        Expr::Bind { name, value, body, span } => {
            let v = lift_remaining_lambdas(value, counter, new_fns, globals);
            let b = lift_remaining_lambdas(body, counter, new_fns, globals);
            Expr::Bind { name: name.clone(), value: Box::new(v), body: Box::new(b), span: *span }
        }
        Expr::Seq { first, second, span } => {
            let f = lift_remaining_lambdas(first, counter, new_fns, globals);
            let s = lift_remaining_lambdas(second, counter, new_fns, globals);
            Expr::Seq { first: Box::new(f), second: Box::new(s), span: *span }
        }
        Expr::If { arms, span } => {
            let mut a = Vec::with_capacity(arms.len());
            for arm in arms.iter_mut() {
                let cond = if let Some(ref mut g) = arm.condition {
                    Some(lift_remaining_lambdas(g, counter, new_fns, globals))
                } else {
                    None
                };
                let body = lift_remaining_lambdas(&mut arm.body, counter, new_fns, globals);
                a.push(IfArm { condition: cond, body });
            }
            Expr::If { arms: a, span: *span }
        }
        Expr::Match { scrutinee, arms, span } => {
            let s = lift_remaining_lambdas(scrutinee, counter, new_fns, globals);
            let a: Vec<MatchArm> = arms.iter_mut().map(|arm| MatchArm {
                pattern: arm.pattern.clone(),
                body: lift_remaining_lambdas(&mut arm.body, counter, new_fns, globals),
            }).collect();
            Expr::Match { scrutinee: Box::new(s), arms: a, span: *span }
        }
        Expr::Do { stmts, span } => {
            let st: Vec<_> = stmts.iter_mut().map(|s| lift_do_stmt_lambda(s, counter, new_fns, globals)).collect();
            Expr::Do { stmts: st, span: *span }
        }
        Expr::ListLit { elems, span } => {
            let e: Vec<Expr> = elems.iter_mut().map(|e| lift_remaining_lambdas(e, counter, new_fns, globals)).collect();
            Expr::ListLit { elems: e, span: *span }
        }
        Expr::Comprehension { generators, accs, guards, result, span } => {
            let g: Vec<_> = generators.iter_mut().map(|gen| wag_syntax::Generator {
                vars: gen.vars.clone(),
                sources: gen.sources.iter_mut().map(|s| lift_remaining_lambdas(s, counter, new_fns, globals)).collect(),
                span: gen.span,
            }).collect();
            let ac: Vec<_> = accs.iter_mut().map(|a| wag_syntax::AccInit {
                name: a.name.clone(),
                init: lift_remaining_lambdas(&mut a.init, counter, new_fns, globals),
                span: a.span,
            }).collect();
            let gd: Vec<Expr> = guards.iter_mut().map(|g| lift_remaining_lambdas(g, counter, new_fns, globals)).collect();
            let r = lift_remaining_lambdas(result, counter, new_fns, globals);
            Expr::Comprehension { generators: g, accs: ac, guards: gd, result: Box::new(r), span: *span }
        }
        Expr::Product { elems, span } => {
            let e: Vec<Expr> = elems.iter_mut().map(|e| lift_remaining_lambdas(e, counter, new_fns, globals)).collect();
            Expr::Product { elems: e, span: *span }
        }
        Expr::Send { target, msg, span } => {
            let t = lift_remaining_lambdas(target, counter, new_fns, globals);
            let m = lift_remaining_lambdas(msg, counter, new_fns, globals);
            Expr::Send { target: Box::new(t), msg: Box::new(m), span: *span }
        }
        // Leaf nodes.
        _ => expr.clone(),
    }
}

fn lift_clause_lambda(
    c: &mut wag_syntax::UpdateClause,
    counter: &mut usize,
    new_fns: &mut Vec<FnEntry>,
    globals: &std::collections::HashSet<String>,
) -> wag_syntax::UpdateClause {
    use wag_syntax::UpdateClause::*;
    match c {
        Leaf { path, value, span } => Leaf {
            path: path.clone(),
            value: lift_remaining_lambdas(value, counter, new_fns, globals),
            span: *span,
        },
        Group { path, clauses, span } => Group {
            path: path.clone(),
            clauses: clauses.iter_mut().map(|cc| lift_clause_lambda(cc, counter, new_fns, globals)).collect(),
            span: *span,
        },
    }
}

fn lift_do_stmt_lambda(
    stmt: &mut wag_syntax::DoStmt,
    counter: &mut usize,
    new_fns: &mut Vec<FnEntry>,
    globals: &std::collections::HashSet<String>,
) -> wag_syntax::DoStmt {
    match stmt {
        wag_syntax::DoStmt::Bind { name, expr } => wag_syntax::DoStmt::Bind {
            name: name.clone(),
            expr: lift_remaining_lambdas(expr, counter, new_fns, globals),
        },
        wag_syntax::DoStmt::Return(e) => wag_syntax::DoStmt::Return(lift_remaining_lambdas(e, counter, new_fns, globals)),
    }
}

/// Collect free variable names in an expression (vars not bound by enclosing
/// lambdas, binds, or match patterns). Used for closure conversion.
fn free_vars_expr(expr: &Expr, bound: &std::collections::HashSet<String>, globals: &std::collections::HashSet<String>) -> Vec<String> {
    let mut vars = std::collections::HashSet::new();
    free_vars_expr_into(expr, bound, globals, &mut vars);
    vars.into_iter().collect()
}

fn free_vars_expr_into(expr: &Expr, bound: &std::collections::HashSet<String>, globals: &std::collections::HashSet<String>, out: &mut std::collections::HashSet<String>) {
    match expr {
        Expr::Var { name, .. } => {
            if !bound.contains(name) && !globals.contains(name) && !name.starts_with("__") {
                out.insert(name.clone());
            }
        }
        Expr::Lambda { params, body, .. } => {
            let mut b = bound.clone();
            b.extend(params.iter().cloned());
            free_vars_expr_into(body, &b, globals, out);
        }
        Expr::Bind { name, value, body, .. } => {
            free_vars_expr_into(value, bound, globals, out);
            let mut b = bound.clone();
            b.insert(name.clone());
            free_vars_expr_into(body, &b, globals, out);
        }
        Expr::Apply { fun, args, .. } => {
            free_vars_expr_into(fun, bound, globals, out);
            for a in args {
                free_vars_expr_into(a, bound, globals, out);
            }
        }
        Expr::BinOp { lhs, rhs, .. } => {
            free_vars_expr_into(lhs, bound, globals, out);
            free_vars_expr_into(rhs, bound, globals, out);
        }
        Expr::Field { base, .. } => {
            free_vars_expr_into(base, bound, globals, out);
        }
        Expr::RecordUpdate { receiver, clauses, .. } => {
            free_vars_expr_into(receiver, bound, globals, out);
            for c in clauses {
                match c {
                    wag_syntax::UpdateClause::Leaf { value, .. } => free_vars_expr_into(value, bound, globals, out),
                    wag_syntax::UpdateClause::Group { clauses: inner, .. } => {
                        for cc in inner {
                            if let wag_syntax::UpdateClause::Leaf { value, .. } = cc {
                                free_vars_expr_into(value, bound, globals, out);
                            }
                        }
                    }
                }
            }
        }
        Expr::SetField { receiver, value, .. } => {
            free_vars_expr_into(receiver, bound, globals, out);
            free_vars_expr_into(value, bound, globals, out);
        }
        Expr::RegionWrite { receiver, value, .. } => {
            free_vars_expr_into(receiver, bound, globals, out);
            free_vars_expr_into(value, bound, globals, out);
        }
        Expr::Borrow { inner, .. } => {
            free_vars_expr_into(inner, bound, globals, out);
        }
        Expr::Seq { first, second, .. } => {
            free_vars_expr_into(first, bound, globals, out);
            free_vars_expr_into(second, bound, globals, out);
        }
        Expr::If { arms, .. } => {
            for arm in arms {
                if let Some(ref cond) = arm.condition {
                    free_vars_expr_into(cond, bound, globals, out);
                }
                free_vars_expr_into(&arm.body, bound, globals, out);
            }
        }
        Expr::Match { scrutinee, arms, .. } => {
            free_vars_expr_into(scrutinee, bound, globals, out);
            for arm in arms {
                let mut b = bound.clone();
                collect_pattern_vars(&arm.pattern, &mut b);
                free_vars_expr_into(&arm.body, &b, globals, out);
            }
        }
        Expr::Do { stmts, .. } => {
            let mut b = bound.clone();
            for s in stmts {
                match s {
                    wag_syntax::DoStmt::Bind { name, expr } => {
                        free_vars_expr_into(expr, &b, globals, out);
                        b.insert(name.clone());
                    }
                    wag_syntax::DoStmt::Return(e) => {
                        free_vars_expr_into(e, &b, globals, out);
                    }
                }
            }
        }
        Expr::ListLit { elems, .. } => {
            for e in elems {
                free_vars_expr_into(e, bound, globals, out);
            }
        }
        Expr::Comprehension { generators, accs, guards, result, .. } => {
            let mut b = bound.clone();
            for g in generators {
                for v in &g.vars {
                    b.insert(v.clone());
                }
                for s in &g.sources {
                    free_vars_expr_into(s, &b, globals, out);
                }
            }
            for a in accs {
                b.insert(a.name.clone());
                free_vars_expr_into(&a.init, &b, globals, out);
            }
            for g in guards {
                free_vars_expr_into(g, &b, globals, out);
            }
            free_vars_expr_into(result, &b, globals, out);
        }
        Expr::Product { elems, .. } => {
            for e in elems {
                free_vars_expr_into(e, bound, globals, out);
            }
        }
        Expr::Send { target, msg, .. } => {
            free_vars_expr_into(target, bound, globals, out);
            free_vars_expr_into(msg, bound, globals, out);
        }
        // Leaf nodes: no vars.
        _ => {}
    }
}

fn collect_pattern_vars(pattern: &wag_syntax::Pat, out: &mut std::collections::HashSet<String>) {
    match pattern {
        wag_syntax::Pat::Var { name, .. } => { out.insert(name.clone()); }
        wag_syntax::Pat::Ctor { args, .. } => {
            for a in args { collect_pattern_vars(a, out); }
        }
        wag_syntax::Pat::Product { elems, .. } => {
            for e in elems { collect_pattern_vars(e, out); }
        }
        _ => {}
    }
}

/// Build load-time tables from a parsed + typed program.
pub fn load(program: &Program, typed: &TypedProgram) -> LoadedProgram {
    let mut functions = Vec::new();
    let mut fn_by_name = HashMap::new();
    let mut fn_overloads: HashMap<(String, usize), FnId> = HashMap::new();

    for item in &program.items {
        match item {
            Item::Func(FuncDef {
                name,
                params,
                body,
                ..
            }) => {
                let id = FnId(functions.len() as u32);
                // Primary: first def per name wins (D052).
                fn_by_name.entry(name.clone()).or_insert(id);
                // Overload table: all defs keyed by (name, arity) — D051.
                fn_overloads.insert((name.clone(), params.len()), id);
                let ty = typed.bindings.get(&(name.clone(), params.len())).cloned().unwrap_or_else(|| {
                    panic!(
                        "load invariant: no type binding for `{name}`/arity {} — \
                         upstream type-checker bug",
                        params.len()
                    )
                });
                let (param_tys, result_ty) = match &ty {
                    Ty::Fun(ps, r) => (ps.clone(), (**r).clone()),
                    // §3.1 value/function unity: 0-param bindings store the bare
                    // value type, not a Fun wrapper.
                    other if params.is_empty() => (vec![], other.clone()),
                    other => panic!(
                        "load invariant: binding for `{name}`/arity {} is not a function type (got `{other}`)",
                        params.len()
                    ),
                };
                let param_linear: Vec<bool> = param_tys.iter().map(ty_is_linear).collect();
                functions.push(FnEntry {
                    name: name.clone(),
                    params: params.clone(),
                    param_tys,
                    param_linear,
                    body: body.clone(),
                    result_ty,
                    recv_moved: false,
                });

            }
            Item::Actor(ActorDef {
                name,
                params,
                body,
                ..
            }) => {
                // Behavior is a normal function table entry named `@Name`.
                // TypedProgram stores the behavior fun type under
                // `("@name", params.len())` — actors occupy the `@`-prefixed namespace (§2).
                let fname = format!("@{name}");
                let id = FnId(functions.len() as u32);
                // Register ONLY under the `@`-prefixed name. Actors live in a separate
                // namespace from functions (§2: `@foo` and `foo` are distinct atoms), so we
                // must NOT alias the bare name — doing so would shadow any user function
                // named `foo` with this actor's behavior. Behaviors are dispatched via
                // ActorEntry.behavior (a FnId), never by a bare-name fn_by_name lookup.
                fn_by_name.entry(fname.clone()).or_insert(id);
                fn_overloads.insert((fname.clone(), params.len()), id);
                let (param_tys, result_ty) = match typed
                    .bindings
                    .get(&(format!("@{name}"), params.len()))
                {
                    Some(Ty::Fun(ps, r)) => (ps.clone(), (**r).clone()),
                    _ => (vec![Ty::unit(); params.len()], Ty::option(Ty::unit())),
                };
                let param_linear: Vec<bool> = param_tys.iter().map(ty_is_linear).collect();
                functions.push(FnEntry {
                    name: fname,
                    params: params.clone(),
                    param_tys,
                    param_linear,
                    body: body.clone(),
                    result_ty,
                    recv_moved: false,
                });
            }
            _ => {}
        }
    }

    let mut ctors = Vec::new();
    let mut ctor_by_name = HashMap::new();

    // Built-ins first (stable ids). One-off inline table.
    // String/Char/Some/None/VBuf are always available so that string literals
    // and `assert` work even in programs that do not import the prelude.
    // When the prelude IS imported, the constrained-body loop links the decider
    // to the existing entry (overwriting `constrained_body: None`).
    let builtins: &[(&str, &str, usize, CtorKind)] = &[
        ("True", "Bool", 0, CtorKind::True),
        ("False", "Bool", 0, CtorKind::False),
        ("Unit", "Unit", 0, CtorKind::Unit),
        ("Pair", "Pair", 2, CtorKind::Pair),
        ("Triple", "Triple", 3, CtorKind::Triple),
        ("Some", "Option", 1, CtorKind::User),
        ("None", "Option", 0, CtorKind::User),
        ("Char", "Char", 1, CtorKind::User),
        ("String", "String", 1, CtorKind::User),
        ("VBuf", "Vec", 3, CtorKind::User),
    ];

    for (name, tname, arity, kind) in builtins {
        let id = ConstructorId(ctors.len() as u32);
        ctor_by_name.insert((*name).to_string(), id);
        ctors.push(CtorEntry {
            name: (*name).to_string(),
            id,
            type_name: (*tname).to_string(),
            arity: *arity,
            pointer_free: true, // specialized at construct time for Option/Pair
            kind: *kind,
            field_is_ref: vec![],
            resident: false,
            field_names: vec![],
            constrained_body: None,
        });
    }

    // User constructors from typed table.
    for (name, info) in &typed.constructors {
        // A1b: VM metadata is derived once in wag-types — read the tables,
        // never declaration-space field types.
        let pf = typed.pointer_free_types.get(&info.type_name).copied().unwrap_or(false);
        let field_is_ref = typed
            .ctor_meta
            .get(name)
            .map(|m| m.field_is_ref.clone())
            .unwrap_or_default();
        let field_names: Vec<String> = info
            .fields
            .iter()
            .map(|f| f.label.clone().unwrap_or_default())
            .collect();
        if let Some(&id) = ctor_by_name.get(name) {
            // Already registered (builtin): refresh metadata so field access
            // and pointer-free tracking see the typed-table values.
            let idx = id.0 as usize;
            ctors[idx].pointer_free = pf;
            ctors[idx].field_is_ref = field_is_ref;
            ctors[idx].field_names = field_names;
            ctors[idx].resident = typed.residency.get(&info.type_name) == Some(&true);
            continue;
        }
        let id = ConstructorId(ctors.len() as u32);
        ctor_by_name.insert(name.clone(), id);
        ctors.push(CtorEntry {
            name: name.clone(),
            id,
            type_name: info.type_name.clone(),
            arity: info.fields.len(),
            pointer_free: pf,
            kind: CtorKind::User,
            field_is_ref,
            resident: typed.residency.get(&info.type_name) == Some(&true),
            field_names,
            constrained_body: None,
        });
    }

    // Constrained datatypes: register the decider body as a function and link
    // it to the ctor entry. The VM evaluates the body (Bool) before construction.
    for item in &program.items {
        if let Item::ConstrainedDatatype(cd) = item {
            let body_fn_id = FnId(functions.len() as u32);
            let (param_tys, _result_ty) = match &cd.sig {
                wag_syntax::Type::Func(params, _ret) => {
                    let pty: Vec<Ty> = params.iter().filter_map(|p| wag_types::surface_to_ty(p).ok()).collect();
                    (pty, Ty::bool())
                }
                _ => (vec![Ty::int()], Ty::bool()),
            };
            let param_linear: Vec<bool> = param_tys.iter().map(ty_is_linear).collect();
            functions.push(FnEntry {
                name: format!("__constrained_{}", cd.name),
                params: vec![cd.param.clone()],
                param_tys,
                param_linear,
                body: cd.body.clone(),
                result_ty: Ty::bool(),
                recv_moved: false,
            });
            // Link the body to the ctor entry.
            if let Some(ctor_id) = ctor_by_name.get(&cd.name) {
                let idx = ctor_id.0 as usize;
                ctors[idx].constrained_body = Some(body_fn_id);
            }
        }
    }

    // Methods (D056/D057): loaded as ordinary fn-table entries named
    // `"TypeName.name"` with an implicit leading `__self__` parameter. Dispatch
    // is a static table lookup keyed by the receiver handle's entry-cell ctor id;
    // a method declared on type T registers under every ctor of T so multi-ctor
    // types dispatch on the runtime entry cell.
    let mut method_fns: HashMap<(ConstructorId, String), FnId> = HashMap::new();
    for item in &program.items {
        if let Item::Method(m) = item {
            let id = FnId(functions.len() as u32);
            let (param_tys, result_ty) = match typed
                .method_tys
                .get(&(m.type_name.clone(), m.name.clone()))
            {
                Some(Ty::Fun(ps, r)) => {
                    // Receiver param type (D061): applied form for parametric types.
                    let recv = match typed.type_params.get(&m.type_name) {
                        // A1b 3c: Bound, not Var — exported tables carry no live
                        // metavars; interp never unifies these.
                        Some(tps) if !tps.is_empty() => Ty::App(
                            m.type_name.clone(),
                            (0..tps.len() as u32).map(Ty::Bound).collect(),
                        ),
                        _ => Ty::Con(m.type_name.clone()),
                    };
                    (
                        std::iter::once(recv).chain(ps.iter().cloned()).collect(),
                        (**r).clone(),
                    )
                },
                _ => (vec![Ty::unit(); m.params.len() + 1], Ty::unit()),
            };
            let mut params = vec!["__self__".to_string()];
            params.extend(m.params.iter().cloned());
            let recv_moved = ty_mentions(&result_ty, &m.type_name);
            let param_linear: Vec<bool> = param_tys.iter().map(ty_is_linear).collect();
            functions.push(FnEntry {
                name: format!("{}.{}", m.type_name, m.name),
                params,
                param_tys,
                param_linear,
                body: rewrite_self(&m.body),
                result_ty,
                recv_moved,
            });
            for c in &ctors {
                if c.type_name == m.type_name {
                    method_fns.insert((c.id, m.name.clone()), id);
                }
            }
        }
    }

    // Typeclass instance methods (D075): materialize as ordinary fn-table entries.
    // Operator-named methods are renamed to synthetic `__op_{Class}_{TypeStr}` by
    // inference (env collisions across instances) and `op_dispatch` rewrites use-sites
    // into applications of them; non-operator methods keep their source name and are
    // called directly. Impls without a typed binding are dead (never dispatched).
    for item in &program.items {
        if let Item::Instance(inst) = item {
            let type_str = wag_syntax::pretty_type(&inst.instance_ty).replace(' ', "");
            for m in &inst.methods {
                let impl_name = if is_op_name(&m.name) {
                    format!("__op_{}_{}", inst.class_name, type_str)
                } else if inst.class_name == "Monad" {
                    format!("__{}_Monad_{}", m.name, type_str)
                } else {
                    m.name.clone()
                };
                let ty = match typed.bindings.get(&(impl_name.clone(), m.params.len())) {
                    Some(t) => t.clone(),
                    None => continue,
                };
                let id = FnId(functions.len() as u32);
                fn_by_name.entry(impl_name.clone()).or_insert(id);
                fn_overloads.insert((impl_name.clone(), m.params.len()), id);
                let (param_tys, result_ty) = match &ty {
                    Ty::Fun(ps, r) => (ps.clone(), (**r).clone()),
                    other if m.params.is_empty() => (vec![], other.clone()),
                    _ => panic!(
                        "load invariant: binding for `{impl_name}`/arity {} is not a function type",
                        m.params.len()
                    ),
                };
                let param_linear: Vec<bool> = param_tys.iter().map(ty_is_linear).collect();
                functions.push(FnEntry {
                    name: impl_name,
                    params: m.params.clone(),
                    param_tys,
                    param_linear,
                    body: m.body.clone(),
                    result_ty,
                    recv_moved: false,
                });
            }
        }
    }

    // Rewrite dispatched BinOp use-sites into applications of the overload impl.
    if !typed.op_dispatch.is_empty() {
        for fn_entry in functions.iter_mut() {
            fn_entry.body = rewrite_op_dispatch(&fn_entry.body, &typed.op_dispatch);
        }
    }

    // Lift any remaining inline lambdas (do-notation continuations kept inline
    // during type checking) into synthetic top-level functions.
    let mut lambda_counter = 0usize;
    let mut all_new_fns: Vec<FnEntry> = Vec::new();
    let mut globals: std::collections::HashSet<String> = fn_by_name.keys().cloned().collect();
    // Built-in names resolved by string match in eval_apply_resolved — never
    // capturable as closure args (the VM has no Value representation for them).
    for name in [
        "arrFromList", "arrGet", "arrLen", "arrNew", "arrSet", "arrToList",
        "assert", "bitNot", "bytesToString", "charToCodepoint", "codepointToChar",
        "codepointsToString", "copy", "envGet", "fileClose", "fileOpenRead",
        "fileOpenWrite", "fileRead", "fileWrite", "nowMs", "randRange",
        "stringByteAt", "stringGet", "stringToBytes", "stringToCodepoints",
        "vecEmpty", "vecFromList", "vecGet", "vecLen", "vecPush", "vecToList",
    ] {
        globals.insert(name.to_string());
    }
    for fn_entry in functions.iter_mut() {
        fn_entry.body = lift_remaining_lambdas(&mut fn_entry.body, &mut lambda_counter, &mut all_new_fns, &globals);
    }
    for new_fn in all_new_fns {
        let id = FnId(functions.len() as u32);
        functions.push(new_fn.clone());
        fn_by_name.entry(new_fn.name.clone()).or_insert(id);
        fn_overloads.insert((new_fn.name.clone(), new_fn.params.len()), id);
    }

    // Actor metadata.
    let mut actors = HashMap::new();
    for item in &program.items {
        if let Item::Actor(a) = item {

            let fname = format!("@{}", a.name);
            let behavior = *fn_by_name
                .get(&fname)
                .expect("actor behavior inserted above");

            // Msg type: second parameter of behavior, or from Actor T binding.
            let msg_type = {
                let entry = &functions[behavior.0 as usize];
                entry
                    .param_tys
                    .get(1)
                    .and_then(|t| match t {
                        Ty::Con(n) => Some(n.clone()),
                        Ty::App(n, _) => Some(n.clone()),
                        _ => None,
                    })
                    .or_else(|| {
                        typed.actors.get(&a.name).and_then(|t| match t {
                            Ty::App(n, args) if n == "Actor" => {
                                args.first().and_then(|m| match m {
                                    Ty::Con(x) => Some(x.clone()),
                                    Ty::App(x, _) => Some(x.clone()),
                                    _ => None,
                                })
                            }
                            _ => None,
                        })
                    })
            };

            let (start_ctor, down_ctor, end_ctor) = if let Some(ref mt) = msg_type {
                let mut start = None;
                let mut down = None;
                let mut end = None;
                for c in &ctors {
                    if &c.type_name == mt {
                        if c.name == "Start" {
                            start = Some(c.id);
                        }
                        if c.name == "Down" {
                            down = Some(c.id);
                        }
                        if c.name == "End" {
                            end = Some(c.id);
                        }
                    }
                }
                (start, down, end)
            } else {
                (None, None, None)
            };

            // For `@main`, resolve the desugared `main_boot`/`main_end` function
            // ids (§5.4). `main_boot` takes 1 param (argv); `main_end` takes 2
            // (state, code). Both are ordinary functions keyed by (name, arity).
            let boot = fn_overloads.get(&("main_boot".to_string(), 1)).copied();
            let end = fn_overloads.get(&("main_end".to_string(), 2)).copied();

            actors.insert(
                a.name.clone(),
                ActorEntry {
                    name: a.name.clone(),
                    behavior,
                    msg_type,
                    start_ctor,
                    #[allow(deprecated)]
                    down_ctor,
                    boot,
                    end,
                    end_ctor,
                },
            );
        }
    }

    let prog = LoadedProgram {
        functions,
        fn_by_name,
        fn_overloads,
        ctors,
        ctor_by_name,
        method_fns,
        actors,
        cell_reuse: false,
    };
    #[cfg(debug_assertions)]
    prog.assert_consistent();
    prog
}



#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use crate::value::{CTOR_ARRAY, ConstructorId, Value};

    /// Minimal program with the ctors the Char/String helpers need.
    fn test_program() -> LoadedProgram {
        let ctors = vec![
            CtorEntry { name: "Some".into(), id: ConstructorId(0), type_name: "Option".into(), arity: 1, pointer_free: false, kind: CtorKind::User, field_is_ref: vec![], resident: false, field_names: vec![], constrained_body: None },
            CtorEntry { name: "None".into(), id: ConstructorId(1), type_name: "Option".into(), arity: 0, pointer_free: true, kind: CtorKind::User, field_is_ref: vec![], resident: false, field_names: vec![], constrained_body: None },
            CtorEntry { name: "Char".into(), id: ConstructorId(2), type_name: "Char".into(), arity: 1, pointer_free: true, kind: CtorKind::User, field_is_ref: vec![], resident: false, field_names: vec![], constrained_body: None },
            CtorEntry { name: "String".into(), id: ConstructorId(3), type_name: "String".into(), arity: 1, pointer_free: false, kind: CtorKind::User, field_is_ref: vec![], resident: false, field_names: vec![], constrained_body: None },
            CtorEntry { name: "VBuf".into(), id: ConstructorId(4), type_name: "Vec".into(), arity: 3, pointer_free: false, kind: CtorKind::User, field_is_ref: vec![], resident: false, field_names: vec![], constrained_body: None },
        ];
        let ctor_by_name: HashMap<String, ConstructorId> = ctors.iter().map(|c| (c.name.clone(), c.id)).collect();
        LoadedProgram {
            functions: vec![],
            fn_by_name: HashMap::new(),
            fn_overloads: HashMap::new(),
            ctors,
            ctor_by_name,
            method_fns: HashMap::new(),
            actors: HashMap::new(),
            cell_reuse: false,
        }
    }

    // -- char_ctor_id / str_ctor_id ------------------------------------------

    #[test]
    fn ctor_id_lookups() {
        let p = test_program();
        assert_eq!(p.char_ctor_id(), Some(ConstructorId(2)));
        assert_eq!(p.str_ctor_id(), Some(ConstructorId(3)));
        assert_eq!(p.opt_some_id(), ConstructorId(0));
        assert_eq!(p.opt_none_id(), ConstructorId(1));
    }

    // -- make_char -----------------------------------------------------------

    #[test]
    fn make_char_produces_data1() {
        let p = test_program();
        let v = p.make_char(104);
        assert!(matches!(v, Value::Data1(c, f) if c == ConstructorId(2) && *f == Value::Int(104)));
    }

    #[test]
    fn make_char_max_codepoint() {
        let p = test_program();
        let v = p.make_char(0x10FFFF);
        assert!(matches!(v, Value::Data1(_, f) if *f == Value::Int(0x10FFFF)));
    }

    // -- make_string / value_to_string roundtrip ------------------------------

    #[test]
    fn string_roundtrip_ascii() {
        let p = test_program();
        let v = p.make_string("hello");
        assert_eq!(p.value_to_string(&v), Some("hello".to_string()));
    }

    #[test]
    fn string_roundtrip_multibyte_utf8() {
        let p = test_program();
        let v = p.make_string("hé");
        assert_eq!(p.value_to_string(&v), Some("hé".to_string()));
    }

    #[test]
    fn string_roundtrip_empty() {
        let p = test_program();
        let v = p.make_string("");
        assert_eq!(p.value_to_string(&v), Some(String::new()));
    }

    #[test]
    fn string_roundtrip_emoji() {
        let p = test_program();
        let v = p.make_string("a\u{1F600}b");
        assert_eq!(p.value_to_string(&v), Some("a\u{1F600}b".to_string()));
    }

    // -- value_to_string on non-String values --------------------------------

    #[test]
    fn value_to_string_rejects_int() {
        let p = test_program();
        assert_eq!(p.value_to_string(&Value::Int(42)), None);
    }

    #[test]
    fn value_to_string_rejects_char() {
        let p = test_program();
        let v = p.make_char(104);
        assert_eq!(p.value_to_string(&v), None);
    }

    #[test]
    fn value_to_string_rejects_bool() {
        let p = test_program();
        assert_eq!(p.value_to_string(&Value::Bool(true)), None);
    }

    #[test]
    fn value_to_string_rejects_unit() {
        let p = test_program();
        assert_eq!(p.value_to_string(&Value::Unit), None);
    }

    // -- make_string structure ------------------------------------------------

    #[test]
    fn make_string_is_data1_wrapping_vbuf() {
        let p = test_program();
        let v = p.make_string("ab");
        // Outer: Data1(String_ctor, VBuf)
        let Value::Data1(str_id, inner) = &v else {
            panic!("expected Data1, got {:?}", v.brief_desc());
        };
        assert_eq!(*str_id, ConstructorId(3));
        // Inner: Data(VBuf, [Array, len, cap])
        let Value::Data(vbuf_id, fields) = inner.as_ref() else {
            panic!("expected VBuf Data, got {:?}", inner.brief_desc());
        };
        assert_eq!(*vbuf_id, ConstructorId(4));
        assert_eq!(fields.len(), 3);
        // Array of 2 Some(Int) slots
        let Value::Data(ctor, slots) = &fields[0] else {
            panic!("expected Array, got {:?}", fields[0].brief_desc());
        };
        assert_eq!(*ctor, CTOR_ARRAY);
        assert_eq!(slots.len(), 2);
        // len and cap are both 2
        assert_eq!(fields[1], Value::Int(2));
        assert_eq!(fields[2], Value::Int(2));
    }
}
