//! Utility functions: string/array helpers, file registry, binops, random.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use wag_syntax::{BinOp, Expr};
use crate::error::VmError;
use crate::value::{Value, CTOR_ARRAY};

use super::engine::{Engine, Env};

impl<'p> Engine<'p> {
    /// Extract the UTF-8 bytes of a `String` value (prelude constrained datatype).
    pub(super) fn string_bytes(&self, v: &Value) -> Result<Vec<u8>, VmError> {
        let s = self.program.value_to_string(v)
            .ok_or_else(|| VmError::Panic(format!("expected String, got {v:?}")))?;
        Ok(s.into_bytes())
    }

    /// Non-consuming view of the element buffer of an `Array T` value.
    pub(super) fn arr_slots<'a>(&self, v: &'a Value) -> Result<&'a [Value], VmError> {
        match v {
            Value::Data(ctor, fields) if *ctor == CTOR_ARRAY => Ok(fields),
            other => Err(VmError::Panic(format!("expected Array, got {other:?}"))),
        }
    }

    /// View a value as a vector of `Int` bytes (works on `String` values).
    /// Used by `stringByteAt` to index into a `String`'s byte buffer.
    pub(super) fn vec_view(&self, v: &Value) -> Result<Vec<Value>, VmError> {
        match v {
            Value::Data(ctor, fields) if *ctor == CTOR_ARRAY => Ok(fields.clone()),
            _ => {
                let s = self.program.value_to_string(v)
                    .ok_or_else(|| VmError::Panic(format!("expected Array/String, got {v:?}")))?;
                Ok(s.bytes().map(|b| Value::Int(b as i64)).collect())
            }
        }
    }

    /// Read a `List T` value (prelude LH handle) into a vector of elements.
    pub(super) fn read_list_to_elems(&self, v: &Value) -> Result<Vec<Value>, VmError> {
        let ctors = crate::list_util::PreludeListCtors::from_program(self.program)
            .ok_or_else(|| VmError::Panic("prelude List ctors not found".into()))?;
        crate::list_util::read_prelude_list(v, ctors)
    }

    /// Build a `List T` value (prelude LH handle) from elements, preserving order.
    pub(super) fn build_list_from_elems(&self, elems: Vec<Value>) -> Result<Value, VmError> {
        let ctors = crate::list_util::PreludeListCtors::from_program(self.program)
            .ok_or_else(|| VmError::Panic("prelude List ctors not found".into()))?;
        crate::list_util::build_prelude_list(elems, ctors)
    }

    /// Build a `Vec T` value (`VBuf`) from elements (D088).
    pub(super) fn build_vec_from_elems(&self, elems: Vec<Value>) -> Result<Value, VmError> {
        let vbuf_id = self.vec_vbuf_id();
        let some_id = self.opt_some_id();
        let slots: Vec<Value> = elems.iter().map(|e| Value::Data1(some_id, Box::new(e.clone()))).collect();
        let arr = Value::Data(CTOR_ARRAY, slots);
        let n = elems.len() as i64;
        Ok(Value::Data(vbuf_id, vec![arr, Value::Int(n), Value::Int(n)]))
    }

    /// Read a `Vec T` value (`VBuf`) into a vector of elements (D088).
    pub(super) fn read_vec_to_elems(&self, v: &Value) -> Result<Vec<Value>, VmError> {
        let vbuf_id = self.vec_vbuf_id();
        let (ctor, fields) = match v {
            Value::Data(c, f) if *c == vbuf_id => (c, f),
            other => return Err(VmError::Panic(format!("expected Vec, got {other:?}"))),
        };
        let _ = ctor;
        if fields.len() != 3 {
            return Err(VmError::Panic(format!("VBuf expects 3 fields, got {}", fields.len())));
        }
        let buf = &fields[0];
        let len = match &fields[1] {
            Value::Int(n) => *n as usize,
            _ => return Err(VmError::Panic("VBuf len must be Int".into())),
        };
        let slots = self.arr_slots(buf)?;
        let some_id = self.opt_some_id();
        let mut out = Vec::with_capacity(len);
        for slot in slots.iter().take(len) {
            match slot {
                Value::Data(c, fs) if *c == some_id && fs.len() == 1 => out.push(fs[0].clone()),
                Value::Data1(c, f) if *c == some_id => out.push(f.as_ref().clone()),
                _ => return Err(VmError::Panic("VBuf slot is not Some".into())),
            }
        }
        Ok(out)
    }

    /// Evaluate an argument and require it to be an `Int` (array/string index).
    pub(super) fn eval_int_arg(&mut self, arg: &Expr, env: &mut Env, name: &str) -> Result<i64, VmError> {
        let v = self.eval_value(arg, env)?;
        match v {
            Value::Int(n) => Ok(n),
            other => Err(VmError::Panic(format!("{name} index must be Int, got {other:?}"))),
        }
    }


}

/// Process-wide file registry for `fileOpenRead`/`fileOpenWrite`/`fileRead`/`fileWrite`/`fileClose`.
/// Maps integer handles to `Arc<Mutex<File>>` objects.
struct FileRegistry {
    next_id: usize,
    files: HashMap<usize, Arc<Mutex<std::fs::File>>>,
}

thread_local! {
    static FILE_REGISTRY: std::cell::RefCell<FileRegistry> = std::cell::RefCell::new(FileRegistry {
        next_id: 0,
        files: HashMap::new(),
    });
}

pub(super) fn file_registry_insert(f: std::fs::File) -> i64 {
    FILE_REGISTRY.with(|reg| {
        let mut r = reg.borrow_mut();
        let id = r.next_id;
        r.next_id += 1;
        r.files.insert(id, Arc::new(Mutex::new(f)));
        id as i64
    })
}

pub(super) fn file_registry_get(id: usize) -> Option<Arc<Mutex<std::fs::File>>> {
    FILE_REGISTRY.with(|reg| {
        let r = reg.borrow();
        r.files.get(&id).cloned()
    })
}

pub(super) fn file_registry_remove(id: usize) -> bool {
    FILE_REGISTRY.with(|reg| {
        let mut r = reg.borrow_mut();
        r.files.remove(&id).is_some()
    })
}

/// Non-cryptographic 64-bit random (xorshift-mixed time+counter seed).
/// Sufficient for `randRange`; not for security-sensitive uses.
pub(super) fn rand_u64_max() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x1234_5678_9ABC_DEF0);
    let c = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut x = t ^ c.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    x ^= x >> 33;
    x = x.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    x ^= x >> 33;
    x
}

/// Raw arithmetic (Rust debug-mode semantics): returns `Int`, traps on
/// overflow or division-by-zero.
pub(super) fn raw_binop(
    lhs: &Value,
    rhs: &Value,
    op: &str,
    f: impl Fn(i64, i64) -> Option<i64>,
) -> Result<Value, VmError> {
    let (Value::Int(a), Value::Int(b)) = (lhs, rhs) else {
        return Err(VmError::Panic("arithmetic on non-Int".into()));
    };
    match f(*a, *b) {
        Some(n) => Ok(Value::Int(n)),
        None => Err(VmError::Panic(format!(
            "integer {op} overflow or division by zero ({a} {op} {b})"
        ))),
    }
}

/// Total bitwise op returning `Int`.
pub(super) fn bitwise_binop(lhs: &Value, rhs: &Value, f: impl Fn(i64, i64) -> i64) -> Result<Value, VmError> {
    let (Value::Int(a), Value::Int(b)) = (lhs, rhs) else {
        return Err(VmError::Panic("bitwise on non-Int".into()));
    };
    Ok(Value::Int(f(*a, *b)))
}

/// Integer comparison returning `Bool`.
pub(super) fn cmp_binop(lhs: &Value, rhs: &Value, f: impl Fn(i64, i64) -> bool) -> Result<Value, VmError> {
    let (Value::Int(a), Value::Int(b)) = (lhs, rhs) else {
        return Err(VmError::Panic("comparison on non-Int".into()));
    };
    Ok(Value::Bool(f(*a, *b)))
}

pub(super) fn eval_binop(op: BinOp, lhs: &Value, rhs: &Value) -> Result<Value, VmError> {
    match op {
        BinOp::Add => raw_binop(lhs, rhs, "addition", i64::checked_add),
        BinOp::Sub => raw_binop(lhs, rhs, "subtraction", i64::checked_sub),
        BinOp::Mul => raw_binop(lhs, rhs, "multiplication", i64::checked_mul),
        BinOp::Div => raw_binop(lhs, rhs, "division", |a, b| if b == 0 { None } else { a.checked_div(b) }),
        BinOp::Rem => raw_binop(lhs, rhs, "remainder", |a, b| if b == 0 { None } else { a.checked_rem(b) }),
        BinOp::BitAnd => match (lhs, rhs) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a & b)),
            (Value::Bool(a), Value::Bool(b)) => Ok(Value::Bool(*a && *b)),
            _ => Err(VmError::Panic("`&` requires Int or Bool operands".into())),
        },
        BinOp::BitOr => match (lhs, rhs) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a | b)),
            (Value::Bool(a), Value::Bool(b)) => Ok(Value::Bool(*a || *b)),
            _ => Err(VmError::Panic("`|` requires Int or Bool operands".into())),
        },
        BinOp::BitXor => bitwise_binop(lhs, rhs, |a, b| a ^ b),
        BinOp::BitNot => match lhs {
            Value::Int(a) => Ok(Value::Int(!(*a))),
            Value::Bool(a) => Ok(Value::Bool(!*a)),
            _ => Err(VmError::Panic("`!` requires Int or Bool operand".into())),
        },
        BinOp::ShiftLeft => {
            let (Value::Int(a), Value::Int(b)) = (lhs, rhs) else {
                return Err(VmError::Panic("shift on non-Int".into()));
            };
            Ok(Value::Int((*a) << ((*b as u64) & 63)))
        }
        BinOp::ShiftRight => {
            let (Value::Int(a), Value::Int(b)) = (lhs, rhs) else {
                return Err(VmError::Panic("shift on non-Int".into()));
            };
            Ok(Value::Int((*a) >> ((*b as u64) & 63)))
        }
        BinOp::Gt => cmp_binop(lhs, rhs, |a, b| a > b),
        BinOp::Lt => cmp_binop(lhs, rhs, |a, b| a < b),
        BinOp::Ge => cmp_binop(lhs, rhs, |a, b| a >= b),
        BinOp::Le => cmp_binop(lhs, rhs, |a, b| a <= b),
        BinOp::Eq | BinOp::Ne => {
            let eq = match (lhs, rhs) {
                (Value::Int(a), Value::Int(b)) => a == b,
                (Value::Bool(a), Value::Bool(b)) => a == b,
                (Value::Unit, Value::Unit) => true,
                // Structural equality for composites (Char, String, Option, pairs, user types, §3.5).
                _ => lhs == rhs,
            };
            Ok(Value::Bool(match op {
                BinOp::Eq => eq,
                BinOp::Ne => !eq,
                _ => unreachable!(),
            }))
        }
    }
}
