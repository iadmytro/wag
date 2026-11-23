//! Call resolution + application (resolve_callee, eval_apply_resolved).

use std::io::{Read, Write};

use wag_syntax::Expr;
use crate::error::VmError;
use crate::value::{ConstructorId, FnId, Value, CTOR_ARRAY};
use super::dispatch::{is_pointer_free, var_appears_in};
use super::engine::{Env, Engine};
use super::util::{file_registry_get, file_registry_insert, file_registry_remove, rand_u64_max};

/// Resolved callee: a real function, a builtin sentinel, or not a function.
#[derive(Debug, Clone, Copy)]
pub(super) enum Callee {
    /// A real function in the program table (or a closure over one).
    Fn(FnId),
    /// A builtin sentinel (copy, bitNot) — not in the program table.
    Builtin,
    /// Not a function (constructor, value, etc.).
    NotFn,
}

/// Result of an arity check: full application ready to call, or partial → closure.
#[derive(Debug)]
pub(super) enum ArityResult {
    /// All params satisfied — args ready for the call.
    Full(Vec<Value>),
    /// Too few args — produced a partial-application closure.
    Partial(Value),
}

impl<'p> Engine<'p> {
    pub(super) fn resolve_callee(
        &self,
        fun: &Expr,
        env: &Env,
    ) -> Result<Callee, VmError> {
        match fun {
            Expr::Var { name, .. } => {
                if let Some(Value::Fn(id)) = env.get(name) {
                    return Ok(Callee::Fn(*id));
                }
                // Closure: Value::Closure with a captured FnId.
                if let Some(Value::Closure(fn_id, _)) = env.get(name) {
                    return Ok(Callee::Fn(*fn_id));
                }
                if let Some(id) = self.program.fn_id(name) {
                    return Ok(Callee::Fn(id));
                }
                if name == "copy" {
                    return Ok(Callee::Builtin);
                }
                if name == "bitNot" {
                    return Ok(Callee::Builtin);
                }
                Ok(Callee::NotFn)
            }
            Expr::Upper { .. } => Ok(Callee::NotFn),
            _ => Ok(Callee::NotFn),
        }
    }

    /// Extract pre_args from a closure Var in env.
    /// - `consume=true`: remove the closure (tail-call position — frame is ending).
    /// - `consume=false`: leave it in env (value-position — caller frame stays alive).
    pub(super) fn closure_pre_args(&self, fun: &Expr, env: &mut Env, consume: bool) -> Vec<Value> {
        if let Expr::Var { name, .. } = fun {
            let get_val = if consume {
                env.remove(name)
            } else {
                env.get(name).cloned()
            };
            if let Some(Value::Closure(_, args)) = get_val {
                if !args.is_empty() {
                    return args;
                }
            }
        }
        Vec::new()
    }

    /// Check arity and either return args for a full call or produce a partial Closure.
    pub(super) fn check_arity(&mut self, fn_id: FnId, args: Vec<Value>) -> Result<ArityResult, VmError> {
        let entry = self.program.get_fn(fn_id);
        if args.len() == entry.params.len() {
            Ok(ArityResult::Full(args))
        } else if args.len() < entry.params.len() {
            // Partial → closure value with captured args.
            Ok(ArityResult::Partial(Value::Closure(fn_id, args)))
        } else {
            Err(VmError::Panic(format!(
                "too many args for `{}`: expected {}, got {}",
                entry.name, entry.params.len(), args.len()
            )))
        }
    }

    /// Look up the `Some` constructor ID (prelude-declared, D088).
    pub(super) fn opt_some_id(&self) -> ConstructorId {
        *self.program.ctor_by_name.get("Some").expect("Some ctor not in program")
    }
    /// Look up the `None` constructor ID (prelude-declared, D088).
    pub(super) fn opt_none_id(&self) -> ConstructorId {
        *self.program.ctor_by_name.get("None").expect("None ctor not in program")
    }
    /// Look up the `VBuf` constructor ID (prelude-declared `Vec`, D088).
    pub(super) fn vec_vbuf_id(&self) -> ConstructorId {
        *self.program.ctor_by_name.get("VBuf").expect("VBuf ctor not in program")
    }

    pub(super) fn eval_apply_resolved(
        &mut self,
        fun: &Expr,
        args: &[Expr],
        env: &mut Env,
    ) -> Result<Value, VmError> {
        // Method dispatch (D056/D057): `l.append 42` — Apply over a Field fun.
        if let Expr::Field { base, field, .. } = fun {
            if let Some(v) = self.try_method_call(base, field, args, env)? {
                return Ok(v);
            }
        }

        // Actor creation / singleton handle: `@name expr` (D078).
        if let Expr::AtName { name, .. } = fun {
            // 1. Installed host singleton (e.g. @std) → return a handle value.
            if let Some(id) = self.with_host(|h| h.resolve_named(name))? {
                return Ok(Value::Actor(self.with_host(|h| h.make_handle(id))?));
            }
            // 2. User-declared actor → spawn a new instance.
            if self.program.actor(name).is_some() {
                if args.len() != 1 {
                    return Err(VmError::Panic(format!(
                        "@{name} expects 1 argument (initial state)"
                    )));
                }
                let init = self.eval_value(&args[0], env)?;
                let id = self.with_host_mut(|h| h.spawn(name, init))??;
                return Ok(Value::Actor(self.with_host(|h| h.make_handle(id))?));
            }
            // 3. Unknown name.
            return Err(VmError::Panic(format!(
                "@{name} is not a known actor declaration"
            )));
        }

        // Built-ins: copy / spawn / send / retire / monitor
        if let Expr::Var { name, .. } = fun {
            match name.as_str() {
                "copy" => {
                    if args.len() != 1 {
                        return Err(VmError::Panic("copy expects 1 argument".into()));
                    }
                    let v = self.eval_value(&args[0], env)?;
                    return Ok(v.clone());
                }
                "bitNot" => {
                    if args.len() != 1 {
                        return Err(VmError::Panic("bitNot expects 1 argument".into()));
                    }
                    let v = self.eval_value(&args[0], env)?;
                    return match v {
                        Value::Int(a) => Ok(Value::Int(!a)),
                        _ => Err(VmError::Panic("bitNot requires an Int operand".into())),
                    };
                }
                "stringToCodepoints" => {
                    // stringToCodepoints : &String -> Vec Int (D088: Vec, was List).
                    let s = self.eval_value(&args[0], env)?;
                    let bytes = self.string_bytes(&s)?;
                    let str = String::from_utf8_lossy(&bytes);
                    let elems: Vec<Value> =
                        str.chars().map(|cp| Value::Int(cp as i64)).collect();
                    return self.build_vec_from_elems(elems);
                }
                "codepointsToString" => {
                    // codepointsToString : Vec Int -> String (D088: Vec, was List).
                    let v = self.eval_value(&args[0], env)?;
                    let elems = self.read_vec_to_elems(&v)?;
                    let mut codepoints: Vec<u32> = Vec::new();
                    for e in elems {
                        if let Value::Int(n) = e {
                            codepoints.push(n as u32);
                        }
                    }
                    let s: String = codepoints.iter().filter_map(|&c| char::from_u32(c)).collect();
                    return Ok(self.program.make_string(&s));
                }
                // ---- Char bridge + string indexing (§3.10 / §3.11) ----
                "stringGet" => {
                    if args.len() != 2 {
                        return Err(VmError::Panic("stringGet expects 2 arguments".into()));
                    }
                    let s = self.eval_value(&args[0], env)?;
                    let idx = self.eval_int_arg(&args[1], env, "stringGet")?;
                    if idx < 0 {
                        return Ok(Value::Data(self.opt_none_id(), vec![]));
                    }
                    let bytes = self.string_bytes(&s)?;
                    let str = std::str::from_utf8(&bytes)
                        .map_err(|e| VmError::Panic(format!("invalid UTF-8 in string: {e}")))?;
                    return match str.chars().nth(idx as usize) {
                        Some(ch) => Ok(Value::Data1(self.opt_some_id(), Box::new(self.program.make_char(ch as u32)))),
                        None => Ok(Value::Data(self.opt_none_id(), vec![])),
                    };
                }

                "charToCodepoint" => {
                    if args.len() != 1 {
                        return Err(VmError::Panic(
                            "charToCodepoint expects 1 argument".into(),
                        ));
                    }
                    let v = self.eval_value(&args[0], env)?;
                    let char_id = self.program.char_ctor_id()
                        .ok_or_else(|| VmError::Panic("Char ctor not in program".into()))?;
                    let Value::Data1(cid, inner) = &v else {
                        return Err(VmError::Panic(format!(
                            "charToCodepoint expects Char, got {v:?}"
                        )))
                    };
                    if *cid != char_id {
                        return Err(VmError::Panic(format!(
                            "charToCodepoint expects Char, got {v:?}"
                        )));
                    }
                    let Value::Int(c) = inner.as_ref() else {
                        return Err(VmError::Panic(format!(
                            "charToCodepoint: Char inner is not Int, got {v:?}"
                        )));
                    };
                    return Ok(Value::Int(*c));
                }
                "codepointToChar" => {
                    if args.len() != 1 {
                        return Err(VmError::Panic(
                            "codepointToChar expects 1 argument".into(),
                        ));
                    }
                    let v = self.eval_value(&args[0], env)?;
                    let n = match v {
                        Value::Int(n) => n,
                        other => {
                            return Err(VmError::Panic(format!(
                                "codepointToChar expects Int, got {other:?}"
                            )))
                        }
                    };
                    return match char::from_u32(n as u32) {
                        Some(ch) => Ok(Value::Data1(self.opt_some_id(), Box::new(self.program.make_char(ch as u32)))),
                        None => Ok(Value::Data(self.opt_none_id(), vec![])),
                    };
                }

                // ---- Arr T (§3.4, §6.8) ----
                "arrNew" => {
                    if args.len() != 1 {
                        return Err(VmError::Panic("arrNew expects 1 argument".into()));
                    }
                    let v = self.eval_value(&args[0], env)?;
                    let n = match v {
                        Value::Int(n) => n,
                        other => {
                            return Err(VmError::Panic(format!(
                                "arrNew expects Int, got {other:?}"
                            )))
                        }
                    };
                    if n < 0 {
                        return Err(VmError::Panic(format!("arrNew: negative length {n}")));
                    }
                    // Empty slot = None (§3.4 Arr (Option T)).
                    let none = Value::Data(self.opt_none_id(), vec![]);
                    let slots: Vec<Value> = vec![none; n as usize];
                    return Ok(Value::Data(CTOR_ARRAY, slots));
                }
                "arrLen" => {
                    if args.len() != 1 {
                        return Err(VmError::Panic("arrLen expects 1 argument".into()));
                    }
                    let v = self.eval_value(&args[0], env)?;
                    let slots = self.arr_slots(&v)?;
                    return Ok(Value::Int(slots.len() as i64));
                }
                "arrGet" => {
                    if args.len() != 2 {
                        return Err(VmError::Panic("arrGet expects 2 arguments".into()));
                    }
                    let a = self.eval_value(&args[0], env)?;
                    let idx = self.eval_int_arg(&args[1], env, "arrGet")?;
                    if idx < 0 {
                        return Ok(Value::Data(self.opt_none_id(), vec![]));
                    }
                    let slots = self.arr_slots(&a)?;
                    return match slots.get(idx as usize) {
                        Some(e) => Ok(Value::Data1(self.opt_some_id(), Box::new(e.clone()))),
                        None => Ok(Value::Data(self.opt_none_id(), vec![])),
                    };
                }
                "arrSet" => {
                    if args.len() != 3 {
                        return Err(VmError::Panic("arrSet expects 3 arguments".into()));
                    }
                    let a = self.eval_value(&args[0], env)?;
                    let idx = self.eval_int_arg(&args[1], env, "arrSet")?;
                    let v = self.eval_value(&args[2], env)?;
                    let (ctor, mut slots) = match a {
                        Value::Data(ctor, fields) if ctor == CTOR_ARRAY => (ctor, fields),
                        other => return Err(VmError::Panic(format!("expected Array, got {other:?}"))),
                    };
                    if (idx as usize) >= slots.len() {
                        return Err(VmError::Panic(format!(
                            "arrSet: index {idx} out of bounds (len {})",
                            slots.len()
                        )));
                    }
                    slots[idx as usize] = v;
                    return Ok(Value::Data(ctor, slots));
                }
                "arrToList" => {
                    // arrToList : Arr T -> List T.
                    let v = self.eval_value(&args[0], env)?;
                    let slots = self.arr_slots(&v)?.to_vec();
                    return self.build_list_from_elems(slots);
                }
                "arrFromList" => {
                    // arrFromList : List T -> Arr T.
                    let v = self.eval_value(&args[0], env)?;
                    let elems = self.read_list_to_elems(&v)?;
                    return Ok(Value::Data(CTOR_ARRAY, elems));
                }
                "stringToBytes" => {
                    // stringToBytes : &String -> Vec Int (D088: Vec, was List).
                    let v = self.eval_value(&args[0], env)?;
                    let bytes = self.string_bytes(&v)?;
                    let elems: Vec<Value> = bytes.into_iter().map(|b| Value::Int(b as i64)).collect();
                    return self.build_vec_from_elems(elems);
                }
                "stringByteAt" => {
                    // stringByteAt : &String, Int -> Option Int.
                    if args.len() != 2 {
                        return Err(VmError::Panic("stringByteAt expects 2 arguments".into()));
                    }
                    let v = self.eval_value(&args[0], env)?;
                    let idx = self.eval_int_arg(&args[1], env, "stringByteAt")?;
                    if idx < 0 {
                        return Ok(Value::Data(self.opt_none_id(), vec![]));
                    }
                    let slots = self.vec_view(&v)?;
                    return match slots.get(idx as usize) {
                        Some(e) => Ok(Value::Data1(self.opt_some_id(), Box::new(e.clone()))),
                        None => Ok(Value::Data(self.opt_none_id(), vec![])),
                    };
                }
                "bytesToString" => {
                    // bytesToString : Vec Int -> String (D088: Vec, was List).
                    let v = self.eval_value(&args[0], env)?;
                    let elems = self.read_vec_to_elems(&v)?;
                    let mut bytes: Vec<u8> = Vec::with_capacity(elems.len());
                    for e in elems {
                        if let Value::Int(n) = e {
                            bytes.push(n as u8);
                        }
                    }
                    let s = String::from_utf8_lossy(&bytes).into_owned();
                    return Ok(self.program.make_string(&s));
                }
                // ---- Tier-2 primitives (S4, §9) ----
                "nowMs" => {
                    if !args.is_empty() {
                        return Err(VmError::Panic("nowMs takes no arguments".into()));
                    }
                    let ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0);
                    return Ok(Value::Int(ms));
                }
                "randRange" => {
                    if args.len() != 2 {
                        return Err(VmError::Panic("randRange expects 2 arguments".into()));
                    }
                    let lo = self.eval_int_arg(&args[0], env, "randRange")?;
                    let hi = self.eval_int_arg(&args[1], env, "randRange")?;
                    if lo > hi {
                        return Err(VmError::Panic(format!(
                            "randRange: lo ({lo}) > hi ({hi})"
                        )));
                    }
                    let range = (hi - lo) as u64;
                    let r = if range == u64::MAX {
                        rand_u64_max()
                    } else {
                        rand_u64_max() % (range + 1)
                    };
                    return Ok(Value::Int(lo + r as i64));
                }
                "envGet" => {
                    if args.len() != 1 {
                        return Err(VmError::Panic("envGet expects 1 argument".into()));
                    }
                    let v = self.eval_value(&args[0], env)?;
                    let bytes = self.string_bytes(&v)?;
                    let name = std::str::from_utf8(&bytes)
                        .map_err(|e| VmError::Panic(format!("envGet: invalid UTF-8: {e}")))?;
                    return match std::env::var(name) {
                        Ok(val) => Ok(Value::Data1(self.opt_some_id(), Box::new(self.program.make_string(&val)))),
                        Err(_) => Ok(Value::Data(self.opt_none_id(), vec![])),
                    };
                }
                // Test infra: fail the enclosing test when the condition is false.
                "assert" => {
                    if args.len() != 2 {
                        return Err(VmError::Panic("assert expects 2 arguments".into()));
                    }
                    let cond = match self.eval_value(&args[0], env)? {
                        Value::Bool(b) => b,
                        other => {
                            return Err(VmError::Panic(format!(
                                "assert condition must be Bool, got {other:?}"
                            )))
                        }
                    };
                    if !cond {
                        let v = self.eval_value(&args[1], env)?;
                        let bytes = self.string_bytes(&v)?;
                        let msg = String::from_utf8_lossy(&bytes).into_owned();
                        return Err(VmError::Panic(format!("assertion failed: {msg}")));
                    }
                    return Ok(Value::Unit);
                }
                // ---- File I/O (S5, v1 builtins) ----
                "fileOpenRead" => {
                    if args.len() != 1 {
                        return Err(VmError::Panic("fileOpenRead expects 1 argument".into()));
                    }
                    let v = self.eval_value(&args[0], env)?;
                    let bytes = self.string_bytes(&v)?;
                    let path = std::str::from_utf8(&bytes)
                        .map_err(|e| VmError::Panic(format!("fileOpenRead: invalid UTF-8: {e}")))?;
                    return match std::fs::File::open(path) {
                        Ok(f) => {
                            let idx = file_registry_insert(f);
                            Ok(Value::Data1(self.opt_some_id(), Box::new(Value::Int(idx as i64))))
                        }
                        Err(_) => Ok(Value::Data(self.opt_none_id(), vec![])),
                    };
                }
                "fileOpenWrite" => {
                    if args.len() != 1 {
                        return Err(VmError::Panic("fileOpenWrite expects 1 argument".into()));
                    }
                    let v = self.eval_value(&args[0], env)?;
                    let bytes = self.string_bytes(&v)?;
                    let path = std::str::from_utf8(&bytes)
                        .map_err(|e| VmError::Panic(format!("fileOpenWrite: invalid UTF-8: {e}")))?;
                    return match std::fs::File::create(path) {
                        Ok(f) => {
                            let idx = file_registry_insert(f);
                            Ok(Value::Data1(self.opt_some_id(), Box::new(Value::Int(idx as i64))))
                        }
                        Err(_) => Ok(Value::Data(self.opt_none_id(), vec![])),
                    };
                }
                "fileWrite" => {
                    // fileWrite : Int, List Int -> Int. Writes the bytes (values 0..255)
                    // and returns the count written, or -1 on an invalid/closed handle.
                    let fd = self.eval_int_arg(&args[0], env, "fileWrite")?;
                    let lst = self.eval_value(&args[1], env)?;
                    let cps = self.read_list_to_elems(&lst)?;
                    let mut bytes: Vec<u8> = Vec::with_capacity(cps.len());
                    for c in cps {
                        match c {
                            Value::Int(n) if (0..=255).contains(&n) => bytes.push(n as u8),
                            Value::Int(n) => {
                                return Err(VmError::Panic(format!(
                                    "fileWrite: byte value out of range {n}"
                                )));
                            }
                            other => {
                                return Err(VmError::Panic(format!(
                                    "fileWrite: List Int element must be Int, got {other:?}"
                                )));
                            }
                        }
                    }
                    return match file_registry_get(fd as usize) {
                        Some(f) => {
                            let mut f = f.lock().unwrap();
                            match f.write_all(&bytes) {
                                Ok(()) => Ok(Value::Int(bytes.len() as i64)),
                                Err(e) => {
                                    Err(VmError::Panic(format!("fileWrite: write failed: {e}")))
                                }
                            }
                        }
                        None => Ok(Value::Int(-1)),
                    }
                }
                "fileRead" => {
                    // fileRead : Int -> Option (List Int). Reads up to 4096 raw bytes
                    // (each as an Int 0..256); None at EOF or for an invalid handle.
                    let fd = self.eval_int_arg(&args[0], env, "fileRead")?;
                    return match file_registry_get(fd as usize) {
                        Some(f) => {
                            let mut f = f.lock().unwrap();
                            let mut buf = [0u8; 4096];
                            match f.read(&mut buf) {
                                Ok(0) => Ok(Value::Data(self.opt_none_id(), vec![])),
                                Ok(n) => {
                                    let elems: Vec<Value> = buf[..n]
                                        .iter()
                                        .map(|&b| Value::Int(b as i64))
                                        .collect();
                                    let lst = self.build_list_from_elems(elems)?;
                                    Ok(Value::Data1(self.opt_some_id(), Box::new(lst)))
                                }
                                Err(e) => {
                                    Err(VmError::Panic(format!("fileRead: read failed: {e}")))
                                }
                            }
                        }
                        None => Ok(Value::Data(self.opt_none_id(), vec![])),
                    }
                }
                "fileClose" => {
                    if args.len() != 1 {
                        return Err(VmError::Panic("fileClose expects 1 argument".into()));
                    }
                    let idx = self.eval_int_arg(&args[0], env, "fileClose")?;
                    return if file_registry_remove(idx as usize) {
                        Ok(Value::Int(0))
                    } else {
                        Ok(Value::Int(-1))
                    };
                }
                _ => {}
            }
        }

        // Function call?
        if let Ok(Callee::Fn(fid)) = self.resolve_callee(fun, env) {
            let entry_name = self.program.get_fn(fid).name.clone();

            // Actor creation interception: `@Name init` where @Name is a registered
            // actor (§5.5). The type system treats this as plain application; the VM
            // intercepts it to call host.spawn + make_handle instead of invoking the
            // behavior function directly.
            if let Some(stripped) = entry_name.strip_prefix('@') {
                if self.program.actor(stripped).is_some() && args.len() == 1 {
                    let init = self.eval_value(&args[0], env)?;
                    return Ok(Value::Actor(self.with_host_mut(|host| -> Result<crate::value::ActorHandle, VmError> {
                        let id = host.spawn(stripped, init)?;
                        Ok(host.make_handle(id))
                    })??));
                }
            }

            // Start with closure pre_args (if any), then append evaluated
            // args. Value position: caller frame stays alive, so do NOT
            // consume the callee Var from env (functions are copyable §5.5).
            let mut argv = self.closure_pre_args(fun, env, false);
            argv.extend(self.eval_call_args(fid, argv.len(), args, env)?);
            // Arity check: full call, partial application, or error.
            let entry_params = self.program.get_fn(fid).params.len();
            self.dbg_trace(&format!(
                "  value-resolve {} ({} args, {} params) from callee={:?}",
                entry_name, argv.len(), entry_params, fun
            ));
            match self.check_arity(fid, argv)? {
                ArityResult::Full(full_args) => {
                    return Ok(self.call(fid, full_args)?);
                }
                ArityResult::Partial(closure) => {
                    self.dbg_trace("  → Closure");
                    return Ok(closure);
                }
            }
        }

        // Constructor application (or nullary already handled).
        let ctor_name = match fun {
            Expr::Upper { name, .. } => name.clone(),
            Expr::Var { name, .. } => name.clone(),
            other => {
                // Evaluate callee to Fn/Closure value.
                let fv = self.eval_value(other, env)?;
                match fv {
                    Value::Fn(id) => {
                        let argv = self.eval_call_args(id, 0, args, env)?;
                        match self.check_arity(id, argv)? {
                            ArityResult::Full(full_args) => return Ok(self.call(id, full_args)?),
                            ArityResult::Partial(closure) => return Ok(closure),
                        }
                    }
                    Value::Closure(fn_id, pre_args) => {
                        let offset = pre_args.len();
                        let mut argv: Vec<Value> = pre_args.clone();
                        argv.extend(self.eval_call_args(fn_id, offset, args, env)?);
                        match self.check_arity(fn_id, argv)? {
                            ArityResult::Full(full_args) => return Ok(self.call(fn_id, full_args)?),
                            ArityResult::Partial(closure) => return Ok(closure),
                        }
                    }
                    _ => {
                        return Err(VmError::Panic(
                            "applied value is not a function or constructor".into(),
                        ))
                    }
                }
            }
        };

        let mut argv = Vec::with_capacity(args.len());
        for (i, a) in args.iter().enumerate() {
            if let Expr::Var { name, .. } = a {
                let appears_elsewhere = args
                    .iter()
                    .enumerate()
                    .filter(|(j, _)| *j != i)
                    .any(|(_, other)| var_appears_in(name, other));
                if !appears_elsewhere {
                    if let Some(v) = env.get(name.as_str()) {
                        if !is_pointer_free(v) {
                            argv.push(env.remove(name).unwrap());
                            continue;
                        }
                    }
                }
            }
            argv.push(self.eval_value(a, env)?);
        }
        self.construct(&ctor_name, argv, env)
    }

    /// Method dispatch (D056/D057): resolve `field` against the receiver's
    /// ctor and call it with `[receiver, args...]`. Returns None when the
    /// receiver is not a Data value or has no such method.
    pub(super) fn try_method_call(
        &mut self,
        base: &Expr,
        field: &str,
        args: &[Expr],
        env: &mut Env,
    ) -> Result<Option<Value>, VmError> {
        let recv = if let Expr::Var { name, .. } = base {
            match env.get(name.as_str()) {
                Some(v) => v.clone(),
                _ => self.eval_value(base, env)?,
            }
        } else {
            self.eval_value(base, env)?
        };
        let Some(cid) = recv.data_ctor() else {
            return Ok(None);
        };
        let Some(&fid) = self.program.method_fns.get(&(cid, field.to_string())) else {
            return Ok(None);
        };
        let mut argv = vec![recv];
        for a in args {
            argv.push(self.eval_value(a, env)?);
        }
        match self.check_arity(fid, argv)? {
            ArityResult::Full(full) => Ok(Some(self.call(fid, full)?)),
            ArityResult::Partial(closure) => Ok(Some(closure)),
        }
    }

}
