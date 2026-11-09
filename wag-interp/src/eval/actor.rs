//! Actor operations (send via `Expr::Send`).

use wag_syntax::Expr;
use crate::error::VmError;
use crate::value::Value;

use super::engine::{Engine, Env};

impl<'p> Engine<'p> {
    pub(super) fn construct_nullary(&mut self, name: &str, env: &mut Env) -> Result<Value, VmError> {
        self.construct(name, vec![], env)
    }

    /// Evaluate `target@ msg` (§5.5): resolve the target handle, evaluate the
    /// payload, and hand it off to the host's mailbox. Returns Unit.
    pub(super) fn eval_send_expr(&mut self, target: &Expr, msg: &Expr, env: &mut Env) -> Result<Value, VmError> {
        // Resolve the target to an actor id.
        let target_id = match target {
            Expr::AtName { name, .. } => {
                // Bare @Name as send target: resolve via host (system actors).
                self.with_host(|host| host.resolve_named(name))?
                    .ok_or_else(|| VmError::Panic(format!(
                        "send @{name}: no live actor binding for built-in/name"
                    )))?
            }
            _ => {
                let v = self.eval_value(target, env)?;
                match v {
                    Value::Actor(liveness) => liveness.id,
                    other => {
                        return Err(VmError::Panic(format!(
                            "send target must be Actor handle, got {other:?}"
                        )))
                    }
                }
            }
        };
        let payload = self.eval_value(msg, env)?;
        self.with_host_mut(|host| host.send(target_id, payload))??;
        Ok(Value::Unit)
    }

}
