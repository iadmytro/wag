//! Engine struct + basic methods (new, call, stats, host access).

use std::collections::HashMap;
use std::ptr::NonNull;

use crate::control::Control;
use crate::error::VmError;
use crate::host::ActorHost;
use crate::program::LoadedProgram;
use crate::value::{ActorId, FnId, Value};

/// Frame-scoped environment: maps variable names to values.
/// No lifetime parameter — all values are self-contained (PF inline or heap index).
pub(super) type Env = HashMap<String, Value>;

/// Hard dev-mode step budget per top-level call.
///
/// **Raising this requires explicit user permission** (see CLAUDE.md). It bounds
/// WAG work to protect the host from runaway/infinite loops. One "step" is one
/// function-body evaluation ([`Engine::dispatch`]); tail-call iterations and
/// nested calls both count against the current top-level budget.
pub const STEP_LIMIT: u64 = 100_000;

/// Dev-mode execution stats for analysis (step usage, §4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecStats {
    /// Function-body evaluations since the last reset (per top-level call).
    pub steps: u64,
}

/// Execution engine for one loaded program (and, under a host, one actor turn).
pub struct Engine<'p> {
    pub program: &'p LoadedProgram,
    /// Steps executed since the last reset (per top-level call); bounded by [`STEP_LIMIT`].
    pub steps: u64,
    /// Valid only for the duration of [`Self::call_with_host`].
    /// Fat pointer erased via transmute of lifetimes (cleared before return).
    pub(super) host: Option<NonNull<dyn ActorHost>>,
    pub(super) running_actor: Option<ActorId>,
    /// Name of the function currently being evaluated (for panic context).
    pub(super) current_fn: Option<String>,
}

impl<'p> Engine<'p> {
    /// Create an engine for a loaded program (standalone / test use).
    pub fn new(program: &'p LoadedProgram) -> Self {
        Engine {
            program,
            steps: 0,
            host: None,
            running_actor: None,
            current_fn: None,
        }
    }

    /// Dev-mode execution stats for analysis (steps).
    pub fn stats(&self) -> ExecStats {
        ExecStats {
            steps: self.steps,
        }
    }

    /// Trampoline: run `fn_id` to a final [`Value`] (§4.6.4).
    pub fn call(&mut self, mut fn_id: FnId, mut args: Vec<Value>) -> Result<Value, VmError> {
        loop {
            match self.dispatch(fn_id, args)? {
                Control::Return(v) => return Ok(v),
                Control::TailCall { target, args: next } => {
                    fn_id = target;
                    args = next;
                }
            }
        }
    }

    /// Run a behavior under an [`ActorHost`] (mailbox turn).
    pub fn call_with_host(
        &mut self,
        host: &mut dyn ActorHost,
        self_id: ActorId,
        fn_id: FnId,
        args: Vec<Value>,
    ) -> Result<Value, VmError> {
        // SAFETY: host lives for this call; we clear the pointer before returning.
        let ptr: *mut dyn ActorHost = host;
        let erased: *mut (dyn ActorHost + 'static) = unsafe { std::mem::transmute(ptr) };
        self.host = NonNull::new(erased);
        self.running_actor = Some(self_id);
        self.steps = 0;
        let result = self.call(fn_id, args);
        self.host = None;
        self.running_actor = None;
        result
    }

    pub fn call_name(&mut self, name: &str, args: Vec<Value>) -> Result<Value, VmError> {
        let id = self
            .program
            .fn_id(name)
            .ok_or_else(|| VmError::Panic(format!("unknown function `{name}`")))?;
        self.steps = 0;
        self.call(id, args)
    }

    pub(super) fn with_host_mut<R>(&mut self, f: impl FnOnce(&mut dyn ActorHost) -> R) -> Result<R, VmError> {
        let ptr = self
            .host
            .ok_or_else(|| VmError::Panic("actor effect outside host context".into()))?;
        // SAFETY: pointer set only during call_with_host.
        let host = unsafe { &mut *ptr.as_ptr() };
        Ok(f(host))
    }

    pub(super) fn with_host<R>(&self, f: impl FnOnce(&dyn ActorHost) -> R) -> Result<R, VmError> {
        let ptr = self
            .host
            .ok_or_else(|| VmError::Panic("actor effect outside host context".into()))?;
        // SAFETY: pointer set only during call_with_host.
        let host = unsafe { &*ptr.as_ptr() };
        Ok(f(host))
    }

    /// Debug trace helper: prints to stderr when WAG_DEBUG is set.
    #[inline]
    pub(super) fn dbg_trace(&self, msg: &str) {
        if std::env::var_os("WAG_DEBUG").is_some() {
            eprintln!("[wag-interp] {msg}");
        }
    }

    // ── Test helpers for building/inspecting list values ──

    /// Build a `List Int` value from an array of ints (test helper).
    pub fn list_from_ints(&mut self, ints: &[i64]) -> Result<Value, VmError> {
        self.build_list_from_elems(ints.iter().map(|&n| Value::Int(n)).collect())
    }

    /// Extract `Vec<i64>` from a `List Int` value (test helper).
    pub fn list_to_ints(&self, v: &Value) -> Result<Vec<i64>, VmError> {
        let elems = self.read_list_to_elems(v)?;
        let mut out = Vec::new();
        for e in elems {
            if let Value::Int(n) = e {
                out.push(n);
            }
        }
        Ok(out)
    }
}
