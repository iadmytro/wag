//! Host hooks for actor effects (implemented by `wag-runtime`).

use crate::error::VmError;
use crate::value::{ActorHandle, ActorId, Value};

/// Runtime services available while evaluating an `@`-declaration body.
///
/// `wag-interp` never implements this; `wag-runtime`'s scheduler does.
/// All values crossing the host boundary are self-contained (PF inline or heap Ref).
pub trait ActorHost {
    /// Currently executing actor (`self`).
    fn self_id(&self) -> ActorId;

    /// Create a new actor instance: register it with the runtime and return its id.
    fn spawn(&mut self, actor_name: &str, init_state: Value) -> Result<ActorId, VmError>;

    /// `target@ msg` — deliver an O(1) value handoff into the target's mailbox (§4.3.4).
    fn send(&mut self, target: ActorId, msg: Value) -> Result<(), VmError>;

    /// Wrap a live actor id in a handle value (`Arc<ActorLiveness>`). The runtime
    /// impl installs an abandonment sink; test/no-host impls pass `None`.
    fn make_handle(&self, id: ActorId) -> ActorHandle;

    /// Resolve a bare `@Name` (e.g. `@std` / `@net`) to a live actor id.
    fn resolve_named(&self, name: &str) -> Option<ActorId>;
}
