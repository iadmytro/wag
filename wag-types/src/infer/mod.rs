//! Hindley–Milner monomorphic inference for a whole module (§3.2).
//!
//! Split into focused submodules:
//! - `context`   — inference state (Context struct + basic methods)
//! - `builtins`  — built-in constructor registration
//! - `ctors`     — user constructor registration + namespacing
//! - `infer_func` — function/actor inference
//! - `infer_expr` — expression inference (apply, field, update)
//! - `infer_pattern` — pattern binding + exhaustiveness
//! - `util`      — type helpers, var renaming, free var collection
//! - `topo`      — call graph + topological ordering (D044)
//! - `program`   — public API (TypedProgram, infer_program, infer_multi_module)

pub mod builtins;
pub mod context;
pub mod ctors;
pub mod infer_expr;
pub mod infer_func;
pub mod infer_pattern;
pub mod program;
pub mod topo;
pub mod util;
pub mod validate;

// Re-export public API.
pub use program::{infer_multi_module, infer_program, ModuleInput, TypedProgram};
pub use util::surface_to_ty;
