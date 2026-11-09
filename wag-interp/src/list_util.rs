//! List construction/reading for builtins and test helpers (owned model, D081).
//!
//! `List T` is a prelude-declared region-based handle (D054/D056/D062):
//! `Data(LH, [head, tail, start, len])` over owned `LNodeC(val, next)` /
//! `LEnd()` trees. The old built-in cons-list representation
//! (`CTOR_LIST_NODE`/`CTOR_LIST_END`) was removed in D088.
//!
//! In the owned model, list nodes are recursive values (not shared cells). The LH
//! handle's `head` and `start` fields hold independent clones of the same tree at
//! construction time; subsequent operations consume+rebuild (no aliasing).

use crate::error::VmError;
use crate::value::{ConstructorId, Value};

/// Constructor IDs for the prelude's region-based `List` type (D054/D056).
#[derive(Debug, Clone, Copy)]
pub struct PreludeListCtors {
    pub lh: ConstructorId,
    pub lnodec: ConstructorId,
    pub lend: ConstructorId,
}

impl PreludeListCtors {
    /// Look up prelude List ctor IDs from a loaded program. Returns `None` when
    /// the program does not define the prelude List type.
    pub fn from_program(program: &crate::program::LoadedProgram) -> Option<Self> {
        let lh = *program.ctor_by_name.get("LH")?;
        let lnodec = *program.ctor_by_name.get("LNodeC")?;
        let lend = *program.ctor_by_name.get("LEnd")?;
        Some(Self { lh, lnodec, lend })
    }
}

/// Build a prelude `List T` value (LH handle over owned LNodeC/LEnd trees).
/// The cursor (`head`) and anchor (`start`) both start at the first node.
pub fn build_prelude_list(
    elems: Vec<Value>,
    ctors: PreludeListCtors,
) -> Result<Value, VmError> {
    let n = elems.len();
    if n == 0 {
        // Three independent LEnd values (no shared cells in owned model).
        let end = || Value::Data(ctors.lend, vec![]);
        return Ok(Value::Data(ctors.lh, vec![end(), end(), end(), Value::Int(0)]));
    }

    // Build the chain from tail to head: Node(e0, Node(e1, ... Node(en-1, End)))
    let mut chain = Value::Data(ctors.lend, vec![]);
    for elem in elems.into_iter().rev() {
        chain = Value::Data(ctors.lnodec, vec![elem, chain]);
    }

    // `head` is the full tree from the first node.
    // `tail` is just the last node (we need to extract it).
    let head = chain;
    let tail = extract_tail(&head, ctors);
    // `start` is a clone of `head` (independent owned copy in its own field slot).
    let start = head.clone();

    Ok(Value::Data(ctors.lh, vec![head, tail, start, Value::Int(n as i64)]))
}

/// Walk to the last LNodeC in a chain and return it (owned clone).
fn extract_tail(v: &Value, ctors: PreludeListCtors) -> Value {
    let mut cur = v;
    loop {
        match cur {
            Value::Data(ctor, fields) if *ctor == ctors.lnodec => {
                if fields.len() != 2 {
                    panic!("LNodeC has wrong arity");
                }
                // Check if next is LEnd (i.e., this is the last node).
                match &fields[1] {
                    Value::Data(next_ctor, _) if *next_ctor == ctors.lend => return cur.clone(),
                    _ => cur = &fields[1],
                }
            }
            _ => panic!("expected LNodeC in list chain"),
        }
    }
}

/// Read a prelude `List T` value (LH handle) into a vector of elements.
pub fn read_prelude_list(
    v: &Value,
    ctors: PreludeListCtors,
) -> Result<Vec<Value>, VmError> {
    let fields = match v {
        Value::Data(ctor, fields) if *ctor == ctors.lh => fields,
        other => {
            return Err(VmError::Panic(format!(
                "expected LH handle, got {other:?}"
            )))
        }
    };
    if fields.len() != 4 {
        return Err(VmError::Panic("LH handle has wrong arity".into()));
    }

    // Walk from `start` (field index 2) through LNodeC values.
    let mut cur = &fields[2];
    let mut out = Vec::new();
    loop {
        match cur {
            Value::Data(ctor, _) if *ctor == ctors.lend => break,
            Value::Data(ctor, f) if *ctor == ctors.lnodec => {
                if f.len() != 2 {
                    return Err(VmError::Panic("LNodeC has wrong arity".into()));
                }
                out.push(f[0].clone());
                cur = &f[1];
            }
            other => {
                return Err(VmError::Panic(format!(
                    "expected LNodeC or LEnd in list, got {other:?}"
                )));
            }
        }
    }
    Ok(out)
}
