//! Tree-walking interpreter + trampoline (§4.6.3–§4.6.4).

mod actor;
mod apply;
mod dispatch;
pub(super) mod engine;
mod match_ctor;
mod util;

pub use engine::{Engine, ExecStats, STEP_LIMIT};

use crate::program::LoadedProgram;
use crate::value::Value;
use crate::error::VmError;

/// Parse, type-check, load (no desugar / cell-reuse).
pub fn prepare(src: &str) -> Result<(wag_syntax::Program, wag_types::TypedProgram, LoadedProgram), Box<dyn std::error::Error + Send + Sync>> {
    let mut program = wag_syntax::parse(src).map_err(|e| e.to_string())?;
    // D060: single early desugar of method-body bare names (before inference).
    wag_syntax::desugar_method_bare_names(&mut program);
    wag_syntax::desugar_main(&mut program);
    let typed = wag_types::check(&program)?;
    let loaded = crate::program::load(&program, &typed);
    Ok((program, typed, loaded))
}

/// Parse, type-check, check+desugar, load with `cell_reuse = true`.
///
/// Requires the `wag-check` crate at the call site — kept free of a hard dep here
/// so interpreter tests stay independent. Prefer `wag_check::check_with_prelude` + `load_checked`.
pub fn load_checked(
    program: &wag_syntax::Program,
    typed: &wag_types::TypedProgram,
    cell_reuse: bool,
) -> LoadedProgram {
    let mut loaded = crate::program::load(program, typed);
    loaded.cell_reuse = cell_reuse;
    loaded
}

/// Convenience: prepare source and call a named function. The result is fully
/// owned (top-level returns cannot carry borrows).
pub fn run_fn(src: &str, name: &str, args: Vec<Value>) -> Result<Value, VmError> {
    let (_p, _t, loaded) = prepare(src).map_err(|e| VmError::Panic(e.to_string()))?;
    let mut eng = Engine::new(&loaded);
    eng.call_name(name, args)
}
