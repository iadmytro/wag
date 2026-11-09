//! WAG runtime values, function table, and trampoline interpreter.
//!
//! Non-actor execution is complete; actor *effects* (`spawn`/`send`/…) are
//! dispatched through [`ActorHost`], implemented by `wag-runtime`.

mod control;
mod error;
mod eval;
mod host;
mod list_util;
mod program;
mod value;

pub use control::Control;
pub use error::VmError;
pub use eval::{load_checked, prepare, run_fn, ExecStats, Engine, STEP_LIMIT};
pub use host::ActorHost;
pub use list_util::{build_prelude_list, read_prelude_list, PreludeListCtors};
pub use program::{load, ActorEntry, CtorEntry, FnEntry, LoadedProgram};
pub use value::{
    ActorHandle, ActorId, ActorLiveness, ConstructorId, FnId, Value,
    CTOR_ARRAY,
    CTOR_PAIR2, CTOR_PAIR3, CTOR_PRODUCT,
};

pub fn crate_name() -> &'static str {
    "wag-interp"
}
