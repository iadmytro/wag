//! WAG namespacing and monomorphic Hindley–Milner inference.
//!
//! Pipeline for a parsed [`wag_syntax::Program`]:
//! 1. Constructor/type namespacing (§3.1.1)
//! 2. Whole-module monomorphic inference (§3.2) with Option arm-lift (§3.3)
//!    and match exhaustiveness (§3.4)
//! 3. `@`-declaration shape + `@main`/`@std`/`@net` wiring (§5.4, §5.5.2, §5.5.7)
//!
//! Self-referential user types are permitted (§7 rule 14 classifies them as
//! non-pointer-free; see `infer::util::is_likely_pointer_free`), so there is no
//! acyclicity pre-pass.

mod boundary;
mod error;
mod infer;
mod outbox;
mod pf;
mod resident;
mod types;
mod unify;

pub use error::{Stage, TypeError};
pub use infer::builtins::builtin_function_types;
pub use infer::{infer_multi_module, infer_program, surface_to_ty, ModuleInput, TypedProgram};
pub use outbox::outbox_ctors;
pub use pf::{compute_pointer_free, ty_is_pointer_free};
pub use resident::compute_residency;
pub use types::{CtorInfo, CtorMeta, FieldInfo, Scheme, Ty, TypeCtors};
pub use unify::pretty_ty;

use wag_syntax::Program;

/// Parse-free entry: run type checks on an already-parsed program.
pub fn check(program: &Program) -> Result<TypedProgram, TypeError> {
    infer_program(program).map_err(|e| {
        let e = match &program.source {
            Some(src) => e.with_source(src.clone()),
            None => e,
        };
        e
    })
}

/// Lex+parse `src`, then type-check.
pub fn check_str(src: &str) -> Result<TypedProgram, Box<dyn std::error::Error + Send + Sync>> {
    let mut program = wag_syntax::parse(src).map_err(|e| e.to_string())?;
    // D060: single early desugar of method-body bare names (before inference).
    wag_syntax::desugar_method_bare_names(&mut program);
    wag_syntax::desugar_main(&mut program);
    infer_program(&program).map_err(|e| e.into())
}
