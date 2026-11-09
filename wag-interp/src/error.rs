//! VM-level failures.

use wag_syntax::Span;

/// Runtime failure distinct from WAG-level `None`.
#[derive(Debug)]
pub enum VmError {
    /// Step budget ([`crate::eval::STEP_LIMIT`]) exhausted — likely an infinite loop.
    StepLimit,
    Panic(String),
    /// An AST node that an earlier pass (field/record desugaring) was
    /// supposed to rewrite reached the VM — the program was loaded without
    /// running `wag_check` (§7). Carries the source span of the offending node.
    Undesugared { what: &'static str, span: Span },
}

impl std::fmt::Display for VmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VmError::StepLimit => write!(
                f,
                "step budget exhausted ({} steps) — possible infinite loop",
                crate::eval::STEP_LIMIT
            ),
            VmError::Panic(s) => write!(f, "vm panic: {s}"),
            VmError::Undesugared { what, span } => {
                write!(f, "vm panic: {what} @ {}..{}", span.start, span.end)
            }
        }
    }
}

impl std::error::Error for VmError {}
