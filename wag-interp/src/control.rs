//! Calling convention control tokens (§4.6.3–§4.6.4).

use crate::value::{FnId, Value};

/// Result of one function-body step before the trampoline continues.
#[derive(Debug)]
pub enum Control {
    Return(Value),
    TailCall { target: FnId, args: Vec<Value> },
}
