//! Constructor construction + move-based pattern matching (peek_match, destructure).
//!
//! Owned model (D081): all composite values are `Value::Data(ctor, Vec<Value>)`.
//! Inline values (Int, Char, Bool, Unit, Str, Fn, Actor) are stored directly in `Value`.
//! No heap arena, no cell indices — Rust ownership handles reclamation.

use wag_syntax::{LitPat, Pat};
use crate::error::VmError;
use crate::program::{CtorEntry, CtorKind};
use crate::value::{Value, CTOR_PAIR2, CTOR_PAIR3, CTOR_PRODUCT};

use super::engine::{Engine};

impl<'p> Engine<'p> {
    pub(super) fn construct(&mut self, name: &str, args: Vec<Value>, _env: &mut super::engine::Env) -> Result<Value, VmError> {
        let id = self
            .program
            .ctor_by_name
            .get(name)
            .copied()
            .ok_or_else(|| VmError::Panic(format!("unknown constructor `{name}`")))?;
        let entry = self.program.get_ctor(id);
        self.construct_with_entry(entry, args)
    }

    pub(super) fn construct_with_entry(
        &mut self,
        entry: &CtorEntry,
        args: Vec<Value>,
    ) -> Result<Value, VmError> {
        match entry.kind {
            CtorKind::True => return Ok(Value::Bool(true)),
            CtorKind::False => return Ok(Value::Bool(false)),
            CtorKind::Unit => return Ok(Value::Unit),
            CtorKind::Pair => {
                if args.len() != 2 {
                    return Err(VmError::Panic("Pair expects 2 fields".into()));
                }
                return Ok(Value::Data(CTOR_PAIR2, args));
            }
            CtorKind::Triple => {
                if args.len() != 3 {
                    return Err(VmError::Panic("Triple expects 3 fields".into()));
                }
                return Ok(Value::Data(CTOR_PAIR3, args));
            }
            CtorKind::User => {}
        }

        // User constructor: owned Data value.
        Ok(Value::Data(entry.id, args))
    }

    // ─── Move-based pattern matching (§4.3.2: no implicit copy) ────────────────

    /// Phase 1: check if pattern shape matches value (no allocation, no consumption).
    pub(super) fn peek_match(&self, pat: &Pat, value: &Value) -> bool {
        match pat {
            Pat::Wildcard { .. } | Pat::Var { .. } => true,
            Pat::Literal { lit, .. } => match lit {
                LitPat::Int(n) => matches!(value, Value::Int(v) if v == n),
                LitPat::Str(s) => value.as_str().map(|v| v == s.as_str()).unwrap_or(false),
            },
            Pat::Product { elems, .. } => self.peek_product(elems, value),
            Pat::Ctor { name, args, .. } => {
                let id = match self.program.ctor_by_name.get(name) {
                    Some(i) => *i,
                    None => return false,
                };
                let entry = self.program.get_ctor(id);
                self.peek_ctor(entry, args, value)
            }
        }
    }

    fn peek_product(&self, elems: &[Pat], value: &Value) -> bool {
        let fields = match value {
            Value::Data(_, fields) => fields,
            _ => return false,
        };
        if fields.len() != elems.len() {
            return false;
        }
        elems.iter().zip(fields.iter()).all(|(p, f)| self.peek_match(p, f))
    }

    fn peek_ctor(&self, entry: &CtorEntry, args: &[Pat], value: &Value) -> bool {
        match entry.kind {
            CtorKind::True => return matches!(value, Value::Bool(true)),
            CtorKind::False => return matches!(value, Value::Bool(false)),
            CtorKind::Unit => return matches!(value, Value::Unit),
            CtorKind::Pair => {
                return match value {
                    Value::Data(ctor, fields) => {
                        (*ctor == CTOR_PAIR2 || *ctor == CTOR_PRODUCT)
                            && self.peek_fields(args, fields)
                    }
                    _ => false,
                };
            }
            CtorKind::Triple => {
                return match value {
                    Value::Data(ctor, fields) => {
                        (*ctor == CTOR_PAIR3 || *ctor == CTOR_PRODUCT)
                            && self.peek_fields(args, fields)
                    }
                    _ => false,
                };
            }
            CtorKind::User => {
                return match value {
                    Value::Data(ctor, fields) => {
                        *ctor == entry.id && self.peek_fields(args, fields)
                    }
                    _ => false,
                };
            }
        }
    }

    fn peek_fields(&self, args: &[Pat], fields: &[Value]) -> bool {
        if args.len() != fields.len() {
            return false;
        }
        for (p, f) in args.iter().zip(fields.iter()) {
            if !self.peek_match(p, f) {
                return false;
            }
        }
        true
    }

    /// Phase 2: destructure value into bindings (moves fields out, no clone).
    /// Precondition: `peek_match` returned true for this (pat, value) pair.
    pub(super) fn destructure(
        &mut self,
        pat: &Pat,
        value: Value,
    ) -> Result<Vec<(String, Value)>, VmError> {
        match pat {
            Pat::Wildcard { .. } => Ok(vec![]),
            Pat::Var { name, .. } => Ok(vec![(name.clone(), value)]),
            Pat::Literal { .. } => Ok(vec![]),
            Pat::Product { elems, .. } => self.destructure_product(elems, value),
            Pat::Ctor { name, args, .. } => {
                let id = match self.program.ctor_by_name.get(name) {
                    Some(i) => *i,
                    None => return Err(VmError::Panic(format!("unknown ctor `{name}` in pattern"))),
                };
                let entry = self.program.get_ctor(id);
                self.destructure_ctor(entry, args, value)
            }
        }
    }

    fn destructure_product(
        &mut self,
        elems: &[Pat],
        value: Value,
    ) -> Result<Vec<(String, Value)>, VmError> {
        let fields = match value {
            Value::Data(_, fields) => fields,
            other => return Err(VmError::Panic(format!("product destructure: not a Data value, got {other:?}"))),
        };
        self.bind_fields("product", elems, fields)
    }

    fn destructure_ctor(
        &mut self,
        entry: &CtorEntry,
        pats: &[Pat],
        value: Value,
    ) -> Result<Vec<(String, Value)>, VmError> {
        let fields: Vec<Value> = match entry.kind {
            // Zero-arity ctors: consume the value, no bindings.
            CtorKind::True | CtorKind::False | CtorKind::Unit => {
                return Ok(vec![]);
            }
            CtorKind::Pair | CtorKind::Triple | CtorKind::User => {
                match value {
                    Value::Data(_, fields) => fields,
                    other => {
                        return Err(VmError::Panic(format!(
                            "ctor destructure: expected Data, got {other:?}"
                        )))
                    }
                }
            }
        };

        self.bind_fields(&entry.name, pats, fields)
    }

    /// Recursively bind pattern elements to field values (all moves).
    fn bind_fields(
        &mut self,
        ctor_name: &str,
        pats: &[Pat],
        fields: Vec<Value>,
    ) -> Result<Vec<(String, Value)>, VmError> {
        if pats.len() != fields.len() {
            return Err(VmError::Panic(format!(
                "pattern arity mismatch for `{ctor_name}`: {} pats vs {} fields",
                pats.len(),
                fields.len()
            )));
        }
        let mut out = Vec::new();
        for (p, f) in pats.iter().zip(fields.into_iter()) {
            out.extend(self.destructure(p, f)?);
        }
        Ok(out)
    }

}
