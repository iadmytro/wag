//! `TurnHost` — the `ActorHost` implementation for one actor turn.

use std::sync::Arc;

use wag_interp::{ActorHandle, ActorHost, ActorId, ActorLiveness, Value, VmError};

use crate::runtime::{ActorKind, Inner, MailItem, RuntimeError};
use crate::scheduler::{enqueue_msg, enqueue_ready, spawn_actor_inner};

/// Host for one actor turn.
pub(crate) struct TurnHost {
    pub(crate) inner: Arc<Inner>,
    pub(crate) self_id: ActorId,
}

impl ActorHost for TurnHost {
    fn self_id(&self) -> ActorId {
        self.self_id
    }

    fn spawn(&mut self, actor_name: &str, init_state: Value) -> Result<ActorId, VmError> {
        spawn_actor_inner(
            &self.inner,
            ActorKind::Named(actor_name.to_string()),
            init_state,
            None,
        )
        .map_err(|e| match e {
            RuntimeError::Vm(v) => v,
            RuntimeError::Panic(s) => VmError::Panic(s),
        })
    }

    fn send(
        &mut self,
        target: ActorId,
        msg: Value,
    ) -> Result<(), VmError> {
        enqueue_msg(
            &self.inner,
            self.self_id,
            target,
            msg,
        )
        .map_err(|e| match e {
            RuntimeError::Vm(v) => v,
            RuntimeError::Panic(s) => VmError::Panic(s),
        })
    }

    fn make_handle(&self, id: ActorId) -> ActorHandle {
        // Clone the canonical liveness Arc (D078). Lazily created on first call;
        // all subsequent calls upgrade the stored Weak to share the same refcount.
        let actors = self.inner.actors.read().unwrap();
        if let Some(mx) = actors.get(&id) {
            let mut cell = mx.lock().unwrap();
            // Try to upgrade existing Weak (fast path for repeated `self` refs).
            if let Some(ref weak) = cell.liveness_weak {
                if let Some(arc) = weak.upgrade() {
                    return arc;
                }
            }
            // First handle: create the canonical Arc.
            let immortal = cell.immortal;
            let liveness = if immortal {
                Arc::new(ActorLiveness { id, on_zero: None })
            } else {
                let inner_clone = Arc::clone(&self.inner);
                let sink: Arc<dyn Fn(ActorId) + Send + Sync> = Arc::new(move |target_id| {
                    let actors = inner_clone.actors.read().unwrap();
                    if let Some(mx) = actors.get(&target_id) {
                        let mut cell = mx.lock().unwrap();
                        if !cell.dead {
                            if cell.mailbox.is_empty() {
                                // No in-flight messages: abandon immediately.
                                cell.mailbox.push_back(MailItem::Abandon);
                                enqueue_ready(&inner_clone, &mut cell);
                            } else {
                                // Messages still in-flight: defer abandonment until drained.
                                cell.abandon_pending = true;
                            }
                        }
                    }
                });
                Arc::new(ActorLiveness { id, on_zero: Some(sink) })
            };
            cell.liveness_weak = Some(Arc::downgrade(&liveness));
            return liveness;
        }
        // Fallback for unknown actor ids (shouldn't happen in practice).
        Arc::new(ActorLiveness { id, on_zero: None })
    }

    fn resolve_named(&self, name: &str) -> Option<ActorId> {
        self.inner.named.lock().unwrap().get(name).copied()
    }
}
