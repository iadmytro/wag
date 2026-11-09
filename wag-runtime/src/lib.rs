//! WAG actor scheduler, mailboxes, and message pools (Milestone 7+9).
//!
//! Implements §4.5 / §4.6.2 / §4.6.6 / §5.5 / §5.6 / §4.3.3 for *valid* actor
//! programs, including runtime-provided `@std`/`@net`.

mod builtin_io;
mod entry;
mod host;
mod native;
mod reply;
mod runtime;
mod scheduler;

pub use builtin_io::IoConfig;
pub use entry::{prepare_runtime, run_main, run_main_with_io};
pub use runtime::{Runtime, RuntimeError, RuntimeStats, SchedulerConfig, MAX_EXEC_TIMEOUT};

pub fn crate_name() -> &'static str {
    "wag-runtime"
}
