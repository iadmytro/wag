//! Dispatch + tail/value evaluation (owned model, D081; §4.6.3–§4.6.4).

use wag_syntax::{Expr, MatchArm};
use crate::control::Control;
use crate::error::VmError;
use crate::value::{ConstructorId, FnId, Value, CTOR_ARRAY, CTOR_PAIR2, CTOR_PAIR3, CTOR_PRODUCT};

use super::apply::{ArityResult, Callee};
use super::engine::{Engine, Env, STEP_LIMIT};
use super::util::eval_binop;

impl<'p> Engine<'p> {

    pub(super) fn dispatch(&mut self, fn_id: FnId, args: Vec<Value>) -> Result<Control, VmError> {
        let mut env = Env::new();
        // Dev-mode step budget (CLAUDE.md): one step per function-body eval.
        self.steps += 1;
        if self.steps > STEP_LIMIT {
            return Err(VmError::StepLimit);
        }
        let entry = self.program.get_fn(fn_id);
        self.dbg_trace(&format!(
            "dispatch {}({} args, {} params)",
            entry.name, args.len(), entry.params.len()
        ));
        if args.len() > entry.params.len() {
            return Err(VmError::Panic(format!(
                "too many args calling `{}`: expected {}, got {}",
                entry.name, entry.params.len(), args.len()
            )));
        }
        if args.len() < entry.params.len() {
            // Partial application → Closure value (mirrors check_arity).
            self.dbg_trace(&format!("  → partial apply {} → Closure", entry.name));
            return Ok(Control::Return(Value::Closure(fn_id, args)));
        }

        // Fresh env per frame. Borrows are erased at runtime — a `&T` param is
        // just a value copy; no ownership tracking needed (§7 rules 10–11).
        for (p, v) in entry.params.iter().zip(args.into_iter()) {
            env.insert(p.clone(), v);
        }

        let saved_fn = self.current_fn.take();
        self.current_fn = Some(entry.name.clone());
        let result = self.eval_tail(&entry.body, &mut env);
        self.current_fn = saved_fn;
        result
    }

    /// Create a panic error with current function context.
    pub(super) fn panic_ctx(&self, msg: &str) -> VmError {
        match &self.current_fn {
            Some(fn_name) => VmError::Panic(format!("{msg} (in `{fn_name})")),
            None => VmError::Panic(msg.to_string()),
        }
    }

    /// Evaluate in tail position — may produce `TailCall`.
    pub(super) fn eval_tail(
        &mut self,
        expr: &Expr,
        env: &mut Env,
    ) -> Result<Control, VmError> {
        match expr {
            // Tail application of a known top-level function.
            Expr::Apply { fun, args, .. } => {
                let callee = self.resolve_callee(fun, env)?;
                self.dbg_trace(&format!("  tail-apply: fun={fun:?} callee={callee:?} nargs={}", args.len()));
                if let Callee::Fn(mut target) = callee {
                    // Arity overloading disambiguation (D051).
                    if let Expr::Var { name, .. } = &**fun {
                        if !env.contains_key(name) && self.program.get_fn(target).params.len() != args.len() {
                            if let Some(alt) = self.program.fn_id_arity(name, args.len()) {
                                target = alt;
                            }
                        }
                    }
                    // Build arg vector: closure pre_args + evaluated args.
                    // Tail call = frame ending, so Vars are moved out of env —
                    // but only if they don't appear in other args (which would
                    // need them during evaluation). Mirrors value-position logic.
                    let mut argv = self.closure_pre_args(fun, env, true);
                    argv.reserve(args.len());
                    for (i, a) in args.iter().enumerate() {
                        if let Expr::Var { name, .. } = a {
                            let appears_elsewhere = args
                                .iter()
                                .enumerate()
                                .filter(|(j, _)| *j != i)
                                .any(|(_, other)| var_appears_in(name, other));
                            if !appears_elsewhere {
                                if let Some(v) = env.remove(name.as_str()) {
                                    argv.push(v);
                                    continue;
                                }
                            }
                        }
                        argv.push(self.eval_value(a, env)?);
                    }
                    let tname = self.program.get_fn(target).name.clone();
                    match self.check_arity(target, argv)? {
                        ArityResult::Full(full_args) => {
                            self.dbg_trace(&format!("  tail-call {} ({} args)", tname, full_args.len()));
                            return Ok(Control::TailCall { target, args: full_args });
                        }
                        ArityResult::Partial(closure_value) => {
                            self.dbg_trace(&format!("  tail-partial {} → Closure", tname));
                            return Ok(Control::Return(closure_value));
                        }
                    }
                }
                // Constructor, builtin, or other — not a tail call target.
                let v = self.eval_apply_resolved(fun, args, env)?;
                Ok(Control::Return(v))
            }
            Expr::Bind { name, value, body, .. } => {
                let v = self.eval_value(value, env)?;
                env.insert(name.clone(), v);
                self.eval_tail(body, env)
            }
            Expr::Seq { first, second, .. } => {
                let _ = self.eval_value(first, env)?;
                self.eval_tail(second, env)
            }
            Expr::If { arms, .. } => {
                for arm in arms {
                    let take = match &arm.condition {
                        None => true,
                        Some(c) => match self.eval_value(c, env)? {
                            Value::Bool(b) => b,
                            _ => return Err(VmError::Panic("if condition is not Bool".into())),
                        },
                    };
                    if take {
                        self.dbg_trace(&format!("  if-arm taken, body={:?}", arm.body));
                        return self.eval_tail(&arm.body, env);
                    }
                }
                Err(self.panic_ctx("if: no arm matched"))
            }
            Expr::Match { scrutinee, arms, .. } => {
                // Borrows are erased at runtime (§4.3.3): a `&T` param is just
                // a copy of the value — matching peeks in place and never consumes.
                let (scrutinee, is_borrow) = match scrutinee.as_ref() {
                    Expr::Borrow { inner, .. } => (inner.as_ref(), true),
                    s => (s, false),
                };
                if !is_borrow {
                    if let Expr::Var { name, .. } = scrutinee {
                        // Fast path: var not reused in arm bodies → move out of env.
                        // Data values are expensive to clone (recursive), so we
                        // prefer moving them. Small inline values are cheap either way.
                        let is_data = matches!(env.get(name), Some(Value::Data(..) | Value::Data1(..)));
                        if is_data && !arms.iter().any(|a| {
                            !pat_binds(name, &a.pattern) && var_appears_in(name, &a.body)
                        }) {
                            if let Some(scr_val) = env.remove(name) {
                                let matched = arms.iter().find(|arm| self.peek_match(&arm.pattern, &scr_val));
                                match matched {
                                    Some(arm) => {
                                        let bindings = self.destructure(&arm.pattern, scr_val)?;
                                        for (k, v) in bindings {
                                            env.insert(k, v);
                                        }
                                        return self.eval_tail(&arm.body, env);
                                    }
                                    None => return Err(self.panic_ctx("match: non-exhaustive at runtime")),
                                }
                            }
                        }
                    }
                }
                // Slow path: non-PF scrutinee, or var reused in body → clone.
                let scr = self.eval_value(scrutinee, env)?;
                self.match_arms_tail(scr, arms, env)
            }
            // Non-tail forms: evaluate fully and return.
            other => Ok(Control::Return(self.eval_value(other, env)?)),
        }
    }

    /// Evaluate call arguments. Borrow params are zero-copy (value copy);
    /// linear params are moved out of env; PF values are cloned.
    pub(super) fn eval_call_args(
        &mut self,
        fid: FnId,
        pre_len: usize,
        args: &[Expr],
        env: &mut Env,
    ) -> Result<Vec<Value>, VmError> {
        let entry = self.program.get_fn(fid);
        let mut out = Vec::with_capacity(args.len());
        for (i, a) in args.iter().enumerate() {
            if let Expr::Var { name, .. } = a {
                let is_borrow = matches!(entry.param_tys.get(pre_len + i), Some(wag_types::Ty::Borrow(_)));
                if is_borrow {
                    // Zero-copy borrow: copy the Value (borrows are erased at
                    // runtime, §4.3.3). The value stays in env (not consumed).
                    if let Some(v) = env.get(name.as_str()) {
                        out.push(v.clone());
                        continue;
                    }
                } else {
                    let linear = entry.param_linear.get(pre_len + i).copied().unwrap_or(false);
                    if linear {
                        // Only move if this var doesn't appear in any other arg.
                        let appears_elsewhere = args
                            .iter()
                            .enumerate()
                            .filter(|(j, _)| *j != i)
                            .any(|(_, other)| var_appears_in(name, other));
                        if !appears_elsewhere {
                            if let Some(v) = env.remove(name.as_str()) {
                                out.push(v);
                                continue;
                            }
                        }
                    }
                }
            }
            out.push(self.eval_value(a, env)?);
        }
        Ok(out)
    }

    /// Evaluate to a value (non-tail / nested context).
    pub(super) fn eval_value(&mut self, expr: &Expr, env: &mut Env) -> Result<Value, VmError> {
        match expr {
            Expr::Int { value, .. } => Ok(Value::Int(*value)),
            Expr::Unit { .. } => Ok(Value::Unit),
            Expr::Str { value, .. } => {
                // Owned UTF-8 string (§3.10) — prelude constrained datatype.
                Ok(self.program.make_string(value))
            }
            Expr::Var { name, .. } => {
                if let Some(v) = env.get(name) {
                    // Non-consuming read: clone the Value (Data clones are deep).
                    return Ok(v.clone());
                }
                // D060 invariant: bare atoms in method bodies are desugared to
                // `Self_.field` before inference.
                if env.contains_key("__self__") {
                    return Err(self.panic_ctx(&format!(
                        "desugaring incomplete (D060): bare name `{name}` in method body"
                    )));
                }
                // Top-level computation reference.
                if let Some(id) = self.program.fn_id(name) {
                    if self.program.get_fn(id).params.is_empty() {
                        return Ok(self.call(id, vec![])?);
                    }
                    return Ok(Value::Fn(id));
                }
                // Built-in nullary-ish names used as values.
                match name.as_str() {
                    "copy" => Ok(Value::Fn(FnId(u32::MAX - 1))),
                    "bitNot" => Ok(Value::Fn(FnId(u32::MAX - 2))),
                    "nowMs" => {
                        let ms = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_millis() as i64)
                            .unwrap_or(0);
                        Ok(Value::Int(ms))
                    }
                    "vecEmpty" => {
                        Ok(Value::Data(CTOR_ARRAY, vec![]))
                    }
                    _ => Err(self.panic_ctx(&format!("unbound variable `{name}`"))),
                }
            }
            Expr::Upper { name, .. } => self.construct_nullary(name, env),
            Expr::AtName { name, .. } => {
                Ok(self.with_host(|host| -> Result<Value, VmError> {
                    let id = host.resolve_named(name).ok_or_else(|| VmError::Panic(format!(
                        "@{name} is not a live actor handle (unknown built-in or not installed)"
                    )))?;
                    Ok(Value::Actor(host.make_handle(id)))
                })??)
            }
            Expr::Self_ { .. } => {
                let id = self
                    .running_actor
                    .ok_or_else(|| VmError::Panic("`self` outside actor host context".into()))?;
                Ok(self.with_host(|host| -> Result<Value, VmError> { Ok(Value::Actor(host.make_handle(id))) })??)
            }
            Expr::Send { target, msg, .. } => self.eval_send_expr(target, msg, env),
            Expr::Apply { fun, args, .. } => self.eval_apply_resolved(fun, args, env),
            Expr::BinOp { op, lhs, rhs, .. } => {
                let l = self.eval_value(lhs, env)?;
                let r = self.eval_value(rhs, env)?;
                eval_binop(*op, &l, &r)
            }
            Expr::Borrow { inner, .. } => {
                // Borrows are erased at runtime — same bits.
                self.eval_value(inner, env)
            }
            Expr::Bind { name, value, body, .. } => {
                let v = self.eval_value(value, env)?;
                let prev = env.insert(name.clone(), v);
                let r = self.eval_value(body, env)?;
                if let Some(p) = prev {
                    env.insert(name.clone(), p);
                } else {
                    env.remove(name);
                }
                Ok(r)
            }
            Expr::Seq { first, second, .. } => {
                let _ = self.eval_value(first, env)?;
                self.eval_value(second, env)
            }
            Expr::If { arms, .. } => {
                for arm in arms {
                    let take = match &arm.condition {
                        None => true,
                        Some(c) => match self.eval_value(c, env)? {
                            Value::Bool(b) => b,
                            _ => return Err(VmError::Panic("if condition is not Bool".into())),
                        },
                    };
                    if take {
                        return self.eval_value(&arm.body, env);
                    }
                }
                Err(self.panic_ctx("if: no arm matched"))
            }
            Expr::Match { scrutinee, arms, .. } => {
                let (scrutinee, is_borrow) = match scrutinee.as_ref() {
                    Expr::Borrow { inner, .. } => (inner.as_ref(), true),
                    s => (s, false),
                };
                if !is_borrow {
                    if let Expr::Var { name, .. } = scrutinee {
                        // Fast path: var is a Data value (expensive to clone) AND
                        // not reused in arm bodies → move out directly.
                        let is_data = matches!(env.get(name), Some(Value::Data(..) | Value::Data1(..)));
                        if is_data && !arms.iter().any(|a| {
                            !pat_binds(name, &a.pattern) && var_appears_in(name, &a.body)
                        }) {
                            if let Some(scr_val) = env.remove(name) {
                                let matched = arms.iter().find(|arm| self.peek_match(&arm.pattern, &scr_val));
                                match matched {
                                    Some(arm) => {
                                        let bindings = self.destructure(&arm.pattern, scr_val)?;
                                        for (k, v) in bindings {
                                            env.insert(k, v);
                                        }
                                        return Ok(self.eval_value(&arm.body, env)?);
                                    }
                                    None => return Err(self.panic_ctx("match: non-exhaustive at runtime")),
                                }
                            }
                        }
                    }
                }
                // Slow path: non-PF scrutinee, or var reused in body → clone.
                let scr = self.eval_value(scrutinee, env)?;
                Ok(self.match_arms_value(scr, arms, env)?)
            }
            Expr::Field { base, field, span } => {
                // Self-peek (D060): `Self_.f` is a non-consuming read.
                if let Expr::Var { name, .. } = base.as_ref() {
                    if name == "__self__" {
                        if let Some(v) = self.self_field(env, field)? {
                            return Ok(v);
                        }
                    }
                }
                // Nullary method dispatch (D057).
                if let Some(v) = self.try_method_call(base, field, &[], env)? {
                    return Ok(v);
                }
                Err(VmError::Undesugared {
                    what: "field access requires desugaring before execution",
                    span: *span,
                })
            },
            Expr::RecordUpdate { span, .. } => Err(VmError::Undesugared {
                what: "record update requires desugaring before execution",
                span: *span,
            }),
            Expr::SetField { receiver, field_idx, value, .. } => {
                let new_val = self.eval_value(value, env)?;
                // Self_ receiver (method body): consume __self__, rebuild, write back.
                if matches!(receiver.as_ref(), Expr::Self_ { .. }) {
                    let Some(recv_val) = env.remove("__self__") else {
                        return Err(self.panic_ctx("set-field: `__self__` is not bound"));
                    };
                    let rebuilt = self.set_field_in_value(recv_val, *field_idx, new_val)?;
                    env.insert("__self__".to_string(), rebuilt);
                    return Ok(Value::Unit);
                }
                // Var receiver: consume the variable, rebuild, return updated value.
                if let Expr::Var { name, .. } = &**receiver {
                    if let Some(recv_val) = env.remove(name) {
                        return Ok(self.set_field_in_value(recv_val, *field_idx, new_val)?);
                    }
                }
                // Fallback: evaluate receiver as an expression (non-env value).
                let recv_val = self.eval_value(receiver, env)?;
                Ok(self.set_field_in_value(recv_val, *field_idx, new_val)?)
            }
            Expr::RegionWrite { receiver, path, value, .. } => {
                let val = self.eval_value(value, env)?;
                let name = match receiver.as_ref() {
                    Expr::Self_ { .. } => "__self__",
                    Expr::Var { name, .. } => name,
                    _ => return Err(self.panic_ctx("region write requires a variable or self receiver")),
                };
                self.write_region_path(env, name, path, val)
            },
            Expr::Product { elems, .. } => {
                if elems.len() > 7 {
                    return Err(self.panic_ctx(&format!(
                        "tuple arity {} exceeds limit of 7", elems.len()
                    )));
                }
                let mut vals = Vec::with_capacity(elems.len());
                for e in elems {
                    vals.push(self.eval_value(e, env)?);
                }
                // Owned product value.
                let ctor = match vals.len() {
                    2 => CTOR_PAIR2,
                    3 => CTOR_PAIR3,
                    _ => CTOR_PRODUCT,
                };
                Ok(Value::Data(ctor, vals))
            }
            Expr::ListLit { elems, .. } => {
                let mut vals = Vec::with_capacity(elems.len());
                for e in elems {
                    vals.push(self.eval_value(e, env)?);
                }
                self.build_list_from_elems(vals)
            }
            other => Err(self.panic_ctx(&format!(
                "unhandled expr variant at runtime: {other:?}"
            ))),
        }
    }

    /// Match an already-materialized scrutinee against arms (tail position).
    fn match_arms_tail(
        &mut self,
        scr: Value,
        arms: &[MatchArm],
        env: &mut Env,
    ) -> Result<Control, VmError> {
        for arm in arms {
            if !self.peek_match(&arm.pattern, &scr) {
                continue;
            }
            let bindings = self.destructure(&arm.pattern, scr)?;
            for (k, v) in bindings {
                env.insert(k, v);
            }
            self.dbg_trace(&format!("  match-arm taken, body={:?}", arm.body));
            return self.eval_tail(&arm.body, env);
        }
        Err(self.panic_ctx("match: non-exhaustive at runtime"))
    }

    /// Match an already-materialized scrutinee against arms (value position).
    fn match_arms_value(
        &mut self,
        scr: Value,
        arms: &[MatchArm],
        env: &mut Env,
    ) -> Result<Value, VmError> {
        for arm in arms {
            if !self.peek_match(&arm.pattern, &scr) {
                continue;
            }
            let bindings = self.destructure(&arm.pattern, scr)?;
            for (k, v) in bindings {
                env.insert(k, v);
            }
            return Ok(self.eval_value(&arm.body, env)?);
        }
        Err(self.panic_ctx("match: non-exhaustive at runtime"))
    }

    /// Bare receiver-field read (D057): resolve `name` against `__self__`'s Data value.
    fn self_field(&self, env: &Env, name: &str) -> Result<Option<Value>, VmError> {
        let Some(v) = env.get("__self__") else {
            return Ok(None);
        };
        let (ctor, fields) = match v {
            Value::Data(ctor, fields) => (*ctor, fields.as_slice()),
            Value::Data1(ctor, field) => (*ctor, std::slice::from_ref(field.as_ref())),
            _ => return Ok(None),
        };
        let entry = self.program.get_ctor(ctor);
        let Some(i) = entry.field_names.iter().position(|f| f == name) else {
            return Ok(None);
        };
        let field = fields.get(i).ok_or_else(|| self.panic_ctx("field out of bounds"))?;
        Ok(Some(field.clone()))
    }

    /// Write through a field path: consume+rebuild the Data tree (D081).
    fn write_region_path(
        &mut self,
        env: &mut Env,
        name: &str,
        path: &[usize],
        val: Value,
    ) -> Result<Value, VmError> {
        if path.is_empty() {
            return Err(self.panic_ctx("region write requires a field path"));
        }
        let Some(old) = env.remove(name) else {
            return Err(self.panic_ctx(&format!(
                "region write: `{name}` is not bound"
            )));
        };
        let rebuilt = Self::rebuild_path(old, path, val)
            .map_err(|e| self.panic_ctx(&e.to_string()))?;
        env.insert(name.to_string(), rebuilt);
        Ok(Value::Unit)
    }

    /// Recursively rebuild a Data tree, replacing the field at `path` with `val`.
    ///
    /// LH-handle propagation (D081): in the owned model `head` and `tail` are
    /// independent trees. Writing `tail.next` (path `[1, 1]`) must also extend
    /// the head chain so traversal from `head` sees the new node.
    fn rebuild_path(v: Value, path: &[usize], val: Value) -> Result<Value, String> {
        // Special case: LH handle tail.next write (path = [1, 1]).
        if path == &[1, 1] {
            if let Value::Data(ctor, fields) = &v {
                if fields.len() == 4
                    && matches!(&fields[1], Value::Data(_, tf) if tf.len() == 2)
                {
                    let new_tail = Self::rebuild_path(fields[1].clone(), &[1], val.clone())?;
                    let new_head = Self::extend_terminal(&fields[0], &val)?;
                    // During construction head == start; avoid a second O(n) walk.
                    let new_start = if fields[2] == fields[0] {
                        new_head.clone()
                    } else {
                        Self::extend_terminal(&fields[2], &val)?
                    };
                    let mut new_fields = fields.clone();
                    new_fields[0] = new_head;
                    new_fields[1] = new_tail;
                    new_fields[2] = new_start;
                    return Ok(Value::Data(*ctor, new_fields));
                }
            }
        }

        let (ctor, mut fields) = match v {
            Value::Data(ctor, fields) => (ctor, fields),
            Value::Data1(ctor, field) => (ctor, vec![*field]),
            other => return Err(format!("region write: expected Data, got {other:?}")),
        };
        if path.len() == 1 {
            let idx = path[0];
            if idx >= fields.len() {
                return Err("region write: field out of bounds".into());
            }
            let mut new_fields = fields;
            new_fields[idx] = val;
            if new_fields.len() == 1 {
                Ok(Value::Data1(ctor, Box::new(new_fields.into_iter().next().unwrap())))
            } else {
                Ok(Value::Data(ctor, new_fields))
            }
        } else {
            let idx = path[0];
            if idx >= fields.len() {
                return Err("region write: field out of bounds".into());
            }
            let child = fields.remove(idx);
            let new_child = Self::rebuild_path(child, &path[1..], val)?;
            let mut new_fields = fields;
            new_fields.insert(idx, new_child);
            if new_fields.len() == 1 {
                Ok(Value::Data1(ctor, Box::new(new_fields.into_iter().next().unwrap())))
            } else {
                Ok(Value::Data(ctor, new_fields))
            }
        }
    }

    /// Replace the terminal LEnd in a linked-list chain with `replacement`.
    /// Walks LNodeC nodes to the last one (whose `next` is a nullary Data)
    /// and substitutes `replacement` for that terminal. Iterative to avoid
    /// stack overflow on deep chains.
    fn extend_terminal(node: &Value, replacement: &Value) -> Result<Value, String> {
        // Collect the path of (ctor, value_field) from root to the last node.
        let mut path: Vec<(ConstructorId, Value)> = Vec::new();
        let mut cur = node;
        loop {
            match cur {
                Value::Data(ctor, fields) if fields.len() == 2 => {
                    let next = &fields[1];
                    if matches!(next, Value::Data(_, f) if f.is_empty()) {
                        // Last node found. Rebuild from the bottom up.
                        let mut result = Value::Data(*ctor, vec![fields[0].clone(), replacement.clone()]);
                        for (pc, pv) in path.into_iter().rev() {
                            result = Value::Data(pc, vec![pv, result]);
                        }
                        return Ok(result);
                    }
                    path.push((*ctor, fields[0].clone()));
                    cur = next;
                }
                _ => return Err("extend_terminal: expected LNodeC chain".into()),
            }
        }
    }

    /// Set a field in an owned Data value (§4.3.2). Consume+rebuild (D081).
    fn set_field_in_value(
        &mut self,
        recv_val: Value,
        field_idx: usize,
        new_val: Value,
    ) -> Result<Value, VmError> {
        let (ctor, mut fields) = match recv_val {
            Value::Data(ctor, fields) => (ctor, fields),
            Value::Data1(ctor, field) => (ctor, vec![*field]),
            other => return Err(VmError::Panic(format!(
                "set-field: receiver must be a Data value, got {other:?}"
            ))),
        };
        if field_idx < fields.len() {
            fields[field_idx] = new_val;
            if fields.len() == 1 {
                Ok(Value::Data1(ctor, Box::new(fields.into_iter().next().unwrap())))
            } else {
                Ok(Value::Data(ctor, fields))
            }
        } else {
            Err(VmError::Panic("set-field: field index out of bounds".into()))
        }
    }

}

/// True if the value is small/cheap to clone (no benefit to moving).
pub(super) fn is_pointer_free(v: &Value) -> bool {
    matches!(v, Value::Int(_) | Value::Bool(_) | Value::Unit | Value::Fn(_))
        || matches!(v, Value::Data1(_, inner) if is_pointer_free(inner))
}

/// Does a pattern bind `name` (recursively through ctor/product sub-patterns)?
pub(super) fn pat_binds(name: &str, pat: &wag_syntax::Pat) -> bool {
    use wag_syntax::Pat::*;
    match pat {
        Var { name: n, .. } => n == name,
        Ctor { args, .. } | Product { elems: args, .. } => args.iter().any(|p| pat_binds(name, p)),
        Wildcard { .. } | Literal { .. } => false,
    }
}

/// True if `name` appears as a Var anywhere in the expression tree.
pub(super) fn var_appears_in(name: &str, expr: &Expr) -> bool {
    use wag_syntax::Expr::*;
    match expr {
        Var { name: n, .. } => n == name,
        Int { .. } | Str { .. } | Upper { .. } | AtName { .. } | Self_ { .. } | Unit { .. } => false,
        Apply { fun, args, .. } => var_appears_in(name, fun) || args.iter().any(|a| var_appears_in(name, a)),
        BinOp { lhs, rhs, .. } => var_appears_in(name, lhs) || var_appears_in(name, rhs),
        Field { base, .. } => var_appears_in(name, base),
        RecordUpdate { receiver, clauses, .. } => {
            var_appears_in(name, receiver)
                || clauses.iter().any(|c| match c {
                    wag_syntax::UpdateClause::Leaf { value, .. } => var_appears_in(name, value),
                    wag_syntax::UpdateClause::Group { clauses, .. } => {
                        clauses.iter().any(|sc| matches!(sc,
                            wag_syntax::UpdateClause::Leaf { value, .. } if var_appears_in(name, value)))
                    }
                })
        }
        SetField { receiver, value, .. } => var_appears_in(name, receiver) || var_appears_in(name, value),
        RegionWrite { receiver, value, .. } => var_appears_in(name, receiver) || var_appears_in(name, value),
        Send { target, msg, .. } => var_appears_in(name, target) || var_appears_in(name, msg),
        Borrow { inner, .. } => var_appears_in(name, inner),
        Bind { value, body, .. } => var_appears_in(name, value) || var_appears_in(name, body),
        Seq { first, second, .. } => var_appears_in(name, first) || var_appears_in(name, second),
        If { arms, .. } => arms.iter().any(|a| {
            a.condition.as_ref().map(|c| var_appears_in(name, c)).unwrap_or(false)
                || var_appears_in(name, &a.body)
        }),
        Match { scrutinee, arms, .. } => {
            var_appears_in(name, scrutinee)
                || arms.iter().any(|a| !pat_binds(name, &a.pattern) && var_appears_in(name, &a.body))
        }
        Product { elems, .. } => elems.iter().any(|e| var_appears_in(name, e)),
        ListLit { elems, .. } => elems.iter().any(|e| var_appears_in(name, e)),
        Lambda { body, .. } => var_appears_in(name, body),
        Do { stmts, .. } => stmts.iter().any(|s| match s {
            wag_syntax::DoStmt::Bind { expr, .. } => var_appears_in(name, expr),
            wag_syntax::DoStmt::Return(e) => var_appears_in(name, e),
        }),
        Comprehension { generators, accs, guards, result, .. } => {
            generators.iter().any(|g| g.sources.iter().any(|s| var_appears_in(name, s)))
                || accs.iter().any(|a| var_appears_in(name, &a.init))
                || guards.iter().any(|g| var_appears_in(name, g))
                || var_appears_in(name, result)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use crate::program::{FnEntry, LoadedProgram};
    use crate::value::CTOR_PAIR2;
    use super::*;

    fn one_fn_prog(param_tys: Vec<wag_types::Ty>, param_linear: Vec<bool>) -> LoadedProgram {
        let sp = wag_syntax::Span::new(0, 0);
        let body = Expr::Int { value: 0, span: sp };
        LoadedProgram {
            functions: vec![FnEntry {
                name: "f".into(),
                params: vec!["x".into()],
                param_tys,
                body,
                result_ty: wag_types::Ty::unit(),
                param_linear,
                recv_moved: false,
            }],
            fn_by_name: HashMap::new(),
            fn_overloads: HashMap::new(),
            ctors: vec![],
            ctor_by_name: HashMap::new(),
            method_fns: HashMap::new(),
            actors: HashMap::new(),
            cell_reuse: false,
        }
    }

    /// A linear (owned-data) `Var` arg is moved out of env.
    #[test]
    fn linear_var_arg_is_moved() {
        let sp = wag_syntax::Span::new(0, 0);
        let prog = one_fn_prog(vec![wag_types::Ty::Con("Box".into())], vec![true]);
        let mut e = Engine::new(&prog);
        let val = Value::Data(CTOR_PAIR2, vec![Value::Int(1)]);
        let mut env: Env = HashMap::new();
        env.insert("x".into(), val.clone());

        let args = vec![Expr::Var { name: "x".into(), span: sp }];
        let argv = e.eval_call_args(FnId(0), 0, &args, &mut env).unwrap();

        assert_eq!(argv[0], val);
        assert!(!env.contains_key("x"), "linear arg must be removed from env");
    }

    /// A borrow param (`&T`) creates a zero-copy reference — the binding stays.
    #[test]
    fn borrow_param_is_shared() {
        let sp = wag_syntax::Span::new(0, 0);
        let prog = one_fn_prog(vec![wag_types::Ty::Borrow(Box::new(wag_types::Ty::Con("Box".into())))], vec![false]);
        let mut e = Engine::new(&prog);
        let val = Value::Data(CTOR_PAIR2, vec![Value::Int(1)]);
        let mut env: Env = HashMap::new();
        env.insert("x".into(), val.clone());

        let args = vec![Expr::Var { name: "x".into(), span: sp }];
        let argv = e.eval_call_args(FnId(0), 0, &args, &mut env).unwrap();

        assert_eq!(argv[0], val);
        assert!(env.contains_key("x"), "borrow arg must stay in env");
    }

    /// Pointer-free values are cloned (not moved) — binding kept.
    #[test]
    fn pointer_free_arg_is_not_moved() {
        let sp = wag_syntax::Span::new(0, 0);
        let prog = one_fn_prog(vec![wag_types::Ty::Con("Int".into())], vec![true]);
        let mut e = Engine::new(&prog);
        let mut env: Env = HashMap::new();
        env.insert("x".into(), Value::Int(7));

        let args = vec![Expr::Var { name: "x".into(), span: sp }];
        let argv = e.eval_call_args(FnId(0), 0, &args, &mut env).unwrap();

        assert_eq!(argv[0], Value::Int(7));
        // PF values are moved out for linear params (no benefit to keeping).
    }
}
