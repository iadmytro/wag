//! WAG structural checks — Milestone 6 pass 1 + Milestone 8 pass 2.
//!
//! Pipeline:
//! 1. `wag-types::check` (caller may pass an already-typed program)
//! 2. Field-path table + desugar `.` / `with` (rules 15/17)
//! 3. Linearity (rule 10), borrow well-formedness (rule 11), `copy`/`&` PF checks
//! 4. Mark the program cell-reuse-eligible for `wag-interp` (`CheckedProgram::cell_reuse`)
//! 5. Pass 2: tail-cycle law (rule 1), exhaustiveness (§3.6)
//!
//! ## Interface with `wag-interp` (D018)
//!
//! After pass 2, [`CheckedProgram::cyclic_fns`] names every top-level function/actor that
//! participates in a call-graph cycle. The interpreter already lowers every **tail-position**
//! call to `Control::TailCall` (trampoline). Rule 1 guarantees every cyclic edge is in
//! tail position, so those edges always hit the trampoline. Acyclic non-tail calls remain
//! ordinary nested `Engine::call` frames. No per-call-site side table is required beyond
//! `cyclic_fns` for diagnostics and tests.

mod error;
mod exhaustive;
mod fields;
mod linear;
mod tail;

pub use error::{CheckError, Rule};
pub use tail::{CallEdge, TailCycleInfo};

use std::collections::{BTreeSet, HashMap};

use crate::fields::FieldTable;
use wag_syntax::{Item, Program};
use wag_types::TypedProgram;

/// Result of pass 1 + pass 2.
#[derive(Debug, Clone)]
pub struct CheckedProgram {
    /// Program with `Field` / `RecordUpdate` desugared to `match` / rebuild.
    program: Program,
    typed: TypedProgram,
    /// When true, `wag-interp` may call [`wag_interp`]-side `Arena::reuse` on matched-out cells.
    ///
    /// Contract (D015): after a successful linearity check, every owned match that
    /// deconstructs an arena cell leaves that cell dead; the evaluator may overwrite
    /// it when rebuilding a same-shaped constructor in the arm. No per-node side table
    /// is required for the conformance floor — the dynamic reuse stack is sound
    /// under the global linearity proof.
    cell_reuse: bool,
    /// Functions/actors on a call-graph cycle (rule 1). Empty when the program is acyclic.
    ///
    /// Contract (D018): `wag-interp`'s trampoline must consume tail calls among these names
    /// as `Control::TailCall` (already the default for any tail-position call).
    cyclic_fns: BTreeSet<String>,
    /// Full edge list from the rule-1 analysis (for tests / diagnostics).
    tail_info: TailCycleInfo,
}

impl CheckedProgram {
    pub fn program(&self) -> &Program {
        &self.program
    }
    pub fn typed(&self) -> &TypedProgram {
        &self.typed
    }
    /// When true, `wag-interp` may reuse matched-out arena cells (D015).
    pub fn cell_reuse(&self) -> bool {
        self.cell_reuse
    }
    /// Functions/actors on a call-graph cycle (rule 1, D018).
    pub fn cyclic_fns(&self) -> &BTreeSet<String> {
        &self.cyclic_fns
    }
    /// Full edge list from the rule-1 analysis.
    pub fn tail_info(&self) -> &TailCycleInfo {
        &self.tail_info
    }
}

/// A borrowed, module-tagged view of one module for checking (D067).
pub struct ModuleView<'a> {
    /// Module name used to stamp errors with their origin. `None` = unnamed
    /// (single-module checks keep the legacy unattributed behavior).
    pub name: Option<&'a str>,
    pub items: &'a [Item],
}

/// Type-check + structural checks on a standalone snippet — no prelude
/// is merged. The snippet is checked as a single unnamed module (unattributed
/// errors). Use [`check_with_prelude`] when the snippet relies on prelude names.
pub fn check_pure(src: &str) -> Result<CheckedProgram, Box<dyn std::error::Error + Send + Sync>> {
    let mut program = wag_syntax::parse(src).map_err(|e| e.to_string())?;
    // D060: single early desugar of method-body bare names (before inference).
    wag_syntax::desugar_method_bare_names(&mut program);
    wag_syntax::desugar_main(&mut program);

    let typed = wag_types::check(&program)?;
    check_modules(
        &[ModuleView { name: None, items: &program.items }],
        typed,
    )
    .map_err(|e| attach_source(e, &[(None, src)]).into())
}

/// Attach the source text matching the error's module tag (D067) so plain
/// `Display` can render line/column. Unmatched tags keep byte offsets.
fn attach_source(e: CheckError, sources: &[(Option<&str>, &str)]) -> CheckError {
    for (name, src) in sources {
        if e.module.as_deref() == *name {
            return e.with_source(*src);
        }
    }
    e
}

/// Type-check + structural checks with the built-in prelude merged in as
/// a first module (D024/D043), module-aware (D067).
///
/// The snippet is checked as a second module importing every prelude export,
/// so check-stage errors carry their originating module (`prelude` vs.
/// `snippet`) instead of relying on merged-program span heuristics.
pub fn check_with_prelude(src: &str) -> Result<CheckedProgram, Box<dyn std::error::Error + Send + Sync>> {
    use wag_types::ModuleInput;

    let mut snippet = wag_syntax::parse(src).map_err(|e| e.to_string())?;
    // D060: single early desugar of method-body bare names (before inference).
    wag_syntax::desugar_method_bare_names(&mut snippet);
    wag_syntax::desugar_main(&mut snippet);

    let prelude_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../prelude/prelude.wag");
    if !prelude_path.exists() {
        // No prelude on disk: fall back to the standalone path.
        return check_pure(src);
    }

    let prelude_src = std::fs::read_to_string(&prelude_path)
        .map_err(|e| format!("failed to read prelude: {e}"))?;
    let mut prelude_prog = wag_syntax::parse(&prelude_src)
        .map_err(|e| format!("prelude parse error: {e}"))?;
    wag_syntax::desugar_method_bare_names(&mut prelude_prog);
    wag_syntax::desugar_main(&mut prelude_prog);

    // Prelude exports all top-level names (canonical keys, actors `@`-prefixed).
    let mut exports: Vec<String> = Vec::new();
    for item in &prelude_prog.items {
        match item {
            Item::Func(f) => exports.push(f.name.clone()),
            Item::Actor(a) => exports.push(format!("@{}", a.name)),
            Item::TypeAlias(a) => exports.push(a.name.clone()),
            Item::ParametricType(p) => exports.push(p.name.clone()),
            _ => {}
        }
    }

    let modules = vec![
        ModuleInput {
            name: "prelude".into(),
            items: prelude_prog.items,
            import_aliases: Vec::new(),
            exports: exports.clone(),
        },
        ModuleInput {
            name: "snippet".into(),
            items: snippet.items,
            // Import every prelude export under its own name.
            import_aliases: exports.into_iter().map(|n| (n.clone(), n)).collect(),
            exports: Vec::new(),
        },
    ];

    let typed = wag_types::infer_multi_module(&modules)?;
    // Check the snippet first (legacy merged-program fail-fast priority:
    // user items before prelude), so negative-test diagnostics surface the
    // same error as before D067 while still carrying module attribution.
    let views: Vec<ModuleView> = modules
        .iter()
        .rev()
        .map(|m| ModuleView { name: Some(m.name.as_str()), items: &m.items })
        .collect();
    check_modules(&views, typed)
        .map_err(|e| attach_source(e, &[(Some("prelude"), prelude_src.as_str()), (Some("snippet"), src)]).into())
}

/// Run pass 1 + pass 2 on a parsed + typed program (single unnamed
/// module — the legacy entry point kept for tests/benches).
pub fn check(program: &Program, typed: TypedProgram) -> Result<CheckedProgram, CheckError> {
    check_modules(&[ModuleView { name: None, items: &program.items }], typed)
}

/// Run pass 1 + pass 2 over a set of modules (D067). Each returned
/// error is stamped with the module of its offending item. The output
/// `CheckedProgram.program` is still the merged desugared program (the VM
/// erases modules at load), so `load_checked` is untouched.
pub fn check_modules<'a>(
    modules: &[ModuleView<'a>],
    typed: TypedProgram,
) -> Result<CheckedProgram, CheckError> {
    let field_table = FieldTable::build(&typed);

    // Desugar field access / record update per module so linearity sees
    // match/rebuild form (same ownership story as the surface sugar).
    // Desugaring is per-expression over global tables, so grouping by module
    // is observationally identical to desugaring the merged program.
    let groups: Vec<(Option<&str>, Vec<Item>)> = modules
        .iter()
        .map(|mv| {
            let prog = Program { exports: None, imports: None, items: mv.items.to_vec(), source: None };
            Ok((mv.name, fields::desugar_program(&prog, &typed, &field_table)?.items))
        })
        .collect::<Result<_, CheckError>>()?;

    // Pass 1: linearity + borrows (rule 10/11), stamped per item.
    for (name, items) in &groups {
        for item in items {
            linear::check_item(item, &typed).map_err(|e| stamp(*name, e))?;
        }
    }

    // Rule 1 is whole-program (global call graph): run on the merged program,
    // attributing cycle errors via a name → module map.
    let merged = Program {
        exports: None,
        imports: None,
        items: groups.iter().flat_map(|(_, i)| i.iter().cloned()).collect(),
        source: None,
    };
    let mut name_module: HashMap<String, String> = HashMap::new();
    for (name, items) in &groups {
        if let Some(n) = name {
            for item in items {
                match item {
                    Item::Func(f) => { name_module.insert(f.name.clone(), n.to_string()); }
                    Item::Actor(a) => { name_module.insert(format!("@{}", a.name), n.to_string()); }
                    _ => {}
                }
            }
        }
    }
    let tail_info = tail::check_tail_cycles_with(&merged, &typed, &name_module)?;

    // Exhaustiveness (§3.6).
    for (name, items) in &groups {
        for item in items {
            exhaustive::check_item(item, &typed).map_err(|e| stamp(*name, e))?;
        }
    }

    Ok(CheckedProgram {
        program: merged,
        typed,
        cell_reuse: true,
        cyclic_fns: tail_info.cyclic_fns.clone(),
        tail_info,
    })
}

/// Stamp a check error with its originating module (D067).
fn stamp(mod_name: Option<&str>, e: CheckError) -> CheckError {
    match mod_name {
        Some(m) => e.with_module(m),
        None => e,
    }
}

pub fn crate_name() -> &'static str {
    "wag-check"
}

#[cfg(test)]
mod prelude_audit {
    //! Regression guard (D068): the built-in prelude must pass rule 10/11
    //! linearity checking with zero violations. The prelude's test-and-keep
    //! combinators (`filter`/`takeWhile`/`partition`/…) reuse their head after a
    //! predicate call via `copy`; the `@std`/`@net` actors reconstruct state in
    //! each match arm instead of reusing consumed bindings.
    use super::*;

    #[test]
    fn prelude_passes_linearity() {
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../prelude/prelude.wag"),
        )
        .unwrap();
        let mut program = wag_syntax::parse(&src).unwrap();
        wag_syntax::desugar_method_bare_names(&mut program);
        wag_syntax::desugar_main(&mut program);
        let typed = wag_types::check(&program).unwrap();
        let field_table = FieldTable::build(&typed);
        let desugared = fields::desugar_program(&program, &typed, &field_table).unwrap();
        let mut violations = Vec::new();
        for item in &desugared.items {
            let label = match item {
                wag_syntax::Item::Func(f) => f.name.clone(),
                wag_syntax::Item::Actor(a) => format!("@{}", a.name),
                wag_syntax::Item::Method(m) => format!("{}.{}", m.type_name, m.name),
                _ => "?".into(),
            };
            if let Err(e) = linear::check_item(item, &typed) {
                violations.push(format!(
                    "[{}] {} at {}..{} origin={:?}",
                    label, e.message, e.span.start, e.span.end, e.origin
                ));
            }
        }
        assert!(
            violations.is_empty(),
            "prelude linearity violations:\n{}",
            violations.join("\n")
        );
    }
}
